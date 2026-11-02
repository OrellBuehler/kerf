//! Lane B (the exact scaler and the blurred fill, a grade on a resized layer, layers of another colour matrix),
//! checked independently of the implementer's cases.
//!
//! Every case builds a short cut, renders it through `kerf_core::render_with` (FFmpeg's real export graph), and
//! holds every frame the plan **draws** to the export's planes with no tolerance: a differing sample in any plane
//! fails the case. A frame the plan refuses is FFmpeg's and is only counted (with its reasons), so a case passes
//! when every drawn frame is exact, and the cases that must stay refused say so. The per-frame outcome of every
//! case is written to `target/parity/checker-b/<case>.frames` so a run with a refusal patched out (the refusal's
//! necessity) can be diffed against one without.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_b_filters -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    Asset, Clip, Color, CompositeColorPolicy, Container, Delivery, ExportOptions, Fit, PlanMode, PlanRequest, Planner, Project,
    RateControl, StreamKind, Timeline, Track, Transition, TransitionKind,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames};

// ---- the machinery ---------------------------------------------------------------------------------------

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
        let _ = kerf_gpu::text::model();
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
    let dir = target_dir().join("parity").join("checker-b").join("montage");
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
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
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-ref-{}", build_key()));
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
    let _ = std::fs::write(dir.join(format!("checker-b-report-{}.txt", std::process::id())), text);
}

fn frame_log_dir() -> PathBuf {
    let mode = if std::env::var_os("KERF_CHECKER_ALLOW").is_some() {
        "allow"
    } else {
        "normal"
    };
    let dir = target_dir()
        .join("parity")
        .join("checker-b")
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
                "{k}\t{}INEXACT\tY {} (max {}) U {} (max {}) V {} (max {})",
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
            let _ = writeln!(log, "{k}\t{}exact", if was_refused { "ANYWAY-" } else { "" });
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

// ---- more media and cut builders -------------------------------------------------------------------------------

/// A clip with an alpha plane (`yuva420p`, FFV1 in Matroska): half of it opaque, half of it at 50 %.
fn alpha_media(w: u32, h: u32, tag: Tag) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("alpha-{w}x{h}-{tag:?}.mkv"));
    if !path.exists() {
        let lavfi = format!(
            "testsrc2=size={w}x{h}:rate=30:duration=4,format=yuva420p,\
             geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(gt(X,W/2),255,128)'{}",
            tag.filter()
        );
        let tmp = dir.join(format!("alpha-{w}x{h}-{tag:?}.tmp.mkv"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuva420p",
            tmp.to_str().unwrap(),
        ]);
        std::fs::rename(&tmp, &path).unwrap();
    }
    probed(&path)
}

/// A clip of the picture-in-picture kind: scaled by `scale` and moved.
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

const TAGS: [Tag; 5] = [Tag::None, Tag::Bt709, Tag::Smpte170m, Tag::Bt470bg, Tag::Bt2020];

/// The matrix a tag names for the composite: BT.601 for untagged / `smpte170m` / `bt470bg`.
fn same_matrix(a: Tag, b: Tag) -> bool {
    let class = |t: Tag| match t {
        Tag::None | Tag::Smpte170m | Tag::Bt470bg => 0,
        Tag::Bt709 => 1,
        Tag::Bt2020 => 2,
    };
    class(a) == class(b)
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

// ---- layers of another matrix: every pair, every arrangement ------------------------------------------------------

/// Two clips back to back on one track, every ordered pair of tags, on busy and smooth pictures at the canvas's own
/// size (nothing resized: the conversion alone).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_every_pair_in_sequence() {
    let mut fam = Family::default();
    for pat in [Pat::Noise, Pat::Test] {
        for a in TAGS {
            for b in TAGS {
                if a == b {
                    continue;
                }
                let (ma, mb) = (media(320, 180, pat, a), media(320, 180, pat, b));
                let tl = timeline(vec![vec![clip(&ma, 0.5, 1.2, 0.0), clip(&mb, 0.5, 1.2, 0.7)]], None);
                let case = format!("checker/b/matrix/seq {pat:?} {a:?} then {b:?}");
                let out = sweep_secs(&case, &tl, &dedup(&[&ma, &mb]), 30.0, 1.4);
                fam.add(&case, &out);
                if negotiates() && !same_matrix(a, b) && !patched_out() {
                    // The conversion is reproduced for a plain opaque picture of the canvas's size: nothing to refuse.
                    assert_eq!(out.refused, 0, "{case}: {:?}", out.reasons);
                }
            }
        }
    }
    fam.finish("matrix every pair in sequence");
}

/// A picture-in-picture of every tag over a full-frame clip of every other tag (the picture is scaled, then converted), and
/// the same layer full frame over the bottom one.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_every_pair_stacked() {
    let mut fam = Family::default();
    for (top_scale, label) in [(0.5, "pip"), (1.0, "full")] {
        for a in TAGS {
            for b in TAGS {
                if a == b {
                    continue;
                }
                let bottom = media(320, 180, Pat::Edges, a);
                let top = media(320, 180, Pat::Noise, b);
                let c = pip(clip(&top, 0.5, 1.2, 0.0), top_scale, 0.15, -0.1);
                let tl = timeline(vec![vec![clip(&bottom, 0.5, 1.2, 0.0)], vec![c]], None);
                let case = format!("checker/b/matrix/stack {label} {b:?} over {a:?}");
                let out = sweep_secs(&case, &tl, &dedup(&[&bottom, &top]), 30.0, 0.6);
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("matrix every pair stacked");
}

/// Which clip decides the composite's matrix: a clip that FFmpeg puts on an alpha plane by a route other than a dissolve
/// (a keyed zoom or position, a source with an alpha channel) against a plain one before it, in either order.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_deciding_clip_on_an_alpha_plane() {
    let mut fam = Family::default();
    let plain601 = media(320, 180, Pat::Noise, Tag::None);
    let plain709 = media(320, 180, Pat::Noise, Tag::Bt709);
    let plain2020 = media(320, 180, Pat::Noise, Tag::Bt2020);
    let alpha709 = alpha_media(320, 180, Tag::Bt709);
    let alpha601 = alpha_media(320, 180, Tag::None);
    let keyed_scale = |a: &Asset, start: f64| {
        let mut c = clip(a, 0.5, 1.2, start);
        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 0.8, 0.0, 0.0)];
        c
    };
    let keyed_pos = |a: &Asset, start: f64| {
        let mut c = clip(a, 0.5, 1.2, start);
        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 1.0, 0.1, 0.05)];
        c
    };
    let held_key = |a: &Asset, start: f64| {
        let mut c = clip(a, 0.5, 1.2, start);
        c.keyframes = vec![zoom_key(0.0, 0.9, 0.0, 0.0), zoom_key(0.7, 0.9, 0.0, 0.0)];
        c
    };
    type Make<'a> = Box<dyn Fn() -> (Vec<Vec<Clip>>, Vec<Asset>) + 'a>;
    let cases: Vec<(&str, Make)> = vec![
        (
            "601 plain, then 709 keyed scale",
            Box::new(|| {
                (
                    vec![vec![clip(&plain601, 0.5, 1.2, 0.0), keyed_scale(&plain709, 0.7)]],
                    dedup(&[&plain601, &plain709]),
                )
            }),
        ),
        (
            "601 plain, then 709 keyed position",
            Box::new(|| {
                (
                    vec![vec![clip(&plain601, 0.5, 1.2, 0.0), keyed_pos(&plain709, 0.7)]],
                    dedup(&[&plain601, &plain709]),
                )
            }),
        ),
        (
            "601 plain, then 709 held keys",
            Box::new(|| {
                (
                    vec![vec![clip(&plain601, 0.5, 1.2, 0.0), held_key(&plain709, 0.7)]],
                    dedup(&[&plain601, &plain709]),
                )
            }),
        ),
        (
            "709 plain, then 601 keyed scale",
            Box::new(|| {
                (
                    vec![vec![clip(&plain709, 0.5, 1.2, 0.0), keyed_scale(&plain601, 0.7)]],
                    dedup(&[&plain709, &plain601]),
                )
            }),
        ),
        (
            "601 plain, then 709 with an alpha channel",
            Box::new(|| {
                (
                    vec![vec![clip(&plain601, 0.5, 1.2, 0.0), clip(&alpha709, 0.5, 1.2, 0.7)]],
                    dedup(&[&plain601, &alpha709]),
                )
            }),
        ),
        (
            "709 with an alpha channel, then 601 plain",
            Box::new(|| {
                (
                    vec![vec![clip(&alpha709, 0.5, 1.2, 0.0), clip(&plain601, 0.5, 1.2, 0.7)]],
                    dedup(&[&alpha709, &plain601]),
                )
            }),
        ),
        (
            "601 plain, 601 with alpha, then 709 plain",
            Box::new(|| {
                (
                    vec![vec![
                        clip(&plain601, 0.5, 1.0, 0.0),
                        clip(&alpha601, 0.5, 1.0, 0.5),
                        clip(&plain709, 0.5, 1.0, 1.0),
                    ]],
                    dedup(&[&plain601, &alpha601, &plain709]),
                )
            }),
        ),
        (
            "601 plain, 709 plain, then 2020 keyed position",
            Box::new(|| {
                (
                    vec![vec![
                        clip(&plain601, 0.5, 1.0, 0.0),
                        clip(&plain709, 0.5, 1.0, 0.5),
                        keyed_pos(&plain2020, 1.0),
                    ]],
                    dedup(&[&plain601, &plain709, &plain2020]),
                )
            }),
        ),
        (
            "709 under, 601 over with keyed scale on a second track",
            Box::new(|| {
                (
                    vec![vec![clip(&plain709, 0.5, 1.2, 0.0)], vec![keyed_scale(&plain601, 0.0)]],
                    dedup(&[&plain709, &plain601]),
                )
            }),
        ),
        (
            "601 plain, then 709 screen-blended on a second track",
            Box::new(|| {
                let mut blended = clip(&plain709, 0.5, 1.2, 0.3);
                blended.blend_mode = kerf_core::BlendMode::Screen;
                (
                    vec![vec![clip(&plain601, 0.5, 1.2, 0.0)], vec![blended]],
                    dedup(&[&plain601, &plain709]),
                )
            }),
        ),
        (
            "601 plain, then 709 screen-blended after it",
            Box::new(|| {
                let mut blended = clip(&plain709, 0.5, 1.2, 0.7);
                blended.blend_mode = kerf_core::BlendMode::Screen;
                (
                    vec![vec![clip(&plain601, 0.5, 1.2, 0.0), blended]],
                    dedup(&[&plain601, &plain709]),
                )
            }),
        ),
        (
            "709 under, 601 over with an alpha channel on a second track",
            Box::new(|| {
                (
                    vec![vec![clip(&plain709, 0.5, 1.2, 0.0)], vec![clip(&alpha601, 0.5, 1.2, 0.0)]],
                    dedup(&[&plain709, &alpha601]),
                )
            }),
        ),
    ];
    for (name, make) in cases {
        let (tracks, assets) = make();
        let tl = timeline(tracks, None);
        let case = format!("checker/b/matrix/decider {name}");
        let out = sweep_secs(&case, &tl, &assets, 30.0, 1.4);
        fam.add(&case, &out);
    }
    fam.finish("matrix deciding clip on an alpha plane");
}

// ---- a grade on a layer the exact scaler resizes -------------------------------------------------------------------

fn curve(p: &[(f64, f64)]) -> Vec<kerf_core::CurvePoint> {
    p.iter().map(|&(x, y)| kerf_core::CurvePoint { x, y }).collect()
}

fn puck(x: f64, y: f64, lightness: f64) -> kerf_core::Wheel {
    kerf_core::Wheel { x, y, lightness }
}

fn cube(name: &str, size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-b-luts");
    std::fs::create_dir_all(&dir).unwrap();
    let n = (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                data.push(f([r as f32 / n, g as f32 / n, b as f32 / n]));
            }
        }
    }
    let table = kerf_core::color_lut::Lut3d {
        title: String::new(),
        size,
        data,
    };
    let path = dir.join(format!("{name}.cube"));
    common::write_fixture(&path, &table.to_cube(1.0));
    path.to_string_lossy().into_owned()
}

fn grades_b() -> Vec<(&'static str, kerf_core::Grade)> {
    use kerf_core::{ClipLut, Curves, Grade, Wheels};
    let look = cube("checker-look17", 17, |c| {
        [
            0.9 * c[0] + 0.1 * c[1] * c[2] + 0.02,
            0.07 * c[0] * c[0] + 0.88 * c[1] + 0.05 * c[2],
            (0.08 + 0.82 * c[2] + 0.12 * c[0] * c[1]).min(1.0),
        ]
    });
    let wheels = Wheels {
        shadows: puck(0.3, -0.25, 0.1),
        midtones: puck(-0.2, 0.4, -0.05),
        highlights: puck(0.1, 0.6, 0.15),
    };
    let curves = Curves {
        master: curve(&[(0.0, 0.03), (0.25, 0.15), (0.75, 0.85), (1.0, 0.98)]),
        red: curve(&[(0.0, 0.0), (0.5, 0.58), (1.0, 1.0)]),
        green: Vec::new(),
        blue: curve(&[(0.0, 0.1), (1.0, 0.8)]),
    };
    vec![
        (
            "curves",
            Grade {
                curves: Some(curves.clone()),
                ..Grade::default()
            },
        ),
        (
            "wheels",
            Grade {
                wheels: Some(wheels),
                ..Grade::default()
            },
        ),
        (
            "lut",
            Grade {
                lut: Some(ClipLut {
                    path: look.clone(),
                    intensity: 0.8,
                }),
                ..Grade::default()
            },
        ),
        (
            "all three",
            Grade {
                lut: Some(ClipLut {
                    path: look,
                    intensity: 0.6,
                }),
                curves: Some(curves),
                wheels: Some(wheels),
            },
        ),
    ]
}

fn eqs_b() -> Vec<(&'static str, Color)> {
    let c = |brightness, contrast, saturation, gamma, temperature| Color {
        brightness,
        contrast,
        saturation,
        gamma,
        temperature,
    };
    vec![
        ("contrast+saturation", c(0.0, 1.08, 1.15, 1.0, 0.0)),
        ("brightness only", c(0.1, 1.0, 1.0, 1.0, 0.0)),
        ("gamma only", c(0.0, 1.0, 1.0, 1.3, 0.0)),
        ("saturation 0", c(0.0, 1.0, 0.0, 1.0, 0.0)),
        ("temperature only", c(0.0, 1.0, 1.0, 1.0, 0.5)),
        ("a hair off identity", c(0.0, 1.0000001, 1.0, 1.0, 0.0)),
        ("steep", c(-0.2, 2.5, 2.0, 1.8, 0.0)),
    ]
}

/// A grade after an `eq`, on a layer the scaler resizes in every way the plan names (a picture-in-picture, a shrink, an
/// enlargement, a Cover crop), over every grade and a spread of `eq`s, on busy and hard-edged pictures: every drawn frame is
/// the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_grade_after_an_eq_on_a_resized_layer() {
    let mut fam = Family::default();
    let noise = media(320, 180, Pat::Noise, Tag::None);
    let edges = media(320, 180, Pat::Edges, Tag::None);
    let test709 = media(320, 180, Pat::Test, Tag::Bt709);
    // (name, source, delivery, transform scale)
    type Setup<'a> = (&'a str, &'a Asset, Option<Delivery>, f64);
    let setups: [Setup; 7] = [
        ("pip 0.3", &noise, None, 0.3),
        ("pip 0.75", &edges, None, 0.75),
        ("pip 0.99", &noise, None, 0.99),
        ("shrunk to 160x90", &noise, Some(Delivery::new(160, 90, Fit::Contain)), 1.0),
        (
            "enlarged to 640x360",
            &edges,
            Some(Delivery::new(640, 360, Fit::Contain)),
            1.0,
        ),
        ("cover into 180x180", &noise, Some(Delivery::new(180, 180, Fit::Cover)), 1.0),
        ("709 pip 0.6", &test709, None, 0.6),
    ];
    for (sname, src, delivery, scale) in setups {
        for (gname, grade) in grades_b() {
            // (Three eqs per setup keeps the family a few minutes: the steep one and two others.)
            for (ename, color) in eqs_b()
                .into_iter()
                .filter(|(n, _)| ["contrast+saturation", "gamma only", "steep"].contains(n))
            {
                let mut c = clip(src, 0.5, 1.0, 0.0);
                c.transform.scale = scale;
                c.color = color;
                c.grade = grade.clone();
                let tl = timeline(vec![vec![c]], delivery);
                let case = format!("checker/b/grade/{sname} {gname} after eq {ename}");
                let out = sweep_secs(&case, &tl, std::slice::from_ref(src), 30.0, 0.4);
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("grade after an eq on a resized layer");
}

/// The remaining `eq`s (one grade): temperature alone, brightness alone, saturation 0 and a hair off identity are still an
/// `eq` in the chain and the plan draws them.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_grade_after_each_kind_of_eq_on_a_pip() {
    let mut fam = Family::default();
    let noise = media(320, 180, Pat::Noise, Tag::None);
    let (_, grade) = grades_b().into_iter().find(|(n, _)| *n == "all three").unwrap();
    for (ename, color) in eqs_b() {
        let mut c = clip(&noise, 0.5, 1.0, 0.0);
        c.transform.scale = 0.6;
        c.color = color;
        c.grade = grade.clone();
        let tl = timeline(vec![vec![c]], None);
        let case = format!("checker/b/grade/pip all three after eq {ename}");
        let out = sweep_secs(&case, &tl, std::slice::from_ref(&noise), 30.0, 0.4);
        fam.add(&case, &out);
    }
    fam.finish("grade after each kind of eq on a pip");
}

/// With no `eq` ahead of the grade the fit `scale` converts to RGB in the same pass, from chroma the compositor has already
/// scaled: such a layer is the plan's refusal, in every frame, and nothing else is drawn inexact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_grade_without_an_eq_on_a_resized_layer_stays_refused() {
    let mut fam = Family::default();
    let noise = media(320, 180, Pat::Noise, Tag::None);
    let edges = media(320, 180, Pat::Edges, Tag::None);
    for (sname, src, delivery, scale) in [
        ("pip 0.6", &noise, None, 0.6),
        ("pip 0.97", &edges, None, 0.97),
        ("shrunk", &noise, Some(Delivery::new(160, 90, Fit::Contain)), 1.0),
        ("enlarged", &edges, Some(Delivery::new(640, 360, Fit::Contain)), 1.0),
        ("cover", &noise, Some(Delivery::new(160, 160, Fit::Cover)), 1.0),
        // (A Cover crop that does not resize the picture - 320x180 into 180x180 scales to its own size - has no scaler in it.)
        ("cover only cropped", &noise, Some(Delivery::new(180, 180, Fit::Cover)), 1.0),
    ] {
        for (gname, grade) in grades_b() {
            let mut c = clip(src, 0.5, 1.0, 0.0);
            c.transform.scale = scale;
            c.grade = grade;
            let tl = timeline(vec![vec![c]], delivery);
            let case = format!("checker/b/grade/no eq {sname} {gname}");
            let out = sweep_secs(&case, &tl, std::slice::from_ref(src), 30.0, 0.4);
            fam.add(&case, &out);
            if patched_out() {
                // (A refusal patched out to draw its frames anyway: nothing to hold the plan to.)
            } else if sname.contains("only cropped") {
                assert!(out.drawn > 0, "{case}: a layer the scaler does not resize is refused");
            } else {
                assert_eq!(out.drawn, 0, "{case}: a graded resized layer with no eq is drawn");
            }
        }
    }
    fam.finish("grade without an eq on a resized layer");
}

// ---- the blurred fill -----------------------------------------------------------------------------------------------

fn blurred(w: u32, h: u32) -> Option<Delivery> {
    Some(Delivery::new(w, h, Fit::Blur))
}

/// A frame size against a source size, over a grid built for the thumbnail's integer arithmetic: frames whose sides are not
/// multiples of 16 (the thumbnail is `2*floor(side/16)`), so the radius changes (1 below 256 high, 2 from 256), the crop offset
/// lands on a tie, and the cover is a few samples off the thumbnail; sources of odd size, thin and square.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fill_frame_and_source_grid() {
    let mut fam = Family::default();
    let frames: [(u32, u32); 9] = [
        (96, 64),
        (98, 66),
        (100, 60),
        (126, 72),
        (130, 74),
        (142, 80),
        (250, 140),
        (256, 144),
        (258, 146),
    ];
    let sources: [(u32, u32, Pat); 9] = [
        (180, 320, Pat::Noise),
        (320, 180, Pat::Edges),
        (321, 181, Pat::Noise),
        (101, 200, Pat::Test),
        (200, 101, Pat::Noise),
        (64, 360, Pat::Noise),
        (360, 64, Pat::Edges),
        (255, 255, Pat::Noise),
        (256, 256, Pat::Test),
    ];
    for (fw, fh) in frames {
        for (sw, sh, pat) in sources {
            let a = media(sw, sh, pat, Tag::None);
            let tl = timeline(vec![vec![clip(&a, 0.5, 0.8, 0.0)]], blurred(fw, fh));
            let case = format!("checker/b/fill/{fw}x{fh} from {sw}x{sh} {pat:?}");
            let out = sweep_secs(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3);
            fam.add(&case, &out);
        }
    }
    fam.finish("fill frame and source grid");
}

/// A picture a few samples off the frame's shape: the bars are one or two columns, off the chroma grid, and the fill shows
/// through only there.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fill_pictures_almost_the_frames_shape() {
    let mut fam = Family::default();
    for dw in -6i32..=6 {
        for dh in [-3i32, 0, 5] {
            let (sw, sh) = ((320 + dw) as u32, (180 + dh) as u32);
            if (sw, sh) == (320, 180) {
                continue;
            }
            let a = media(sw, sh, Pat::Noise, Tag::None);
            let tl = timeline(vec![vec![clip(&a, 0.5, 0.8, 0.0)]], blurred(320, 180));
            let case = format!("checker/b/fill/near shape {sw}x{sh} in 320x180");
            let out = sweep_secs(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3);
            fam.add(&case, &out);
        }
    }
    fam.finish("fill pictures almost the frame's shape");
}

/// The thumbnail is the smallest the arithmetic allows: the radius sits on the boundary `2r+1 == the chroma line`, with a
/// source steep enough that the cover scale is a 20:1 to 40:1 shrink.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fill_thumbnail_at_its_limits() {
    let mut fam = Family::default();
    // (frame, source): 48x48 frames have 4x4... thumbnails of 6x6 (chroma lines of 3: radius 1 fits exactly); 64x64 8x8; steep
    // sources of 640x360 and 1280x720 into 64x64 (thumbnail 8x8: a cover of 14x8 / 14x8 from 40:1 and 80:1).
    for (fw, fh, sw, sh) in [
        (48, 48, 90, 160),
        (48, 64, 160, 90),
        (64, 64, 160, 90),
        (64, 64, 320, 180),
        (64, 64, 640, 360),
        (64, 96, 640, 360),
        (80, 80, 640, 360),
        (32, 32, 90, 160),
        (16, 16, 90, 160),
        (96, 96, 1280, 720),
        (160, 32, 320, 180),
        (32, 160, 320, 180),
    ] {
        let a = media(sw, sh, Pat::Noise, Tag::None);
        let tl = timeline(vec![vec![clip(&a, 0.5, 0.8, 0.0)]], blurred(fw, fh));
        let case = format!("checker/b/fill/limits {fw}x{fh} from {sw}x{sh}");
        if let Some(out) = sweep_try(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
            fam.add(&case, &out);
        }
    }
    fam.finish("fill thumbnail at its limits");
}

/// The fill under a dissolve, a slide and a push between pictures of different shapes, and of different matrices (the fill
/// of a converted layer is the layer's own, converted with it), over another track, with speed and a reversal.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fill_in_transitions_and_stacks() {
    let mut fam = Family::default();
    let portrait = media(180, 320, Pat::Noise, Tag::None);
    let land = media(320, 180, Pat::Test, Tag::None);
    let square = media(200, 200, Pat::Edges, Tag::None);
    let p709 = media(180, 320, Pat::Noise, Tag::Bt709);
    let frame = blurred(256, 144);
    let kinds = [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::SlideUp,
        TransitionKind::PushRight,
        TransitionKind::PushDown,
    ];
    for kind in kinds {
        for (an, a, bn, b) in [
            ("portrait", &portrait, "landscape", &land),
            ("square", &square, "portrait", &portrait),
            ("portrait", &portrait, "portrait 709", &p709),
        ] {
            let tl = timeline(
                vec![vec![clip(a, 0.5, 1.2, 0.0), dissolve_into(clip(b, 0.5, 1.2, 0.5), kind, 0.3)]],
                frame,
            );
            let case = format!("checker/b/fill/{kind:?} {an} into {bn}");
            let out = sweep_secs(&case, &tl, &dedup(&[a, b]), 30.0, 1.1);
            fam.add(&case, &out);
        }
    }
    // Over and under other tracks; speed; reverse (a reversed clip is decoded another way round).
    let tl = timeline(
        vec![vec![clip(&land, 0.5, 1.2, 0.0)], vec![clip(&portrait, 0.5, 1.2, 0.0)]],
        frame,
    );
    fam.add(
        "stack",
        &sweep_secs(
            "checker/b/fill/portrait over landscape",
            &tl,
            &dedup(&[&land, &portrait]),
            30.0,
            0.7,
        ),
    );
    let tl = timeline(
        vec![
            vec![clip(&portrait, 0.5, 1.2, 0.0)],
            vec![pip(clip(&land, 0.5, 1.2, 0.0), 0.5, 0.2, 0.1)],
        ],
        frame,
    );
    fam.add(
        "stack2",
        &sweep_secs(
            "checker/b/fill/pip over portrait",
            &tl,
            &dedup(&[&land, &portrait]),
            30.0,
            0.7,
        ),
    );
    for speed in [0.5, 1.5, 4.0] {
        let mut c = clip(&portrait, 0.5, 1.5, 0.0);
        c.speed = speed;
        let tl = timeline(vec![vec![c]], frame);
        let case = format!("checker/b/fill/speed {speed}");
        fam.add(&case, &sweep_secs(&case, &tl, std::slice::from_ref(&portrait), 30.0, 0.5));
    }
    fam.finish("fill in transitions and stacks");
}

// ---- the scaler model against FFmpeg's `scale`, on inputs chosen to hit clamps and table corners -----------------------

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

/// The scalar model against FFmpeg's `scale` over ratios near 1 (both ways, each axis), tiny sources and destinations down to
/// 1 sample, the ratios around 40:1 (the plan's old limit) and enlargements to hundreds of times, on noise and on the hard patterns
/// that clip the intermediate.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_b_scaler_model_against_ffmpeg_at_the_extremes() {
    use kerf_gpu::sws::Scaler;
    assert!(
        kerf_gpu::scaler::kernels().exact(),
        "this FFmpeg's scaler is not the model: {:?}",
        kerf_gpu::scaler::kernels()
    );
    let mut sizes: Vec<((usize, usize), (usize, usize))> = Vec::new();
    // Near 1.
    for (s, d) in [
        (641, 640),
        (640, 641),
        (639, 640),
        (640, 639),
        (1000, 999),
        (999, 1000),
        (333, 331),
        (65, 64),
        (64, 65),
        (33, 32),
        (17, 16),
        (16, 17),
    ] {
        sizes.push(((s, 90), (d, 90)));
        sizes.push(((90, s), (90, d)));
        sizes.push(((s, s), (d, d)));
    }
    // Tiny and 1-sample.
    for (s, d) in [
        (1, 2),
        (1, 5),
        (2, 1),
        (2, 3),
        (2, 5),
        (3, 1),
        (3, 2),
        (3, 7),
        (4, 1),
        (4, 9),
        (5, 2),
        (5, 3),
        (6, 5),
        (7, 4),
        (8, 3),
        (9, 1),
    ] {
        sizes.push(((s, 40), (d, 40)));
        sizes.push(((40, s), (40, d)));
        sizes.push(((s, s), (d, d)));
    }
    // The 40:1 border and around it (luma and 4:2:0 chroma at each side of it).
    for (s, d) in [
        (640, 16),
        (641, 16),
        (639, 16),
        (640, 15),
        (1280, 32),
        (1000, 25),
        (1001, 25),
        (1039, 26),
        (80, 2),
        (81, 2),
        (79, 2),
        (400, 10),
        (200, 5),
        (120, 3),
        (40, 1),
        (41, 1),
    ] {
        sizes.push(((s, 120), (d, 120)));
        sizes.push(((120, s), (120, d)));
        sizes.push(((s, s.min(700)), (d, (d * s.min(700)).div_ceil(s))));
    }
    // Enlargements to hundreds of times.
    for (s, d) in [(2, 640), (3, 500), (4, 333), (5, 640), (16, 640), (31, 640), (64, 1000)] {
        sizes.push(((s, 6), (d, 12)));
        sizes.push(((6, s), (12, d)));
    }
    let mut failures = Vec::new();
    let mut n = 0;
    for (src, dst) in sizes {
        for flag in [Scaler::Bicubic, Scaler::Bilinear] {
            for fill in [
                Fill::Noise,
                Fill::Checker,
                Fill::Stripes,
                Fill::Impulse,
                Fill::White,
                Fill::Black,
            ] {
                // (Hard patterns on the large sizes would only repeat what the small ones show.)
                if !matches!(fill, Fill::Noise) && src.0 * src.1 > 200_000 {
                    continue;
                }
                n += 1;
                if let Err(e) = model_vs_ffmpeg(fill, src, dst, flag, "1") {
                    failures.push(e);
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

// ---- the Still path --------------------------------------------------------------------------------------------------

/// The still's own `yuv444p` planes against the compositor's composite put through the same conversion: the number of
/// differing samples and the largest difference (the comparator `tests/parity.rs` makes for still plans, copied here).
fn still_planes_differ(plan: &kerf_core::RenderPlan, tl: &Timeline, assets: &[Asset], t: f64) -> (usize, i32) {
    let (_, _, want, got) = still_planes(plan, tl, assets, t);
    let (mut n, mut max) = (0, 0);
    for (a, b) in got.iter().zip(&want) {
        let d = (i32::from(*a) - i32::from(*b)).abs();
        n += usize::from(d != 0);
        max = max.max(d);
    }
    (n, max)
}

/// The still's `yuv444p` planes (`want`) and the compositor's composite put through the same conversion (`got`), with the size.
fn still_planes(plan: &kerf_core::RenderPlan, tl: &Timeline, assets: &[Asset], t: f64) -> (u32, u32, Vec<u8>, Vec<u8>) {
    use std::io::Write;
    let opts = ExportOptions::default();
    let size = plan.size(u32::MAX);
    let (w, h, want) =
        kerf_core::timeline_frame_yuv_cancellable(tl, assets, &opts, t, size.0, &|| false).expect("the FFmpeg still's planes");
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
fn still_sweep(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) -> StillOutcome {
    let opts = ExportOptions::default();
    let mut out = StillOutcome::default();
    let mut log = String::new();
    for &t in times {
        let plan = kerf_core::RenderPlan::at(tl, assets, &opts, t, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let why = plan.reasons(&compositor().caps(), size);
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
        let (n, max) = still_planes_differ(&plan, tl, assets, t);
        if n > 0 {
            out.bad.push((t, n, max));
            let _ = writeln!(log, "{t}\tINEXACT\t{n} samples differ, max {max}");
        } else {
            let _ = writeln!(log, "{t}\texact");
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

/// A Still plan of two or three layers on screen: every ordered pair of tags, full frame and picture-in-picture, and a dissolve.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_still_matrix_pairs() {
    let mut fam = Family::default();
    for a in TAGS {
        for b in TAGS {
            if a == b {
                continue;
            }
            let bottom = media(320, 180, Pat::Edges, a);
            let top = media(320, 180, Pat::Noise, b);
            let assets = dedup(&[&bottom, &top]);
            for (label, scale) in [("full", 1.0), ("pip", 0.5)] {
                let tl = timeline(
                    vec![
                        vec![clip(&bottom, 0.0, 2.0, 0.0)],
                        vec![pip(clip(&top, 0.0, 2.0, 0.0), scale, 0.1, 0.1)],
                    ],
                    None,
                );
                let case = format!("checker/b/still/{label} {b:?} over {a:?}");
                fam.add_still(&case, &still_sweep(&case, &tl, &assets, &[0.5]));
            }
            // A dissolve between the two clips, in the middle of it.
            let tl = timeline(
                vec![vec![
                    clip(&bottom, 0.0, 2.0, 0.0),
                    dissolve_into(clip(&top, 0.0, 2.0, 1.5), TransitionKind::Crossfade, 0.5),
                ]],
                None,
            );
            let case = format!("checker/b/still/dissolve {a:?} into {b:?}");
            fam.add_still(&case, &still_sweep(&case, &tl, &assets, &[0.2, 1.75, 2.2]));
        }
    }
    fam.finish("still matrix pairs");
}

// ---- the shader's scaler in the export, at the plan's limit ------------------------------------------------------------

/// A source shrunk into deliveries around 40:1, once the limit of the plan's exact scaler (now 63:1), and into a picture-in-picture whose zoom
/// puts the picture around it: a drawn frame is exact; a refused one is counted.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_scaler_shader_around_the_shrink_limit() {
    let mut fam = Family::default();
    let wide = media(640, 360, Pat::Noise, Tag::None);
    let edges = media(640, 360, Pat::Edges, Tag::None);
    let hd = media(1280, 720, Pat::Noise, Tag::None);
    let mut drawn_at = Vec::new();
    for (src, w, h) in [
        (&hd, 32, 18),
        (&hd, 34, 18),
        (&hd, 30, 16),
        (&hd, 36, 20),
        (&hd, 64, 36),
        (&wide, 16, 10),
        (&wide, 16, 12),
        (&wide, 18, 10),
        (&wide, 20, 12),
        (&wide, 32, 18),
        (&edges, 16, 10),
        (&edges, 24, 14),
    ] {
        let tl = timeline(vec![vec![clip(src, 0.5, 0.8, 0.0)]], Some(Delivery::new(w, h, Fit::Contain)));
        let case = format!(
            "checker/b/scaler/{}x{} into {w}x{h}",
            src.streams[0].width.unwrap(),
            src.streams[0].height.unwrap()
        );
        let out = sweep_secs(&case, &tl, std::slice::from_ref(src), 30.0, 0.3);
        if out.drawn > 0 {
            drawn_at.push(case.clone());
        }
        fam.add(&case, &out);
    }
    for s in [0.0125, 0.02, 0.0249, 0.025, 0.0251, 0.026, 0.03, 0.045, 0.05, 0.1, 0.33, 0.9] {
        for src in [&wide, &edges] {
            let c = pip(clip(src, 0.5, 0.8, 0.0), s, 0.1, -0.2);
            let tl = timeline(vec![vec![c]], None);
            let case = format!(
                "checker/b/scaler/pip scale {s} of {}",
                if std::ptr::eq(src, &wide) { "noise" } else { "edges" }
            );
            let out = sweep_secs(&case, &tl, std::slice::from_ref(src), 30.0, 0.3);
            fam.add(&case, &out);
        }
    }
    eprintln!("drawn deliveries: {drawn_at:?}");
    fam.finish("scaler shader around the shrink limit");
}

/// Ratios near 1 in the export: the fit scales a picture a sample or two off the delivery in either direction, with
/// odd sides (FFV1) and even, each way round per axis.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_scaler_shader_near_unity() {
    let mut fam = Family::default();
    for (sw, sh) in [
        (322, 182),
        (321, 181),
        (319, 179),
        (318, 178),
        (320, 182),
        (322, 180),
        (320, 178),
        (318, 180),
        (324, 184),
        (316, 176),
        (330, 190),
        (310, 170),
        (641, 361),
        (639, 359),
    ] {
        for pat in [Pat::Noise, Pat::Edges] {
            let a = media(sw, sh, pat, Tag::None);
            for fit in [Fit::Contain, Fit::Cover] {
                let tl = timeline(vec![vec![clip(&a, 0.5, 0.8, 0.0)]], Some(Delivery::new(320, 180, fit)));
                let case = format!("checker/b/scaler/near unity {sw}x{sh} {pat:?} {fit:?}");
                let out = sweep_secs(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3);
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("scaler shader near unity");
}

/// Tiny sources (2x2 to 16x16, several sides odd) enlarged into the frame, and 1-sample-wide ones.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_scaler_shader_tiny_sources() {
    let mut fam = Family::default();
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
        let a = media(sw, sh, Pat::Noise, Tag::None);
        for delivery in [
            Delivery::new(64, 36, Fit::Contain),
            Delivery::new(64, 36, Fit::Cover),
            Delivery::new(64, 36, Fit::Blur),
        ] {
            let tl = timeline(vec![vec![clip(&a, 0.5, 0.8, 0.0)]], Some(delivery));
            let case = format!("checker/b/scaler/tiny {sw}x{sh} {:?}", delivery.fit);
            if let Some(out) = sweep_try(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("scaler shader tiny sources");
}

// ---- a seeded fuzz over cuts of mixed matrices, fits, transitions and layers ----------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }
}

/// What the fuzz may put in a cut. The pre-existing ladders (a translucent or turned layer, a blur or sharpen) are left out
/// unless asked for: they are inexact by design in the base and not this lane's.
#[derive(Clone, Copy)]
struct FuzzMix {
    translucent_and_rotated: bool,
    eq_and_grade: bool,
    keyed: bool,
    alpha_sources: bool,
}

fn fuzz_cut(seed: u64, mix: FuzzMix) -> (String, Timeline, Vec<Asset>, f64) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xd1b5_4a32);
    let mut desc = String::new();
    let mut assets: Vec<Asset> = Vec::new();
    let use_asset = |a: Asset, assets: &mut Vec<Asset>| {
        if !assets.iter().any(|b| b.id == a.id) {
            assets.push(a.clone());
        }
        a
    };
    let pick_asset = |rng: &mut Rng, assets: &mut Vec<Asset>, desc: &mut String| -> Asset {
        let shape = rng.pick(&[(320, 180), (320, 180), (320, 180), (180, 320), (240, 240), (160, 90)]);
        let tag = rng.pick(&TAGS);
        let pat = rng.pick(&[Pat::Noise, Pat::Test, Pat::Edges, Pat::Bars]);
        let a = if mix.alpha_sources && rng.chance(12) {
            let _ = write!(desc, "[alpha {}x{} {tag:?}]", shape.0, shape.1);
            alpha_media(shape.0, shape.1, tag)
        } else {
            let _ = write!(desc, "[{}x{} {pat:?} {tag:?}]", shape.0, shape.1);
            media(shape.0, shape.1, pat, tag)
        };
        use_asset(a, assets)
    };
    let delivery = match rng.below(10) {
        0..=3 => None,
        4..=6 => Some(Delivery::new(320, 180, Fit::Contain)),
        7 | 8 => Some(Delivery::new(320, 180, Fit::Blur)),
        _ => Some(Delivery::new(180, 320, Fit::Cover)),
    };
    let _ = write!(desc, "delivery {:?}; ", delivery.map(|d| (d.width, d.height, d.fit)));
    let mut tracks: Vec<Vec<Clip>> = Vec::new();
    let mut end: f64 = 0.0;
    let mut v1 = Vec::new();
    let n = 2 + rng.below(2) as usize;
    let mut t = 0.0;
    for i in 0..n {
        let a = pick_asset(&mut rng, &mut assets, &mut desc);
        let len = 0.6 + 0.1 * rng.below(3) as f64;
        let mut c = clip(&a, 0.5, 0.5 + len, t);
        if i > 0 && rng.chance(45) {
            let kind = rng.pick(&[
                TransitionKind::Crossfade,
                TransitionKind::Crossfade,
                TransitionKind::DipToBlack,
                TransitionKind::SlideLeft,
                TransitionKind::PushUp,
                TransitionKind::SlideDown,
            ]);
            c = dissolve_into(c, kind, 0.3);
            let _ = write!(desc, "<{kind:?}>");
        }
        if rng.chance(8) {
            c.speed = 1.5;
            c.source_out = c.source_in + len * 1.5;
            desc.push_str("<speed 1.5>");
        }
        if mix.eq_and_grade && rng.chance(20) {
            c.color = Color {
                contrast: 1.1,
                saturation: 1.2,
                ..Color::default()
            };
            desc.push_str("<eq>");
            if rng.chance(50) {
                c.grade = grades_b()[rng.below(3) as usize].1.clone();
                desc.push_str("<grade>");
            }
        }
        if rng.chance(10) {
            c.fade_in = 0.2;
            desc.push_str("<fade_in>");
        }
        if mix.keyed && rng.chance(12) {
            c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(len, 0.8, 0.05, 0.0)];
            desc.push_str("<keyed>");
        }
        if rng.chance(15) {
            c = pip(c, 0.5, 0.15, -0.1);
            desc.push_str("<pip>");
        }
        t += len;
        v1.push(c);
    }
    end = end.max(t);
    tracks.push(v1);
    if rng.chance(40) {
        let a = pick_asset(&mut rng, &mut assets, &mut desc);
        let start = 0.1 * rng.below(6) as f64;
        let mut c = clip(&a, 0.5, 1.1, start);
        let scale = rng.pick(&[0.5, 0.75, 1.0]);
        c = pip(c, scale, 0.1, 0.1);
        desc.push_str(&format!("<V2 scale {scale}>"));
        if mix.translucent_and_rotated {
            if rng.chance(30) {
                c.transform.opacity = 0.7;
                desc.push_str("<opacity>");
            }
            if rng.chance(20) {
                c.transform.rotation = 8.0;
                desc.push_str("<rotation>");
            }
        }
        if mix.keyed && rng.chance(15) {
            c.keyframes = vec![zoom_key(0.0, scale, 0.0, 0.0), zoom_key(0.6, scale * 0.8, 0.1, 0.0)];
            desc.push_str("<keyed>");
        }
        tracks.push(vec![c]);
        end = end.max(start + 0.6);
    }
    let tl = timeline(tracks, delivery);
    (desc, tl, assets, end)
}

fn run_fuzz(label: &str, seeds: std::ops::Range<u64>, mix: FuzzMix) {
    // A translucent or turned layer has its own ladder in the base: that fuzz reports its figures and does not fail.
    let ladder = mix.translucent_and_rotated;
    let mut fam = Family::default();
    let mut drawn_total = 0;
    for seed in seeds {
        let (desc, tl, assets, secs) = fuzz_cut(seed, mix);
        let case = format!("checker/b/fuzz/{label}/{seed:03}");
        eprintln!("-- {case}: {desc}");
        let Some(out) = sweep_try(&case, &tl, &assets, 15.0, secs) else {
            continue;
        };
        drawn_total += out.drawn;
        if ladder {
            fam.add_known_ladder(&case, &out);
            continue;
        }
        let before = fam.failures.len();
        fam.add(&case, &out);
        if fam.failures.len() > before {
            let last = fam.failures.last_mut().unwrap();
            *last = format!("{last}\n    cut: {desc}");
        }
    }
    eprintln!("== fuzz {label}: {drawn_total} frames drawn");
    fam.finish(&format!("fuzz {label}"));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fuzz_matrices_fits_and_transitions() {
    run_fuzz(
        "plain",
        0..40,
        FuzzMix {
            translucent_and_rotated: false,
            eq_and_grade: false,
            keyed: false,
            alpha_sources: false,
        },
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fuzz_with_alpha_sources_and_keyed_placement() {
    run_fuzz(
        "alpha-keyed",
        100..140,
        FuzzMix {
            translucent_and_rotated: false,
            eq_and_grade: false,
            keyed: true,
            alpha_sources: true,
        },
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fuzz_with_eq_and_grade() {
    run_fuzz(
        "grade",
        200..230,
        FuzzMix {
            translucent_and_rotated: false,
            eq_and_grade: true,
            keyed: false,
            alpha_sources: false,
        },
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fuzz_with_translucent_and_rotated_layers() {
    run_fuzz(
        "translucent",
        300..330,
        FuzzMix {
            translucent_and_rotated: true,
            eq_and_grade: false,
            keyed: false,
            alpha_sources: false,
        },
    );
}

// ---- the deciding clip, by storage order and by track ---------------------------------------------------------------------

/// Where the clip that decides the composite's matrix is stored: the clips of a track stored out of timeline order, the first
/// clip of the graph on the upper track, a gap before the first clip, the same file used twice.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_storage_order_tracks_and_gaps() {
    let mut fam = Family::default();
    let a601 = media(320, 180, Pat::Noise, Tag::None);
    let b709 = media(320, 180, Pat::Edges, Tag::Bt709);
    let c2020 = media(320, 180, Pat::Test, Tag::Bt2020);
    type CutCase<'a> = (&'a str, Vec<Vec<Clip>>, Vec<Asset>);
    let cases: Vec<CutCase> = vec![
        (
            "stored later-first: 709 at 0.7 stored before 601 at 0",
            vec![vec![clip(&b709, 0.5, 1.2, 0.7), clip(&a601, 0.5, 1.2, 0.0)]],
            dedup(&[&a601, &b709]),
        ),
        (
            "stored later-first with a dissolve on the earlier one",
            vec![vec![
                clip(&b709, 0.5, 1.2, 0.7),
                dissolve_into(clip(&a601, 0.5, 1.2, 0.0), TransitionKind::Crossfade, 0.3),
            ]],
            dedup(&[&a601, &b709]),
        ),
        (
            "a gap before the first clip",
            vec![vec![clip(&a601, 0.5, 1.0, 0.4), clip(&b709, 0.5, 1.0, 0.9)]],
            dedup(&[&a601, &b709]),
        ),
        (
            "a gap between, 709 first",
            vec![vec![clip(&b709, 0.5, 1.0, 0.0), clip(&a601, 0.5, 1.0, 0.8)]],
            dedup(&[&a601, &b709]),
        ),
        (
            "a dissolve after a gap (against black)",
            vec![vec![
                clip(&b709, 0.5, 1.0, 0.0),
                dissolve_into(clip(&a601, 0.5, 1.0, 0.8), TransitionKind::Crossfade, 0.3),
            ]],
            dedup(&[&a601, &b709]),
        ),
        (
            "upper track starts first, 709 under 601",
            vec![vec![clip(&b709, 0.5, 1.2, 0.5)], vec![clip(&a601, 0.5, 1.2, 0.0)]],
            dedup(&[&a601, &b709]),
        ),
        (
            "three tracks 601, 709, 2020",
            vec![
                vec![clip(&a601, 0.5, 1.2, 0.0)],
                vec![pip(clip(&b709, 0.5, 1.2, 0.0), 0.6, -0.1, 0.0)],
                vec![pip(clip(&c2020, 0.5, 1.2, 0.2), 0.3, 0.2, 0.2)],
            ],
            dedup(&[&a601, &b709, &c2020]),
        ),
        (
            "the same 709 file twice around a 601 clip",
            vec![vec![
                clip(&b709, 0.5, 1.0, 0.0),
                clip(&a601, 0.5, 1.0, 0.5),
                clip(&b709, 1.0, 1.5, 1.0),
            ]],
            dedup(&[&a601, &b709]),
        ),
        (
            "dissolve out of 709 into 601 then 2020 pip over it",
            vec![
                vec![
                    clip(&b709, 0.5, 1.2, 0.0),
                    dissolve_into(clip(&a601, 0.5, 1.2, 0.7), TransitionKind::Crossfade, 0.4),
                ],
                vec![pip(clip(&c2020, 0.5, 1.0, 0.3), 0.4, 0.2, 0.2)],
            ],
            dedup(&[&a601, &b709, &c2020]),
        ),
    ];
    for (name, tracks, assets) in cases {
        let tl = timeline(tracks, None);
        let case = format!("checker/b/matrix/order {name}");
        let out = sweep_secs(&case, &tl, &assets, 30.0, 1.9);
        fam.add(&case, &out);
    }
    fam.finish("matrix storage order, tracks and gaps");
}

// ---- what is kept from the conversion, case by case (each refusal's necessity is judged by drawing the frames anyway) --------

/// A 709 clip, then a BT.601 one carrying one thing the conversion does not cover: every drawn frame is exact and the plan's
/// refusals are counted. (The same cases run with the refusal patched out say whether it was needed.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_what_the_conversion_does_not_cover() {
    let mut fam = Family::default();
    let first = media(320, 180, Pat::Edges, Tag::Bt709);
    let plain = media(320, 180, Pat::Noise, Tag::None);
    let wide = media(960, 540, Pat::Noise, Tag::None);
    let phone601 = media(180, 320, Pat::Test, Tag::None);
    let phone709 = media(180, 320, Pat::Noise, Tag::Bt709);
    let odd = media(321, 181, Pat::Noise, Tag::None);
    let sliver = media(179, 320, Pat::Noise, Tag::None);
    let y444 = {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("y444.mkv");
        if !p.exists() {
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30:duration=4",
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv444p",
                p.to_str().unwrap(),
            ]);
        }
        probed(&p)
    };
    let tenbit = {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
        let p = dir.join("tenbit.mp4");
        if !p.exists() {
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30:duration=4",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-pix_fmt",
                "yuv420p10le",
                p.to_str().unwrap(),
            ]);
        }
        probed(&p)
    };
    let fullrange = {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
        let p = dir.join("pcflag.mp4");
        if !p.exists() {
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30:duration=4",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-color_range",
                "pc",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
        }
        probed(&p)
    };
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    let curves = grades_b().into_iter().find(|(n, _)| *n == "curves").unwrap().1;
    // (name, the converted clip's asset, the edit, the delivery)
    let cases: Vec<(&str, &Asset, Edit, Option<Delivery>)> = vec![
        ("grade", &plain, Box::new(|c| c.grade = curves.clone()), None),
        (
            "effect invert",
            &plain,
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Invert]),
            None,
        ),
        (
            "effect grayscale",
            &plain,
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Grayscale]),
            None,
        ),
        (
            "mask",
            &plain,
            Box::new(|c| {
                c.mask = Some(kerf_core::Mask {
                    shape: kerf_core::MaskShape::Rect,
                    x: 0.5,
                    y: 0.5,
                    width: 0.6,
                    height: 0.6,
                    feather: 0.0,
                    inverted: false,
                });
            }),
            None,
        ),
        ("opacity 0.6", &plain, Box::new(|c| c.transform.opacity = 0.6), None),
        ("rotation 10", &plain, Box::new(|c| c.transform.rotation = 10.0), None),
        (
            "chroma key",
            &plain,
            Box::new(|c| {
                c.effects = vec![kerf_core::VideoEffect::ChromaKey {
                    color: "0x00ff00".into(),
                    similarity: 0.3,
                    blend: 0.1,
                }];
            }),
            None,
        ),
        (
            "eq",
            &plain,
            Box::new(|c| {
                c.color = Color {
                    contrast: 1.2,
                    saturation: 1.1,
                    ..Color::default()
                }
            }),
            None,
        ),
        (
            "keyed scale",
            &plain,
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 0.8, 0.0, 0.0)]),
            None,
        ),
        ("clip fade-in running", &plain, Box::new(|c| c.fade_in = 0.4), None),
        (
            "a dip to black into it",
            &plain,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::DipToBlack, 0.4)),
            None,
        ),
        (
            "a dip to white into it",
            &plain,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::DipToWhite, 0.4)),
            None,
        ),
        (
            "a slide into it",
            &plain,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::SlideLeft, 0.4)),
            None,
        ),
        (
            "a push into it",
            &plain,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::PushUp, 0.4)),
            None,
        ),
        (
            "a dissolve into it",
            &plain,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::Crossfade, 0.4)),
            None,
        ),
        (
            "a dissolve into a pip",
            &plain,
            Box::new(|c| *c = pip(dissolve_into(c.clone(), TransitionKind::Crossfade, 0.4), 0.6, 0.1, 0.1)),
            None,
        ),
        ("4:4:4", &y444, Box::new(|_| {}), None),
        ("10-bit", &tenbit, Box::new(|_| {}), None),
        ("flagged full range", &fullrange, Box::new(|_| {}), None),
        (
            "odd source 321x181",
            &odd,
            Box::new(|_| {}),
            Some(Delivery::new(320, 180, Fit::Contain)),
        ),
        (
            "odd fitted picture 179x320 into 320x180",
            &sliver,
            Box::new(|_| {}),
            Some(Delivery::new(320, 180, Fit::Contain)),
        ),
        ("pip 0.5", &plain, Box::new(|c| *c = pip(c.clone(), 0.5, 0.1, 0.1)), None),
        (
            "speed 2",
            &plain,
            Box::new(|c| {
                c.speed = 2.0;
                c.source_out = c.source_in + 2.0;
            }),
            None,
        ),
        (
            "phone 601 letterboxed",
            &phone601,
            Box::new(|_| {}),
            Some(Delivery::new(320, 180, Fit::Contain)),
        ),
        (
            "phone 601 over a blurred fill",
            &phone601,
            Box::new(|_| {}),
            Some(Delivery::new(320, 180, Fit::Blur)),
        ),
        ("960x540 shrunk into 320x180", &wide, Box::new(|_| {}), None),
    ];
    for (name, asset, edit, delivery) in cases {
        // The base clip is of the same size as the frame, or the portrait 709 for the letterboxed cases.
        let base =
            if delivery.is_some_and(|d| d.height == 180) && asset.streams[0].width.unwrap() < asset.streams[0].height.unwrap() {
                &phone709
            } else {
                &first
            };
        let mut c = clip(asset, 0.0, 0.8, 0.7);
        edit(&mut c);
        let tl = timeline(vec![vec![clip(base, 0.5, 1.2, 0.0), c]], delivery);
        let case = format!("checker/b/matrix/uncovered {name}");
        if let Some(out) = sweep_try(&case, &tl, &dedup(&[base, asset]), 30.0, 1.5) {
            // (A translucent or turned layer, and a dip to white, have their own ladders in the base: reported, not failed.)
            if ["opacity 0.6", "rotation 10", "a dip to white into it"].contains(&name) {
                fam.add_known_ladder(&case, &out);
            } else {
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("matrix what the conversion does not cover");
}

// ---- phone clips, as a vlog cuts them -------------------------------------------------------------------------------------

/// Portrait phone clips of both matrices in a landscape frame, fitted with bars or over a blurred fill, dissolving, plain, at
/// 60 fps and with a speed change: the T5 archetype at a size a test can afford.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_phone_vlog() {
    let mut fam = Family::default();
    let p709 = media(180, 320, Pat::Test, Tag::Bt709);
    let p601 = media(180, 320, Pat::Noise, Tag::Smpte170m);
    let pn = media(180, 320, Pat::Edges, Tag::None);
    for (fit, label) in [(Fit::Contain, "bars"), (Fit::Blur, "blur")] {
        for fps in [60.0, 30.0] {
            for (name, plan) in [
                ("709 601 709 all dissolving", [(&p709, false), (&p601, true), (&p709, true)]),
                ("709 601 709 plain", [(&p709, false), (&p601, false), (&p709, false)]),
                (
                    "601 709 untagged dissolving second",
                    [(&p601, false), (&p709, true), (&pn, false)],
                ),
                (
                    "709 untagged dissolving 601 plain",
                    [(&p709, false), (&pn, true), (&p601, false)],
                ),
            ] {
                let mut track = Vec::new();
                for (i, (a, dissolving)) in plan.iter().enumerate() {
                    let c = clip(a, 0.5, 1.1, 0.6 * i as f64);
                    track.push(if *dissolving {
                        dissolve_into(c, TransitionKind::Crossfade, 0.3)
                    } else {
                        c
                    });
                }
                let tl = timeline(vec![track], Some(Delivery::new(320, 180, fit)));
                let case = format!("checker/b/matrix/phone {label} {fps}fps {name}");
                let assets = dedup(&[&p709, &p601, &pn]);
                let out = sweep_secs(&case, &tl, &assets, fps, 1.7);
                fam.add(&case, &out);
            }
        }
    }
    // A speed change and a reverse-like fast clip among them.
    let mut fast = clip(&p601, 0.5, 2.0, 0.6);
    fast.speed = 2.0;
    let tl = timeline(
        vec![vec![clip(&p709, 0.5, 1.1, 0.0), fast]],
        Some(Delivery::new(320, 180, Fit::Contain)),
    );
    let case = "checker/b/matrix/phone speed 2";
    fam.add(case, &sweep_secs(case, &tl, &dedup(&[&p709, &p601]), 30.0, 1.2));
    fam.finish("matrix phone vlog");
}

// ---- the fill on pictures it does not draw -----------------------------------------------------------------------------------

/// A blurred fill behind pictures that are not 8-bit limited-range `yuv420p` (4:4:4, 4:2:2, gray, 10-bit, a full-range flag)
/// and sources shrunk steeply into the thumbnail (past 63:1 the plan refuses the fill; every frame it does draw is exact).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_fill_on_pictures_it_refuses() {
    let mut fam = Family::default();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str, pix: &str, extra: &[&str], codec: &[&str]| -> Asset {
        let p = dir.join(name);
        if !p.exists() {
            let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=180x320:rate=30:duration=4"];
            args.extend_from_slice(codec);
            args.extend_from_slice(extra);
            args.extend_from_slice(&["-pix_fmt", pix, p.to_str().unwrap()]);
            ffmpeg(&args);
        }
        probed(&p)
    };
    let x264 = ["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
    let ffv1 = ["-c:v", "ffv1"];
    let list = [
        ("yuv444p", make("fill-444.mkv", "yuv444p", &[], &ffv1)),
        ("yuv422p", make("fill-422.mkv", "yuv422p", &[], &ffv1)),
        ("gray", make("fill-gray.mkv", "gray", &[], &ffv1)),
        ("10-bit", make("fill-10.mp4", "yuv420p10le", &[], &x264)),
        ("pc flagged", make("fill-pc.mp4", "yuv420p", &["-color_range", "pc"], &x264)),
    ];
    for (name, a) in &list {
        let tl = timeline(vec![vec![clip(a, 0.5, 0.8, 0.0)]], blurred(256, 144));
        let case = format!("checker/b/fill/refused format {name}");
        if let Some(out) = sweep_try(&case, &tl, std::slice::from_ref(a), 30.0, 0.3) {
            fam.add(&case, &out);
        }
    }
    // Steep into the thumbnail (45:1 is drawn since the limit went to 63:1, 90:1 is refused).
    for (fw, fh, sw, sh) in [
        (64, 64, 640, 360),
        (96, 96, 1280, 720),
        (64, 64, 1280, 720),
        (80, 48, 800, 450),
    ] {
        let a = media(sw, sh, Pat::Noise, Tag::None);
        let tl = timeline(vec![vec![clip(&a, 0.5, 0.8, 0.0)]], blurred(fw, fh));
        let case = format!("checker/b/fill/refused shrink {sw}x{sh} into {fw}x{fh}");
        if let Some(out) = sweep_try(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
            fam.add(&case, &out);
        }
    }
    fam.finish_refusing("fill on pictures it refuses");
}

// ---- the real-world sizes of the ledger's archetypes -------------------------------------------------------------------------

/// T8 of the ledger at its size: 4K footage in a 1080p Cover canvas, graded (an `eq` with warmth, a master curve, wheels on
/// some): the layer is resized 2:1 and the grade follows an `eq`. A few frames; the export of 4K is slow.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); slow: 4K"]
fn checker_b_ledger_t8_4k_graded_in_a_1080p_canvas() {
    use kerf_core::{CurvePoint, Curves, Grade, Wheel, Wheels};
    let mut fam = Family::default();
    for (w, h, pat) in [
        (3840, 2160, Pat::Test),
        (3840, 2160, Pat::Bars),
        (2560, 1440, Pat::Test),
        (1920, 1080, Pat::Test),
    ] {
        let a = media(w, h, pat, Tag::None);
        for wheels in [false, true] {
            let mut c = clip(&a, 0.5, 0.8, 0.0);
            c.color = Color {
                contrast: 1.08,
                saturation: 1.15,
                temperature: 0.1,
                ..Color::default()
            };
            c.grade = Grade {
                curves: Some(Curves {
                    master: [(0.0, 0.0), (0.25, 0.2), (0.75, 0.8), (1.0, 1.0)]
                        .map(|(x, y)| CurvePoint { x, y })
                        .to_vec(),
                    ..Curves::default()
                }),
                wheels: wheels.then(|| Wheels {
                    shadows: Wheel {
                        x: 0.1,
                        y: -0.05,
                        lightness: 0.1,
                    },
                    ..Wheels::default()
                }),
                ..Grade::default()
            };
            let tl = timeline(vec![vec![c]], Some(Delivery::new(1920, 1080, Fit::Cover)));
            let case = format!("checker/b/t8/{w}x{h} {pat:?} wheels {wheels}");
            let out = sweep(&case, &tl, std::slice::from_ref(&a), &at_fps(30.0), 4);
            fam.add(&case, &out);
        }
    }
    fam.finish("ledger T8 at its size");
}

/// T5 of the ledger at half its size: phone clips of both matrices in a landscape frame at 60 fps, over bars and over a
/// blurred fill, dissolving.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); slow"]
fn checker_b_ledger_t5_phone_vlog_at_960x540() {
    let mut fam = Family::default();
    let p709 = media(540, 960, Pat::Test, Tag::Bt709);
    let p601 = media(540, 960, Pat::Test, Tag::Smpte170m);
    for fit in [Fit::Contain, Fit::Blur] {
        let mut track = Vec::new();
        for i in 0..4 {
            let src = if i % 3 == 1 { &p601 } else { &p709 };
            let mut c = clip(src, 0.5 + 0.1 * i as f64, 1.0 + 0.1 * i as f64, 0.5 * i as f64);
            if i > 0 {
                c = dissolve_into(c, TransitionKind::Crossfade, 0.25);
            }
            track.push(c);
        }
        let tl = timeline(vec![track], Some(Delivery::new(960, 540, fit)));
        let case = format!("checker/b/t5/{fit:?} 60fps");
        let out = sweep_secs(&case, &tl, &dedup(&[&p709, &p601]), 60.0, 1.9);
        fam.add(&case, &out);
    }
    fam.finish("ledger T5 at half size");
}

// ---- chains of scales: fit, zoom, crop, position, in every fit ---------------------------------------------------------------

/// A static zoom (below and above 1), a position and an edge crop on pictures of both shapes in every fit, at deliveries of
/// the frame's shape and of another: the chains the plan names `FitOrder::Folded`, `Fitted` and `Uncropped`, each a different
/// cascade of scalers and crops. Seeded: 80 of the combinations.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_scaler_chains_of_fit_zoom_crop_and_position() {
    let mut fam = Family::default();
    let mut rng = Rng(0x05ee_db0b);
    for n in 0..80 {
        let (sw, sh) = rng.pick(&[(320, 180), (180, 320), (240, 240), (322, 182), (400, 300)]);
        let pat = rng.pick(&[Pat::Noise, Pat::Edges, Pat::Test]);
        let a = media(sw, sh, pat, Tag::None);
        let (dw, dh) = rng.pick(&[(320, 180), (256, 144), (180, 320), (200, 200), (160, 90)]);
        let fit = rng.pick(&[Fit::Contain, Fit::Cover, Fit::Blur]);
        let scale = rng.pick(&[0.5, 0.6, 0.8, 1.0, 1.25, 1.5, 2.0]);
        let (px, py) = rng.pick(&[(0.0, 0.0), (0.2, 0.1), (-0.25, 0.0), (0.05, -0.3)]);
        let mut c = clip(&a, 0.5, 0.8, 0.0);
        c.transform.scale = scale;
        c.transform.pos_x = px;
        c.transform.pos_y = py;
        let crop = rng.chance(35);
        if crop {
            c.transform.crop_left = 0.1;
            c.transform.crop_bottom = 0.15;
        }
        let tl = timeline(vec![vec![c]], Some(Delivery::new(dw, dh, fit)));
        let case =
            format!("checker/b/chain/{n:02} {sw}x{sh} {pat:?} into {dw}x{dh} {fit:?} scale {scale} at ({px},{py}) crop {crop}");
        let out = sweep_secs(&case, &tl, std::slice::from_ref(&a), 30.0, 0.2);
        fam.add(&case, &out);
    }
    fam.finish("scaler chains of fit, zoom, crop and position");
}

/// The blurred fill in a Still plan: frames and sources of the grid, a dissolve between pictures of two shapes, and a
/// picture-in-picture over a filled one.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_still_blurred_fill() {
    let mut fam = Family::default();
    for (fw, fh, sw, sh, pat) in [
        (96, 64, 180, 320, Pat::Noise),
        (98, 66, 321, 181, Pat::Noise),
        (130, 74, 200, 101, Pat::Edges),
        (256, 144, 180, 320, Pat::Test),
        (256, 144, 255, 255, Pat::Noise),
        (258, 146, 64, 360, Pat::Noise),
        (250, 140, 360, 64, Pat::Edges),
        (320, 180, 180, 320, Pat::Noise),
    ] {
        let a = media(sw, sh, pat, Tag::None);
        let tl = timeline(vec![vec![clip(&a, 0.0, 2.0, 0.0)]], blurred(fw, fh));
        let case = format!("checker/b/still/fill {fw}x{fh} from {sw}x{sh} {pat:?}");
        fam.add_still(&case, &still_sweep(&case, &tl, std::slice::from_ref(&a), &[0.5]));
    }
    let portrait = media(180, 320, Pat::Noise, Tag::None);
    let land = media(320, 180, Pat::Test, Tag::None);
    let tl = timeline(
        vec![vec![
            clip(&portrait, 0.0, 2.0, 0.0),
            dissolve_into(clip(&land, 0.0, 2.0, 1.5), TransitionKind::Crossfade, 0.5),
        ]],
        blurred(256, 144),
    );
    let case = "checker/b/still/fill dissolve portrait into landscape";
    fam.add_still(case, &still_sweep(case, &tl, &dedup(&[&portrait, &land]), &[0.5, 1.75, 2.3]));
    let tl = timeline(
        vec![
            vec![clip(&portrait, 0.0, 2.0, 0.0)],
            vec![pip(clip(&land, 0.0, 2.0, 0.0), 0.5, 0.2, 0.1)],
        ],
        blurred(256, 144),
    );
    let case = "checker/b/still/fill pip over portrait";
    fam.add_still(case, &still_sweep(case, &tl, &dedup(&[&portrait, &land]), &[0.5]));
    fam.finish("still blurred fill");
}

/// A grade after an `eq` on a resized layer, in a Still plan, and without the `eq`.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_still_grade_on_a_resized_layer() {
    let mut fam = Family::default();
    let noise = media(320, 180, Pat::Noise, Tag::None);
    let edges = media(320, 180, Pat::Edges, Tag::Bt709);
    for (gname, grade) in grades_b() {
        for (src, delivery, scale, label) in [
            (&noise, None, 0.6, "pip"),
            (&edges, None, 0.75, "709 pip"),
            (&noise, Some(Delivery::new(160, 90, Fit::Contain)), 1.0, "shrunk"),
            (&edges, Some(Delivery::new(640, 360, Fit::Contain)), 1.0, "enlarged"),
        ] {
            for with_eq in [true, false] {
                let mut c = clip(src, 0.0, 2.0, 0.0);
                c.transform.scale = scale;
                c.grade = grade.clone();
                if with_eq {
                    c.color = Color {
                        contrast: 1.1,
                        saturation: 1.2,
                        ..Color::default()
                    };
                }
                let tl = timeline(vec![vec![c]], delivery);
                let case = format!("checker/b/still/grade {label} {gname} eq {with_eq}");
                let out = still_sweep(&case, &tl, std::slice::from_ref(src), &[0.5]);
                if !with_eq && !patched_out() {
                    assert_eq!(out.drawn, 0, "{case}: drawn with no eq");
                }
                fam.add_still(&case, &out);
            }
        }
    }
    fam.finish("still grade on a resized layer");
}

/// A delivery that names its scaler: `bicubic` is the plan's own (the compositor "implements swscale's default"), any other is
/// refused. On a build whose unflagged `scale` is not bicubic (FFmpeg before 5.0) an explicit `bicubic` is another scale than
/// the default the compositor draws with.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_scaler_named_by_the_delivery() {
    let mut fam = Family::default();
    let noise = media(320, 180, Pat::Noise, Tag::None);
    for scaler in ["bicubic", "bilinear", "lanczos", "neighbor"] {
        let c = pip(clip(&noise, 0.5, 0.8, 0.0), 0.6, 0.0, 0.0);
        let tl = timeline(vec![vec![c]], None);
        let opts = ExportOptions {
            scaler: Some(scaler.to_string()),
            ..at_fps(30.0)
        };
        let case = format!("checker/b/scaler/delivery scaler {scaler}");
        let out = sweep(&case, &tl, std::slice::from_ref(&noise), &opts, 9);
        if !scaler.eq_ignore_ascii_case("bicubic") && !patched_out() {
            assert_eq!(out.drawn, 0, "{case}: a scaler other than bicubic is drawn");
        }
        fam.add(&case, &out);
        // And for a fit: the picture enlarged into a bigger frame.
        let tl = timeline(
            vec![vec![clip(&noise, 0.5, 0.8, 0.0)]],
            Some(Delivery::new(640, 360, Fit::Contain)),
        );
        let case = format!("checker/b/scaler/delivery scaler {scaler} enlarged");
        let out = sweep(&case, &tl, std::slice::from_ref(&noise), &opts, 9);
        fam.add(&case, &out);
    }
    fam.finish("scaler named by the delivery");
}

/// An `eq` on a resized layer with no grade after it: the tables are applied to the scaler's picture, and where the scaler is
/// exact there is no level for a steep table to multiply (the plan no longer refuses how steep they are). Gentle and extreme
/// `eq`s on a ramp, edges and noise, in every way a layer is resized, in an export and in a Still plan.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_eq_alone_on_a_resized_layer_is_drawn_whatever_its_steps() {
    let mut fam = Family::default();
    let eq = |brightness, contrast, saturation, gamma, temperature| Color {
        brightness,
        contrast,
        saturation,
        gamma,
        temperature,
    };
    let colours = [
        ("t15 warm 1.8/1.5/0.7", eq(0.0, 1.8, 1.5, 0.7, 0.0)),
        ("contrast 3 saturation 3", eq(0.0, 3.0, 3.0, 1.0, 0.0)),
        ("brightness .3 gamma .5", eq(0.3, 1.0, 1.0, 0.5, 0.0)),
        ("gamma 2.5", eq(0.0, 1.0, 1.0, 2.5, 0.0)),
        ("brightness -.4 contrast 2", eq(-0.4, 2.0, 0.0, 1.0, 0.0)),
        ("temperature 1 gamma 3", eq(0.0, 1.0, 1.0, 3.0, 1.0)),
        ("gentle", eq(0.02, 1.05, 1.05, 1.05, 0.1)),
    ];
    let ramp = media(320, 180, Pat::Ramp, Tag::None);
    let edges = media(320, 180, Pat::Edges, Tag::None);
    let noise = media(320, 180, Pat::Noise, Tag::None);
    for (cname, color) in colours {
        for (label, src, delivery, scale) in [
            ("ramp pip 0.5", &ramp, None, 0.5),
            ("edges shrunk 2:1", &edges, Some(Delivery::new(160, 90, Fit::Contain)), 1.0),
            ("noise enlarged 2:1", &noise, Some(Delivery::new(640, 360, Fit::Contain)), 1.0),
            ("ramp cover 160x160", &ramp, Some(Delivery::new(160, 160, Fit::Cover)), 1.0),
        ] {
            let mut c = clip(src, 0.5, 0.8, 0.0);
            c.transform.scale = scale;
            c.color = color;
            let tl = timeline(vec![vec![c]], delivery);
            let case = format!("checker/b/eq/{label} {cname}");
            let out = sweep_secs(&case, &tl, std::slice::from_ref(src), 30.0, 0.3);
            fam.add(&case, &out);
            let mut c = clip(src, 0.0, 2.0, 0.0);
            c.transform.scale = scale;
            c.color = color;
            let tl = timeline(vec![vec![c]], delivery);
            let case = format!("checker/b/eq/still {label} {cname}");
            fam.add_still(&case, &still_sweep(&case, &tl, std::slice::from_ref(src), &[0.5]));
        }
    }
    fam.finish("eq alone on a resized layer");
}

/// The first clip is a picture the compositor does not convert (10-bit, 4:4:4, 4:2:2, gray, a full-range flag) and tagged
/// BT.709; a plain untagged one follows. The first clip decides the matrix and the second is converted into it: a drawn frame
/// is exact whichever clip it is of, and the frames of the first are refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_first_clip_of_another_pixel_format() {
    let mut fam = Family::default();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str, pix: &str, codec: &[&str], extra: &[&str]| -> Asset {
        static WRITING: Mutex<()> = Mutex::new(());
        let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let p = dir.join(name);
        if !p.exists() {
            let lavfi = format!("testsrc2=size=320x180:rate=30:duration=4{}", Tag::Bt709.filter());
            let mut args = vec!["-f", "lavfi", "-i", lavfi.as_str()];
            args.extend_from_slice(codec);
            args.extend_from_slice(extra);
            args.extend_from_slice(&["-pix_fmt", pix, p.to_str().unwrap()]);
            ffmpeg(&args);
        }
        probed(&p)
    };
    let x264 = ["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
    let ffv1 = ["-c:v", "ffv1"];
    let second = media(320, 180, Pat::Noise, Tag::None);
    let list = [
        ("10-bit 709", make("first-10.mp4", "yuv420p10le", &x264, &[])),
        ("4:4:4 709", make("first-444.mkv", "yuv444p", &ffv1, &[])),
        ("4:2:2 709", make("first-422.mkv", "yuv422p", &ffv1, &[])),
        ("gray 709", make("first-gray.mkv", "gray", &ffv1, &[])),
        (
            "pc-flagged 709",
            make("first-pc.mp4", "yuv420p", &x264, &["-color_range", "pc"]),
        ),
        ("alpha 709", alpha_media(320, 180, Tag::Bt709)),
    ];
    for (name, first) in &list {
        for order in ["first then plain", "plain then first"] {
            let tracks = if order == "first then plain" {
                vec![vec![clip(first, 0.5, 1.0, 0.0), clip(&second, 0.5, 1.0, 0.5)]]
            } else {
                vec![vec![clip(&second, 0.5, 1.0, 0.0), clip(first, 0.5, 1.0, 0.5)]]
            };
            let tl = timeline(tracks, None);
            let case = format!("checker/b/matrix/format {name} {order}");
            if let Some(out) = sweep_try(&case, &tl, &dedup(&[first, &second]), 30.0, 1.0) {
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("matrix first clip of another pixel format");
}

/// An RGB picture (an opaque PNG) in a cut whose video is BT.709: FFmpeg 9 takes it into YCbCr with the cut's matrix, wherever
/// the video is on the timeline, so a frame of the picture alone is converted with BT.709. The plan refuses an RGB picture in
/// a stack that is not BT.601 (`RgbInStack`), but judges the stack on screen: a frame of the picture alone is a BT.601 stack.
/// (Not a mixed-matrices cut, so `cut_matrix` is not made: the video's matrix is the only one.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_an_rgb_picture_alone_in_a_bt709_cut() {
    let mut fam = Family::default();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str, lavfi: &str| -> Asset {
        static WRITING: Mutex<()> = Mutex::new(());
        let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let p = dir.join(name);
        if !p.exists() {
            ffmpeg(&["-f", "lavfi", "-i", lavfi, "-frames:v", "1", p.to_str().unwrap()]);
        }
        probed(&p)
    };
    let pictures = [
        (
            "noise",
            make(
                "rgb-noise.png",
                "nullsrc=size=320x180:rate=1:duration=1,format=rgb24,geq=r='random(1)*255':g='random(2)*255':b='random(3)*255'",
            ),
        ),
        (
            "testsrc2",
            make("rgb-testsrc2.png", "testsrc2=size=320x180:rate=1:duration=1"),
        ),
        (
            "saturated bars",
            make(
                "rgb-bars.png",
                "nullsrc=size=320x180:rate=1:duration=1,format=rgb24,geq=r='255*lt(mod(floor(X/40),3),1)':g='255*gte(mod(floor(X/40),3),1)*lt(mod(floor(X/40),3),2)':b='255*gte(mod(floor(X/40),3),2)'",
            ),
        ),
    ];
    for (pname, png) in &pictures {
        for tag in [Tag::Bt709, Tag::Bt2020, Tag::None] {
            let video = media(320, 180, Pat::Test, tag);
            for order in ["video then picture", "picture then video"] {
                let tracks = if order == "video then picture" {
                    vec![vec![clip(&video, 0.5, 1.2, 0.0), clip(png, 0.0, 1.0, 0.7)]]
                } else {
                    vec![vec![clip(png, 0.0, 0.7, 0.0), clip(&video, 0.5, 1.2, 0.7)]]
                };
                let tl = timeline(tracks, None);
                let case = format!("checker/b/matrix/rgb picture {pname} with {tag:?} video, {order}");
                let out = sweep_secs(&case, &tl, &dedup(&[&video, png]), 30.0, 1.4);
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("matrix an RGB picture alone in a BT.709 cut");
}

/// Portrait pictures whose fitted width in a 320x180 frame is even and odd, with the bars on and off the 4:2:0 grid, of both
/// matrices in both orders, plain and dissolving: what the plan draws (the picture and its bars converted together) is exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_matrix_letterboxed_widths_on_and_off_the_chroma_grid() {
    let mut fam = Family::default();
    let base709 = media(320, 180, Pat::Edges, Tag::Bt709);
    let mut drawn = BTreeSet::new();
    for pw in [176u32, 180, 182, 184, 186, 190, 192, 200, 208, 216, 240] {
        let portrait601 = media(pw, 320, Pat::Noise, Tag::None);
        let portrait709 = media(pw, 320, Pat::Test, Tag::Bt709);
        for (label, tracks, assets) in [
            (
                "601 portrait after 709 landscape",
                vec![vec![clip(&base709, 0.5, 1.0, 0.0), clip(&portrait601, 0.5, 1.0, 0.5)]],
                dedup(&[&base709, &portrait601]),
            ),
            (
                "709 portrait after 601 portrait",
                vec![vec![clip(&portrait601, 0.5, 1.0, 0.0), clip(&portrait709, 0.5, 1.0, 0.5)]],
                dedup(&[&portrait601, &portrait709]),
            ),
            (
                "709 portrait dissolving after 601 portrait",
                vec![vec![
                    clip(&portrait601, 0.5, 1.0, 0.0),
                    dissolve_into(clip(&portrait709, 0.5, 1.0, 0.5), TransitionKind::Crossfade, 0.3),
                ]],
                dedup(&[&portrait601, &portrait709]),
            ),
        ] {
            let tl = timeline(tracks, Some(Delivery::new(320, 180, Fit::Contain)));
            let case = format!("checker/b/matrix/letterbox {pw}x320 {label}");
            let out = sweep_secs(&case, &tl, &assets, 30.0, 1.0);
            if out.exact() > 0 {
                drawn.insert(format!("{pw}:{label}"));
            }
            fam.add(&case, &out);
        }
    }
    eprintln!("drawn: {drawn:?}");
    fam.finish("matrix letterboxed widths");
}

/// What the three probes say about this machine's FFmpeg, and what the compositor's caps follow from them. The probes are the
/// lane's safety: a layer is drawn on the strength of them, so a probe that answered `Match` for the wrong model is an inexact
/// draw, and one that answered `Differs` is a refusal of everything.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_probes_describe_this_ffmpeg() {
    use kerf_gpu::scaler::{Kernel, Kernels};
    let scaler = kerf_gpu::scaler::kernels();
    let convert = kerf_gpu::convert::kernels();
    let grade = kerf_gpu::grade::kernels();
    eprintln!(
        "{}: scaler {scaler:?}, convert {convert:?}, grade {grade:?}",
        kerf_core::ffmpeg_path()
    );
    let Kernels::Match(model) = scaler else {
        panic!("this FFmpeg's scaler is not a model: {scaler:?}");
    };
    if cfg!(target_arch = "x86_64") {
        assert_eq!(
            model.kernel,
            Kernel::X86,
            "an x86-64 build with SIMD scalers runs the x86 kernel"
        );
    }
    let caps = compositor().caps();
    assert!(caps.exact_scaler && caps.blur_fill, "{caps:?}");
    if negotiates() {
        assert_eq!(convert, kerf_gpu::convert::Kernels::Match);
        assert!(caps.matrix_convert, "{caps:?}");
    }
    // The measured answers are what the compositor draws with, and they hold for the next call as for the first.
    assert_eq!(kerf_gpu::scaler::kernels(), scaler);
    assert_eq!(kerf_gpu::scaler::measured(), scaler);
}

/// The scalar model against FFmpeg around the plan's 63:1 limit (`MAX_SHRINK`, was 40:1). Prints where the model stops being
/// swscale's. Asserts that the model holds at every ratio up to the limit and fails at 64:1 (swscale needs a second scaler from
/// there, so the refusal is needed); the ratios beyond are reported.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_b_scaler_model_beyond_the_shrink_limit() {
    use kerf_gpu::sws::Scaler;
    let mut report = Vec::new();
    for ratio_x10 in [
        400usize, 405, 410, 420, 450, 500, 550, 600, 620, 625, 630, 632, 635, 640, 650, 700, 800, 1000,
    ] {
        let dw = 16usize;
        let sw = dw * ratio_x10 / 10;
        let mut bad = Vec::new();
        for flag in [Scaler::Bicubic, Scaler::Bilinear] {
            for fill in [Fill::Noise, Fill::Checker] {
                if let Err(e) = model_vs_ffmpeg(fill, (sw, 90), (dw, 90), flag, "1") {
                    bad.push(e);
                }
                if let Err(e) = model_vs_ffmpeg(fill, (90, sw), (90, dw), flag, "1") {
                    bad.push(e);
                }
            }
        }
        eprintln!(
            "shrink {:.1}:1 ({sw} -> {dw}): {}",
            ratio_x10 as f64 / 10.0,
            if bad.is_empty() {
                "model matches".to_string()
            } else {
                bad[0].clone()
            }
        );
        report.push((ratio_x10, bad.is_empty()));
        if ratio_x10 <= 630 {
            assert!(
                bad.is_empty(),
                "the model fails at {ratio_x10}/10:1, inside the plan's own limit: {bad:?}"
            );
        }
        if ratio_x10 == 640 {
            assert!(
                !bad.is_empty(),
                "the model matches at 64:1: the refusal past 63:1 is wider than it has to be"
            );
        }
    }
    eprintln!(
        "beyond the limit the model matches at x10 ratios {:?}",
        report.iter().filter(|(_, ok)| *ok).map(|(r, _)| *r).collect::<Vec<_>>()
    );
}

/// Untagged HD is labelled BT.709 by the graph when another input names a matrix, and a clip so labelled is converted (or
/// decides) as a BT.709 one: every arrangement of an untagged HD clip with tagged and untagged SD ones, shrunk into an SD
/// frame (scaled, then converted).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); slow: 720p"]
fn checker_b_matrix_untagged_hd_labelled_bt709() {
    let mut fam = Family::default();
    let hd = media(1280, 720, Pat::Test, Tag::None);
    let hd709 = media(1280, 720, Pat::Noise, Tag::Bt709);
    let sd170 = media(640, 360, Pat::Noise, Tag::Smpte170m);
    let sd_none = media(640, 360, Pat::Edges, Tag::None);
    let sd709 = media(640, 360, Pat::Test, Tag::Bt709);
    let cases: Vec<(&str, Vec<Clip>, Vec<Asset>)> = vec![
        (
            "hd untagged then sd 170m",
            vec![clip(&hd, 0.5, 0.9, 0.0), clip(&sd170, 0.5, 0.9, 0.4)],
            dedup(&[&hd, &sd170]),
        ),
        (
            "sd 170m then hd untagged",
            vec![clip(&sd170, 0.5, 0.9, 0.0), clip(&hd, 0.5, 0.9, 0.4)],
            dedup(&[&hd, &sd170]),
        ),
        (
            "hd untagged then sd untagged",
            vec![clip(&hd, 0.5, 0.9, 0.0), clip(&sd_none, 0.5, 0.9, 0.4)],
            dedup(&[&hd, &sd_none]),
        ),
        (
            "hd untagged then sd 709",
            vec![clip(&hd, 0.5, 0.9, 0.0), clip(&sd709, 0.5, 0.9, 0.4)],
            dedup(&[&hd, &sd709]),
        ),
        (
            "sd 709 then hd untagged",
            vec![clip(&sd709, 0.5, 0.9, 0.0), clip(&hd, 0.5, 0.9, 0.4)],
            dedup(&[&hd, &sd709]),
        ),
        (
            "hd untagged then hd 709",
            vec![clip(&hd, 0.5, 0.9, 0.0), clip(&hd709, 0.5, 0.9, 0.4)],
            dedup(&[&hd, &hd709]),
        ),
        (
            "sd untagged, hd untagged, sd 170m",
            vec![
                clip(&sd_none, 0.5, 0.8, 0.0),
                clip(&hd, 0.5, 0.8, 0.3),
                clip(&sd170, 0.5, 0.8, 0.6),
            ],
            dedup(&[&hd, &sd_none, &sd170]),
        ),
    ];
    for (name, clips, assets) in cases {
        for dissolve in [false, true] {
            let mut clips = clips.clone();
            if dissolve {
                clips[1] = dissolve_into(clips[1].clone(), TransitionKind::Crossfade, 0.2);
            }
            let tl = timeline(vec![clips], Some(Delivery::new(640, 360, Fit::Contain)));
            let case = format!(
                "checker/b/matrix/hd label {name}{}",
                if dissolve { " dissolving" } else { "" }
            );
            let out = sweep_secs(&case, &tl, &assets, 15.0, 1.1);
            fam.add(&case, &out);
        }
    }
    fam.finish("matrix untagged HD labelled BT.709");
}

/// A graded layer with letterbox bars or a blurred fill (the bars are graded with the picture and their chroma meets the
/// picture's in the conversion back) stays the plan's refusal, with an `eq` ahead of it as without one. Drawn frames are
/// exact; the same cases run with the refusal patched out say whether it is needed.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_grade_on_a_letterboxed_or_filled_layer() {
    let mut fam = Family::default();
    let portrait = media(180, 320, Pat::Noise, Tag::None);
    let wide = media(320, 180, Pat::Edges, Tag::None);
    let eq = Color {
        contrast: 1.1,
        saturation: 1.2,
        ..Color::default()
    };
    for (gname, grade) in grades_b() {
        for (label, src, delivery) in [
            ("portrait bars", &portrait, Delivery::new(320, 180, Fit::Contain)),
            ("portrait blur", &portrait, Delivery::new(320, 180, Fit::Blur)),
            ("wide in tall bars", &wide, Delivery::new(180, 320, Fit::Contain)),
            ("wide in tall blur", &wide, Delivery::new(180, 320, Fit::Blur)),
        ] {
            for with_eq in [true, false] {
                let mut c = clip(src, 0.5, 0.8, 0.0);
                c.grade = grade.clone();
                if with_eq {
                    c.color = eq;
                }
                let tl = timeline(vec![vec![c]], Some(delivery));
                let case = format!("checker/b/grade/{label} {gname} eq {with_eq}");
                let out = sweep_secs(&case, &tl, std::slice::from_ref(src), 30.0, 0.3);
                fam.add(&case, &out);
            }
        }
    }
    fam.finish_refusing("grade on a letterboxed or filled layer");
}

/// The slides, pushes and dips between clips of two matrices in a Still plan, in the middle of the transition and either side.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_still_matrix_slides_pushes_and_dips() {
    let mut fam = Family::default();
    for (a, b) in [(Tag::Bt709, Tag::None), (Tag::None, Tag::Bt2020), (Tag::Bt2020, Tag::Bt709)] {
        let from = media(320, 180, Pat::Edges, a);
        let to = media(320, 180, Pat::Noise, b);
        let assets = dedup(&[&from, &to]);
        for kind in [
            TransitionKind::SlideLeft,
            TransitionKind::SlideDown,
            TransitionKind::PushRight,
            TransitionKind::PushUp,
            TransitionKind::DipToBlack,
            TransitionKind::Crossfade,
        ] {
            let tl = timeline(
                vec![vec![
                    clip(&from, 0.0, 2.0, 0.0),
                    dissolve_into(clip(&to, 0.0, 2.0, 1.5), kind, 0.5),
                ]],
                None,
            );
            let case = format!("checker/b/still/{kind:?} {a:?} into {b:?}");
            fam.add_still(&case, &still_sweep(&case, &tl, &assets, &[0.5, 1.3, 1.6, 1.75, 1.95, 2.4]));
        }
    }
    fam.finish("still matrix slides, pushes and dips");
}

/// A picture of odd height (or width) whose letterbox offset `(frame - picture) / 2` is odd: FFmpeg's `pad` rounds the
/// offset down to the chroma grid, so the picture sits on the even sample before. A picture one row high is the extreme of it.
/// (Not this lane's arithmetic: the plain `Contain` chain of the base; found by the 1-px sizes.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_pad_of_an_odd_picture_at_an_odd_offset() {
    let mut fam = Family::default();
    // (source, frame): the fitted picture keeps the source's shape where it fits the frame on one axis.
    for (sw, sh, fw, fh) in [
        (64, 1, 64, 36),
        (64, 5, 64, 36),
        (64, 9, 64, 36),
        (64, 13, 64, 36),
        (64, 3, 64, 36),
        (64, 7, 64, 36),
        (1, 64, 36, 64),
        (5, 64, 36, 64),
        (9, 64, 36, 64),
        (3, 64, 36, 64),
        (96, 7, 96, 54),
        (96, 11, 96, 54),
    ] {
        let a = media(sw, sh, Pat::Noise, Tag::None);
        let tl = timeline(vec![vec![clip(&a, 0.5, 0.8, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
        let case = format!("checker/b/pad/{sw}x{sh} in {fw}x{fh}");
        let out = sweep_secs(&case, &tl, std::slice::from_ref(&a), 30.0, 0.2);
        fam.add(&case, &out);
    }
    fam.finish("pad of an odd picture at an odd offset");
}

// ---- round 2: the checks of 76ed1c11 (which clip decides, the scaler named and probed, the lifted refusals) --------------

/// 320x180 noise in `pix` (4:2:2 or 4:4:4, FFV1 in Matroska), tagged: chroma as busy as luma.
fn pixfmt_media(pix: &str, tag: Tag) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("noise-{pix}-{tag:?}.mkv"));
    if !path.exists() {
        let lavfi = format!(
            "nullsrc=size=320x180:rate=30:duration=4,format={pix},geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'{}",
            tag.filter()
        );
        let tmp = dir.join(format!("noise-{pix}-{tag:?}.tmp.mkv"));
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Plain,
    Crossfade,
    DipBlack,
    DipWhite,
    SlideLeft,
    PushUp,
    FadeIn,
    FadeOut,
    KeyedScale,
    KeyedPos,
    HeldKey,
    AlphaSource,
    Pip,
    Crop,
    Speed2,
    Reverse,
}

const ROUTES: [Route; 16] = [
    Route::Plain,
    Route::Crossfade,
    Route::DipBlack,
    Route::DipWhite,
    Route::SlideLeft,
    Route::PushUp,
    Route::FadeIn,
    Route::FadeOut,
    Route::KeyedScale,
    Route::KeyedPos,
    Route::HeldKey,
    Route::AlphaSource,
    Route::Pip,
    Route::Crop,
    Route::Speed2,
    Route::Reverse,
];

/// A clip of 0.7 s at `start` that takes `route` onto the graph, from a 320x180 picture tagged `tag`.
fn routed(route: Route, tag: Tag, pat: Pat, start: f64) -> (Clip, Asset) {
    let a = if route == Route::AlphaSource {
        alpha_media(320, 180, tag)
    } else {
        media(320, 180, pat, tag)
    };
    let mut c = clip(&a, 0.5, 1.2, start);
    match route {
        Route::Plain | Route::AlphaSource => {}
        Route::Crossfade => c = dissolve_into(c, TransitionKind::Crossfade, 0.3),
        Route::DipBlack => c = dissolve_into(c, TransitionKind::DipToBlack, 0.3),
        Route::DipWhite => c = dissolve_into(c, TransitionKind::DipToWhite, 0.3),
        Route::SlideLeft => c = dissolve_into(c, TransitionKind::SlideLeft, 0.3),
        Route::PushUp => c = dissolve_into(c, TransitionKind::PushUp, 0.3),
        Route::FadeIn => c.fade_in = 0.3,
        Route::FadeOut => c.fade_out = 0.3,
        Route::KeyedScale => c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 0.8, 0.0, 0.0)],
        Route::KeyedPos => c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 1.0, 0.1, 0.05)],
        Route::HeldKey => c.keyframes = vec![zoom_key(0.0, 0.9, 0.0, 0.0), zoom_key(0.7, 0.9, 0.0, 0.0)],
        Route::Pip => c = pip(c, 0.6, 0.1, 0.1),
        Route::Crop => {
            c.transform.crop_left = 0.1;
            c.transform.crop_bottom = 0.15;
        }
        Route::Speed2 => {
            c.speed = 2.0;
            c.source_out = 0.5 + 1.4;
        }
        Route::Reverse => c.speed = -1.0,
    }
    (c, a)
}

/// Runs `(name, timeline, assets)` cases as one family, every drawn frame exact.
fn run_family(family: &str, cases: Vec<(String, Timeline, Vec<Asset>)>, fps: f64, secs: f64) {
    let mut fam = Family::default();
    for (name, tl, assets) in cases {
        let case = format!("checker/b/r2/{family}/{name}");
        if let Some(out) = sweep_try(&case, &tl, &assets, fps, secs) {
            // A dip to white in a cut has the base's ladder on every frame of the graph (the equal-matrix cut of the same shape
            // shows the same figures: `checker_b_r2_dip_to_white_in_the_cut_leaves_the_clips_before_it_exact`): held to 3 levels.
            if name.contains("DipWhite") {
                let worst = out.bad.iter().flat_map(|b| b.planes.iter().map(|p| p.max)).max().unwrap_or(0);
                if worst > 3 {
                    fam.failures.push(format!(
                        "{case}: a dip to white's ladder is 3 levels, drawn frames differ by {worst}"
                    ));
                }
                fam.add_known_ladder(&case, &out);
            } else {
                fam.add(&case, &out);
            }
        }
    }
    fam.finish(family);
}

/// Two clips whose matrices differ, back to back on one track, each taking every route onto the graph: the composite's matrix
/// is the first clip on the alpha plane's, and the plan either reads it or refuses the cut. (The first clip takes six routes,
/// the second all sixteen, in both tag orders.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_on_one_track() {
    let mut cases = Vec::new();
    for first in [
        Route::Plain,
        Route::KeyedScale,
        Route::AlphaSource,
        Route::FadeOut,
        Route::Crossfade,
        Route::Pip,
    ] {
        for second in ROUTES {
            for (ta, tb) in [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None)] {
                let (a, aa) = routed(first, ta, Pat::Edges, 0.0);
                let (b, ab) = routed(second, tb, Pat::Noise, 0.7);
                cases.push((
                    format!("{first:?} {ta:?} then {second:?} {tb:?}"),
                    timeline(vec![vec![a, b]], None),
                    dedup(&[&aa, &ab]),
                ));
            }
        }
    }
    run_family("decider on one track", cases, 15.0, 1.4);
}

/// The same pairs stacked: the second clip on a track above the first, over the whole of it (a pip and a full-frame layer).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_stacked() {
    let mut cases = Vec::new();
    for first in [Route::Plain, Route::KeyedScale, Route::AlphaSource] {
        for second in ROUTES {
            for (ta, tb) in [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None), (Tag::Bt709, Tag::Bt2020)] {
                let (a, aa) = routed(first, ta, Pat::Edges, 0.0);
                let (mut b, ab) = routed(second, tb, Pat::Noise, 0.0);
                if second != Route::Pip && second != Route::KeyedScale {
                    b = pip(b, 0.6, 0.15, -0.1);
                }
                let mut a = a;
                a.source_out = a.source_in + 1.4;
                cases.push((
                    format!("{second:?} {tb:?} over {first:?} {ta:?}"),
                    timeline(vec![vec![a], vec![b]], None),
                    dedup(&[&aa, &ab]),
                ));
            }
        }
    }
    run_family("decider stacked", cases, 15.0, 0.7);
}

/// Three clips of three matrices on one track and across tracks, the deciding route on the first, the second, the third, and a
/// track that is hidden or soloed away, a disabled clip, the same start time twice.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_three_clips_hidden_tracks_and_ties() {
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    let tags = [Tag::None, Tag::Bt709, Tag::Bt2020];
    for order in [[0, 1, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        for deciding in [0usize, 1, 2] {
            for route in [Route::Crossfade, Route::KeyedPos, Route::AlphaSource, Route::FadeIn] {
                let mut clips = Vec::new();
                let mut assets = Vec::new();
                for (i, &t) in order.iter().enumerate() {
                    let r = if i == deciding { route } else { Route::Plain };
                    let (c, a) = routed(r, tags[t], Pat::Noise, 0.7 * i as f64);
                    clips.push(c);
                    assets.push(a);
                }
                let refs: Vec<&Asset> = assets.iter().collect();
                cases.push((
                    format!("{route:?} on clip {deciding} of {:?}", order.map(|t| tags[t])),
                    timeline(vec![clips], None),
                    dedup(&refs),
                ));
            }
        }
    }
    // A hidden track: its clips are not in the graph, so they decide nothing and convert nothing.
    for hidden_by in ["muted", "solo elsewhere"] {
        let (a, aa) = routed(Route::Plain, Tag::None, Pat::Noise, 0.0);
        let (b, ab) = routed(Route::KeyedScale, Tag::Bt709, Pat::Test, 0.0);
        let (c, ac) = routed(Route::Plain, Tag::Bt2020, Pat::Edges, 0.0);
        let mut tl = timeline(vec![vec![a], vec![b], vec![c]], None);
        match hidden_by {
            "muted" => tl.tracks[1].muted = true,
            _ => tl.tracks[2].solo = true,
        }
        cases.push((format!("hidden track ({hidden_by})"), tl, dedup(&[&aa, &ab, &ac])));
    }
    // A disabled clip is dropped before the graph is built.
    {
        let (a, aa) = routed(Route::Plain, Tag::None, Pat::Noise, 0.0);
        let (mut b, ab) = routed(Route::Crossfade, Tag::Bt709, Pat::Test, 0.7);
        b.enabled = false;
        let (c, ac) = routed(Route::Plain, Tag::Bt2020, Pat::Edges, 0.7);
        cases.push((
            "a disabled dissolving clip".into(),
            timeline(vec![vec![a, b, c]], None),
            dedup(&[&aa, &ab, &ac]),
        ));
    }
    // Two clips that start together (a tie in the export's sort), the dissolving one stored second, and first.
    for stored_first in [false, true] {
        let (a, aa) = routed(Route::Plain, Tag::None, Pat::Noise, 0.0);
        let (b, ab) = routed(Route::KeyedPos, Tag::Bt709, Pat::Test, 0.0);
        let pair = if stored_first { vec![b, a] } else { vec![a, b] };
        cases.push((
            format!("a tie in start times, keyed stored first {stored_first}"),
            timeline(vec![pair], None),
            dedup(&[&aa, &ab]),
        ));
    }
    run_family("decider, three clips, hidden tracks, ties", cases, 15.0, 2.0);
}

// ---- a wider seeded fuzz: every feature that may touch a clip's chain, in cuts of mixed matrices and formats ----------------

/// Whether the wide fuzz draws pictures of other formats too (the third run of it).
static FUZZ2_FORMATS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 320x180 noise in a format that is not 8-bit 4:2:0 / 4:2:2 / 4:4:4 yuv, tagged.
fn formats_media(pix: &str, tag: Tag) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("fmt-{pix}-{tag:?}.mkv"));
    if !path.exists() {
        let base = if pix == "gray" || pix == "yuv420p10le" {
            "yuv420p"
        } else {
            pix
        };
        let lavfi = format!(
            "nullsrc=size=320x180:rate=30:duration=4,format={base},geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'{}",
            tag.filter()
        );
        let tmp = dir.join(format!("fmt-{pix}-{tag:?}.tmp.mkv"));
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

/// The pools a clip of the wide fuzz is drawn from.
fn fuzz2_asset(rng: &mut Rng, desc: &mut String) -> Asset {
    let tag = rng.pick(&TAGS);
    let roll = rng.below(100);
    if roll >= 94 && FUZZ2_FORMATS.load(std::sync::atomic::Ordering::Relaxed) {
        // Pictures the lane does not draw as converted layers (or draws only in a corner), as members of the cut.
        let which = rng.below(4);
        let a = match which {
            0 => formats_media("gray", tag),
            1 => formats_media("yuva444p", tag),
            2 => formats_media("yuv420p10le", tag),
            _ => rgb_noise_png(),
        };
        let _ = write!(desc, "[fmt{which} {tag:?}]");
        return a;
    }
    if roll < 8 {
        let _ = write!(desc, "[alpha {tag:?}]");
        alpha_media(320, 180, tag)
    } else if roll < 14 {
        let pix = rng.pick(&["yuv444p", "yuv422p"]);
        let tag = rng.pick(&[Tag::None, Tag::Bt709]);
        let _ = write!(desc, "[{pix} {tag:?}]");
        pixfmt_media(pix, tag)
    } else {
        let shape = rng.pick(&[(320, 180), (320, 180), (320, 180), (180, 320), (160, 90), (240, 240)]);
        let pat = rng.pick(&[Pat::Noise, Pat::Test, Pat::Edges, Pat::Bars]);
        let _ = write!(desc, "[{}x{} {pat:?} {tag:?}]", shape.0, shape.1);
        media(shape.0, shape.1, pat, tag)
    }
}

/// One cut of the wide fuzz and whether it holds a feature with a ladder in the base (a translucent or turned layer, a
/// dip to white, a blur or a sharpen, a chroma key): such a cut is held to a gross bound only (a wrong matrix is off by ten
/// levels and more), the others are held to zero.
fn fuzz2_cut(seed: u64) -> (String, Timeline, Vec<Asset>, f64, bool) {
    use kerf_core::{BlendMode, VideoEffect};
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x51ed_270b);
    let mut desc = String::new();
    let mut assets: Vec<Asset> = Vec::new();
    let mut ladder = false;
    let delivery = match rng.below(10) {
        0..=2 => None,
        3 | 4 => Some(Delivery::new(320, 180, Fit::Contain)),
        5 | 6 => Some(Delivery::new(320, 180, Fit::Blur)),
        7 => Some(Delivery::new(256, 144, Fit::Contain)),
        8 => Some(Delivery::new(180, 320, Fit::Cover)),
        _ => Some(Delivery::new(256, 144, Fit::Blur)),
    };
    let _ = write!(desc, "delivery {:?}; ", delivery.map(|d| (d.width, d.height, d.fit)));
    let add = |a: Asset, assets: &mut Vec<Asset>| {
        if !assets.iter().any(|b| b.id == a.id) {
            assets.push(a);
        }
    };
    let mut tracks: Vec<Vec<Clip>> = Vec::new();
    let n_tracks = 1 + usize::from(rng.chance(45)) + usize::from(rng.chance(15));
    let mut end: f64 = 0.0;
    let mut muted_tracks = Vec::new();
    for ti in 0..n_tracks {
        let n = if ti == 0 { 2 + rng.below(2) as usize } else { 1 };
        let mut t = if ti == 0 { 0.0 } else { 0.1 * rng.below(5) as f64 };
        let mut clips = Vec::new();
        for i in 0..n {
            let a = fuzz2_asset(&mut rng, &mut desc);
            let len = 0.6 + 0.1 * rng.below(3) as f64;
            let mut c = clip(&a, 0.5, 0.5 + len, t);
            let _ = write!(desc, "(t{ti}c{i}");
            if ti > 0 && rng.chance(70) {
                c = pip(c, rng.pick(&[0.5, 0.75, 1.0]), 0.1, 0.1);
                desc.push_str(" pip");
            }
            if (i > 0 || (ti == 0 && rng.chance(8))) && rng.chance(40) {
                let kind = rng.pick(&[
                    TransitionKind::Crossfade,
                    TransitionKind::Crossfade,
                    TransitionKind::DipToBlack,
                    TransitionKind::DipToWhite,
                    TransitionKind::SlideLeft,
                    TransitionKind::PushUp,
                ]);
                ladder |= kind == TransitionKind::DipToWhite;
                c = dissolve_into(c, kind, 0.3);
                let _ = write!(desc, " {kind:?}");
            }
            if rng.chance(10) {
                c.fade_in = 0.2;
                desc.push_str(" fade_in");
            }
            if rng.chance(10) {
                c.fade_out = 0.2;
                desc.push_str(" fade_out");
            }
            if rng.chance(12) {
                c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(len, 0.8, 0.05, 0.0)];
                desc.push_str(" keyed");
            } else if rng.chance(5) {
                c.keyframes = vec![zoom_key(0.0, 0.9, 0.0, 0.0), zoom_key(len, 0.9, 0.0, 0.0)];
                desc.push_str(" held");
            }
            if rng.chance(6) {
                c.speed = 1.5;
                c.source_out = c.source_in + len * 1.5;
                desc.push_str(" speed1.5");
            } else if rng.chance(4) {
                c.speed = -1.0;
                desc.push_str(" reverse");
            }
            if rng.chance(6) {
                c.transform.crop_left = 0.1;
                c.transform.crop_top = 0.1;
                desc.push_str(" crop");
            }
            if rng.chance(10) {
                c.effects = vec![if rng.chance(50) {
                    VideoEffect::Invert
                } else {
                    VideoEffect::Grayscale
                }];
                desc.push_str(" tableeffect");
            } else if rng.chance(4) {
                c.effects = vec![if rng.chance(50) {
                    VideoEffect::Blur { sigma: 2.0 }
                } else {
                    VideoEffect::Sharpen { amount: 0.5 }
                }];
                ladder = true;
                desc.push_str(" blur/sharpen");
            } else if rng.chance(2) {
                c.effects = vec![VideoEffect::ChromaKey {
                    color: "0x00ff00".into(),
                    similarity: 0.3,
                    blend: 0.1,
                }];
                ladder = true;
                desc.push_str(" chromakey");
            }
            if rng.chance(10) {
                c.color = Color {
                    contrast: 1.1,
                    saturation: 1.2,
                    ..Color::default()
                };
                desc.push_str(" eq");
                if rng.chance(40) {
                    c.grade = grades_b()[rng.below(3) as usize].1.clone();
                    desc.push_str("+grade");
                }
            }
            if rng.chance(7) {
                c.transform.opacity = 0.7;
                ladder = true;
                desc.push_str(" opacity");
            }
            if rng.chance(4) {
                c.mask = Some(kerf_core::Mask {
                    shape: kerf_core::MaskShape::Rect,
                    x: 0.5,
                    y: 0.5,
                    width: 0.6,
                    height: 0.6,
                    feather: 0.0,
                    inverted: false,
                });
                ladder = true;
                desc.push_str(" mask");
            }
            if rng.chance(4) {
                c.blend_mode = rng.pick(&[BlendMode::Screen, BlendMode::Multiply]);
                desc.push_str(" blend");
            }
            if rng.chance(5) && c.keyframes.is_empty() {
                c.channels = vec![kerf_core::PropertyTrack {
                    prop: kerf_core::Property::CropZoom,
                    keys: [(0.0, 1.0), (len, 1.5)]
                        .iter()
                        .map(|&(t, v)| kerf_core::PropertyKey::new(t, v))
                        .collect(),
                }];
                desc.push_str(" keyedcrop");
            } else if rng.chance(5) && c.keyframes.is_empty() {
                c.channels = vec![kerf_core::PropertyTrack {
                    prop: kerf_core::Property::Saturation,
                    keys: [(0.0, 1.0), (len, 1.4)]
                        .iter()
                        .map(|&(t, v)| kerf_core::PropertyKey::new(t, v))
                        .collect(),
                }];
                desc.push_str(" keyedcolour");
            }
            if rng.chance(3) {
                c.enabled = false;
                desc.push_str(" DISABLED");
            }
            desc.push(')');
            t += len;
            add(a, &mut assets);
            clips.push(c);
        }
        end = end.max(t);
        if ti > 0 && rng.chance(8) {
            muted_tracks.push(ti);
            desc.push_str(" <muted>");
        }
        tracks.push(clips);
    }
    let mut tl = timeline(tracks, delivery);
    for ti in muted_tracks {
        tl.tracks[ti].muted = true;
    }
    (desc, tl, assets, end.min(2.0), ladder)
}

/// `seeds` of the wide fuzz: the cuts without a ladder feature must be exact in every drawn frame; the others are held to a
/// gross bound (Y and UV within `GROSS` levels) and their figures reported.
fn run_fuzz2(label: &str, seeds: std::ops::Range<u64>) {
    const GROSS: i32 = 4;
    let mut fam = Family::default();
    let (mut drawn_total, mut refused_total) = (0, 0);
    for seed in seeds {
        let (desc, tl, assets, secs, ladder) = fuzz2_cut(seed);
        let case = format!("checker/b/fuzz2/{label}/{seed:03}");
        eprintln!("-- {case}{}: {desc}", if ladder { " [ladder]" } else { "" });
        let Some(out) = sweep_try(&case, &tl, &assets, 15.0, secs) else {
            continue;
        };
        drawn_total += out.drawn;
        refused_total += out.refused;
        if ladder {
            // A ladder is a few levels everywhere (a translucent layer's round trip, a dip's) or a thin line (a rotated layer's
            // edge): a plane that is off by more than GROSS levels over more than 4 % of its samples is not either.
            let gross = out
                .bad
                .iter()
                .flat_map(|b| (0..3).map(move |i| (b.planes[i].max, b.planes[i].differing * 100 / b.len[i].max(1))))
                .filter(|&(max, percent)| max > GROSS && percent > 4)
                .max();
            if let Some((max, percent)) = gross {
                fam.failures.push(format!(
                    "{case}: a ladder cut, drawn frames differ by up to {max} levels over {percent} % of a plane (more than the {GROSS} levels or the thin edge a ladder explains)\n    cut: {desc}",
                ));
            }
            fam.add_known_ladder(&case, &out);
            continue;
        }
        let before = fam.failures.len();
        fam.add(&case, &out);
        if fam.failures.len() > before {
            let last = fam.failures.last_mut().unwrap();
            *last = format!("{last}\n    cut: {desc}");
        }
    }
    eprintln!("== fuzz2 {label}: {drawn_total} frames drawn, {refused_total} refused");
    fam.finish(&format!("fuzz2 {label}"));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_fuzz_wide_a() {
    run_fuzz2("a", 0..60);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_fuzz_wide_b() {
    run_fuzz2("b", 1000..1060);
}

// ---- the scaler: chosen by a probe of the export's own `scale`, pinned to the answers a build can give --------------------

/// The compositor of this machine with its scaler pinned to `kernels` (what a probe on another FFmpeg would have said).
fn pinned(kernels: kerf_gpu::scaler::Kernels) -> Compositor {
    Compositor::new(gpu()).expect("a compositor").with_scaler_kernels(kernels)
}

/// The choice of the scaler is a measurement of the build's own `scale`, never its version: a compositor pinned to the answers
/// 4.4.2's probe would give draws what that answer says (bilinear by default, bicubic when the delivery names it), and one
/// whose scaler is not measured (`Differs`, `Unknown`) draws no layer a scale resizes. (4.4.2 itself is not available here.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_scaler_choice_follows_the_probe_and_the_delivery() {
    use kerf_gpu::scaler::{Kernels, Model};
    use kerf_gpu::sws::Scaler;
    let Kernels::Match(real) = kerf_gpu::scaler::kernels() else {
        panic!("this FFmpeg's scaler is not a model: {:?}", kerf_gpu::scaler::kernels());
    };
    let bilinear_default = Kernels::Match(Model {
        kernel: real.kernel,
        default: Scaler::Bilinear,
    });
    let wide = media(960, 540, Pat::Noise, Tag::None);
    let small = media(320, 180, Pat::Noise, Tag::None);
    let assets = dedup(&[&wide, &small]);
    // A 960x540 picture shrunk into the canvas and a picture-in-picture over it: both resized.
    let resized = timeline(
        vec![
            vec![clip(&wide, 0.5, 1.0, 0.0)],
            vec![pip(clip(&small, 0.5, 1.0, 0.0), 0.5, 0.1, 0.1)],
        ],
        Some(Delivery::new(320, 180, Fit::Contain)),
    );
    // Nothing resized: the canvas's own size.
    let unresized = timeline(
        vec![vec![clip(&small, 0.5, 1.0, 0.0)]],
        Some(Delivery::new(320, 180, Fit::Contain)),
    );
    let named = |s: Option<&str>| ExportOptions {
        scaler: s.map(str::to_string),
        ..at_fps(15.0)
    };
    let mut failures: Vec<String> = Vec::new();

    // 1. A scaler that is not measured as swscale's draws no resized layer, and draws an unresized one as it always did.
    for (label, k) in [("Differs", Kernels::Differs), ("Unknown", Kernels::Unknown)] {
        let comp = pinned(k);
        let caps = comp.caps();
        if !caps.scaler_approximate || caps.exact_scaler {
            failures.push(format!("{label}: caps {caps:?} do not carry scaler_approximate"));
        }
        let opts = named(None);
        let out = sweep_played_on(
            &comp,
            &format!("checker/b/r2/scaler {label} resized"),
            &resized,
            &assets,
            &opts,
            7,
            export("r2-scaler-resized", &resized, &assets, &opts),
        );
        if out.drawn != 0 || out.refused != out.frames || out.refused_for("scaler") < out.frames {
            failures.push(format!(
                "{label}: resized layers were drawn or not refused for the scaler: {out:?}"
            ));
        }
        let out = sweep_played_on(
            &comp,
            &format!("checker/b/r2/scaler {label} unresized"),
            &unresized,
            &assets,
            &opts,
            7,
            export("r2-scaler-unresized", &unresized, &assets, &opts),
        );
        if out.drawn != out.frames || !out.bad.is_empty() {
            failures.push(format!("{label}: an unresized layer was refused or drawn inexactly: {out:?}"));
        }
    }

    // 2. The pinned answer of an FFmpeg 4.4 (bilinear unflagged): a delivery naming bicubic is bicubic on every build, and the
    // compositor must draw it so; unflagged it draws bilinear, which these builds (bicubic by default) do not render, so the
    // pin is seen to take effect.
    let comp = pinned(bilinear_default);
    let opts = named(Some("bicubic"));
    let out = sweep_played_on(
        &comp,
        "checker/b/r2/scaler pinned-bilinear, delivery names bicubic",
        &resized,
        &assets,
        &opts,
        7,
        export("r2-scaler-named-bicubic", &resized, &assets, &opts),
    );
    if out.drawn != out.frames || !out.bad.is_empty() {
        failures.push(format!(
            "a delivery naming bicubic was not drawn exactly on a bilinear-default model: {out:?}"
        ));
    }
    let opts = named(None);
    let out = sweep_played_on(
        &comp,
        "checker/b/r2/scaler pinned-bilinear, unflagged",
        &resized,
        &assets,
        &opts,
        7,
        export("r2-scaler-unflagged", &resized, &assets, &opts),
    );
    if real.default == Scaler::Bicubic && out.bad.is_empty() {
        failures
            .push("the pinned bilinear default drew a bicubic build's unflagged scale exactly: the pin did nothing".to_string());
    }

    // 3. The real answer, flagged and not.
    for s in [None, Some("bicubic")] {
        let opts = named(s);
        let out = sweep_played_on(
            compositor(),
            &format!("checker/b/r2/scaler measured, delivery {s:?}"),
            &resized,
            &assets,
            &opts,
            7,
            export(&format!("r2-scaler-measured-{s:?}"), &resized, &assets, &opts),
        );
        if out.drawn != out.frames || !out.bad.is_empty() {
            failures.push(format!("the measured scaler, delivery {s:?}: {out:?}"));
        }
    }

    // 4. Every other name is refused by the plan; is each refusal needed? (reported: `bilinear` has a model.)
    for s in ["bilinear", "lanczos", "neighbor", "spline"] {
        let opts = named(Some(s));
        let a = anyway(
            &format!("checker/b/r2/scaler delivery names {s}"),
            &resized,
            &assets,
            &opts,
            7,
            compositor(),
            3,
        );
        if a.refused != 7 || !a.reasons.keys().any(|r| r.contains(&format!("scaler '{s}'"))) {
            failures.push(format!("a delivery naming {s} was not refused as such: {a:?}"));
        }
        if patched_out() && a.needed() != Some(true) {
            failures.push(format!(
                "a delivery naming {s}: the refusal is not needed (drawn anyway exact): {a:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The probe's scales are the export's own text: the fit `scale` of a Contain export reads `scale=W:H:force_original_aspect_ratio=
/// decrease` plus the delivery's `:flags=` (or none), and the probe asks the same filter, unflagged and with each flag, of a
/// complex graph. Checks the source names no version anywhere on the path from the probe to the caps.
#[test]
fn checker_b_r2_scaler_probe_reads_no_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in ["scaler.rs", "convert.rs", "compositor.rs"] {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for needle in [
                "ffmpeg_version",
                "version()",
                "\"-version\"",
                "avcodec_version",
                "avfilter_version",
                "starts_with(\"4",
            ] {
                assert!(!code.contains(needle), "{file}:{}: reads a version: {line}", n + 1);
            }
        }
    }
}

// ---- the refusals the change keeps or adds: each drawn anyway, to see whether it is needed ---------------------------------

/// A short (12-frame) lossless clip of any size, tagged: the steep-shrink sources are too large to keep four seconds of.
fn short_media(w: u32, h: u32, pat: Pat, tag: Tag) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("short-{w}x{h}-{pat:?}-{tag:?}.mkv"));
    if !path.exists() {
        let src = match pat {
            Pat::Noise => format!(
                "nullsrc=size={w}x{h}:rate=30:duration=0.4,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
            ),
            Pat::Edges => format!(
                "nullsrc=size={w}x{h}:rate=30:duration=0.4,format=yuv420p,\
                 geq=lum='if(mod(floor(X/8)+floor(Y/8),2),235,16)':cb='if(mod(floor(X/16),2),200,60)':cr='if(mod(floor(Y/16),2),190,70)'"
            ),
            _ => format!("testsrc2=size={w}x{h}:rate=30:duration=0.4"),
        };
        let lavfi = format!("{src}{}", tag.filter());
        let tmp = dir.join(format!("short-{w}x{h}-{pat:?}-{tag:?}.tmp.mkv"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
            tmp.to_str().unwrap(),
        ]);
        std::fs::rename(&tmp, &path).unwrap();
    }
    probed(&path)
}

/// A converted layer over a blurred fill: portrait pictures of both matrices (widths on and off the chroma grid) in landscape
/// frames, back to back, stacked over a landscape picture of the other matrix, and dissolving (a converted layer that itself
/// dissolves in over a fill stays refused).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_lifted_converted_layer_over_a_blurred_fill() {
    let mut fam = Family::default();
    let land709 = media(320, 180, Pat::Edges, Tag::Bt709);
    let mut necessity: Vec<(String, Anyway)> = Vec::new();
    let mut drawn_converted = 0;
    for (fw, fh) in [(320, 180), (256, 144), (320, 240)] {
        for pw in [176u32, 180, 182, 184, 190, 200] {
            let p601 = media(pw, 320, Pat::Test, Tag::None);
            let p709 = media(pw, 320, Pat::Noise, Tag::Bt709);
            let delivery = Some(Delivery::new(fw, fh, Fit::Blur));
            let name = format!("{pw}x320 in {fw}x{fh}");
            let seq = |a: &Asset, b: &Asset| timeline(vec![vec![clip(a, 0.5, 1.0, 0.0), clip(b, 0.5, 1.0, 0.5)]], delivery);
            for (label, tl, assets) in [
                ("709 then 601", seq(&p709, &p601), dedup(&[&p709, &p601])),
                ("601 then 709", seq(&p601, &p709), dedup(&[&p601, &p709])),
                (
                    "landscape 709 then portrait 601",
                    seq(&land709, &p601),
                    dedup(&[&land709, &p601]),
                ),
                (
                    "portrait 601 over landscape 709",
                    timeline(
                        vec![vec![clip(&land709, 0.5, 1.0, 0.0)], vec![clip(&p601, 0.5, 1.0, 0.0)]],
                        delivery,
                    ),
                    dedup(&[&land709, &p601]),
                ),
            ] {
                let case = format!("checker/b/r2/lifted fill {label} {name}");
                let out = sweep_secs(&case, &tl, &assets, 15.0, 1.0);
                if negotiates() && !same_matrix(Tag::Bt709, Tag::None) {
                    drawn_converted += out.exact();
                }
                fam.add(&case, &out);
            }
            // Dissolving into the second portrait: the dissolving clip is the one on the alpha plane that decides, and the plain
            // one before it is the converted layer over the fill (drawn); a third clip dissolving in after it, of the first's
            // matrix again, is a converted layer on the alpha plane over a fill (refused: its fill's scales convert inside).
            let two = timeline(
                vec![vec![
                    clip(&p709, 0.5, 1.0, 0.0),
                    dissolve_into(clip(&p601, 0.5, 1.0, 0.5), TransitionKind::Crossfade, 0.3),
                ]],
                delivery,
            );
            let case = format!("checker/b/r2/lifted fill dissolving {name}");
            let out = sweep_secs(&case, &two, &dedup(&[&p709, &p601]), 15.0, 1.0);
            fam.add(&case, &out);
            if pw == 180 || pw == 190 {
                let three = timeline(
                    vec![vec![
                        clip(&p601, 0.5, 1.0, 0.0),
                        dissolve_into(clip(&p709, 0.5, 1.0, 0.5), TransitionKind::Crossfade, 0.3),
                        dissolve_into(clip(&p601, 1.0, 1.5, 1.0), TransitionKind::Crossfade, 0.3),
                    ]],
                    delivery,
                );
                let case = format!("checker/b/r2/fill dissolving converted {name}");
                let assets = dedup(&[&p709, &p601]);
                let out = sweep_secs(&case, &three, &assets, 15.0, 1.5);
                fam.add(&case, &out);
                if negotiates() && out.refused_for("blurred fill") == 0 && out.refused_for("matrix") == 0 {
                    fam.failures.push(format!(
                        "{case}: a converted layer dissolving in over a fill is not refused: {:?}",
                        out.reasons
                    ));
                }
                if patched_out() {
                    let a = anyway(&case, &three, &assets, &at_fps(15.0), 22, compositor(), 3);
                    necessity.push((case, a));
                }
            }
        }
    }
    eprintln!("converted layers over a blurred fill drawn exactly: {drawn_converted} frames");
    if negotiates() {
        assert!(
            drawn_converted > 0,
            "the lift draws nothing: every converted layer over a fill is refused"
        );
    }
    fam.finish("lifted: a converted layer over a blurred fill");
    for (n, a) in necessity {
        if negotiates() && a.needed() == Some(false) {
            eprintln!("{n}: the refusal is wider than it has to be: {a:?}");
        }
    }
}

/// Invert and grayscale on a converted layer: first, second, both, with a dissolve, a crop, resized, with a colour correction or a
/// blur beside them (refused).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_lifted_table_effects_on_converted_layers() {
    use kerf_core::VideoEffect as E;
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    let a709 = media(320, 180, Pat::Edges, Tag::Bt709);
    let b601 = media(320, 180, Pat::Noise, Tag::None);
    let c2020 = media(320, 180, Pat::Test, Tag::Bt2020);
    let wide601 = media(640, 360, Pat::Test, Tag::None);
    for (ename, effect) in [("invert", E::Invert), ("grayscale", E::Grayscale)] {
        for place in [
            "second",
            "first",
            "both",
            "dissolve",
            "crop",
            "stacked",
            "over 2020",
            "shrunk",
        ] {
            let mut first = clip(&a709, 0.5, 1.2, 0.0);
            let mut second = clip(&b601, 0.5, 1.2, 0.7);
            let mut assets = dedup(&[&a709, &b601]);
            let mut tracks;
            match place {
                "second" => second.effects = vec![effect.clone()],
                "first" => first.effects = vec![effect.clone()],
                "both" => {
                    first.effects = vec![effect.clone()];
                    second.effects = vec![effect.clone()];
                }
                "dissolve" => {
                    second = dissolve_into(second, TransitionKind::Crossfade, 0.3);
                    second.effects = vec![effect.clone()];
                }
                "crop" => {
                    second.effects = vec![effect.clone()];
                    second.transform.crop_left = 0.1;
                }
                "stacked" => {
                    second = pip(clip(&b601, 0.5, 1.2, 0.0), 1.0, 0.0, 0.0);
                    second.effects = vec![effect.clone()];
                    first.source_out = 1.2;
                }
                "over 2020" => {
                    second = clip(&c2020, 0.5, 1.2, 0.7);
                    second.effects = vec![effect.clone()];
                    assets = dedup(&[&a709, &c2020]);
                }
                _ => {
                    second = clip(&wide601, 0.5, 1.2, 0.7);
                    second.effects = vec![effect.clone()];
                    assets = dedup(&[&a709, &wide601]);
                }
            }
            tracks = if place == "stacked" {
                vec![vec![first], vec![second]]
            } else {
                vec![vec![first, second]]
            };
            cases.push((
                format!("{ename} {place}"),
                timeline(std::mem::take(&mut tracks), None),
                assets,
            ));
        }
    }
    // Beside them, and refused for it.
    for (name, effects, colour) in [
        ("invert and eq", vec![E::Invert], true),
        ("blur", vec![E::Blur { sigma: 2.0 }], false),
        ("sharpen", vec![E::Sharpen { amount: 0.5 }], false),
        ("vignette", vec![E::Vignette], false),
    ] {
        let first = clip(&a709, 0.5, 1.2, 0.0);
        let mut second = clip(&b601, 0.5, 1.2, 0.7);
        second.effects = effects;
        if colour {
            second.color = Color {
                contrast: 1.2,
                ..Color::default()
            };
        }
        cases.push((
            format!("beside: {name}"),
            timeline(vec![vec![first, second]], None),
            dedup(&[&a709, &b601]),
        ));
    }
    run_family("lifted: table effects on converted layers", cases, 15.0, 1.4);
}

/// 4:2:2 and 4:4:4 converted layers: first and second of a cut, stacked, dissolving, cropped, letterboxed, resized (refused).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_lifted_422_and_444_converted_layers() {
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    let plain709 = media(320, 180, Pat::Edges, Tag::Bt709);
    let plain601 = media(320, 180, Pat::Test, Tag::None);
    let portrait601 = media(180, 320, Pat::Noise, Tag::None);
    for pix in ["yuv444p", "yuv422p"] {
        for tag in [Tag::None, Tag::Bt709, Tag::Bt2020] {
            let p = pixfmt_media(pix, tag);
            for (other_name, other) in [("709", &plain709), ("601", &plain601)] {
                if same_matrix(tag, if other_name == "709" { Tag::Bt709 } else { Tag::None }) {
                    continue;
                }
                let seq = |first_is_p: bool| {
                    if first_is_p {
                        timeline(vec![vec![clip(&p, 0.5, 1.2, 0.0), clip(other, 0.5, 1.2, 0.7)]], None)
                    } else {
                        timeline(vec![vec![clip(other, 0.5, 1.2, 0.0), clip(&p, 0.5, 1.2, 0.7)]], None)
                    }
                };
                cases.push((
                    format!("{pix} {tag:?} first, then {other_name}"),
                    seq(true),
                    dedup(&[&p, other]),
                ));
                cases.push((format!("{pix} {tag:?} after {other_name}"), seq(false), dedup(&[&p, other])));
                cases.push((
                    format!("{pix} {tag:?} over {other_name}"),
                    timeline(vec![vec![clip(other, 0.5, 1.2, 0.0)], vec![clip(&p, 0.5, 1.2, 0.0)]], None),
                    dedup(&[&p, other]),
                ));
                let mut d = clip(&p, 0.5, 1.2, 0.7);
                d = dissolve_into(d, TransitionKind::Crossfade, 0.3);
                cases.push((
                    format!("{pix} {tag:?} dissolving after {other_name}"),
                    timeline(vec![vec![clip(other, 0.5, 1.2, 0.0), d]], None),
                    dedup(&[&p, other]),
                ));
                let mut c = clip(&p, 0.5, 1.2, 0.7);
                c.transform.crop_left = 0.1;
                c.transform.crop_top = 0.2;
                cases.push((
                    format!("{pix} {tag:?} cropped after {other_name}"),
                    timeline(vec![vec![clip(other, 0.5, 1.2, 0.0), c]], None),
                    dedup(&[&p, other]),
                ));
                cases.push((
                    format!("{pix} {tag:?} pip over {other_name}"),
                    timeline(
                        vec![
                            vec![clip(other, 0.5, 1.2, 0.0)],
                            vec![pip(clip(&p, 0.5, 1.2, 0.0), 0.5, 0.1, 0.1)],
                        ],
                        None,
                    ),
                    dedup(&[&p, other]),
                ));
                cases.push((
                    format!("{pix} {tag:?} letterboxed in a portrait frame after {other_name}"),
                    timeline(
                        vec![vec![clip(&portrait601, 0.5, 1.2, 0.0), clip(&p, 0.5, 1.2, 0.7)]],
                        Some(Delivery::new(180, 320, Fit::Contain)),
                    ),
                    dedup(&[&p, &portrait601]),
                ));
            }
        }
    }
    run_family("lifted: 4:2:2 and 4:4:4 converted layers", cases, 15.0, 1.4);
}

/// A shrink up to 63:1 is drawn (exactly), 64:1 and steeper is refused (the model has no table) or exact: a 20x12 canvas
/// holding sources of `r` times its size, noise and edges, and one axis steeper than the other.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); slow: large sources"]
fn checker_b_r2_lifted_shrinks_to_63_to_1() {
    let mut fam = Family::default();
    let mut refused_names = Vec::new();
    let mut failures = Vec::new();
    for (w, h) in [(20u32, 12u32), (24, 14)] {
        for ratio in [30u32, 40, 50, 58, 62, 63, 64, 65, 70, 90] {
            for (pat, only_x) in [(Pat::Noise, false), (Pat::Edges, false), (Pat::Test, true)] {
                let (sw, sh) = if only_x {
                    (w * ratio, h * (ratio / 3).max(1))
                } else {
                    (w * ratio, h * ratio)
                };
                let a = short_media(sw, sh, pat, Tag::None);
                let tl = timeline(vec![vec![clip(&a, 0.0, 0.2, 0.0)]], Some(Delivery::new(w, h, Fit::Contain)));
                let case = format!("checker/b/r2/shrink {sw}x{sh} into {w}x{h} {pat:?}");
                let opts = at_fps(30.0);
                let out = sweep(&case, &tl, std::slice::from_ref(&a), &opts, 6);
                fam.add(&case, &out);
                let steep = ratio > 63;
                if steep && out.drawn > 0 && !only_x {
                    failures.push(format!("{case}: {ratio}:1 was drawn"));
                }
                if !steep && !only_x && out.refused_for("steeper") > 0 {
                    failures.push(format!("{case}: {ratio}:1 was refused as too steep"));
                }
                if out.refused > 0 && out.refused_for("steeper") > 0 {
                    let a = anyway(
                        &format!("{case} anyway"),
                        &tl,
                        std::slice::from_ref(&a),
                        &opts,
                        6,
                        compositor(),
                        2,
                    );
                    refused_names.push(format!(
                        "{case}: {}",
                        a.needed().map_or("-", |n| if n { "needed" } else { "WIDER THAN NEEDED" })
                    ));
                }
            }
        }
    }
    eprintln!("steep refusals: {refused_names:#?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    fam.finish("lifted: shrinks to 63:1");
}

/// A dip to white into the second clip of a cut: the frames of the first clip (before the dip, where the plan draws a plain
/// layer) against the export, for every pair of matrices including equal ones. Tells whether a mixed cut's first-clip frames
/// differ because of the matrices or because of the dip's own `rgb24` stage in the graph.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_dip_to_white_in_the_cut_leaves_the_clips_before_it_exact() {
    let mut fam = Family::default();
    for a in [Tag::None, Tag::Bt709, Tag::Bt2020] {
        for b in [Tag::None, Tag::Bt709, Tag::Bt2020] {
            for pat in [Pat::Noise, Pat::Test] {
                let (ca, aa) = routed(Route::Plain, a, pat, 0.0);
                let (cb, ab) = routed(Route::DipWhite, b, pat, 0.7);
                let tl = timeline(vec![vec![ca, cb]], None);
                let case = format!("checker/b/r2/dipwhite {pat:?} {a:?} then dip into {b:?}");
                let out = sweep_secs(&case, &tl, &dedup(&[&aa, &ab]), 15.0, 1.4);
                // (The dip's own ladder: held to 3 levels, and the same figures for equal and different matrices.)
                let worst = out.bad.iter().flat_map(|b| b.planes.iter().map(|p| p.max)).max().unwrap_or(0);
                if worst > 3 {
                    fam.failures.push(format!(
                        "{case}: a dip to white's ladder is 3 levels, drawn frames differ by {worst}"
                    ));
                }
                fam.add_known_ladder(&case, &out);
            }
        }
    }
    fam.finish("a dip to white in the cut");
}

/// A 320x180 opaque RGB PNG of noise.
fn rgb_noise_png() -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("rgb-noise.png");
    if !p.exists() {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=320x180:rate=1:duration=1,format=rgb24,geq=r='random(1)*255':g='random(2)*255':b='random(3)*255'",
            "-frames:v",
            "1",
            p.to_str().unwrap(),
        ]);
    }
    probed(&p)
}

/// Every refusal the change adds or keeps around the mixed-matrices lift, drawn anyway: `(name, whether it must refuse on a
/// negotiating build, the cut)`. A refusal whose frames, drawn anyway, are all the export's is wider than it has to be; the
/// table is printed, and the test fails only when a refusal that must exist does not (or a draw-anyway frame panics).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_refusals_are_needed() {
    use kerf_core::{BlendMode, VideoEffect as E};
    let first709 = media(320, 180, Pat::Edges, Tag::Bt709);
    let b601 = media(320, 180, Pat::Noise, Tag::None);
    let png = rgb_noise_png();
    let alpha601 = alpha_media(320, 180, Tag::None);
    let curves = grades_b().into_iter().find(|(n, _)| *n == "curves").unwrap().1;
    let mask = kerf_core::Mask {
        shape: kerf_core::MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather: 0.0,
        inverted: false,
    };
    let eq = Color {
        contrast: 1.2,
        saturation: 1.1,
        ..Color::default()
    };
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    // (name, must refuse on a negotiating build, edit of the 601 clip, whether it dissolves in (the tied route))
    let mut list: Vec<(&str, bool, Edit, bool)> = vec![
        ("tied+eq", true, Box::new(|c| c.color = eq), true),
        ("tied+grade", true, Box::new(|c| c.grade = curves.clone()), true),
        (
            "tied+blur",
            true,
            Box::new(|c| c.effects = vec![E::Blur { sigma: 2.0 }]),
            true,
        ),
        ("tied+invert", true, Box::new(|c| c.effects = vec![E::Invert]), true),
        ("tied+grayscale", true, Box::new(|c| c.effects = vec![E::Grayscale]), true),
        ("tied+opacity", true, Box::new(|c| c.transform.opacity = 0.6), true),
        ("tied+rotation", true, Box::new(|c| c.transform.rotation = 10.0), true),
        ("tied+mask", true, Box::new(|c| c.mask = Some(mask)), true),
        (
            "tied+chroma key",
            true,
            Box::new(|c| {
                c.effects = vec![E::ChromaKey {
                    color: "0x00ff00".into(),
                    similarity: 0.3,
                    blend: 0.1,
                }];
            }),
            true,
        ),
        ("blend screen", true, Box::new(|c| c.blend_mode = BlendMode::Screen), false),
        (
            "blend multiply",
            true,
            Box::new(|c| c.blend_mode = BlendMode::Multiply),
            false,
        ),
        ("plain+eq", true, Box::new(|c| c.color = eq), false),
        ("plain+grade", true, Box::new(|c| c.grade = curves.clone()), false),
        (
            "plain+blur",
            true,
            Box::new(|c| c.effects = vec![E::Blur { sigma: 2.0 }]),
            false,
        ),
        (
            "plain+sharpen",
            true,
            Box::new(|c| c.effects = vec![E::Sharpen { amount: 0.5 }]),
            false,
        ),
        ("plain+vignette", true, Box::new(|c| c.effects = vec![E::Vignette]), false),
        ("plain+opacity", true, Box::new(|c| c.transform.opacity = 0.6), false),
        ("plain+rotation", true, Box::new(|c| c.transform.rotation = 10.0), false),
        ("plain+mask", true, Box::new(|c| c.mask = Some(mask)), false),
        (
            "plain+keyed scale",
            true,
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 0.8, 0.0, 0.0)]),
            false,
        ),
        ("plain+fade_in", true, Box::new(|c| c.fade_in = 0.4), false),
        ("plain+fade_out", true, Box::new(|c| c.fade_out = 0.4), false),
        (
            "plain+dip black",
            true,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::DipToBlack, 0.4)),
            false,
        ),
        (
            "plain+dip white",
            true,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::DipToWhite, 0.4)),
            false,
        ),
        (
            "plain+dissolve",
            false,
            Box::new(|c| *c = dissolve_into(c.clone(), TransitionKind::Crossfade, 0.4)),
            false,
        ),
        ("plain+pip", false, Box::new(|c| *c = pip(c.clone(), 0.5, 0.1, 0.1)), false),
        (
            "plain+speed 2",
            false,
            Box::new(|c| {
                c.speed = 2.0;
                c.source_out = c.source_in + 2.0;
            }),
            false,
        ),
    ];
    let mut lines = Vec::new();
    let mut wider = Vec::new();
    let mut missing = Vec::new();
    for (name, must, edit, tied) in list.drain(..) {
        let mut c = clip(&b601, 0.5, 1.2, 0.7);
        if tied {
            c = dissolve_into(c, TransitionKind::Crossfade, 0.3);
        }
        edit(&mut c);
        let tl = timeline(vec![vec![clip(&first709, 0.5, 1.2, 0.0), c]], None);
        let case = format!("checker/b/r2/refusal {name}");
        let a = anyway(&case, &tl, &dedup(&[&first709, &b601]), &at_fps(15.0), 21, compositor(), 8);
        if negotiates() && must && a.refused == 0 {
            missing.push(name.to_string());
        }
        if a.needed() == Some(false) {
            wider.push(name.to_string());
        }
        lines.push(name.to_string());
    }
    // Cuts whose deciding clip is an RGB picture or whose picture is alone in a BT.709 cut, and a source with an alpha channel
    // carrying the things that are not read.
    let extra: Vec<(&str, bool, Timeline, Vec<Asset>)> = vec![
        (
            "rgb picture first, then 709 and 601 video",
            true,
            timeline(
                vec![vec![
                    clip(&png, 0.0, 0.5, 0.0),
                    clip(&first709, 0.5, 1.0, 0.5),
                    clip(&b601, 0.5, 1.0, 1.0),
                ]],
                None,
            ),
            dedup(&[&png, &first709, &b601]),
        ),
        (
            "rgb picture alone in a 709 cut",
            true,
            timeline(vec![vec![clip(&first709, 0.5, 1.0, 0.0), clip(&png, 0.0, 0.7, 0.5)]], None),
            dedup(&[&png, &first709]),
        ),
        (
            "rgb picture pip over a 709 video",
            true,
            timeline(
                vec![
                    vec![clip(&first709, 0.5, 1.2, 0.0)],
                    vec![pip(clip(&png, 0.0, 1.0, 0.0), 0.5, 0.1, 0.1)],
                ],
                None,
            ),
            dedup(&[&png, &first709]),
        ),
        (
            "alpha source + eq decides",
            true,
            timeline(
                vec![vec![clip(&first709, 0.5, 1.2, 0.0), {
                    let mut c = clip(&alpha601, 0.5, 1.2, 0.7);
                    c.color = eq;
                    c
                }]],
                None,
            ),
            dedup(&[&first709, &alpha601]),
        ),
        (
            "keyed position + fade_out decides",
            true,
            timeline(
                vec![vec![clip(&first709, 0.5, 1.2, 0.0), {
                    let mut c = clip(&b601, 0.5, 1.2, 0.7);
                    c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 1.0, 0.1, 0.05)];
                    c.fade_out = 0.3;
                    c
                }]],
                None,
            ),
            dedup(&[&first709, &b601]),
        ),
    ];
    for (name, must, tl, assets) in extra {
        let case = format!("checker/b/r2/refusal {name}");
        let a = anyway(&case, &tl, &assets, &at_fps(15.0), 21, compositor(), 8);
        if negotiates() && must && a.refused == 0 {
            missing.push(name.to_string());
        }
        if a.needed() == Some(false) {
            wider.push(name.to_string());
        }
    }
    eprintln!("== refusals wider than they have to be: {wider:?}");
    let table: Vec<String> = necessity_lines().lock().unwrap().values().cloned().collect();
    let _ = std::fs::write(
        target_dir()
            .join("parity")
            .join(format!("checker-b-necessity-{}.txt", build_key())),
        table.join("\n"),
    );
    assert!(
        missing.is_empty(),
        "refusals that no longer exist on a negotiating build: {missing:?}"
    );
}

/// A delivery that names `bicubic` writes `flags=bicubic` on the fit and zoom scales (not on the blurred fill's thumbnail, which
/// is always bilinear): drawn exactly in every fit, on the measured model and on one pinned to a bilinear default.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_scaler_named_in_every_fit() {
    use kerf_gpu::scaler::{Kernels, Model};
    use kerf_gpu::sws::Scaler;
    let Kernels::Match(real) = kerf_gpu::scaler::kernels() else {
        panic!("this FFmpeg's scaler is not a model");
    };
    let bilinear_default = pinned(Kernels::Match(Model {
        kernel: real.kernel,
        default: Scaler::Bilinear,
    }));
    let landscape = media(640, 360, Pat::Noise, Tag::None);
    let portrait = media(180, 320, Pat::Test, Tag::None);
    let odd = media(321, 181, Pat::Noise, Tag::None);
    let mut failures = Vec::new();
    let mut drawn_total = 0;
    for (fit, dw, dh) in [
        (Fit::Contain, 320, 180),
        (Fit::Cover, 320, 180),
        (Fit::Blur, 320, 180),
        (Fit::Blur, 256, 144),
        (Fit::Cover, 180, 320),
    ] {
        for (label, mut c, a) in [
            ("landscape", clip(&landscape, 0.5, 1.0, 0.0), &landscape),
            ("portrait", clip(&portrait, 0.5, 1.0, 0.0), &portrait),
            ("odd", clip(&odd, 0.5, 1.0, 0.0), &odd),
            (
                "landscape pip",
                pip(clip(&landscape, 0.5, 1.0, 0.0), 0.5, 0.1, 0.1),
                &landscape,
            ),
            ("portrait zoomed", clip(&portrait, 0.5, 1.0, 0.0), &portrait),
        ] {
            if label == "portrait zoomed" {
                c.transform.scale = 1.4;
                c.transform.crop_left = 0.1;
            }
            let tl = timeline(vec![vec![c]], Some(Delivery::new(dw, dh, fit)));
            for name in [Some("bicubic"), None] {
                let opts = ExportOptions {
                    scaler: name.map(str::to_string),
                    ..at_fps(15.0)
                };
                let case = format!("checker/b/r2/named {label} {fit:?} {dw}x{dh} {name:?}");
                let played = export(&case, &tl, std::slice::from_ref(a), &opts);
                let mut comps: Vec<(&str, &Compositor)> = vec![("measured", compositor())];
                if name.is_some() {
                    comps.push(("pinned bilinear default", &bilinear_default));
                }
                for (which, comp) in comps {
                    let played = Played {
                        w: played.w,
                        h: played.h,
                        yuv: played.yuv.clone(),
                    };
                    let out = sweep_played_on(
                        comp,
                        &format!("{case} [{which}]"),
                        &tl,
                        std::slice::from_ref(a),
                        &opts,
                        7,
                        played,
                    );
                    drawn_total += out.drawn;
                    if !out.bad.is_empty() {
                        failures.push(format!(
                            "{case} [{which}]: {} of {} drawn frames inexact",
                            out.bad.len(),
                            out.drawn
                        ));
                    }
                }
            }
        }
    }
    eprintln!("== scaler named in every fit: {drawn_total} frames drawn");
    assert!(drawn_total > 0, "nothing was drawn");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A plain clip alone on the lower track for 1.4 s with the route-taking clip of another matrix arriving on the track above at
/// 0.7 s: the frames before it show the lower clip alone, and FFmpeg 9 has converted it into whichever matrix the graph's
/// deciding clip tags (the arriving one's, when its route puts it on the alpha plane).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_arriving_on_a_higher_track() {
    let mut cases = Vec::new();
    for second in ROUTES {
        for (ta, tb) in [
            (Tag::None, Tag::Bt709),
            (Tag::Bt709, Tag::None),
            (Tag::Bt709, Tag::Bt2020),
            (Tag::Bt2020, Tag::None),
        ] {
            let (mut a, aa) = routed(Route::Plain, ta, Pat::Edges, 0.0);
            a.source_out = a.source_in + 1.4;
            let (b, ab) = routed(second, tb, Pat::Noise, 0.7);
            cases.push((
                format!("{second:?} {tb:?} arriving over {ta:?}"),
                timeline(vec![vec![a], vec![b]], None),
                dedup(&[&aa, &ab]),
            ));
        }
    }
    run_family("decider arriving on a higher track", cases, 15.0, 1.4);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_fuzz_wide_c_other_formats_and_keyed_channels() {
    FUZZ2_FORMATS.store(true, std::sync::atomic::Ordering::Relaxed);
    run_fuzz2("c", 2000..2080);
}

/// C2 again, with other decoders: the odd last row of a picture that `scale` left alone is kept by `pad` because `scale` forwards
/// the decoder's frame untouched. The decoder's frame is not the same buffer for every codec (FFV1 was the first measured), so the
/// picture comes from rawvideo, UT Video, FFV1 + Huffyuv, VP9 and MPEG-4 too, at the frame's width and not, 4:2:0.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_pad_of_odd_pictures_from_other_decoders() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let make = |codec: &str, args: &[&str], ext: &str, w: u32, h: u32| -> Option<Asset> {
        static WRITING: Mutex<()> = Mutex::new(());
        let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = dir.join(format!("pad-{codec}-{w}x{h}.{ext}"));
        if !path.exists() {
            let lavfi = format!(
                "nullsrc=size={w}x{h}:rate=30:duration=0.5,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
            );
            let tmp = dir.join(format!("pad-{codec}-{w}x{h}.tmp.{ext}"));
            let mut cmd = vec!["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &lavfi];
            cmd.extend_from_slice(args);
            cmd.extend_from_slice(&["-pix_fmt", "yuv420p", tmp.to_str().unwrap()]);
            let ok = Command::new(kerf_core::ffmpeg_path())
                .args(&cmd)
                .stdin(Stdio::null())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                let _ = std::fs::remove_file(&tmp);
                return None;
            }
            std::fs::rename(&tmp, &path).unwrap();
        }
        Some(probed(&path))
    };
    let codecs: Vec<(&str, Vec<&str>, &str)> = vec![
        ("rawvideo", vec!["-c:v", "rawvideo"], "mkv"),
        ("utvideo", vec!["-c:v", "utvideo"], "mkv"),
        ("ffvhuff", vec!["-c:v", "ffvhuff"], "mkv"),
        ("vp9", vec!["-c:v", "libvpx-vp9", "-lossless", "1"], "webm"),
        ("mpeg4", vec!["-c:v", "mpeg4", "-q:v", "1"], "mkv"),
        ("qtrle-less ffv1 slices", vec!["-c:v", "ffv1", "-slices", "4"], "mkv"),
    ];
    let mut fam = Family::default();
    let mut made = 0;
    for (codec, args, ext) in &codecs {
        for (sw, sh, fw, fh) in [
            (64, 5, 64, 36),
            (64, 9, 64, 36),
            (64, 13, 64, 36),
            (96, 7, 96, 54),
            (62, 9, 64, 36),
            (64, 31, 64, 36),
            // One row short of the frame: `pad` has no top offset to leave room for, and pads in place where the decoder's frame
            // is writable and has the slack (`vf_pad.c`'s `frame_needs_copy`), which drops the odd last row.
            (64, 35, 64, 36),
            (96, 53, 96, 54),
            (62, 35, 64, 36),
        ] {
            let Some(a) = make(codec, args, ext, sw, sh) else {
                eprintln!("{codec} {sw}x{sh}: this FFmpeg cannot make it");
                continue;
            };
            made += 1;
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            let case = format!("checker/b/r2/pad {codec} {sw}x{sh} in {fw}x{fh}");
            // (A file the frame source will not seek into — a codec this FFmpeg does not flag keyframes for — is not a case.)
            let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sweep_try(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3)
            }));
            match attempt {
                Ok(Some(out)) => fam.add(&case, &out),
                Ok(None) => {}
                Err(e) => {
                    let text = e
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(ToString::to_string))
                        .unwrap_or_default();
                    if text.contains("does not decode from a keyframe") {
                        eprintln!("{case}: the frame source will not seek into this file; skipped");
                    } else {
                        std::panic::resume_unwind(e);
                    }
                }
            }
        }
    }
    assert!(made > 6, "too few codecs could make the pictures: {made}");
    fam.finish("pad of odd pictures from other decoders");
}

/// The Still plan decides the composite's matrix over the layers on screen: the route-taking clip over a plain clip of another
/// matrix, at times inside and after its transitions, for every route; a dissolve between two clips of two matrices mid-way.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_still_decider() {
    let mut fam = Family::default();
    for second in ROUTES {
        for (ta, tb) in [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None), (Tag::Bt709, Tag::Bt2020)] {
            let (mut a, aa) = routed(Route::Plain, ta, Pat::Edges, 0.0);
            a.source_out = a.source_in + 1.4;
            let (b, ab) = routed(second, tb, Pat::Noise, 0.0);
            // (Not a pip: a layer a still's last `scale` resizes is refused for its conversion, which says nothing of the decider.)
            let tl = timeline(vec![vec![a], vec![b]], None);
            let case = format!("checker/b/r2/still {second:?} {tb:?} over {ta:?}");
            let out = still_sweep(&case, &tl, &dedup(&[&aa, &ab]), &[0.1, 0.35, 0.6]);
            if second == Route::DipWhite {
                let worst = out.bad.iter().map(|b| b.2).max().unwrap_or(0);
                if worst > 3 {
                    fam.failures.push(format!(
                        "{case}: a dip to white's ladder is 3 levels, drawn stills differ by {worst}"
                    ));
                }
                fam.cases += 1;
                fam.drawn += out.drawn;
                fam.refused += out.refused;
            } else {
                fam.add_still(&case, &out);
            }
        }
    }
    fam.finish("still decider");
}

/// Clips that open the same file at the same point share one decoded input, fanned out with a `split` that is the first filter
/// of the graph: the decider is read past it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_with_a_shared_input() {
    let a709 = media(320, 180, Pat::Noise, Tag::Bt709);
    let b601 = media(320, 180, Pat::Edges, Tag::None);
    let c2020 = media(320, 180, Pat::Test, Tag::Bt2020);
    let assets = dedup(&[&a709, &b601, &c2020]);
    let keyed = |mut c: Clip| {
        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 1.0, 0.1, 0.05)];
        c
    };
    let cases: Vec<(String, Timeline, Vec<Asset>)> = vec![
        (
            "709 twice (shared), 601 between".into(),
            timeline(
                vec![
                    vec![clip(&a709, 0.5, 1.2, 0.0), clip(&b601, 0.5, 1.2, 0.7)],
                    vec![pip(clip(&a709, 0.5, 1.2, 0.0), 0.5, 0.1, 0.1)],
                ],
                None,
            ),
            assets.clone(),
        ),
        (
            "601 plain then 709 dissolving, 709 shared as a pip".into(),
            timeline(
                vec![
                    vec![
                        clip(&b601, 0.5, 1.2, 0.0),
                        dissolve_into(clip(&a709, 0.5, 1.2, 0.7), TransitionKind::Crossfade, 0.3),
                    ],
                    vec![pip(clip(&a709, 0.5, 1.2, 0.7), 0.5, 0.1, 0.1)],
                ],
                None,
            ),
            assets.clone(),
        ),
        (
            "709 plain then 601, 601 shared as a keyed position above".into(),
            timeline(
                vec![
                    vec![clip(&a709, 0.5, 1.2, 0.0), clip(&b601, 0.5, 1.2, 0.7)],
                    vec![keyed(clip(&b601, 0.5, 1.2, 0.7))],
                ],
                None,
            ),
            assets.clone(),
        ),
        (
            "three matrices, 2020 shared by a dissolve and a plain pip".into(),
            timeline(
                vec![
                    vec![
                        clip(&a709, 0.5, 1.2, 0.0),
                        dissolve_into(clip(&c2020, 0.5, 1.2, 0.7), TransitionKind::Crossfade, 0.3),
                    ],
                    vec![pip(clip(&c2020, 0.5, 1.2, 0.7), 0.4, -0.2, -0.2)],
                    vec![pip(clip(&b601, 0.5, 1.2, 0.0), 0.3, 0.2, 0.2)],
                ],
                None,
            ),
            assets,
        ),
    ];
    run_family("decider with a shared input", cases, 15.0, 1.4);
}

/// Just outside each lift: table effects on a converted layer that leaves bars (letterboxed, or over a blurred fill), 4:2:2 /
/// 4:4:4 converted layers with bars, over a fill, or of odd size, a held key beside a moving one. Every drawn frame is exact; the
/// refusals are in the log.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_lifted_edges() {
    use kerf_core::VideoEffect as E;
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    let land709 = media(320, 180, Pat::Edges, Tag::Bt709);
    let portrait601 = media(180, 320, Pat::Noise, Tag::None);
    let portrait709 = media(180, 320, Pat::Test, Tag::Bt709);
    for (ename, effect) in [("invert", E::Invert), ("grayscale", E::Grayscale)] {
        for fit in [Fit::Contain, Fit::Blur] {
            for (pname, first, second) in [
                ("601 portrait after 709 landscape", &land709, &portrait601),
                ("709 portrait after 601 portrait", &portrait601, &portrait709),
            ] {
                let mut c = clip(second, 0.5, 1.2, 0.7);
                c.effects = vec![effect.clone()];
                cases.push((
                    format!("{ename} on {pname}, {fit:?}"),
                    timeline(vec![vec![clip(first, 0.5, 1.2, 0.0), c]], Some(Delivery::new(320, 180, fit))),
                    dedup(&[first, second]),
                ));
            }
        }
    }
    for pix in ["yuv444p", "yuv422p"] {
        let p = pixfmt_media(pix, Tag::None);
        for (fw, fh, fit) in [(320, 240, Fit::Contain), (320, 240, Fit::Blur), (320, 180, Fit::Contain)] {
            cases.push((
                format!("{pix} 601 at the frame's width in {fw}x{fh} {fit:?} after 709"),
                timeline(
                    vec![vec![clip(&land709, 0.5, 1.2, 0.0), clip(&p, 0.5, 1.2, 0.7)]],
                    Some(Delivery::new(fw, fh, fit)),
                ),
                dedup(&[&land709, &p]),
            ));
        }
        let odd = {
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("odd-{pix}.mkv"));
            static WRITING: Mutex<()> = Mutex::new(());
            let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !path.exists() {
                let lavfi = format!(
                    "nullsrc=size=321x181:rate=30:duration=2,format={pix},geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
                );
                ffmpeg(&[
                    "-f",
                    "lavfi",
                    "-i",
                    &lavfi,
                    "-c:v",
                    "ffv1",
                    "-pix_fmt",
                    pix,
                    path.to_str().unwrap(),
                ]);
            }
            probed(&path)
        };
        cases.push((
            format!("{pix} odd 321x181 after 709"),
            timeline(
                vec![vec![clip(&land709, 0.5, 1.2, 0.0), clip(&odd, 0.5, 1.2, 0.7)]],
                Some(Delivery::new(320, 180, Fit::Contain)),
            ),
            dedup(&[&land709, &odd]),
        ));
    }
    // A held key beside a moving one, in both orders and tag pairs.
    for (ta, tb) in [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None)] {
        let (a, aa) = routed(Route::HeldKey, ta, Pat::Edges, 0.0);
        let (b, ab) = routed(Route::KeyedPos, tb, Pat::Noise, 0.7);
        cases.push((
            format!("held {ta:?} then moving {tb:?}"),
            timeline(vec![vec![a, b]], None),
            dedup(&[&aa, &ab]),
        ));
        let (a, aa) = routed(Route::KeyedPos, ta, Pat::Edges, 0.0);
        let (b, ab) = routed(Route::HeldKey, tb, Pat::Noise, 0.7);
        cases.push((
            format!("moving {ta:?} then held {tb:?}"),
            timeline(vec![vec![a, b]], None),
            dedup(&[&aa, &ab]),
        ));
    }
    run_family("lifted edges", cases, 15.0, 1.4);
}

/// The blurred fill's thumbnail shrink at the 63:1 line (`BlurFill::resolve`'s cover against the source, per axis): a 64x64 frame's
/// 8x8 thumbnail covers a 16:9 picture with 14x8, a 9:16 one with 8x14, so sources of 882x496 and 496x882 are 63.0:1 and the next
/// size up is refused. Each side of the line, landscape and portrait, two patterns; the drawn frames are exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); slow: large sources"]
fn checker_b_r2_fill_thumbnail_at_the_63_line() {
    let mut fam = Family::default();
    let mut lines = Vec::new();
    for w in [840u32, 880, 882, 883, 884, 900, 1000, 1200] {
        let h = (w * 9 + 8) / 16;
        for (sw, sh) in [(w, h), (h, w)] {
            for pat in [Pat::Noise, Pat::Test] {
                let a = short_media(sw, sh, pat, Tag::None);
                let tl = timeline(vec![vec![clip(&a, 0.0, 0.2, 0.0)]], blurred(64, 64));
                let case = format!("checker/b/r2/fillline {sw}x{sh} {pat:?}");
                let out = sweep(&case, &tl, std::slice::from_ref(&a), &at_fps(30.0), 6);
                lines.push(format!("{sw}x{sh}: {} drawn, {} refused", out.drawn, out.refused));
                fam.add(&case, &out);
            }
        }
    }
    eprintln!("fill thumbnail at the 63:1 line:\n{}", lines.join("\n"));
    fam.finish_refusing("fill thumbnail at the 63 line");
}

/// The routes onto the alpha plane that nothing in the chain is read to disturb, each with one more thing in the chain that is
/// not read as a converter: a clip's own fades, a static crop or zoom, a speed change, a reversal, a held key, a pip, in both tag
/// orders and as the first or the second clip of the cut.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_route_with_modifiers() {
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    for route in [Route::Crossfade, Route::KeyedPos, Route::AlphaSource, Route::KeyedScale] {
        for modifier in [
            "fade_in",
            "fade_out",
            "both fades",
            "crop",
            "speed 2",
            "reverse",
            "static zoom",
            "pip",
            "dip black before",
            "slide before",
        ] {
            for (ta, tb) in [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None)] {
                for first in [false, true] {
                    let (mut m, am) = routed(route, tb, Pat::Noise, if first { 0.0 } else { 0.7 });
                    match modifier {
                        "fade_in" => m.fade_in = 0.3,
                        "fade_out" => m.fade_out = 0.3,
                        "both fades" => {
                            m.fade_in = 0.2;
                            m.fade_out = 0.2;
                        }
                        "crop" => {
                            m.transform.crop_right = 0.1;
                            m.transform.crop_top = 0.1;
                        }
                        "speed 2" => {
                            m.speed = 2.0;
                            m.source_out = m.source_in + 1.4;
                        }
                        "reverse" => m.speed = -1.0,
                        "static zoom" => m.transform.scale = 1.2,
                        "pip" => m = pip(m, 0.6, 0.1, 0.1),
                        _ => {}
                    }
                    let (mut other, ao) = routed(Route::Plain, ta, Pat::Edges, if first { 0.7 } else { 0.0 });
                    match modifier {
                        "dip black before" => other.fade_out = 0.3,
                        "slide before" => other = dissolve_into(other, TransitionKind::SlideLeft, 0.3),
                        _ => {}
                    }
                    let clips = if first { vec![m, other] } else { vec![other, m] };
                    cases.push((
                        format!(
                            "{route:?} {tb:?} {} with {modifier} vs plain {ta:?}",
                            if first { "first" } else { "second" }
                        ),
                        timeline(vec![clips], None),
                        dedup(&[&am, &ao]),
                    ));
                }
            }
        }
    }
    run_family("decider routes with modifiers", cases, 15.0, 1.4);
}

/// `pad`'s odd last row and column once more: pictures that fit the frame untouched (odd widths and heights at the frame's own
/// size on one axis or both), and pictures of other formats, which `scale` converts and so resamples.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_pad_odd_pictures_in_other_widths_and_formats() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let make = |pix: &str, w: u32, h: u32| -> Asset {
        static WRITING: Mutex<()> = Mutex::new(());
        let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = dir.join(format!("padfmt-{pix}-{w}x{h}.mkv"));
        if !path.exists() {
            let base = if pix == "gray" { "yuv420p" } else { pix };
            let lavfi = format!(
                "nullsrc=size={w}x{h}:rate=30:duration=0.5,format={base},geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
            );
            let tmp = dir.join(format!("padfmt-{pix}-{w}x{h}.tmp.mkv"));
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
    };
    let mut fam = Family::default();
    // (Gray is left to `checker_b_r2_resized_gray_and_ten_bit_pictures`: an odd gray picture is resized into the frame there.)
    for pix in ["yuv420p", "yuv444p", "yuv422p"] {
        for (sw, sh, fw, fh) in [
            (63, 36, 64, 36),
            (63, 35, 64, 36),
            (61, 35, 64, 36),
            (64, 35, 64, 36),
            (64, 34, 64, 36),
            (33, 18, 36, 18),
            (35, 17, 36, 18),
            (64, 5, 64, 36),
            (64, 9, 64, 36),
            (63, 5, 64, 36),
            (5, 36, 64, 36),
            (9, 35, 64, 36),
        ] {
            let a = make(pix, sw, sh);
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            let case = format!("checker/b/r2/padfmt {pix} {sw}x{sh} in {fw}x{fh}");
            if let Some(out) = sweep_try(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("pad odd pictures in other widths and formats");
}

/// One frame of `plan` drawn at `size`, or why the compositor will not (a frame the plan refuses is FFmpeg's: a diagnostic then has
/// nothing of the GPU's to print, unless a run with `KERF_CHECKER_ALLOW` and the scratch patch lets it draw).
fn drawn_or_refused(plan: &kerf_core::RenderPlan, size: (u32, u32)) -> Result<kerf_gpu::YuvFrame, String> {
    let mut motion = MotionFrames::new(frame_source());
    let picture = motion.frames(plan).map_err(|e| e.to_string())?;
    compositor()
        .composite_yuv_counted(plan, &picture, motion.counts(), size)
        .map_err(|e| e.to_string())
}

/// Diagnostic for the odd last row: prints the rows around the picture's edge of FFmpeg's export and of the GPU's frame.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_pad_rows_at_the_picture_edge() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    for (sw, sh, fw, fh) in [(64u32, 35u32, 64u32, 36u32), (64, 13, 64, 36), (63, 35, 64, 36)] {
        let path = dir.join(format!("padrows-noise-{sw}x{sh}.mkv"));
        if !path.exists() {
            let lavfi = format!(
                "nullsrc=size={sw}x{sh}:rate=30:duration=0.5,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
            );
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &lavfi,
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv420p",
                path.to_str().unwrap(),
            ]);
        }
        let a = probed(&path);
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
        let opts = at_fps(30.0);
        let played = export("padrows", &tl, std::slice::from_ref(&a), &opts);
        let planner = Planner::new(&tl, std::slice::from_ref(&a), &opts, PlanRequest::motion(policy())).unwrap();
        let plan = planner.at_frame(0).unwrap();
        let size = (played.w as u32, played.h as u32);
        let want = played.planes(0);
        eprintln!(
            "{sw}x{sh} in {fw}x{fh}: reasons {:?}",
            plan.reasons(&compositor().caps(), size)
        );
        let drawn = match drawn_or_refused(&plan, size) {
            Ok(drawn) => drawn,
            Err(e) => {
                eprintln!("  refused, nothing of the GPU's to print: {e}");
                continue;
            }
        };
        let w = played.w;
        for y in (0..played.h).filter(|y| *y + 6 >= sh as usize + (fh as usize - sh as usize) / 2 && *y < played.h) {
            eprintln!(
                "  Y row {y:>2}: ffmpeg {:?} gpu {:?}",
                &want[0][y * w..y * w + 4],
                &drawn.y[y * w..y * w + 4]
            );
        }
        let cw = w / 2;
        for y in (played.h / 2).saturating_sub(4)..played.h / 2 {
            eprintln!(
                "  U row {y:>2}: ffmpeg {:?} gpu {:?}",
                &want[1][y * cw..y * cw + 4],
                &drawn.u[y * cw..y * cw + 4]
            );
        }
    }
}

/// Diagnostic: the same frame drawn repeatedly against the export, for the 64x35 yuv420p picture in a 64x36 frame that differed
/// in some runs and not in others: is the GPU's draw repeatable, and is it the export's?
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_pad_64x35_is_repeatable() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("padfmt-yuv420p-64x35.mkv");
    if !path.exists() {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=64x35:rate=30:duration=0.5,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ]);
    }
    let a = probed(&path);
    let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(64, 36, Fit::Contain)));
    let opts = at_fps(30.0);
    let played = export("pad64x35", &tl, std::slice::from_ref(&a), &opts);
    let planner = Planner::new(&tl, std::slice::from_ref(&a), &opts, PlanRequest::motion(policy())).unwrap();
    let size = (played.w as u32, played.h as u32);
    let mut summary = Vec::new();
    for k in 0..played.count().min(9) as u64 {
        let plan = planner.at_frame(k).unwrap();
        let want = played.planes(k as usize);
        let mut results = Vec::new();
        for _ in 0..6 {
            let drawn = match drawn_or_refused(&plan, size) {
                Ok(drawn) => drawn,
                Err(e) => {
                    summary.push(format!("frame {k}: refused ({e})"));
                    break;
                }
            };
            let row34_gpu = drawn.y[34 * 64..35 * 64].to_vec();
            let row34_ffmpeg = want[0][34 * 64..35 * 64].to_vec();
            results.push(diff_plane(want[0], &drawn.y, 64).differing);
            if k == 1 && results.len() == 1 {
                eprintln!("frame 1 row 34: ffmpeg {:?}", &row34_ffmpeg[..8]);
                eprintln!("frame 1 row 34: gpu    {:?}", &row34_gpu[..8]);
                eprintln!(
                    "frame 0 row 34 source-ish: ffmpeg frame0 {:?}",
                    &played.planes(0)[0][34 * 64..34 * 64 + 8]
                );
            }
        }
        if results.len() == 6 {
            summary.push(format!("frame {k}: differing Y samples over 6 draws {results:?}"));
        }
    }
    eprintln!("{}", summary.join("\n"));
}

/// Diagnostic: where a gray odd-by-odd picture letterboxed differs from the export.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_pad_gray_where_it_differs() {
    for (pix, sw, sh, fw, fh) in [
        ("gray", 63u32, 35u32, 64u32, 36u32),
        ("gray", 9, 35, 64, 36),
        ("gray", 63, 36, 64, 36),
    ] {
        let a = scratch_media("graydiag", pix, sw, sh);
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
        let opts = at_fps(30.0);
        let played = export(&format!("padgray-{sw}x{sh}"), &tl, std::slice::from_ref(&a), &opts);
        let planner = Planner::new(&tl, std::slice::from_ref(&a), &opts, PlanRequest::motion(policy())).unwrap();
        let plan = planner.at_frame(0).unwrap();
        let size = (played.w as u32, played.h as u32);
        let want = played.planes(0);
        let w = played.w;
        eprintln!(
            "{pix} {sw}x{sh} in {fw}x{fh}: reasons {:?}",
            plan.reasons(&compositor().caps(), size)
        );
        let drawn = match drawn_or_refused(&plan, size) {
            Ok(drawn) => drawn,
            Err(e) => {
                eprintln!("  refused, nothing of the GPU's to print: {e}");
                continue;
            }
        };
        let mut rows = Vec::new();
        for y in 0..played.h {
            let n = (0..w).filter(|x| want[0][y * w + x] != drawn.y[y * w + x]).count();
            if n > 0 {
                let x = (0..w).find(|x| want[0][y * w + x] != drawn.y[y * w + x]).unwrap();
                rows.push(format!(
                    "row {y}: {n} differ, first x {x}: ffmpeg {} gpu {}",
                    want[0][y * w + x],
                    drawn.y[y * w + x]
                ));
            }
        }
        eprintln!("  {}", rows.join("\n  "));
        eprintln!("  U/V planes differ: {} {}", want[1] != &drawn.u[..], want[2] != &drawn.v[..]);
        eprintln!("  first row ffmpeg {:?}", &want[0][..12.min(w)]);
        eprintln!("  first row gpu    {:?}", &drawn.y[..12.min(w)]);
    }
}

/// Gray and 10-bit 4:2:0 pictures are refused when resized (`FormatResize`: the exact scaler is proven for planar 8-bit 4:2:0
/// only; FFmpeg scales a gray picture in gray and converts to 4:2:0 after, and a 10-bit one in 10 bits and dithers once at the
/// end). Every frame of every case is FFmpeg's: none is drawn, and each is refused for the format.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_resized_gray_and_ten_bit_pictures() {
    let mut fam = Family::default();
    for (pix, sw, sh) in [
        ("gray", 640u32, 360u32),
        ("gray", 320, 180),
        ("gray", 63, 35),
        ("yuv420p10le", 640, 360),
        ("yuv420p10le", 320, 180),
        ("yuv420p10le", 63, 35),
    ] {
        let a = formats_sized(pix, sw, sh);
        for (label, tl) in resize_cuts(&a) {
            let case = format!("checker/b/r2/resized {pix} {sw}x{sh} {label}");
            if let Some(out) = sweep_try(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                refused_for_format_resize(&mut fam, &case, &out);
            }
        }
    }
    fam.finish_refusing("resized gray and 10-bit pictures are refused");
}

/// The cuts that make the plan resize a clip: a picture-in-picture, a letterbox, a cover, a zoom and an enlargement.
fn resize_cuts(a: &Asset) -> Vec<(&'static str, Timeline)> {
    vec![
        (
            "pip 0.5",
            timeline(vec![vec![pip(clip(a, 0.0, 0.3, 0.0), 0.5, 0.1, 0.1)]], None),
        ),
        (
            "contain 256x144",
            timeline(
                vec![vec![clip(a, 0.0, 0.3, 0.0)]],
                Some(Delivery::new(256, 144, Fit::Contain)),
            ),
        ),
        (
            "cover 180x320",
            timeline(vec![vec![clip(a, 0.0, 0.3, 0.0)]], Some(Delivery::new(180, 320, Fit::Cover))),
        ),
        (
            "zoom 1.3",
            timeline(
                vec![vec![{
                    let mut c = clip(a, 0.0, 0.3, 0.0);
                    c.transform.scale = 1.3;
                    c
                }]],
                None,
            ),
        ),
        (
            "enlarge into 800x450",
            timeline(
                vec![vec![clip(a, 0.0, 0.3, 0.0)]],
                Some(Delivery::new(800, 450, Fit::Contain)),
            ),
        ),
    ]
}

/// The reason `FormatResize` gives, as the plan spells it.
const FORMAT_RESIZE: &str = "not planar 8-bit 4:2:0";

/// A case whose every frame must be refused for the picture's format (a run that patched the refusal out to draw the frames
/// anyway only reports what they would have been, in the sweep's line).
fn refused_for_format_resize(fam: &mut Family, case: &str, out: &Outcome) {
    fam.cases += 1;
    fam.drawn += out.drawn;
    fam.refused += out.refused;
    if patched_out() {
        return;
    }
    if out.drawn != 0 || out.refused_for(FORMAT_RESIZE) != out.frames {
        fam.failures.push(format!(
            "{case}: {} of {} frames drawn, {} refused for the format (every frame must be refused for it): {:?}",
            out.drawn,
            out.frames,
            out.refused_for(FORMAT_RESIZE),
            out.reasons
        ));
    }
}

/// Noise in `pix` at `w` x `h` under a scratch name of the caller's own (`family`), so that no other family's test writes or
/// reads the file: FFV1 in Matroska, written to a temporary name and renamed under a lock.
fn scratch_media(family: &str, pix: &str, w: u32, h: u32) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
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

/// Noise in `pix` at `w` x `h` (FFV1 in Matroska).
fn formats_sized(pix: &str, w: u32, h: u32) -> Asset {
    scratch_media("sized", pix, w, h)
}

/// A clip on the alpha plane (a source with an alpha channel, a crossfade in, a keyed position) that also carries a white fade
/// (a dip to white into it, or out of it into the next clip): `fade c=white` works on RGB, so a converter sits in the chain and
/// the clip is not the one that decides. Found by the wide fuzz (seed 2041 of `c`): an alpha source with a dip to white in.
/// The dip's own ladder (3 levels, `DIP_LADDER`) is allowed for the dip's frames; a whole-plane mismatch is a wrong matrix.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_on_the_plane_with_a_white_fade() {
    const DIP_LADDER: i32 = 3;
    let mut fam = Family::default();
    let mut ladder_failures = Vec::new();
    for (t1, t2, t3) in [
        (Tag::None, Tag::Bt709, Tag::None),
        (Tag::Bt709, Tag::None, Tag::Bt709),
        (Tag::None, Tag::Bt709, Tag::Bt709),
        (Tag::Bt709, Tag::None, Tag::None),
        (Tag::None, Tag::Bt709, Tag::Bt2020),
    ] {
        let p1 = media(320, 180, Pat::Edges, t1);
        let p3 = media(320, 180, Pat::Noise, t3);
        let alpha2 = alpha_media(320, 180, t2);
        let plain2 = media(320, 180, Pat::Test, t2);
        let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
        // 1. the alpha source dips to white in
        cases.push((
            "alpha source dipping to white in".into(),
            timeline(
                vec![vec![
                    clip(&p1, 0.5, 1.2, 0.0),
                    dissolve_into(clip(&alpha2, 0.5, 1.2, 0.7), TransitionKind::DipToWhite, 0.3),
                ]],
                None,
            ),
            dedup(&[&p1, &alpha2]),
        ));
        // 2. the alpha source is the one before a dip to white
        cases.push((
            "alpha source before a dip to white".into(),
            timeline(
                vec![vec![
                    clip(&p1, 0.5, 1.2, 0.0),
                    clip(&alpha2, 0.5, 1.2, 0.7),
                    dissolve_into(clip(&p3, 0.5, 1.2, 1.4), TransitionKind::DipToWhite, 0.3),
                ]],
                None,
            ),
            dedup(&[&p1, &alpha2, &p3]),
        ));
        // 3. a crossfade-in clip before a dip to white
        cases.push((
            "crossfade clip before a dip to white".into(),
            timeline(
                vec![vec![
                    clip(&p1, 0.5, 1.2, 0.0),
                    dissolve_into(clip(&plain2, 0.5, 1.2, 0.7), TransitionKind::Crossfade, 0.3),
                    dissolve_into(clip(&p3, 0.5, 1.2, 1.4), TransitionKind::DipToWhite, 0.3),
                ]],
                None,
            ),
            dedup(&[&p1, &plain2, &p3]),
        ));
        // 4. a keyed position before a dip to white
        cases.push((
            "keyed position before a dip to white".into(),
            timeline(
                vec![vec![
                    clip(&p1, 0.5, 1.2, 0.0),
                    {
                        let mut c = clip(&plain2, 0.5, 1.2, 0.7);
                        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 1.0, 0.1, 0.05)];
                        c
                    },
                    dissolve_into(clip(&p3, 0.5, 1.2, 1.4), TransitionKind::DipToWhite, 0.3),
                ]],
                None,
            ),
            dedup(&[&p1, &plain2, &p3]),
        ));
        // 5. the alpha source dips to white in and out
        cases.push((
            "alpha source between two dips to white".into(),
            timeline(
                vec![vec![
                    clip(&p1, 0.5, 1.2, 0.0),
                    dissolve_into(clip(&alpha2, 0.5, 1.2, 0.7), TransitionKind::DipToWhite, 0.3),
                    dissolve_into(clip(&p3, 0.5, 1.2, 1.4), TransitionKind::DipToWhite, 0.3),
                ]],
                None,
            ),
            dedup(&[&p1, &alpha2, &p3]),
        ));
        // 6. the alpha source dips to white in, two plain clips ahead of it (so the frames before the dip show a clip that is
        // neither the outgoing one nor the dissolving one)
        cases.push((
            "alpha source dipping to white in after two plain clips".into(),
            timeline(
                vec![vec![
                    clip(&p1, 0.5, 1.2, 0.0),
                    clip(&p3, 0.5, 1.2, 0.7),
                    dissolve_into(clip(&alpha2, 0.5, 1.2, 1.4), TransitionKind::DipToWhite, 0.3),
                ]],
                None,
            ),
            dedup(&[&p1, &p3, &alpha2]),
        ));
        // 7. the same with a crossfade clip dipping in
        cases.push((
            "a dip to white two clips after a crossfade one".into(),
            timeline(
                vec![vec![
                    clip(&p1, 0.5, 1.2, 0.0),
                    dissolve_into(clip(&plain2, 0.5, 1.2, 0.7), TransitionKind::Crossfade, 0.3),
                    clip(&p3, 0.5, 1.2, 1.4),
                    dissolve_into(clip(&p1, 1.2, 1.9, 2.1), TransitionKind::DipToWhite, 0.3),
                ]],
                None,
            ),
            dedup(&[&p1, &plain2, &p3]),
        ));
        for (name, tl, assets) in cases {
            let case = format!("checker/b/r2/whitefade {name} {t1:?}/{t2:?}/{t3:?}");
            if let Some(out) = sweep_try(&case, &tl, &assets, 15.0, 2.8) {
                // The ladder of a dip is a few levels over a plane; a wrong matrix is tens of levels over all of it.
                let gross = out
                    .bad
                    .iter()
                    .flat_map(|b| (0..3).map(move |i| (b.planes[i].max, b.planes[i].differing * 100 / b.len[i].max(1))))
                    .filter(|&(max, percent)| max > DIP_LADDER && percent > 4)
                    .max();
                if let Some((max, percent)) = gross {
                    ladder_failures.push(format!(
                        "{case}: drawn frames differ by up to {max} levels over {percent} % of a plane"
                    ));
                }
                fam.add_known_ladder(&case, &out);
            }
        }
    }
    fam.failures.extend(ladder_failures);
    fam.finish("the deciding clip with a white fade");
}

/// The clip that decides, followed by a clip that enters through each kind of transition: the outgoing side of a dissolve, a dip
/// to black, a slide and a push (the outgoing clip moves away, or keeps its tail under the next) is a part of its chain too.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_outgoing_transitions() {
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    for route in [
        Route::Crossfade,
        Route::KeyedPos,
        Route::AlphaSource,
        Route::HeldKey,
        Route::Plain,
    ] {
        for kind in [
            TransitionKind::Crossfade,
            TransitionKind::DipToBlack,
            TransitionKind::SlideLeft,
            TransitionKind::SlideDown,
            TransitionKind::PushUp,
            TransitionKind::PushRight,
        ] {
            for (t1, t2, t3) in [
                (Tag::None, Tag::Bt709, Tag::None),
                (Tag::Bt709, Tag::None, Tag::Bt709),
                (Tag::None, Tag::Bt709, Tag::Bt2020),
            ] {
                let (a, aa) = routed(Route::Plain, t1, Pat::Edges, 0.0);
                let (x, ax) = routed(route, t2, Pat::Noise, 0.7);
                let (y, ay) = routed(Route::Plain, t3, Pat::Test, 1.4);
                let y = dissolve_into(y, kind, 0.3);
                cases.push((
                    format!("{route:?} {t2:?} then {kind:?} into {t3:?} after {t1:?}"),
                    timeline(vec![vec![a, x, y]], None),
                    dedup(&[&aa, &ax, &ay]),
                ));
            }
        }
    }
    run_family("decider with the outgoing side of a transition", cases, 15.0, 2.1);
}

/// A caption over a cut of mixed matrices: the text is drawn after the overlay chain, on whichever matrix the cut's deciding clip
/// sets (the frames before that clip, converted into it, carry the text in it too). Coloured text and a box, over the plain and the
/// dissolving arrangement, in both tag orders.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_text_over_a_cut_of_mixed_matrices() {
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    for (ta, tb) in [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None), (Tag::Bt709, Tag::Bt2020)] {
        for route in [Route::Plain, Route::Crossfade, Route::AlphaSource, Route::KeyedPos] {
            let (a, aa) = routed(Route::Plain, ta, Pat::Test, 0.0);
            let (b, ab) = routed(route, tb, Pat::Edges, 0.7);
            let mut tl = timeline(vec![vec![a, b]], None);
            for (i, (color, bg)) in [("red", None), ("yellow", Some("blue@0.6")), ("#20c0ff", Some("black"))]
                .into_iter()
                .enumerate()
            {
                let mut t = kerf_core::TextOverlay::new(format!("caption {i}"), 0.1 + 0.4 * i as f64, 0.5 + 0.4 * i as f64);
                t.color = color.to_string();
                t.bg = bg.map(str::to_string);
                t.pos_y = 0.2 + 0.3 * i as f64;
                tl.overlays.push(t);
            }
            cases.push((format!("{route:?} {tb:?} after {ta:?}"), tl, dedup(&[&aa, &ab])));
        }
    }
    run_family("text over a cut of mixed matrices", cases, 15.0, 1.4);
}

/// The routes onto the alpha plane with the other things a chain can hold that are not read as a converter: a keyed crop, a speed
/// ramp, a keyed zoom held beside a static one, a source with a different frame rate, a head trim into the picture, in both orders.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r2_decider_route_with_keyed_crop_ramp_and_rates() {
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    let channel = |prop: kerf_core::Property, a: f64, b: f64| kerf_core::PropertyTrack {
        prop,
        keys: [(0.0, a), (0.7, b)]
            .iter()
            .map(|&(t, v)| kerf_core::PropertyKey::new(t, v))
            .collect(),
    };
    for route in [Route::Crossfade, Route::KeyedPos, Route::AlphaSource] {
        for modifier in [
            "keyed crop",
            "keyed crop x",
            "speed ramp",
            "scale channel held",
            "keyed mask-free opacity at 1",
            "late in point",
        ] {
            for (ta, tb) in [(Tag::None, Tag::Bt709), (Tag::Bt709, Tag::None)] {
                for first in [false, true] {
                    let (mut m, am) = routed(route, tb, Pat::Noise, if first { 0.0 } else { 0.7 });
                    match modifier {
                        "keyed crop" => m.channels.push(channel(kerf_core::Property::CropZoom, 1.0, 1.5)),
                        "keyed crop x" => {
                            m.channels.push(channel(kerf_core::Property::CropZoom, 1.4, 1.4));
                            m.channels.push(channel(kerf_core::Property::CropX, -0.5, 0.5));
                        }
                        "speed ramp" => {
                            m.speed_ramp = vec![kerf_core::SpeedKey::new(0.0, 1.0), kerf_core::SpeedKey::new(0.7, 1.6)];
                        }
                        "scale channel held" => m.channels.push(channel(kerf_core::Property::Scale, 0.9, 0.9)),
                        "keyed mask-free opacity at 1" => m.channels.push(channel(kerf_core::Property::Opacity, 1.0, 1.0)),
                        _ => {
                            m.source_in = 2.0;
                            m.source_out = 2.7;
                        }
                    }
                    let (other, ao) = routed(Route::Plain, ta, Pat::Edges, if first { 0.7 } else { 0.0 });
                    let clips = if first { vec![m, other] } else { vec![other, m] };
                    cases.push((
                        format!(
                            "{route:?} {tb:?} {} with {modifier} vs plain {ta:?}",
                            if first { "first" } else { "second" }
                        ),
                        timeline(vec![clips], None),
                        dedup(&[&am, &ao]),
                    ));
                }
            }
        }
    }
    run_family("decider routes with keyed crop, ramp and trims", cases, 15.0, 1.4);
}

// ---- round three: resized gray / ten-bit pictures, a white fade on a tied clip, the odd-height pad at the origin ------------

/// The reason the odd-height pad at the frame's origin gives, as the plan spells it.
const ODD_ORIGIN: &str = "odd height at the frame's origin";

/// The reason a cut whose deciding clip is unread gives.
const MIXED: &str = "layers tagged with different YCbCr matrices";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    /// Every frame of the case carries the reason.
    Refused,
    /// No frame of the case carries it.
    NotRefused,
    /// Reported, not judged.
    Either,
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
        Want::Either => false,
    };
    if wrong {
        fam.failures.push(format!(
            "{case}: {n} of {} frames refused for \"{needle}\", expected {want:?} ({:?})",
            out.frames, out.reasons
        ));
    }
}

/// `w` x `h` noise made with `codec` (FFmpeg `args`, container `ext`) in `pix`, at `rate` frames per second, under a name of the
/// round's own; `None` where this FFmpeg cannot make it.
fn codec_media(codec: &str, args: &[&str], ext: &str, pix: &str, w: u32, h: u32, rate: u32) -> Option<Asset> {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("r3-{codec}-{pix}-{w}x{h}-{rate}.{ext}"));
    if !path.exists() {
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate={rate}:duration=0.6,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
        );
        let tmp = dir.join(format!("r3-{codec}-{pix}-{w}x{h}-{rate}.tmp.{ext}"));
        let mut cmd = vec!["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &lavfi];
        cmd.extend_from_slice(args);
        cmd.extend_from_slice(&["-pix_fmt", pix, tmp.to_str().unwrap()]);
        // (An encoder that never finishes — libvpx on an odd height was seen to — is killed, and the picture is not made.)
        let mut child = Command::new(kerf_core::ffmpeg_path())
            .args(&cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let ok = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        if !ok {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        std::fs::rename(&tmp, &path).unwrap();
    }
    Some(probed(&path))
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

/// [`sweep_try`] for a file the frame source may not seek into: skipped.
fn sweep_guarded(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) -> Option<Outcome> {
    guarded(case, || sweep_try(case, tl, assets, fps, secs)).flatten()
}

type Codec = (&'static str, Vec<&'static str>, &'static str);

fn r3_codecs() -> Vec<Codec> {
    vec![
        ("ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("ffv1s4", vec!["-c:v", "ffv1", "-slices", "4"], "mkv"),
        ("ffvhuff", vec!["-c:v", "ffvhuff"], "mkv"),
        ("rawvideo", vec!["-c:v", "rawvideo"], "mkv"),
        ("utvideo", vec!["-c:v", "utvideo"], "mkv"),
        ("magicyuv", vec!["-c:v", "magicyuv"], "mkv"),
        ("vp9", vec!["-c:v", "libvpx-vp9", "-lossless", "1"], "webm"),
        ("mpeg4", vec!["-c:v", "mpeg4", "-q:v", "1"], "mkv"),
    ]
}

/// A picture of `sw` x `sh` from every decoder, delivered at `fw` x `fh`: the case list, the refusal expected of the plan, and
/// every drawn frame held to the export. Returns how many cases were judged.
fn pad_cases(fam: &mut Family, family: &str, codecs: &[Codec], sizes: &[(u32, u32, u32, u32)], want: Want) -> usize {
    let mut judged = 0;
    for (codec, args, ext) in codecs {
        for &(sw, sh, fw, fh) in sizes {
            let Some(a) = codec_media(codec, args, ext, "yuv420p", sw, sh, 30) else {
                eprintln!("{codec} {sw}x{sh}: this FFmpeg cannot make it");
                continue;
            };
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            let case = format!("checker/b/r3/{family} {codec} {sw}x{sh} in {fw}x{fh}");
            if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                judged += 1;
                fam.add(&case, &out);
                expect_refusal(fam, &case, &out, ODD_ORIGIN, want);
            }
        }
    }
    judged
}

/// The odd-height pad at the origin: a 4:2:0 picture one or three rows short of the frame, at the frame's width, sits at the
/// frame's origin and `pad` fills the bottom bar in place (the odd row goes) or copies (it stays) as the decoder's buffer allows.
/// Every decoder, five sizes: every frame is refused for it, and a frame that is drawn is the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_pad_odd_height_at_the_origin_is_refused_from_every_decoder() {
    let mut fam = Family::default();
    let judged = pad_cases(
        &mut fam,
        "pad origin",
        &r3_codecs(),
        &[
            (64, 35, 64, 36),
            (64, 33, 64, 36),
            (96, 53, 96, 54),
            (96, 51, 96, 54),
            (128, 71, 128, 72),
            (64, 39, 64, 40),
            (64, 37, 64, 40),
        ],
        Want::Refused,
    );
    assert!(judged > 14, "too few cases could be made: {judged}");
    fam.finish_refusing("odd-height pad at the origin, every decoder");
}

/// What the new condition must leave alone: a picture off the origin (the gap to the frame is 5 rows or more, so `pad` has a
/// top offset and always copies), an even picture, and a resampled one (`scale` wrote a fresh buffer; its odd row is dropped
/// either way). Not refused for it, and every drawn frame is exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_pad_off_the_origin_resampled_and_even_pictures_are_not_refused_for_it() {
    let mut fam = Family::default();
    let codecs: Vec<Codec> = r3_codecs()
        .into_iter()
        .filter(|(c, ..)| ["ffv1", "ffvhuff", "rawvideo", "utvideo", "vp9", "magicyuv"].contains(c))
        .collect();
    let judged = pad_cases(
        &mut fam,
        "pad off origin",
        &codecs,
        &[
            (64, 31, 64, 36),
            (64, 29, 64, 36),
            (64, 35, 64, 40),
            (64, 13, 64, 36),
            (64, 5, 64, 36),
            (64, 34, 64, 36),
            (64, 36, 64, 36),
            (62, 35, 64, 36),
            (60, 35, 64, 36),
            (50, 27, 64, 36),
            (100, 55, 64, 36),
            (63, 35, 64, 36),
            (65, 35, 64, 36),
            (65, 37, 64, 36),
            (96, 52, 96, 54),
        ],
        Want::NotRefused,
    );
    assert!(judged > 40, "too few cases could be made: {judged}");
    fam.finish("odd pictures off the origin, resampled or even");
}

/// The refusal's three conditions one by one, on one decoder (FFV1 and ffvhuff, which are the two buffer behaviours): a blurred
/// fill, a cover, a delivery of the picture's own size, 4:2:2 / 4:4:4 / gray / RGB sources (`scale` converts them: resampled),
/// a 24 and a 60 fps source (the `fps` filter clones the frames it repeats).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_pad_odd_height_fits_formats_and_rates() {
    let mut fam = Family::default();
    let mut judged = 0;
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        // (A blurred fill the compositor cannot draw — a frame too small for the thumbnail's box blur, under 96 wide — leaves the
        // rule standing; one it draws is not the pad's picture alone, and the rule must not refuse it.)
        let fits = |fw: u32| -> [(&str, Fit, Want); 3] {
            [
                ("contain", Fit::Contain, Want::Refused),
                ("blur", Fit::Blur, if fw >= 96 { Want::NotRefused } else { Want::Either }),
                ("cover", Fit::Cover, Want::NotRefused),
            ]
        };
        for (sw, sh, fw, fh) in [
            (64, 35, 64, 36),
            (96, 51, 96, 54),
            (64, 33, 64, 36),
            (320, 179, 320, 180),
            (640, 359, 640, 360),
            (320, 177, 320, 180),
        ] {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            for (label, fit, want) in fits(fw) {
                let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, fit)));
                let case = format!("checker/b/r3/pad fit {label} {codec} {sw}x{sh} in {fw}x{fh}");
                if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                    judged += 1;
                    fam.add(&case, &out);
                    expect_refusal(&mut fam, &case, &out, ODD_ORIGIN, want);
                }
            }
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], None);
            let case = format!("checker/b/r3/pad no delivery {codec} {sw}x{sh}");
            if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                judged += 1;
                fam.add(&case, &out);
                expect_refusal(&mut fam, &case, &out, ODD_ORIGIN, Want::NotRefused);
            }
        }
        for pix in ["yuv422p", "yuv444p", "gray", "gbrp"] {
            for (sw, sh, fw, fh) in [(64, 35, 64, 36), (64, 33, 64, 36), (63, 35, 64, 36)] {
                let Some(a) = codec_media(codec, &args, ext, pix, sw, sh, 30) else {
                    continue;
                };
                let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
                let case = format!("checker/b/r3/pad format {pix} {codec} {sw}x{sh} in {fw}x{fh}");
                if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                    judged += 1;
                    fam.add(&case, &out);
                    expect_refusal(&mut fam, &case, &out, ODD_ORIGIN, Want::NotRefused);
                }
            }
        }
        for rate in [24u32, 60] {
            for (sw, sh, fw, fh) in [(64, 35, 64, 36), (64, 33, 64, 36), (64, 31, 64, 36)] {
                let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, rate) else {
                    continue;
                };
                let tl = timeline(vec![vec![clip(&a, 0.0, 0.4, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
                let case = format!("checker/b/r3/pad rate {rate} {codec} {sw}x{sh} in {fw}x{fh}");
                if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.4) {
                    judged += 1;
                    fam.add(&case, &out);
                    let want = if sh == 31 { Want::NotRefused } else { Want::Refused };
                    expect_refusal(&mut fam, &case, &out, ODD_ORIGIN, want);
                }
            }
        }
    }
    assert!(judged > 40, "too few cases could be made: {judged}");
    fam.finish("odd-height pad: fits, formats, rates");
}

/// The odd picture at the origin inside the things a clip can carry: its own fades, a dissolve, a dip, a slide, a colour
/// correction, an effect, a speed, a crop, a rotation, an opacity, a mask, a position, a pip, over a base track and under one.
/// Whatever the plan draws is the export's (the odd-origin refusal needs no other reason to stand).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_pad_odd_height_at_the_origin_inside_other_clip_features() {
    use kerf_core::VideoEffect as E;
    let mut fam = Family::default();
    let mut judged = 0;
    let mask = kerf_core::Mask {
        shape: kerf_core::MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather: 0.0,
        inverted: false,
    };
    let eq = Color {
        contrast: 1.2,
        saturation: 1.1,
        ..Color::default()
    };
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        let Some(a) = codec_media(codec, &args, ext, "yuv420p", 64, 35, 30) else {
            continue;
        };
        let Some(b) = codec_media(codec, &args, ext, "yuv420p", 64, 33, 30) else {
            continue;
        };
        let even = media(64, 36, Pat::Test, Tag::None);
        let frame = Some(Delivery::new(64, 36, Fit::Contain));
        type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
        let edits: Vec<(&str, Edit)> = vec![
            ("fade_in", Box::new(|c| c.fade_in = 0.15)),
            ("fade_out", Box::new(|c| c.fade_out = 0.15)),
            ("eq", Box::new(move |c| c.color = eq)),
            ("invert", Box::new(|c| c.effects = vec![E::Invert])),
            ("blur", Box::new(|c| c.effects = vec![E::Blur { sigma: 1.5 }])),
            ("grayscale", Box::new(|c| c.effects = vec![E::Grayscale])),
            (
                "speed 2",
                Box::new(|c| {
                    c.speed = 2.0;
                    c.source_out = c.source_in + 0.6;
                }),
            ),
            ("reverse", Box::new(|c| c.speed = -1.0)),
            (
                "crop",
                Box::new(|c| {
                    c.transform.crop_left = 0.1;
                    c.transform.crop_bottom = 0.1;
                }),
            ),
            ("rotation", Box::new(|c| c.transform.rotation = 8.0)),
            ("opacity", Box::new(|c| c.transform.opacity = 0.6)),
            ("mask", Box::new(move |c| c.mask = Some(mask))),
            ("position", Box::new(|c| c.transform.pos_x = 0.1)),
            ("pip", Box::new(|c| *c = pip(c.clone(), 0.5, 0.1, 0.1))),
            (
                "keyed position",
                Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.4, 1.0, 0.1, 0.05)]),
            ),
        ];
        for (label, edit) in &edits {
            let mut c = clip(&a, 0.0, 0.4, 0.0);
            edit(&mut c);
            let tl = timeline(vec![vec![c]], frame);
            let case = format!("checker/b/r3/pad inside {label} {codec} 64x35");
            if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.4) {
                judged += 1;
                if *label == "rotation" {
                    // (A rotated layer's edge has the base's own ladder, the same for an even picture the size of the frame:
                    // `checker_b_r3_rotation_of_odd_height_pictures_is_no_worse_than_an_even_one`.)
                    fam.add_known_ladder(&case, &out);
                } else {
                    fam.add(&case, &out);
                }
            }
        }
        for kind in [
            TransitionKind::Crossfade,
            TransitionKind::DipToBlack,
            TransitionKind::DipToWhite,
            TransitionKind::SlideLeft,
            TransitionKind::PushUp,
        ] {
            for (label, first, second) in [
                ("odd then odd", &a, &b),
                ("odd then even", &a, &even),
                ("even then odd", &even, &a),
            ] {
                let tl = timeline(
                    vec![vec![
                        clip(first, 0.0, 0.4, 0.0),
                        dissolve_into(clip(second, 0.0, 0.4, 0.4), kind, 0.2),
                    ]],
                    frame,
                );
                let case = format!("checker/b/r3/pad transition {kind:?} {label} {codec}");
                let assets = dedup(&[first, second]);
                if let Some(out) = sweep_guarded(&case, &tl, &assets, 30.0, 0.8) {
                    judged += 1;
                    if kind == TransitionKind::DipToWhite {
                        let worst = out.bad.iter().flat_map(|b| b.planes.iter().map(|p| p.max)).max().unwrap_or(0);
                        if worst > 3 {
                            fam.failures.push(format!(
                                "{case}: a dip to white's ladder is 3 levels, drawn frames differ by {worst}"
                            ));
                        }
                        fam.add_known_ladder(&case, &out);
                    } else {
                        fam.add(&case, &out);
                    }
                }
            }
        }
        for (label, tracks) in [
            (
                "odd over even",
                vec![vec![clip(&even, 0.0, 0.4, 0.0)], vec![clip(&a, 0.0, 0.4, 0.0)]],
            ),
            (
                "even over odd",
                vec![vec![clip(&a, 0.0, 0.4, 0.0)], vec![clip(&even, 0.0, 0.4, 0.0)]],
            ),
            (
                "odd pip over odd",
                vec![
                    vec![clip(&a, 0.0, 0.4, 0.0)],
                    vec![pip(clip(&b, 0.0, 0.4, 0.0), 0.5, 0.2, 0.2)],
                ],
            ),
        ] {
            let tl = timeline(tracks, frame);
            let case = format!("checker/b/r3/pad stacked {label} {codec}");
            let assets = dedup(&[&a, &b, &even]);
            if let Some(out) = sweep_guarded(&case, &tl, &assets, 30.0, 0.4) {
                judged += 1;
                fam.add(&case, &out);
            }
        }
    }
    assert!(judged > 40, "too few cases could be made: {judged}");
    fam.finish("odd-height pad at the origin inside other clip features");
}

/// Planar 8-bit 4:2:0 is the only format the exact scaler resizes: `yuv420p` and `yuvj420p` (full-range x264) still draw,
/// in every cut that resizes, odd pictures included. Not refused for the format, and every drawn frame exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_resized_planar_8bit_420_is_still_drawn_exactly() {
    let mut fam = Family::default();
    let mut drawn_plain = 0;
    for (pix, codec, args, ext) in [
        ("yuv420p", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv420p", "ffvhuff", vec!["-c:v", "ffvhuff"], "mkv"),
        (
            "yuvj420p",
            "x264j",
            vec!["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"],
            "mp4",
        ),
    ] {
        for (sw, sh) in [(640u32, 360u32), (320, 180), (64, 36), (63, 35), (90, 160)] {
            let Some(a) = codec_media(codec, &args, ext, pix, sw, sh, 30) else {
                continue;
            };
            for (label, tl) in resize_cuts(&a) {
                let case = format!("checker/b/r3/resized {pix} {codec} {sw}x{sh} {label}");
                if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                    fam.add(&case, &out);
                    expect_refusal(&mut fam, &case, &out, FORMAT_RESIZE, Want::NotRefused);
                    if pix == "yuv420p" {
                        drawn_plain += out.drawn;
                    }
                }
            }
        }
    }
    assert!(drawn_plain > 0 || patched_out(), "no resized yuv420p frame was drawn");
    fam.finish_refusing("resized planar 8-bit 4:2:0");
}

/// Everything else that is resized is FFmpeg's: gray (8 and 10-bit), 9 / 10 / 12-bit 4:2:0, semi-planar `nv12`, 4:2:2, 4:4:4,
/// 4:4:0, planar RGB. None is drawn; each frame is refused for the format.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_resized_other_formats_stay_refused() {
    let mut fam = Family::default();
    let mut judged = 0;
    for (pix, codec, args, ext) in [
        ("gray", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("gray10le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv420p9le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv420p10le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv420p12le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("nv12", "rawvideo", vec!["-c:v", "rawvideo"], "mkv"),
        ("yuv422p", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv444p", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv440p", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("gbrp", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
    ] {
        for (sw, sh) in [(320u32, 180u32), (63, 35)] {
            let Some(a) = codec_media(codec, &args, ext, pix, sw, sh, 30) else {
                eprintln!("{pix} {sw}x{sh}: this FFmpeg cannot make it");
                continue;
            };
            let seen = a.streams.iter().find_map(|s| s.pix_fmt.clone()).unwrap_or_default();
            eprintln!("{pix} {sw}x{sh}: probed as {seen}");
            for (label, tl) in resize_cuts(&a) {
                let case = format!("checker/b/r3/resized {pix} {sw}x{sh} {label}");
                if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                    judged += 1;
                    refused_for_format_resize(&mut fam, &case, &out);
                }
            }
        }
    }
    assert!(judged > 50, "too few cases could be made: {judged}");
    fam.finish_refusing("resized other formats are refused");
}

/// The same pictures left at their size (an even one: the default canvas rounds an odd one down, which resizes it) are not
/// refused for the format: drawn frames are exact (gray and 10-bit are converted on the way in, not resized).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_unresized_gray_and_ten_bit_pictures_are_not_refused_for_the_format() {
    let mut fam = Family::default();
    let mut judged = 0;
    for pix in ["gray", "gray10le", "yuv420p10le", "yuv422p", "yuv444p"] {
        for (sw, sh) in [(320u32, 180u32), (64, 36), (90, 160)] {
            let Some(a) = codec_media("ffv1", &["-c:v", "ffv1"], "mkv", pix, sw, sh, 30) else {
                continue;
            };
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], None);
            let case = format!("checker/b/r3/unresized {pix} {sw}x{sh}");
            if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                judged += 1;
                fam.add(&case, &out);
                expect_refusal(&mut fam, &case, &out, FORMAT_RESIZE, Want::NotRefused);
            }
        }
    }
    assert!(judged > 10, "too few cases could be made: {judged}");
    fam.finish_refusing("unresized gray and ten-bit pictures");
}

/// The format check reads the probe's pixel-format string: which strings let a resize through (`yuv420p`, `yuvj420p`, in any
/// case), and which do not (the rest, and a stream whose format was never recorded). No export: the plan alone.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_format_resize_follows_the_probed_pixel_format_string() {
    let base = media(320, 180, Pat::Test, Tag::None);
    let opts = at_fps(30.0);
    let mut wrong = Vec::new();
    for (pix, resizes) in [
        (Some("yuv420p"), true),
        (Some("yuvj420p"), true),
        (Some("YUV420P"), true),
        (Some("YuvJ420p"), true),
        (Some("gray"), false),
        (Some("gray8"), false),
        (Some("gray10le"), false),
        (Some("gray12le"), false),
        (Some("yuv420p9le"), false),
        (Some("yuv420p10le"), false),
        (Some("yuv420p10be"), false),
        (Some("yuv420p12le"), false),
        (Some("yuv420p16le"), false),
        (Some("nv12"), false),
        (Some("nv21"), false),
        (Some("p010le"), false),
        (Some("p016le"), false),
        (Some("yuv422p"), false),
        (Some("yuv444p"), false),
        (Some("yuva420p"), false),
        (Some("rgb24"), false),
        (Some("bgr0"), false),
        (Some("pal8"), false),
        (Some(""), false),
        (None, false),
    ] {
        let mut asset = base.clone();
        for s in &mut asset.streams {
            if s.kind == StreamKind::Video {
                s.pix_fmt = pix.map(str::to_string);
            }
        }
        for (label, tl) in resize_cuts(&asset) {
            let planner = Planner::new(&tl, std::slice::from_ref(&asset), &opts, PlanRequest::motion(policy())).expect("planner");
            let plan = planner.at_frame(2).expect("plan");
            let why = plan.reasons(&compositor().caps(), plan.size(u32::MAX));
            let refused = why.iter().any(|r| r.to_string().contains(FORMAT_RESIZE));
            if refused == resizes {
                wrong.push(format!(
                    "{pix:?} {label}: {} for the format, expected {}",
                    if refused { "refused" } else { "not refused" },
                    if resizes { "let through" } else { "refused" }
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Holds a case that holds a dip to white to the export: a dip to white has the base's ladder on every frame of the graph (the
/// RGB round trip's stage: `checker_b_r2_dip_to_white_in_the_cut_leaves_the_clips_before_it_exact`, the same figures for equal
/// and for different matrices), so no frame of such a cut differs by more than 3 levels; a wrong matrix moves every sample of
/// the cut by tens. The frames outside the dip's own are counted (`windows`, seconds, a frame either side).
fn judge_dips(fam: &mut Family, case: &str, out: &Outcome, fps: f64, windows: &[(f64, f64)]) {
    fam.cases += 1;
    fam.drawn += out.drawn;
    fam.refused += out.refused;
    let inside = |k: u64| {
        let t = k as f64 / fps;
        windows.iter().any(|&(lo, hi)| t >= lo - 1.5 / fps && t <= hi + 1.5 / fps)
    };
    for b in &out.bad {
        let worst = b.planes.iter().map(|p| p.max).max().unwrap_or(0);
        if worst > 3 {
            fam.failures.push(format!(
                "{case}: frame {} ({} a dip): Y {} (max {}, first {:?}) U {} (max {}, first {:?}) V {} (max {}, first {:?})",
                b.frame,
                if inside(b.frame) { "inside" } else { "outside" },
                b.planes[0].differing,
                b.planes[0].max,
                b.planes[0].first,
                b.planes[1].differing,
                b.planes[1].max,
                b.planes[1].first,
                b.planes[2].differing,
                b.planes[2].max,
                b.planes[2].first
            ));
        }
    }
    if !out.bad.is_empty() {
        let outside = out.bad.iter().filter(|b| !inside(b.frame)).count();
        eprintln!(
            "   dip ladder (the base's, at most 3): {case}: {} of {} drawn frames, {outside} outside the dip",
            out.bad.len(),
            out.drawn
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhiteShape {
    AlphaDipIn,
    AlphaDipOut,
    AlphaDipBoth,
    CrossfadeDipOut,
    KeyedDipIn,
    KeyedDipOut,
    KeyedDipBoth,
    AlphaFirstDipOut,
    AlphaAfterTwoPlainDipIn,
    DipOnTheFirstAlpha,
    CleanThenAlphaDip,
    AlphaDipThenCrossfade,
    StackedAlphaDip,
    PlainDipIn,
    PlainDipOut,
    PlainDipBoth,
    PipDipIn,
}

const WHITE_SHAPES: [WhiteShape; 17] = [
    WhiteShape::AlphaDipIn,
    WhiteShape::AlphaDipOut,
    WhiteShape::AlphaDipBoth,
    WhiteShape::CrossfadeDipOut,
    WhiteShape::KeyedDipIn,
    WhiteShape::KeyedDipOut,
    WhiteShape::KeyedDipBoth,
    WhiteShape::AlphaFirstDipOut,
    WhiteShape::AlphaAfterTwoPlainDipIn,
    WhiteShape::DipOnTheFirstAlpha,
    WhiteShape::CleanThenAlphaDip,
    WhiteShape::AlphaDipThenCrossfade,
    WhiteShape::StackedAlphaDip,
    WhiteShape::PlainDipIn,
    WhiteShape::PlainDipOut,
    WhiteShape::PlainDipBoth,
    WhiteShape::PipDipIn,
];

impl WhiteShape {
    /// Whether a clip on the alpha plane carries the dip (the clip that would decide is unread).
    fn unread_decider(self) -> Option<bool> {
        match self {
            Self::PlainDipIn | Self::PlainDipOut | Self::PlainDipBoth | Self::PipDipIn => Some(false),
            Self::CleanThenAlphaDip => None,
            _ => Some(true),
        }
    }
}

/// The cut, its assets, the dip windows (seconds) and the tags it uses.
type WhiteCut = (Timeline, Vec<Asset>, Vec<(f64, f64)>, Vec<Tag>);

/// A cut of one dip shape in tags `t`, the dip lasting `d`.
fn white_cut(shape: WhiteShape, t: [Tag; 3], d: f64) -> WhiteCut {
    use WhiteShape as S;
    let dip = |c: Clip| dissolve_into(c, TransitionKind::DipToWhite, d);
    let window = |at: f64| (at - d / 2.0, at + d / 2.0);
    let (p1, a1) = routed(Route::Plain, t[0], Pat::Edges, 0.0);
    let (p2, a2) = routed(Route::Plain, t[1], Pat::Noise, 0.7);
    let (p3, a3) = routed(Route::Plain, t[2], Pat::Test, 1.4);
    let (al1, aa1) = routed(Route::AlphaSource, t[0], Pat::Edges, 0.0);
    let (al2, aa2) = routed(Route::AlphaSource, t[1], Pat::Noise, 0.7);
    let (al3, aa3) = routed(Route::AlphaSource, t[2], Pat::Test, 1.4);
    let (x2, ax2) = routed(Route::Crossfade, t[1], Pat::Noise, 0.7);
    let (x3, ax3) = routed(Route::Crossfade, t[2], Pat::Test, 1.4);
    let (k2, ak2) = routed(Route::KeyedPos, t[1], Pat::Noise, 0.7);
    let (pip2, apip2) = routed(Route::Pip, t[1], Pat::Noise, 0.7);
    let first = |tl: Vec<Vec<Clip>>, assets: &[&Asset], windows: Vec<(f64, f64)>, used: usize| {
        (timeline(tl, None), dedup(assets), windows, t[..used].to_vec())
    };
    match shape {
        S::AlphaDipIn => first(vec![vec![p1, dip(al2)]], &[&a1, &aa2], vec![window(0.7)], 2),
        S::AlphaDipOut => first(vec![vec![p1, al2, dip(p3)]], &[&a1, &aa2, &a3], vec![window(1.4)], 3),
        S::AlphaDipBoth => first(
            vec![vec![p1, dip(al2), dip(p3)]],
            &[&a1, &aa2, &a3],
            vec![window(0.7), window(1.4)],
            3,
        ),
        S::CrossfadeDipOut => first(vec![vec![p1, x2, dip(p3)]], &[&a1, &ax2, &a3], vec![window(1.4)], 3),
        S::KeyedDipIn => first(vec![vec![p1, dip(k2)]], &[&a1, &ak2], vec![window(0.7)], 2),
        S::KeyedDipOut => first(vec![vec![p1, k2, dip(p3)]], &[&a1, &ak2, &a3], vec![window(1.4)], 3),
        S::KeyedDipBoth => first(
            vec![vec![p1, dip(k2), dip(p3)]],
            &[&a1, &ak2, &a3],
            vec![window(0.7), window(1.4)],
            3,
        ),
        S::AlphaFirstDipOut => first(vec![vec![al1, dip(p2)]], &[&aa1, &a2], vec![window(0.7)], 2),
        S::AlphaAfterTwoPlainDipIn => first(vec![vec![p1, p2, dip(al3)]], &[&a1, &a2, &aa3], vec![window(1.4)], 3),
        S::DipOnTheFirstAlpha => first(vec![vec![dip(al1), p2]], &[&aa1, &a2], vec![window(0.0)], 2),
        S::CleanThenAlphaDip => first(vec![vec![al1, p2, dip(al3)]], &[&aa1, &a2, &aa3], vec![window(1.4)], 3),
        S::AlphaDipThenCrossfade => first(vec![vec![p1, dip(al2), x3]], &[&a1, &aa2, &ax3], vec![window(0.7)], 3),
        S::StackedAlphaDip => {
            let (mut over, ao) = routed(Route::AlphaSource, t[1], Pat::Noise, 0.35);
            over = pip(dip(over), 0.6, 0.15, -0.1);
            first(vec![vec![p1, p3], vec![over]], &[&a1, &a3, &ao], vec![window(0.35)], 3)
        }
        S::PlainDipIn => first(vec![vec![p1, dip(p2)]], &[&a1, &a2], vec![window(0.7)], 2),
        S::PlainDipOut => first(vec![vec![p1, p2, dip(p3)]], &[&a1, &a2, &a3], vec![window(1.4)], 3),
        S::PlainDipBoth => first(
            vec![vec![p1, dip(p2), dip(p3)]],
            &[&a1, &a2, &a3],
            vec![window(0.7), window(1.4)],
            3,
        ),
        S::PipDipIn => first(vec![vec![p1, dip(pip2)]], &[&a1, &apip2], vec![window(0.7)], 2),
    }
}

/// A clip that is tied to the alpha plane and dips to white on either side (`white_in` / `white_out`) may carry a converter in
/// its chain (`fade c=white` runs on RGB), so on a build that negotiates (9.0.2) it is not the one whose tag decides the cut's
/// matrix, and a cut of mixed matrices holding one is refused. Plain clips that dip stay drawn. Both fade directions, an alpha
/// source, a dissolving clip, a keyed position, a pip, three-clip cuts, the dip at the head, a short and a long dip, over every
/// tag arrangement. Outside the dips' own frames every drawn frame is exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_white_fade_on_tied_clips_is_not_a_decider() {
    let tag_sets = [
        [Tag::None, Tag::Bt709, Tag::None],
        [Tag::Bt709, Tag::None, Tag::Bt709],
        [Tag::None, Tag::Bt709, Tag::Bt709],
        [Tag::Bt709, Tag::None, Tag::None],
        [Tag::None, Tag::Bt709, Tag::Bt2020],
        [Tag::Bt709, Tag::Bt2020, Tag::None],
        [Tag::Bt709, Tag::Bt709, Tag::Bt709],
    ];
    let mut fam = Family::default();
    let mut refused_cuts = 0;
    for tags in tag_sets {
        for shape in WHITE_SHAPES {
            let durations: &[f64] = match shape {
                WhiteShape::AlphaDipIn | WhiteShape::AlphaDipOut | WhiteShape::CrossfadeDipOut => &[0.3, 0.08, 1.4],
                _ => &[0.3],
            };
            for &d in durations {
                let (tl, assets, windows, used) = white_cut(shape, tags, d);
                let case = format!("checker/b/r3/whitefade {shape:?} d{d} {tags:?}");
                let Some(out) = sweep_try(&case, &tl, &assets, 15.0, 2.1) else {
                    continue;
                };
                judge_dips(&mut fam, &case, &out, 15.0, &windows);
                let mixed = used.iter().any(|&u| !same_matrix(u, used[0]));
                let want = match (negotiates() && mixed, shape.unread_decider()) {
                    (true, Some(true)) => Want::Refused,
                    (true, Some(false)) => Want::NotRefused,
                    _ => Want::Either,
                };
                expect_refusal(&mut fam, &case, &out, MIXED, want);
                if out.refused_for(MIXED) > 0 {
                    refused_cuts += 1;
                }
            }
        }
    }
    eprintln!("== cuts refused as mixed: {refused_cuts}");
    fam.finish("white fade on tied clips");
}

/// A rotation of an odd-height picture next to the same rotation of an even one: a rotated layer's edge has the base's own ladder
/// (chroma at the turned edge; figures below, the same for an even picture the size of the frame), so this holds the odd heights to
/// the even control's figures and to no more than the ladder's bounds, and nothing else moves.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_rotation_of_odd_height_pictures_is_no_worse_than_an_even_one() {
    let mut fam = Family::default();
    let mut judged = 0;
    let mut worst_by_degrees: BTreeMap<String, (i32, i32)> = BTreeMap::new();
    let mut odd: Vec<(String, String, i32, i32)> = Vec::new();
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        for (sw, sh, fw, fh) in [
            (64, 36, 64, 36),
            (64, 34, 64, 36),
            (64, 35, 64, 36),
            (64, 33, 64, 36),
            (64, 31, 64, 36),
        ] {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            for degrees in [8.0, 45.0, 90.0, -3.0] {
                let mut c = clip(&a, 0.0, 0.4, 0.0);
                c.transform.rotation = degrees;
                let tl = timeline(vec![vec![c]], Some(Delivery::new(fw, fh, Fit::Contain)));
                let case = format!("checker/b/r3/rotation {degrees} {codec} {sw}x{sh} in {fw}x{fh}");
                let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.4) else {
                    continue;
                };
                judged += 1;
                fam.add_known_ladder(&case, &out);
                let y = out.bad.iter().map(|b| b.planes[0].max).max().unwrap_or(0);
                let uv = out
                    .bad
                    .iter()
                    .flat_map(|b| [b.planes[1].max, b.planes[2].max])
                    .max()
                    .unwrap_or(0);
                if sh % 2 == 0 {
                    let e = worst_by_degrees.entry(format!("{degrees}")).or_insert((0, 0));
                    *e = (e.0.max(y), e.1.max(uv));
                } else {
                    odd.push((format!("{degrees}"), case, y, uv));
                }
            }
        }
    }
    for (degrees, case, y, uv) in odd {
        let (ey, euv) = worst_by_degrees.get(&degrees).copied().unwrap_or((0, 0));
        if y > ey.max(1) || uv > euv.max(100) {
            fam.failures
                .push(format!("{case}: Y max {y} UV max {uv}, the even control's {ey} / {euv}"));
        }
    }
    assert!(judged >= 16, "too few cases could be made: {judged}");
    fam.finish("rotation of odd-height pictures");
}

/// A Still plan of an odd-height picture at the frame's width, from the gap that leaves it at the origin (1 and 3 rows: refused)
/// to the gaps that leave it off it (5 rows and more), and the even neighbours. Whatever the plan draws is the still's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_stills_of_odd_height_pictures_by_gap() {
    let mut fam = Family::default();
    let mut judged = 0;
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        for (sw, sh, fw, fh) in [
            (64, 36, 64, 36),
            (64, 35, 64, 36),
            (64, 34, 64, 36),
            (64, 33, 64, 36),
            (64, 32, 64, 36),
            (64, 31, 64, 36),
            (64, 30, 64, 36),
            (64, 29, 64, 36),
            (64, 27, 64, 36),
            (64, 13, 64, 36),
            (64, 5, 64, 36),
            (64, 31, 64, 40),
            (64, 33, 64, 40),
            (62, 35, 64, 36),
        ] {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.5, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            let case = format!("checker/b/r3/still gap {codec} {sw}x{sh} in {fw}x{fh}");
            let times = [0.0, 0.1, 0.2, 0.3];
            let Some(out) = guarded(&case, || still_sweep(&case, &tl, std::slice::from_ref(&a), &times)) else {
                continue;
            };
            judged += 1;
            fam.add_still(&case, &out);
            let at_origin = sw == fw && sh % 2 == 1 && fh - sh <= 3;
            let n: usize = out
                .reasons
                .iter()
                .filter(|(r, _)| r.contains(ODD_ORIGIN))
                .map(|(_, n)| n)
                .sum();
            if !patched_out() && ((at_origin && n != times.len()) || (!at_origin && n != 0)) {
                fam.failures.push(format!(
                    "{case}: {n} of {} stills refused for the odd-height pad, expected {}",
                    times.len(),
                    if at_origin { "all" } else { "none" }
                ));
            }
        }
    }
    assert!(judged > 20, "too few cases could be made: {judged}");
    fam.finish("stills of odd-height pictures by gap");
}

/// Diagnostic: where a still of an odd-height picture off the origin differs from the still's own planes.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_diag_still_gap_rows() {
    let (codec, args, ext) = r3_codecs().remove(0);
    for (sw, sh, fw, fh) in [(64, 5, 64, 36), (64, 31, 64, 36), (64, 30, 64, 36), (64, 31, 64, 40)] {
        let a = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30).unwrap();
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.5, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
        let plan = kerf_core::RenderPlan::at(&tl, std::slice::from_ref(&a), &ExportOptions::default(), 0.1, policy()).unwrap();
        let (w, h, want, got) = still_planes(&plan, &tl, std::slice::from_ref(&a), 0.1);
        let (w, h) = (w as usize, h as usize);
        eprintln!("== {sw}x{sh} in {fw}x{fh}: still {w}x{h}");
        for (name, plane) in ["Y", "U", "V"].iter().enumerate() {
            let mut rows = Vec::new();
            for y in 0..h {
                let n = (0..w)
                    .filter(|x| want[name * w * h + y * w + x] != got[name * w * h + y * w + x])
                    .count();
                if n > 0 {
                    rows.push(format!("{y}:{n}"));
                }
            }
            eprintln!("  {plane}: rows with differences (row:count) {}", rows.join(" "));
        }
        if let Some(y) = (0..h).find(|y| (0..w).any(|x| want[y * w + x] != got[y * w + x])) {
            eprintln!(
                "  first differing Y row {y}: still {:?} gpu {:?}",
                &want[y * w..y * w + 8],
                &got[y * w..y * w + 8]
            );
        }
    }
}

/// The black fades on a clip that is tied to the alpha plane (the other half of the white fade's question: which fades put a
/// converter into a tied chain): a clip's own fade in or out, a dip to black into it or out of it, on an alpha source, a
/// dissolving clip and a keyed position, in cuts of mixed matrices. No ladder here: every drawn frame is the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r3_black_fades_on_tied_clips() {
    let dip = |c: Clip| dissolve_into(c, TransitionKind::DipToBlack, 0.3);
    let mut fam = Family::default();
    for t in [
        [Tag::None, Tag::Bt709, Tag::None],
        [Tag::Bt709, Tag::None, Tag::Bt709],
        [Tag::None, Tag::Bt709, Tag::Bt2020],
    ] {
        let (p1, a1) = routed(Route::Plain, t[0], Pat::Edges, 0.0);
        let (p3, a3) = routed(Route::Plain, t[2], Pat::Test, 1.4);
        let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
        for route in [Route::AlphaSource, Route::Crossfade, Route::KeyedPos, Route::Pip] {
            let (r2, a2) = routed(route, t[1], Pat::Noise, 0.7);
            let with = |f: &dyn Fn(Clip) -> Clip| f(r2.clone());
            let shapes: Vec<(&str, Vec<Clip>)> = vec![
                (
                    "fade_in",
                    vec![
                        p1.clone(),
                        with(&|mut c| {
                            c.fade_in = 0.3;
                            c
                        }),
                    ],
                ),
                (
                    "fade_out",
                    vec![
                        p1.clone(),
                        with(&|mut c| {
                            c.fade_out = 0.3;
                            c
                        }),
                    ],
                ),
                (
                    "fade_in and out",
                    vec![
                        p1.clone(),
                        with(&|mut c| {
                            c.fade_in = 0.2;
                            c.fade_out = 0.2;
                            c
                        }),
                    ],
                ),
                ("dip black out", vec![p1.clone(), r2.clone(), dip(p3.clone())]),
                (
                    "fade_out then plain",
                    vec![
                        p1.clone(),
                        with(&|mut c| {
                            c.fade_out = 0.3;
                            c
                        }),
                        p3.clone(),
                    ],
                ),
            ];
            for (label, clips) in shapes {
                cases.push((
                    format!("{route:?} {label} {t:?}"),
                    timeline(vec![clips], None),
                    dedup(&[&a1, &a2, &a3]),
                ));
            }
            if route != Route::Crossfade {
                cases.push((
                    format!("{route:?} dip black in {t:?}"),
                    timeline(vec![vec![p1.clone(), dip(r2.clone())]], None),
                    dedup(&[&a1, &a2]),
                ));
            }
        }
        for (name, tl, assets) in cases {
            let case = format!("checker/b/r3/blackfade {name}");
            if let Some(out) = sweep_try(&case, &tl, &assets, 15.0, 2.1) {
                fam.add(&case, &out);
            }
        }
    }
    fam.finish("black fades on tied clips");
}

// ---- round four: the odd-height pad off the origin (81d94b2d) --------------------------------------------------------------
//
// A Still plan draws a plain `yuv420p` odd-height picture's `scale,pad,setsar` off the origin without its odd last row (the still's
// `scale` converts into `pad`'s buffer, `pad` fills in place), refuses every other still chain, and an export refuses the odd-height
// pad of any format but `yuv420p` and of a chain on the alpha plane. Every drawn frame is held to the export's planes.

/// The reason the plan gives for an odd-height pad off the origin in a still whose chain is not a plain `scale,pad,setsar`.
const R4_STILL: &str = "odd height in a still";

/// ... in an export, for a picture whose format is not `yuv420p`.
const R4_FORMAT: &str = "odd height whose format";

/// ... in an export, for a chain on the alpha plane (a crossfade's incoming clip, a mask, a chroma key).
const R4_ALPHA: &str = "odd height on the alpha plane";

#[derive(Default, Debug)]
struct R4Still {
    times: usize,
    drawn: usize,
    refused: usize,
    bad: Vec<(f64, [PlaneDiff; 3])>,
    reasons: BTreeMap<String, usize>,
    /// Refused stills drawn anyway (a run with `KERF_CHECKER_ALLOW` and the scratch patch): how many, how many were the still's.
    anyway_drawn: usize,
    anyway_exact: usize,
    anyway_undrawable: usize,
    anyway_worst: i32,
}

impl R4Still {
    fn refused_for(&self, needle: &str) -> usize {
        self.reasons.iter().filter(|(r, _)| r.contains(needle)).map(|(_, n)| n).sum()
    }
}

/// The size, the still's `yuv444p` planes and the compositor's put through the same conversion.
type StillPlanes = ((usize, usize), Vec<u8>, Vec<u8>);

/// The still's `yuv444p` planes (`want`) and the compositor's composite put through the same conversion (`got`), with the size;
/// the error where the compositor will not draw the plan.
fn r4_still_planes(
    plan: &kerf_core::RenderPlan,
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
) -> Result<StillPlanes, String> {
    let size = plan.size(max_width);
    let (w, h, want) =
        kerf_core::timeline_frame_yuv_cancellable(tl, assets, opts, t, size.0, &|| false).map_err(|e| format!("{e}"))?;
    assert_eq!((w, h), size, "the still is the plan's size");
    let frames = kerf_gpu::decode_layers(&plan.layers).map_err(|e| e.to_string())?;
    let drawn = compositor().composite_yuv(plan, &frames, size).map_err(|e| e.to_string())?;
    let mut raw = drawn.y.clone();
    raw.extend_from_slice(&drawn.u);
    raw.extend_from_slice(&drawn.v);
    let dims = format!("{w}x{h}");
    let got = ffmpeg_piped(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &dims,
            "-i",
            "pipe:0",
            "-filter_complex",
            "[0:v]format=yuv444p[o]",
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ],
        &raw,
    );
    assert_eq!(got.len(), want.len());
    Ok(((w as usize, h as usize), want, got))
}

fn r4_compare(want: &[u8], got: &[u8], w: usize, h: usize) -> [PlaneDiff; 3] {
    let n = w * h;
    [0, 1, 2].map(|p| diff_plane(&want[p * n..(p + 1) * n], &got[p * n..(p + 1) * n], w))
}

/// A `FFmpeg | GPU | diff x8` strip of one still (`yuv444p` planes) in `target/parity/checker-b/montage/`.
fn r4_still_montage(case: &str, t: f64, (w, h): (usize, usize), want: &[u8], got: &[u8]) {
    let dims = format!("{w}x{h}");
    let rgb = |planes: &[u8]| -> Vec<u8> {
        ffmpeg_piped(
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv444p",
                "-s",
                &dims,
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
            planes,
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
    let dir = target_dir().join("parity").join("checker-b").join("montage");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}-still-t{}.png", slug(case), (t * 1000.0).round() as i64));
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

/// Each time of the cut as a Still plan with `opts`: refused (counted with the reasons; drawn anyway where a run has the refusal
/// patched out), or drawn and held to the still's own planes, sample for sample.
fn r4_still(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, times: &[f64]) -> R4Still {
    r4_still_at(case, tl, assets, opts, times, u32::MAX)
}

/// [`r4_still`] for the still FFmpeg draws at most `max_width` wide (a preview's), the plan taking that size.
fn r4_still_at(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, times: &[f64], max_width: u32) -> R4Still {
    let mut out = R4Still {
        times: times.len(),
        ..R4Still::default()
    };
    let mut log = String::new();
    for &t in times {
        let plan = kerf_core::RenderPlan::at(tl, assets, opts, t, policy()).expect("plan");
        let size = plan.size(max_width);
        let why = plan.reasons(&compositor().caps(), size);
        let refused = !why.is_empty();
        if refused {
            out.refused += 1;
            let names: BTreeSet<String> = why.iter().map(ToString::to_string).collect();
            for n in &names {
                *out.reasons.entry(n.clone()).or_insert(0) += 1;
            }
            let _ = writeln!(log, "{t}\trefused\t{}", names.into_iter().collect::<Vec<_>>().join(" | "));
            if !patched_out() {
                continue;
            }
        }
        match r4_still_planes(&plan, tl, assets, opts, t, max_width) {
            Ok(((w, h), want, got)) => {
                let planes = r4_compare(&want, &got, w, h);
                let exact = planes.iter().all(|p| p.differing == 0);
                if refused {
                    out.anyway_drawn += 1;
                    out.anyway_exact += usize::from(exact);
                    out.anyway_worst = out.anyway_worst.max(planes.iter().map(|p| p.max).max().unwrap_or(0));
                    let _ = writeln!(log, "{t}\tANYWAY-{}", if exact { "exact" } else { "INEXACT" });
                } else {
                    out.drawn += 1;
                    if exact {
                        let _ = writeln!(log, "{t}\texact");
                    } else {
                        if out.bad.is_empty() {
                            r4_still_montage(case, t, (w, h), &want, &got);
                        }
                        let _ = writeln!(log, "{t}\tINEXACT");
                        out.bad.push((t, planes));
                    }
                }
            }
            Err(_) if refused => out.anyway_undrawable += 1,
            Err(e) => panic!("{case}: t={t}: {e}"),
        }
    }
    let _ = std::fs::write(frame_log_dir().join(format!("r4-still-{}.frames", slug(case))), &log);
    let mut line = format!(
        "{case:<66} {:>2} times: {:>2} drawn ({:>2} exact), {:>2} refused",
        out.times,
        out.drawn,
        out.drawn - out.bad.len(),
        out.refused
    );
    if let Some((t, p)) = out.bad.first() {
        let _ = write!(
            line,
            " | INEXACT {} stills, first at {t} s | Y {} max {} first {:?} | U {} max {} first {:?} | V {} max {} first {:?}",
            out.bad.len(),
            p[0].differing,
            p[0].max,
            p[0].first,
            p[1].differing,
            p[1].max,
            p[1].first,
            p[2].differing,
            p[2].max,
            p[2].first
        );
    }
    if out.anyway_drawn + out.anyway_undrawable > 0 {
        let _ = write!(
            line,
            " | drawn anyway {} ({} exact, {} undrawable) worst {}",
            out.anyway_drawn, out.anyway_exact, out.anyway_undrawable, out.anyway_worst
        );
    }
    if !out.reasons.is_empty() {
        let mut why: Vec<String> = out
            .reasons
            .iter()
            .map(|(r, n)| format!("{n}x {}", r.chars().take(60).collect::<String>()))
            .collect();
        why.truncate(2);
        let _ = write!(line, " | refused: {}", why.join("; "));
    }
    eprintln!("{line}");
    report_lines().lock().unwrap().insert(format!("r4 still {case}"), line);
    write_report();
    out
}

impl Family {
    fn add_r4(&mut self, case: &str, out: &R4Still) {
        self.cases += 1;
        self.drawn += out.drawn;
        self.refused += out.refused;
        if let Some((t, p)) = out.bad.first() {
            self.failures.push(format!(
                "{case} [{}]: {}/{} drawn stills inexact, first at {t} s: Y {} max {} first {:?}, U {} max {} first {:?}, V {} max {} first {:?}",
                build_key(),
                out.bad.len(),
                out.drawn,
                p[0].differing,
                p[0].max,
                p[0].first,
                p[1].differing,
                p[1].max,
                p[1].first,
                p[2].differing,
                p[2].max,
                p[2].first
            ));
        }
    }
}

/// [`expect_refusal`] for a still sweep.
fn r4_expect(fam: &mut Family, case: &str, out: &R4Still, needle: &str, want: Want) {
    if patched_out() {
        return;
    }
    let n = out.refused_for(needle);
    let wrong = match want {
        Want::Refused => n < out.times,
        Want::NotRefused => n != 0,
        Want::Either => false,
    };
    if wrong {
        fam.failures.push(format!(
            "{case}: {n} of {} stills refused for \"{needle}\", expected {want:?} ({:?})",
            out.times, out.reasons
        ));
    }
}

/// `a` cut to its first `rows` rows (FFV1): the same picture without its odd last row.
fn r4_first_rows(a: &Asset, rows: u32) -> Asset {
    r4_first_rows_in(a, rows, "yuv420p")
}

/// [`r4_first_rows`] in the pixel format `pix`.
fn r4_first_rows_in(a: &Asset, rows: u32, pix: &str) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let src = Path::new(&a.path);
    let dir = src.parent().expect("a directory");
    let stem = src.file_stem().expect("a name").to_string_lossy().into_owned();
    let path = dir.join(format!("r4-rows{rows}-{pix}-{stem}.mkv"));
    if !path.exists() {
        let tmp = dir.join(format!("r4-rows{rows}-{pix}-{stem}.tmp.mkv"));
        let crop = format!("crop=iw:{rows}:0:0");
        let codec: &[&str] = if pix.starts_with("yuvj") {
            &["-c:v", "mjpeg", "-q:v", "1"]
        } else {
            &["-c:v", "ffv1"]
        };
        let mut cmd = vec!["-i", src.to_str().unwrap(), "-vf", &crop];
        cmd.extend_from_slice(codec);
        cmd.extend_from_slice(&["-pix_fmt", pix, tmp.to_str().unwrap()]);
        ffmpeg(&cmd);
        std::fs::rename(&tmp, &path).unwrap();
    }
    probed(&path)
}

/// A `w` x `h` noise clip tagged as PQ HDR (BT.2020), FFV1.
fn r4_pq_media(w: u32, h: u32) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("r4-pq-{w}x{h}.mkv"));
    if !path.exists() {
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=0.6,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255',\
             setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084"
        );
        let tmp = dir.join(format!("r4-pq-{w}x{h}.tmp.mkv"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
            tmp.to_str().unwrap(),
        ]);
        std::fs::rename(&tmp, &path).unwrap();
    }
    probed(&path)
}

/// Whether FFmpeg's still of a cut with an odd-height picture is the still of the same cut with the picture's first rows alone (the
/// odd last row gone): `Some(true)` when every plane is equal (`pad` dropped the row), `Some(false)` when they differ (the row is
/// there), `None` where FFmpeg will not draw either. The gap has to be 1 modulo 4 rows, so that both pictures sit at the same top
/// offset (`pad` rounds it down to an even row).
fn r4_row_dropped(odd: (&Timeline, &Asset), even: (&Timeline, &Asset), opts: &ExportOptions, t: f64) -> Option<bool> {
    let still = |(tl, a): (&Timeline, &Asset)| {
        let plan = kerf_core::RenderPlan::at(tl, std::slice::from_ref(a), opts, t, policy()).ok()?;
        let size = plan.size(u32::MAX);
        kerf_core::timeline_frame_yuv_cancellable(tl, std::slice::from_ref(a), opts, t, size.0, &|| false).ok()
    };
    Some(still(odd)? == still(even)?)
}

/// [`codec_media`] at 30 fps, except that an MJPEG clip is made 1.5 s long: Kerf takes a Motion-JPEG stream under a second for a still
/// picture, which an export does not read as a video.
fn r4_source(codec: &str, args: &[&str], ext: &str, pix: &str, w: u32, h: u32) -> Option<Asset> {
    if codec != "mjpeg" {
        return codec_media(codec, args, ext, pix, w, h, 30);
    }
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("r4-mjpeg-{pix}-{w}x{h}.{ext}"));
    if !path.exists() {
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=1.5,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
        );
        let tmp = dir.join(format!("r4-mjpeg-{pix}-{w}x{h}.tmp.{ext}"));
        let mut cmd = vec!["-f", "lavfi", "-i", &lavfi];
        cmd.extend_from_slice(args);
        cmd.extend_from_slice(&["-pix_fmt", pix, tmp.to_str().unwrap()]);
        ffmpeg(&cmd);
        std::fs::rename(&tmp, &path).unwrap();
    }
    Some(probed(&path))
}

/// The sizes `(source width, height, frame width, height)` of a picture at the frame's width, one row short of nothing: every gap
/// from 5 rows to 41, odd pictures only.
const R4_SIZES: [(u32, u32, u32, u32); 13] = [
    (64, 31, 64, 36),
    (64, 29, 64, 36),
    (64, 27, 64, 36),
    (64, 13, 64, 36),
    (64, 5, 64, 36),
    (64, 3, 64, 36),
    (64, 1, 64, 36),
    (64, 33, 64, 40),
    (64, 35, 64, 44),
    (96, 53, 96, 64),
    (128, 71, 128, 90),
    (320, 179, 320, 200),
    (640, 359, 640, 400),
];

const R4_TIMES: [f64; 4] = [0.0, 0.1, 0.2, 0.3];

/// `KERF_CHECKER_QUICK` set: one decoder and a few sizes, for the scratch runs that look for a mutation's first failure.
fn r4_quick() -> bool {
    std::env::var_os("KERF_CHECKER_QUICK").is_some()
}

/// The plain still chain, off the origin: a `yuv420p` odd-height picture, `scale,pad,setsar` into an overlay. The plan draws it
/// (without the odd last row), from every decoder, at every gap, at 24 / 29.97 / default frame rates, and every drawn still is the
/// FFmpeg still's planes.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_plain_odd_height_off_the_origin_is_drawn_exactly() {
    let mut fam = Family::default();
    let mut judged = 0;
    let rates: [(&str, ExportOptions); 3] = [
        ("default fps", ExportOptions::default()),
        ("24 fps", at_fps(24.0)),
        ("29.97 fps", at_fps(30000.0 / 1001.0)),
    ];
    for (codec, args, ext) in r3_codecs() {
        if r4_quick() && codec != "ffv1" {
            continue;
        }
        let wide = codec == "ffv1" || codec == "ffvhuff";
        let sizes: Vec<_> = if r4_quick() {
            vec![R4_SIZES[0], R4_SIZES[4]]
        } else if wide {
            R4_SIZES.to_vec()
        } else {
            vec![R4_SIZES[0], R4_SIZES[3], R4_SIZES[4], R4_SIZES[7], R4_SIZES[9], R4_SIZES[11]]
        };
        for (sw, sh, fw, fh) in sizes {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                eprintln!("{codec} {sw}x{sh}: this FFmpeg cannot make it");
                continue;
            };
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.5, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            for (rate, opts) in &rates {
                if !wide && !rate.starts_with("default") {
                    continue;
                }
                if rate.starts_with("29") && (sw, sh) != (64, 31) && (sw, sh) != (64, 5) {
                    continue;
                }
                let case = format!("checker/b/r4/still plain {codec} {sw}x{sh} in {fw}x{fh} {rate}");
                judged += 1;
                let out = r4_still(&case, &tl, std::slice::from_ref(&a), opts, &R4_TIMES);
                fam.add_r4(&case, &out);
                r4_expect(&mut fam, &case, &out, R4_STILL, Want::NotRefused);
                r4_expect(&mut fam, &case, &out, ODD_ORIGIN, Want::NotRefused);
                if !patched_out() && out.drawn != out.times {
                    fam.failures
                        .push(format!("{case}: {} of {} stills drawn", out.drawn, out.times));
                }
            }
        }
    }
    assert!(
        judged > if r4_quick() { 2 } else { 60 },
        "too few cases could be made: {judged}"
    );
    fam.finish("plain odd-height stills off the origin");
}

/// The same pictures at the origin (one or three rows short, five rows is the first gap off it): still refused in a still, from every
/// decoder and frame rate, and the first gap off it is drawn.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_origin_stays_refused_and_the_first_gap_off_it_is_drawn() {
    let mut fam = Family::default();
    let mut judged = 0;
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| !r4_quick() || *c == "ffv1") {
        for (sw, sh, fw, fh, origin) in [
            (64, 35, 64, 36, true),
            (64, 33, 64, 36, true),
            (64, 37, 64, 40, true),
            (64, 31, 64, 36, false),
            (64, 35, 64, 40, false),
            (96, 51, 96, 54, true),
            (96, 49, 96, 54, false),
        ] {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.5, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            for opts in [ExportOptions::default(), at_fps(24.0), at_fps(30000.0 / 1001.0)] {
                let case = format!("checker/b/r4/still origin {codec} {sw}x{sh} in {fw}x{fh} {:?}", opts.fps);
                judged += 1;
                let out = r4_still(&case, &tl, std::slice::from_ref(&a), &opts, &R4_TIMES);
                fam.add_r4(&case, &out);
                r4_expect(
                    &mut fam,
                    &case,
                    &out,
                    ODD_ORIGIN,
                    if origin { Want::Refused } else { Want::NotRefused },
                );
                r4_expect(&mut fam, &case, &out, R4_STILL, Want::NotRefused);
            }
        }
    }
    assert!(
        judged > if r4_quick() { 10 } else { 60 },
        "too few cases could be made: {judged}"
    );
    fam.finish("odd-height stills at the origin and the first gap off it");
}

/// Odd-height pictures in a still with other layers: stacked over and under an even picture and an odd one, in a picture-in-picture, and
/// across every transition (the stills inside and outside it). A slide or a push moves a plain layer and leaves the chain alone, so
/// those stills are drawn; a dissolve or a dip puts a fade into the chain and is refused for it. Whatever is drawn is the still's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_stacks_and_transitions_with_odd_height_pictures() {
    let mut fam = Family::default();
    let mut judged = 0;
    let frame = Some(Delivery::new(64, 36, Fit::Contain));
    let times = [0.05, 0.2, 0.3, 0.38, 0.45, 0.5, 0.55, 0.7];
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        let (Some(a), Some(b)) = (
            codec_media(codec, &args, ext, "yuv420p", 64, 31, 30),
            codec_media(codec, &args, ext, "yuv420p", 64, 29, 30),
        ) else {
            continue;
        };
        let even = media(64, 36, Pat::Test, Tag::None);
        let assets = dedup(&[&a, &b, &even]);
        for (label, tracks) in [
            (
                "odd over even",
                vec![vec![clip(&even, 0.0, 0.8, 0.0)], vec![clip(&a, 0.0, 0.8, 0.0)]],
            ),
            (
                "even over odd",
                vec![vec![clip(&a, 0.0, 0.8, 0.0)], vec![clip(&even, 0.0, 0.8, 0.0)]],
            ),
            (
                "odd over odd",
                vec![vec![clip(&a, 0.0, 0.8, 0.0)], vec![clip(&b, 0.0, 0.8, 0.0)]],
            ),
            (
                "odd pip over odd",
                vec![
                    vec![clip(&a, 0.0, 0.8, 0.0)],
                    vec![pip(clip(&b, 0.0, 0.8, 0.0), 0.5, 0.2, 0.2)],
                ],
            ),
            ("odd under a translucent odd", {
                let mut c = clip(&b, 0.0, 0.8, 0.0);
                c.transform.opacity = 0.5;
                vec![vec![clip(&a, 0.0, 0.8, 0.0)], vec![c]]
            }),
            (
                "three odd tracks",
                vec![
                    vec![clip(&a, 0.0, 0.8, 0.0)],
                    vec![clip(&b, 0.0, 0.8, 0.0)],
                    vec![clip(&a, 0.0, 0.8, 0.0)],
                ],
            ),
        ] {
            let tl = timeline(tracks, frame);
            let case = format!("checker/b/r4/still stack {label} {codec}");
            judged += 1;
            let out = r4_still(&case, &tl, &assets, &ExportOptions::default(), &times);
            fam.add_r4(&case, &out);
        }
        for kind in [
            TransitionKind::Crossfade,
            TransitionKind::DipToBlack,
            TransitionKind::SlideLeft,
            TransitionKind::SlideRight,
            TransitionKind::SlideUp,
            TransitionKind::SlideDown,
            TransitionKind::PushLeft,
            TransitionKind::PushRight,
            TransitionKind::PushUp,
            TransitionKind::PushDown,
        ] {
            for (label, first, second) in [
                ("odd then odd", &a, &b),
                ("odd then even", &a, &even),
                ("even then odd", &even, &a),
            ] {
                let tl = timeline(
                    vec![vec![
                        clip(first, 0.0, 0.4, 0.0),
                        dissolve_into(clip(second, 0.0, 0.5, 0.4), kind, 0.3),
                    ]],
                    frame,
                );
                let assets = dedup(&[first, second]);
                for opts in [ExportOptions::default(), at_fps(24.0), at_fps(30000.0 / 1001.0)] {
                    let case = format!("checker/b/r4/still transition {kind:?} {label} {codec} {:?}", opts.fps);
                    judged += 1;
                    let out = r4_still(&case, &tl, &assets, &opts, &times);
                    fam.add_r4(&case, &out);
                    let moving = matches!(
                        kind,
                        TransitionKind::SlideLeft
                            | TransitionKind::SlideRight
                            | TransitionKind::SlideUp
                            | TransitionKind::SlideDown
                            | TransitionKind::PushLeft
                            | TransitionKind::PushRight
                            | TransitionKind::PushUp
                            | TransitionKind::PushDown
                    );
                    if moving {
                        r4_expect(&mut fam, &case, &out, R4_STILL, Want::NotRefused);
                    }
                }
            }
        }
    }
    assert!(judged > 60, "too few cases could be made: {judged}");
    fam.finish("odd-height stills in stacks and across transitions");
}

/// The fits and the shapes round the rule: a Cover and a Blur delivery (no bars, or a blurred fill: nothing to pad), a delivery of the
/// picture's own size, a delivery with an odd width, no delivery (the first picture sets the frame), a picture narrower than the frame
/// (scaled up: resampled) and a taller one. The plan draws what it draws, and every drawn still is the still's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_fits_and_shapes_of_odd_height_pictures() {
    let mut fam = Family::default();
    let mut judged = 0;
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        for (sw, sh, fw, fh) in [
            (64, 31, 64, 36),
            (96, 53, 96, 64),
            (128, 71, 128, 90),
            (64, 35, 64, 36),
            (63, 31, 64, 36),
            (62, 31, 64, 36),
            (50, 27, 64, 36),
            (100, 55, 64, 36),
            (63, 31, 63, 36),
            (63, 31, 63, 31),
            (65, 33, 65, 40),
            (64, 31, 64, 31),
        ] {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            for (label, fit) in [("contain", Fit::Contain), ("cover", Fit::Cover), ("blur", Fit::Blur)] {
                let tl = timeline(vec![vec![clip(&a, 0.0, 0.5, 0.0)]], Some(Delivery::new(fw, fh, fit)));
                let case = format!("checker/b/r4/still fit {label} {codec} {sw}x{sh} in {fw}x{fh}");
                judged += 1;
                let out = r4_still(&case, &tl, std::slice::from_ref(&a), &ExportOptions::default(), &R4_TIMES);
                fam.add_r4(&case, &out);
                if fit != Fit::Contain {
                    r4_expect(&mut fam, &case, &out, R4_STILL, Want::NotRefused);
                }
            }
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.5, 0.0)]], None);
            let case = format!("checker/b/r4/still no delivery {codec} {sw}x{sh}");
            judged += 1;
            let out = r4_still(&case, &tl, std::slice::from_ref(&a), &ExportOptions::default(), &R4_TIMES);
            fam.add_r4(&case, &out);
            r4_expect(&mut fam, &case, &out, R4_STILL, Want::NotRefused);
        }
    }
    assert!(judged > 60, "too few cases could be made: {judged}");
    fam.finish("fits and shapes of odd-height stills");
}

/// Picture formats and tags of a plain still: the `yuv420p` picture tagged BT.601, BT.709, BT.2020, untagged, flagged full range (`pc`)
/// and the `yuvj420p` and 10-bit ones. Plain `yuv420p` pictures are drawn (the matrix is the stack's, the range flag changes nothing
/// of `scale`'s format step), any other format is refused for the pad (a still's `scale` converts a `yuvj` or 10-bit picture), and
/// every drawn still is the still's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_odd_height_in_every_tag_range_and_format() {
    let mut fam = Family::default();
    let mut judged = 0;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str, tag: Tag, pix: &str, range: Option<&str>, w: u32, h: u32| -> Asset {
        static WRITING: Mutex<()> = Mutex::new(());
        let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = dir.join(format!("r4-tag-{name}-{w}x{h}.mkv"));
        if !path.exists() {
            let range_filter = range.map(|r| format!(",setparams=range={r}")).unwrap_or_default();
            let lavfi = format!(
                "nullsrc=size={w}x{h}:rate=30:duration=0.6,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'{}{range_filter}",
                tag.filter()
            );
            let tmp = dir.join(format!("r4-tag-{name}-{w}x{h}.tmp.mkv"));
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
    };
    for (w, h, fw, fh) in [(64, 31, 64, 36), (64, 13, 64, 36), (64, 33, 64, 40)] {
        let cases: Vec<(String, Asset, Want)> = vec![
            (
                "untagged".into(),
                make("untagged", Tag::None, "yuv420p", None, w, h),
                Want::NotRefused,
            ),
            (
                "bt709".into(),
                make("bt709", Tag::Bt709, "yuv420p", None, w, h),
                Want::NotRefused,
            ),
            (
                "bt601".into(),
                make("smpte170m", Tag::Smpte170m, "yuv420p", None, w, h),
                Want::NotRefused,
            ),
            (
                "bt470bg".into(),
                make("bt470bg", Tag::Bt470bg, "yuv420p", None, w, h),
                Want::NotRefused,
            ),
            (
                "bt2020".into(),
                make("bt2020", Tag::Bt2020, "yuv420p", None, w, h),
                Want::NotRefused,
            ),
            (
                "tv flag".into(),
                make("tv", Tag::None, "yuv420p", Some("tv"), w, h),
                Want::NotRefused,
            ),
            (
                "pc flag".into(),
                make("pc", Tag::None, "yuv420p", Some("pc"), w, h),
                Want::NotRefused,
            ),
            (
                "bt709 pc flag".into(),
                make("bt709pc", Tag::Bt709, "yuv420p", Some("pc"), w, h),
                Want::NotRefused,
            ),
            (
                "10-bit".into(),
                make("p10", Tag::None, "yuv420p10le", None, w, h),
                Want::Refused,
            ),
            (
                "10-bit bt709".into(),
                make("p10709", Tag::Bt709, "yuv420p10le", None, w, h),
                Want::Refused,
            ),
        ];
        let mut cases = cases;
        // (A real `yuvj420p`: FFV1 in Matroska comes back as `yuv420p`, MJPEG does not.)
        if let Some(a) = r4_source("mjpeg", &["-c:v", "mjpeg", "-q:v", "1"], "mkv", "yuvj420p", w, h) {
            cases.push(("yuvj420p mjpeg".into(), a, Want::Refused));
        }
        for (label, a, want) in cases {
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.5, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            for opts in [ExportOptions::default(), at_fps(24.0)] {
                let case = format!("checker/b/r4/still tag {label} {w}x{h} in {fw}x{fh} {:?}", opts.fps);
                judged += 1;
                let out = r4_still(&case, &tl, std::slice::from_ref(&a), &opts, &R4_TIMES);
                fam.add_r4(&case, &out);
                r4_expect(&mut fam, &case, &out, R4_STILL, want);
            }
        }
    }
    // (Two layers: the bottom one sets the composite's matrix on 9.0.2; every pair of tags, an odd picture over an even one and under.)
    let even = media(64, 36, Pat::Test, Tag::None);
    for (ta, tb) in [
        (Tag::None, Tag::Bt709),
        (Tag::Bt709, Tag::None),
        (Tag::Bt709, Tag::Bt2020),
        (Tag::Bt2020, Tag::Bt709),
    ] {
        let (a, b) = (
            make(&format!("pair{ta:?}"), ta, "yuv420p", None, 64, 31),
            make(&format!("pair{tb:?}"), tb, "yuv420p", None, 64, 29),
        );
        for (label, tracks) in [
            (
                "odd over odd",
                vec![vec![clip(&a, 0.0, 0.5, 0.0)], vec![clip(&b, 0.0, 0.5, 0.0)]],
            ),
            (
                "odd over even",
                vec![vec![clip(&even, 0.0, 0.5, 0.0)], vec![clip(&a, 0.0, 0.5, 0.0)]],
            ),
        ] {
            let tl = timeline(tracks, Some(Delivery::new(64, 36, Fit::Contain)));
            let case = format!("checker/b/r4/still tag pair {ta:?}/{tb:?} {label}");
            judged += 1;
            let out = r4_still(&case, &tl, &dedup(&[&a, &b, &even]), &ExportOptions::default(), &R4_TIMES);
            fam.add_r4(&case, &out);
        }
    }
    assert!(judged > 60, "too few cases could be made: {judged}");
    fam.finish("odd-height stills in every tag, range and format");
}

/// Every other still chain: a colour correction, a grade, each effect, a fade in and out, a mask, a keyed number, a blend, an HDR
/// picture. The plan refuses an odd-height pad off the origin in each (a still, not a plain `yuv420p` `scale,pad,setsar`). Drawn
/// anyway (a run with the scratch patch) the plan's geometry keeps the odd row: that is exact only where FFmpeg keeps it too, so each
/// kind is also asked of FFmpeg's own still, on a grey picture with a white last row. `ROW` lines name what FFmpeg does, and `NEEDED`
/// or `WIDER THAN NEEDED` is the verdict on the refusal (a kind whose odd row FFmpeg keeps is drawn exactly by the plan's geometry).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_other_chains_are_refused_and_ffmpegs_odd_row_in_each() {
    use kerf_core::VideoEffect as E;
    let mut fam = Family::default();
    let mut judged = 0;
    let mut verdicts: Vec<String> = Vec::new();
    let full_mask = kerf_core::Mask {
        shape: kerf_core::MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 1.0,
        height: 1.0,
        feather: 0.0,
        inverted: false,
    };
    let channel = |prop: kerf_core::Property, a: f64, b: f64| kerf_core::PropertyTrack {
        prop,
        keys: [(0.0, a), (0.5, b)]
            .iter()
            .map(|&(t, v)| kerf_core::PropertyKey::new(t, v))
            .collect(),
    };
    let eq = Color {
        contrast: 1.2,
        saturation: 1.1,
        ..Color::default()
    };
    let grade = grades_b().remove(0).1;
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    let kinds: Vec<(&str, bool, Edit)> = vec![
        ("eq contrast", false, Box::new(move |c| c.color = eq)),
        (
            "eq brightness",
            false,
            Box::new(|c| {
                c.color = Color {
                    brightness: 0.1,
                    ..Color::default()
                }
            }),
        ),
        ("grade", false, Box::new(move |c| c.grade = grade.clone())),
        ("blur", false, Box::new(|c| c.effects = vec![E::Blur { sigma: 1.5 }])),
        ("sharpen", false, Box::new(|c| c.effects = vec![E::Sharpen { amount: 1.0 }])),
        ("grayscale", false, Box::new(|c| c.effects = vec![E::Grayscale])),
        ("invert", false, Box::new(|c| c.effects = vec![E::Invert])),
        ("vignette", false, Box::new(|c| c.effects = vec![E::Vignette])),
        (
            "chroma key",
            false,
            Box::new(|c| {
                c.effects = vec![E::ChromaKey {
                    color: "0x00ff00".into(),
                    similarity: 0.05,
                    blend: 0.0,
                }];
            }),
        ),
        ("fade in", false, Box::new(|c| c.fade_in = 0.5)),
        ("fade out", false, Box::new(|c| c.fade_out = 0.5)),
        ("mask", false, Box::new(move |c| c.mask = Some(full_mask))),
        (
            "keyed colour",
            false,
            Box::new(move |c| c.channels = vec![channel(kerf_core::Property::Saturation, 1.0, 1.4)]),
        ),
        (
            "keyed scale held",
            false,
            Box::new(move |c| c.channels = vec![channel(kerf_core::Property::Scale, 1.0, 1.0)]),
        ),
        (
            "keyed opacity held",
            false,
            Box::new(move |c| c.channels = vec![channel(kerf_core::Property::Opacity, 1.0, 1.0)]),
        ),
        (
            "blend multiply",
            false,
            Box::new(|c| c.blend_mode = kerf_core::BlendMode::Multiply),
        ),
        (
            "blend screen",
            false,
            Box::new(|c| c.blend_mode = kerf_core::BlendMode::Screen),
        ),
        ("hdr", true, Box::new(|_| {})),
    ];
    let times = [0.1, 0.2, 0.3, 0.4];
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        for (sw, sh, fw, fh) in [(64, 31, 64, 36), (64, 27, 64, 36), (64, 35, 64, 44), (64, 33, 64, 40)] {
            let Some(noise) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            let pq = r4_pq_media(sw, sh);
            for (label, hdr, edit) in &kinds {
                let source = if *hdr { &pq } else { &noise };
                let mut c = clip(source, 0.0, 0.5, 0.0);
                edit(&mut c);
                let tl = timeline(vec![vec![c]], Some(Delivery::new(fw, fh, Fit::Contain)));
                let case = format!("checker/b/r4/still chain {label} {codec} {sw}x{sh} in {fw}x{fh}");
                judged += 1;
                let out = r4_still(&case, &tl, std::slice::from_ref(source), &ExportOptions::default(), &times);
                fam.add_r4(&case, &out);
                r4_expect(&mut fam, &case, &out, R4_STILL, Want::Refused);
                // What FFmpeg does with the odd row in this chain, against the same chain over the picture without it.
                if codec != "ffv1" || (fh - sh) % 4 != 1 {
                    continue;
                }
                let short = r4_first_rows(source, sh - 1);
                let mut c = clip(&short, 0.0, 0.5, 0.0);
                edit(&mut c);
                let short_tl = timeline(vec![vec![c]], Some(Delivery::new(fw, fh, Fit::Contain)));
                let dropped: Vec<Option<bool>> = times
                    .iter()
                    .map(|&t| r4_row_dropped((&tl, source), (&short_tl, &short), &ExportOptions::default(), t))
                    .collect();
                let verdict = if *label == "vignette" {
                    "UNREAD (the effect depends on the picture's size)"
                } else if dropped.iter().all(|d| *d == Some(false)) {
                    "WIDER THAN NEEDED (FFmpeg keeps the odd row: the plan's geometry draws it)"
                } else if dropped.iter().all(|d| *d == Some(true)) {
                    "NEEDED (FFmpeg drops the odd row; the plan's geometry keeps it)"
                } else {
                    "MIXED / UNREAD"
                };
                let line = format!(
                    "ROW [{}] {label:<18} {sw}x{sh} in {fw}x{fh}: dropped {dropped:?} -> {verdict}",
                    build_key()
                );
                eprintln!("{line}");
                verdicts.push(line);
            }
        }
    }
    let _ = std::fs::write(
        target_dir().join("parity").join(format!("r4-rows-{}.txt", build_key())),
        verdicts.join("\n"),
    );
    assert!(judged > 40, "too few cases could be made: {judged}");
    fam.finish_refusing("other still chains refused for the odd-height pad");
}

/// How many of the first `total` frames of an export at `fps` fall in `[from, to)` seconds, give or take the frame at an edge (the
/// `fps` filter's pick there is the export's to make): the least and the most.
fn r4_frames_between(fps: f64, from: f64, to: f64, total: usize) -> (usize, usize) {
    let inside = |slack: f64| {
        (0..total)
            .filter(|&k| {
                let t = k as f64 / fps;
                t + slack >= from && t - slack < to
            })
            .count()
    };
    (inside(-0.5 / fps), inside(1.5 / fps))
}

/// A `yuv420p` odd-height picture off the origin in an export (`scale`, `pad`, `setsar`, `fps`, `trim`, ..., `format=yuv420p`): the
/// `scale` forwards the decoder's frame and `pad` copies it, whatever else the clip carries. Every clip feature that leaves the chain
/// on the `yuv420p` planes is drawn, and every drawn frame is the export's; a mask and a chroma key put the chain on the alpha plane
/// (`format=yuva420p`) and are refused for it, on every frame.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_odd_height_off_the_origin_inside_other_clip_features() {
    use kerf_core::VideoEffect as E;
    let mut fam = Family::default();
    let mut judged = 0;
    let mask = kerf_core::Mask {
        shape: kerf_core::MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather: 0.0,
        inverted: false,
    };
    let eq = Color {
        contrast: 1.2,
        saturation: 1.1,
        ..Color::default()
    };
    let channel = |prop: kerf_core::Property, a: f64, b: f64| kerf_core::PropertyTrack {
        prop,
        keys: [(0.0, a), (0.4, b)]
            .iter()
            .map(|&(t, v)| kerf_core::PropertyKey::new(t, v))
            .collect(),
    };
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        let Some(a) = codec_media(codec, &args, ext, "yuv420p", 64, 31, 30) else {
            continue;
        };
        let grade = grades_b().remove(0).1;
        let frame = Some(Delivery::new(64, 36, Fit::Contain));
        type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
        let edits: Vec<(&str, bool, Edit)> = vec![
            ("plain", false, Box::new(|_| {})),
            ("fade_in", false, Box::new(|c| c.fade_in = 0.15)),
            ("fade_out", false, Box::new(|c| c.fade_out = 0.15)),
            (
                "fade in and out",
                false,
                Box::new(|c| {
                    c.fade_in = 0.1;
                    c.fade_out = 0.1;
                }),
            ),
            ("eq", false, Box::new(move |c| c.color = eq)),
            (
                "grade and eq",
                false,
                Box::new(move |c| {
                    c.color = eq;
                    c.grade = grade.clone();
                }),
            ),
            ("invert", false, Box::new(|c| c.effects = vec![E::Invert])),
            ("blur", false, Box::new(|c| c.effects = vec![E::Blur { sigma: 1.5 }])),
            ("sharpen", false, Box::new(|c| c.effects = vec![E::Sharpen { amount: 1.0 }])),
            ("grayscale", false, Box::new(|c| c.effects = vec![E::Grayscale])),
            ("vignette", false, Box::new(|c| c.effects = vec![E::Vignette])),
            (
                "invert and blur and eq",
                false,
                Box::new(move |c| {
                    c.color = eq;
                    c.effects = vec![E::Invert, E::Blur { sigma: 1.0 }];
                }),
            ),
            (
                "speed 2",
                false,
                Box::new(|c| {
                    c.speed = 2.0;
                    c.source_out = c.source_in + 0.6;
                }),
            ),
            (
                "speed half",
                false,
                Box::new(|c| {
                    c.speed = 0.5;
                    c.source_out = c.source_in + 0.2;
                }),
            ),
            ("reverse", false, Box::new(|c| c.speed = -1.0)),
            (
                "crop",
                false,
                Box::new(|c| {
                    c.transform.crop_left = 0.1;
                    c.transform.crop_bottom = 0.1;
                }),
            ),
            ("opacity", false, Box::new(|c| c.transform.opacity = 0.6)),
            ("position", false, Box::new(|c| c.transform.pos_x = 0.1)),
            ("pip", false, Box::new(|c| *c = pip(c.clone(), 0.5, 0.1, 0.1))),
            (
                "keyed position",
                false,
                Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.4, 1.0, 0.1, 0.05)]),
            ),
            (
                "keyed colour",
                false,
                Box::new(move |c| c.channels = vec![channel(kerf_core::Property::Saturation, 1.0, 1.4)]),
            ),
            (
                "keyed opacity",
                false,
                Box::new(move |c| c.channels = vec![channel(kerf_core::Property::Opacity, 1.0, 0.5)]),
            ),
            (
                "keyed crop",
                false,
                Box::new(move |c| c.channels = vec![channel(kerf_core::Property::CropZoom, 1.0, 1.5)]),
            ),
            ("mask", true, Box::new(move |c| c.mask = Some(mask))),
            (
                "chroma key",
                true,
                Box::new(|c| {
                    c.effects = vec![E::ChromaKey {
                        color: "0x00ff00".into(),
                        similarity: 0.05,
                        blend: 0.0,
                    }];
                }),
            ),
            (
                "mask and fade",
                true,
                Box::new(move |c| {
                    c.mask = Some(mask);
                    c.fade_in = 0.1;
                }),
            ),
        ];
        for (label, alpha, edit) in &edits {
            for fps in [30.0, 24.0] {
                let mut c = clip(&a, 0.0, 0.4, 0.0);
                edit(&mut c);
                let tl = timeline(vec![vec![c]], frame);
                let case = format!("checker/b/r4/export inside {label} {codec} 64x31 {fps}");
                if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), fps, 0.4) {
                    judged += 1;
                    fam.add(&case, &out);
                    expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::NotRefused);
                    expect_refusal(
                        &mut fam,
                        &case,
                        &out,
                        R4_ALPHA,
                        if *alpha { Want::Refused } else { Want::NotRefused },
                    );
                }
            }
        }
        // (A mask or a key on one of two layers: the other layer's frames are not refused for it.)
        let even = media(64, 36, Pat::Test, Tag::None);
        for (label, over, under) in [("odd mask over even", true, false), ("even mask over odd", false, true)] {
            let mut top = if over {
                clip(&a, 0.0, 0.4, 0.0)
            } else {
                clip(&even, 0.0, 0.4, 0.0)
            };
            top.mask = Some(mask);
            let bottom = if under {
                clip(&a, 0.0, 0.4, 0.0)
            } else {
                clip(&even, 0.0, 0.4, 0.0)
            };
            let tl = timeline(vec![vec![bottom], vec![top]], frame);
            let case = format!("checker/b/r4/export stacked {label} {codec}");
            if let Some(out) = sweep_guarded(&case, &tl, &dedup(&[&a, &even]), 30.0, 0.4) {
                judged += 1;
                fam.add(&case, &out);
                expect_refusal(
                    &mut fam,
                    &case,
                    &out,
                    R4_ALPHA,
                    if over { Want::Refused } else { Want::NotRefused },
                );
            }
        }
    }
    assert!(judged > 40, "too few cases could be made: {judged}");
    fam.finish("odd-height pad off the origin inside other clip features, in an export");
}

/// Every transition into and out of odd-height pictures off the origin (a crossfade's incoming clip is on the alpha plane: refused for
/// the pad, on every frame of that clip; the others leave the chain on `yuv420p`: drawn). Every drawn frame is the export's. The dip
/// to white has its own test.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_odd_height_off_the_origin_across_transitions() {
    let mut fam = Family::default();
    let mut judged = 0;
    let frame = Some(Delivery::new(64, 36, Fit::Contain));
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        let (Some(a), Some(b)) = (
            codec_media(codec, &args, ext, "yuv420p", 64, 31, 30),
            codec_media(codec, &args, ext, "yuv420p", 64, 29, 30),
        ) else {
            continue;
        };
        let even = media(64, 36, Pat::Test, Tag::None);
        for kind in [
            TransitionKind::Crossfade,
            TransitionKind::DipToBlack,
            TransitionKind::SlideLeft,
            TransitionKind::SlideRight,
            TransitionKind::SlideUp,
            TransitionKind::SlideDown,
            TransitionKind::PushLeft,
            TransitionKind::PushRight,
            TransitionKind::PushUp,
            TransitionKind::PushDown,
        ] {
            for (label, first, second, odd_incoming) in [
                ("odd then odd", &a, &b, true),
                ("odd then even", &a, &even, false),
                ("even then odd", &even, &a, true),
            ] {
                for fps in [30.0, 24.0] {
                    let tl = timeline(
                        vec![vec![
                            clip(first, 0.0, 0.4, 0.0),
                            dissolve_into(clip(second, 0.0, 0.4, 0.4), kind, 0.2),
                        ]],
                        frame,
                    );
                    let case = format!("checker/b/r4/export transition {kind:?} {label} {codec} {fps}");
                    if let Some(out) = sweep_guarded(&case, &tl, &dedup(&[first, second]), fps, 0.8) {
                        judged += 1;
                        fam.add(&case, &out);
                        expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::NotRefused);
                        if kind == TransitionKind::Crossfade && odd_incoming {
                            // (The whole of the incoming clip's frames: from 0.4 s on.)
                            let n = out.refused_for(R4_ALPHA);
                            let (least, most) = r4_frames_between(fps, 0.4, 0.8, out.frames);
                            if !patched_out() && (n < least || n > most) {
                                fam.failures.push(format!(
                                    "{case}: {n} frames refused for \"{R4_ALPHA}\", expected the incoming clip's {least} to {most} ({:?})",
                                    out.reasons
                                ));
                            }
                        } else {
                            expect_refusal(&mut fam, &case, &out, R4_ALPHA, Want::NotRefused);
                        }
                    }
                }
            }
        }
        // (A clip fading in over an odd picture, a crossfade over a stack, a slide over an odd picture on another track.)
        let mut fading = clip(&b, 0.0, 0.4, 0.0);
        fading.fade_in = 0.2;
        fading.fade_out = 0.2;
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.4, 0.0)], vec![fading]], frame);
        let case = format!("checker/b/r4/export fades over odd {codec}");
        if let Some(out) = sweep_guarded(&case, &tl, &dedup(&[&a, &b]), 30.0, 0.4) {
            judged += 1;
            fam.add(&case, &out);
        }
    }
    assert!(judged > 40, "too few cases could be made: {judged}");
    fam.finish("odd-height pad off the origin across transitions, in an export");
}

/// The dip to white with odd-height pictures off the origin: no allowance. (The base draws a dip to white's frames to within three
/// levels, not exactly; the other lane tests hold it to that ladder. This one holds it to nothing, so a failure here is the ladder.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_dip_to_white_frames_the_plan_draws_are_exact() {
    let mut fam = Family::default();
    let mut judged = 0;
    let frame = Some(Delivery::new(64, 36, Fit::Contain));
    let codec = ("ffv1", vec!["-c:v", "ffv1"], "mkv");
    let (Some(a), Some(b)) = (
        codec_media(codec.0, &codec.1, codec.2, "yuv420p", 64, 31, 30),
        codec_media(codec.0, &codec.1, codec.2, "yuv420p", 64, 29, 30),
    ) else {
        return;
    };
    let even = media(64, 36, Pat::Test, Tag::None);
    for (label, first, second) in [
        ("odd then odd", &a, &b),
        ("odd then even", &a, &even),
        ("even then odd", &even, &a),
    ] {
        let tl = timeline(
            vec![vec![
                clip(first, 0.0, 0.4, 0.0),
                dissolve_into(clip(second, 0.0, 0.4, 0.4), TransitionKind::DipToWhite, 0.2),
            ]],
            frame,
        );
        let case = format!("checker/b/r4/export dip to white {label}");
        if let Some(out) = sweep_guarded(&case, &tl, &dedup(&[first, second]), 30.0, 0.8) {
            judged += 1;
            fam.add(&case, &out);
        }
    }
    let even_only = timeline(
        vec![vec![
            clip(&even, 0.0, 0.4, 0.0),
            dissolve_into(clip(&even, 0.4, 0.8, 0.4), TransitionKind::DipToWhite, 0.2),
        ]],
        frame,
    );
    let case = "checker/b/r4/export dip to white even then even";
    if let Some(out) = sweep_guarded(case, &even_only, std::slice::from_ref(&even), 30.0, 0.8) {
        judged += 1;
        fam.add(case, &out);
    }
    assert!(judged > 3, "too few cases could be made: {judged}");
    fam.finish("dip to white, no allowance");
}

/// An odd-height picture off the origin in an export, in a format that is not `yuv420p` (10, 9 and 12-bit, `yuvj420p`, `nv12`,
/// `p010le`, an alpha plane): the chain's `format=yuv420p` converts it, `scale` converts into the buffer `pad` hands it and `pad`
/// fills in place, so the odd row goes; the plan refuses it, on every frame. (Drawn anyway, a run with the scratch patch, the frames
/// are not the export's.) The formats the plan resizes or never pads (gray, 4:2:2, 4:4:4, RGB) and an even picture of the same
/// formats are not refused for it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_odd_height_off_the_origin_in_other_formats() {
    let mut fam = Family::default();
    let mut judged = 0;
    let sizes = [
        (64, 31, 64, 36),
        (64, 29, 64, 36),
        (64, 13, 64, 36),
        (64, 33, 64, 40),
        (64, 5, 64, 36),
    ];
    let sizes = &sizes[..if r4_quick() { 1 } else { sizes.len() }];
    let mut necessity: Vec<String> = Vec::new();
    for (pix, codec, args, ext) in [
        ("yuv420p10le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv420p10le", "ffvhuff", vec!["-c:v", "ffvhuff"], "mkv"),
        ("yuv420p12le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv420p9le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuvj420p", "mjpeg", vec!["-c:v", "mjpeg", "-q:v", "1"], "mkv"),
        ("nv12", "rawvideo", vec!["-c:v", "rawvideo"], "mkv"),
        ("p010le", "rawvideo", vec!["-c:v", "rawvideo"], "mkv"),
    ] {
        for &(sw, sh, fw, fh) in sizes {
            let Some(a) = r4_source(codec, &args, ext, pix, sw, sh) else {
                eprintln!("{codec} {pix} {sw}x{sh}: this FFmpeg cannot make it");
                continue;
            };
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            let case = format!("checker/b/r4/export format {pix} {codec} {sw}x{sh} in {fw}x{fh}");
            if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                judged += 1;
                fam.add(&case, &out);
                expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::Refused);
                if patched_out() && (sw, sh) == (64, 31) {
                    let any = guarded(&case, || {
                        anyway(&case, &tl, std::slice::from_ref(&a), &at_fps(30.0), 9, compositor(), 5)
                    });
                    necessity.extend(any.map(|a| format!("{case}: {:?}", a.needed())));
                }
            }
            // The same format at an even height one row taller: no odd row, not refused for it.
            let Some(e) = r4_source(codec, &args, ext, pix, sw, sh + 1) else {
                continue;
            };
            let tl = timeline(vec![vec![clip(&e, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            let case = format!("checker/b/r4/export format even {pix} {codec} {sw}x{} in {fw}x{fh}", sh + 1);
            if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&e), 30.0, 0.3) {
                judged += 1;
                fam.add(&case, &out);
                expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::NotRefused);
            }
        }
    }
    // (The alpha source: `yuva420p` is not `yuv420p` either.)
    for (sw, sh, fw, fh) in [(64, 31, 64, 36), (64, 13, 64, 36)] {
        let a = alpha_media(sw, sh, Tag::None);
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
        let case = format!("checker/b/r4/export format yuva420p {sw}x{sh} in {fw}x{fh}");
        if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
            judged += 1;
            fam.add(&case, &out);
            // (An alpha picture is refused as it is; the pad's reason is not needed to stop it, and is not judged.)
            expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::Either);
            if !patched_out() && out.drawn != 0 {
                fam.failures
                    .push(format!("{case}: {} frames of an alpha picture were drawn", out.drawn));
            }
        }
    }
    // (Formats the pad never sees as an odd 4:2:0 picture: `scale` resamples or converts them before it.)
    for (pix, codec, args, ext) in [
        ("gray", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv422p", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuv444p", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("gbrp", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("gray10le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
    ] {
        for (sw, sh, fw, fh) in [(64, 31, 64, 36), (64, 13, 64, 36)] {
            let Some(a) = codec_media(codec, &args, ext, pix, sw, sh, 30) else {
                continue;
            };
            let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
            let case = format!("checker/b/r4/export format resampled {pix} {sw}x{sh} in {fw}x{fh}");
            if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
                judged += 1;
                fam.add(&case, &out);
                expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::NotRefused);
            }
        }
    }
    // (Cover and Blur never pad a picture that fills the frame; a 10-bit odd picture is not refused for the pad there.)
    for (fit, label) in [(Fit::Cover, "cover"), (Fit::Blur, "blur")] {
        let Some(a) = codec_media("ffv1", &["-c:v", "ffv1"], "mkv", "yuv420p10le", 128, 71, 30) else {
            continue;
        };
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], Some(Delivery::new(128, 80, fit)));
        let case = format!("checker/b/r4/export format 10-bit {label} 128x71 in 128x80");
        if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.3) {
            judged += 1;
            fam.add(&case, &out);
            expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::NotRefused);
        }
    }
    for line in &necessity {
        eprintln!("NECESSITY {line}");
    }
    assert!(
        judged > if r4_quick() { 10 } else { 60 },
        "too few cases could be made: {judged}"
    );
    fam.finish("odd-height pad off the origin in other formats, in an export");
}

/// A chain on the alpha plane in an export, with an odd-height picture off the origin: a crossfade's incoming clip, a mask, a chroma
/// key, at several gaps, from several decoders, over another track and under one, with a fade, at 24 and 29.97 fps. Every frame of
/// such a clip is refused for the pad, and none of the frames of a clip that is not on the alpha plane; drawn anyway (a run with the
/// scratch patch) the frames are not the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_alpha_plane_chains_of_odd_height_pictures_are_refused() {
    use kerf_core::VideoEffect as E;
    let mut fam = Family::default();
    let mut judged = 0;
    let mut necessity: Vec<String> = Vec::new();
    // (Full height, so the odd last row is under the mask: a mask that cut it out would make the row's fate invisible.)
    let mask = kerf_core::Mask {
        shape: kerf_core::MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 0.8,
        height: 1.0,
        feather: 0.0,
        inverted: false,
    };
    for (codec, args, ext) in r3_codecs()
        .into_iter()
        .filter(|(c, ..)| ["ffv1", "ffvhuff", "rawvideo", "utvideo", "mpeg4"].contains(c))
        .filter(|(c, ..)| !r4_quick() || *c == "ffv1")
    {
        for (sw, sh, fw, fh) in [
            (64, 31, 64, 36),
            (64, 13, 64, 36),
            (64, 5, 64, 36),
            (64, 33, 64, 40),
            (96, 53, 96, 64),
            (320, 179, 320, 200),
        ]
        .into_iter()
        .take(if r4_quick() { 1 } else { 6 })
        {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            let frame = Some(Delivery::new(fw, fh, Fit::Contain));
            let even = media(fw, fh, Pat::Test, Tag::None);
            for fps in [30.0, 24.0, 30000.0 / 1001.0] {
                if fps != 30.0 && (!(codec == "ffv1" || codec == "ffvhuff") || (sw, sh) != (64, 31)) {
                    continue;
                }
                // (label, cut, assets, seconds swept, the window of the timeline the alpha-plane clip is on screen in)
                type Case = (String, Timeline, Vec<Asset>, f64, Option<(f64, f64)>);
                let mut cases: Vec<Case> = Vec::new();
                let mut masked = clip(&a, 0.0, 0.4, 0.0);
                masked.mask = Some(mask);
                cases.push((
                    "mask".into(),
                    timeline(vec![vec![masked]], frame),
                    vec![a.clone()],
                    0.4,
                    Some((0.0, 0.4)),
                ));
                let mut keyed = clip(&a, 0.0, 0.4, 0.0);
                keyed.effects = vec![E::ChromaKey {
                    color: "0x00ff00".into(),
                    similarity: 0.1,
                    blend: 0.05,
                }];
                cases.push((
                    "chroma key".into(),
                    timeline(vec![vec![keyed]], frame),
                    vec![a.clone()],
                    0.4,
                    Some((0.0, 0.4)),
                ));
                cases.push((
                    "crossfade in over even".into(),
                    timeline(
                        vec![vec![
                            clip(&even, 0.0, 0.4, 0.0),
                            dissolve_into(clip(&a, 0.0, 0.4, 0.4), TransitionKind::Crossfade, 0.2),
                        ]],
                        frame,
                    ),
                    dedup(&[&a, &even]),
                    0.8,
                    Some((0.4, 0.8)),
                ));
                cases.push((
                    "crossfade in over odd".into(),
                    timeline(
                        vec![vec![
                            clip(&a, 0.0, 0.4, 0.0),
                            dissolve_into(clip(&a, 0.0, 0.4, 0.4), TransitionKind::Crossfade, 0.2),
                        ]],
                        frame,
                    ),
                    vec![a.clone()],
                    0.8,
                    Some((0.4, 0.8)),
                ));
                cases.push((
                    "crossfade out of odd".into(),
                    timeline(
                        vec![vec![
                            clip(&a, 0.0, 0.4, 0.0),
                            dissolve_into(clip(&even, 0.0, 0.4, 0.4), TransitionKind::Crossfade, 0.2),
                        ]],
                        frame,
                    ),
                    dedup(&[&a, &even]),
                    0.8,
                    None,
                ));
                let mut over = clip(&a, 0.0, 0.4, 0.0);
                over.mask = Some(mask);
                cases.push((
                    "masked odd over even".into(),
                    timeline(vec![vec![clip(&even, 0.0, 0.4, 0.0)], vec![over]], frame),
                    dedup(&[&a, &even]),
                    0.4,
                    Some((0.0, 0.4)),
                ));
                let mut under = clip(&a, 0.0, 0.4, 0.0);
                under.mask = Some(mask);
                let mut over = clip(&even, 0.0, 0.4, 0.0);
                over.transform.opacity = 0.5;
                cases.push((
                    "masked odd under translucent even".into(),
                    timeline(vec![vec![under], vec![over]], frame),
                    dedup(&[&a, &even]),
                    0.4,
                    Some((0.0, 0.4)),
                ));
                for (label, tl, assets, secs, window) in cases {
                    let case = format!("checker/b/r4/export alpha {label} {codec} {sw}x{sh} in {fw}x{fh} {fps:.2}");
                    if let Some(out) = sweep_guarded(&case, &tl, &assets, fps, secs) {
                        judged += 1;
                        fam.add(&case, &out);
                        let n = out.refused_for(R4_ALPHA);
                        let (least, most) = window.map_or((0, 0), |(from, to)| r4_frames_between(fps, from, to, out.frames));
                        if !patched_out() && (n < least || n > most) {
                            fam.failures.push(format!(
                                "{case}: {n} frames refused for \"{R4_ALPHA}\", expected {least} to {most} ({:?})",
                                out.reasons
                            ));
                        }
                    }
                    if patched_out() && codec == "ffv1" && window.is_some() && (sw, sh) != (64, 5) && fps == 30.0 {
                        let any = guarded(&case, || {
                            anyway(&case, &tl, &assets, &at_fps(fps), (secs * fps) as usize, compositor(), 6)
                        });
                        necessity.extend(any.map(|a| format!("{case}: {:?}", a.needed())));
                    }
                }
            }
        }
    }
    for line in &necessity {
        eprintln!("NECESSITY {line}");
    }
    assert!(
        judged > if r4_quick() { 5 } else { 60 },
        "too few cases could be made: {judged}"
    );
    fam.finish_refusing("alpha-plane chains of odd-height pictures");
}

/// 64 x `h` noise tagged `tag`, in `pix`, with the range flag `range` (`tv` / `pc`) the container carries: FFV1 in Matroska.
fn r4_tag_media(name: &str, tag: Tag, pix: &str, range: Option<&str>, w: u32, h: u32) -> Asset {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-b-media-{}", build_key()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("r4-tag-{name}-{w}x{h}.mkv"));
    if !path.exists() {
        let range_filter = range.map(|r| format!(",setparams=range={r}")).unwrap_or_default();
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=0.8,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'{}{range_filter}",
            tag.filter()
        );
        let tmp = dir.join(format!("r4-tag-{name}-{w}x{h}.tmp.mkv"));
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

/// Colour tags, range flags and the delivery's scaler on an odd-height `yuv420p` picture off the origin in an export: `scale` is a
/// pass-through only while it has nothing to convert, and a range or a matrix to convert would put the picture into `pad`'s buffer
/// (the odd row would go). The plan draws these (it refuses only formats and the alpha plane); every drawn frame is the export's,
/// alone, in sequence and stacked, with each pair of tags.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_odd_height_off_the_origin_in_every_tag_range_and_scaler() {
    r4_export_tag_family(false);
}

/// The same pictures flagged full range (`pc`), odd and even in height: a letterboxed picture's bars are `pad`'s black, which
/// FFmpeg makes in the picture's range. Every drawn frame is the export's. (Even heights are in it so that a miss here is told apart
/// from the odd row's.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_full_range_flagged_letterboxed_pictures_are_exact() {
    r4_export_tag_family(true);
}

/// The cases of the two tests above: `pc` the full-range flagged pictures, else the others.
fn r4_export_tag_family(pc: bool) {
    let mut fam = Family::default();
    let mut judged = 0;
    let frame = Some(Delivery::new(64, 36, Fit::Contain));
    let even = media(64, 36, Pat::Test, Tag::None);
    for (h, fh) in [(31, 36), (13, 36), (33, 40), (30, 36), (32, 40)] {
        let frame = if fh == 36 {
            frame
        } else {
            Some(Delivery::new(64, fh, Fit::Contain))
        };
        let tagged: Vec<(&str, Asset)> = vec![
            ("untagged", r4_tag_media("untagged", Tag::None, "yuv420p", None, 64, h)),
            ("bt709", r4_tag_media("bt709", Tag::Bt709, "yuv420p", None, 64, h)),
            ("bt601", r4_tag_media("smpte170m", Tag::Smpte170m, "yuv420p", None, 64, h)),
            ("bt470bg", r4_tag_media("bt470bg", Tag::Bt470bg, "yuv420p", None, 64, h)),
            ("bt2020", r4_tag_media("bt2020", Tag::Bt2020, "yuv420p", None, 64, h)),
            ("tv flag", r4_tag_media("tv", Tag::None, "yuv420p", Some("tv"), 64, h)),
            ("pc flag", r4_tag_media("pc", Tag::None, "yuv420p", Some("pc"), 64, h)),
            (
                "bt709 pc flag",
                r4_tag_media("bt709pc", Tag::Bt709, "yuv420p", Some("pc"), 64, h),
            ),
            (
                "bt2020 pc flag",
                r4_tag_media("bt2020pc", Tag::Bt2020, "yuv420p", Some("pc"), 64, h),
            ),
        ];
        for (label, a) in &tagged {
            if label.contains("pc flag") != pc {
                continue;
            }
            for fps in [30.0, 24.0] {
                let tl = timeline(vec![vec![clip(a, 0.0, 0.4, 0.0)]], frame);
                let case = format!("checker/b/r4/export tag {label} 64x{h} in 64x{fh} {fps}");
                if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(a), fps, 0.4) {
                    judged += 1;
                    fam.add(&case, &out);
                    expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::NotRefused);
                    expect_refusal(&mut fam, &case, &out, R4_ALPHA, Want::NotRefused);
                }
            }
        }
        if h != 31 {
            continue;
        }
        let pairs: Vec<(&str, &str)> = vec![
            ("untagged", "bt709"),
            ("bt709", "untagged"),
            ("bt709", "bt2020"),
            ("bt2020", "bt601"),
            ("bt601", "bt709"),
            ("untagged", "pc flag"),
            ("pc flag", "untagged"),
            ("bt709", "bt709 pc flag"),
            ("bt709 pc flag", "bt709"),
        ];
        let find = |n: &str| tagged.iter().find(|(l, _)| *l == n).map(|(_, a)| a).expect("tag");
        for (x, y) in pairs {
            if (x.contains("pc flag") || y.contains("pc flag")) != pc {
                continue;
            }
            let (a, b) = (find(x), find(y));
            for (label, tracks) in [
                ("sequence", vec![vec![clip(a, 0.0, 0.4, 0.0), clip(b, 0.0, 0.4, 0.4)]]),
                ("stacked", vec![vec![clip(a, 0.0, 0.4, 0.0)], vec![clip(b, 0.0, 0.4, 0.0)]]),
                (
                    "odd over even",
                    vec![vec![clip(&even, 0.0, 0.4, 0.0)], vec![clip(b, 0.0, 0.4, 0.0)]],
                ),
                (
                    "even over odd",
                    vec![vec![clip(a, 0.0, 0.4, 0.0)], vec![clip(&even, 0.0, 0.4, 0.0)]],
                ),
            ] {
                let tl = timeline(tracks, frame);
                let case = format!("checker/b/r4/export tag pair {x}/{y} {label}");
                if let Some(out) = sweep_guarded(&case, &tl, &dedup(&[a, b, &even]), 30.0, 0.8) {
                    judged += 1;
                    fam.add(&case, &out);
                }
            }
        }
    }
    // (The delivery's scaler: it does not resample an unscaled picture, and must not change what `pad` does with it.)
    let a = r4_tag_media("untagged", Tag::None, "yuv420p", None, 64, 31);
    for scaler in ["bicubic", "bilinear", "lanczos", "neighbor", "spline"]
        .into_iter()
        .filter(|_| !pc)
    {
        let opts = ExportOptions {
            fps: Some(30.0),
            scaler: Some(scaler.to_string()),
            ..ExportOptions::default()
        };
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.4, 0.0)]], frame);
        let case = format!("checker/b/r4/export scaler {scaler} 64x31 in 64x36");
        let out = sweep(&case, &tl, std::slice::from_ref(&a), &opts, 12);
        judged += 1;
        fam.add(&case, &out);
        expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::NotRefused);
        let case = format!("checker/b/r4/still scaler {scaler} 64x31 in 64x36");
        let out = r4_still(&case, &tl, std::slice::from_ref(&a), &opts, &R4_TIMES);
        judged += 1;
        fam.add_r4(&case, &out);
        r4_expect(&mut fam, &case, &out, R4_STILL, Want::NotRefused);
    }
    assert!(judged > if pc { 10 } else { 60 }, "too few cases could be made: {judged}");
    fam.finish("odd-height pad off the origin in every tag, range and scaler");
}

/// An odd-height untagged HD picture (taller than 576 rows) beside a clip that names a matrix is labelled BT.709
/// (`setparams=colorspace=bt709` ahead of its chain): the label states what the picture is and moves no pixel, so it must not change
/// what `pad` does with an odd picture off the origin, in an export or in a still. Alone, in sequence and stacked, odd over odd and
/// over even; every drawn frame is the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_odd_height_untagged_hd_beside_a_matrix_is_labelled_bt709() {
    let mut fam = Family::default();
    let mut judged = 0;
    let hd = r4_tag_media("hd", Tag::None, "yuv420p", None, 640, 599);
    let named = r4_tag_media("hdnamed", Tag::Bt709, "yuv420p", None, 640, 360);
    let named_odd = r4_tag_media("hdnamedodd", Tag::Bt709, "yuv420p", None, 640, 595);
    let sd_named = r4_tag_media("hdnamedsd", Tag::Smpte170m, "yuv420p", None, 640, 571);
    let frame = Some(Delivery::new(640, 608, Fit::Contain));
    type Case<'a> = (&'a str, Vec<Vec<Clip>>, Vec<Asset>);
    let cases: Vec<Case> = vec![
        ("alone", vec![vec![clip(&hd, 0.0, 0.4, 0.0)]], vec![hd.clone()]),
        (
            "hd then named",
            vec![vec![clip(&hd, 0.0, 0.4, 0.0), clip(&named, 0.0, 0.4, 0.4)]],
            dedup(&[&hd, &named]),
        ),
        (
            "named then hd",
            vec![vec![clip(&named, 0.0, 0.4, 0.0), clip(&hd, 0.0, 0.4, 0.4)]],
            dedup(&[&hd, &named]),
        ),
        (
            "hd then named odd",
            vec![vec![clip(&hd, 0.0, 0.4, 0.0), clip(&named_odd, 0.0, 0.4, 0.4)]],
            dedup(&[&hd, &named_odd]),
        ),
        (
            "named odd then hd",
            vec![vec![clip(&named_odd, 0.0, 0.4, 0.0), clip(&hd, 0.0, 0.4, 0.4)]],
            dedup(&[&hd, &named_odd]),
        ),
        (
            "hd over named",
            vec![vec![clip(&named, 0.0, 0.8, 0.0)], vec![clip(&hd, 0.0, 0.8, 0.0)]],
            dedup(&[&hd, &named]),
        ),
        (
            "named over hd",
            vec![vec![clip(&hd, 0.0, 0.8, 0.0)], vec![clip(&named, 0.0, 0.8, 0.0)]],
            dedup(&[&hd, &named]),
        ),
        (
            "hd over named odd",
            vec![vec![clip(&named_odd, 0.0, 0.8, 0.0)], vec![clip(&hd, 0.0, 0.8, 0.0)]],
            dedup(&[&hd, &named_odd]),
        ),
        (
            "hd over a named SD odd",
            vec![vec![clip(&sd_named, 0.0, 0.8, 0.0)], vec![clip(&hd, 0.0, 0.8, 0.0)]],
            dedup(&[&hd, &sd_named]),
        ),
    ];
    for (label, tracks, assets) in cases {
        let tl = timeline(tracks, frame);
        let case = format!("checker/b/r4/export label {label}");
        if let Some(out) = sweep_guarded(&case, &tl, &assets, 30.0, 0.8) {
            judged += 1;
            fam.add(&case, &out);
        }
        let case = format!("checker/b/r4/still label {label}");
        let out = r4_still(&case, &tl, &assets, &ExportOptions::default(), &[0.1, 0.3, 0.5, 0.7]);
        judged += 1;
        fam.add_r4(&case, &out);
    }
    assert!(judged > 10, "too few cases could be made: {judged}");
    fam.finish("odd-height untagged HD beside a named matrix");
}

/// A picture whose pixel format was never recorded (an asset saved before the field existed) is assumed nothing: its odd-height pad
/// off the origin is refused in an export and in a still, whatever FFmpeg does with it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_odd_height_of_an_unrecorded_pixel_format_is_refused() {
    let mut fam = Family::default();
    let mut judged = 0;
    for (sw, sh, fw, fh) in [(64, 31, 64, 36), (64, 13, 64, 36), (64, 33, 64, 40)] {
        let mut a = r4_tag_media("untagged", Tag::None, "yuv420p", None, sw, sh);
        for s in a.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            s.pix_fmt = None;
        }
        let tl = timeline(vec![vec![clip(&a, 0.0, 0.4, 0.0)]], Some(Delivery::new(fw, fh, Fit::Contain)));
        let case = format!("checker/b/r4/export unrecorded format {sw}x{sh} in {fw}x{fh}");
        if let Some(out) = sweep_guarded(&case, &tl, std::slice::from_ref(&a), 30.0, 0.4) {
            judged += 1;
            fam.add(&case, &out);
            expect_refusal(&mut fam, &case, &out, R4_FORMAT, Want::Refused);
        }
        let case = format!("checker/b/r4/still unrecorded format {sw}x{sh} in {fw}x{fh}");
        let out = r4_still(&case, &tl, std::slice::from_ref(&a), &ExportOptions::default(), &R4_TIMES);
        judged += 1;
        fam.add_r4(&case, &out);
        r4_expect(&mut fam, &case, &out, R4_STILL, Want::Refused);
    }
    assert!(judged > 3, "too few cases could be made: {judged}");
    fam.finish_refusing("odd-height pad of an unrecorded pixel format");
}

/// The preview's stills are FFmpeg's at a smaller width: the canvas shrinks, and a picture whose size is the fitted window's at that
/// width is not resampled there (640x319 in a 1280x720 frame at 640 wide) while it is at the full size. A plain `yuv420p` odd-height
/// picture off the origin is drawn without its odd row at each width, and every drawn still is FFmpeg's at that width.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_odd_height_at_preview_widths() {
    let mut fam = Family::default();
    let mut judged = 0;
    for (codec, args, ext) in r3_codecs().into_iter().filter(|(c, ..)| *c == "ffv1" || *c == "ffvhuff") {
        for (sw, sh) in [(640, 319), (320, 159), (960, 479), (1280, 711), (480, 239), (1280, 359)] {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            let tl = timeline(
                vec![vec![clip(&a, 0.0, 0.5, 0.0)]],
                Some(Delivery::new(1280, 720, Fit::Contain)),
            );
            for width in [1280, 960, 640, 540, 480, 320, 160] {
                let case = format!("checker/b/r4/still preview {codec} {sw}x{sh} in 1280x720 at {width} wide");
                judged += 1;
                let out = r4_still_at(
                    &case,
                    &tl,
                    std::slice::from_ref(&a),
                    &ExportOptions::default(),
                    &[0.1, 0.3],
                    width,
                );
                fam.add_r4(&case, &out);
                r4_expect(&mut fam, &case, &out, R4_STILL, Want::NotRefused);
            }
        }
    }
    assert!(judged > 40, "too few cases could be made: {judged}");
    fam.finish("odd-height stills at preview widths");
}

/// Which export chains drop the odd last row of a picture off the origin, asked of FFmpeg alone: the export of a cut with the odd
/// picture against the export of the same cut with the picture's first rows (the odd row gone), at a gap where both sit at the same
/// offset. Equal frames mean `pad` dropped the row. Each kind is then set against the plan's verdict on that frame: a chain FFmpeg
/// drops that the plan neither refuses for the pad nor for anything else is a frame drawn with a row FFmpeg does not show (the
/// exactness cases would fail on it; this names it). `EXPORT ROW` lines also show where the plan leans on another refusal alone
/// (a blend, say), which a lane that lifts that refusal has to meet.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_export_which_chains_drop_the_odd_row() {
    use kerf_core::VideoEffect as E;
    let mut fam = Family::default();
    let mut lines: Vec<String> = Vec::new();
    let full_mask = kerf_core::Mask {
        shape: kerf_core::MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 1.0,
        height: 1.0,
        feather: 0.0,
        inverted: false,
    };
    let eq = Color {
        contrast: 1.2,
        saturation: 1.1,
        ..Color::default()
    };
    let channel = |prop: kerf_core::Property, a: f64, b: f64| kerf_core::PropertyTrack {
        prop,
        keys: [(0.0, a), (0.4, b)]
            .iter()
            .map(|&(t, v)| kerf_core::PropertyKey::new(t, v))
            .collect(),
    };
    let grade = grades_b().remove(0).1;
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    // (label, edit, the transition the clip comes in through, whether the clip is a second one)
    let kinds: Vec<(&str, Edit, Option<TransitionKind>)> = vec![
        ("plain", Box::new(|_| {}), None),
        ("eq", Box::new(move |c| c.color = eq), None),
        (
            "grade and eq",
            Box::new(move |c| {
                c.color = eq;
                c.grade = grade.clone();
            }),
            None,
        ),
        ("blur", Box::new(|c| c.effects = vec![E::Blur { sigma: 1.5 }]), None),
        ("sharpen", Box::new(|c| c.effects = vec![E::Sharpen { amount: 1.0 }]), None),
        ("grayscale", Box::new(|c| c.effects = vec![E::Grayscale]), None),
        ("invert", Box::new(|c| c.effects = vec![E::Invert]), None),
        (
            "chroma key",
            Box::new(|c| {
                c.effects = vec![E::ChromaKey {
                    color: "0x00ff00".into(),
                    similarity: 0.05,
                    blend: 0.0,
                }];
            }),
            None,
        ),
        ("mask", Box::new(move |c| c.mask = Some(full_mask)), None),
        ("fade in", Box::new(|c| c.fade_in = 0.4), None),
        ("fade out", Box::new(|c| c.fade_out = 0.4), None),
        (
            "keyed colour",
            Box::new(move |c| c.channels = vec![channel(kerf_core::Property::Saturation, 1.0, 1.4)]),
            None,
        ),
        (
            "blend multiply",
            Box::new(|c| c.blend_mode = kerf_core::BlendMode::Multiply),
            None,
        ),
        (
            "blend screen",
            Box::new(|c| c.blend_mode = kerf_core::BlendMode::Screen),
            None,
        ),
        ("crossfade in", Box::new(|_| {}), Some(TransitionKind::Crossfade)),
        ("dip to black in", Box::new(|_| {}), Some(TransitionKind::DipToBlack)),
        ("slide in", Box::new(|_| {}), Some(TransitionKind::SlideLeft)),
        ("push in", Box::new(|_| {}), Some(TransitionKind::PushUp)),
    ];
    let even = media(64, 36, Pat::Test, Tag::None);
    let opts = at_fps(30.0);
    let mut formats: Vec<(String, Asset, Asset)> = Vec::new();
    for (pix, codec, args, ext) in [
        ("yuv420p10le", "ffv1", vec!["-c:v", "ffv1"], "mkv"),
        ("yuvj420p", "mjpeg", vec!["-c:v", "mjpeg", "-q:v", "1"], "mkv"),
        ("nv12", "rawvideo", vec!["-c:v", "rawvideo"], "mkv"),
    ] {
        if let Some(a) = r4_source(codec, &args, ext, pix, 64, 31) {
            let short = r4_first_rows_in(&a, 30, pix);
            formats.push((pix.to_string(), a, short));
        }
    }
    let ffv1 = ("ffv1", vec!["-c:v", "ffv1"], "mkv");
    let Some(odd) = codec_media(ffv1.0, &ffv1.1, ffv1.2, "yuv420p", 64, 31, 30) else {
        return;
    };
    let short = r4_first_rows(&odd, 30);
    let frame = Some(Delivery::new(64, 36, Fit::Contain));
    let mut studied: Vec<(String, Asset, Asset, &Edit, Option<TransitionKind>)> = Vec::new();
    for (label, edit, kind) in &kinds {
        studied.push((label.to_string(), odd.clone(), short.clone(), edit, *kind));
    }
    let nothing: Edit = Box::new(|_| {});
    for (pix, a, s) in &formats {
        studied.push((format!("format {pix}"), a.clone(), s.clone(), &nothing, None));
    }
    for (label, odd, short, edit, kind) in studied {
        let build = |a: &Asset| {
            let mut c = clip(a, 0.0, 0.4, if kind.is_some() { 0.4 } else { 0.0 });
            edit(&mut c);
            match kind {
                Some(k) => timeline(vec![vec![clip(&even, 0.0, 0.4, 0.0), dissolve_into(c, k, 0.2)]], frame),
                None => timeline(vec![vec![c]], frame),
            }
        };
        let (tl_odd, tl_short) = (build(&odd), build(&short));
        let case = format!("checker/b/r4/export row {label}");
        let assets_of = |a: &Asset| if kind.is_some() { dedup(&[a, &even]) } else { vec![a.clone()] };
        let (played_odd, played_short) = (
            try_export(&format!("{case} odd"), &tl_odd, &assets_of(&odd), &opts),
            try_export(&format!("{case} short"), &tl_short, &assets_of(&short), &opts),
        );
        let (played_odd, played_short) = match (played_odd, played_short) {
            (Ok(o), Ok(s)) => (o, s),
            (o, s) => {
                let why = |r: Result<Played, String>| r.err().map(|e| e.lines().last().unwrap_or("").to_string());
                let line = format!(
                    "EXPORT ROW [{}] {label:<18} FFmpeg cannot render it (odd: {:?}, short: {:?})",
                    build_key(),
                    why(o),
                    why(s)
                );
                eprintln!("{line}");
                lines.push(line);
                continue;
            }
        };
        let frames: Vec<usize> = if kind.is_some() { vec![20, 22] } else { vec![3, 6, 9] };
        let dropped: Vec<bool> = frames
            .iter()
            .map(|&k| played_odd.planes(k) == played_short.planes(k))
            .collect();
        let planner = Planner::new(&tl_odd, &assets_of(&odd), &opts, PlanRequest::motion(policy())).expect("planner");
        let size = (played_odd.w as u32, played_odd.h as u32);
        let mut verdicts: Vec<String> = Vec::new();
        let mut plan_draws = 0;
        for &k in &frames {
            let plan = planner.at_frame(k as u64).expect("plan");
            let why = plan.reasons(&compositor().caps(), size);
            if why.is_empty() {
                plan_draws += 1;
            }
            let pad = why.iter().map(ToString::to_string).any(|r| r.contains("pad of a"));
            let other = why.iter().map(ToString::to_string).any(|r| !r.contains("pad of a"));
            verdicts.push(match (why.is_empty(), pad, other) {
                (true, ..) => "drawn".to_string(),
                (_, true, false) => "refused for the pad".to_string(),
                (_, true, true) => "refused for the pad and more".to_string(),
                _ => "refused, not for the pad".to_string(),
            });
        }
        verdicts.dedup();
        let row = if dropped.iter().all(|d| *d) {
            "FFmpeg DROPS the odd row"
        } else if dropped.iter().all(|d| !*d) {
            "FFmpeg keeps the odd row"
        } else {
            "FFmpeg drops it on some frames"
        };
        let flag = if dropped.iter().any(|d| *d) && plan_draws > 0 {
            " <<< DRAWN WITH A ROW FFMPEG DROPS"
        } else {
            ""
        };
        let line = format!(
            "EXPORT ROW [{}] {label:<18} {row:<32} | plan: {}{flag}",
            build_key(),
            verdicts.join(", ")
        );
        eprintln!("{line}");
        if !flag.is_empty() && !patched_out() {
            fam.failures.push(format!("{case}: {line}"));
        }
        lines.push(line);
        fam.cases += 1;
    }
    let _ = std::fs::write(
        target_dir()
            .join("parity")
            .join(format!("r4-export-rows-{}.txt", build_key())),
        lines.join("\n"),
    );
    fam.finish_refusing("which export chains drop the odd row");
}

/// The dip to white with odd-height pictures in a still: no allowance (the base draws a dip to white's stills to within two
/// levels; the other lane tests hold it to that ladder, this one to nothing, so a failure here is the ladder).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_still_dip_to_white_stills_the_plan_draws_are_exact() {
    let mut fam = Family::default();
    let frame = Some(Delivery::new(64, 36, Fit::Contain));
    let (Some(a), Some(b)) = (
        codec_media("ffv1", &["-c:v", "ffv1"], "mkv", "yuv420p", 64, 31, 30),
        codec_media("ffv1", &["-c:v", "ffv1"], "mkv", "yuv420p", 64, 29, 30),
    ) else {
        return;
    };
    let even = media(64, 36, Pat::Test, Tag::None);
    for (label, first, second) in [
        ("odd then odd", &a, &b),
        ("odd then even", &a, &even),
        ("even then odd", &even, &a),
        ("even then even", &even, &even),
    ] {
        let tl = timeline(
            vec![vec![
                clip(first, 0.0, 0.4, 0.0),
                dissolve_into(clip(second, 0.0, 0.5, 0.4), TransitionKind::DipToWhite, 0.3),
            ]],
            frame,
        );
        for opts in [ExportOptions::default(), at_fps(24.0)] {
            let case = format!("checker/b/r4/still dip to white {label} {:?}", opts.fps);
            let out = r4_still(
                &case,
                &tl,
                &dedup(&[first, second]),
                &opts,
                &[0.05, 0.2, 0.3, 0.38, 0.45, 0.5, 0.55, 0.7],
            );
            fam.add_r4(&case, &out);
        }
    }
    fam.finish("dip to white stills, no allowance");
}

/// What FFmpeg does with the odd last row of a picture one row short of the frame (at the frame's origin), from each decoder, asked
/// of FFmpeg alone: its still, and its export, against the same cut with the picture's first rows. `ORIGIN ROW` lines name the
/// outcome per decoder (`drop` / `keep`); a still is the graph's (the still's `scale` converts into `pad`'s buffer), an export the
/// decoder's. Nothing is judged: the lines are the evidence for the refusal at the origin (what a Drops geometry would have to
/// know to lift it).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_r4_origin_which_decoders_drop_the_odd_row() {
    let opts = at_fps(30.0);
    let mut lines: Vec<String> = Vec::new();
    for (codec, args, ext) in r3_codecs() {
        for (sw, sh, fw, fh) in [(64, 35, 64, 36), (96, 53, 96, 54), (128, 71, 128, 72), (64, 39, 64, 40)] {
            let Some(a) = codec_media(codec, &args, ext, "yuv420p", sw, sh, 30) else {
                continue;
            };
            let short = r4_first_rows(&a, sh - 1);
            let frame = Some(Delivery::new(fw, fh, Fit::Contain));
            let odd_tl = timeline(vec![vec![clip(&a, 0.0, 0.4, 0.0)]], frame);
            let short_tl = timeline(vec![vec![clip(&short, 0.0, 0.4, 0.0)]], frame);
            let stills: Vec<Option<bool>> = [0.1, 0.2, 0.3]
                .iter()
                .map(|&t| r4_row_dropped((&odd_tl, &a), (&short_tl, &short), &opts, t))
                .collect();
            let name = |d: Option<bool>| match d {
                Some(true) => "drop",
                Some(false) => "keep",
                None => "?",
            };
            let case = format!("checker/b/r4/origin row {codec} {sw}x{sh}");
            let exported = match (
                try_export(&format!("{case} odd"), &odd_tl, std::slice::from_ref(&a), &opts),
                try_export(&format!("{case} short"), &short_tl, std::slice::from_ref(&short), &opts),
            ) {
                (Ok(o), Ok(s)) => [2usize, 5, 8]
                    .iter()
                    .map(|&k| Some(o.planes(k) == s.planes(k)))
                    .collect::<Vec<_>>(),
                _ => vec![None; 3],
            };
            let line = format!(
                "ORIGIN ROW [{}] {codec:<9} {sw}x{sh} in {fw}x{fh}: still {:?}, export {:?}",
                build_key(),
                stills.iter().map(|d| name(*d)).collect::<Vec<_>>(),
                exported.iter().map(|d| name(*d)).collect::<Vec<_>>()
            );
            eprintln!("{line}");
            lines.push(line);
        }
    }
    let _ = std::fs::write(
        target_dir()
            .join("parity")
            .join(format!("r4-origin-rows-{}.txt", build_key())),
        lines.join("\n"),
    );
    assert!(lines.len() > 10, "too few cases could be made: {}", lines.len());
}
