//! Lane B's filters (the exact scaler, the blurred fill, the odd pad, the dip to white, the mixed-matrix cut) against what lanes A and E
//! added to the same chains, checked as interactions neither lane tested alone: lane A's placement (a moving or held zoom and position, a
//! letterbox) over lane B's resizes, formats and pads; lane E's quarter turns over lane B's resizes, pads and blur fills; lane E's blends over
//! layers lane B resizes or pads; mixed-matrix cuts with placed and turned clips; and the `geq` gate of a mask or a keyed opacity, measured
//! and not.
//!
//! Every case builds a short cut, renders it through `kerf_core::render_with` (FFmpeg's real export graph) and holds every frame the plan
//! **draws** to the export's planes with no tolerance: a differing sample in any plane fails the case. A frame the plan refuses is
//! FFmpeg's and is only counted (with its reasons); the cases that must stay refused say so. Per-frame outcomes are written to
//! `target/parity/checker-b-m/<build>-<mode>/<case>.frames`, so a run with a refusal patched out (`KERF_CHECKER_ALLOW`, a scratch
//! patch of `Compositor::composite_texts` that drops the reasons naming a needle) can be diffed against one without.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_b_m -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

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

// ---- the machinery ---------------------------------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    let gpu = GPU
        .get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone();
    // (Every compositor of this file is built on this device: the measurements are made before any of them draws.)
    prime();
    gpu
}

/// Measure, once, everything a cut's caps are made of: the grade's conversions, the scaler, the matrix conversion, the `geq` edge, the
/// planar conversion under a placement, the placement itself, the blend chain and the text layout. A render only reads them.
fn prime() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = kerf_gpu::grade::kernels();
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::convert::kernels();
        let _ = kerf_core::geq_edge();
        let _ = kerf_gpu::conversion::conversion();
        let _ = kerf_gpu::placement_probe::placement();
        let _ = kerf_gpu::blend::kernels();
        let _ = kerf_gpu::text::model();
    });
}

thread_local! {
    /// Set by the test that pins a compositor to an unmeasured `geq` on purpose (every sweep of that thread then expects the refusal).
    static ALLOW_GEQ_REFUSAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A frame refused for a mask or a keyed opacity is refused because the caps lack them, which is the `geq` edge not being measured
/// ([`prime`]): the plan refuses nothing else under those names. That is the harness's fault, not the plan's, and it must not pass as a
/// refusal.
fn no_geq_refusal(case: &str, why: &[kerf_core::Unsupported]) {
    if ALLOW_GEQ_REFUSAL.with(std::cell::Cell::get) {
        return;
    }
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
        Compositor::new(gpu()).expect("a compositor")
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

fn negotiates() -> bool {
    policy() == CompositeColorPolicy::BottomLayerTag
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

/// A `FFmpeg | GPU | diff x8` strip of one frame in `target/parity/checker-b-m/montage/`.
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
    let dir = target_dir().join("parity").join("checker-b-m").join("montage");
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

// ---- media -------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tag {
    None,
    Bt709,
    Smpte170m,
    Bt470bg,
    Bt2020,
}

impl Tag {
    fn filter(self) -> &'static str {
        match self {
            Tag::None => "",
            Tag::Bt709 => ",setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709",
            Tag::Smpte170m => ",setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m",
            Tag::Bt470bg => ",setparams=colorspace=bt470bg:color_primaries=bt470bg:color_trc=bt470bg",
            Tag::Bt2020 => ",setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Pat {
    /// A moving test pattern with a frame counter.
    Test,
    /// Per-sample noise in all three planes.
    Noise,
    /// Colour bars.
    Bars,
    /// 8-pixel blocks of limited-range black and white.
    Edges,
    /// Luma 16 to 235 left to right, chroma ramping against it: every entry of every table, smoothly.
    Ramp,
}

/// A lossless clip of `w` x `h` (FFV1 when a side is odd, which H.264 refuses), four seconds at 30 fps, cached by
/// what it is made of and the FFmpeg that made it.
fn media(w: u32, h: u32, pat: Pat, tag: Tag) -> Asset {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Asset>>> = OnceLock::new();
    let key = format!("{w}x{h}-{pat:?}-{tag:?}");
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    // (One at a time: two tests asking for the same file must not write it at once.)
    let mut cache = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(a) = cache.get(&key) {
        return a.clone();
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-m-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let odd = w % 2 == 1 || h % 2 == 1;
    let ext = if odd { "mkv" } else { "mp4" };
    let path = dir.join(format!("{}.{ext}", slug(&key)));
    if !path.exists() {
        let src = match pat {
            Pat::Test => format!("testsrc2=size={w}x{h}:rate=30:duration=4"),
            Pat::Noise => format!(
                "nullsrc=size={w}x{h}:rate=30:duration=4,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
            ),
            Pat::Bars => format!("smptehdbars=size={w}x{h}:rate=30:duration=4"),
            Pat::Ramp => format!(
                "nullsrc=size={w}x{h}:rate=30:duration=4,format=yuv420p,\
                 geq=lum='clip(16+219*X/W,16,235)':cb='clip(128+110*(X/W-0.5)+20*sin(Y/9),16,240)':cr='clip(128-110*(X/W-0.5)+20*cos(Y/7),16,240)'"
            ),
            Pat::Edges => format!(
                "nullsrc=size={w}x{h}:rate=30:duration=4,format=yuv420p,\
                 geq=lum='if(mod(floor(X/8)+floor(Y/8),2),235,16)':cb='if(mod(floor(X/16),2),200,60)':cr='if(mod(floor(Y/16),2),190,70)'"
            ),
        };
        let lavfi = format!("{src}{}", tag.filter());
        let tmp = dir.join(format!("{}.tmp.{ext}", slug(&key)));
        // (FFV1 will not open a picture of a few samples: raw frames are as lossless.)
        let codec: &[&str] = if odd && (w < 16 || h < 16) {
            &["-c:v", "rawvideo"]
        } else if odd {
            &["-c:v", "ffv1"]
        } else {
            &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"]
        };
        let mut args = vec!["-f", "lavfi", "-i", &lavfi];
        args.extend_from_slice(codec);
        args.extend_from_slice(&["-pix_fmt", "yuv420p", tmp.to_str().unwrap()]);
        ffmpeg(&args);
        std::fs::rename(&tmp, &path).unwrap();
    }
    let a = probed(&path);
    cache.insert(key, a.clone());
    a
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
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-m-ref-{}", build_key()));
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
    let _ = std::fs::write(dir.join(format!("checker-b-m-report-{}.txt", std::process::id())), text);
}

fn frame_log_dir() -> PathBuf {
    let mode = if std::env::var_os("KERF_CHECKER_ALLOW").is_some() {
        "allow"
    } else {
        "normal"
    };
    let dir = target_dir()
        .join("parity")
        .join("checker-b-m")
        .join(format!("{}-{mode}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

/// Failures are collected over a family and reported together, so one bad case does not hide the others.
#[derive(Default)]
struct Family {
    failures: Vec<String>,
    drawn: usize,
    refused: usize,
    cases: usize,
}

impl Family {
    /// A case whose inexactness is a documented ladder of the base (a translucent layer's RGB round trip, a dip to white's, a
    /// rotated edge), not this lane's: it is reported with its figures and does not fail the family.
    fn add_known_ladder(&mut self, case: &str, out: &Outcome) {
        self.cases += 1;
        self.drawn += out.drawn;
        self.refused += out.refused;
        if !out.bad.is_empty() {
            let worst = |i: usize| out.bad.iter().map(|b| b.planes[i].max).max().unwrap_or(0);
            eprintln!(
                "   known ladder (pre-existing, not failed): {case}: {}/{} drawn frames differ, max Y {} U {} V {}",
                out.bad.len(),
                out.drawn,
                worst(0),
                worst(1),
                worst(2)
            );
        }
    }

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

// ---- more media and cut builders -------------------------------------------------------------------------------

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

fn dedup(assets: &[&Asset]) -> Vec<Asset> {
    let mut out: Vec<Asset> = Vec::new();
    for a in assets {
        if !out.iter().any(|b| b.id == a.id) {
            out.push((*a).clone());
        }
    }
    out
}

/// Noise in `pix` at `w` x `h` under a scratch name of the caller's own (`family`): FFV1 in Matroska, written to a temporary name and
/// renamed under a lock.
fn scratch_media(family: &str, pix: &str, w: u32, h: u32) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-m-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{family}-{pix}-{w}x{h}.mkv"));
    if !path.exists() {
        let base = if pix == "gray" || pix.starts_with("yuv420p1") || pix == "p010le" || pix == "nv12" {
            "yuv420p"
        } else {
            pix
        };
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=0.5,format={base},geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
        );
        let tmp = dir.join(format!("{family}-{pix}-{w}x{h}.tmp.mkv"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            pix,
            tmp.to_str().unwrap(),
        ]);
        std::fs::rename(&tmp, &path).unwrap();
    }
    probed(&path)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    /// Every frame of the case carries the reason.
    Refused,
    /// No frame of the case carries it.
    NotRefused,
}

/// Holds a case to whether the plan gives `needle` among its refusals (a run that patched the refusal out is judging the
/// refusal's necessity: the sweep's own line carries that).
fn expect_refusal(fam: &mut Family, case: &str, out: &Outcome, needle: &str, want: Want) {
    if patched_out() {
        return;
    }
    let n = out.refused_for(needle);
    let wrong = match want {
        Want::Refused => n < out.frames,
        Want::NotRefused => n != 0,
    };
    if wrong {
        fam.failures.push(format!(
            "{case}: {n} of {} frames refused for \"{needle}\", expected {want:?} ({:?})",
            out.frames, out.reasons
        ));
    }
}

/// `run`, or `None` for a file the frame source will not seek into (a codec this FFmpeg does not flag keyframes for).
fn guarded<T>(case: &str, run: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(out) => Some(out),
        Err(e) => {
            let text = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(ToString::to_string))
                .unwrap_or_default();
            if text.contains("does not decode from a keyframe") {
                eprintln!("{case}: the frame source will not seek into this file; skipped");
                None
            } else {
                std::panic::resume_unwind(e)
            }
        }
    }
}

// ---- the cases ---------------------------------------------------------------------------------------------------

/// [`sweep_try`] for a file the frame source may not seek into: skipped.
fn sweep_guarded(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) -> Option<Outcome> {
    guarded(case, || sweep_try(case, tl, assets, fps, secs)).flatten()
}

/// The edit a case makes to its clip.
type Edit = Box<dyn Fn(&mut Clip)>;
/// A case's label, the size of its source, the delivery frame it is fitted into and the edit of its clip.
type Variant = (&'static str, (u32, u32), Option<Delivery>, Edit);
/// A label and the edit of a clip.
type Labelled = (&'static str, Edit);
/// A [`Variant`] and whether a scale resizes its picture.
type Resizing = (&'static str, (u32, u32), Option<Delivery>, Edit, bool);
/// A [`Variant`] whose edit takes the outgoing and the incoming clip of a transition.
type Arrangement = (&'static str, (u32, u32), Option<Delivery>, Box<dyn Fn(&mut Clip, &mut Clip)>);

/// A label, the delivery frame, the clips of each track and the assets they use.
type TitleCase = (&'static str, Option<Delivery>, Vec<Vec<Clip>>, Vec<Asset>);

/// What the plan says of a layer below full opacity under a quarter turn.
const TRANSLUCENT_TURN: &str = "a quarter turn of a layer below full opacity";

/// What a family drew and refused, by the label of the group a case belongs to (printed at the end: a group that is refused whole is
/// said so, not hidden in a pass).
#[derive(Default)]
struct Groups(BTreeMap<String, (usize, usize, usize)>);

impl Groups {
    fn add(&mut self, label: &str, out: &Outcome) {
        let e = self.0.entry(label.to_string()).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 += out.drawn;
        e.2 += out.refused;
    }

    fn print(&self, family: &str) {
        eprintln!("-- {family}: drawn / refused frames by group");
        for (label, (cases, drawn, refused)) in &self.0 {
            eprintln!("   {label:<64} {cases:>3} cases {drawn:>5} drawn {refused:>5} refused");
        }
    }

    fn drawn(&self, label: &str) -> usize {
        self.0.get(label).map_or(0, |e| e.1)
    }
}

/// Run one case, add it to the family and the group; `None` where FFmpeg cannot render it or the frame source will not seek into it.
fn go(
    fam: &mut Family,
    groups: &mut Groups,
    label: &str,
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    secs: f64,
) -> Option<Outcome> {
    go_at(fam, groups, label, case, tl, assets, 30.0, secs)
}

/// [`go`] at the export rate `fps`.
#[allow(clippy::too_many_arguments)]
fn go_at(
    fam: &mut Family,
    groups: &mut Groups,
    label: &str,
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    fps: f64,
    secs: f64,
) -> Option<Outcome> {
    let out = sweep_guarded(case, tl, assets, fps, secs)?;
    fam.add(case, &out);
    groups.add(label, &out);
    Some(out)
}

#[derive(Clone, Copy, Debug)]
enum Pose {
    ZoomIn,
    ZoomOut,
    Pan,
    ZoomPan,
    HeldZoom,
    HeldKey,
    Breathe,
}

impl Pose {
    const ALL: [Pose; 7] = [
        Pose::ZoomIn,
        Pose::ZoomOut,
        Pose::Pan,
        Pose::ZoomPan,
        Pose::HeldZoom,
        Pose::HeldKey,
        Pose::Breathe,
    ];

    fn moves(self) -> bool {
        !matches!(self, Pose::HeldZoom | Pose::HeldKey)
    }
}

fn posed(mut c: Clip, pose: Pose) -> Clip {
    match pose {
        Pose::ZoomIn => c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.3, 1.6, 0.0, 0.0)],
        Pose::ZoomOut => c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.3, 0.6, 0.0, 0.0)],
        Pose::Pan => c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.3, 1.0, 0.2, -0.15)],
        Pose::ZoomPan => c.keyframes = vec![zoom_key(0.0, 0.8, -0.1, 0.1), zoom_key(0.3, 1.3, 0.1, 0.05)],
        Pose::HeldZoom => {
            c.transform.scale = 0.6;
            c.transform.pos_x = 0.1;
            c.transform.pos_y = -0.1;
        }
        Pose::HeldKey => c.keyframes = vec![zoom_key(0.0, 0.8, 0.05, 0.0), zoom_key(0.3, 0.8, 0.05, 0.0)],
        Pose::Breathe => {
            c.behaviors = vec![kerf_core::Behavior::Breathe {
                amount: 0.2,
                speed: 1.5,
                phase: 0.0,
            }];
        }
    }
    c
}

fn with_edges(w: u32, h: u32) -> Asset {
    media(w, h, Pat::Edges, Tag::None)
}

/// Lane A's placement over lane B's resizes: a moving or held zoom and position of a picture the fit resizes (a shrink, an enlargement,
/// a letterbox, a pillarbox, an odd width, a blur fill that gives way to a plain fit), alone and over a full-frame clip.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_placement_over_resizes_letterboxes_and_odd_fits() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let wide = |w, h| Some(Delivery::new(w, h, Fit::Contain));
    let sources: Vec<(u32, u32, Option<Delivery>, &str)> = vec![
        (320, 180, None, "same size"),
        (160, 120, wide(320, 180), "4:3 enlarged into a letterbox"),
        (640, 360, wide(320, 180), "2:1 shrink"),
        (160, 90, wide(320, 180), "2:1 enlarge"),
        (321, 180, wide(320, 180), "odd width fitted"),
        (180, 320, wide(320, 180), "portrait pillarboxed (odd fitted width)"),
        (63, 36, wide(64, 36), "tiny 63x36 into 64x36"),
        (
            320,
            180,
            Some(Delivery::new(180, 320, Fit::Cover)),
            "landscape covered into portrait",
        ),
        (320, 180, wide(256, 144), "shrink 0.8"),
        (
            180,
            320,
            Some(Delivery::new(320, 180, Fit::Blur)),
            "portrait with a blurred fill (a placed clip gives it up)",
        ),
        (
            320,
            180,
            Some(Delivery::new(180, 320, Fit::Blur)),
            "landscape with a blurred fill into portrait",
        ),
    ];
    for (i, (w, h, delivery, label)) in sources.iter().enumerate() {
        let noise = media(*w, *h, Pat::Noise, Tag::None);
        let base = with_edges(320, 180);
        for pose in Pose::ALL {
            let group = format!("{label} / {}", if pose.moves() { "moving" } else { "held" });
            let c = posed(clip(&noise, 0.0, 0.4, 0.0), pose);
            let tl = timeline(vec![vec![c.clone()]], *delivery);
            go(
                &mut fam,
                &mut groups,
                &group,
                &format!("checker/b/m/place {w}x{h} {label} {pose:?} alone"),
                &tl,
                std::slice::from_ref(&noise),
                0.4,
            );
            if i < 6 {
                let tl = timeline(vec![vec![clip(&base, 0.0, 0.4, 0.0)], vec![c]], *delivery);
                go(
                    &mut fam,
                    &mut groups,
                    &group,
                    &format!("checker/b/m/place {w}x{h} {label} {pose:?} over a base"),
                    &tl,
                    &dedup(&[&noise, &base]),
                    0.4,
                );
            }
        }
    }
    groups.print("placement over resizes");
    assert!(
        groups.drawn("same size / moving") > 0,
        "nothing of a plain moving zoom was drawn"
    );
    assert!(
        groups.drawn("shrink 0.8 / held") > 0,
        "nothing of a held zoom on a shrink was drawn"
    );
    fam.finish("placement over resizes, letterboxes and odd fits");
}

/// Lane A's placement over lane B's pixel formats: a 4:2:2, 4:4:4, gray, 10-bit and full-range picture under a moving or held zoom, at the
/// canvas's size (nothing resized) and resized by the fit.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_placement_over_pixel_formats() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    for pix in [
        "yuv422p",
        "yuv444p",
        "gray",
        "yuv420p10le",
        "yuvj420p",
        "yuv422p10le",
        "yuv440p",
        "yuvj444p",
    ] {
        let asset = scratch_media("bm", pix, 320, 180);
        for (delivery, size) in [
            (None, "same size"),
            (Some(Delivery::new(256, 144, Fit::Contain)), "resized"),
            (Some(Delivery::new(320, 240, Fit::Contain)), "letterboxed"),
        ] {
            for pose in [Pose::ZoomPan, Pose::Pan, Pose::HeldZoom] {
                let tl = timeline(vec![vec![posed(clip(&asset, 0.0, 0.4, 0.0), pose)]], delivery);
                go(
                    &mut fam,
                    &mut groups,
                    &format!("{pix} {size}"),
                    &format!("checker/b/m/place-fmt {pix} {size} {pose:?}"),
                    &tl,
                    std::slice::from_ref(&asset),
                    0.4,
                );
            }
        }
    }
    groups.print("placement over pixel formats");
    fam.finish("placement over pixel formats");
}

/// Lane A's placement over lane B's odd pad: an odd-height or odd-width picture centred in a taller frame (the pad), under a moving or
/// held zoom. An odd-height picture under a moving placement is lane A's refusal; the odd width and the held zoom are drawn.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_placement_over_the_odd_pad() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let frame = Some(Delivery::new(64, 36, Fit::Contain));
    for (w, h) in [
        (64u32, 31u32),
        (64, 35),
        (64, 33),
        (65, 36),
        (63, 35),
        (61, 31),
        (64, 29),
        (33, 36),
    ] {
        let asset = media(w, h, Pat::Noise, Tag::None);
        for pose in Pose::ALL {
            let tl = timeline(vec![vec![posed(clip(&asset, 0.0, 0.4, 0.0), pose)]], frame);
            let label = format!("{w}x{h} / {}", if pose.moves() { "moving" } else { "held" });
            go(
                &mut fam,
                &mut groups,
                &label,
                &format!("checker/b/m/place-odd {w}x{h} {pose:?}"),
                &tl,
                std::slice::from_ref(&asset),
                0.4,
            );
        }
    }
    groups.print("placement over the odd pad");
    fam.finish("placement over the odd pad");
}

/// Lane A's placement and lane B's resizes and pads meeting a transition: the incoming clip (or the outgoing one) is placed, held-zoomed,
/// letterboxed, odd-padded or blur-filled, and the cut between the two is a dip to white (lane B's), a dissolve, a slide or a push.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_placement_resizes_and_pads_through_a_transition() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let kinds = [
        TransitionKind::DipToWhite,
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::PushDown,
    ];
    // (label, source of the clips, delivery, edit of (outgoing, incoming))
    let wide = Some(Delivery::new(160, 90, Fit::Contain));
    let arrangements: Vec<Arrangement> = vec![
        ("plain control", (160, 90), None, Box::new(|_, _| {})),
        (
            "incoming moving zoom",
            (160, 90),
            None,
            Box::new(|_, b| *b = posed(b.clone(), Pose::ZoomIn)),
        ),
        (
            "outgoing moving zoom",
            (160, 90),
            None,
            Box::new(|a, _| *a = posed(a.clone(), Pose::ZoomOut)),
        ),
        (
            "both panning",
            (160, 90),
            None,
            Box::new(|a, b| {
                *a = posed(a.clone(), Pose::Pan);
                *b = posed(b.clone(), Pose::ZoomPan);
            }),
        ),
        (
            "incoming held zoom",
            (160, 90),
            None,
            Box::new(|_, b| *b = posed(b.clone(), Pose::HeldZoom)),
        ),
        (
            "outgoing held zoom",
            (160, 90),
            None,
            Box::new(|a, _| *a = posed(a.clone(), Pose::HeldZoom)),
        ),
        (
            "incoming breathing",
            (160, 90),
            None,
            Box::new(|_, b| *b = posed(b.clone(), Pose::Breathe)),
        ),
        ("both letterboxed and enlarged", (80, 60), wide, Box::new(|_, _| {})),
        (
            "incoming letterboxed and moving",
            (80, 60),
            wide,
            Box::new(|_, b| *b = posed(b.clone(), Pose::ZoomIn)),
        ),
        ("both shrunk 2:1", (320, 180), wide, Box::new(|_, _| {})),
        (
            "incoming shrunk and held-zoomed",
            (320, 180),
            wide,
            Box::new(|_, b| *b = posed(b.clone(), Pose::HeldZoom)),
        ),
        (
            "outgoing shrunk and moving",
            (320, 180),
            wide,
            Box::new(|a, _| *a = posed(a.clone(), Pose::ZoomPan)),
        ),
        ("both odd-padded 80x45", (80, 45), wide, Box::new(|_, _| {})),
        (
            "incoming odd-padded and moving",
            (80, 45),
            wide,
            Box::new(|_, b| *b = posed(b.clone(), Pose::Pan)),
        ),
        (
            "incoming odd-padded and held-zoomed",
            (80, 45),
            wide,
            Box::new(|_, b| *b = posed(b.clone(), Pose::HeldZoom)),
        ),
        (
            "blur-filled portrait",
            (90, 160),
            Some(Delivery::new(160, 90, Fit::Blur)),
            Box::new(|_, _| {}),
        ),
        (
            "blur-filled portrait, incoming moving",
            (90, 160),
            Some(Delivery::new(160, 90, Fit::Blur)),
            Box::new(|_, b| *b = posed(b.clone(), Pose::ZoomIn)),
        ),
        (
            "incoming turned 90 and shrunk",
            (160, 90),
            None,
            Box::new(|_, b| {
                b.transform.rotation = 90.0;
                b.transform.scale = 0.5;
            }),
        ),
        (
            "outgoing turned 180",
            (160, 90),
            None,
            Box::new(|a, _| a.transform.rotation = 180.0),
        ),
    ];
    for (label, (w, h), delivery, edit) in &arrangements {
        let a = media(*w, *h, Pat::Noise, Tag::None);
        let b = media(*w, *h, Pat::Test, Tag::None);
        for kind in kinds {
            if !matches!(kind, TransitionKind::DipToWhite | TransitionKind::Crossfade)
                && !label.contains("control")
                && !label.contains("incoming moving")
                && !label.contains("incoming shrunk")
                && !label.contains("blur-filled portrait,")
            {
                continue;
            }
            let (mut first, mut second) = (clip(&a, 0.0, 0.5, 0.0), clip(&b, 0.0, 0.5, 0.5));
            edit(&mut first, &mut second);
            let second = dissolve_into(second, kind, 0.3);
            let tl = timeline(vec![vec![first, second]], *delivery);
            go(
                &mut fam,
                &mut groups,
                &format!("{label} / {kind:?}"),
                &format!("checker/b/m/transition {label} {kind:?}"),
                &tl,
                &dedup(&[&a, &b]),
                0.9,
            );
        }
    }
    groups.print("placement, resizes and pads through a transition");
    fam.finish("placement, resizes and pads through a transition");
}

/// Lane E's quarter turns over lane B's resizes, pads and blur fills: 90, 180 and 270 degrees of a picture the fit shrinks, enlarges,
/// letterboxes, pillarboxes, odd-sides or blur-fills, held at the fit and zoomed out, alone and over a full-frame clip.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_quarter_turns_over_resizes_pads_and_fills() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let wide = |w, h| Some(Delivery::new(w, h, Fit::Contain));
    let sources: Vec<(u32, u32, Option<Delivery>, &str)> = vec![
        (320, 180, None, "same size"),
        (160, 120, wide(320, 180), "4:3 enlarged into a letterbox"),
        (640, 360, wide(320, 180), "2:1 shrink"),
        (180, 320, wide(320, 180), "portrait pillarboxed"),
        (321, 180, wide(320, 180), "odd width fitted"),
        (65, 37, wide(64, 36), "odd 65x37 fitted"),
        (64, 31, wide(64, 36), "odd-height pad 64x31"),
        (320, 180, Some(Delivery::new(180, 320, Fit::Cover)), "covered into portrait"),
        (
            320,
            180,
            Some(Delivery::new(180, 320, Fit::Blur)),
            "landscape with a blurred fill",
        ),
        (
            180,
            320,
            Some(Delivery::new(320, 180, Fit::Blur)),
            "portrait with a blurred fill",
        ),
    ];
    for (i, (w, h, delivery, label)) in sources.iter().enumerate() {
        let noise = media(*w, *h, Pat::Noise, Tag::None);
        let base = with_edges(320, 180);
        for turn in [90.0, 180.0, 270.0] {
            for scale in [1.0, 0.5625, 0.4] {
                let mut c = clip(&noise, 0.0, 0.3, 0.0);
                c.transform.rotation = turn;
                c.transform.scale = scale;
                let group = format!("{label} / scale {scale}");
                let tl = timeline(vec![vec![c.clone()]], *delivery);
                go(
                    &mut fam,
                    &mut groups,
                    &group,
                    &format!("checker/b/m/turn {w}x{h} {label} {turn} at {scale} alone"),
                    &tl,
                    std::slice::from_ref(&noise),
                    0.3,
                );
                if i < 5 && scale < 1.0 {
                    let tl = timeline(vec![vec![clip(&base, 0.0, 0.3, 0.0)], vec![c]], *delivery);
                    go(
                        &mut fam,
                        &mut groups,
                        &group,
                        &format!("checker/b/m/turn {w}x{h} {label} {turn} at {scale} over a base"),
                        &tl,
                        &dedup(&[&noise, &base]),
                        0.3,
                    );
                }
            }
        }
    }
    groups.print("quarter turns over resizes, pads and fills");
    assert!(
        groups.drawn("same size / scale 0.5625") > 0,
        "no quarter turn was drawn at all"
    );
    fam.finish("quarter turns over resizes, pads and fills");
}

/// A quarter turn beside what lane B draws around it: a turned clip with a fade, a dissolve in and out, a moved turned clip (a position),
/// and a turned clip whose keys also zoom (the plan refuses the keyed turn or zoom).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_quarter_turns_with_fades_positions_and_keys() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let a = media(160, 90, Pat::Noise, Tag::None);
    let b = media(160, 90, Pat::Test, Tag::None);
    let under = with_edges(160, 90);
    for turn in [90.0, 180.0, 270.0, 45.0, 91.0] {
        let edits: Vec<Labelled> = vec![
            (
                "fade in/out",
                Box::new(|c| {
                    c.fade_in = 0.15;
                    c.fade_out = 0.15;
                }),
            ),
            (
                "moved",
                Box::new(|c| {
                    c.transform.pos_x = 0.2;
                    c.transform.pos_y = -0.1;
                    c.transform.scale = 0.5;
                }),
            ),
            ("held zoom 1.3", Box::new(|c| c.transform.scale = 1.3)),
            // (An opacity below 1 under a quarter turn is `checker_b_m_translucent_layers_turned_a_quarter_stay_on_the_translucent_ladder`'s.)
            (
                "cropped",
                Box::new(|c| {
                    c.transform.crop_left = 0.1;
                    c.transform.crop_top = 0.15;
                    c.transform.scale = 0.7;
                }),
            ),
            (
                "keyed zoom (the keys carry the turn)",
                Box::new(move |c| {
                    c.keyframes = vec![
                        kerf_core::Keyframe {
                            rotation: turn,
                            ..zoom_key(0.0, 0.5, 0.0, 0.0)
                        },
                        kerf_core::Keyframe {
                            rotation: turn,
                            ..zoom_key(0.3, 0.8, 0.0, 0.0)
                        },
                    ];
                }),
            ),
            (
                "keyed zoom (the keys do not: the clip is not turned)",
                Box::new(|c| {
                    c.keyframes = vec![zoom_key(0.0, 0.5, 0.0, 0.0), zoom_key(0.3, 0.8, 0.0, 0.0)];
                }),
            ),
        ];
        for (label, edit) in &edits {
            let mut c = clip(&b, 0.0, 0.5, 0.0);
            c.transform.rotation = turn;
            edit(&mut c);
            let tl = timeline(vec![vec![clip(&under, 0.0, 0.5, 0.0)], vec![c.clone()]], None);
            go(
                &mut fam,
                &mut groups,
                &format!("turn {turn} / {label}"),
                &format!("checker/b/m/turn-with {turn} {label} over a base"),
                &tl,
                &dedup(&[&b, &under]),
                0.5,
            );
        }
        // a dissolve from a plain clip into a turned one, and from a turned one into a plain one
        for kind in [TransitionKind::Crossfade, TransitionKind::DipToWhite] {
            let mut turned = clip(&b, 0.0, 0.5, 0.5);
            turned.transform.rotation = turn;
            turned.transform.scale = 0.5625;
            let tl = timeline(
                vec![vec![clip(&a, 0.0, 0.5, 0.0), dissolve_into(turned.clone(), kind, 0.3)]],
                None,
            );
            go(
                &mut fam,
                &mut groups,
                &format!("turn {turn} / into a turned clip"),
                &format!("checker/b/m/turn-with {turn} {kind:?} into a turned clip"),
                &tl,
                &dedup(&[&a, &b]),
                0.9,
            );
            let mut leaving = clip(&a, 0.0, 0.5, 0.0);
            leaving.transform.rotation = turn;
            leaving.transform.scale = 0.5625;
            let tl = timeline(vec![vec![leaving, dissolve_into(clip(&b, 0.0, 0.5, 0.5), kind, 0.3)]], None);
            go(
                &mut fam,
                &mut groups,
                &format!("turn {turn} / out of a turned clip"),
                &format!("checker/b/m/turn-with {turn} {kind:?} out of a turned clip"),
                &tl,
                &dedup(&[&a, &b]),
                0.9,
            );
        }
    }
    groups.print("quarter turns with fades, positions and keys");
    fam.finish("quarter turns with fades, positions and keys");
}

/// Lane E's blends over layers lane B resizes, pads or fills: a full-frame blended clip over a base that the fit shrinks, letterboxes, odd-pads,
/// blur-fills, zooms out, moves or turns; and a blended clip that is itself resized, letterboxed, odd-padded, held-zoomed, moved or turned.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_blends_over_and_under_resized_padded_and_turned_layers() {
    use kerf_core::BlendMode;
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let canvas = Some(Delivery::new(160, 90, Fit::Contain));
    let small = Some(Delivery::new(64, 36, Fit::Contain));
    let modes = [
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::HardLight,
        BlendMode::Add,
        BlendMode::Difference,
    ];
    // (label, base source, delivery, edit of the base clip)
    let bases: Vec<Variant> = vec![
        ("base shrunk 2:1", (320, 180), canvas, Box::new(|_| {})),
        ("base letterboxed and enlarged", (80, 60), canvas, Box::new(|_| {})),
        ("base odd-padded 64x31", (64, 31), small, Box::new(|_| {})),
        ("base odd-padded 64x35", (64, 35), small, Box::new(|_| {})),
        (
            "base blur-filled portrait",
            (90, 160),
            Some(Delivery::new(160, 90, Fit::Blur)),
            Box::new(|_| {}),
        ),
        (
            "base held zoom out",
            (160, 90),
            canvas,
            Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom)),
        ),
        (
            "base moving zoom",
            (160, 90),
            canvas,
            Box::new(|c| *c = posed(c.clone(), Pose::ZoomPan)),
        ),
        (
            "base turned 90 at 0.5625",
            (160, 90),
            canvas,
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
            }),
        ),
        (
            "base turned 180",
            (160, 90),
            canvas,
            Box::new(|c| c.transform.rotation = 180.0),
        ),
    ];
    for (label, (w, h), delivery, edit) in &bases {
        let (cw, ch) = if delivery.is_some_and(|d| d.width == 64) {
            (64, 36)
        } else {
            (160, 90)
        };
        let below = media(*w, *h, Pat::Noise, Tag::None);
        let top = media(cw, ch, Pat::Test, Tag::None);
        for mode in modes {
            let mut base = clip(&below, 0.0, 0.4, 0.0);
            edit(&mut base);
            let mut blended = clip(&top, 0.0, 0.4, 0.0);
            blended.blend_mode = mode;
            let tl = timeline(vec![vec![base], vec![blended]], *delivery);
            go(
                &mut fam,
                &mut groups,
                label,
                &format!("checker/b/m/blend {label} {mode:?}"),
                &tl,
                &dedup(&[&below, &top]),
                0.4,
            );
        }
    }
    // the blended clip is the odd one
    let tops: Vec<Variant> = vec![
        ("top shrunk 2:1", (320, 180), canvas, Box::new(|_| {})),
        ("top letterboxed and enlarged", (80, 60), canvas, Box::new(|_| {})),
        ("top odd-padded 64x31", (64, 31), small, Box::new(|_| {})),
        (
            "top held zoom out",
            (160, 90),
            canvas,
            Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom)),
        ),
        ("top held zoom in", (160, 90), canvas, Box::new(|c| c.transform.scale = 1.3)),
        (
            "top moving zoom",
            (160, 90),
            canvas,
            Box::new(|c| *c = posed(c.clone(), Pose::ZoomIn)),
        ),
        (
            "top turned 180",
            (160, 90),
            canvas,
            Box::new(|c| c.transform.rotation = 180.0),
        ),
        (
            "top turned 90 (overflowing)",
            (160, 90),
            canvas,
            Box::new(|c| c.transform.rotation = 90.0),
        ),
        (
            "top turned 270 at 0.5625",
            (160, 90),
            canvas,
            Box::new(|c| {
                c.transform.rotation = 270.0;
                c.transform.scale = 0.5625;
            }),
        ),
        (
            "top blur-filled portrait",
            (90, 160),
            Some(Delivery::new(160, 90, Fit::Blur)),
            Box::new(|_| {}),
        ),
    ];
    for (label, (w, h), delivery, edit) in &tops {
        let (cw, ch) = if delivery.is_some_and(|d| d.width == 64) {
            (64, 36)
        } else {
            (160, 90)
        };
        let below = media(cw, ch, Pat::Edges, Tag::None);
        let top = media(*w, *h, Pat::Noise, Tag::None);
        for mode in [
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Difference,
        ] {
            let mut blended = clip(&top, 0.0, 0.4, 0.0);
            blended.blend_mode = mode;
            edit(&mut blended);
            let tl = timeline(vec![vec![clip(&below, 0.0, 0.4, 0.0)], vec![blended]], *delivery);
            go(
                &mut fam,
                &mut groups,
                label,
                &format!("checker/b/m/blend {label} {mode:?}"),
                &tl,
                &dedup(&[&below, &top]),
                0.4,
            );
        }
    }
    groups.print("blends over and under resized, padded and turned layers");
    assert!(
        groups.drawn("base shrunk 2:1") > 0,
        "a blend over a shrunk base was never drawn"
    );
    fam.finish("blends over and under resized, padded and turned layers");
}

/// Mixed-matrix cuts with a placed or turned clip: the second clip (or a layer over the first) is tagged with another matrix and moves, is held
/// zoomed, turned a quarter, letterboxed or odd-sided. On 9.0.2 the composite is the bottom layer's tag and the clip is converted; on 6.1.1 it
/// is BT.601 whatever the tags say.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_mixed_matrix_cuts_with_placed_and_turned_clips() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let pairs = [
        (Tag::None, Tag::Bt709),
        (Tag::Bt709, Tag::None),
        (Tag::Bt709, Tag::Bt2020),
        (Tag::Bt2020, Tag::Bt709),
        (Tag::None, Tag::Bt2020),
        (Tag::Smpte170m, Tag::Bt709),
    ];
    let edits: Vec<(&str, (u32, u32), Edit)> = vec![
        ("plain", (160, 90), Box::new(|_| {})),
        ("moving zoom", (160, 90), Box::new(|c| *c = posed(c.clone(), Pose::ZoomIn))),
        ("moving pan", (160, 90), Box::new(|c| *c = posed(c.clone(), Pose::Pan))),
        (
            "held zoom out",
            (160, 90),
            Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom)),
        ),
        (
            "turned 90 at 0.5625",
            (160, 90),
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
            }),
        ),
        ("turned 180", (160, 90), Box::new(|c| c.transform.rotation = 180.0)),
        (
            "turned 270 shrunk",
            (160, 90),
            Box::new(|c| {
                c.transform.rotation = 270.0;
                c.transform.scale = 0.4;
            }),
        ),
        ("shrunk 2:1 by the fit", (320, 180), Box::new(|_| {})),
        ("letterboxed 4:3", (80, 60), Box::new(|_| {})),
        ("odd sides 79x45", (79, 45), Box::new(|_| {})),
    ];
    let frame = Some(Delivery::new(160, 90, Fit::Contain));
    for (a_tag, b_tag) in pairs {
        for (label, (w, h), edit) in &edits {
            let first = media(160, 90, Pat::Noise, a_tag);
            let second = media(*w, *h, Pat::Test, b_tag);
            // in sequence, one track
            let mut b = clip(&second, 0.0, 0.4, 0.4);
            edit(&mut b);
            let tl = timeline(vec![vec![clip(&first, 0.0, 0.4, 0.0), b]], frame);
            go(
                &mut fam,
                &mut groups,
                &format!("{label} / in sequence"),
                &format!("checker/b/m/matrix {a_tag:?} then {b_tag:?} {label}"),
                &tl,
                &dedup(&[&first, &second]),
                0.8,
            );
            // stacked: over the first
            let mut b = clip(&second, 0.0, 0.4, 0.0);
            edit(&mut b);
            let tl = timeline(vec![vec![clip(&first, 0.0, 0.4, 0.0)], vec![b]], frame);
            go(
                &mut fam,
                &mut groups,
                &format!("{label} / stacked"),
                &format!("checker/b/m/matrix {b_tag:?} over {a_tag:?} {label}"),
                &tl,
                &dedup(&[&first, &second]),
                0.4,
            );
        }
    }
    groups.print("mixed-matrix cuts with placed and turned clips");
    fam.finish("mixed-matrix cuts with placed and turned clips");
}

fn copy_of(p: &Played) -> Played {
    Played {
        w: p.w,
        h: p.h,
        yuv: p.yuv.clone(),
    }
}

fn opacity_key(time: f64, opacity: f64) -> kerf_core::Keyframe {
    kerf_core::Keyframe {
        time,
        scale: 1.0,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity,
        easing: Default::default(),
    }
}

fn mask_of(shape: kerf_core::MaskShape, feather: f64, inverted: bool, size: f64) -> kerf_core::Mask {
    kerf_core::Mask {
        shape,
        x: 0.5,
        y: 0.5,
        width: size,
        height: size,
        feather,
        inverted,
    }
}

/// The `geq` of a mask and of a keyed opacity beside lane A's placements, lane E's turns and lane B's resizes, with the edge read as the
/// build does ([`prime`]): every drawn frame is the export's. The same cuts with the compositor **pinned to an unmeasured `geq`** draw no
/// frame that carries a mask or a keyed opacity (the plan refuses them up front), and the frames before the clip are still drawn exactly.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_masks_and_keyed_opacity_with_the_geq_gate_measured_and_not() {
    use kerf_core::{GeqEdge, MaskShape};
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let unknown = Compositor::new(gpu()).expect("a compositor").with_geq_edge(GeqEdge::Unknown);
    let canvas = Some(Delivery::new(160, 90, Fit::Contain));
    let geq: Vec<(&str, Edit)> = vec![
        (
            "rect mask over the whole frame",
            Box::new(|c| c.mask = Some(mask_of(MaskShape::Rect, 0.0, false, 1.0))),
        ),
        (
            "feathered ellipse",
            Box::new(|c| c.mask = Some(mask_of(MaskShape::Ellipse, 0.3, false, 0.7))),
        ),
        (
            "inverted rect",
            Box::new(|c| c.mask = Some(mask_of(MaskShape::Rect, 0.2, true, 0.4))),
        ),
        (
            "keyed opacity ramp",
            Box::new(|c| c.keyframes = vec![opacity_key(0.0, 1.0), opacity_key(0.3, 0.3)]),
        ),
    ];
    let bodies: Vec<Variant> = vec![
        ("plain", (160, 90), canvas, Box::new(|_| {})),
        ("shrunk 2:1", (320, 180), canvas, Box::new(|_| {})),
        ("letterboxed and enlarged", (80, 60), canvas, Box::new(|_| {})),
        ("odd sides fitted", (79, 45), canvas, Box::new(|_| {})),
        (
            "odd-height pad",
            (64, 31),
            Some(Delivery::new(64, 36, Fit::Contain)),
            Box::new(|_| {}),
        ),
        (
            "held zoom out",
            (160, 90),
            canvas,
            Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom)),
        ),
        ("moving zoom", (160, 90), canvas, Box::new(|c| c.transform.scale = 1.0)),
        ("turned 180", (160, 90), canvas, Box::new(|c| c.transform.rotation = 180.0)),
        (
            "turned 90 shrunk",
            (160, 90),
            canvas,
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
            }),
        ),
        (
            "blur-filled portrait",
            (90, 160),
            Some(Delivery::new(160, 90, Fit::Blur)),
            Box::new(|_| {}),
        ),
    ];
    let under = media(160, 90, Pat::Edges, Tag::None);
    for (g_label, g) in &geq {
        for (b_label, (w, h), delivery, body) in &bodies {
            let noise = media(*w, *h, Pat::Noise, Tag::None);
            let (cw, ch) = if delivery.is_some_and(|d| d.width == 64) {
                (64, 36)
            } else {
                (160, 90)
            };
            let below = if (cw, ch) == (160, 90) {
                under.clone()
            } else {
                media(cw, ch, Pat::Edges, Tag::None)
            };
            let mut c = clip(&noise, 0.0, 0.4, 0.0);
            body(&mut c);
            g(&mut c);
            if *b_label == "moving zoom" {
                // (a keyed opacity already owns the keys: the zoom moves through a second pair, the mask's clip through its own)
                if c.keyframes.is_empty() {
                    c = posed(c, Pose::ZoomIn);
                } else {
                    c.keyframes = vec![
                        kerf_core::Keyframe {
                            scale: 1.0,
                            ..opacity_key(0.0, 1.0)
                        },
                        kerf_core::Keyframe {
                            scale: 1.5,
                            ..opacity_key(0.3, 0.3)
                        },
                    ];
                }
            }
            let tl = timeline(vec![vec![clip(&below, 0.0, 0.4, 0.0)], vec![c]], *delivery);
            let case = format!("checker/b/m/geq {g_label} / {b_label}");
            let assets = dedup(&[&noise, &below]);
            let Some(played) = guarded(&case, || try_export(&case, &tl, &assets, &at_fps(30.0)).ok()).flatten() else {
                continue;
            };
            let frames = 12;
            ALLOW_GEQ_REFUSAL.with(|f| f.set(false));
            let measured = guarded(&case, || {
                sweep_played(&case, &tl, &assets, &at_fps(30.0), frames, copy_of(&played))
            });
            if let Some(out) = measured {
                fam.add(&case, &out);
                groups.add(&format!("{g_label} / measured"), &out);
                if out.refused_for(": mask") + out.refused_for("keyframed opacity") != 0 {
                    fam.failures.push(format!(
                        "{case}: refused for the geq with the edge measured: {:?}",
                        out.reasons
                    ));
                }
            }
            // The same cut against a compositor whose geq edge is not known: nothing drawn, and not by a render that probes.
            ALLOW_GEQ_REFUSAL.with(|f| f.set(true));
            let pinned = guarded(&case, || {
                sweep_played_on(
                    &unknown,
                    &format!("{case} (geq unknown)"),
                    &tl,
                    &assets,
                    &at_fps(30.0),
                    frames,
                    copy_of(&played),
                )
            });
            ALLOW_GEQ_REFUSAL.with(|f| f.set(false));
            if let Some(out) = pinned {
                groups.add(&format!("{g_label} / geq unknown"), &out);
                if out.drawn != 0 || !out.bad.is_empty() {
                    fam.failures.push(format!(
                        "{case}: {} frames drawn with the geq edge unknown (it must refuse every frame of a masked or keyed-opacity clip)",
                        out.drawn
                    ));
                }
                let geq_reasons = out.refused_for(": mask") + out.refused_for("keyframed opacity");
                if geq_reasons == 0 && !patched_out() {
                    fam.failures.push(format!(
                        "{case}: refused with the geq unknown, but not for the geq: {:?}",
                        out.reasons
                    ));
                }
            }
        }
    }
    // The frames before the clip are the plain layer's, and drawn with the geq unknown; the clip's own frames are refused.
    {
        let a = media(160, 90, Pat::Noise, Tag::None);
        let b = media(160, 90, Pat::Test, Tag::None);
        let mut masked = clip(&b, 0.0, 0.4, 0.4);
        masked.mask = Some(mask_of(MaskShape::Ellipse, 0.2, false, 0.8));
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.4, 0.0), masked]], canvas);
        let case = "checker/b/m/geq a masked clip after a plain one".to_string();
        let assets = dedup(&[&a, &b]);
        if let Some(played) = guarded(&case, || try_export(&case, &tl, &assets, &at_fps(30.0)).ok()).flatten() {
            ALLOW_GEQ_REFUSAL.with(|f| f.set(true));
            let out = sweep_played_on(&unknown, &case, &tl, &assets, &at_fps(30.0), 24, played);
            ALLOW_GEQ_REFUSAL.with(|f| f.set(false));
            fam.add(&case, &out);
            groups.add("masked clip after a plain one / geq unknown", &out);
            if out.drawn != 12 || out.refused != 12 {
                fam.failures.push(format!(
                    "{case}: {} drawn, {} refused (12 and 12 expected: the plain clip's frames drawn, the masked clip's refused)",
                    out.drawn, out.refused
                ));
            }
        }
    }
    groups.print("masks and keyed opacity with the geq gate");
    fam.finish("masks and keyed opacity with the geq gate, measured and not");
}

/// The pin follows the build: a compositor told the **other** edge than this FFmpeg's draws the last row and column of a masked noise
/// picture as the other build would, which is not this export's. (A control: it proves the gate reads the value, not only whether it is known.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_the_geq_edge_pin_is_read_not_only_checked_for_unknown() {
    use kerf_core::{GeqEdge, MaskShape};
    let measured = kerf_core::geq_edge_measured();
    let wrong = match measured {
        GeqEdge::Exact => GeqEdge::Penultimate,
        GeqEdge::Penultimate => GeqEdge::Exact,
        GeqEdge::Unknown => panic!("the geq edge is not measured on this machine"),
    };
    let mut fam = Family::default();
    let noise = media(160, 90, Pat::Noise, Tag::None);
    let under = media(160, 90, Pat::Edges, Tag::None);
    let mut c = clip(&noise, 0.0, 0.3, 0.0);
    c.mask = Some(mask_of(MaskShape::Rect, 0.0, false, 1.0));
    let tl = timeline(vec![vec![clip(&under, 0.0, 0.3, 0.0)], vec![c]], None);
    let assets = dedup(&[&noise, &under]);
    let case = "checker/b/m/geq pinned to the other build's edge";
    let played = export(case, &tl, &assets, &at_fps(30.0));
    let right = Compositor::new(gpu()).expect("a compositor").with_geq_edge(measured);
    let out = sweep_played_on(
        &right,
        &format!("{case} (right)"),
        &tl,
        &assets,
        &at_fps(30.0),
        9,
        copy_of(&played),
    );
    fam.add(case, &out);
    let lie = Compositor::new(gpu()).expect("a compositor").with_geq_edge(wrong);
    let lied = sweep_played_on(&lie, &format!("{case} (wrong)"), &tl, &assets, &at_fps(30.0), 9, played);
    assert!(
        out.drawn > 0 && lied.drawn > 0,
        "nothing drawn: {:?} {:?}",
        out.reasons,
        lied.reasons
    );
    assert!(
        !lied.bad.is_empty(),
        "a compositor pinned to the other build's geq edge ({wrong:?} on a build that reads {measured:?}) drew the export's frames: the pin is not read"
    );
    fam.finish("the geq edge pin is read");
}

/// What the plan says of a layer the scaler resizes, when the compositor is told this FFmpeg's scaler is not the model's (`Differs`) or has not
/// been measured (`Unknown`): every layer a scale resizes is refused (`ScalerApproximate`), nothing that no scale resizes is, and every frame
/// drawn is the export's. Lane B's rule beside lane A's placement and lane E's turn.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_a_scaler_that_is_not_measured_refuses_the_layers_it_resizes_and_only_those() {
    use kerf_gpu::scaler::Kernels;
    const REASON: &str = "not measured to be swscale's";
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let wide = Some(Delivery::new(160, 90, Fit::Contain));
    // (label, source, delivery, edit, resized by a scale?)
    let cases: Vec<Resizing> = vec![
        ("same size", (160, 90), None, Box::new(|_| {}), false),
        ("letterbox without a resize", (160, 60), wide, Box::new(|_| {}), false),
        ("pillarbox without a resize", (60, 90), wide, Box::new(|_| {}), false),
        ("odd-height pad without a resize", (160, 61), wide, Box::new(|_| {}), false),
        (
            "turned 180",
            (160, 90),
            None,
            Box::new(|c| c.transform.rotation = 180.0),
            false,
        ),
        (
            "moving zoom of a picture at the frame's size",
            (160, 90),
            None,
            Box::new(|c| *c = posed(c.clone(), Pose::ZoomIn)),
            false,
        ),
        ("shrunk by the fit", (320, 180), wide, Box::new(|_| {}), true),
        ("enlarged by the fit", (80, 45), wide, Box::new(|_| {}), true),
        ("4:3 enlarged into a letterbox", (80, 60), wide, Box::new(|_| {}), true),
        (
            "held zoom out",
            (160, 90),
            None,
            Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom)),
            true,
        ),
        ("held zoom in", (160, 90), None, Box::new(|c| c.transform.scale = 1.3), true),
        (
            "moving zoom of a shrunk picture",
            (320, 180),
            wide,
            Box::new(|c| *c = posed(c.clone(), Pose::ZoomOut)),
            true,
        ),
        (
            "turned 90 and shrunk",
            (160, 90),
            None,
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
            }),
            true,
        ),
        (
            "blur-filled portrait",
            (90, 160),
            Some(Delivery::new(160, 90, Fit::Blur)),
            Box::new(|_| {}),
            true,
        ),
    ];
    let pins = [("Differs", Kernels::Differs), ("Unknown", Kernels::Unknown)];
    let comps: Vec<(&str, Compositor)> = pins
        .iter()
        .map(|(name, k)| (*name, Compositor::new(gpu()).expect("a compositor").with_scaler_kernels(*k)))
        .collect();
    for (label, (w, h), delivery, edit, resized) in &cases {
        let noise = media(*w, *h, Pat::Noise, Tag::None);
        let mut c = clip(&noise, 0.0, 0.4, 0.0);
        edit(&mut c);
        let tl = timeline(vec![vec![c]], *delivery);
        let case = format!("checker/b/m/scaler-pin {label}");
        let assets = [noise.clone()];
        let Some(played) = guarded(&case, || try_export(&case, &tl, &assets, &at_fps(30.0)).ok()).flatten() else {
            continue;
        };
        for (name, comp) in &comps {
            let case = format!("{case} ({name})");
            let Some(out) = guarded(&case, || {
                sweep_played_on(comp, &case, &tl, &assets, &at_fps(30.0), 12, copy_of(&played))
            }) else {
                continue;
            };
            fam.add(&case, &out);
            groups.add(
                &format!("{} / {name}", if *resized { "resized" } else { "not resized" }),
                &out,
            );
            if *resized {
                expect_refusal(&mut fam, &case, &out, REASON, Want::Refused);
                if out.drawn != 0 && !patched_out() {
                    fam.failures.push(format!(
                        "{case}: {} frames of a resized layer drawn with the scaler unmeasured",
                        out.drawn
                    ));
                }
            } else {
                expect_refusal(&mut fam, &case, &out, REASON, Want::NotRefused);
            }
        }
    }
    groups.print("a scaler that is not measured");
    assert!(
        groups.drawn("not resized / Differs") > 0 && groups.drawn("not resized / Unknown") > 0,
        "nothing unresized was drawn"
    );
    fam.finish("a scaler that is not measured refuses the layers it resizes and only those");
}

/// A conversion between matrices that is not measured (`Differs`) refuses the clip of another matrix where the composite follows the bottom
/// layer's tag (9.0.2), whatever lane A or lane E did to the clip; where the composite is BT.601 whatever the tags say (6.1.1) it changes
/// nothing. Every drawn frame is the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_an_unmeasured_matrix_conversion_refuses_the_other_clip_and_only_it() {
    use kerf_gpu::convert::Kernels as Convert;
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let comps = [
        (
            "Differs",
            Compositor::new(gpu())
                .expect("a compositor")
                .with_convert_kernels(Convert::Differs("rehearsed")),
        ),
        (
            "Unknown",
            Compositor::new(gpu())
                .expect("a compositor")
                .with_convert_kernels(Convert::Unknown),
        ),
    ];
    let frame = Some(Delivery::new(160, 90, Fit::Contain));
    let first = media(160, 90, Pat::Noise, Tag::None);
    let other = media(160, 90, Pat::Test, Tag::Bt709);
    let same = media(160, 90, Pat::Test, Tag::None);
    let edits: Vec<(&str, Edit)> = vec![
        ("plain", Box::new(|_| {})),
        ("moving zoom", Box::new(|c| *c = posed(c.clone(), Pose::ZoomIn))),
        ("held zoom out", Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom))),
        (
            "turned 90 shrunk",
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
            }),
        ),
        ("turned 180", Box::new(|c| c.transform.rotation = 180.0)),
    ];
    for (label, edit) in &edits {
        for (second, what) in [(&other, "709 clip"), (&same, "601 clip (control)")] {
            let mut b = clip(second, 0.0, 0.4, 0.4);
            edit(&mut b);
            let tl = timeline(vec![vec![clip(&first, 0.0, 0.4, 0.0), b]], frame);
            let case = format!("checker/b/m/convert-pin {what} {label}");
            let assets = dedup(&[&first, second]);
            let Some(played) = guarded(&case, || try_export(&case, &tl, &assets, &at_fps(30.0)).ok()).flatten() else {
                continue;
            };
            for (name, comp) in &comps {
                let case = format!("{case} ({name})");
                let Some(out) = guarded(&case, || {
                    sweep_played_on(comp, &case, &tl, &assets, &at_fps(30.0), 24, copy_of(&played))
                }) else {
                    continue;
                };
                fam.add(&case, &out);
                groups.add(&format!("{what} / {name}"), &out);
                let mixed = what.starts_with("709") && negotiates();
                if mixed
                    && out.refused_for("different YCbCr matrices") + out.refused_for("colour matrix other than") == 0
                    && !patched_out()
                {
                    fam.failures.push(format!(
                        "{case}: the other clip's frames were not refused for the matrix: {:?}",
                        out.reasons
                    ));
                }
                if !mixed && out.refused != 0 && !patched_out() {
                    fam.failures
                        .push(format!("{case}: refused with nothing to convert: {:?}", out.reasons));
                }
            }
        }
    }
    groups.print("an unmeasured matrix conversion");
    fam.finish("an unmeasured matrix conversion refuses the other clip and only it");
}

/// A translucent layer (an opacity below 1) turned, over a base. The translucent layer has a ladder of its own against the export (the RGB
/// round trip `colorchannelmixer` makes it take: chroma within one level, luma exact: `TRANSLUCENT` in `motion_parity`). Turned a quarter or
/// three quarters the RGBA picture is turned before the way back to 4:2:0 and the compositor's round trip is not, so the plan refuses it (a
/// drawn frame there was up to 25 levels of chroma off); an upright and a half-turned layer are the controls and keep the ladder. A drawn
/// frame beyond the ladder is a failure, a quarter turn that is drawn is one, and so is a refusal of the upright or half-turned layer.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_translucent_layers_turned_a_quarter_stay_on_the_translucent_ladder() {
    const LADDER: i32 = 1;
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let under = with_edges(160, 90);
    for pat in [Pat::Test, Pat::Noise, Pat::Bars] {
        let top = media(160, 90, pat, Tag::None);
        for opacity in [0.25, 0.6, 0.9] {
            for scale in [1.0, 0.5625] {
                for turn in [0.0, 90.0, 180.0, 270.0] {
                    let mut c = clip(&top, 0.0, 0.3, 0.0);
                    c.transform.rotation = turn;
                    c.transform.opacity = opacity;
                    c.transform.scale = scale;
                    let tl = timeline(vec![vec![clip(&under, 0.0, 0.3, 0.0)], vec![c]], None);
                    let case = format!("checker/b/m/translucent-turn {pat:?} opacity {opacity} scale {scale} turn {turn}");
                    let assets = dedup(&[&top, &under]);
                    let Some(out) = sweep_guarded(&case, &tl, &assets, 30.0, 0.3) else {
                        continue;
                    };
                    groups.add(&format!("{pat:?} turn {turn}"), &out);
                    let worst = out.bad.iter().flat_map(|b| b.planes.iter().map(|p| p.max)).max().unwrap_or(0);
                    if worst <= LADDER && turn % 180.0 == 0.0 {
                        fam.add_known_ladder(&case, &out);
                    } else {
                        fam.add(&case, &out);
                    }
                    let quarter = turn % 180.0 != 0.0;
                    expect_refusal(
                        &mut fam,
                        &case,
                        &out,
                        TRANSLUCENT_TURN,
                        if quarter { Want::Refused } else { Want::NotRefused },
                    );
                    if quarter && out.drawn != 0 && !patched_out() {
                        fam.failures
                            .push(format!("{case}: {} frames of a translucent quarter turn drawn", out.drawn));
                    }
                }
            }
        }
    }
    groups.print("translucent layers turned a quarter");
    fam.finish("translucent layers turned a quarter stay on the translucent ladder");
}

/// The BT.709 label (an untagged picture of 1280x720 or more is read as BT.709 by the whole graph that holds a matrix) beside placement, turns
/// and pads: an untagged HD clip that moves, is held-zoomed, turned a quarter or letterboxed, alone, over a BT.709 base and under a BT.601 one.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_a_labelled_hd_clip_with_placement_turns_and_pads() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let hd = media(1280, 720, Pat::Test, Tag::None);
    let hd_noise = media(1280, 720, Pat::Noise, Tag::None);
    let base709 = media(1280, 720, Pat::Edges, Tag::Bt709);
    let base601 = media(1280, 720, Pat::Edges, Tag::Smpte170m);
    let edits: Vec<(&str, Option<Delivery>, Edit)> = vec![
        ("plain", None, Box::new(|_| {})),
        ("moving zoom", None, Box::new(|c| *c = posed(c.clone(), Pose::ZoomIn))),
        ("moving pan", None, Box::new(|c| *c = posed(c.clone(), Pose::Pan))),
        ("held zoom out", None, Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom))),
        ("turned 180", None, Box::new(|c| c.transform.rotation = 180.0)),
        (
            "turned 90 shrunk",
            None,
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
            }),
        ),
        (
            "letterboxed into 1280x800",
            Some(Delivery::new(1280, 800, Fit::Contain)),
            Box::new(|_| {}),
        ),
        (
            "shrunk into 640x360",
            Some(Delivery::new(640, 360, Fit::Contain)),
            Box::new(|_| {}),
        ),
    ];
    for (label, delivery, edit) in &edits {
        for (top, top_name) in [(&hd, "test"), (&hd_noise, "noise")] {
            let mut c = clip(top, 0.0, 0.2, 0.0);
            edit(&mut c);
            let tl = timeline(vec![vec![c.clone()]], *delivery);
            go(
                &mut fam,
                &mut groups,
                &format!("{label} / alone"),
                &format!("checker/b/m/hd-label {top_name} {label} alone"),
                &tl,
                std::slice::from_ref(top),
                0.2,
            );
            for (base, base_name) in [(&base709, "over 709"), (&base601, "over 601")] {
                let tl = timeline(vec![vec![clip(base, 0.0, 0.2, 0.0)], vec![c.clone()]], *delivery);
                go(
                    &mut fam,
                    &mut groups,
                    &format!("{label} / {base_name}"),
                    &format!("checker/b/m/hd-label {top_name} {label} {base_name}"),
                    &tl,
                    &dedup(&[top, base]),
                    0.2,
                );
            }
        }
    }
    groups.print("a labelled HD clip with placement, turns and pads");
    fam.finish("a labelled HD clip with placement, turns and pads");
}

/// The guard of [`no_geq_refusal`] fires: a sweep that meets a refusal for a mask or a keyed opacity (what a compositor without the `geq` edge
/// gives every masked clip) fails the case instead of counting the frame as refused; every other refusal passes it.
#[test]
fn checker_b_m_a_geq_refusal_in_a_sweep_fails_the_case_instead_of_passing_as_refused() {
    use kerf_core::{LayerRef, Unsupported};
    let layer = || LayerRef {
        index: 0,
        name: "a.mp4".into(),
    };
    for gate in [Unsupported::Mask(layer()), Unsupported::KeyedOpacity(layer())] {
        let refused = std::panic::catch_unwind(|| no_geq_refusal("a case", &[Unsupported::KeyedMask(layer()), gate.clone()]));
        assert!(refused.is_err(), "{gate:?} passed the guard");
    }
    no_geq_refusal(
        "a case",
        &[
            Unsupported::KeyedMask(layer()),
            Unsupported::MaskWith(layer(), "on a rotated layer"),
        ],
    );
    no_geq_refusal("a case", &[]);
}

/// A quarter turn beside what lane B draws on the same picture: a colour correction, an effect, a crop, a speed, at the fit and shrunk, over a
/// base. Drawn exactly or refused, never drawn wrong.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_quarter_turns_with_effects_colour_crop_and_speed() {
    use kerf_core::VideoEffect;
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let under = with_edges(160, 90);
    let noise = media(160, 90, Pat::Noise, Tag::None);
    let test = media(160, 90, Pat::Test, Tag::None);
    let edits: Vec<(&str, Edit)> = vec![
        ("contrast", Box::new(|c| c.color.contrast = 1.3)),
        (
            "saturation and gamma",
            Box::new(|c| {
                c.color.saturation = 1.4;
                c.color.gamma = 1.2;
            }),
        ),
        (
            "brightness and temperature",
            Box::new(|c| {
                c.color.brightness = 0.1;
                c.color.temperature = 0.4;
            }),
        ),
        ("blur", Box::new(|c| c.effects = vec![VideoEffect::Blur { sigma: 2.0 }])),
        (
            "sharpen",
            Box::new(|c| c.effects = vec![VideoEffect::Sharpen { amount: 0.5 }]),
        ),
        ("grayscale", Box::new(|c| c.effects = vec![VideoEffect::Grayscale])),
        ("invert", Box::new(|c| c.effects = vec![VideoEffect::Invert])),
        ("vignette", Box::new(|c| c.effects = vec![VideoEffect::Vignette])),
        (
            "chroma key",
            Box::new(|c| {
                c.effects = vec![VideoEffect::ChromaKey {
                    color: "0x00ff00".into(),
                    similarity: 0.3,
                    blend: 0.1,
                }];
            }),
        ),
        (
            "cropped",
            Box::new(|c| {
                c.transform.crop_left = 0.1;
                c.transform.crop_right = 0.07;
            }),
        ),
        ("speed 2", Box::new(|c| c.speed = 2.0)),
        ("speed 0.5", Box::new(|c| c.speed = 0.5)),
    ];
    for (label, edit) in &edits {
        for (src, pat) in [(&noise, "noise"), (&test, "test")] {
            for turn in [90.0, 180.0, 270.0] {
                for scale in [1.0, 0.5625] {
                    let mut c = clip(src, 0.0, 0.6, 0.0);
                    c.transform.rotation = turn;
                    c.transform.scale = scale;
                    edit(&mut c);
                    let tl = timeline(vec![vec![clip(&under, 0.0, 0.3, 0.0)], vec![c]], None);
                    go(
                        &mut fam,
                        &mut groups,
                        label,
                        &format!("checker/b/m/turn-fx {label} {pat} turn {turn} scale {scale}"),
                        &tl,
                        &dedup(&[src, &under]),
                        0.3,
                    );
                }
            }
        }
    }
    groups.print("quarter turns with effects, colour, crop and speed");
    fam.finish("quarter turns with effects, colour, crop and speed");
}

/// A dissolve or a dip into a clip of another colour matrix that the fit resizes, letterboxes or places (the phone clip pillarboxed into a
/// landscape frame, a 4:3 clip enlarged, a picture-in-picture, a moving zoom), with and without a fade to black running on it: where the composite
/// follows the bottom layer's tag (9.0.2) the plan refuses the frames whose conversion FFmpeg makes in the same pass as the resize; every frame
/// it draws is the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_a_dissolve_into_another_matrix_over_a_resize_a_letterbox_or_a_placement() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let frame = Some(Delivery::new(160, 90, Fit::Contain));
    let pairs = [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None), (Tag::Bt709, Tag::Bt2020)];
    let variants: Vec<Variant> = vec![
        ("plain", (160, 90), frame, Box::new(|_| {})),
        ("portrait pillarboxed", (90, 160), frame, Box::new(|_| {})),
        ("4:3 enlarged", (80, 60), frame, Box::new(|_| {})),
        ("shrunk 2:1 by the fit", (320, 180), frame, Box::new(|_| {})),
        (
            "held zoom out",
            (160, 90),
            frame,
            Box::new(|c| *c = posed(c.clone(), Pose::HeldZoom)),
        ),
        (
            "moving zoom",
            (160, 90),
            frame,
            Box::new(|c| *c = posed(c.clone(), Pose::ZoomIn)),
        ),
        ("fade out running", (160, 90), frame, Box::new(|c| c.fade_out = 0.2)),
        ("portrait with a fade out", (90, 160), frame, Box::new(|c| c.fade_out = 0.2)),
        (
            "turned 90 shrunk",
            (160, 90),
            frame,
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
            }),
        ),
    ];
    for (a_tag, b_tag) in pairs {
        let first = media(160, 90, Pat::Noise, a_tag);
        for (label, (w, h), delivery, edit) in &variants {
            let second = media(*w, *h, Pat::Test, b_tag);
            for kind in [TransitionKind::Crossfade, TransitionKind::DipToBlack] {
                let mut b = clip(&second, 0.0, 0.5, 0.5);
                edit(&mut b);
                let tl = timeline(
                    vec![vec![clip(&first, 0.0, 0.5, 0.0), dissolve_into(b, kind, 0.3)]],
                    *delivery,
                );
                go(
                    &mut fam,
                    &mut groups,
                    &format!("{label} / {kind:?}"),
                    &format!("checker/b/m/dissolve-matrix {a_tag:?} into {b_tag:?} {label} {kind:?}"),
                    &tl,
                    &dedup(&[&first, &second]),
                    0.9,
                );
            }
        }
    }
    groups.print("a dissolve into another matrix over a resize, a letterbox or a placement");
    assert!(
        groups.drawn("plain / Crossfade") > 0,
        "a plain dissolve into another matrix was never drawn"
    );
    fam.finish("a dissolve into another matrix over a resize, a letterbox or a placement");
}

/// The shape of the coverage ledger's T5: a vlog of portrait phone clips pillarboxed into a landscape frame, alternating BT.709 and BT.601, each
/// dissolving into the next, with a fade in and a fade out at the ends, at 30 and 60 fps, over a plain fit and a blurred fill. Every frame
/// the plan draws is the export's (the frames it refuses are FFmpeg's: on 9.0.2 the conversion into the bottom layer's matrix is made in the
/// resize's own pass while a clip dissolves).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_a_phone_vlog_of_alternating_matrices_dissolving() {
    let mut fam = Family::default();
    let mut groups = Groups::default();
    for (fit, fit_name) in [(Fit::Contain, "contain"), (Fit::Blur, "blur")] {
        for fps in [30.0, 60.0] {
            for (tags, tag_name) in [
                ([Tag::Bt709, Tag::None, Tag::Bt709], "709 601 709"),
                ([Tag::None, Tag::Bt709, Tag::None], "601 709 601"),
                ([Tag::Bt709, Tag::Bt709, Tag::None], "709 709 601"),
            ] {
                let assets: Vec<Asset> = tags.iter().map(|t| media(90, 160, Pat::Test, *t)).collect();
                let mut clips: Vec<Clip> = Vec::new();
                for (i, asset) in assets.iter().enumerate() {
                    let mut c = clip(asset, 0.0, 0.5, 0.4 * i as f64);
                    if i > 0 {
                        c.transition_in = Some(kerf_core::Transition {
                            kind: TransitionKind::Crossfade,
                            duration: 0.1,
                        });
                    }
                    clips.push(c);
                }
                clips[0].fade_in = 0.1;
                if let Some(last) = clips.last_mut() {
                    last.fade_out = 0.1;
                }
                let delivery = Delivery::new(160, 90, fit).with_fps(Some(fps)).expect("a rate");
                let tl = timeline(vec![clips], Some(delivery));
                let case = format!("checker/b/m/vlog {fit_name} {fps} fps {tag_name}");
                go_at(
                    &mut fam,
                    &mut groups,
                    &format!("{fit_name} {fps} fps"),
                    &case,
                    &tl,
                    &dedup(&assets.iter().collect::<Vec<_>>()),
                    fps,
                    1.2,
                );
            }
        }
    }
    groups.print("a phone vlog of alternating matrices dissolving");
    fam.finish("a phone vlog of alternating matrices dissolving");
}

/// A title (FFmpeg's `drawtext`, blended by the compositor) over the layers of both lanes: a placed one, a turned one, a resized and
/// letterboxed one, a blurred fill, a blended one and a masked one. The composite under the text is the layer's, so the text's own exactness
/// depends on it: every drawn frame is the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_a_title_over_placed_turned_resized_and_blended_layers() {
    use kerf_core::{BlendMode, TextOverlay};
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let canvas = Some(Delivery::new(320, 180, Fit::Contain));
    let under = media(320, 180, Pat::Edges, Tag::None);
    let top = media(320, 180, Pat::Test, Tag::None);
    let portrait = media(180, 320, Pat::Test, Tag::None);
    let shrunk = media(640, 360, Pat::Noise, Tag::None);
    let cases: Vec<TitleCase> = vec![
        ("plain", canvas, vec![vec![clip(&top, 0.0, 0.4, 0.0)]], vec![top.clone()]),
        (
            "moving zoom",
            canvas,
            vec![vec![posed(clip(&top, 0.0, 0.4, 0.0), Pose::ZoomIn)]],
            vec![top.clone()],
        ),
        (
            "moving pan over a base",
            canvas,
            vec![
                vec![clip(&under, 0.0, 0.4, 0.0)],
                vec![posed(clip(&top, 0.0, 0.4, 0.0), Pose::Pan)],
            ],
            dedup(&[&under, &top]),
        ),
        (
            "turned 90 shrunk",
            canvas,
            vec![vec![{
                let mut c = clip(&top, 0.0, 0.4, 0.0);
                c.transform.rotation = 90.0;
                c.transform.scale = 0.5625;
                c
            }]],
            vec![top.clone()],
        ),
        (
            "turned 180 over a base",
            canvas,
            vec![
                vec![clip(&under, 0.0, 0.4, 0.0)],
                vec![{
                    let mut c = clip(&top, 0.0, 0.4, 0.0);
                    c.transform.rotation = 180.0;
                    c
                }],
            ],
            dedup(&[&under, &top]),
        ),
        (
            "shrunk by the fit",
            canvas,
            vec![vec![clip(&shrunk, 0.0, 0.4, 0.0)]],
            vec![shrunk.clone()],
        ),
        (
            "portrait pillarboxed",
            canvas,
            vec![vec![clip(&portrait, 0.0, 0.4, 0.0)]],
            vec![portrait.clone()],
        ),
        (
            "portrait over a blurred fill",
            Some(Delivery::new(320, 180, Fit::Blur)),
            vec![vec![clip(&portrait, 0.0, 0.4, 0.0)]],
            vec![portrait.clone()],
        ),
        (
            "blended over a base",
            canvas,
            vec![
                vec![clip(&under, 0.0, 0.4, 0.0)],
                vec![{
                    let mut c = clip(&top, 0.0, 0.4, 0.0);
                    c.blend_mode = BlendMode::Multiply;
                    c
                }],
            ],
            dedup(&[&under, &top]),
        ),
        (
            "masked over a base",
            canvas,
            vec![
                vec![clip(&under, 0.0, 0.4, 0.0)],
                vec![{
                    let mut c = clip(&top, 0.0, 0.4, 0.0);
                    c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false, 0.7));
                    c
                }],
            ],
            dedup(&[&under, &top]),
        ),
    ];
    for (label, delivery, tracks, assets) in cases {
        for (text, name) in [("Day 3: the coast", "title"), ("Wg ij", "tall glyphs")] {
            let mut tl = timeline(tracks.clone(), delivery);
            let mut overlay = TextOverlay::new(text, 0.0, 0.4);
            overlay.bg = Some("black@0.5".into());
            tl.overlays = vec![overlay];
            go(
                &mut fam,
                &mut groups,
                label,
                &format!("checker/b/m/text {label} {name}"),
                &tl,
                &assets,
                0.4,
            );
        }
    }
    groups.print("a title over placed, turned, resized and blended layers");
    fam.finish("a title over placed, turned, resized and blended layers");
}

/// The refusal of a translucent quarter turn is exactly that: a constant opacity below 1 (the RGB round trip) under a turn of 90 or 270
/// degrees. Everything else under a quarter turn is drawn, and drawn to the sample: an opaque layer alone, faded in and out and coming in
/// through a dissolve, a dip to black, a slide or a push; an opacity keyed (a `geq` alpha on `yuva420p`: a ramp, a constant below 1, a constant
/// at 1, one key, with a fade); an opacity of exactly 1; the upright and half-turned layers (at the translucent ladder). A turn a hair off a
/// quarter (within what `rotate` takes for one) is the quarter's, one farther off is another refusal's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_the_translucent_quarter_turn_refusal_is_tight() {
    use kerf_core::{Property, PropertyKey};
    const LADDER: i32 = 1;
    let mut fam = Family::default();
    let mut groups = Groups::default();
    let under = with_edges(160, 90);
    let first = media(160, 90, Pat::Bars, Tag::None);
    let noise = media(160, 90, Pat::Noise, Tag::None);
    let test = media(160, 90, Pat::Test, Tag::None);
    // (label, edit of the turned clip, whether its opacity is a constant below 1, the clip comes in through a transition)
    type Variant = (&'static str, Box<dyn Fn(&mut Clip)>, bool, Option<TransitionKind>);
    let keyed = |keys: Vec<(f64, f64)>| -> Box<dyn Fn(&mut Clip)> {
        Box::new(move |c: &mut Clip| {
            c.set_property_keys(Property::Opacity, keys.iter().map(|&(t, v)| PropertyKey::new(t, v)).collect());
        })
    };
    let variants: Vec<Variant> = vec![
        ("static 0.6", Box::new(|c| c.transform.opacity = 0.6), true, None),
        ("static 0.999", Box::new(|c| c.transform.opacity = 0.999), true, None),
        ("static 0.25", Box::new(|c| c.transform.opacity = 0.25), true, None),
        (
            "static 0.6 and faded",
            Box::new(|c| {
                c.transform.opacity = 0.6;
                c.fade_in = 0.15;
                c.fade_out = 0.15;
            }),
            true,
            None,
        ),
        ("static 1.0", Box::new(|c| c.transform.opacity = 1.0), false, None),
        ("opaque", Box::new(|_| {}), false, None),
        (
            "opaque and faded",
            Box::new(|c| {
                c.fade_in = 0.15;
                c.fade_out = 0.15;
            }),
            false,
            None,
        ),
        ("keyed ramp", keyed(vec![(0.0, 1.0), (0.3, 0.3)]), false, None),
        // (A keyed opacity overrules the static one: the export writes no `colorchannelmixer` for it.)
        (
            "static 0.6 under a keyed ramp",
            Box::new({
                let k = keyed(vec![(0.0, 1.0), (0.3, 0.3)]);
                move |c| {
                    c.transform.opacity = 0.6;
                    k(c);
                }
            }),
            false,
            None,
        ),
        (
            "static 0.6 under keyed constant 1",
            Box::new({
                let k = keyed(vec![(0.0, 1.0), (0.3, 1.0)]);
                move |c| {
                    c.transform.opacity = 0.6;
                    k(c);
                }
            }),
            false,
            None,
        ),
        ("keyed constant 0.6", keyed(vec![(0.0, 0.6), (0.3, 0.6)]), false, None),
        ("keyed constant 1", keyed(vec![(0.0, 1.0), (0.3, 1.0)]), false, None),
        ("keyed one key 0.6", keyed(vec![(0.0, 0.6)]), false, None),
        (
            "keyed ramp and faded in",
            Box::new({
                let k = keyed(vec![(0.0, 1.0), (0.3, 0.4)]);
                move |c| {
                    k(c);
                    c.fade_in = 0.15;
                }
            }),
            false,
            None,
        ),
        (
            "opaque in through a crossfade",
            Box::new(|_| {}),
            false,
            Some(TransitionKind::Crossfade),
        ),
        (
            "opaque in through a dip to black",
            Box::new(|_| {}),
            false,
            Some(TransitionKind::DipToBlack),
        ),
        (
            "opaque in through a slide",
            Box::new(|_| {}),
            false,
            Some(TransitionKind::SlideLeft),
        ),
        (
            "opaque in through a push",
            Box::new(|_| {}),
            false,
            Some(TransitionKind::PushUp),
        ),
        (
            "opaque, faded in, through a crossfade",
            Box::new(|c| c.fade_in = 0.1),
            false,
            Some(TransitionKind::Crossfade),
        ),
        (
            "static 0.6 in through a crossfade",
            Box::new(|c| c.transform.opacity = 0.6),
            true,
            Some(TransitionKind::Crossfade),
        ),
        (
            "keyed ramp in through a crossfade",
            keyed(vec![(0.0, 1.0), (0.3, 0.4)]),
            false,
            Some(TransitionKind::Crossfade),
        ),
    ];
    // 90.000001 degrees is inside what `rotate` takes for a quarter (FLT_EPSILON radians); 90.00001 is not.
    let turns: [(f64, bool); 9] = [
        (0.0, false),
        (90.0, true),
        (180.0, false),
        (270.0, true),
        (-90.0, true),
        (450.0, true),
        (360.0, false),
        (90.000001, true),
        (90.00001, false),
    ];
    for (label, edit, translucent, transition) in &variants {
        for (turn, quarter) in turns {
            for (scale, src) in [(1.0, &noise), (0.5625, &test)] {
                let mut c = clip(src, 0.0, 0.3, if transition.is_some() { 0.3 } else { 0.0 });
                c.transform.rotation = turn;
                c.transform.scale = scale;
                edit(&mut c);
                let tracks = match transition {
                    Some(kind) => {
                        let c = dissolve_into(c, *kind, 0.2);
                        vec![vec![clip(&first, 0.0, 0.3, 0.0), c]]
                    }
                    None => vec![vec![clip(&under, 0.0, 0.3, 0.0)], vec![c]],
                };
                let tl = timeline(tracks, None);
                let case = format!("checker/b/m/translucent-tight {label} turn {turn} scale {scale}");
                let assets = dedup(&[src, &under, &first]);
                let secs = if transition.is_some() { 0.6 } else { 0.3 };
                let Some(out) = sweep_guarded(&case, &tl, &assets, 30.0, secs) else {
                    continue;
                };
                groups.add(label, &out);
                let worst = out.bad.iter().flat_map(|b| b.planes.iter().map(|p| p.max)).max().unwrap_or(0);
                // Drawn frames are the export's; the translucent ladder (one level of chroma) is allowed to an upright or half-turned layer.
                if *translucent && worst <= LADDER && !quarter {
                    fam.add_known_ladder(&case, &out);
                } else {
                    fam.add(&case, &out);
                }
                let refused = *translucent && quarter;
                if refused {
                    // (Every frame of the clip is refused; the frames before it, in a transition, are another layer's.)
                    let n = out.refused_for(TRANSLUCENT_TURN);
                    if n == 0 && !patched_out() {
                        fam.failures.push(format!(
                            "{case}: a translucent quarter turn was not refused for it ({:?})",
                            out.reasons
                        ));
                    }
                } else {
                    expect_refusal(&mut fam, &case, &out, TRANSLUCENT_TURN, Want::NotRefused);
                }
            }
        }
    }
    groups.print("the translucent quarter-turn refusal");
    assert!(
        groups.drawn("opaque") > 0 && groups.drawn("keyed ramp") > 0,
        "nothing opaque or keyed was drawn"
    );
    fam.finish("the translucent quarter-turn refusal is tight");
}
