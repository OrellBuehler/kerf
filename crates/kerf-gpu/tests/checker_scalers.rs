//! The delivery's named scalers (`lanczos`, `spline`, `neighbor`, `bilinear`), checked independently of the implementer's cases:
//! `checker/scalers/...`. Every case builds a short cut, renders it through `kerf_core::render_with` (FFmpeg's real export graph, with
//! `scale=...:flags=<name>` on every fit and zoom) and holds every frame the plan **draws** to the export's planes with no tolerance;
//! a frame the plan refuses is FFmpeg's and is only counted. The per-frame outcome of every case is written to
//! `target/parity/checker-sc/<build>-<mode>/<case>.frames`, and `KERF_CHECKER_ALLOW` (with a scratch patch of the refusals under test,
//! never committed) draws the refused frames anyway to judge each refusal's necessity.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_scalers -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr, dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    Asset, Clip, CompositeColorPolicy, Container, Delivery, ExportOptions, Fit, PlanMode, PlanRequest, Planner, Project,
    RateControl, StreamKind, Timeline, Track, Transition, TransitionKind,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames};

// ---- the machinery (copied from checker_b_filters.rs) ------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    let gpu = GPU
        .get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone();
    // (Every compositor of this file is built on this device: the measurements are made before any of them draws.)
    prime();
    gpu
}

/// Measure, once, everything a cut's caps are made of: the grade's conversions, the scaler, the matrix conversion, the `geq` edge (a mask
/// and a keyed opacity are in the caps only once it is known: a render only reads it), the planar conversion and the placement (a moving zoom
/// or position), the blend chain and the text layout. A compositor built without it refuses every such clip, and a sweep counts a refusal as
/// fine: the cases would pass without holding a masked, placed or blended frame to the export.
fn prime() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = kerf_gpu::grade::kernels();
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::convert::kernels();
        let _ = kerf_core::geq_edge();
        let _ = kerf_gpu::conversion::conversion();
        let _ = kerf_gpu::placement_probe::placement();
        // ... and whether it takes a translucent layer through RGB, and blurs, as the model does (a layer below full opacity and a blur are in the caps only then).
        let _ = kerf_gpu::roundtrip_probe::translucency();
        let _ = kerf_gpu::blur_probe::blur();
        let _ = kerf_gpu::display_probe::display();
        let _ = kerf_gpu::blend::kernels();
        let _ = kerf_gpu::text::model();
        // ... and for which kinds of picture, in which chains, a moving still is drawn as this FFmpeg draws it.
        let _ = kerf_gpu::still_probe::all();
    });
}

/// A frame refused for a mask or a keyed opacity is refused because the caps lack them, which is the `geq` edge not being measured
/// ([`prime`]): the plan refuses nothing else under those names. That is the harness's fault, not the plan's, and it must not pass as a
/// refusal.
fn no_geq_refusal(case: &str, why: &[kerf_core::Unsupported]) {
    assert!(
        !why.iter()
            .any(|u| matches!(u, kerf_core::Unsupported::Mask(_) | kerf_core::Unsupported::KeyedOpacity(_))),
        "{case}: refused for a mask or a keyed opacity, which only a compositor without the geq edge refuses (measured: {:?}): the \
         case would pass without holding a masked frame to the export",
        kerf_core::geq_edge_measured()
    );
}

/// What a frame's plan carries that only a measured probe lets through (printed in the frame log beside `exact`).
fn frame_tags(plan: &kerf_core::RenderPlan) -> String {
    let mut tags = String::new();
    if plan.layers.iter().any(|l| l.mask.is_some() || l.keyed_alpha.is_some()) {
        tags.push_str(" [geq]");
    }
    if plan.layers.iter().any(|l| plan.pose(l).is_some()) {
        tags.push_str(" [placed]");
    }
    if plan.layers.iter().any(|l| !l.blend.is_normal()) {
        tags.push_str(" [blend]");
    }
    tags
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| {
        prime();
        let comp = Compositor::new(gpu()).expect("a compositor");
        let caps = comp.caps();
        assert!(
            caps.mask
                && caps.keyed_opacity
                && caps.placement
                && caps.behaviors
                && caps.placed_chroma
                && caps.blend != kerf_core::BlendKinds::NONE,
            "a probe could not tell on this machine, so cases would pass as refused: {caps:?}"
        );
        comp
    })
}

fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

fn policy() -> CompositeColorPolicy {
    let measured = kerf_core::composite_color_policy();
    assert_ne!(
        measured,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    measured
}

/// Set when a run has had a refusal patched out of the plan to draw its frames anyway (`KERF_CHECKER_ALLOW`).
fn patched_out() -> bool {
    std::env::var_os("KERF_CHECKER_ALLOW").is_some()
}

fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("target/")
        .to_path_buf()
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

fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

/// `ffmpeg` with `input` on its stdin, stdout returned.
fn ffmpeg_piped(args: &[&str], input: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A `FFmpeg | GPU | diff x8` strip of one frame in `target/parity/checker-b/montage/`.
fn montage(case: &str, k: u64, (w, h): (usize, usize), want: [&[u8]; 3], got: [&[u8]; 3]) {
    let rgb = |planes: [&[u8]; 3]| -> Vec<u8> {
        let mut raw = planes[0].to_vec();
        raw.extend_from_slice(planes[1]);
        raw.extend_from_slice(planes[2]);
        let size = format!("{w}x{h}");
        ffmpeg_piped(
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
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
            ],
            &raw,
        )
    };
    let (reference, drawn) = (rgb(want), rgb(got));
    let mut strip = Vec::with_capacity(w * h * 9);
    for row in 0..h {
        strip.extend_from_slice(&reference[row * w * 3..(row + 1) * w * 3]);
        strip.extend_from_slice(&drawn[row * w * 3..(row + 1) * w * 3]);
        for i in 0..w {
            let d =
                |c: usize| (i32::from(reference[(row * w + i) * 3 + c]) - i32::from(drawn[(row * w + i) * 3 + c])).unsigned_abs();
            let mag = (d(0).max(d(1)).max(d(2)) * 8).min(255) as u8;
            strip.extend_from_slice(&[mag, mag, mag]);
        }
    }
    let dir = target_dir().join("parity").join("checker-sc").join("montage");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}-f{k:03}.png", slug(case)));
    let size = format!("{}x{h}", w * 3);
    ffmpeg_piped(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &size,
            "-i",
            "pipe:0",
            path.to_str().unwrap(),
        ],
        &strip,
    );
    eprintln!("   montage (FFmpeg | GPU | diff x8): {}", path.display());
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// A short stable hash of the FFmpeg in use: the media a build made is kept apart from another build's.
fn build_key() -> String {
    let path = kerf_core::ffmpeg_path();
    let mut h: u64 = 1469598103934665603;
    for b in path.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(1099511628211);
    }
    format!("{:08x}", h & 0xffff_ffff)
}

fn clip(a: &Asset, source_in: f64, source_out: f64, start: f64) -> Clip {
    Clip::new(a.id, source_in, source_out, start)
}

fn timeline(tracks: Vec<Vec<Clip>>, format: Option<Delivery>) -> Timeline {
    Timeline {
        tracks: tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| Track {
                clips,
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            })
            .collect(),
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

fn at_fps(fps: f64) -> ExportOptions {
    ExportOptions {
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

fn dissolve_into(mut c: Clip, kind: TransitionKind, secs: f64) -> Clip {
    c.transition_in = Some(Transition { kind, duration: secs });
    c
}

// ---- the export, read back as planes ------------------------------------------------------------------

struct Played {
    w: usize,
    h: usize,
    yuv: Vec<u8>,
}

impl Played {
    fn frame_len(&self) -> usize {
        self.w * self.h * 3 / 2
    }

    fn count(&self) -> usize {
        self.yuv.len() / self.frame_len()
    }

    fn planes(&self, k: usize) -> [&[u8]; 3] {
        let f = &self.yuv[k * self.frame_len()..(k + 1) * self.frame_len()];
        let (y, uv) = f.split_at(self.w * self.h);
        let (u, v) = uv.split_at(self.w * self.h / 4);
        [y, u, v]
    }
}

fn export(name: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Played {
    try_export(name, tl, assets, opts).unwrap_or_else(|e| panic!("{name}: export: {e}"))
}

fn try_export(name: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Result<Played, String> {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-sc-ref-{}", build_key()));
    std::fs::create_dir_all(&scratch).unwrap();
    let file = scratch.join(format!("{}.mkv", slug(name)));
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts.clone()
    };
    kerf_core::render_with(tl, assets, &file, &lossless).map_err(|e| e.to_string())?;
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
    Ok(Played {
        w: video.width.unwrap() as usize,
        h: video.height.unwrap() as usize,
        yuv,
    })
}

// ---- the comparison -------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct PlaneDiff {
    pub differing: usize,
    pub max: i32,
    pub first: Option<(usize, usize)>,
}

fn diff_plane(want: &[u8], got: &[u8], width: usize) -> PlaneDiff {
    let mut d = PlaneDiff::default();
    for (i, (a, b)) in want.iter().zip(got).enumerate() {
        let e = (i32::from(*a) - i32::from(*b)).abs();
        if e != 0 {
            d.differing += 1;
            d.max = d.max.max(e);
            d.first.get_or_insert((i % width, i / width));
        }
    }
    if want.len() != got.len() {
        d.differing += want.len().abs_diff(got.len());
        d.max = d.max.max(255);
    }
    d
}

#[derive(Clone, Debug)]
pub struct Bad {
    pub frame: u64,
    pub planes: [PlaneDiff; 3],
    /// Samples in each plane.
    pub len: [usize; 3],
}

#[derive(Default, Debug)]
pub struct Outcome {
    pub frames: usize,
    pub drawn: usize,
    pub refused: usize,
    pub bad: Vec<Bad>,
    /// Each refusal reason with the number of frames it was among the reasons of.
    pub reasons: BTreeMap<String, usize>,
}

impl Outcome {
    pub fn exact(&self) -> usize {
        self.drawn - self.bad.len()
    }

    pub fn refused_for(&self, needle: &str) -> usize {
        self.reasons.iter().filter(|(r, _)| r.contains(needle)).map(|(_, n)| n).sum()
    }
}

fn report_lines() -> &'static Mutex<BTreeMap<String, String>> {
    static R: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn write_report() {
    let mut text = String::new();
    for line in report_lines().lock().unwrap().values() {
        let _ = writeln!(text, "{line}");
    }
    let dir = target_dir().join("parity");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("checker-sc-report-{}.txt", std::process::id())), text);
}

fn frame_log_dir() -> PathBuf {
    let mode = if std::env::var_os("KERF_CHECKER_ALLOW").is_some() {
        "allow"
    } else {
        "normal"
    };
    let dir = target_dir()
        .join("parity")
        .join("checker-sc")
        .join(format!("{}-{mode}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Plan, draw and compare every frame of `frames` frames of the cut at `opts`; a frame the plan refuses is counted.
/// The outcome is printed, logged, and returned for the case to judge.
fn sweep(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, frames: usize) -> Outcome {
    let played = export(case, tl, assets, opts);
    sweep_played(case, tl, assets, opts, frames, played)
}

/// [`sweep`] for a cut FFmpeg itself may not be able to render: `None` then (nothing to hold a drawing to).
fn sweep_try(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) -> Option<Outcome> {
    let opts = at_fps(fps);
    match try_export(case, tl, assets, &opts) {
        Ok(played) => Some(sweep_played(case, tl, assets, &opts, (secs * fps).floor() as usize, played)),
        Err(e) => {
            let first = e
                .lines()
                .find(|l| l.contains("Invalid") || l.contains("Error"))
                .unwrap_or("export failed");
            eprintln!("{case:<58} FFmpeg cannot render it: {first}");
            if std::env::var_os("KERF_CHECKER_VERBOSE").is_some() {
                eprintln!("{e}");
            }
            None
        }
    }
}

fn sweep_played(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, frames: usize, played: Played) -> Outcome {
    sweep_played_on(compositor(), case, tl, assets, opts, frames, played)
}

/// [`sweep_played`] for a compositor of the caller's (one whose scaler is pinned, say).
fn sweep_played_on(
    comp: &Compositor,
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    frames: usize,
    played: Played,
) -> Outcome {
    let frames = frames.min(played.count());
    assert!(frames > 0, "{case}: the export has no frame");
    let planner = Planner::new(tl, assets, opts, PlanRequest::motion(policy())).expect("planner");
    assert_eq!(planner.mode(), PlanMode::Motion);
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let mut out = Outcome {
        frames,
        ..Outcome::default()
    };
    let mut log = String::new();
    for k in 0..frames as u64 {
        let plan = planner.at_frame(k).expect("plan");
        let why = plan.reasons(&comp.caps(), size);
        no_geq_refusal(case, &why);
        let tags = frame_tags(&plan);
        let was_refused = !why.is_empty();
        if was_refused {
            out.refused += 1;
            let names: BTreeSet<String> = why.iter().map(ToString::to_string).collect();
            for n in &names {
                *out.reasons.entry(n.clone()).or_insert(0) += 1;
            }
            let _ = writeln!(log, "{k}\trefused\t{}", names.into_iter().collect::<Vec<_>>().join(" | "));
            // (A run with `KERF_CHECKER_ALLOW` and the scratch patch draws the refused frames anyway, to judge the refusal.)
            if !patched_out() {
                continue;
            }
        }
        let attempt = motion.frames(&plan).map_err(|e| e.to_string()).and_then(|picture| {
            comp.composite_yuv_counted(&plan, &picture, motion.counts(), size)
                .map_err(|e| e.to_string())
        });
        let drawn = match attempt {
            Ok(drawn) => drawn,
            // (With a refusal patched out to draw the frame anyway, the compositor may have nothing to draw it with.)
            Err(e) if std::env::var_os("KERF_CHECKER_ALLOW").is_some() => {
                let _ = writeln!(log, "{k}\tundrawable\t{e}");
                continue;
            }
            Err(e) => panic!("{case}: frame {k}: {e}"),
        };
        out.drawn += 1;
        let want = played.planes(k as usize);
        let got: [&[u8]; 3] = [&drawn.y, &drawn.u, &drawn.v];
        let planes = [
            diff_plane(want[0], got[0], played.w),
            diff_plane(want[1], got[1], played.w / 2),
            diff_plane(want[2], got[2], played.w / 2),
        ];
        if planes.iter().any(|p| p.differing > 0) {
            if out.bad.is_empty() && !patched_out() {
                montage(case, k, (played.w, played.h), want, got);
            }
            let _ = writeln!(
                log,
                "{k}\t{}INEXACT\tY {} (max {}) U {} (max {}) V {} (max {}){tags}",
                if was_refused { "ANYWAY-" } else { "" },
                planes[0].differing,
                planes[0].max,
                planes[1].differing,
                planes[1].max,
                planes[2].differing,
                planes[2].max
            );
            out.bad.push(Bad {
                frame: k,
                planes,
                len: [want[0].len(), want[1].len(), want[2].len()],
            });
        } else {
            let _ = writeln!(log, "{k}\t{}exact{tags}", if was_refused { "ANYWAY-" } else { "" });
        }
    }
    let _ = std::fs::write(frame_log_dir().join(format!("{}.frames", slug(case))), &log);
    let worst = |i: usize| out.bad.iter().map(|b| b.planes[i].max).max().unwrap_or(0);
    let differing = |i: usize| out.bad.iter().map(|b| b.planes[i].differing).sum::<usize>();
    let mut line = format!(
        "{case:<58} {:>3} frames: {:>3} drawn ({:>3} exact), {:>3} refused",
        out.frames,
        out.drawn,
        out.exact(),
        out.refused
    );
    if !out.bad.is_empty() {
        let first = &out.bad[0];
        let _ = write!(
            line,
            " | INEXACT {} frames, first {} | Y {} max {} | U {} max {} | V {} max {} | first (x,y) Y {:?} U {:?} V {:?}",
            out.bad.len(),
            first.frame,
            differing(0),
            worst(0),
            differing(1),
            worst(1),
            differing(2),
            worst(2),
            first.planes[0].first,
            first.planes[1].first,
            first.planes[2].first
        );
    }
    if !out.reasons.is_empty() {
        let mut why: Vec<String> = out.reasons.iter().map(|(r, n)| format!("{n}x {r}")).collect();
        why.truncate(4);
        let _ = write!(line, " | refused: {}", why.join("; "));
    }
    eprintln!("{line}");
    report_lines().lock().unwrap().insert(case.to_string(), line);
    write_report();
    out
}

fn sweep_secs(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) -> Outcome {
    sweep(case, tl, assets, &at_fps(fps), (secs * fps).floor() as usize)
}

/// Failures are collected over a family and reported together, so one bad case does not hide the others.
#[derive(Default)]
struct Family {
    failures: Vec<String>,
    drawn: usize,
    refused: usize,
    cases: usize,
}

impl Family {
    fn add(&mut self, case: &str, out: &Outcome) {
        self.cases += 1;
        self.drawn += out.drawn;
        self.refused += out.refused;
        if !out.bad.is_empty() {
            let first = &out.bad[0];
            self.failures.push(format!(
                "{case}: {}/{} drawn frames inexact, first frame {} (Y {} max {}, U {} max {}, V {} max {})",
                out.bad.len(),
                out.drawn,
                first.frame,
                first.planes[0].differing,
                first.planes[0].max,
                first.planes[1].differing,
                first.planes[1].max,
                first.planes[2].differing,
                first.planes[2].max
            ));
        }
    }

    /// The family passes when every drawn frame is exact and **something was drawn**: a plan that refuses everything is not
    /// the compositor drawing exactly (a probe that wrongly says "not exact" would pass every case otherwise).
    fn finish(self, family: &str) {
        let nothing = self.drawn == 0 && !patched_out();
        self.finish_with(family, nothing);
    }

    /// [`Family::finish`] for a family whose cases are all about what the plan refuses.
    fn finish_refusing(self, family: &str) {
        self.finish_with(family, false);
    }

    fn finish_with(self, family: &str, nothing_drawn: bool) {
        eprintln!(
            "== {family}: {} cases, {} frames drawn, {} refused, {} inexact cases",
            self.cases,
            self.drawn,
            self.refused,
            self.failures.len()
        );
        assert!(self.failures.is_empty(), "{family}:\n{}", self.failures.join("\n"));
        assert!(
            !nothing_drawn,
            "{family}: the plan refused every frame ({} refused)",
            self.refused
        );
    }
}

fn pip(mut c: Clip, scale: f64, x: f64, y: f64) -> Clip {
    c.transform.scale = scale;
    c.transform.pos_x = x;
    c.transform.pos_y = y;
    c
}

fn zoom_key(time: f64, scale: f64, pos_x: f64, pos_y: f64) -> kerf_core::Keyframe {
    kerf_core::Keyframe {
        time,
        scale,
        pos_x,
        pos_y,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    }
}

/// What drawing a cut's refused frames anyway came to: the plan's refusal is *necessary* when some of them are not the export's.
#[derive(Default, Debug)]
struct Anyway {
    refused: usize,
    drawn: usize,
    exact: usize,
    undrawable: usize,
    worst_y: i32,
    worst_uv: i32,
    first_error: Option<String>,
    reasons: BTreeMap<String, usize>,
}

impl Anyway {
    fn inexact(&self) -> usize {
        self.drawn - self.exact
    }

    /// `None` when nothing was refused or nothing could be drawn anyway (the compositor asks the plan itself: the scratch patch
    /// behind `KERF_CHECKER_ALLOW` lets it draw); else whether the refusal was needed (`Some(true)`), or every frame drawn anyway
    /// was the export's (`Some(false)`: a refusal wider than it has to be).
    fn needed(&self) -> Option<bool> {
        (self.refused > 0 && self.drawn > 0).then_some(self.inexact() > 0)
    }
}

fn necessity_lines() -> &'static Mutex<BTreeMap<String, String>> {
    static R: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Draw the frames of `tl` that the plan refuses (at most `cap` of them, every few frames) with `comp` anyway and hold them to
/// the export. The compositor may refuse to draw them itself (`undrawable`).
fn anyway(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    frames: usize,
    comp: &Compositor,
    cap: usize,
) -> Anyway {
    let played = export(&format!("anyway-{case}"), tl, assets, opts);
    let frames = frames.min(played.count());
    let planner = Planner::new(tl, assets, opts, PlanRequest::motion(policy())).expect("planner");
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let mut out = Anyway::default();
    let mut refused_frames: Vec<u64> = Vec::new();
    for k in 0..frames as u64 {
        let plan = planner.at_frame(k).expect("plan");
        let why = plan.reasons(&comp.caps(), size);
        no_geq_refusal(case, &why);
        if why.is_empty() {
            continue;
        }
        out.refused += 1;
        refused_frames.push(k);
        for n in why.iter().map(ToString::to_string).collect::<BTreeSet<_>>() {
            *out.reasons.entry(n).or_insert(0) += 1;
        }
    }
    // (At most `cap` of the refused frames, spread over all of them: the first, the last and those between.)
    let chosen: Vec<u64> = if refused_frames.len() <= cap {
        refused_frames.clone()
    } else {
        (0..cap)
            .map(|i| refused_frames[i * (refused_frames.len() - 1) / (cap - 1).max(1)])
            .collect()
    };
    for k in chosen {
        let plan = planner.at_frame(k).expect("plan");
        let attempt = motion.frames(&plan).map_err(|e| e.to_string()).and_then(|picture| {
            comp.composite_yuv_counted(&plan, &picture, motion.counts(), size)
                .map_err(|e| e.to_string())
        });
        let drawn = match attempt {
            Ok(drawn) => drawn,
            Err(e) => {
                out.undrawable += 1;
                out.first_error.get_or_insert(e);
                continue;
            }
        };
        out.drawn += 1;
        let want = played.planes(k as usize);
        let got: [&[u8]; 3] = [&drawn.y, &drawn.u, &drawn.v];
        let planes = [
            diff_plane(want[0], got[0], played.w),
            diff_plane(want[1], got[1], played.w / 2),
            diff_plane(want[2], got[2], played.w / 2),
        ];
        if planes.iter().all(|p| p.differing == 0) {
            out.exact += 1;
        }
        out.worst_y = out.worst_y.max(planes[0].max);
        out.worst_uv = out.worst_uv.max(planes[1].max).max(planes[2].max);
    }
    let verdict = match out.needed() {
        None if out.refused == 0 => "NOTHING REFUSED".to_string(),
        None => "undrawable here (needs the scratch patch + KERF_CHECKER_ALLOW)".to_string(),
        Some(true) => "needed".to_string(),
        Some(false) => "WIDER THAN NEEDED (every frame drawn anyway is exact)".to_string(),
    };
    let mut why: Vec<String> = out
        .reasons
        .iter()
        .map(|(r, n)| format!("{n}x {}", r.chars().take(70).collect::<String>()))
        .collect();
    why.truncate(2);
    let line = format!(
        "{case:<60} refused {:>3}, drawn anyway {:>2} ({} exact, {} undrawable) Y max {} UV max {} | {verdict} | {}",
        out.refused,
        out.drawn,
        out.exact,
        out.undrawable,
        out.worst_y,
        out.worst_uv,
        why.join("; ")
    );
    eprintln!("{line}");
    necessity_lines().lock().unwrap().insert(case.to_string(), line);
    out
}

// ---- which clip decides the composite's matrix: every route onto the alpha plane, in both orders, on one track and stacked ------

/// The compositor of this machine with its scaler pinned to `kernels` (what a probe on another FFmpeg would have said).
fn pinned(kernels: kerf_gpu::scaler::Kernels) -> Compositor {
    Compositor::new(gpu()).expect("a compositor").with_scaler_kernels(kernels)
}

// ---- media ---------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tag {
    None,
    Bt709,
    Smpte170m,
    Bt470bg,
    Bt2020,
    /// `yuv420p` with the full-range flag set (`pc`).
    PcFlag,
}

impl Tag {
    fn filter(self) -> &'static str {
        match self {
            Tag::None => "",
            Tag::Bt709 => ",setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709",
            Tag::Smpte170m => ",setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m",
            Tag::Bt470bg => ",setparams=colorspace=bt470bg:color_primaries=bt470bg:color_trc=bt470bg",
            Tag::Bt2020 => ",setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
            Tag::PcFlag => ",setparams=range=pc",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Pat {
    Test,
    Noise,
    Bars,
    Edges,
    Ramp,
    /// 0 / 255 checkerboard in all planes: the overshoot of every negative lobe clips at both ends.
    Checker,
    /// 0 / 255 vertical stripes.
    Stripes,
    /// One bright luma sample in the middle of a flat picture: the table's weights, read back.
    Impulse,
}

/// A lossless clip (FFV1 in Matroska; JPEG frames for `yuvj420p`) of `w` x `h`, 0.6 s at 15 fps, cached by what it is made of and
/// the FFmpeg that made it.
fn media_with(w: u32, h: u32, pat: Pat, tag: Tag, pix: &str) -> Asset {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Asset>>> = OnceLock::new();
    let key = format!("{w}x{h}-{pat:?}-{tag:?}-{pix}");
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut cache = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(a) = cache.get(&key) {
        return a.clone();
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-sc-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.mkv", slug(&key)));
    if !path.exists() {
        let geq = |lum: &str, cb: &str, cr: &str| {
            format!("nullsrc=size={w}x{h}:rate=15:duration=0.6,format=yuv420p,geq=lum='{lum}':cb='{cb}':cr='{cr}'")
        };
        let src = match pat {
            Pat::Test => format!("testsrc2=size={w}x{h}:rate=15:duration=0.6"),
            Pat::Noise => geq("random(1)*255", "random(2)*255", "random(3)*255"),
            Pat::Bars => format!("smptehdbars=size={w}x{h}:rate=15:duration=0.6"),
            Pat::Ramp => geq(
                "clip(16+219*X/W,16,235)",
                "clip(128+110*(X/W-0.5)+20*sin(Y/9),16,240)",
                "clip(128-110*(X/W-0.5)+20*cos(Y/7),16,240)",
            ),
            Pat::Edges => geq(
                "if(mod(floor(X/8)+floor(Y/8),2),235,16)",
                "if(mod(floor(X/16),2),200,60)",
                "if(mod(floor(Y/16),2),190,70)",
            ),
            Pat::Checker => geq("255*mod(X+Y,2)", "255*mod(X+Y,2)", "255*mod(X+Y+1,2)"),
            Pat::Stripes => geq("255*mod(X,2)", "255*mod(X,2)", "255*mod(X+1,2)"),
            Pat::Impulse => geq("if(eq(X,floor(W/2))*eq(Y,floor(H/2)),255,40)", "128", "128"),
        };
        let lavfi = format!("{src}{}", tag.filter());
        let tmp = dir.join(format!("{}.tmp.mkv", slug(&key)));
        let codec: &[&str] = if pix == "yuvj420p" {
            &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0"]
        } else if w < 16 || h < 16 {
            &["-c:v", "rawvideo"]
        } else {
            &["-c:v", "ffv1"]
        };
        let mut args = vec!["-f", "lavfi", "-i", &lavfi];
        args.extend_from_slice(codec);
        args.extend_from_slice(&["-pix_fmt", pix, tmp.to_str().unwrap()]);
        ffmpeg(&args);
        std::fs::rename(&tmp, &path).unwrap();
    }
    let a = probed(&path);
    cache.insert(key, a.clone());
    a
}

fn src(w: u32, h: u32, pat: Pat) -> Asset {
    media_with(w, h, pat, Tag::None, "yuv420p")
}

/// A still picture (a PNG or a JPEG), cached.
fn photo(name: &str, size: &str, quality: &[&str]) -> Asset {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-sc-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    if !p.exists() {
        let lavfi = format!("testsrc2=size={size}:rate=1:duration=1");
        let tmp = dir.join(format!("tmp-{name}"));
        let mut args = vec!["-f", "lavfi", "-i", &lavfi, "-frames:v", "1"];
        args.extend(quality.iter().copied());
        args.push(tmp.to_str().unwrap());
        ffmpeg(&args);
        std::fs::rename(&tmp, &p).unwrap();
    }
    probed(&p)
}

// ---- the cases' builders -------------------------------------------------------------------------------------------

const NAMES: [&str; 4] = ["bilinear", "lanczos", "neighbor", "spline"];
const FPS: f64 = 15.0;
const FRAMES: usize = 6;

fn named(name: &str) -> ExportOptions {
    ExportOptions {
        scaler: Some(name.to_string()),
        fps: Some(FPS),
        ..ExportOptions::default()
    }
}

/// The steepest shrink per stage the plan draws a name to, as the docs state it: 42:1 for lanczos, 18:1 for the spline, 63:1 for the rest. (Written
/// out, not read from the plan, so that a limit that moves is a failing case and not a moved goalpost; a scratch patch that raises the plan's limits
/// to draw the refused frames anyway leaves the cases where they are.)
fn limit(name: &str) -> usize {
    let stated = match name {
        "lanczos" => 42,
        "spline" => 18,
        _ => 63,
    };
    if std::env::var_os("KERF_SC_LIMIT").is_none() {
        assert_eq!(
            kerf_core::max_shrink_for(Some(name)) as usize,
            stated,
            "{name}: the plan's limit is not the documented one"
        );
    }
    stated
}

fn into(a: &Asset, w: u32, h: u32, fit: Fit) -> Timeline {
    timeline(vec![vec![clip(a, 0.0, 0.4, 0.0)]], Some(Delivery::new(w, h, fit)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// Every frame drawn (and, as always, exact).
    Drawn,
    /// Every frame refused for a shrink past the limit.
    Shrink,
    /// Whatever the plan says; the drawn frames are exact.
    Any,
}

fn necessity_report() -> &'static Mutex<BTreeMap<String, String>> {
    static R: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// One case: the cut at the scaler `name`, swept; judged against `expect` (when no refusal is patched out).
fn run(fam: &mut Family, case: &str, tl: &Timeline, assets: &[Asset], name: &str, expect: Expect) -> Outcome {
    let opts = named(name);
    // (A cut FFmpeg itself cannot render -- a 2-sample picture's blurred fill on a build that refuses the box blur -- is nothing to hold a drawing to.)
    let played = match try_export(case, tl, assets, &opts) {
        Ok(played) => played,
        Err(e) => {
            assert_eq!(
                expect,
                Expect::Any,
                "{case}: FFmpeg cannot render a cut that must be drawn: {e}"
            );
            let first = e
                .lines()
                .find(|l| l.contains("Invalid") || l.contains("Error"))
                .unwrap_or("export failed");
            eprintln!("{case:<64} FFmpeg cannot render it: {first}");
            return Outcome::default();
        }
    };
    let out = sweep_played(case, tl, assets, &opts, FRAMES, played);
    fam.add(case, &out);
    if patched_out() {
        // (Refusals patched out: what the plan refused is drawn anyway, and what it no longer refuses is drawn as any frame is; a frame the
        // compositor has no table for is not drawn at all.)
        let line = format!(
            "{case:<72} frames {:>2}, refused {:>2}, drawn {:>2}, INEXACT {:>2}, undrawable {:>2}",
            out.frames,
            out.refused,
            out.drawn,
            out.bad.len(),
            out.frames - out.drawn
        );
        necessity_report().lock().unwrap().insert(case.to_string(), line);
        return out;
    }
    let bad = match expect {
        Expect::Drawn => out.refused != 0 || out.drawn != out.frames,
        Expect::Shrink => out.drawn != 0 || out.refused_for("steeper") != out.frames,
        Expect::Any => false,
    };
    if bad {
        fam.failures.push(format!(
            "{case}: expected {expect:?}; {} drawn, {} refused ({:?})",
            out.drawn, out.refused, out.reasons
        ));
    }
    out
}

fn finish(fam: Family, family: &str) {
    if patched_out() {
        for line in necessity_report().lock().unwrap().values() {
            eprintln!("NECESSITY {line}");
        }
        fam.finish_refusing(family);
    } else {
        fam.finish(family);
    }
}

// ---- 1. the limit of each name: at it, inside it, one past it ----------------------------------------------------

/// Each name at the steepest shrink the plan draws it to and one step past: 42:1 and 43:1 / 44:1 for lanczos, 18:1 and 19:1 / 20:1 for the
/// spline, 63:1 and 64:1 / 65:1 for the others. The sources have the delivery's aspect, so the fit is the one `scale`, luma and chroma.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_limits_at_and_one_past() {
    let mut fam = Family::default();
    for name in NAMES {
        let l = limit(name) as u32;
        // (the delivery, in units of the aspect 5:3 at 6 px; the sources are `l`, `l - 1`, `l + 1`, ... times it)
        let (dw, dh) = match name {
            "spline" => (70u32, 42u32),
            "lanczos" => (30, 18),
            _ => (20, 12),
        };
        for (what, sw, sh, expect) in [
            ("at the limit", l * dw, l * dh, Expect::Drawn),
            ("a sample under (odd)", l * dw - 1, l * dh - 1, Expect::Drawn),
            ("a sample under on one axis", l * dw, l * dh - 1, Expect::Drawn),
            ("a sample past on one axis", l * dw + 1, l * dh, Expect::Shrink),
            ("a sample past", l * dw + 1, l * dh + 1, Expect::Shrink),
            ("one ratio past", (l + 1) * dw, (l + 1) * dh, Expect::Shrink),
            ("two ratios past", (l + 2) * dw, (l + 2) * dh, Expect::Shrink),
        ] {
            let a = src(sw, sh, Pat::Noise);
            let tl = into(&a, dw, dh, Fit::Contain);
            let case = format!("checker/scalers/limit/{name}/{what} {sw}x{sh} to {dw}x{dh}");
            run(&mut fam, &case, &tl, std::slice::from_ref(&a), name, expect);
        }
        // The hard patterns at the limit itself (the intermediate clips at both ends there).
        for pat in [Pat::Checker, Pat::Stripes, Pat::Impulse, Pat::Edges, Pat::Ramp] {
            let a = src(l * dw, l * dh, pat);
            let tl = into(&a, dw, dh, Fit::Contain);
            let case = format!("checker/scalers/limit/{name}/{pat:?} at the limit");
            run(&mut fam, &case, &tl, std::slice::from_ref(&a), name, Expect::Drawn);
        }
    }
    finish(fam, "scalers: the limits");
}

/// The limit is judged **per stage**: a zoom out is the fit's `scale` and then the zoom's own, so a small picture enlarged into a big frame and
/// zoomed down to the limit is the second stage at the limit (and one factor past it, a stage past it). Cover is the same ratio on both axes.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_limit_in_the_zoom_stage_and_in_cover() {
    let mut fam = Family::default();
    let small = src(90, 54, Pat::Noise);
    let base = src(1260, 756, Pat::Ramp);
    for name in NAMES {
        // (the zoom factor whose stage is exactly at the limit with margin to truncate to it, and one past it)
        let (inside, past) = match name {
            "lanczos" => (0.0239, 0.0234),
            "spline" => (0.0558, 0.0525),
            _ => (0.0160, 0.0155),
        };
        for (what, s, expect) in [("at the limit", inside, Expect::Drawn), ("past it", past, Expect::Shrink)] {
            let c = pip(clip(&small, 0.0, 0.4, 0.0), s, 0.0, 0.0);
            let tl = timeline(
                vec![vec![clip(&base, 0.0, 0.4, 0.0)], vec![c]],
                Some(Delivery::new(1260, 756, Fit::Contain)),
            );
            let case = format!("checker/scalers/limit-stage/{name}/zoom stage {what} (scale {s})");
            let out = run(&mut fam, &case, &tl, &[base.clone(), small.clone()], name, Expect::Any);
            // (The layer under it is a Ramp at the frame's own size, drawn; the zoomed layer's stage is what is judged.)
            if !patched_out() {
                let past_refused = out.refused_for("steeper") == out.frames;
                if (expect == Expect::Shrink) != past_refused {
                    fam.failures.push(format!(
                        "{case}: expected {expect:?}, refused for a shrink in {} of {} frames ({:?})",
                        out.refused_for("steeper"),
                        out.frames,
                        out.reasons
                    ));
                }
            }
        }
        // Cover: the same ratio on both axes, at the limit, with the other side cropped.
        let l = limit(name) as u32;
        let (dw, dh) = (if name == "spline" { 70 } else { 20 }, 12u32);
        let a = src(l * dw, l * dh + 6 * l, Pat::Noise);
        let tl = into(&a, dw, dh, Fit::Cover);
        let case = format!(
            "checker/scalers/limit-stage/{name}/cover at the limit {}x{} to {dw}x{dh}",
            l * dw,
            l * dh + 6 * l
        );
        run(&mut fam, &case, &tl, std::slice::from_ref(&a), name, Expect::Drawn);
        let a = src(l * dw + 2, l * dh + 6 * l, Pat::Noise);
        let tl = into(&a, dw, dh, Fit::Cover);
        let case = format!(
            "checker/scalers/limit-stage/{name}/cover past the limit {}x{} to {dw}x{dh}",
            l * dw + 2,
            l * dh + 6 * l
        );
        run(&mut fam, &case, &tl, std::slice::from_ref(&a), name, Expect::Shrink);
    }
    finish(fam, "scalers: the limit per stage and in cover");
}

// ---- 2. enlargements to hundreds of times, tiny sources and tiny outputs ---------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_enlargements_and_tiny_outputs() {
    let mut fam = Family::default();
    for name in NAMES {
        for (sw, sh) in [
            (2, 2),
            (4, 4),
            (3, 3),
            (5, 4),
            (8, 8),
            (16, 16),
            (2, 90),
            (90, 2),
            (1, 64),
            (1, 1),
            (7, 9),
        ] {
            let a = src(sw, sh, Pat::Noise);
            for fit in [Fit::Contain, Fit::Cover, Fit::Blur] {
                let tl = into(&a, 64, 36, fit);
                let case = format!("checker/scalers/enlarge/{name}/{sw}x{sh} {fit:?}");
                run(&mut fam, &case, &tl, std::slice::from_ref(&a), name, Expect::Any);
            }
        }
        // 2x2 and 3x3 up to 640x360 (hundreds of times).
        for (sw, sh) in [(2, 2), (3, 3), (5, 7)] {
            let a = src(sw, sh, Pat::Noise);
            let tl = into(&a, 640, 360, Fit::Cover);
            let case = format!("checker/scalers/enlarge/{name}/{sw}x{sh} to 640x360");
            run(&mut fam, &case, &tl, std::slice::from_ref(&a), name, Expect::Any);
        }
        // Outputs of 1 and 2 samples: a picture scaled down in a frame (the layer is the output), 1 sample at 63:1 and below, 2 at 126:1.
        for (sw, sh, scale) in [
            (63u32, 63u32, 1.0 / 63.0),
            (62, 62, 1.0 / 62.0),
            (50, 50, 1.0 / 50.0),
            (40, 40, 1.0 / 40.0),
            (42, 42, 1.0 / 42.0),
            (18, 18, 1.0 / 18.0),
            (17, 17, 1.0 / 17.0),
            (36, 36, 1.0 / 36.0),
            (126, 126, 2.0 / 126.0),
            (84, 84, 2.0 / 84.0),
            (36, 36, 2.0 / 36.0),
            (21, 21, 2.0 / 21.0),
            (125, 70, 2.0 / 125.0),
            (83, 47, 2.0 / 83.0),
            (12, 12, 0.0002),
        ] {
            let a = src(sw, sh, Pat::Noise);
            let c = pip(clip(&a, 0.0, 0.4, 0.0), scale, 0.1, -0.1);
            let bg = src(64, 36, Pat::Ramp);
            let tl = timeline(
                vec![vec![clip(&bg, 0.0, 0.4, 0.0)], vec![c]],
                Some(Delivery::new(64, 36, Fit::Contain)),
            );
            let case = format!("checker/scalers/tiny-layer/{name}/{sw}x{sh} at {scale:.5}");
            run(&mut fam, &case, &tl, &[bg, a], name, Expect::Any);
        }
    }
    finish(fam, "scalers: enlargements and tiny outputs");
}

// ---- 3. odd sizes and ratios that are not whole -----------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % u64::from(hi - lo + 1)) as u32
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_odd_sizes_and_non_integer_ratios() {
    let mut fam = Family::default();
    let mut rng = Rng(0x5ca1e5);
    for name in NAMES {
        for i in 0..14 {
            // A source of any shape up to 700 wide and a frame (even sides: the export's encoder needs them) whose ratio to it is anything from
            // 1:1 to the name's limit.
            let dw = 2 * rng.range(3, 60);
            let dh = 2 * rng.range(3, 40);
            let ratio = f64::from(rng.range(105, (limit(name) as u32).min(40) * 100)) / 100.0;
            let sw = ((f64::from(dw) * ratio) as u32).max(3);
            let sh = ((f64::from(dh) * (ratio * f64::from(rng.range(70, 130)) / 100.0)) as u32).clamp(3, 700);
            let a = src(sw, sh, if i % 3 == 0 { Pat::Ramp } else { Pat::Noise });
            let fit = [Fit::Contain, Fit::Cover][(i % 2) as usize];
            let tl = into(&a, dw, dh, fit);
            let case = format!("checker/scalers/odd/{name}/{i} {sw}x{sh} to {dw}x{dh} {fit:?}");
            run(&mut fam, &case, &tl, std::slice::from_ref(&a), name, Expect::Any);
        }
    }
    finish(fam, "scalers: odd sizes and ratios that are not whole");
}

// ---- 4. fits, zooms and a keyed zoom, in several frames -----------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_fits_zooms_and_keyed_zoom() {
    let mut fam = Family::default();
    let wide = src(640, 360, Pat::Noise);
    let tall = src(360, 640, Pat::Test);
    let odd = src(641, 359, Pat::Ramp);
    let bars = src(320, 180, Pat::Bars);
    for name in NAMES {
        let case = |s: &str| format!("checker/scalers/fit/{name}/{s}");
        for (what, a, w, h, fit) in [
            ("contain 16:9 into 9:16", &wide, 180u32, 320u32, Fit::Contain),
            ("cover 16:9 into 9:16", &wide, 180, 320, Fit::Cover),
            ("contain 9:16 into 16:9", &tall, 320, 180, Fit::Contain),
            ("cover 9:16 into 16:9", &tall, 320, 180, Fit::Cover),
            ("odd source contain", &odd, 320, 180, Fit::Contain),
            ("odd source cover", &odd, 200, 150, Fit::Cover),
            ("enlarged contain", &bars, 640, 362, Fit::Contain),
            ("blurred fill", &tall, 320, 180, Fit::Blur),
        ] {
            let tl = into(a, w, h, fit);
            run(&mut fam, &case(what), &tl, std::slice::from_ref(a), name, Expect::Any);
        }
        // A zoom out (the fit then the zoom's own `scale`: two tables in cascade), a zoom in (folded into the fit), cropped and moved.
        let base = src(320, 180, Pat::Ramp);
        for scale in [0.9, 0.7, 0.45, 0.2, 1.3, 2.0] {
            let c = pip(clip(&wide, 0.0, 0.4, 0.0), scale, 0.15, -0.1);
            let tl = timeline(
                vec![vec![clip(&base, 0.0, 0.4, 0.0)], vec![c]],
                Some(Delivery::new(320, 180, Fit::Contain)),
            );
            run(
                &mut fam,
                &case(&format!("pip scale {scale}")),
                &tl,
                &[base.clone(), wide.clone()],
                name,
                Expect::Any,
            );
        }
        let mut cropped = pip(clip(&wide, 0.0, 0.4, 0.0), 0.6, 0.0, 0.0);
        cropped.transform.crop_left = 0.13;
        cropped.transform.crop_top = 0.07;
        cropped.transform.crop_right = 0.21;
        let tl = timeline(
            vec![vec![clip(&base, 0.0, 0.4, 0.0)], vec![cropped]],
            Some(Delivery::new(320, 180, Fit::Contain)),
        );
        run(
            &mut fam,
            &case("cropped pip"),
            &tl,
            &[base.clone(), wide.clone()],
            name,
            Expect::Any,
        );
        // Fades and a dissolve over the scaled layers.
        let mut fade = pip(clip(&wide, 0.0, 0.4, 0.0), 0.5, -0.1, 0.1);
        fade.fade_in = 0.2;
        let tl = timeline(
            vec![vec![clip(&base, 0.0, 0.4, 0.0)], vec![fade]],
            Some(Delivery::new(320, 180, Fit::Contain)),
        );
        run(
            &mut fam,
            &case("fade in on a pip"),
            &tl,
            &[base.clone(), wide.clone()],
            name,
            Expect::Any,
        );
        let first = clip(&wide, 0.0, 0.3, 0.0);
        let second = dissolve_into(clip(&odd, 0.0, 0.3, 0.2), TransitionKind::Crossfade, 0.1);
        let tl = timeline(vec![vec![first, second]], Some(Delivery::new(320, 180, Fit::Contain)));
        run(
            &mut fam,
            &case("dissolve between a fit and a fit"),
            &tl,
            &[wide.clone(), odd.clone()],
            name,
            Expect::Any,
        );
    }
    finish(fam, "scalers: fits and zooms");
}

/// A keyed zoom: a table per frame, several frames (zooming out, in, and panning while it does).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_keyed_zoom_frames() {
    let mut fam = Family::default();
    let wide = src(640, 360, Pat::Noise);
    let base = src(320, 180, Pat::Bars);
    let ramp = src(480, 270, Pat::Ramp);
    for name in NAMES {
        for (what, a, keys) in [
            (
                "out 1.0 to 0.4",
                &wide,
                vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.9, 0.4, 0.0, 0.0)],
            ),
            (
                "out and pan",
                &wide,
                vec![zoom_key(0.0, 0.9, -0.2, 0.1), zoom_key(0.9, 0.3, 0.25, -0.15)],
            ),
            (
                "in 0.5 to 1.6",
                &ramp,
                vec![zoom_key(0.0, 0.5, 0.0, 0.0), zoom_key(0.9, 1.6, 0.1, 0.0)],
            ),
            (
                "three keys",
                &wide,
                vec![
                    zoom_key(0.0, 0.8, 0.0, 0.0),
                    zoom_key(0.45, 0.25, 0.1, 0.1),
                    zoom_key(0.9, 0.7, -0.1, 0.0),
                ],
            ),
        ] {
            let mut c = clip(a, 0.0, 0.9, 0.0);
            c.keyframes = keys;
            let tl = timeline(
                vec![vec![clip(&base, 0.0, 0.9, 0.0)], vec![c]],
                Some(Delivery::new(320, 180, Fit::Contain)),
            );
            let case = format!("checker/scalers/keyed-zoom/{name}/{what}");
            let out = sweep(&case, &tl, &[base.clone(), a.clone()], &named(name), 13);
            fam.add(&case, &out);
            if !patched_out() && out.drawn + out.refused != out.frames {
                fam.failures.push(format!(
                    "{case}: {} of {} frames neither drawn nor refused",
                    out.frames - out.drawn - out.refused,
                    out.frames
                ));
            }
            if patched_out() {
                necessity_report().lock().unwrap().insert(
                    case.clone(),
                    format!(
                        "{case:<64} refused {:>2} of {:>2}, drawn {:>2}, INEXACT {:>2}",
                        out.refused,
                        out.frames,
                        out.drawn,
                        out.bad.len()
                    ),
                );
            }
        }
    }
    finish(fam, "scalers: keyed zoom");
}

// ---- 5. matrices, ranges and pixel formats ---------------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_matrices_ranges_and_pixel_formats() {
    let mut fam = Family::default();
    for name in NAMES {
        let case = |s: &str| format!("checker/scalers/format/{name}/{s}");
        for tag in [Tag::None, Tag::Bt709, Tag::Smpte170m, Tag::Bt470bg, Tag::Bt2020, Tag::PcFlag] {
            let a = media_with(480, 270, Pat::Noise, tag, "yuv420p");
            let tl = into(&a, 240, 136, Fit::Contain);
            run(
                &mut fam,
                &case(&format!("{tag:?} 480x270 to 240x136")),
                &tl,
                std::slice::from_ref(&a),
                name,
                Expect::Any,
            );
            // ... zoomed out over a tagged background (the composite's matrix is the bottom layer's).
            let bg = media_with(240, 136, Pat::Ramp, Tag::Bt709, "yuv420p");
            let c = pip(clip(&a, 0.0, 0.4, 0.0), 0.5, 0.2, 0.1);
            let tl = timeline(
                vec![vec![clip(&bg, 0.0, 0.4, 0.0)], vec![c]],
                Some(Delivery::new(240, 136, Fit::Contain)),
            );
            run(
                &mut fam,
                &case(&format!("{tag:?} pip over bt709")),
                &tl,
                &[bg, a],
                name,
                Expect::Any,
            );
        }
        // Untagged HD beside a clip that names a matrix (FFmpeg labels it BT.709).
        let hd = media_with(1280, 720, Pat::Noise, Tag::None, "yuv420p");
        let sd = media_with(320, 180, Pat::Ramp, Tag::Smpte170m, "yuv420p");
        let c = pip(clip(&hd, 0.0, 0.4, 0.0), 0.5, 0.0, 0.0);
        let tl = timeline(
            vec![vec![clip(&sd, 0.0, 0.4, 0.0)], vec![c]],
            Some(Delivery::new(320, 180, Fit::Contain)),
        );
        run(
            &mut fam,
            &case("untagged HD labelled bt709 beside bt601"),
            &tl,
            &[sd.clone(), hd.clone()],
            name,
            Expect::Any,
        );
        // Pixel formats: the plan refuses to resize what FFmpeg scales in another format; whatever it draws is exact.
        for pix in ["yuvj420p", "yuv422p", "yuv444p", "gray", "gbrp", "yuva420p"] {
            let a = media_with(320, 180, Pat::Noise, Tag::None, pix);
            let tl = into(&a, 160, 90, Fit::Contain);
            run(
                &mut fam,
                &case(&format!("{pix} to 160x90")),
                &tl,
                std::slice::from_ref(&a),
                name,
                Expect::Any,
            );
            let tl = into(&a, 360, 202, Fit::Cover);
            run(
                &mut fam,
                &case(&format!("{pix} to 360x202")),
                &tl,
                std::slice::from_ref(&a),
                name,
                Expect::Any,
            );
        }
    }
    finish(fam, "scalers: matrices, ranges and pixel formats");
}

/// The still's own `yuv444p` planes against the compositor's composite put through the same conversion: the number of
/// differing samples and the largest difference (the comparator `tests/parity.rs` makes for still plans, copied here).
fn still_planes_differ(
    plan: &kerf_core::RenderPlan,
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
) -> (usize, i32) {
    let (_, _, want, got) = still_planes(plan, tl, assets, opts, t);
    let (mut n, mut max) = (0, 0);
    for (a, b) in got.iter().zip(&want) {
        let d = (i32::from(*a) - i32::from(*b)).abs();
        n += usize::from(d != 0);
        max = max.max(d);
    }
    (n, max)
}

/// The still's `yuv444p` planes (`want`) and the compositor's composite put through the same conversion (`got`), with the size.
fn still_planes(
    plan: &kerf_core::RenderPlan,
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
) -> (u32, u32, Vec<u8>, Vec<u8>) {
    use std::io::Write;
    let size = plan.size(u32::MAX);
    let (w, h, want) =
        kerf_core::timeline_frame_yuv_cancellable(tl, assets, opts, t, size.0, &|| false).expect("the FFmpeg still's planes");
    assert_eq!((w, h), size);
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    let drawn = compositor().composite_yuv(plan, &frames, size).expect("the GPU's planes");
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
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&raw);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len(), want.len());
    (w, h, want, out.stdout)
}

#[derive(Default, Debug)]
struct StillOutcome {
    drawn: usize,
    refused: usize,
    bad: Vec<(f64, usize, i32)>,
    reasons: BTreeMap<String, usize>,
}

/// Each time `times` of the cut as a Still plan: refused (counted with the reasons), or drawn and held to the still's planes.
fn still_sweep(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, times: &[f64]) -> StillOutcome {
    let mut out = StillOutcome::default();
    let mut log = String::new();
    for &t in times {
        let plan = kerf_core::RenderPlan::at(tl, assets, opts, t, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let why = plan.reasons(&compositor().caps(), size);
        no_geq_refusal(case, &why);
        if !why.is_empty() {
            out.refused += 1;
            let names: BTreeSet<String> = why.iter().map(ToString::to_string).collect();
            for n in &names {
                *out.reasons.entry(n.clone()).or_insert(0) += 1;
            }
            let _ = writeln!(log, "{t}\trefused\t{}", names.into_iter().collect::<Vec<_>>().join(" | "));
            continue;
        }
        out.drawn += 1;
        let tags = frame_tags(&plan);
        let (n, max) = still_planes_differ(&plan, tl, assets, opts, t);
        if n > 0 {
            out.bad.push((t, n, max));
            let _ = writeln!(log, "{t}\tINEXACT\t{n} samples differ, max {max}{tags}");
        } else {
            let _ = writeln!(log, "{t}\texact{tags}");
        }
    }
    let _ = std::fs::write(frame_log_dir().join(format!("still-{}.frames", slug(case))), &log);
    let mut line = format!(
        "{case:<58} {:>2} times: {:>2} drawn ({:>2} exact), {:>2} refused",
        times.len(),
        out.drawn,
        out.drawn - out.bad.len(),
        out.refused
    );
    if !out.bad.is_empty() {
        let _ = write!(line, " | INEXACT at {:?}", out.bad);
    }
    if !out.reasons.is_empty() {
        let mut why: Vec<String> = out.reasons.iter().map(|(r, n)| format!("{n}x {r}")).collect();
        why.truncate(3);
        let _ = write!(line, " | refused: {}", why.join("; "));
    }
    eprintln!("{line}");
    report_lines().lock().unwrap().insert(format!("still {case}"), line);
    write_report();
    out
}

impl Family {
    fn add_still(&mut self, case: &str, out: &StillOutcome) {
        self.cases += 1;
        self.drawn += out.drawn;
        self.refused += out.refused;
        if let Some((t, n, max)) = out.bad.first() {
            self.failures.push(format!(
                "{case}: {}/{} drawn stills inexact, first at {t} s ({n} samples, max {max})",
                out.bad.len(),
                out.drawn
            ));
        }
    }
}
fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as u8
        })
        .collect()
}

/// Which planes of a source a pattern fills: noise over the whole range, a 0 / 255 checkerboard (overshoot clips at both ends),
/// vertical 0 / 255 stripes, a single bright impulse on black, all 255, all 0.
#[derive(Clone, Copy, Debug)]
enum Fill {
    Noise,
    Checker,
    Stripes,
    Impulse,
    White,
    Black,
}

fn plane_of(fill: Fill, w: usize, h: usize, seed: u64) -> Vec<u8> {
    match fill {
        Fill::Noise => lcg_bytes(seed, w * h),
        Fill::Checker => (0..w * h)
            .map(|i| if (i % w + i / w).is_multiple_of(2) { 255 } else { 0 })
            .collect(),
        Fill::Stripes => (0..w * h).map(|i| if (i % w).is_multiple_of(2) { 255 } else { 0 }).collect(),
        Fill::Impulse => (0..w * h).map(|i| if i == (h / 2) * w + w / 2 { 255 } else { 0 }).collect(),
        Fill::White => vec![255; w * h],
        Fill::Black => vec![0; w * h],
    }
}

fn run_scale(raw: &[u8], (sw, sh): (usize, usize), (dw, dh): (usize, usize), flags: &str, threads: &str) -> Vec<u8> {
    use std::io::Write;
    let size = format!("{sw}x{sh}");
    let graph = format!("[0:v]scale={dw}:{dh}:flags={flags}[o]");
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
            &size,
        ])
        .args(["-i", "pipe:0", "-filter_complex_threads", threads, "-filter_complex", &graph])
        .args([
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data = raw.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(
        out.status.success(),
        "scale {size} -> {dw}x{dh}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// `Err(description)` when the model and FFmpeg's `scale` differ in any plane.
fn model_vs_ffmpeg(
    fill: Fill,
    src: (usize, usize),
    dst: (usize, usize),
    flag: kerf_gpu::sws::Scaler,
    threads: &str,
) -> Result<(), String> {
    use kerf_gpu::scaler::scale_plane;
    let model = kerf_gpu::scaler::kernels().model();
    let (sw, sh) = src;
    let (cw, ch) = (sw.div_ceil(2), sh.div_ceil(2));
    let (dw, dh) = dst;
    let (ocw, och) = (dw.div_ceil(2), dh.div_ceil(2));
    let seed = (sw * 31 + sh * 7 + dw * 3 + dh) as u64;
    let y = plane_of(fill, sw, sh, seed);
    let u = plane_of(fill, cw, ch, seed + 1);
    let v = plane_of(fill, cw, ch, seed + 2);
    let mut raw = y.clone();
    raw.extend_from_slice(&u);
    raw.extend_from_slice(&v);
    let flags = match flag {
        kerf_gpu::sws::Scaler::Bicubic => "bicubic",
        kerf_gpu::sws::Scaler::Bilinear => "bilinear",
        kerf_gpu::sws::Scaler::Lanczos => "lanczos",
        kerf_gpu::sws::Scaler::Spline => "spline",
        kerf_gpu::sws::Scaler::Neighbor => "neighbor",
    };
    let want = run_scale(&raw, src, dst, flags, threads);
    assert_eq!(want.len(), dw * dh + 2 * ocw * och, "{src:?} -> {dst:?}: size");
    let (wy, rest) = want.split_at(dw * dh);
    let (wu, wv) = rest.split_at(ocw * och);
    let mut errors = Vec::new();
    for (name, plane, from, to, rows, expect) in [
        ("Y", &y, (sw, sh), (dw, dh), 2, wy),
        ("U", &u, (cw, ch), (ocw, och), 1, wu),
        ("V", &v, (cw, ch), (ocw, och), 1, wv),
    ] {
        let Some(got) = scale_plane(plane, from, to, flag, model.kernel, rows) else {
            errors.push(format!("{name}: the model has no table"));
            continue;
        };
        let d = diff_plane(expect, &got, to.0);
        if d.differing > 0 {
            errors.push(format!("{name}: {} differ, max {}, first {:?}", d.differing, d.max, d.first));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{fill:?} {src:?} -> {dst:?} {flags} (filter threads {threads}): {}",
            errors.join("; ")
        ))
    }
}

// ---- 6. Still plans ---------------------------------------------------------------------------------------------------------------

fn still_opts(name: &str) -> ExportOptions {
    ExportOptions {
        scaler: Some(name.to_string()),
        ..ExportOptions::default()
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_still_plans() {
    let mut fam = Family::default();
    let wide = src(640, 360, Pat::Noise);
    let tall = src(360, 640, Pat::Edges);
    let odd = src(321, 181, Pat::Ramp);
    let base = src(320, 180, Pat::Bars);
    for name in NAMES {
        let opts = still_opts(name);
        let case = |s: &str| format!("checker/scalers/still/{name}/{s}");
        let add = |fam: &mut Family, label: String, tl: &Timeline, assets: &[Asset], times: &[f64]| {
            let out = still_sweep(&label, tl, assets, &opts, times);
            fam.add_still(&label, &out);
        };
        for (what, a, w, h, fit) in [
            ("contain 16:9 into 9:16", &wide, 180u32, 320u32, Fit::Contain),
            ("cover 16:9 into 9:16", &wide, 180, 320, Fit::Cover),
            ("cover 9:16 into 16:9", &tall, 320, 180, Fit::Cover),
            ("odd contain", &odd, 200, 150, Fit::Contain),
            ("odd cover", &odd, 160, 90, Fit::Cover),
            ("blurred fill", &tall, 320, 180, Fit::Blur),
            ("enlarged cover", &base, 642, 362, Fit::Cover),
        ] {
            let tl = into(a, w, h, fit);
            add(&mut fam, case(what), &tl, std::slice::from_ref(a), &[0.0, 0.2, 0.37]);
        }
        let l = limit(name) as u32;
        let (dw, dh) = match name {
            "spline" => (70u32, 42u32),
            "lanczos" => (30, 18),
            _ => (20, 12),
        };
        for (what, sw, sh) in [
            ("at the limit", l * dw, l * dh),
            ("odd under", l * dw - 1, l * dh - 1),
            ("past", l * dw + 1, l * dh + 1),
        ] {
            let a = src(sw, sh, Pat::Noise);
            let tl = into(&a, dw, dh, Fit::Contain);
            add(
                &mut fam,
                case(&format!("{what} {sw}x{sh}")),
                &tl,
                std::slice::from_ref(&a),
                &[0.0, 0.3],
            );
        }
        // A zoom out over a picture, a zoom with a crop, and a keyed zoom at instants through it.
        for scale in [0.8, 0.5, 0.3, 0.07] {
            let c = pip(clip(&wide, 0.0, 0.9, 0.0), scale, 0.1, -0.2);
            let tl = timeline(
                vec![vec![clip(&base, 0.0, 0.9, 0.0)], vec![c]],
                Some(Delivery::new(320, 180, Fit::Contain)),
            );
            add(
                &mut fam,
                case(&format!("pip {scale}")),
                &tl,
                &[base.clone(), wide.clone()],
                &[0.0, 0.4],
            );
        }
        let mut keyed = clip(&wide, 0.0, 0.9, 0.0);
        keyed.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.9, 0.3, 0.2, -0.1)];
        let tl = timeline(
            vec![vec![clip(&base, 0.0, 0.9, 0.0)], vec![keyed]],
            Some(Delivery::new(320, 180, Fit::Contain)),
        );
        add(
            &mut fam,
            case("keyed zoom out"),
            &tl,
            &[base.clone(), wide.clone()],
            &[0.0, 0.1, 0.25, 0.45, 0.6, 0.75, 0.89],
        );
        let mut keyed = clip(&odd, 0.0, 0.9, 0.0);
        keyed.keyframes = vec![zoom_key(0.0, 0.4, -0.2, 0.2), zoom_key(0.9, 1.5, 0.1, 0.0)];
        let tl = timeline(
            vec![vec![clip(&base, 0.0, 0.9, 0.0)], vec![keyed]],
            Some(Delivery::new(320, 180, Fit::Contain)),
        );
        add(
            &mut fam,
            case("keyed zoom in, odd source"),
            &tl,
            &[base.clone(), odd.clone()],
            &[0.0, 0.1, 0.25, 0.45, 0.6, 0.75, 0.89],
        );
        // Tiny outputs: 1 and 2 samples.
        for (sw, scale) in [
            (63u32, 1.0 / 63.0),
            (40, 1.0 / 40.0),
            (36, 2.0 / 36.0),
            (18, 1.0 / 18.0),
            (20, 0.001),
        ] {
            let a = src(sw, sw, Pat::Noise);
            let bg = src(64, 36, Pat::Ramp);
            let tl = timeline(
                vec![
                    vec![clip(&bg, 0.0, 0.4, 0.0)],
                    vec![pip(clip(&a, 0.0, 0.4, 0.0), scale, 0.0, 0.0)],
                ],
                Some(Delivery::new(64, 36, Fit::Contain)),
            );
            add(
                &mut fam,
                case(&format!("tiny layer {sw} at {scale:.5}")),
                &tl,
                &[bg, a],
                &[0.0, 0.2],
            );
        }
    }
    finish(fam, "scalers: still plans");
}

// ---- 7. moving stills and the fit picture FFmpeg makes ----------------------------------------------------------------------------

/// A moving still (Ken Burns), a still placed by a zoom and a plain still, in a delivery that names each scaler. The two kinds of picture the
/// plan lets FFmpeg make under a named scaler (`StillMotion`) are refused; the drawn frames are exact. With a refusal patched out
/// (`KERF_CHECKER_ALLOW`) the refused frames are drawn anyway and counted.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_moving_stills_and_fit_copies() {
    let mut fam = Family::default();
    let png = photo("sc-photo-16x9.png", "1920x1080", &["-pix_fmt", "rgb24"]);
    let jpeg = photo("sc-photo-3x2.jpg", "1920x1280", &["-q:v", "3", "-pix_fmt", "yuvj420p"]);
    let jpeg_odd = photo("sc-photo-odd.jpg", "1501x1003", &["-q:v", "3", "-pix_fmt", "yuvj420p"]);
    let key = |t: f64, s: f64, x: f64, y: f64| zoom_key(t, s, x, y);
    for name in NAMES {
        let case = |s: &str| format!("checker/scalers/moving-still/{name}/{s}");
        for (what, a, w, h, fit) in [
            ("png 16:9 cover", &png, 640u32, 360u32, Fit::Cover),
            ("jpeg 3:2 cover into 16:9", &jpeg, 640, 360, Fit::Cover),
            ("jpeg 3:2 contain into 16:9", &jpeg, 640, 360, Fit::Contain),
            ("odd jpeg cover", &jpeg_odd, 480, 270, Fit::Cover),
        ] {
            let mut moving = clip(a, 0.0, 0.9, 0.0);
            moving.keyframes = vec![key(0.0, 1.2, 0.04, 0.03), key(0.9, 1.0, 0.0, 0.0)];
            let tl = timeline(vec![vec![moving]], Some(Delivery::new(w, h, fit)));
            let out = sweep(
                &case(&format!("{what}, ken burns")),
                &tl,
                std::slice::from_ref(a),
                &named(name),
                12,
            );
            fam.add(&case(&format!("{what}, ken burns")), &out);
            if !patched_out() && out.drawn > 0 && out.reasons.keys().all(|r| !r.contains(name)) {
                fam.failures.push(format!(
                    "{what}: frames drawn without a refusal naming {name}: {:?}",
                    out.reasons
                ));
            }
            // A still placed by a zoom (a fit picture FFmpeg makes) and the plain one.
            let mut placed = clip(a, 0.0, 0.9, 0.0);
            placed.transform.scale = 0.8;
            placed.transform.pos_x = 0.1;
            let tl = timeline(vec![vec![placed]], Some(Delivery::new(w, h, fit)));
            let out = sweep(
                &case(&format!("{what}, placed 0.8")),
                &tl,
                std::slice::from_ref(a),
                &named(name),
                12,
            );
            fam.add(&case(&format!("{what}, placed 0.8")), &out);
            let tl = timeline(vec![vec![clip(a, 0.0, 0.9, 0.0)]], Some(Delivery::new(w, h, fit)));
            let out = sweep(
                &case(&format!("{what}, plain")),
                &tl,
                std::slice::from_ref(a),
                &named(name),
                12,
            );
            fam.add(&case(&format!("{what}, plain")), &out);
        }
    }
    // (Refused wholesale in some cases: this family is about the drawn frames being exact, and about the refusals being named.)
    fam.finish_refusing("scalers: moving stills and fit copies");
}

// ---- 8. the names: strangers, a name the probe did not prove, and a probe that proved some -----------------------------------------------

fn plan_at(tl: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> kerf_core::RenderPlan {
    kerf_core::RenderPlan::at(tl, assets, opts, t, policy()).expect("plan")
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_names_the_engine_does_not_list_and_names_not_proven() {
    use kerf_gpu::scaler::{Kernels, Model};
    let a = src(320, 180, Pat::Noise);
    let tl = into(&a, 160, 90, Fit::Contain);
    let size = (160u32, 90u32);
    let real = match kerf_gpu::scaler::kernels() {
        Kernels::Match(m) => m,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        real.named,
        kerf_core::ScalerKinds::ALL,
        "the probe proves every name on this FFmpeg"
    );
    // A spelling the engine's list does not have is refused as a scaler whatever the caps say (the engine writes no flags for it), except the
    // case of `bicubic` (the default is what it draws).
    for stranger in [
        "Lanczos",
        "LANCZOS",
        "Spline",
        "NEIGHBOR",
        "area",
        "gauss",
        "sinc",
        "x",
        "",
        "lanczos ",
        "bilinear:x=1",
        "fast_bilinear",
        "bicublin",
    ] {
        let plan = plan_at(&tl, std::slice::from_ref(&a), &still_opts(stranger), 0.1);
        let mut caps = compositor().caps();
        caps.scalers = kerf_core::ScalerKinds::ALL;
        let why = plan.reasons(&caps, size);
        assert!(
            why.iter()
                .any(|u| matches!(u, kerf_core::Unsupported::Scaler(s) if s == stranger)),
            "{stranger:?} is drawn: {why:?}"
        );
    }
    for spelled in ["Bicubic", "BICUBIC", "bicubic"] {
        let plan = plan_at(&tl, std::slice::from_ref(&a), &still_opts(spelled), 0.1);
        assert!(plan.reasons(&compositor().caps(), size).is_empty(), "{spelled}");
    }
    // A name not proven is refused, and only that name: a compositor pinned to each subset the probe could have come back with.
    for missing in NAMES {
        let proven = kerf_core::ScalerKinds::ALL.without(kerf_core::ScalerKinds::of_name(missing).unwrap());
        let comp = pinned(Kernels::Match(Model { named: proven, ..real }));
        for name in NAMES {
            let plan = plan_at(&tl, std::slice::from_ref(&a), &still_opts(name), 0.1);
            let why = comp.caps();
            let reasons = plan.reasons(&why, size);
            let refused = reasons
                .iter()
                .any(|u| matches!(u, kerf_core::Unsupported::Scaler(s) if s == name));
            assert_eq!(refused, name == missing, "{name} with {missing} unproven: {reasons:?}");
            // ... and the compositor itself will not draw it either (the plan is not the only gate).
            let drawn = kerf_gpu::decode_layers(&plan.layers).map(|frames| comp.composite_yuv(&plan, &frames, size));
            match drawn {
                Ok(Ok(_)) => assert_ne!(name, missing, "{missing} unproven but drawn"),
                Ok(Err(e)) => assert_eq!(name, missing, "{name} with {missing} unproven: {e}"),
                Err(e) => panic!("decode: {e}"),
            }
        }
    }
    // A kernel that is not the model (or not known) leaves every name to FFmpeg, bicubic included.
    for k in [Kernels::Differs, Kernels::Unknown] {
        let comp = pinned(k);
        assert_eq!(comp.caps().scalers, kerf_core::ScalerKinds::NONE);
        for name in NAMES.into_iter().chain(["bicubic"]) {
            let plan = plan_at(&tl, std::slice::from_ref(&a), &still_opts(name), 0.1);
            assert!(!plan.reasons(&comp.caps(), size).is_empty(), "{name} under {k:?}");
        }
    }
}

/// With each name unproven on a pinned compositor, an export is drawn exactly where it is drawn and the name's frames are FFmpeg's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_a_name_not_proven_refuses_only_its_own_frames() {
    use kerf_gpu::scaler::{Kernels, Model};
    let real = match kerf_gpu::scaler::kernels() {
        Kernels::Match(m) => m,
        other => panic!("{other:?}"),
    };
    let a = src(480, 270, Pat::Noise);
    let tl = into(&a, 240, 136, Fit::Contain);
    for missing in NAMES {
        let proven = kerf_core::ScalerKinds::ALL.without(kerf_core::ScalerKinds::of_name(missing).unwrap());
        let comp = pinned(Kernels::Match(Model { named: proven, ..real }));
        for name in NAMES {
            let case = format!("checker/scalers/unproven/{missing}/delivery names {name}");
            let opts = named(name);
            let played = export(&case, &tl, std::slice::from_ref(&a), &opts);
            let out = sweep_played_on(&comp, &case, &tl, std::slice::from_ref(&a), &opts, FRAMES, played);
            assert!(out.bad.is_empty(), "{case}: {:?}", out.bad);
            if name == missing {
                assert_eq!((out.drawn, out.refused), (0, FRAMES), "{case}");
                assert!(
                    out.reasons.keys().any(|r| r.contains("not measured")),
                    "{case}: {:?}",
                    out.reasons
                );
            } else {
                assert_eq!((out.drawn, out.refused), (FRAMES, 0), "{case}: {:?}", out.reasons);
            }
        }
    }
}

// ---- 9. the model against FFmpeg's `scale`, plane by plane: limits, the band past them, extremes, threads ---------------------------------

fn flag_of(name: &str) -> kerf_gpu::sws::Scaler {
    kerf_gpu::scaler::named(name).expect("a name the engine lists")
}

fn this_model_is_exact() {
    assert!(
        kerf_gpu::scaler::kernels().exact(),
        "this FFmpeg's scaler is not the model: {:?}",
        kerf_gpu::scaler::kernels()
    );
    assert_eq!(kerf_gpu::scaler::kernels().named(), kerf_core::ScalerKinds::ALL);
}

fn all_fills() -> [Fill; 6] {
    [
        Fill::Noise,
        Fill::Checker,
        Fill::Stripes,
        Fill::Impulse,
        Fill::White,
        Fill::Black,
    ]
}

/// The scale sizes: at each name's limit and just inside, with odd sides and luma/chroma at either side of it; 1- and 2-sample outputs;
/// enlargements to hundreds of times; ratios that are not whole.
fn size_list(name: &str) -> Vec<((usize, usize), (usize, usize))> {
    let l = limit(name);
    let mut sizes = Vec::new();
    for dst in [1usize, 2, 3, 4, 5, 7, 8, 11, 16, 17, 30, 31, 33, 64, 97] {
        for src in [l * dst, l * dst - 1, (l * dst).saturating_sub(l / 2), l * dst * 9 / 10 + 1] {
            if src > dst + 1 && src <= 900 {
                sizes.push(((src, 24), (dst, 24)));
                sizes.push(((24, src), (24, dst)));
            }
        }
    }
    for (s, d) in [
        (641usize, 331usize),
        (1000, 999),
        (999, 1000),
        (333, 331),
        (65, 64),
        (64, 65),
        (17, 16),
        (16, 17),
        (7, 4),
        (9, 2),
        (3, 2),
        (5, 3),
    ] {
        sizes.push(((s, 40), (d, 40)));
        sizes.push(((40, s), (40, d)));
        sizes.push(((s.min(300), s.min(300)), (d.min(300), d.min(300))));
    }
    // Within swscale's "unscaled" window (|inc - 1| under 10/65536 of a sample step: the filter is the identity though the sizes differ by one).
    for (s, d) in [(8000usize, 7999usize), (7999, 8000), (9000, 8999), (8191, 8192)] {
        sizes.push(((s, 4), (d, 4)));
        sizes.push(((4, s), (4, d)));
    }
    for (s, d) in [
        (2usize, 640usize),
        (3, 500),
        (4, 333),
        (5, 640),
        (16, 640),
        (31, 640),
        (64, 1000),
        (1, 5),
        (1, 2),
    ] {
        sizes.push(((s, 6), (d, 12)));
        sizes.push(((6, s), (12, d)));
    }
    sizes
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_scalers_model_against_ffmpeg_limits_and_extremes() {
    this_model_is_exact();
    let mut failures = Vec::new();
    let mut n = 0;
    for name in NAMES {
        let flag = flag_of(name);
        for (srcs, dst) in size_list(name) {
            if srcs.0 > limit(name) * dst.0 || srcs.1 > limit(name) * dst.1 {
                continue;
            }
            for fill in all_fills() {
                if !matches!(fill, Fill::Noise) && srcs.0 * srcs.1 > 100_000 {
                    continue;
                }
                n += 1;
                if let Err(e) = model_vs_ffmpeg(fill, srcs, dst, flag, "1") {
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
    }
    eprintln!("scaler model: {n} scales, {} differ", failures.len());
    for f in failures.iter().take(60) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} of {n} scales differ from FFmpeg's", failures.len());
}

/// 1, 4 and 16 filter threads: the slices of the output the threads cut do not move a sample.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_scalers_model_against_ffmpeg_at_1_4_and_16_threads() {
    this_model_is_exact();
    let mut failures = Vec::new();
    let mut n = 0;
    for threads in ["1", "4", "16"] {
        for name in NAMES {
            let flag = flag_of(name);
            let l = limit(name);
            for (srcs, dst) in [
                ((l * 24, 240), (24, 240)),
                ((l * 24 - 1, 239), (24, 239)),
                ((240, l * 24), (240, 24)),
                ((241, l * 24 - 1), (240, 24)),
                ((640, 480), (333, 251)),
                ((333, 251), (640, 480)),
                ((1280, 720), (1279, 719)),
                ((321, 181), (320, 180)),
                ((1280, 720), (97, 55)),
                ((100, 60), (1000, 600)),
            ] {
                for fill in [Fill::Noise, Fill::Checker, Fill::Impulse] {
                    n += 1;
                    if let Err(e) = model_vs_ffmpeg(fill, srcs, dst, flag, threads) {
                        failures.push(format!("{name}: {e}"));
                    }
                }
            }
        }
    }
    eprintln!("scaler model at 1/4/16 threads: {n} scales, {} differ", failures.len());
    for f in failures.iter().take(60) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} of {n} scales differ from FFmpeg's", failures.len());
}

/// The band between the plan's limit (an integer ratio under the first one where swscale cascades) and the cascade: the model has a table
/// there and FFmpeg's `scale` is not cascaded, so the model equals it. **The plan refuses these frames though they are exact**: reported.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_scalers_band_between_the_plan_limit_and_the_cascade() {
    this_model_is_exact();
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for (name, dst, srcs) in [
        ("lanczos", 30usize, vec![1261usize, 1275, 1290, 1299]),
        ("lanczos", 7, vec![295, 298, 301]),
        ("lanczos", 97, vec![4075, 4100, 4150, 4200]),
        ("spline", 70, vec![1261, 1295, 1330, 1365]),
        ("spline", 12, vec![217, 228, 233]),
        ("spline", 33, vec![595, 620, 640]),
        ("bilinear", 20, vec![1261, 1280, 1500, 2000, 2500]),
        ("neighbor", 20, vec![1261, 1280, 1500, 2000, 2500, 3000]),
    ] {
        let flag = flag_of(name);
        for src in srcs {
            let ratio = src as f64 / dst as f64;
            let mut exact = true;
            let mut note = String::new();
            for fill in [Fill::Noise, Fill::Checker] {
                match model_vs_ffmpeg(fill, (src, 36), (dst, 36), flag, "1") {
                    Ok(()) => {}
                    Err(e) => {
                        exact = false;
                        note = e;
                    }
                }
            }
            report.push(format!(
                "{name} {src}->{dst} ({ratio:.2}:1, plan limit {}): model {}",
                limit(name),
                if exact { "EQUALS FFmpeg" } else { "differs / has no table" }
            ));
            if !exact && ratio < 43.0 && name == "lanczos" || !exact && ratio < 19.5 && name == "spline" {
                failures.push(note);
            }
        }
    }
    for r in &report {
        eprintln!("band: {r}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---- 10. where swscale cascades: the model's last table against FFmpeg's own log -----------------------------------------------------

/// Whether FFmpeg cascades a second scaler for this scale: its own debug log says `sws: initFilter failed` (`RETCODE_USE_CASCADE`).
fn ffmpeg_cascades(name: &str, (sw, sh): (u32, u32), (dw, dh): (u32, u32)) -> bool {
    let input = format!("nullsrc=s={sw}x{sh}:d=0.04,format=yuv420p");
    let vf = format!("scale={dw}:{dh}:flags={name}");
    let out = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-nostdin",
            "-v",
            "debug",
            "-f",
            "lavfi",
            "-i",
            &input,
            "-vf",
            &vf,
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success() || String::from_utf8_lossy(&out.stderr).contains("initFilter"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).contains("initFilter failed")
}

/// The model has a table for a scale (the four it needs: luma and chroma, each way) exactly when FFmpeg does not cascade it.
fn model_has_tables(flag: kerf_gpu::sws::Scaler, (sw, sh): (u32, u32), (dw, dh): (u32, u32)) -> bool {
    use kerf_gpu::sws::{
        table_aligned_uncached as table, ONE_HORIZONTAL as H, ONE_VERTICAL as V, SIMD_ALIGN_HORIZONTAL as AH,
        SIMD_ALIGN_VERTICAL as AV,
    };
    let c = |x: u32| x.div_ceil(2);
    table(flag, sw, dw, H, AH).is_some()
        && table(flag, c(sw), c(dw), H, AH).is_some()
        && table(flag, sh, dh, V, AV).is_some()
        && table(flag, c(sh), c(dh), V, AV).is_some()
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_scalers_the_models_last_table_is_where_ffmpeg_starts_to_cascade() {
    use kerf_gpu::sws::{table_aligned_uncached as table, ONE_HORIZONTAL as H, SIMD_ALIGN_HORIZONTAL as AH};
    let mut mismatches = Vec::new();
    let mut compared = 0;
    let mut lowest = BTreeMap::new();
    for name in ["lanczos", "spline", "bicubic", "bilinear"] {
        let flag = flag_of(name);
        for dst in [3u32, 7, 12, 16, 24, 30, 31, 45, 64, 97, 135, 270] {
            // The first source size without a horizontal table (a binary search; the model's tables stop at a ratio).
            let none = |s: u32| table(flag, s, dst, H, AH).is_none();
            let mut hi = dst * 2;
            while hi < dst * 300 && !none(hi) {
                hi *= 2;
            }
            if !none(hi) {
                continue;
            }
            let mut lo = dst;
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if none(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let ratio = f64::from(hi) / f64::from(dst);
            if dst >= 4 {
                let e = lowest.entry(name).or_insert((f64::MAX, 0, 0));
                if ratio < e.0 {
                    *e = (ratio, hi, dst);
                }
            }
            // Both sides of the edge and a little way each way, across and down.
            for s in hi.saturating_sub(2)..=hi + 1 {
                for ((sw, sh), (dw, dh)) in [((s, 16), (dst, 16)), ((16, s), (16, dst))] {
                    compared += 1;
                    let (model, ffmpeg) = (
                        !model_has_tables(flag, (sw, sh), (dw, dh)),
                        ffmpeg_cascades(name, (sw, sh), (dw, dh)),
                    );
                    if model != ffmpeg {
                        mismatches.push(format!(
                            "{name} {sw}x{sh}->{dw}x{dh}: model cascades {model}, FFmpeg {ffmpeg}"
                        ));
                    }
                }
            }
        }
    }
    eprintln!(
        "cascade edge: {compared} scales compared, {} differ; lowest ratio without a table: {lowest:?}",
        mismatches.len()
    );
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    // The plan's limit is under the first ratio at which any size has no table, and every size at the limit has one (luma, chroma, both ways).
    for name in ["lanczos", "spline", "bicubic"] {
        let (ratio, _, _) = lowest[name];
        let l = limit(if name == "bicubic" { "x" } else { name });
        assert!(
            (l as f64) < ratio,
            "{name}: the limit {l} is not under the first size without a table ({ratio})"
        );
        let flag = flag_of(name);
        for dst in 1u32..=200 {
            for src in [l as u32 * dst, l as u32 * dst - 1, l as u32 * dst * 9 / 10 + 1] {
                if src > dst + 1 {
                    assert!(
                        model_has_tables(flag, (src, src), (dst, dst)),
                        "{name} {src}->{dst} at the limit has no table"
                    );
                    assert!(
                        !ffmpeg_cascades(name, (src, 16), (dst, 16)) || dst > 200,
                        "{name} {src}->{dst}: FFmpeg cascades inside the limit"
                    );
                }
            }
        }
    }
}

/// A short comparison of the model with FFmpeg's `scale` for each name: sizes that exercise the whole table (window, trim, borders, the
/// shrink, the enlargement, the one-tap row), a minute's run. What a changed constant in the tables is caught by first.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_scalers_model_quick() {
    this_model_is_exact();
    let mut failures = Vec::new();
    let mut n = 0;
    for name in NAMES {
        let flag = flag_of(name);
        let l = limit(name);
        for (srcs, dst) in [
            ((l * 12, 36), (12, 36)),
            ((l * 12 - 1, 35), (12, 35)),
            ((36, l * 9), (36, 9)),
            ((200, 120), (97, 61)),
            ((97, 61), (200, 120)),
            ((640, 360), (333, 187)),
            ((321, 181), (320, 180)),
            ((5, 5), (123, 77)),
            ((64, 64), (2, 2)),
            ((63, 63), (1, 1)),
        ] {
            if srcs.0 > l * dst.0 || srcs.1 > l * dst.1 {
                continue;
            }
            for fill in [Fill::Noise, Fill::Checker] {
                n += 1;
                if let Err(e) = model_vs_ffmpeg(fill, srcs, dst, flag, "1") {
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
    }
    eprintln!("scaler model (quick): {n} scales, {} differ", failures.len());
    for f in failures.iter().take(40) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} of {n} scales differ from FFmpeg's", failures.len());
}

/// Outputs of one and of two samples at each name's limit: a canvas of the limit's side, a picture the size of the canvas, zoomed down to 1 or 2
/// samples (the zoom's `scale` is the only one, `limit`:1 on both axes, luma and the 1-sample chroma). The canvas is even in the export (its encoder
/// asks for it); the Still plan takes the odd ones.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_one_and_two_sample_outputs_at_the_limit() {
    let mut fam = Family::default();
    for name in NAMES {
        let l = limit(name) as u32;
        // (the side of the canvas for one sample and for two; the zoom that leaves 1.0x and 2.0x with margin to truncate to it)
        let side1 = if l.is_multiple_of(2) { l } else { l - 1 };
        let side2 = 2 * side1;
        for (what, side, samples) in [("1 sample", side1, 1.0), ("2 samples", side2, 2.0)] {
            let scale = (samples + 0.4) / f64::from(side);
            let top = src(side, side, Pat::Noise);
            let bg = src(side, side, Pat::Ramp);
            let tl = timeline(
                vec![
                    vec![clip(&bg, 0.0, 0.4, 0.0)],
                    vec![pip(clip(&top, 0.0, 0.4, 0.0), scale, 0.0, 0.0)],
                ],
                Some(Delivery::new(side, side, Fit::Contain)),
            );
            let case = format!("checker/scalers/one-sample/{name}/{what} from a {side}x{side} canvas (scale {scale:.5})");
            run(&mut fam, &case, &tl, &[bg.clone(), top.clone()], name, Expect::Drawn);
            // Past the limit by one sample of canvas: refused.
            let past = side + 2;
            let top = src(past, past, Pat::Noise);
            let bg = src(past, past, Pat::Ramp);
            let tl = timeline(
                vec![
                    vec![clip(&bg, 0.0, 0.4, 0.0)],
                    vec![pip(
                        clip(&top, 0.0, 0.4, 0.0),
                        scale * f64::from(side) / f64::from(past),
                        0.0,
                        0.0,
                    )],
                ],
                Some(Delivery::new(past, past, Fit::Contain)),
            );
            let case = format!("checker/scalers/one-sample/{name}/{what} from a {past}x{past} canvas (two samples past)");
            // (the stage is `past`:`samples`-ish: past the limit by far less than a ratio; whatever the plan decides, the drawn frames are exact)
            run(&mut fam, &case, &tl, &[bg, top], name, Expect::Any);
        }
        // The Still plan, odd canvases: 1 sample at the limit and under it.
        let opts = still_opts(name);
        for side in [l - 1, l - 3, l] {
            if side < 3 {
                continue;
            }
            let top = src(side, side, Pat::Noise);
            let bg = src(side, side, Pat::Ramp);
            let tl = timeline(
                vec![
                    vec![clip(&bg, 0.0, 0.4, 0.0)],
                    vec![pip(clip(&top, 0.0, 0.4, 0.0), 1.4 / f64::from(side), 0.0, 0.0)],
                ],
                Some(Delivery::new(side, side, Fit::Contain)),
            );
            let label = format!("checker/scalers/one-sample/{name}/still, {side}x{side} canvas to one sample");
            let out = still_sweep(&label, &tl, &[bg, top], &opts, &[0.0, 0.2]);
            fam.add_still(&label, &out);
        }
    }
    finish(fam, "scalers: one and two sample outputs at the limit");
}

/// Past the ratio where swscale cascades a second scaler the model has no table, and **a single table would not be FFmpeg's picture** either: with
/// the 256-tap cap lifted by a scratch patch (`KERF_MUT=cap_off`) the model builds the wide table anyway, and this reports how many samples of
/// FFmpeg's cascaded scale it misses. (Unpatched, the model has no table there and the case says so.)
#[test]
#[ignore = "needs ffmpeg"]
fn checker_scalers_past_the_cascade_a_single_table_is_not_ffmpegs_picture() {
    this_model_is_exact();
    let lifted = std::env::var("KERF_MUT").is_ok_and(|m| m == "cap_off");
    let (mut differ, mut same, mut none) = (0, 0, 0);
    for (name, srcs, dst) in [
        ("lanczos", vec![1400usize, 1500, 2000], 30usize),
        ("spline", vec![1500, 1800, 2500], 60),
        ("bicubic", vec![2000, 2500, 4000], 30),
        ("bilinear", vec![4000, 6000], 30),
    ] {
        let flag = flag_of(name);
        for src in srcs {
            for fill in [Fill::Noise, Fill::Checker] {
                match model_vs_ffmpeg(fill, (src, 12), (dst, 12), flag, "1") {
                    Ok(()) => {
                        same += 1;
                        eprintln!("cascade: {name} {src}->{dst} {fill:?}: the single table EQUALS FFmpeg's picture");
                    }
                    Err(e) if e.contains("no table") => {
                        none += 1;
                        eprintln!("cascade: {name} {src}->{dst} {fill:?}: the model has no table");
                    }
                    Err(e) => {
                        differ += 1;
                        eprintln!("cascade: {name} {src}->{dst} {fill:?}: DIFFERS from FFmpeg's picture: {e}");
                    }
                }
            }
        }
    }
    eprintln!("cascade: {differ} differ, {same} equal, {none} without a table (cap lifted: {lifted})");
    if lifted {
        assert!(
            differ > 0,
            "a single wide table is FFmpeg's cascaded picture: the refusal past the cascade would be wider than it has to be"
        );
    } else {
        assert_eq!((differ, same), (0, 0), "the model draws a ratio swscale cascades");
    }
}

/// The probe's refusal of lanczos on an FFmpeg whose `sin` is not ours is **necessary** when that `sin` moves the tables: run this test under a
/// wrapper that preloads a `sin` mutant into FFmpeg (`LD_PRELOAD`, `KERF_FFMPEG` the wrapper, `KERF_SC_SIN_MUTANT` its name; see the report) and
/// it draws lanczos exports anyway, with a compositor pinned to "every name proven", and says how many drawn frames are not FFmpeg's. Without
/// the variable it does nothing.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter; KERF_SC_SIN_MUTANT names the libm mutant FFmpeg runs under"]
fn checker_scalers_lanczos_drawn_anyway_under_another_sin() {
    use kerf_gpu::scaler::{Kernels, Model};
    let Ok(mutant) = std::env::var("KERF_SC_SIN_MUTANT") else {
        eprintln!("KERF_SC_SIN_MUTANT is not set: nothing to do");
        return;
    };
    let real = match kerf_gpu::scaler::kernels() {
        Kernels::Match(m) => m,
        other => panic!("{other:?}"),
    };
    eprintln!("sin mutant {mutant}: the probe proved {:?}", real.named);
    let comp = pinned(Kernels::Match(Model {
        named: kerf_core::ScalerKinds::ALL,
        ..real
    }));
    let (mut drawn, mut inexact) = (0, 0);
    for (sw, sh, w, h, pat) in [
        (480u32, 270u32, 240u32, 136u32, Pat::Noise),
        (640, 360, 120, 68, Pat::Noise),
        (1280, 720, 64, 36, Pat::Ramp),
        (320, 180, 640, 360, Pat::Noise),
        (1920, 1080, 960, 540, Pat::Noise),
        (2560, 1440, 1280, 720, Pat::Noise),
        (2560, 1440, 1920, 1080, Pat::Noise),
    ] {
        let a = src(sw, sh, pat);
        let tl = into(&a, w, h, Fit::Contain);
        let case = format!("checker/scalers/sin-mutant/{mutant}/{sw}x{sh} to {w}x{h} {pat:?}");
        let opts = named("lanczos");
        let played = export(&case, &tl, std::slice::from_ref(&a), &opts);
        let out = sweep_played_on(&comp, &case, &tl, std::slice::from_ref(&a), &opts, FRAMES, played);
        drawn += out.drawn;
        inexact += out.bad.len();
    }
    eprintln!("sin mutant {mutant}: {drawn} lanczos frames drawn on tables from the real sin, {inexact} of them not FFmpeg's");
}

/// One compositor, the same cut and the same instants, the delivery's scaler changing from one plan to the next (bicubic, lanczos, neighbor,
/// default, spline, bilinear, lanczos again): nothing a name drew (a table, a scaled plane, a pipeline) is reused for another.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_alternating_names_on_one_compositor_share_nothing() {
    let mut fam = Family::default();
    let wide = src(640, 360, Pat::Noise);
    let base = src(320, 180, Pat::Edges);
    let c = pip(clip(&wide, 0.0, 0.9, 0.0), 0.45, 0.1, -0.1);
    let mut keyed = clip(&wide, 0.0, 0.9, 0.0);
    keyed.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.9, 0.35, 0.1, 0.1)];
    let cuts = [
        (
            "pip",
            timeline(
                vec![vec![clip(&base, 0.0, 0.9, 0.0)], vec![c]],
                Some(Delivery::new(320, 180, Fit::Contain)),
            ),
        ),
        (
            "keyed",
            timeline(
                vec![vec![clip(&base, 0.0, 0.9, 0.0)], vec![keyed]],
                Some(Delivery::new(320, 180, Fit::Contain)),
            ),
        ),
        ("cover", into(&wide, 180, 320, Fit::Cover)),
    ];
    let order = [
        Some("bicubic"),
        Some("lanczos"),
        Some("neighbor"),
        None,
        Some("spline"),
        Some("bilinear"),
        Some("lanczos"),
        Some("bicubic"),
    ];
    for (what, tl) in &cuts {
        let assets = [base.clone(), wide.clone()];
        for t in [0.1, 0.4, 0.75] {
            for scaler in order {
                let opts = ExportOptions {
                    scaler: scaler.map(str::to_string),
                    ..ExportOptions::default()
                };
                let label = format!("checker/scalers/alternate/{what} t={t} {}", scaler.unwrap_or("default"));
                let out = still_sweep(&label, tl, &assets, &opts, &[t]);
                fam.add_still(&label, &out);
            }
        }
    }
    finish(fam, "scalers: alternating names on one compositor");
}

/// Sizes at which swscale calls the luma axis unscaled (`|xInc - 1| < 10/65536`, an identity filter though the sizes differ by one) and not the
/// chroma axis, and the other way round: the same name's kernel on one plane and the identity on the other.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_scalers_chroma_only_edges() {
    this_model_is_exact();
    let inc = |s: usize, d: usize| ((s << 16) + (d >> 1)) / d;
    let unscaled = |s: usize, d: usize| inc(s, d).abs_diff(1 << 16) < 10;
    let mut failures = Vec::new();
    let mut n = 0;
    let (mut luma_only, mut chroma_only) = (0, 0);
    for name in NAMES {
        let flag = flag_of(name);
        for (s, d) in [
            (6901usize, 6900usize),
            (6900, 6901),
            (6898, 6899),
            (3000, 2999),
            (3001, 3002),
            (3003, 3004),
            (6000, 5999),
        ] {
            let (luma, chroma) = (unscaled(s, d), unscaled(s.div_ceil(2), d.div_ceil(2)));
            assert!(luma != chroma, "{s}->{d} is not a case where only one plane is unscaled");
            if luma {
                luma_only += 1;
            } else {
                chroma_only += 1;
            }
            for fill in [Fill::Noise, Fill::Checker, Fill::Impulse] {
                n += 1;
                if let Err(e) = model_vs_ffmpeg(fill, (s, 4), (d, 4), flag, "1") {
                    failures.push(format!("{name}: {e}"));
                }
                if let Err(e) = model_vs_ffmpeg(fill, (4, s), (4, d), flag, "1") {
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
    }
    eprintln!(
        "chroma-only edges: {n} x2 scales ({luma_only} luma-unscaled, {chroma_only} chroma-unscaled pairs), {} differ",
        failures.len()
    );
    for f in failures.iter().take(30) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} scales differ from FFmpeg's", failures.len());
}
