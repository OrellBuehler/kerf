//! Lane A, work package T6 (still pictures on the GPU path), checked a second time and independently, on the merged tree
//! (T6 merged into the lane B/E tree): stills, PNG and JPEG of every pixel format FFmpeg decodes, odd / tiny / huge sizes, every
//! fit, moving zooms and pans on both routes (the moving still's own `perspective` of a scaled copy, and the placement laid from the
//! fit picture FFmpeg makes), rotation, opacity, fades, masks, effects and titles over stills, dissolves and dips, cuts that name a matrix,
//! full-range footage, and the exemption of a resized still whose fit picture FFmpeg makes from the lane B refusals.
//!
//! The comparison is the project's exact one with no bound anywhere: a Still frame is FFmpeg's own still (`timeline_frame_yuv_cancellable`,
//! 4:4:4 planes) against the compositor's 4:2:0 planes taken to 4:4:4 by FFmpeg; an export frame is the lossless export's `yuv420p` planes
//! against the compositor's planes. Every frame is `Exact`, `Off` (drawn and different: a failure), `Refused` (the plan or the render said
//! `Unsupported`: FFmpeg draws it) or `Failed` (the compositor or FFmpeg errored where it should have drawn). A family line counts each.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_a_stills -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
//!
//! The `anyway` tests draw refused frames all the same and judge nothing (`WIDER` is a refusal that was not needed, `NECESSARY` one that
//! was); the plan-level `StillMotion` refusals stay in force unless the build is patched (a scratch patch of `src/` that reads `KERF_MUT`, a
//! comma list of the refusals to skip, with `KERF_SCEN`, a `;` list of scenario-name parts, to pick the scenarios of the compact suite).

#![allow(
    clippy::print_stderr,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::semicolon_if_nothing_returned
)]

#[path = "common/t6.rs"]
mod t6_files;

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    Asset, Clip, Color, CompositeColorPolicy, Container, Delivery, ExportOptions, Fit, Keyframe, OriginalMedia, PlanRequest,
    Planner, Project, RateControl, RenderPlan, StreamKind, Timeline, Track, Transition, TransitionKind,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames, YuvFrame};

// ---- the machinery -----------------------------------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

/// The compositor as an app has it after `prepare`: everything this FFmpeg's probes measured is in the caps.
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
        let _ = kerf_gpu::still_probe::kinds();
        let _ = kerf_gpu::blend::kernels();
        Compositor::new(gpu()).expect("a compositor")
    })
}

/// Every kind of picture in the caps, whatever the probe said: what a refusal about a kind is drawn through.
fn compositor_all() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| {
        let _ = compositor();
        Compositor::new(gpu())
            .expect("a compositor")
            .with_still_motion(kerf_core::StillKinds::ALL)
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
        "the composite colour probe could not read {}",
        kerf_core::ffmpeg_path()
    );
    measured
}

fn negotiated() -> bool {
    policy() != CompositeColorPolicy::FixedBt601
}

fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("target/")
        .to_path_buf()
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

fn ffmpeg(args: &[&str]) {
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
}

fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

// ---- outcomes and the tally ---------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Out {
    Exact,
    /// Drawn, and different: `(plane lines)`.
    Off(String),
    /// Refused by the plan or the render: the kinds, and the text.
    Refused(String, String),
    /// Errored where it should have drawn or compared.
    Failed(String),
}

#[derive(Default, Clone)]
struct Tally {
    exact: usize,
    off: usize,
    refused: usize,
    failed: usize,
    why: BTreeMap<String, usize>,
    /// Frames drawn that a lane B refusal would have refused without the exemption: `(exact, inexact)` by refusal.
    exempt: BTreeMap<&'static str, (usize, usize)>,
}

fn tallies() -> &'static Mutex<BTreeMap<String, Tally>> {
    static T: OnceLock<Mutex<BTreeMap<String, Tally>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn count(family: &str, out: &Out, exempt: &[&'static str]) {
    let mut all = tallies().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let t = all.entry(family.to_string()).or_default();
    if matches!(out, Out::Exact | Out::Off(_)) {
        for kind in exempt {
            let e = t.exempt.entry(kind).or_default();
            if matches!(out, Out::Exact) {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
        }
    }
    match out {
        Out::Exact => t.exact += 1,
        Out::Off(_) => t.off += 1,
        Out::Refused(kinds, _) => {
            t.refused += 1;
            *t.why.entry(kinds.clone()).or_default() += 1;
        }
        Out::Failed(_) => t.failed += 1,
    }
}

/// Print the families whose name starts with `prefix`; returns `(drawn, exact, off, refused, failed)`.
fn report(prefix: &str) -> (usize, usize, usize, usize, usize) {
    let all = tallies().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut exact, mut off, mut refused, mut failed) = (0, 0, 0, 0);
    for (name, t) in all.iter().filter(|(n, _)| n.starts_with(prefix)) {
        let why: Vec<String> = t.why.iter().map(|(k, n)| format!("{k} x{n}")).collect();
        let ex: Vec<String> = t
            .exempt
            .iter()
            .map(|(k, (e, o))| format!("{k}: {e} exact {o} inexact"))
            .collect();
        eprintln!(
            "A6 FAMILY {name:<48} drawn {:>4}  exact {:>4}  inexact {:>3}  refused {:>4}  failed {:>2}  [{}]{}",
            t.exact + t.off,
            t.exact,
            t.off,
            t.refused,
            t.failed,
            why.join("; "),
            if ex.is_empty() {
                String::new()
            } else {
                format!("  EXEMPTED[{}]", ex.join("; "))
            }
        );
        exact += t.exact;
        off += t.off;
        refused += t.refused;
        failed += t.failed;
    }
    eprintln!(
        "A6 TOTAL {prefix}: drawn {}  exact {exact}  inexact {off}  refused {refused}  failed {failed}",
        exact + off
    );
    (exact + off, exact, off, refused, failed)
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn reasons_of(plan: &RenderPlan, comp: &Compositor, size: (u32, u32)) -> Vec<(String, String)> {
    plan.reasons(&comp.caps(), size)
        .iter()
        .map(|u| (u.kind().to_string(), u.to_string()))
        .collect()
}

fn refused_with(why: &[(String, String)]) -> Out {
    let mut kinds: Vec<&str> = why.iter().map(|(k, _)| k.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    Out::Refused(
        kinds.join("+"),
        why.iter()
            .map(|(_, t)| t.chars().take(160).collect::<String>())
            .collect::<Vec<_>>()
            .join("; "),
    )
}

// ---- the timelines -------------------------------------------------------------------------------------------

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

fn delivery(w: u32, h: u32, fit: Fit, fps: f64) -> Option<Delivery> {
    Some(Delivery::new(w, h, fit).with_fps(Some(fps)).expect("a rate"))
}

fn cover(w: u32, h: u32) -> Option<Delivery> {
    delivery(w, h, Fit::Cover, 10.0)
}

fn key(time: f64, scale: f64, pos_x: f64, pos_y: f64) -> Keyframe {
    Keyframe {
        easing: Default::default(),
        time,
        scale,
        pos_x,
        pos_y,
        rotation: 0.0,
        opacity: 1.0,
    }
}

/// A move: zoom and position at the start and at the end.
type Mv = (f64, (f64, f64));

const HOME: Mv = (1.0, (0.0, 0.0));
/// Inside the frame the whole way: the moving still's own route.
const IN_A: (Mv, Mv) = ((1.3, (0.03, 0.02)), (1.05, (-0.01, 0.0)));
const IN_B: (Mv, Mv) = ((1.2, (0.04, 0.03)), HOME);
/// Panned off the picture's edge at the start: the placement, with FFmpeg making the fit picture.
const OFF_A: (Mv, Mv) = ((1.0, (-0.04, 0.0)), (1.2, (0.04, -0.02)));
const OFF_B: (Mv, Mv) = ((1.0, (-0.03, 0.0)), (1.18, (0.03, -0.02)));
/// A zoom out (the picture leaves bars under Contain; under Cover it leaves the frame bare).
const OUT_A: (Mv, Mv) = ((0.9, (0.0, 0.0)), (0.7, (0.04, 0.0)));

fn moving(photo: &Asset, start: f64, secs: f64, (a, b): (Mv, Mv)) -> Clip {
    let mut c = Clip::new(photo.id, 0.0, secs, start);
    c.keyframes = vec![key(0.0, a.0, a.1 .0, a.1 .1), key(secs, b.0, b.1 .0, b.1 .1)];
    c
}

fn still(photo: &Asset, start: f64, secs: f64) -> Clip {
    Clip::new(photo.id, 0.0, secs, start)
}

fn at_fps(fps: f64) -> ExportOptions {
    ExportOptions {
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

// ---- the files -----------------------------------------------------------------------------------------------

fn media_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-t6-media");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn png(w: u32, h: u32) -> Asset {
    t6_files::png(w, h)
}

fn jpeg(w: u32, h: u32) -> Asset {
    t6_files::jpeg(w, h)
}

fn pic(kind: &str, w: u32, h: u32) -> Asset {
    if kind == "jpeg" {
        jpeg(w, h)
    } else {
        png(w, h)
    }
}

/// A picture of a pixel format by name (`png` or `jpg` file, by the extension).
fn format_pic(name: &str, ext: &str, w: u32, h: u32, args: &[&str]) -> Asset {
    t6_files::photo(&format!("{name}-{w}x{h}.{ext}"), &t6_files::content(w, h), args)
}

/// A clip of footage: `testsrc2` of the size and the rate, with `vf` first and the pixel format after it, lossless.
fn footage(name: &str, w: u32, h: u32, fps: u32, secs: u32, vf: &str, pix: &str) -> Asset {
    static MADE: OnceLock<Mutex<BTreeMap<String, Asset>>> = OnceLock::new();
    let made = MADE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut made = made.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(a) = made.get(name) {
        return a.clone();
    }
    let path = media_dir().join(format!("{name}.mp4"));
    let src = format!("testsrc2=size={w}x{h}:rate={fps}:duration={secs}");
    let filter = if vf.is_empty() {
        format!("format={pix}")
    } else {
        format!("{vf},format={pix}")
    };
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &src,
        "-vf",
        &filter,
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
        path.to_str().unwrap(),
    ]);
    let asset = probed(&path);
    made.insert(name.to_string(), asset.clone());
    asset
}

const UNTAGGED: &str = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
const BT709: &str = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
const BT2020: &str = "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10";
const BT601: &str = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";

struct Footage {
    untagged: Asset,
    bt709: Asset,
    bt2020: Asset,
    bt601: Asset,
    full_untagged: Asset,
    full_709: Asset,
    hd_untagged: Asset,
    hd_709: Asset,
}

fn clips() -> &'static Footage {
    static F: OnceLock<Footage> = OnceLock::new();
    F.get_or_init(|| Footage {
        untagged: footage("untagged", 320, 180, 10, 4, UNTAGGED, "yuv420p"),
        bt709: footage("bt709", 320, 180, 10, 4, BT709, "yuv420p"),
        bt2020: footage("bt2020", 320, 180, 10, 4, BT2020, "yuv420p"),
        bt601: footage("bt601", 320, 180, 10, 4, BT601, "yuv420p"),
        full_untagged: footage(
            "full-untagged",
            320,
            180,
            10,
            4,
            &format!("{UNTAGGED},setparams=range=pc"),
            "yuvj420p",
        ),
        full_709: footage(
            "full-709",
            320,
            180,
            10,
            4,
            &format!("{BT709},setparams=range=pc"),
            "yuvj420p",
        ),
        hd_untagged: footage("hd-untagged", 1280, 720, 10, 4, UNTAGGED, "yuv420p"),
        hd_709: footage("hd-709", 1280, 720, 10, 4, BT709, "yuv420p"),
    })
}

// ---- one Still frame -------------------------------------------------------------------------------------------

/// The compositor's 4:2:0 planes as the 4:4:4 planes FFmpeg's own conversion makes of them.
fn to_444(frame: &YuvFrame) -> Vec<u8> {
    let (w, h) = (frame.width, frame.height);
    let data: Vec<u8> = [&frame.y, &frame.u, &frame.v].into_iter().flatten().copied().collect();
    piped(
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
    )
}

fn plane_diff(w: usize, want: &[u8], got: &[u8]) -> Option<(usize, i32, (usize, usize))> {
    let differing = want.iter().zip(got).filter(|(a, b)| a != b).count();
    if differing == 0 && want.len() == got.len() {
        return None;
    }
    let max = want
        .iter()
        .zip(got)
        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
        .max()
        .unwrap_or(0);
    let first = want.iter().zip(got).position(|(a, b)| a != b).unwrap_or(0);
    Some((
        differing.max(want.len().abs_diff(got.len())),
        max,
        (first % w.max(1), first / w.max(1)),
    ))
}

fn montage_png(case: &str, t: &str, (w, h): (usize, usize), want_444: &[u8], got_444: &[u8]) -> String {
    let rgb = |planes: &[u8]| {
        piped(
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv444p",
                "-s",
                &format!("{w}x{h}"),
                "-i",
                "pipe:0",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "pipe:1",
            ],
            planes.to_vec(),
        )
    };
    let (a, b) = (rgb(want_444), rgb(got_444));
    let mut strip = Vec::with_capacity(w * h * 9);
    for row in 0..h {
        strip.extend_from_slice(&a[row * w * 3..(row + 1) * w * 3]);
        strip.extend_from_slice(&b[row * w * 3..(row + 1) * w * 3]);
        for i in 0..w * 3 {
            let d = (i32::from(a[row * w * 3 + i]) - i32::from(b[row * w * 3 + i])).unsigned_abs();
            strip.push((d * 8).min(255) as u8);
        }
    }
    let dir = std::env::var_os("KERF_A6_MONTAGE").map_or_else(|| target_dir().join("parity"), PathBuf::from);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("a6-{}-{}.png", slug(case), slug(t)));
    let _ = piped(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &format!("{}x{h}", w * 3),
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            path.to_str().unwrap(),
        ],
        strip,
    );
    path.display().to_string()
}

#[derive(Clone)]
struct StillCase {
    family: String,
    name: String,
    tl: Timeline,
    assets: Vec<Asset>,
    told: Vec<Asset>,
    times: Vec<f64>,
    width: u32,
    opts: ExportOptions,
    anyway: bool,
}

fn stills(family: &str, name: &str, tl: Timeline, assets: &[Asset], times: &[f64]) -> StillCase {
    StillCase {
        family: family.to_string(),
        name: name.to_string(),
        tl,
        assets: assets.to_vec(),
        told: assets.to_vec(),
        times: times.to_vec(),
        width: u32::MAX,
        opts: ExportOptions::default(),
        anyway: false,
    }
}

impl StillCase {
    fn at_width(mut self, width: u32) -> Self {
        self.width = width;
        self
    }
}

fn shared_frames(layers: &[kerf_core::PlanLayer]) -> Result<Vec<Option<Arc<YuvFrame>>>, kerf_gpu::GpuError> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        match frame_source().frames(layers, kerf_gpu::Hint::Scrub) {
            Err(kerf_gpu::GpuError::Busy(_)) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

fn still_frame(c: &StillCase, t: f64) -> (Out, Vec<&'static str>) {
    let mut ex = Vec::new();
    let out = still_frame_inner(c, t, &mut ex);
    (out, ex)
}

fn still_frame_inner(c: &StillCase, t: f64, ex: &mut Vec<&'static str>) -> Out {
    let plan = match RenderPlan::at(&c.tl, &c.told, &c.opts, t, policy()) {
        Ok(p) => p,
        Err(e) => return Out::Failed(format!("plan: {e}")),
    };
    let size = plan.size(c.width);
    let plan = plan.for_render(size);
    let comp = if c.anyway { compositor_all() } else { compositor() };
    let why = reasons_of(&plan, comp, size);
    if !why.is_empty() {
        return refused_with(&why);
    }
    *ex = exempted(&plan, size);
    let (w, h, want) = match kerf_core::timeline_frame_yuv_cancellable(&c.tl, &c.assets, &c.opts, t, c.width, &|| false) {
        Ok(r) => r,
        Err(e) => return Out::Failed(format!("FFmpeg's own still failed where the plan draws: {e}")),
    };
    if (w, h) != size {
        return Out::Failed(format!("FFmpeg's still is {w}x{h}, the plan's size is {size:?}"));
    }
    let frames = match shared_frames(&plan.layers) {
        Ok(f) => f,
        Err(kerf_gpu::GpuError::Unsupported(e)) => return Out::Refused("render".into(), format!("render: {e}")),
        Err(e) => return Out::Failed(format!("decode: {e}")),
    };
    let got = match comp.composite_yuv_shared(&plan, &frames, size) {
        Ok(f) => f,
        Err(kerf_gpu::GpuError::Unsupported(e)) => return Out::Refused("render".into(), format!("render: {e}")),
        Err(e) => return Out::Failed(format!("composite: {e}")),
    };
    let got444 = to_444(&got);
    let (n, wu) = (w as usize * h as usize, w as usize);
    if got444.len() != want.len() || want.len() != 3 * n {
        return Out::Failed(format!("{} bytes of planes for {}", got444.len(), want.len()));
    }
    let mut lines = Vec::new();
    for (p, name) in ["Y", "U", "V"].into_iter().enumerate() {
        if let Some((count, max, (x, y))) = plane_diff(wu, &want[p * n..(p + 1) * n], &got444[p * n..(p + 1) * n]) {
            lines.push(format!("{name}: {count} samples differ, max {max}, first ({x}, {y})"));
        }
    }
    if lines.is_empty() {
        return Out::Exact;
    }
    let m = montage_png(&c.name, &format!("{t}"), (wu, h as usize), &want, &got444);
    Out::Off(format!("{} (montage {m})", lines.join("; ")))
}

struct Judged {
    fails: Vec<String>,
}

fn judge_stills(cases: &[StillCase]) -> Judged {
    let mut fails = Vec::new();
    for c in cases {
        for &t in &c.times {
            let (out, ex) = still_frame(c, t);
            count(&c.family, &out, &ex);
            let tag = match &out {
                Out::Exact => "exact".to_string(),
                Out::Off(w) => format!("OFF {w}"),
                Out::Refused(k, w) => format!("refused [{k}] {}", w.chars().take(110).collect::<String>()),
                Out::Failed(w) => format!("FAILED {w}"),
            };
            eprintln!("A6 still  {:<28} {:<60} t={t:<6} {tag}", c.family, c.name);
            match out {
                Out::Off(w) | Out::Failed(w) => fails.push(format!("{} / {} @ {t}: {w}", c.family, c.name)),
                _ => {}
            }
        }
    }
    Judged { fails }
}

// ---- one export frame -------------------------------------------------------------------------------------------

#[derive(Clone)]
struct ExportCase {
    family: String,
    name: String,
    tl: Timeline,
    assets: Vec<Asset>,
    told: Vec<Asset>,
    opts: ExportOptions,
    frames: Range<u64>,
    anyway: bool,
}

fn exports(family: &str, name: &str, tl: Timeline, assets: &[Asset], fps: f64, secs: f64) -> ExportCase {
    ExportCase {
        family: family.to_string(),
        name: name.to_string(),
        tl,
        assets: assets.to_vec(),
        told: assets.to_vec(),
        opts: at_fps(fps),
        frames: 0..(secs * fps).round() as u64,
        anyway: false,
    }
}

impl ExportCase {
    fn told(mut self, told: Vec<Asset>) -> Self {
        self.told = told;
        self
    }
}

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

fn export(slug_: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Result<Played, String> {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-t6-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    let file = scratch.join(format!("{slug_}.mkv"));
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts.clone()
    };
    kerf_core::render_with(tl, assets, &file, &lossless).map_err(|e| format!("FFmpeg's export failed: {e}"))?;
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("read back: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let info = Project::probe_asset(&file).map_err(|e| format!("probe the export: {e}"))?;
    let video = info
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .ok_or("no video stream")?;
    let _ = std::fs::remove_file(&file);
    Ok(Played {
        w: video.width.unwrap() as usize,
        h: video.height.unwrap() as usize,
        yuv: out.stdout,
    })
}

/// Every frame of `c`: `(frame, outcome)`.
fn export_frames(c: &ExportCase) -> Vec<(u64, Out, Vec<&'static str>)> {
    let slug_ = format!("{}-{}", slug(&c.family), slug(&c.name));
    let played = match export(&slug_, &c.tl, &c.assets, &c.opts) {
        Ok(p) => p,
        Err(e) => return c.frames.clone().map(|k| (k, Out::Failed(e.clone()), vec![])).collect(),
    };
    if (played.count() as u64) < c.frames.end {
        let why = format!("the export has {} frames, the case looks at {}", played.count(), c.frames.end);
        return c.frames.clone().map(|k| (k, Out::Failed(why.clone()), vec![])).collect();
    }
    let planner = match Planner::new(
        &c.tl,
        &c.told,
        &c.opts,
        PlanRequest::motion(policy()).with_media(&OriginalMedia),
    ) {
        Ok(p) => p,
        Err(e) => {
            return c
                .frames
                .clone()
                .map(|k| (k, Out::Failed(format!("planner: {e}")), vec![]))
                .collect()
        }
    };
    let size = (played.w as u32, played.h as u32);
    let comp = if c.anyway { compositor_all() } else { compositor() };
    let mut motion = MotionFrames::new(frame_source());
    let mut out = Vec::new();
    for k in c.frames.clone() {
        let plan = match planner.at_frame(k) {
            Ok(p) => p,
            Err(e) => {
                out.push((k, Out::Failed(format!("plan: {e}")), vec![]));
                continue;
            }
        };
        let why = reasons_of(&plan, comp, size);
        if !why.is_empty() {
            out.push((k, refused_with(&why), vec![]));
            continue;
        }
        let ex = exempted(&plan, size);
        let picture = match motion.frames(&plan) {
            Ok(p) => p,
            Err(kerf_gpu::GpuError::Unsupported(e)) => {
                out.push((k, Out::Refused("render".into(), format!("frames: {e}")), vec![]));
                continue;
            }
            Err(e) => {
                out.push((k, Out::Failed(format!("frames: {e}")), vec![]));
                continue;
            }
        };
        let drawn = match comp.composite_yuv_counted(&plan, &picture, motion.counts(), size) {
            Ok(d) => d,
            Err(kerf_gpu::GpuError::Unsupported(e)) => {
                out.push((k, Out::Refused("render".into(), format!("composite: {e}")), vec![]));
                continue;
            }
            Err(e) => {
                out.push((k, Out::Failed(format!("composite: {e}")), vec![]));
                continue;
            }
        };
        let want = played.planes(k as usize);
        let got = [&drawn.y[..], &drawn.u[..], &drawn.v[..]];
        let mut lines = Vec::new();
        for (p, name) in ["Y", "U", "V"].into_iter().enumerate() {
            let w = if p == 0 { played.w } else { played.w / 2 };
            if let Some((count, max, (x, y))) = plane_diff(w, want[p], got[p]) {
                lines.push(format!("{name}: {count} samples differ, max {max}, first ({x}, {y})"));
            }
        }
        out.push((
            k,
            if lines.is_empty() {
                Out::Exact
            } else {
                Out::Off(format!("{} ({}x{})", lines.join("; "), played.w, played.h))
            },
            ex,
        ));
    }
    out
}

fn judge_exports(cases: &[ExportCase]) -> Judged {
    let mut fails = Vec::new();
    for c in cases {
        let rows = export_frames(c);
        let (mut exact, mut refused) = (0, 0);
        let mut first_off: Option<String> = None;
        let mut offs = Vec::new();
        for (k, out, ex) in &rows {
            count(&c.family, out, ex);
            match out {
                Out::Exact => exact += 1,
                Out::Refused(..) => refused += 1,
                Out::Off(w) => {
                    offs.push(*k);
                    first_off.get_or_insert(format!("frame {k}: {w}"));
                }
                Out::Failed(w) => {
                    offs.push(*k);
                    first_off.get_or_insert(format!("frame {k}: FAILED {w}"));
                }
            }
        }
        let first_refusal = rows.iter().find_map(|(_, o, _)| match o {
            Out::Refused(k, w) => Some(format!("[{k}] {}", w.chars().take(110).collect::<String>())),
            _ => None,
        });
        eprintln!(
            "A6 export {:<28} {:<60} {} frames: exact {exact}, refused {refused}, off/failed {}{}{}",
            c.family,
            c.name,
            rows.len(),
            offs.len(),
            first_refusal.map(|r| format!(" | {r}")).unwrap_or_default(),
            first_off.as_ref().map(|r| format!(" | {r}")).unwrap_or_default(),
        );
        if let Some(w) = first_off {
            fails.push(format!(
                "{} / {}: {} frame(s) {:?}...; first {w}",
                c.family,
                c.name,
                offs.len(),
                &offs[..offs.len().min(8)]
            ));
        }
    }
    Judged { fails }
}

fn finish(what: &str, prefix: &str, judged: Vec<Judged>) {
    let fails: Vec<String> = judged.into_iter().flat_map(|j| j.fails).collect();
    let (drawn, exact, off, refused, failed) = report(prefix);
    eprintln!(
        "A6 {what}: drawn {drawn}, exact {exact}, inexact {off}, refused {refused}, failed {failed}, failing cases {}",
        fails.len()
    );
    for f in &fails {
        eprintln!("A6 FAIL {f}");
    }
    assert!(
        fails.is_empty(),
        "{what}: {} failing frame set(s); first: {}",
        fails.len(),
        fails[0]
    );
}

/// A seeded generator for the sweeps (no external crate).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn unit(&mut self) -> f64 {
        (self.next() as f64) / ((1u64 << 31) as f64)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn int(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % u64::from(hi - lo + 1)) as u32
    }
}

// ---- family: pixel formats ---------------------------------------------------------------------------------------

/// Pictures of every pixel format a still arrives in, 1200 x 800 (3:2), and whether the plan is expected to draw its moving still (`true`) on
/// every FFmpeg (a JPEG only where the composite's range is fixed).
fn format_files() -> Vec<(String, Asset, Option<&'static str>)> {
    let jpg = |name: &str, pix: &str| {
        (
            name.to_string(),
            format_pic(name, "jpg", 1200, 800, &["-q:v", "3", "-pix_fmt", pix]),
        )
    };
    let png_ = |name: &str, pix: &str| (name.to_string(), format_pic(name, "png", 1200, 800, &["-pix_fmt", pix]));
    let mut v: Vec<(String, Asset, Option<&'static str>)> = Vec::new();
    let (n, a) = png_("png-rgb24", "rgb24");
    v.push((n, a, Some("png")));
    for (n, a) in [
        png_("png-rgba", "rgba"),
        png_("png-gray", "gray"),
        png_("png-gray16", "gray16be"),
        png_("png-rgb48", "rgb48be"),
        png_("png-rgba64", "rgba64be"),
        png_("png-ya8", "ya8"),
        png_("png-ya16", "ya16be"),
        png_("png-pal8", "pal8"),
        png_("png-mono", "monob"),
    ] {
        v.push((n, a, None));
    }
    let (n, a) = jpg("jpeg-yuvj420p", "yuvj420p");
    v.push((n, a, Some("jpeg")));
    for (n, a) in [
        jpg("jpeg-yuvj422p", "yuvj422p"),
        jpg("jpeg-yuvj444p", "yuvj444p"),
        jpg("jpeg-yuvj440p", "yuvj440p"),
    ] {
        v.push((n, a, None));
    }
    v.push((
        "jpeg-yuv420p-limited".to_string(),
        format_pic(
            "jpeg-limited",
            "jpg",
            1200,
            800,
            &["-pix_fmt", "yuv420p", "-strict", "-1", "-q:v", "3"],
        ),
        None,
    ));
    v.push((
        "bmp-bgr24".to_string(),
        format_pic("bmp", "bmp", 1200, 800, &["-pix_fmt", "bgr24"]),
        None,
    ));
    v.push((
        "tiff-rgb24".to_string(),
        format_pic("tiff", "tiff", 1200, 800, &["-pix_fmt", "rgb24"]),
        None,
    ));
    v.push((
        "webp-yuv420p".to_string(),
        format_pic(
            "webp",
            "webp",
            1200,
            800,
            &["-pix_fmt", "yuv420p", "-c:v", "libwebp", "-lossless", "0", "-q:v", "90"],
        ),
        None,
    ));
    v
}

fn fmt_of(a: &Asset) -> String {
    a.streams.iter().find_map(|s| s.pix_fmt.clone()).unwrap_or_else(|| "?".into())
}

/// Whether a refusal in a case that is expected to draw is a failure: a PNG always, a JPEG where the composite's range is fixed.
fn must_draw(kind: Option<&str>) -> bool {
    match kind {
        Some("png") => true,
        Some("jpeg") => !negotiated(),
        _ => false,
    }
}

fn demand(judged: &mut Judged, family_prefix: &str, expect: &[(String, bool)]) {
    let all = tallies().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for (family, must) in expect {
        if !must {
            continue;
        }
        let t = all.get(family).cloned().unwrap_or_default();
        if t.exact == 0 || t.refused > 0 {
            judged.fails.push(format!(
                "{family_prefix}: {family} must be drawn: exact {}, refused {} {:?}",
                t.exact, t.refused, t.why
            ));
        }
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_pixel_formats() {
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    let mut expect = Vec::new();
    for (name, photo, kind) in format_files() {
        let fmt = fmt_of(&photo);
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let family = format!("formats/{name} [{fmt}] {route}");
            expect.push((family.clone(), must_draw(kind)));
            let tl = timeline(vec![vec![moving(&photo, 0.0, 2.0, mv)]], cover(320, 180));
            still_cases.push(stills(
                &family,
                "320x180 cover",
                tl.clone(),
                std::slice::from_ref(&photo),
                &[0.0, 0.5, 1.0, 1.5, 1.99],
            ));
            export_cases.push(exports(&family, "320x180 cover", tl, std::slice::from_ref(&photo), 10.0, 2.0));
        }
        // The control: a held zoom (nothing moves; the old static path).
        let mut held = still(&photo, 0.0, 2.0);
        held.transform.scale = 1.3;
        let family = format!("formats/{name} [{fmt}] held zoom");
        still_cases.push(stills(
            &family,
            "320x180 cover",
            timeline(vec![vec![held.clone()]], cover(320, 180)),
            std::slice::from_ref(&photo),
            &[0.0, 1.0],
        ));
        export_cases.push(exports(
            &family,
            "320x180 cover",
            timeline(vec![vec![held]], cover(320, 180)),
            std::slice::from_ref(&photo),
            10.0,
            1.0,
        ));
    }
    let mut a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    demand(&mut a, "pixel formats", &expect);
    finish("pixel formats", "formats/", vec![a, b]);
}

// ---- family: sizes ------------------------------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_picture_sizes() {
    let sizes: &[(u32, u32)] = &[
        (1, 1),
        (2, 2),
        (3, 7),
        (16, 9),
        (17, 13),
        (31, 5),
        (63, 64),
        (99, 100),
        (321, 181),
        (640, 360),
        (641, 359),
        (1001, 999),
        (1280, 720),
        (1920, 1080),
        (3001, 2001),
        (4000, 3000),
        (6000, 4000),
        (8192, 64),
        (64, 8192),
        (8193, 100),
    ];
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    let mut expect = Vec::new();
    for &(w, h) in sizes {
        for kind in ["png", "jpeg"] {
            let photo = pic(kind, w, h);
            for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
                let family = format!("sizes/{kind} {route}");
                let tl = timeline(vec![vec![moving(&photo, 0.0, 1.0, mv)]], cover(320, 180));
                still_cases.push(stills(
                    &family,
                    &format!("{w}x{h}"),
                    tl.clone(),
                    std::slice::from_ref(&photo),
                    &[0.0, 0.5, 0.99],
                ));
                export_cases.push(exports(
                    &family,
                    &format!("{w}x{h}"),
                    tl,
                    std::slice::from_ref(&photo),
                    10.0,
                    1.0,
                ));
            }
        }
    }
    for kind in ["png", "jpeg"] {
        for route in ["in", "off"] {
            expect.push((format!("sizes/{kind} {route}"), false));
        }
    }
    // One big picture into tiny canvases: a shrink of more than 63:1 (the plan's limit for the compositor's own scaler).
    for kind in ["png", "jpeg"] {
        let big = pic(kind, 6000, 4000);
        for (cw, ch) in [(64, 36), (96, 54), (48, 32)] {
            for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
                let family = format!("sizes/{kind} shrink past 63:1 {route}");
                let tl = timeline(vec![vec![moving(&big, 0.0, 1.0, mv)]], cover(cw, ch));
                still_cases.push(stills(
                    &family,
                    &format!("6000x4000 into {cw}x{ch}"),
                    tl.clone(),
                    std::slice::from_ref(&big),
                    &[0.0, 0.5, 0.99],
                ));
                export_cases.push(exports(
                    &family,
                    &format!("6000x4000 into {cw}x{ch}"),
                    tl,
                    std::slice::from_ref(&big),
                    10.0,
                    1.0,
                ));
            }
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("picture sizes", "sizes/", vec![a, b]);
}

// ---- family: canvases and fits ---------------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_canvases_and_fits() {
    let photos = [
        ("wide 3:2", 1200, 800),
        ("tall 2:3", 800, 1200),
        ("16:9", 1280, 720),
        ("square", 600, 600),
        ("thin 16:3", 1600, 300),
        ("odd", 1001, 667),
    ];
    let canvases = [(320, 180), (180, 320), (100, 100), (64, 36), (642, 358), (96, 200)];
    let fits = [("cover", Fit::Cover), ("contain", Fit::Contain), ("blur", Fit::Blur)];
    let moves = [("in", IN_A), ("off", OFF_A), ("out", OUT_A)];
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    for kind in ["png", "jpeg"] {
        for (pname, w, h) in photos {
            let photo = pic(kind, w, h);
            for (cw, ch) in canvases {
                for (fname, fit) in fits {
                    for (route, mv) in moves {
                        let family = format!("fits/{kind} {fname} {route}");
                        let tl = timeline(vec![vec![moving(&photo, 0.0, 1.0, mv)]], delivery(cw, ch, fit, 10.0));
                        let name = format!("{pname} {w}x{h} into {cw}x{ch}");
                        still_cases.push(stills(&family, &name, tl.clone(), std::slice::from_ref(&photo), &[0.0, 0.6]));
                        // (Export frames for the commonest canvas only: one ffmpeg export a case.)
                        if (cw, ch) == (320, 180) || (cw, ch) == (180, 320) {
                            export_cases.push(exports(&family, &name, tl, std::slice::from_ref(&photo), 10.0, 1.0));
                        }
                    }
                }
            }
        }
    }
    // A static crop under every fit, with a move, and a held zoom.
    for kind in ["png", "jpeg"] {
        let photo = pic(kind, 1200, 800);
        for (fname, fit) in fits {
            for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
                let mut c = moving(&photo, 0.0, 1.0, mv);
                c.transform.crop_left = 0.1;
                c.transform.crop_top = 0.05;
                c.transform.crop_right = 0.07;
                let family = format!("fits/{kind} static crop {fname} {route}");
                let tl = timeline(vec![vec![c]], delivery(320, 180, fit, 10.0));
                still_cases.push(stills(
                    &family,
                    "1200x800 cropped",
                    tl.clone(),
                    std::slice::from_ref(&photo),
                    &[0.0, 0.6, 0.99],
                ));
                export_cases.push(exports(
                    &family,
                    "1200x800 cropped",
                    tl,
                    std::slice::from_ref(&photo),
                    10.0,
                    1.0,
                ));
            }
        }
    }
    // Smaller than the preview's width: the still at 160 and 96 wide.
    for kind in ["png", "jpeg"] {
        let photo = pic(kind, 1200, 800);
        for width in [160u32, 96, 50] {
            for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
                let tl = timeline(vec![vec![moving(&photo, 0.0, 1.0, mv)]], cover(320, 180));
                still_cases.push(
                    stills(
                        &format!("fits/{kind} at a smaller render {route}"),
                        &format!("width {width}"),
                        tl,
                        std::slice::from_ref(&photo),
                        &[0.0, 0.5],
                    )
                    .at_width(width),
                );
            }
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("canvases and fits", "fits/", vec![a, b]);
}

// ---- family: the values a branch switches at --------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_move_extremes_and_ties() {
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    let mut add = |family: &str, name: &str, photo: &Asset, clip: Clip, d: Option<Delivery>, secs: f64| {
        let tl = timeline(vec![vec![clip]], d);
        still_cases.push(stills(
            family,
            name,
            tl.clone(),
            std::slice::from_ref(photo),
            &[0.0, secs * 0.25, secs * 0.5, secs * 0.75, secs - 0.01],
        ));
        export_cases.push(exports(family, name, tl, std::slice::from_ref(photo), 10.0, secs));
    };
    for kind in ["png", "jpeg"] {
        let photo = pic(kind, 1200, 800);
        let wide = pic(kind, 1280, 720);
        // The boundary of what covers: the zoom that just covers a pan of `p` is `1 + 2|p|` (a Cover frame of the picture's own shape).
        for (s, p) in [
            (1.2, 0.1),
            (1.5, 0.25),
            (2.0, 0.5),
            (1.1, 0.05),
            (3.0, 0.9),
            (1.2, 0.0999999),
            (1.2, 0.1000001),
            (1.5, 0.2500001),
        ] {
            add(
                &format!("moves/{kind} covers boundary"),
                &format!("16:9 scale {s} pos {p}"),
                &wide,
                moving(&wide, 0.0, 1.0, ((s, (p, 0.0)), (s, (-p, 0.0)))),
                cover(320, 180),
                1.0,
            );
            add(
                &format!("moves/{kind} covers boundary"),
                &format!("3:2 scale {s} pos {p} down"),
                &photo,
                moving(&photo, 0.0, 1.0, ((s, (0.0, p)), (s + 0.001, (0.0, -p)))),
                cover(320, 180),
                1.0,
            );
        }
        // A tiny move and a huge one.
        for (name, mv) in [
            ("1.0 to 1.0001", ((1.0, (0.0, 0.0)), (1.0001, (0.0, 0.0)))),
            ("1.0 to 1.000001", ((1.0, (0.0, 0.0)), (1.000001, (0.0, 0.0)))),
            ("1.5 to 1.5 pan 1e-5", ((1.5, (0.0, 0.0)), (1.5, (0.00001, 0.0)))),
            ("1.0 to 30", ((1.0, (0.0, 0.0)), (30.0, (0.0, 0.0)))),
            ("30 to 1", ((30.0, (0.0, 0.0)), (1.0, (0.0, 0.0)))),
            ("8 to 12 pan far", ((8.0, (0.3, -0.3)), (12.0, (-0.4, 0.4)))),
            ("1.2 pan corner to corner", ((1.2, (0.1, 0.1)), (1.2, (-0.1, -0.1)))),
            ("0.5 to 2 (out then in)", ((0.5, (0.0, 0.0)), (2.0, (0.0, 0.0)))),
        ] {
            add(
                &format!("moves/{kind} extremes"),
                name,
                &photo,
                moving(&photo, 0.0, 1.0, mv),
                cover(320, 180),
                1.0,
            );
        }
        // Many keys, an easing on each segment, frame times that are not on the frame grid.
        let mut rng = Lcg(0x5eed_0000 + if kind == "png" { 1 } else { 2 });
        for n in 0..6 {
            let mut c = still(&photo, 0.0, 2.0);
            let count = 3 + (n % 4);
            let mut keys = Vec::new();
            for i in 0..count {
                let t = if i == 0 {
                    0.0
                } else if i == count - 1 {
                    2.0
                } else {
                    rng.range(0.1, 1.9)
                };
                let mut k = key(t, rng.range(1.15, 1.6), rng.range(-0.05, 0.05), rng.range(-0.05, 0.05));
                k.easing = match rng.int(0, 4) {
                    0 => kerf_core::Easing::Linear,
                    1 => kerf_core::Easing::Hold,
                    2 => kerf_core::Easing::EaseIn,
                    3 => kerf_core::Easing::EaseOut,
                    _ => kerf_core::Easing::EaseInOut,
                };
                keys.push(k);
            }
            keys.sort_by(|a, b| a.time.total_cmp(&b.time));
            keys.dedup_by(|a, b| (a.time - b.time).abs() < 0.05);
            c.keyframes = keys;
            add(
                &format!("moves/{kind} random keys"),
                &format!("seed {n}"),
                &photo,
                c,
                cover(320, 180),
                2.0,
            );
        }
        // The clip does not start at zero, is trimmed, is sped up, is reversed.
        for (name, edit) in [
            (
                "starts at 0.37",
                Box::new(|c: &mut Clip| c.timeline_start = 0.37) as Box<dyn Fn(&mut Clip)>,
            ),
            ("source in 0.3", Box::new(|c: &mut Clip| c.source_in = 0.3)),
            ("speed 2", Box::new(|c: &mut Clip| c.speed = 2.0)),
            ("speed 0.5", Box::new(|c: &mut Clip| c.speed = 0.5)),
        ] {
            let mut c = moving(&photo, 0.0, 1.0, IN_A);
            edit(&mut c);
            let secs = c.timeline_end();
            add(&format!("moves/{kind} retimed"), name, &photo, c, cover(320, 180), secs);
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("move extremes", "moves/", vec![a, b]);
}

// ---- family: layers, fades, titles over stills ------------------------------------------------------------------

fn title(text: &str, start: f64, end: f64) -> kerf_core::TextOverlay {
    kerf_core::TextOverlay::new(text, start, end)
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_layers_fades_titles_over_stills() {
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    type Edit = Box<dyn Fn(&mut Clip)>;
    let edits: Vec<(&str, Edit)> = vec![
        ("plain", Box::new(|_c| {})),
        ("fade in 0.5", Box::new(|c| c.fade_in = 0.5)),
        ("fade out 0.5", Box::new(|c| c.fade_out = 0.5)),
        (
            "fade in and out",
            Box::new(|c| {
                c.fade_in = 0.4;
                c.fade_out = 0.4;
            }),
        ),
        (
            "static rotation 5",
            Box::new(|c| c.keyframes.iter_mut().for_each(|k| k.rotation = 5.0)),
        ),
        (
            "keyed rotation",
            Box::new(|c| {
                c.keyframes.first_mut().unwrap().rotation = 0.0;
                c.keyframes.last_mut().unwrap().rotation = 10.0;
            }),
        ),
        (
            "static opacity 0.6",
            Box::new(|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.6)),
        ),
        (
            "keyed opacity",
            Box::new(|c| {
                c.keyframes.first_mut().unwrap().opacity = 0.2;
                c.keyframes.last_mut().unwrap().opacity = 1.0;
            }),
        ),
        (
            "mask ellipse",
            Box::new(|c| {
                c.mask = Some(kerf_core::Mask {
                    shape: kerf_core::MaskShape::Ellipse,
                    x: 0.5,
                    y: 0.5,
                    width: 0.6,
                    height: 0.6,
                    feather: 0.1,
                    inverted: false,
                })
            }),
        ),
        (
            "colour contrast",
            Box::new(|c| {
                c.color = Color {
                    contrast: 1.3,
                    ..Color::default()
                }
            }),
        ),
        (
            "colour saturation 0",
            Box::new(|c| {
                c.color = Color {
                    saturation: 0.0,
                    ..Color::default()
                }
            }),
        ),
        (
            "effect grayscale",
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Grayscale]),
        ),
        (
            "effect invert",
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Invert]),
        ),
        (
            "effect vignette",
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Vignette]),
        ),
        (
            "effect blur",
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 2.0 }]),
        ),
        (
            "effect sharpen",
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Sharpen { amount: 1.0 }]),
        ),
        (
            "effect chroma key",
            Box::new(|c| {
                c.effects = vec![kerf_core::VideoEffect::ChromaKey {
                    color: "green".into(),
                    similarity: 0.3,
                    blend: 0.1,
                }]
            }),
        ),
        ("blend multiply", Box::new(|c| c.blend_mode = kerf_core::BlendMode::Multiply)),
        (
            "keyed crop zoom",
            Box::new(|c| {
                c.set_property_keys(
                    kerf_core::Property::CropZoom,
                    vec![kerf_core::PropertyKey::new(0.0, 1.0), kerf_core::PropertyKey::new(1.0, 1.3)],
                );
            }),
        ),
        (
            "static crop",
            Box::new(|c| {
                c.transform.crop_left = 0.08;
                c.transform.crop_bottom = 0.1;
            }),
        ),
    ];
    for kind in ["png", "jpeg"] {
        let photo = pic(kind, 1200, 800);
        for (name, edit) in &edits {
            for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
                let mut c = moving(&photo, 0.0, 1.0, mv);
                edit(&mut c);
                let family = format!("layers/{kind} {name} {route}");
                let tl = timeline(vec![vec![c]], cover(320, 180));
                still_cases.push(stills(
                    &family,
                    "320x180",
                    tl.clone(),
                    std::slice::from_ref(&photo),
                    &[0.0, 0.3, 0.7, 0.99],
                ));
                export_cases.push(exports(&family, "320x180", tl, std::slice::from_ref(&photo), 10.0, 1.0));
            }
        }
        // Titles over a moving still: one, two with a box, one keyed in and out.
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let mut tl = timeline(vec![vec![moving(&photo, 0.0, 1.0, mv)]], cover(320, 180));
            let mut boxed = title("Second line", 0.2, 0.9);
            boxed.bg = Some("black@0.5".into());
            boxed.pos_y = 0.2;
            let mut keyed = title("Faded", 0.0, 1.0);
            keyed.keyframes = vec![
                kerf_core::TextKeyframe {
                    time: 0.0,
                    pos_x: 0.5,
                    pos_y: 0.5,
                    opacity: 0.0,
                },
                kerf_core::TextKeyframe {
                    time: 0.5,
                    pos_x: 0.5,
                    pos_y: 0.5,
                    opacity: 1.0,
                },
            ];
            tl.overlays = vec![title("Title", 0.0, 1.0), boxed, keyed];
            let family = format!("layers/{kind} titles {route}");
            still_cases.push(stills(
                &family,
                "320x180",
                tl.clone(),
                std::slice::from_ref(&photo),
                &[0.0, 0.3, 0.7, 0.99],
            ));
            export_cases.push(exports(&family, "320x180", tl, std::slice::from_ref(&photo), 10.0, 1.0));
        }
        // A moving still over another track's footage and under a second moving still at half opacity.
        let f = clips();
        let mut top = moving(&photo, 0.0, 1.0, OFF_B);
        top.keyframes.iter_mut().for_each(|k| k.opacity = 0.5);
        let tl = timeline(
            vec![
                vec![still(&f.untagged, 0.0, 1.0)],
                vec![moving(&photo, 0.0, 1.0, IN_B)],
                vec![top],
            ],
            cover(320, 180),
        );
        let assets = [photo.clone(), f.untagged.clone()];
        let family = format!("layers/{kind} stacked over footage");
        still_cases.push(stills(&family, "320x180", tl.clone(), &assets, &[0.0, 0.5, 0.99]));
        export_cases.push(exports(&family, "320x180", tl, &assets, 10.0, 1.0));
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("layers, fades and titles", "layers/", vec![a, b]);
}

// ---- family: transitions ---------------------------------------------------------------------------------------

const KINDS: [(&str, TransitionKind); 11] = [
    ("crossfade", TransitionKind::Crossfade),
    ("dip black", TransitionKind::DipToBlack),
    ("dip white", TransitionKind::DipToWhite),
    ("slide left", TransitionKind::SlideLeft),
    ("slide right", TransitionKind::SlideRight),
    ("slide up", TransitionKind::SlideUp),
    ("slide down", TransitionKind::SlideDown),
    ("push left", TransitionKind::PushLeft),
    ("push right", TransitionKind::PushRight),
    ("push up", TransitionKind::PushUp),
    ("push down", TransitionKind::PushDown),
];

fn dissolving(mut c: Clip, kind: TransitionKind, duration: f64) -> Clip {
    c.transition_in = Some(Transition { kind, duration });
    c
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_transitions_with_moving_stills() {
    let f = clips();
    let (p_in, p_off) = (png(1200, 800), png(1280, 720));
    let (j_in, j_off) = (jpeg(1200, 800), jpeg(800, 1200));
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    for (kname, kind) in KINDS {
        // (outgoing clip, incoming clip, assets) over 0..1.5 and 1.0..2.5 with a 0.5 s transition.
        let arrangements: Vec<(&str, Clip, Clip, Vec<Asset>)> = vec![
            (
                "png in -> png off",
                moving(&p_in, 0.0, 1.5, IN_A),
                moving(&p_off, 1.0, 1.5, OFF_A),
                vec![p_in.clone(), p_off.clone()],
            ),
            (
                "png off -> png in",
                moving(&p_off, 0.0, 1.5, OFF_B),
                moving(&p_in, 1.0, 1.5, IN_B),
                vec![p_off.clone(), p_in.clone()],
            ),
            (
                "jpeg in -> png off",
                moving(&j_in, 0.0, 1.5, IN_A),
                moving(&p_off, 1.0, 1.5, OFF_A),
                vec![j_in.clone(), p_off.clone()],
            ),
            (
                "png in -> jpeg off",
                moving(&p_in, 0.0, 1.5, IN_A),
                moving(&j_off, 1.0, 1.5, OFF_A),
                vec![p_in.clone(), j_off.clone()],
            ),
            (
                "jpeg in -> jpeg off",
                moving(&j_in, 0.0, 1.5, IN_A),
                moving(&j_off, 1.0, 1.5, OFF_B),
                vec![j_in.clone(), j_off.clone()],
            ),
            (
                "png static -> png in",
                still(&p_off, 0.0, 1.5),
                moving(&p_in, 1.0, 1.5, IN_A),
                vec![p_off.clone(), p_in.clone()],
            ),
            (
                "png in -> png static",
                moving(&p_in, 0.0, 1.5, IN_A),
                still(&p_off, 1.0, 1.5),
                vec![p_in.clone(), p_off.clone()],
            ),
            (
                "video -> png in",
                still(&f.untagged, 0.0, 1.5),
                moving(&p_in, 1.0, 1.5, IN_A),
                vec![f.untagged.clone(), p_in.clone()],
            ),
            (
                "png off -> video",
                moving(&p_off, 0.0, 1.5, OFF_A),
                still(&f.untagged, 1.0, 1.5),
                vec![p_off.clone(), f.untagged.clone()],
            ),
            (
                "video -> jpeg off",
                still(&f.untagged, 0.0, 1.5),
                moving(&j_off, 1.0, 1.5, OFF_A),
                vec![f.untagged.clone(), j_off.clone()],
            ),
            (
                "jpeg in -> video",
                moving(&j_in, 0.0, 1.5, IN_A),
                still(&f.untagged, 1.0, 1.5),
                vec![j_in.clone(), f.untagged.clone()],
            ),
        ];
        for (aname, out, inc, assets) in arrangements {
            let family = format!("transitions/{aname}");
            let tl = timeline(vec![vec![out, dissolving(inc, kind, 0.5)]], cover(320, 180));
            let name = kname.to_string();
            still_cases.push(stills(&family, &name, tl.clone(), &assets, &[0.5, 1.05, 1.25, 1.45, 2.0]));
            export_cases.push(exports(&family, &name, tl, &assets, 10.0, 2.5));
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("transitions", "transitions/", vec![a, b]);
}

// ---- family: cuts that name a matrix, full range -----------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_cuts_that_name_a_matrix_and_full_range() {
    let f = clips();
    let footage_list: Vec<(&str, &Asset, (u32, u32))> = vec![
        ("untagged", &f.untagged, (320, 180)),
        ("bt709", &f.bt709, (320, 180)),
        ("bt2020", &f.bt2020, (320, 180)),
        ("bt601", &f.bt601, (320, 180)),
        ("full range untagged", &f.full_untagged, (320, 180)),
        ("full range bt709", &f.full_709, (320, 180)),
        ("hd untagged", &f.hd_untagged, (1280, 720)),
        ("hd bt709", &f.hd_709, (1280, 720)),
    ];
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    for (kind, w, h) in [
        ("png", 1920, 1080),
        ("png", 640, 360),
        ("png", 1200, 800),
        ("jpeg", 1920, 1080),
        ("jpeg", 1200, 800),
    ] {
        let photo = pic(kind, w, h);
        for (vname, video, (vw, vh)) in &footage_list {
            let assets = [photo.clone(), (*video).clone()];
            let canvas = cover(*vw.min(&640), *vh.min(&360));
            let cw = (*vw).min(640);
            let ch = (*vh).min(360);
            let _ = canvas;
            let canvas = cover(cw, ch);
            let arrange: Vec<(&str, Vec<Vec<Clip>>, f64)> = vec![
                (
                    "video then moving still dissolving in",
                    vec![vec![
                        still(video, 0.0, 1.5),
                        dissolving(moving(&photo, 1.0, 1.5, IN_A), TransitionKind::Crossfade, 0.5),
                    ]],
                    2.5,
                ),
                (
                    "moving still then video dissolving in",
                    vec![vec![
                        moving(&photo, 0.0, 1.5, IN_A),
                        dissolving(still(video, 1.0, 1.5), TransitionKind::Crossfade, 0.5),
                    ]],
                    2.5,
                ),
                (
                    "moving still over video",
                    vec![vec![still(video, 0.0, 1.5)], vec![moving(&photo, 0.0, 1.5, IN_B)]],
                    1.5,
                ),
                (
                    "pan-off still after video",
                    vec![vec![still(video, 0.0, 1.0), moving(&photo, 1.0, 1.5, OFF_A)]],
                    2.5,
                ),
                (
                    "static still after video, dissolving (control)",
                    vec![vec![
                        still(video, 0.0, 1.5),
                        dissolving(still(&photo, 1.0, 1.5), TransitionKind::Crossfade, 0.5),
                    ]],
                    2.5,
                ),
                (
                    "video after moving still with a dip to black",
                    vec![vec![
                        moving(&photo, 0.0, 1.5, OFF_B),
                        dissolving(still(video, 1.0, 1.5), TransitionKind::DipToBlack, 0.5),
                    ]],
                    2.5,
                ),
            ];
            for (aname, tracks, secs) in arrange {
                let family = format!("colour/{kind} {w}x{h} {aname}");
                let tl = timeline(tracks, canvas);
                let name = format!("with {vname} footage");
                let times: Vec<f64> = vec![0.2, secs * 0.45, secs * 0.6, secs - 0.15];
                still_cases.push(stills(&family, &name, tl.clone(), &assets, &times));
                export_cases.push(exports(&family, &name, tl, &assets, 10.0, secs));
            }
        }
    }
    // The PNG tagged BT.709 full range by its cICP chunk, and the colour chunks of other writers, beside BT.709 footage.
    for (name, photo) in t6_files::png_variants()
        .into_iter()
        .filter(|(n, _)| n.starts_with("cICP") || n == "plain" || n.starts_with("gAMA") || n.starts_with("iCCP"))
    {
        for (vname, video) in [
            ("bt709", &f.bt709),
            ("untagged", &f.untagged),
            ("full range bt709", &f.full_709),
        ] {
            let assets = [photo.clone(), video.clone()];
            let tl = timeline(
                vec![vec![
                    still(video, 0.0, 1.5),
                    dissolving(moving(&photo, 1.0, 1.5, IN_A), TransitionKind::Crossfade, 0.5),
                ]],
                cover(320, 180),
            );
            let family = format!("colour/png variant {name} after footage");
            still_cases.push(stills(
                &family,
                &format!("after {vname}"),
                tl.clone(),
                &assets,
                &[0.3, 1.3, 2.3],
            ));
            export_cases.push(exports(&family, &format!("after {vname}"), tl, &assets, 10.0, 2.5));
        }
        // Alone, in a cut of its own: nothing names a matrix but the file.
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let tl = timeline(vec![vec![moving(&photo, 0.0, 1.0, mv)]], cover(320, 180));
            let family = format!("colour/png variant {name} alone {route}");
            still_cases.push(stills(
                &family,
                "alone",
                tl.clone(),
                std::slice::from_ref(&photo),
                &[0.0, 0.5, 0.99],
            ));
            export_cases.push(exports(&family, "alone", tl, std::slice::from_ref(&photo), 10.0, 1.0));
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("cuts that name a matrix and full range", "colour/", vec![a, b]);
}

// ---- the merge decision: the exemption of a still whose fit picture FFmpeg makes ------------------------------------

/// The lane B refusals a layer would meet if the exemption (`fits_by_ffmpeg`) were not there, judged here from the plan's own pieces
/// exactly as `RenderPlan::reasons` judges them: a resize of a picture that is not 8-bit 4:2:0 (`FormatResize`), a shrink steeper than
/// 63:1 (`Shrink`), a full-range picture resized in an export frame or inside a fade (`FullRangeResized`).
fn exempted(plan: &RenderPlan, size: (u32, u32)) -> Vec<&'static str> {
    let mut v = Vec::new();
    for layer in &plan.layers {
        if !plan.fits_by_ffmpeg(layer) {
            continue;
        }
        let Ok(geom) = plan.layer_geometry(layer, size) else {
            continue;
        };
        let resizing = geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h));
        let fmt = layer.stream.pix_fmt.clone().unwrap_or_default().to_ascii_lowercase();
        if resizing && !matches!(fmt.as_str(), "yuv420p" | "yuvj420p") {
            v.push("FormatResize");
        }
        let past = |from: u32, to: u32| u64::from(from) > u64::from(kerf_core::MAX_SHRINK) * u64::from(to);
        if geom
            .stages
            .iter()
            .any(|s| past(s.src.w, s.scaled.0) || past(s.src.h, s.scaled.1))
        {
            v.push("Shrink");
        }
        if layer.stream.maybe_full_range() && resizing && (plan.mode == kerf_core::PlanMode::Motion || !layer.fx.fades.is_empty())
        {
            v.push("FullRangeResized");
        }
    }
    v
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_the_exemption_from_lane_b_is_exact_on_adversarial_sizes_and_moves() {
    // Pictures whose fit the lane B refusals would have caught: RGB resized (any ratio), a full-range JPEG resized, and a shrink past 63:1.
    let mut rng = Lcg(0xfeed_f00d);
    let canvases: &[(u32, u32)] = &[
        (320, 180),
        (180, 320),
        (100, 100),
        (64, 36),
        (96, 54),
        (642, 358),
        (48, 80),
        (256, 256),
    ];
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    let mut plans: Vec<(String, Timeline, Asset, (u32, u32))> = Vec::new();
    for n in 0..44 {
        let kind = if n % 2 == 0 { "png" } else { "jpeg" };
        let (w, h) = match n % 11 {
            0 => (rng.int(2, 24), rng.int(2, 24)),
            1 => (rng.int(1500, 2600), rng.int(30, 90)),
            2 => (rng.int(30, 90), rng.int(1500, 2600)),
            3 => (rng.int(2000, 3200), rng.int(1500, 2400)),
            _ => (rng.int(40, 1900), rng.int(40, 1900)),
        };
        let photo = pic(kind, w, h);
        let (cw, ch) = canvases[(rng.int(0, canvases.len() as u32 - 1)) as usize];
        let fit = [Fit::Cover, Fit::Contain, Fit::Blur][(rng.int(0, 2)) as usize];
        // A move that leaves the frame bare at some moment (the placement's, FFmpeg makes the fit) or not (the route's), at random.
        let mv: (Mv, Mv) = (
            (rng.range(0.55, 1.35), (rng.range(-0.25, 0.25), rng.range(-0.25, 0.25))),
            (rng.range(0.55, 1.5), (rng.range(-0.25, 0.25), rng.range(-0.25, 0.25))),
        );
        let secs = 1.0;
        let mut c = moving(&photo, 0.0, secs, mv);
        if rng.int(0, 3) == 0 {
            c.transform.crop_left = rng.range(0.0, 0.2);
            c.transform.crop_top = rng.range(0.0, 0.2);
        }
        if rng.int(0, 4) == 0 {
            c.fade_in = 0.4;
        }
        let tl = timeline(vec![vec![c]], delivery(cw, ch, fit, 10.0));
        let family = format!("exemption/{kind}");
        let name = format!(
            "{n}: {w}x{h} {fit:?} into {cw}x{ch}, {:.2}@({:.2},{:.2}) to {:.2}@({:.2},{:.2})",
            mv.0 .0, mv.0 .1 .0, mv.0 .1 .1, mv.1 .0, mv.1 .1 .0, mv.1 .1 .1
        );
        still_cases.push(stills(
            &family,
            &name,
            tl.clone(),
            std::slice::from_ref(&photo),
            &[0.0, 0.5, 0.99],
        ));
        export_cases.push(exports(&family, &name, tl.clone(), std::slice::from_ref(&photo), 10.0, secs));
        plans.push((name, tl, photo, (cw, ch)));
    }
    // The count of frames the exemption admits (the lane B refusals' conditions, judged from the plan), over export frames and stills.
    let mut admitted: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut admitted_frames = 0;
    for (_, tl, photo, canvas) in &plans {
        let planner = Planner::new(
            tl,
            std::slice::from_ref(photo),
            &at_fps(10.0),
            PlanRequest::motion(policy()).with_media(&OriginalMedia),
        )
        .unwrap();
        for k in 0..10 {
            let plan = planner.at_frame(k).unwrap();
            let why = exempted(&plan, *canvas);
            if !why.is_empty() && reasons_of(&plan, compositor(), *canvas).is_empty() {
                admitted_frames += 1;
            }
            for w in why {
                *admitted.entry(w).or_default() += 1;
            }
        }
    }
    eprintln!("A6 EXEMPTION: export frames the exemption admits (drawn, and a lane B refusal would otherwise apply): {admitted_frames}; by refusal {admitted:?}");
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("the exemption sweep", "exemption/", vec![a, b]);
}

// ---- the probe: unprimed sessions, and the render that never runs it ---------------------------------------------

fn cold_cut(kind: &str, route: (Mv, Mv)) -> (Timeline, Asset) {
    let photo = pic(kind, 1200, 800);
    (timeline(vec![vec![moving(&photo, 0.0, 2.0, route)]], cover(320, 180)), photo)
}

fn refusal_text<T>(r: Result<T, kerf_gpu::GpuError>) -> Option<String> {
    match r {
        Ok(_) => None,
        Err(kerf_gpu::GpuError::Unsupported(w)) => Some(w),
        Err(other) => Some(format!("NOT A REFUSAL: {other}")),
    }
}

fn require_cold() {
    assert_eq!(
        kerf_gpu::still_probe::measured(),
        kerf_core::StillKinds::NONE,
        "the still probe ran before this test asked: run it alone, with --exact"
    );
}

/// A session nobody primed (no `prepare`, no `prepare_ahead`; `prepare_timeline` is not one) refuses a moving still on both routes, in a
/// still plan and in an export frame, through every entry of the compositor that draws, and none of them runs the probe.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter; run alone with --exact (a fresh process is what unprimed is)"]
fn checker_a_t6_an_unprimed_session_refuses_a_moving_still_and_the_render_never_runs_the_probe() {
    require_cold();
    let comp = Compositor::new(gpu()).expect("a compositor");
    assert_eq!(comp.caps().still_motion, kerf_core::StillKinds::NONE, "cold caps");
    let mut lines = Vec::new();
    for kind in ["png", "jpeg"] {
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let (tl, photo) = cold_cut(kind, mv);
            let assets = std::slice::from_ref(&photo);
            // The host's priming of a timeline is not the still probe's.
            kerf_gpu::prepare_timeline(&tl);
            assert_eq!(
                kerf_gpu::still_probe::measured(),
                kerf_core::StillKinds::NONE,
                "prepare_timeline primed the still probe"
            );
            // A still plan.
            let plan = RenderPlan::at(&tl, assets, &ExportOptions::default(), 1.0, policy()).unwrap();
            let size = plan.size(u32::MAX);
            let worked = plan.clone().for_render(size);
            let why = worked.reasons(&comp.caps(), size);
            assert!(
                why.iter().any(|u| u.kind() == "still_motion"),
                "{kind} {route}: a cold moving still was not refused as one: {why:?}"
            );
            // Every entry that draws: refused, and the probe not run.
            let frames = shared_frames(&worked.layers).expect("the decode");
            let a = refusal_text(comp.composite_yuv_shared(&worked, &frames, size));
            let b = refusal_text(comp.render_plan_with(&worked, size, &frame_source(), kerf_gpu::Hint::Exact));
            let d = refusal_text(comp.render_draft_texture_with(&worked, size, &frame_source(), kerf_gpu::Hint::Exact));
            let c = refusal_text(comp.render_plan_texture_with(&worked, size, &frame_source(), kerf_gpu::Hint::Exact));
            for (entry, r) in [
                ("composite_yuv_shared", &a),
                ("render_plan_with", &b),
                ("render_plan_texture_with", &c),
                ("render_draft_texture_with", &d),
            ] {
                let r = r
                    .as_deref()
                    .unwrap_or_else(|| panic!("{kind} {route}: {entry} DREW a cold moving still"));
                assert!(r.contains("still") || r.contains("moving"), "{kind} {route}: {entry}: {r}");
            }
            // An export frame.
            let planner = Planner::new(&tl, assets, &at_fps(10.0), PlanRequest::motion(policy())).unwrap();
            let plan = planner.at_frame(5).unwrap();
            let size = (plan.canvas.width, plan.canvas.height);
            let why = plan.reasons(&comp.caps(), size);
            assert!(
                why.iter().any(|u| u.kind() == "still_motion"),
                "{kind} {route}: export frame not refused: {why:?}"
            );
            let mut motion = MotionFrames::new(frame_source());
            let picture = motion.frames(&plan).expect("frames");
            let r = refusal_text(comp.composite_yuv_counted(&plan, &picture, motion.counts(), size));
            assert!(
                r.is_some_and(|r| r.contains("still")),
                "{kind} {route}: an export frame was drawn cold"
            );
            assert_eq!(
                kerf_gpu::still_probe::measured(),
                kerf_core::StillKinds::NONE,
                "{kind} {route}: a render ran the probe"
            );
            lines.push(format!(
                "{kind} {route}: still plan and export frame refused through 4 entries, probe not run"
            ));
        }
    }
    for l in &lines {
        eprintln!("A6 UNPRIMED {l}");
    }
    // The one-shot entry has no host to prime for it: it asks for what it needs, and so primes the probe. Then it draws.
    let (tl, photo) = cold_cut("png", IN_A);
    let plan = RenderPlan::at(&tl, std::slice::from_ref(&photo), &ExportOptions::default(), 1.0, policy()).unwrap();
    let size = plan.size(u32::MAX);
    let r = comp.render_plan(&plan, size);
    eprintln!(
        "A6 UNPRIMED render_plan (the one-shot entry): {} ; probe afterwards {:?}",
        match &r {
            Ok(_) => "drew".to_string(),
            Err(e) => format!("refused: {e}"),
        },
        kerf_gpu::still_probe::measured()
    );
}

/// `prepare_ahead` over a cut primes the probe (and so does `prepare` of a plan): the same cut is drawn afterwards.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter; run alone with --exact (a fresh process is what unprimed is)"]
fn checker_a_t6_prepare_ahead_primes_the_probe_and_prepare_timeline_does_not() {
    require_cold();
    let (tl, photo) = cold_cut("png", IN_A);
    let assets = std::slice::from_ref(&photo);
    kerf_gpu::prepare_timeline(&tl);
    assert_eq!(
        kerf_gpu::still_probe::measured(),
        kerf_core::StillKinds::NONE,
        "prepare_timeline primed the probe"
    );
    let cut = kerf_core::PlayCut::new(&tl, assets, 0.0, policy(), &OriginalMedia).expect("a cut");
    let comp = Compositor::new(gpu()).expect("a compositor");
    let before = comp.session_caps();
    assert_eq!(before.still_motion, kerf_core::StillKinds::NONE);
    kerf_gpu::prepare_ahead(&cut, 0.0, 1.0);
    let kinds = kerf_gpu::still_probe::measured();
    eprintln!("A6 PREPARE_AHEAD: kinds measured afterwards {kinds:?}");
    assert!(
        kinds.contains(kerf_core::StillKinds::RGB24),
        "prepare_ahead did not prime the PNG kind: {kinds:?}"
    );
    let plan = cut.at_frame(cut.frame_at(1.0)).expect("a plan");
    let size = (plan.canvas.width, plan.canvas.height);
    let why = plan.reasons(&comp.session_caps(), size);
    assert!(why.is_empty(), "primed, the frame is refused: {why:?}");
}

// ---- the compact suite the mutants and the refusals drawn anyway are judged by -----------------------------------------

fn told_as(a: &Asset, pix: &str) -> Asset {
    let mut told = a.clone();
    for s in told.streams.iter_mut() {
        s.pix_fmt = Some(pix.to_string());
    }
    told
}

/// One scenario: a name, a cut, the files, what the plan is told, and the width the stills render at.
struct Scenario {
    name: String,
    tl: Timeline,
    assets: Vec<Asset>,
    told: Vec<Asset>,
    width: u32,
    stills: Vec<f64>,
    fps_secs: Option<(f64, f64)>,
}

fn add(v: &mut Vec<Scenario>, name: &str, tl: Timeline, assets: Vec<Asset>, told: Option<Vec<Asset>>, width: u32) {
    v.push(Scenario {
        name: name.to_string(),
        tl,
        told: told.unwrap_or_else(|| assets.clone()),
        assets,
        width,
        stills: vec![0.25, 0.75],
        fps_secs: Some((10.0, 1.0)),
    });
}

fn scenarios() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();
    let f = clips();
    let (pn, pj) = (png(1200, 800), jpeg(1200, 800));
    let big = png(6000, 4000);
    let gray = format_pic("png-gray", "png", 1200, 800, &["-pix_fmt", "gray"]);
    let rgba = format_pic("png-rgba", "png", 1200, 800, &["-pix_fmt", "rgba"]);
    let j422 = format_pic("jpeg-yuvj422p", "jpg", 1200, 800, &["-q:v", "3", "-pix_fmt", "yuvj422p"]);
    let j444 = format_pic("jpeg-yuvj444p", "jpg", 1200, 800, &["-q:v", "3", "-pix_fmt", "yuvj444p"]);
    let j420l = format_pic(
        "jpeg-limited",
        "jpg",
        1200,
        800,
        &["-pix_fmt", "yuv420p", "-strict", "-1", "-q:v", "3"],
    );
    let one = |photo: &Asset, mv: (Mv, Mv), edit: &dyn Fn(&mut Clip)| {
        let mut c = moving(photo, 0.0, 1.0, mv);
        edit(&mut c);
        timeline(vec![vec![c]], cover(320, 180))
    };
    let nop = |_: &mut Clip| {};
    for (kind, p) in [("png", &pn), ("jpeg", &pj)] {
        add(
            &mut v,
            &format!("{kind} in"),
            one(p, IN_A, &nop),
            vec![p.clone()],
            None,
            u32::MAX,
        );
        add(
            &mut v,
            &format!("{kind} off"),
            one(p, OFF_A, &nop),
            vec![p.clone()],
            None,
            u32::MAX,
        );
        add(
            &mut v,
            &format!("{kind} in at width 160"),
            one(p, IN_A, &nop),
            vec![p.clone()],
            None,
            160,
        );
        add(
            &mut v,
            &format!("{kind} off at width 160"),
            one(p, OFF_A, &nop),
            vec![p.clone()],
            None,
            160,
        );
        add(
            &mut v,
            &format!("{kind} in with fade"),
            one(p, IN_A, &|c| c.fade_in = 0.5),
            vec![p.clone()],
            None,
            u32::MAX,
        );
        add(
            &mut v,
            &format!("{kind} off with fade"),
            one(p, OFF_A, &|c| c.fade_in = 0.5),
            vec![p.clone()],
            None,
            u32::MAX,
        );
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            add(
                &mut v,
                &format!("{kind} {route} with contrast"),
                one(p, mv, &|c| {
                    c.color = Color {
                        contrast: 1.4,
                        ..Color::default()
                    }
                }),
                vec![p.clone()],
                None,
                u32::MAX,
            );
            add(
                &mut v,
                &format!("{kind} {route} with grayscale"),
                one(p, mv, &|c| c.effects = vec![kerf_core::VideoEffect::Grayscale]),
                vec![p.clone()],
                None,
                u32::MAX,
            );
            add(
                &mut v,
                &format!("{kind} {route} with a keyed crop"),
                one(p, mv, &|c| {
                    c.set_property_keys(
                        kerf_core::Property::CropZoom,
                        vec![kerf_core::PropertyKey::new(0.0, 1.0), kerf_core::PropertyKey::new(1.0, 1.2)],
                    )
                }),
                vec![p.clone()],
                None,
                u32::MAX,
            );
            add(
                &mut v,
                &format!("{kind} {route} with a mask"),
                one(p, mv, &|c| {
                    c.mask = Some(kerf_core::Mask {
                        shape: kerf_core::MaskShape::Ellipse,
                        x: 0.5,
                        y: 0.5,
                        width: 0.6,
                        height: 0.6,
                        feather: 0.1,
                        inverted: false,
                    })
                }),
                vec![p.clone()],
                None,
                u32::MAX,
            );
            add(
                &mut v,
                &format!("{kind} {route} at opacity 0.6"),
                one(p, mv, &|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.6)),
                vec![p.clone()],
                None,
                u32::MAX,
            );
            add(
                &mut v,
                &format!("{kind} {route} with a rotation"),
                one(p, mv, &|c| c.keyframes.iter_mut().for_each(|k| k.rotation = 6.0)),
                vec![p.clone()],
                None,
                u32::MAX,
            );
            let mut tl = timeline(
                vec![vec![
                    still(&f.untagged, 0.0, 0.6),
                    dissolving(moving(p, 0.4, 0.6, mv), TransitionKind::DipToWhite, 0.4),
                ]],
                cover(320, 180),
            );
            tl.overlays.clear();
            v.push(Scenario {
                name: format!("{kind} {route} dipping from white"),
                tl,
                told: vec![p.clone(), f.untagged.clone()],
                assets: vec![p.clone(), f.untagged.clone()],
                width: u32::MAX,
                stills: vec![0.5, 0.7],
                fps_secs: Some((10.0, 1.0)),
            });
        }
    }
    add(
        &mut v,
        "png off, 6000x4000 into 64x36 (shrink past 63:1)",
        timeline(vec![vec![moving(&big, 0.0, 1.0, OFF_A)]], cover(64, 36)),
        vec![big.clone()],
        None,
        u32::MAX,
    );
    add(
        &mut v,
        "png in, 6000x4000 into 64x36",
        timeline(vec![vec![moving(&big, 0.0, 1.0, IN_A)]], cover(64, 36)),
        vec![big],
        None,
        u32::MAX,
    );
    for (name, file, told_pix) in [
        ("gray png", &gray, "rgb24"),
        ("rgba png", &rgba, "rgb24"),
        ("4:2:2 jpeg", &j422, "yuvj420p"),
        ("4:4:4 jpeg", &j444, "yuvj420p"),
        ("limited-range 4:2:0 jpeg", &j420l, "yuvj420p"),
    ] {
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let tl = timeline(vec![vec![moving(file, 0.0, 1.0, mv)]], cover(320, 180));
            add(
                &mut v,
                &format!("{name} {route} (the plan told {told_pix})"),
                tl,
                vec![file.clone()],
                Some(vec![told_as(file, told_pix)]),
                u32::MAX,
            );
        }
    }
    // The same photograph twice with different chains (the copies must not be shared), and on two canvases one after another.
    let tl = timeline(
        vec![vec![moving(&pn, 0.0, 1.0, IN_B)], vec![moving(&pn, 0.0, 1.0, OFF_B)]],
        cover(320, 180),
    );
    add(
        &mut v,
        "png twice: the route over the placement",
        tl,
        vec![pn.clone()],
        None,
        u32::MAX,
    );
    let tl = timeline(
        vec![vec![moving(&pn, 0.0, 1.0, OFF_B)], vec![moving(&pn, 0.0, 1.0, IN_B)]],
        cover(320, 180),
    );
    add(
        &mut v,
        "png twice: the placement over the route",
        tl,
        vec![pn.clone()],
        None,
        u32::MAX,
    );
    for (cw, ch) in [(320, 180), (180, 320), (200, 200)] {
        let tl = timeline(vec![vec![moving(&pn, 0.0, 1.0, OFF_A)]], cover(cw, ch));
        add(
            &mut v,
            &format!("png off on a {cw}x{ch} canvas (same file, other canvases)"),
            tl,
            vec![pn.clone()],
            None,
            u32::MAX,
        );
        let tl = timeline(vec![vec![moving(&pn, 0.0, 1.0, IN_A)]], cover(cw, ch));
        add(
            &mut v,
            &format!("png in on a {cw}x{ch} canvas (same file, other canvases)"),
            tl,
            vec![pn.clone()],
            None,
            u32::MAX,
        );
    }
    // After footage that names a matrix (the PNG not on screen with it), and beside it.
    for (vname, video) in [("bt709", &f.bt709), ("bt2020", &f.bt2020), ("full-range", &f.full_untagged)] {
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let tl = timeline(
                vec![vec![
                    still(video, 0.0, 1.0),
                    dissolving(moving(&pn, 0.8, 1.0, mv), TransitionKind::Crossfade, 0.4),
                ]],
                cover(320, 180),
            );
            v.push(Scenario {
                name: format!("png {route} after {vname} footage"),
                tl,
                told: vec![pn.clone(), video.clone()],
                assets: vec![pn.clone(), video.clone()],
                width: u32::MAX,
                stills: vec![0.4, 0.9, 1.5],
                fps_secs: Some((10.0, 1.8)),
            });
        }
    }
    v
}

/// Every scenario, Still frames and export frames, with the outcome of each kept as one word: `EXACT`, `OFF`, `REFUSED`, `FAILED`.
/// `anyway` draws with every kind of picture in the caps. Judges nothing but a drawn frame that differs when `strict`.
fn run_scenarios(prefix: &str, anyway: bool, strict: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let mut fails = Vec::new();
    let only: Vec<String> = std::env::var("KERF_SCEN")
        .map(|v| v.split(';').filter(|x| !x.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    for s in scenarios() {
        if !only.is_empty() && !only.iter().any(|o| s.name.contains(o.as_str())) {
            continue;
        }
        let still_case = StillCase {
            family: format!("{prefix}/{}", s.name),
            name: s.name.clone(),
            tl: s.tl.clone(),
            assets: s.assets.clone(),
            told: s.told.clone(),
            times: s.stills.clone(),
            width: s.width,
            opts: ExportOptions::default(),
            anyway,
        };
        let mut word = |outs: Vec<Out>, what: &str| {
            let (mut exact, mut off, mut refused, mut failed) = (0, 0, 0, 0);
            let mut note = String::new();
            for o in &outs {
                match o {
                    Out::Exact => exact += 1,
                    Out::Off(w) => {
                        off += 1;
                        if note.is_empty() {
                            note = w.chars().take(150).collect();
                        }
                    }
                    Out::Refused(k, w) => {
                        refused += 1;
                        if note.is_empty() {
                            note = format!("[{k}] {}", w.chars().take(110).collect::<String>());
                        }
                    }
                    Out::Failed(w) => {
                        failed += 1;
                        if note.is_empty() {
                            note = w.chars().take(150).collect();
                        }
                    }
                }
            }
            let verdict = if off > 0 {
                "OFF"
            } else if failed > 0 {
                "FAILED"
            } else if exact > 0 && refused == 0 {
                "EXACT"
            } else if exact > 0 {
                "MIXED"
            } else {
                "REFUSED"
            };
            lines.push(format!(
                "A6 SCENARIO {verdict:<7} {what:<7} {:<64} exact {exact} off {off} refused {refused} failed {failed} {note}",
                s.name
            ));
            if strict && (off > 0 || failed > 0) {
                fails.push(format!("{} ({what}): {note}", s.name));
            }
        };
        word(
            still_case.times.iter().map(|&t| still_frame(&still_case, t).0).collect(),
            "still",
        );
        if let Some((fps, secs)) = s.fps_secs {
            let mut ec =
                exports(&format!("{prefix}/{}", s.name), &s.name, s.tl.clone(), &s.assets, fps, secs).told(s.told.clone());
            ec.anyway = anyway;
            word(export_frames(&ec).into_iter().map(|(_, o, _)| o).collect(), "export");
        }
    }
    for l in &lines {
        eprintln!("{l}");
    }
    assert!(
        fails.is_empty() || !strict,
        "{} scenarios drawn and different: {fails:?}",
        fails.len()
    );
    lines
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_scenarios_as_planned() {
    let lines = run_scenarios("scenario", false, true);
    let count = |w: &str| lines.iter().filter(|l| l.contains(&format!("SCENARIO {w}"))).count();
    eprintln!(
        "A6 SCENARIOS as planned: {} exact, {} mixed, {} refused, {} off, {} failed",
        count("EXACT"),
        count("MIXED"),
        count("REFUSED"),
        count("OFF"),
        count("FAILED")
    );
}

/// The refusals drawn anyway: every kind of picture in the caps and the plan told a kind it takes where the refusal is about the kind. The
/// plan-level refusals of a moving still stay in force unless the build is patched (`KERF_MUT=skip_...`): the patched run is the verdict.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); the plan-level refusals need the scratch patch"]
fn checker_a_t6_refusals_drawn_anyway() {
    let lines = run_scenarios("anyway", true, false);
    let wide = lines.iter().filter(|l| l.contains("SCENARIO EXACT")).count();
    let necessary = lines
        .iter()
        .filter(|l| l.contains("SCENARIO OFF") || l.contains("SCENARIO FAILED"))
        .count();
    eprintln!("A6 ANYWAY: {wide} scenarios exact when drawn anyway, {necessary} inexact or undrawable");
}

// ---- family: files of every make -----------------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg, python3 with Pillow and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_files_of_every_make() {
    let mut variants = t6_files::jpeg_variants();
    variants.extend(t6_files::png_variants());
    variants.extend(t6_files::other_formats());
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    for (name, photo) in variants {
        let fmt = fmt_of(&photo);
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let family = format!("files/{name} [{fmt}] {route}");
            let tl = timeline(vec![vec![moving(&photo, 0.0, 1.0, mv)]], cover(320, 180));
            still_cases.push(stills(
                &family,
                "320x180",
                tl.clone(),
                std::slice::from_ref(&photo),
                &[0.0, 0.5, 0.99],
            ));
            export_cases.push(exports(&family, "320x180", tl, std::slice::from_ref(&photo), 10.0, 1.0));
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("files of every make", "files/", vec![a, b]);
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_a_t6_probe_verdicts() {
    require_cold();
    let started = std::time::Instant::now();
    let (j, r) = (kerf_gpu::still_probe::jpeg(), kerf_gpu::still_probe::rgb());
    eprintln!(
        "A6 PROBE {} policy {:?}: jpeg {:?}, rgb {:?}, measured {:?}, took {:.1?}",
        kerf_core::ffmpeg_path(),
        policy(),
        j,
        r,
        kerf_gpu::still_probe::measured(),
        started.elapsed()
    );
    let again = std::time::Instant::now();
    let _ = kerf_gpu::still_probe::kinds();
    assert!(
        again.elapsed() < std::time::Duration::from_millis(500),
        "an answered probe was run again"
    );
}

// ---- the size a copy was made for ---------------------------------------------------------------------------------

/// A plan worked out for one render size is refused at another by the plan, and by the render when a caller composites by hand; an export frame
/// (made for the delivery's frame) at any other size; and a still plan that was never worked out is refused by the render, never drawn from a
/// picture the compositor scaled itself.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_a_copy_is_drawn_only_at_the_size_it_was_made_for() {
    let comp = compositor();
    let mut seen = Vec::new();
    for kind in ["png", "jpeg"] {
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let photo = pic(kind, 1200, 800);
            let assets = std::slice::from_ref(&photo);
            let tl = timeline(vec![vec![moving(&photo, 0.0, 2.0, mv)]], cover(320, 180));
            let plan = RenderPlan::at(&tl, assets, &ExportOptions::default(), 1.0, policy()).unwrap();
            let worked = plan.clone().for_render((320, 180));
            let kinds_cannot_draw =
                |p: &RenderPlan, size| p.reasons(&comp.caps(), size).iter().any(|u| u.kind() == "still_motion");
            if kind == "jpeg" && negotiated() {
                // (A JPEG is refused for its range here whatever the size: nothing more to say.)
                continue;
            }
            assert!(
                !kinds_cannot_draw(&worked, (320, 180)),
                "{kind} {route}: refused at the size it was worked out for"
            );
            assert!(
                kinds_cannot_draw(&worked, (160, 90)),
                "{kind} {route}: a plan worked out for 320x180 was judged drawable at 160x90"
            );
            // By hand: the frames of the right plan, composited at the wrong size.
            let frames = shared_frames(&worked.layers).expect("the decode");
            let wrong = refusal_text(comp.composite_yuv_shared(&worked, &frames, (160, 90)));
            assert!(
                wrong.is_some_and(|w| !w.starts_with("NOT A REFUSAL")),
                "{kind} {route}: composited by hand at another size"
            );
            // A plan that was never worked out.
            let cold = refusal_text(comp.composite_yuv_shared(&plan, &frames, (320, 180)));
            let cold_reasons = plan.reasons(&comp.caps(), (320, 180));
            seen.push(format!(
                "{kind} {route}: worked-out plan refused at 160x90 and by hand; an unworked plan {} (reasons {:?})",
                cold.as_deref().map_or("was DRAWN".to_string(), |c| format!(
                    "refused by the render: {}",
                    c.chars().take(80).collect::<String>()
                )),
                cold_reasons.iter().map(|u| u.kind()).collect::<Vec<_>>()
            ));
            // An export frame at another size than the delivery's.
            let planner = Planner::new(&tl, assets, &at_fps(10.0), PlanRequest::motion(policy())).unwrap();
            let frame = planner.at_frame(5).unwrap();
            assert!(
                !kinds_cannot_draw(&frame, (320, 180)),
                "{kind} {route}: an export frame refused at the delivery's frame"
            );
            assert!(
                kinds_cannot_draw(&frame, (160, 90)),
                "{kind} {route}: an export frame judged drawable at 160x90"
            );
            let mut motion = MotionFrames::new(frame_source());
            let picture = motion.frames(&frame).expect("frames");
            let wrong = refusal_text(comp.composite_yuv_counted(&frame, &picture, motion.counts(), (160, 90)));
            assert!(
                wrong.is_some_and(|w| !w.starts_with("NOT A REFUSAL")),
                "{kind} {route}: an export frame composited by hand at another size"
            );
        }
    }
    for s in &seen {
        eprintln!("A6 SIZE {s}");
    }
}

// ---- F3: a PNG after a BT.709 clip ---------------------------------------------------------------------------------

/// A PNG, static (no resize: the picture is the canvas's own size) and moving (both routes), cut in after or before or over a clip that names
/// BT.709 (and one that is untagged, BT.2020, full range), in Still frames and in export frames: drawn exactly or refused, on both builds.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_a_png_after_a_bt709_clip() {
    let f = clips();
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    let picture = png(320, 180);
    let tagged: Vec<(&str, &Asset)> = vec![
        ("bt709", &f.bt709),
        ("untagged", &f.untagged),
        ("bt2020", &f.bt2020),
        ("bt601", &f.bt601),
        ("full range bt709", &f.full_709),
    ];
    for (vname, video) in tagged {
        let assets = [picture.clone(), video.clone()];
        let arrangements: Vec<(&str, Vec<Vec<Clip>>, f64)> = vec![
            (
                "static png dissolving in after",
                vec![vec![
                    still(video, 0.0, 1.5),
                    dissolving(still(&picture, 1.0, 1.5), TransitionKind::Crossfade, 0.5),
                ]],
                2.5,
            ),
            (
                "static png cut in after",
                vec![vec![still(video, 0.0, 1.0), still(&picture, 1.0, 1.5)]],
                2.5,
            ),
            (
                "static png before",
                vec![vec![still(&picture, 0.0, 1.0), still(video, 1.0, 1.5)]],
                2.5,
            ),
            (
                "moving png (inside) cut in after",
                vec![vec![still(video, 0.0, 1.0), moving(&picture, 1.0, 1.5, IN_A)]],
                2.5,
            ),
            (
                "moving png (off the edge) cut in after",
                vec![vec![still(video, 0.0, 1.0), moving(&picture, 1.0, 1.5, OFF_A)]],
                2.5,
            ),
            (
                "moving png (inside) dissolving in after",
                vec![vec![
                    still(video, 0.0, 1.5),
                    dissolving(moving(&picture, 1.0, 1.5, IN_A), TransitionKind::Crossfade, 0.5),
                ]],
                2.5,
            ),
            (
                "static png under the clip",
                vec![vec![still(&picture, 0.0, 1.5)], vec![still(video, 0.5, 0.5)]],
                1.5,
            ),
        ];
        for (aname, tracks, secs) in arrangements {
            let family = format!("f3/{aname}");
            let tl = timeline(tracks, cover(320, 180));
            let times: Vec<f64> = (0..8).map(|i| 0.1 + secs * f64::from(i) / 8.0).collect();
            still_cases.push(stills(&family, &format!("{vname} clip"), tl.clone(), &assets, &times));
            export_cases.push(exports(&family, &format!("{vname} clip"), tl, &assets, 10.0, secs));
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("a PNG after a BT.709 clip", "f3/", vec![a, b]);
}

// ---- family: clip lengths and starts that are not on the frame grid ----------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_clips_of_every_length_and_start() {
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    for kind in ["png", "jpeg"] {
        let photo = pic(kind, 1200, 800);
        let other = pic(kind, 800, 1200);
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let family = format!("lengths/{kind} {route}");
            // Longer than the asset's default duration, a frame, less than a frame, not a whole number of frames, starting between frames.
            for (name, start, secs) in [
                ("8 s (past the asset's 5 s)", 0.0, 8.0),
                ("one frame", 0.0, 0.1),
                ("half a frame", 0.0, 0.05),
                ("1.5 frames", 0.0, 0.15),
                ("starts at 0.033", 0.033, 0.6),
                ("starts at 0.05 (a tie)", 0.05, 0.6),
                ("starts at 0.0999", 0.0999, 0.6),
                ("starts at 0.1001", 0.1001, 0.6),
                ("2.37 s", 0.0, 2.37),
            ] {
                let tl = timeline(vec![vec![moving(&photo, start, secs, mv)]], cover(320, 180));
                let end = start + secs;
                let times: Vec<f64> = [start, start + secs * 0.3, start + secs * 0.7, end - 0.001]
                    .into_iter()
                    .filter(|t| *t >= 0.0 && *t < end)
                    .collect();
                still_cases.push(stills(&family, name, tl.clone(), std::slice::from_ref(&photo), &times));
                export_cases.push(exports(&family, name, tl, std::slice::from_ref(&photo), 10.0, end.min(8.5)));
            }
            // Back to back: the same photograph held across a cut, two photographs, a gap between, a one-frame clip between.
            let tl = timeline(
                vec![vec![
                    moving(&photo, 0.0, 0.5, mv),
                    moving(&photo, 0.5, 0.5, mv),
                    moving(&other, 1.0, 0.1, mv),
                    moving(&photo, 1.3, 0.5, mv),
                ]],
                cover(320, 180),
            );
            let assets = [photo.clone(), other.clone()];
            still_cases.push(stills(
                &family,
                "cuts and a gap",
                tl.clone(),
                &assets,
                &[0.25, 0.5, 0.75, 1.05, 1.2, 1.5],
            ));
            export_cases.push(exports(&family, "cuts and a gap", tl, &assets, 10.0, 1.8));
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("clips of every length and start", "lengths/", vec![a, b]);
}

// ---- the kinds of picture the route is not drawn for, drawn anyway --------------------------------------------------------

/// Every pixel format the route and the fit picture are refused for, with the plan told the file is a kind it takes (`rgb24` for an RGB-family
/// file, `yuvj420p` for a YCbCr one) and every kind in the caps: what the compositor draws from the copy FFmpeg makes, held to FFmpeg's own
/// still and export. Judges nothing: `exact` is a refusal that is wider than it has to be, `OFF` one that is necessary.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_pixel_formats_drawn_anyway() {
    let mut lines = Vec::new();
    let mut variants: Vec<(String, Asset)> = format_files()
        .into_iter()
        .filter(|(_, _, k)| k.is_none())
        .map(|(n, a, _)| (n, a))
        .collect();
    variants.extend(t6_files::other_formats());
    for (name, photo) in variants {
        let fmt = fmt_of(&photo);
        let told_fmt = if fmt.starts_with("yuv") || fmt.starts_with("gray") && name.contains("jpeg") {
            "yuvj420p"
        } else {
            "rgb24"
        };
        let told = told_as(&photo, told_fmt);
        for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
            let tl = timeline(vec![vec![moving(&photo, 0.0, 1.0, mv)]], cover(320, 180));
            let still = StillCase {
                told: vec![told.clone()],
                anyway: true,
                ..stills("anyway", &name, tl.clone(), std::slice::from_ref(&photo), &[0.0, 0.5, 0.99])
            };
            let export_case = ExportCase {
                told: vec![told.clone()],
                anyway: true,
                ..exports("anyway", &name, tl, std::slice::from_ref(&photo), 10.0, 1.0)
            };
            let mut outs: Vec<Out> = still.times.iter().map(|&t| still_frame(&still, t).0).collect();
            outs.extend(export_frames(&export_case).into_iter().map(|(_, o, _)| o));
            let word = |f: fn(&Out) -> bool| outs.iter().filter(|o| f(o)).count();
            let (exact, off, refused, failed) = (
                word(|o| matches!(o, Out::Exact)),
                word(|o| matches!(o, Out::Off(_))),
                word(|o| matches!(o, Out::Refused(..))),
                word(|o| matches!(o, Out::Failed(_))),
            );
            let note = outs
                .iter()
                .find_map(|o| match o {
                    Out::Off(w) | Out::Failed(w) => Some(w.chars().take(120).collect::<String>()),
                    Out::Refused(k, w) => Some(format!("[{k}] {}", w.chars().take(90).collect::<String>())),
                    Out::Exact => None,
                })
                .unwrap_or_default();
            let verdict = if off + failed > 0 {
                "NECESSARY"
            } else if exact > 0 && refused == 0 {
                "WIDER"
            } else if exact > 0 {
                "MIXED"
            } else {
                "STILL REFUSED"
            };
            lines.push(format!(
                "A6 ANYWAY {verdict:<13} {name:<28} [{fmt}] told {told_fmt} {route:<3} exact {exact} off {off} refused {refused} failed {failed} {note}"
            ));
        }
    }
    for l in &lines {
        eprintln!("{l}");
    }
}

// ---- the repros of the findings that are not the moving stills' ----------------------------------------------------------

/// A still that does not move (the path the moving-still package did not touch): a JPEG resized into the canvas and a PNG at the canvas's own
/// size, in a cut of its own. The frame is drawn and compared; the montage goes to `$KERF_A6_MONTAGE`.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_repro_a_static_jpeg_resized_is_drawn_and_not_exact() {
    let mut cases = Vec::new();
    for (w, h) in [(1200, 800), (1920, 1080), (640, 360), (320, 180)] {
        let photo = jpeg(w, h);
        let tl = timeline(vec![vec![still(&photo, 0.0, 2.0)]], cover(320, 180));
        cases.push(stills(
            "repro/static jpeg",
            &format!("static jpeg {w}x{h} into 320x180"),
            tl,
            std::slice::from_ref(&photo),
            &[0.5],
        ));
    }
    let photo = png(320, 180);
    let tl = timeline(vec![vec![still(&photo, 0.0, 2.0)]], cover(320, 180));
    cases.push(stills(
        "repro/static png",
        "static png 320x180 into 320x180",
        tl,
        std::slice::from_ref(&photo),
        &[0.5],
    ));
    let a = judge_stills(&cases);
    eprintln!("A6 REPRO static: {} frames drawn and different", a.fails.len());
    report("repro/");
}

/// Full-range footage on screen in a Still plan, with and without a PNG on it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_repro_full_range_footage_in_a_still_plan() {
    let f = clips();
    let mut cases = Vec::new();
    let tl = timeline(vec![vec![still(&f.full_untagged, 0.0, 2.0)]], cover(320, 180));
    cases.push(stills(
        "repro/full-range footage alone",
        "full-range untagged footage alone",
        tl,
        std::slice::from_ref(&f.full_untagged),
        &[0.2, 1.0],
    ));
    let photo = png(1200, 800);
    let tl = timeline(
        vec![vec![still(&f.full_untagged, 0.0, 1.5)], vec![moving(&photo, 0.0, 1.5, IN_B)]],
        cover(320, 180),
    );
    cases.push(stills(
        "repro/png over full-range footage",
        "moving png (inside) over full-range untagged footage",
        tl,
        &[photo.clone(), f.full_untagged.clone()],
        &[0.2, 0.9],
    ));
    let tl = timeline(
        vec![vec![still(&f.untagged, 0.0, 1.5)], vec![moving(&photo, 0.0, 1.5, IN_B)]],
        cover(320, 180),
    );
    cases.push(stills(
        "repro/png over untagged footage (control)",
        "moving png (inside) over limited-range untagged footage",
        tl,
        &[photo, f.untagged.clone()],
        &[0.2, 0.9],
    ));
    let a = judge_stills(&cases);
    eprintln!("A6 REPRO footage: {} frames drawn and different", a.fails.len());
    report("repro/");
}

// ---- family: frame rates -----------------------------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_t6_frame_rates() {
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    for kind in ["png", "jpeg"] {
        let photo = pic(kind, 1200, 800);
        for fps in [23.976, 24.0, 25.0, 29.97, 30.0, 48.0, 50.0, 59.94, 60.0, 12.5, 1.0, 3.0] {
            for (route, mv) in [("in", IN_A), ("off", OFF_A)] {
                let family = format!("rates/{kind} {route}");
                let secs = if fps > 40.0 { 0.5 } else { 1.0 };
                let tl = timeline(vec![vec![moving(&photo, 0.0, secs, mv)]], delivery(320, 180, Fit::Cover, fps));
                let name = format!("{fps} fps");
                let times = [0.0, secs * 0.4, secs - 0.001];
                still_cases.push(stills(&family, &name, tl.clone(), std::slice::from_ref(&photo), &times));
                let mut case = exports(&family, &name, tl, std::slice::from_ref(&photo), fps, secs);
                case.opts = at_fps(fps);
                case.frames = 0..((secs * fps).floor() as u64).max(1);
                export_cases.push(case);
            }
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("frame rates", "rates/", vec![a, b]);
}
