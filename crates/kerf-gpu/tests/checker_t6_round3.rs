//! T6 fix round 3, checked independently on both FFmpeg builds: a fit picture whose text has a `crop` besides the guard (a Contain frame behind
//! the clip's static crop, a static crop ahead of a held zoom, the Cover overhang cut after a first scale) is `FitShape::Cropped`, which no toy
//! cut of the probe takes: the plan must refuse every frame that holds one, whatever the caps (measured, or every chain pinned), and the text of
//! every copy that is drawn is classified here by hand (a splitter of the top-level filters, not the plan's `FitShape`) and must be a text a toy
//! cut ran: the route's, the engine's, the engine's behind a static crop, or the geometry's `scale`s behind the guard and no other `crop`. Every frame
//! that is drawn is held to FFmpeg's with no tolerance (a Still frame is FFmpeg's own still, an export frame the lossless export's planes).
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_t6_round3 -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
//!
//! Families: `r3s` (every staged shape reached by construction: Contain, a held zoom, the Cover overhang, static crops, odd sizes, one pixel
//! off the frame's shape, a static zoom with a keyed position), `r3x` (a seeded sweep weighted to those shapes), `r3a` (the Cropped frames
//! drawn anyway, which needs a scratch patch of `src/`: unpatched every frame of it is refused and nothing is judged).

#![allow(
    dead_code,
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
    Asset, Clip, CompositeColorPolicy, Container, Delivery, ExportOptions, Fit, Keyframe, OriginalMedia, PlanRequest, Planner,
    Project, RateControl, RenderPlan, StreamKind, Timeline, Track, Transition, TransitionKind,
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
            "R3 FAMILY {name:<48} drawn {:>4}  exact {:>4}  inexact {:>3}  refused {:>4}  failed {:>2}  [{}]{}",
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
        "R3 TOTAL {prefix}: drawn {}  exact {exact}  inexact {off}  refused {refused}  failed {failed}",
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("r3-t6-media");
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
    let dir = std::env::var_os("KERF_R2_MONTAGE").map_or_else(|| target_dir().join("parity"), PathBuf::from);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("r2-{}-{}.png", slug(case), slug(t)));
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

    fn scaler(mut self, scaler: Option<&str>) -> Self {
        self.opts.scaler = scaler.map(String::from);
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
    if !c.anyway {
        kerf_gpu::prepare(&plan);
    }
    let why = reasons_of(&plan, comp, size);
    note_classes(&plan, size, why.is_empty());
    if !why.is_empty() {
        return refused_with(&why);
    }
    *ex = Vec::new();
    if let Err(e) = chain_audit(&plan, size, !c.anyway) {
        return Out::Failed(format!("chain audit: {e}"));
    }
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
            eprintln!("R3 still  {:<28} {:<60} t={t:<6} {tag}", c.family, c.name);
            match out {
                Out::Off(w) | Out::Failed(w) => fails.push(format!("{} / {} @ {t}: {w}", c.family, c.name)),
                Out::Refused(k, w) if k == "render" && !c.anyway => fails.push(format!(
                    "{} / {} @ {t}: the plan drew it and the render refused: {w}",
                    c.family, c.name
                )),
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

    fn scaler(mut self, scaler: Option<&str>) -> Self {
        self.opts.scaler = scaler.map(String::from);
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
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("r3-t6-ref");
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
                .collect();
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
        if !c.anyway {
            kerf_gpu::prepare(&plan);
        }
        let why = reasons_of(&plan, comp, size);
        note_classes(&plan, size, why.is_empty());
        if !why.is_empty() {
            out.push((k, refused_with(&why), vec![]));
            continue;
        }
        let ex: Vec<&'static str> = Vec::new();
        if let Err(e) = chain_audit(&plan, size, !c.anyway) {
            out.push((k, Out::Failed(format!("chain audit: {e}")), vec![]));
            continue;
        }
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
                Out::Refused(why, w) => {
                    refused += 1;
                    if why == "render" && !c.anyway {
                        offs.push(*k);
                        first_off.get_or_insert(format!("frame {k}: the plan drew it and the render refused: {w}"));
                    }
                }
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
            "R3 export {:<28} {:<60} {} frames: exact {exact}, refused {refused}, off/failed {}{}{}",
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
        "R3 {what}: drawn {drawn}, exact {exact}, inexact {off}, refused {refused}, failed {failed}, failing cases {}",
        fails.len()
    );
    for f in &fails {
        eprintln!("R3 FAIL {f}");
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

// ---- the timelines of this round ------------------------------------------------------------------------------------------

/// A zoom held (a second stage) while the position moves.
const HELD_08: (Mv, Mv) = ((0.8, (-0.04, 0.0)), (0.8, (0.04, -0.02)));
const HELD_05: (Mv, Mv) = ((0.5, (-0.05, 0.0)), (0.5, (0.05, 0.03)));
const HELD_10: (Mv, Mv) = ((1.0, (-0.05, 0.0)), (1.0, (0.05, 0.0)));
const HELD_125: (Mv, Mv) = ((1.25, (-0.1, 0.0)), (1.25, (0.1, 0.0)));

const CROPS3: [(f64, f64, f64, f64); 6] = [
    (0.1, 0.05, 0.0, 0.0),
    (0.0, 0.0, 0.2, 0.13),
    (0.07, 0.11, 0.03, 0.09),
    (0.3, 0.0, 0.0, 0.3),
    (0.013, 0.007, 0.011, 0.017),
    (0.25, 0.25, 0.25, 0.25),
];

// ---- the text of the copy, classified by hand --------------------------------------------------------------------------------

fn split_filters(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for ch in s.chars() {
        match ch {
            '\'' => {
                quoted = !quoted;
                cur.push(ch);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn name_of(part: &str) -> &str {
    part.split('=').next().unwrap_or("")
}

#[derive(Debug, Clone, PartialEq)]
enum Class {
    Route { bicubic: bool },
    Engine { bicubic: bool },
    EngineCrop { bicubic: bool },
    Staged { bicubic: bool },
    Cropped { bicubic: bool },
    Unknown(String),
}

fn staged_scale(p: &str) -> Option<bool> {
    let a: Vec<&str> = p.strip_prefix("scale=")?.split(':').collect();
    match a.as_slice() {
        [w, h] if digits(w) && digits(h) => Some(false),
        [w, h, "flags=bicubic"] if digits(w) && digits(h) => Some(true),
        _ => None,
    }
}

fn engine_scale(p: &str) -> Option<(String, bool)> {
    let a: Vec<&str> = p.strip_prefix("scale=")?.split(':').collect();
    match a.as_slice() {
        [w, h, "force_original_aspect_ratio=increase"] if digits(w) && digits(h) => Some((format!("{w}:{h}"), false)),
        [w, h, "force_original_aspect_ratio=increase", "flags=bicubic"] if digits(w) && digits(h) => {
            Some((format!("{w}:{h}"), true))
        }
        _ => None,
    }
}

fn plain_crop(p: &str) -> Option<Vec<String>> {
    let a: Vec<String> = p.strip_prefix("crop=")?.split(':').map(String::from).collect();
    a.iter().all(|x| digits(x)).then_some(a)
}

/// The class of the text of a copy (with the closing `format=yuva420p` the compositor's copy has).
fn class_of(text: &str) -> Class {
    let unknown = || Class::Unknown(text.to_string());
    let mut parts = split_filters(text);
    if parts.pop().as_deref() != Some("format=yuva420p") {
        return unknown();
    }
    match parts.as_slice() {
        [g, rest @ ..] if g.starts_with("crop=w='if(eq(iw,") && g.ends_with(":exact=1") => {
            let (mut scales, mut flagged, mut crops) = (0, 0, 0);
            for p in rest {
                match name_of(p) {
                    "scale" => match staged_scale(p) {
                        Some(b) => {
                            scales += 1;
                            flagged += usize::from(b);
                        }
                        None => return unknown(),
                    },
                    "crop" => {
                        if plain_crop(p).is_none_or(|a| a.len() != 4) {
                            return unknown();
                        }
                        crops += 1;
                    }
                    _ => return unknown(),
                }
            }
            if scales == 0 || (flagged != 0 && flagged != scales) {
                return unknown();
            }
            if crops == 0 {
                Class::Staged { bicubic: flagged != 0 }
            } else {
                Class::Cropped { bicubic: flagged != 0 }
            }
        }
        [s, c] if s.starts_with("scale=") && !s.contains("force_original") => {
            let a: Vec<&str> = s.strip_prefix("scale=").unwrap().split(':').collect();
            match (a.as_slice(), plain_crop(c)) {
                ([w, h, flags @ ("flags=lanczos" | "flags=bicubic")], Some(crop))
                    if digits(w) && digits(h) && crop.len() == 4 =>
                {
                    Class::Route {
                        bicubic: *flags == "flags=bicubic",
                    }
                }
                _ => unknown(),
            }
        }
        [s, c] => match (engine_scale(s), plain_crop(c)) {
            (Some((size, bicubic)), Some(crop)) if crop.join(":") == size => Class::Engine { bicubic },
            _ => unknown(),
        },
        [k, s, c] if k.starts_with("crop=w=iw*") => {
            let a: Vec<&str> = k.strip_prefix("crop=").unwrap().split(':').collect();
            let well_formed = a.len() == 4
                && ["w=iw*", "h=ih*", "x=iw*", "y=ih*"]
                    .iter()
                    .zip(&a)
                    .all(|(prefix, x)| x.strip_prefix(prefix).is_some_and(|n| n.parse::<f64>().is_ok()));
            match (well_formed, engine_scale(s), plain_crop(c)) {
                (true, Some((size, bicubic)), Some(crop)) if crop.join(":") == size => Class::EngineCrop { bicubic },
                _ => unknown(),
            }
        }
        _ => unknown(),
    }
}

fn label(class: &Class) -> String {
    let b = |bicubic: &bool| if *bicubic { "+bicubic" } else { "" };
    match class {
        Class::Route { bicubic } => format!("Route{}", b(bicubic)),
        Class::Engine { bicubic } => format!("Engine{}", b(bicubic)),
        Class::EngineCrop { bicubic } => format!("EngineCrop{}", b(bicubic)),
        Class::Staged { bicubic } => format!("Staged{}", b(bicubic)),
        Class::Cropped { bicubic } => format!("Cropped{}", b(bicubic)),
        Class::Unknown(t) => format!("Unknown({t})"),
    }
}

/// The class of the text of every copy the plan has (each layer that is drawn from a copy FFmpeg makes).
fn classes(plan: &RenderPlan) -> Vec<Class> {
    plan.layers
        .iter()
        .filter_map(|l| match kerf_gpu::source::copy_of(l) {
            Ok(Some(copy)) => Some(class_of(&copy.filters)),
            _ => None,
        })
        .collect()
}

fn class_tally() -> &'static Mutex<BTreeMap<String, (usize, usize)>> {
    static T: OnceLock<Mutex<BTreeMap<String, (usize, usize)>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// `(frames with a copy of this class, of them the frames the plan lets through)`.
fn note_classes(plan: &RenderPlan, _size: (u32, u32), drawn: bool) {
    let mut all = class_tally().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for class in classes(plan) {
        let e = all.entry(label(&class).trim_end_matches("+bicubic").to_string()).or_default();
        e.0 += 1;
        e.1 += usize::from(drawn);
    }
}

fn tally_of(class: &str) -> (usize, usize) {
    class_tally()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(class)
        .copied()
        .unwrap_or_default()
}

/// Whether the cases audit the text of the copy (off only where a case draws a plan it knows to be refused).
static AUDIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Every layer the plan draws a copy for has a text a toy cut ran (no crop besides the guard), the chain the plan says, and that chain and its
/// kind's base chains were measured.
fn chain_audit(plan: &RenderPlan, size: (u32, u32), measured: bool) -> Result<(), String> {
    if !AUDIT.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    for layer in &plan.layers {
        let Ok(Some(copy)) = kerf_gpu::source::copy_of(layer) else {
            continue;
        };
        let class = class_of(&copy.filters);
        let want = match class {
            Class::Route { bicubic } => kerf_core::StillChain::inside(bicubic),
            Class::Engine { bicubic } => kerf_core::StillChain::edge(kerf_core::FitShape::Engine, bicubic),
            Class::EngineCrop { bicubic } => kerf_core::StillChain::edge(kerf_core::FitShape::EngineCrop, bicubic),
            Class::Staged { bicubic } => kerf_core::StillChain::edge(kerf_core::FitShape::Staged, bicubic),
            Class::Cropped { .. } => {
                return Err(format!(
                    "a text with a crop besides the guard reached the draw: {}",
                    copy.filters
                ));
            }
            Class::Unknown(t) => return Err(format!("a text of no shape a toy cut ran: {t}")),
        };
        let said = plan
            .still_chain(layer, size)
            .ok_or_else(|| format!("a layer with a copy and no chain: {}", copy.filters))?;
        if said != want {
            return Err(format!("the plan says {said:?}, the text is {want:?}: {}", copy.filters));
        }
        let kind = plan.still_kind(layer).ok_or("a copy for a layer of no measured kind")?;
        if measured {
            let m = kerf_gpu::still_probe::measured();
            if !m.contains(kind) {
                return Err(format!("{kind:?} is drawn and its base chains were not measured"));
            }
            if !m.contains_chain(kind, said) {
                return Err(format!("{kind:?} {said:?} is drawn and was not measured"));
            }
        }
    }
    Ok(())
}

// ---- the specs ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Mo {
    Keys((Mv, Mv)),
    /// A static zoom with the position keyed (the zoom is a `scale` ahead of the placement and not a keyed one).
    StaticZoom(f64, bool),
}

#[derive(Clone, Copy, Debug)]
struct Spec {
    cause: &'static str,
    kind: &'static str,
    pic: (u32, u32),
    canvas: (u32, u32),
    fit: Fit,
    mo: Mo,
    crop: Option<usize>,
    scaler: Option<&'static str>,
    fps: f64,
}

fn spec_clip(photo: &Asset, spec: &Spec) -> Clip {
    let mut c = match spec.mo {
        Mo::Keys(mv) => moving(photo, 0.0, 1.0, mv),
        Mo::StaticZoom(z, vertical) => {
            let mut c = Clip::new(photo.id, 0.0, 1.0, 0.0);
            c.transform.scale = z;
            let prop = if vertical {
                kerf_core::Property::PosY
            } else {
                kerf_core::Property::PosX
            };
            c.set_property_keys(
                prop,
                vec![
                    kerf_core::PropertyKey::new(0.0, -0.05),
                    kerf_core::PropertyKey::new(1.0, 0.05),
                ],
            );
            c
        }
    };
    if let Some(i) = spec.crop {
        let (l, t, r, b) = CROPS3[i % CROPS3.len()];
        c.transform.crop_left = l;
        c.transform.crop_top = t;
        c.transform.crop_right = r;
        c.transform.crop_bottom = b;
    }
    c
}

fn spec_case(spec: &Spec) -> (StillCase, ExportCase) {
    let photo = pic(spec.kind, spec.pic.0, spec.pic.1);
    let c = spec_clip(&photo, spec);
    let tl = timeline(vec![vec![c]], delivery(spec.canvas.0, spec.canvas.1, spec.fit, spec.fps));
    let family = format!("r3s/{}/{}/{}", spec.cause, spec.kind, spec.scaler.unwrap_or("default"));
    let name = format!(
        "{} {}x{} on {}x{} {:?} {:?} crop {:?} @{}",
        spec.kind, spec.pic.0, spec.pic.1, spec.canvas.0, spec.canvas.1, spec.fit, spec.mo, spec.crop, spec.fps
    );
    let assets = std::slice::from_ref(&photo);
    let mut s = stills(&family, &name, tl.clone(), assets, &[0.0, 0.31, 0.5, 0.99]).scaler(spec.scaler);
    s.opts.fps = Some(spec.fps);
    let e = exports(&family, &name, tl, assets, spec.fps, 1.0).scaler(spec.scaler);
    (s, e)
}

const CANVASES: [(u32, u32); 7] = [
    (320, 180),
    (426, 240),
    (322, 182),
    (180, 320),
    (256, 256),
    (640, 360),
    (160, 90),
];

fn shape_specs() -> Vec<Spec> {
    let mut v = Vec::new();
    let mut n = 0usize;
    for kind in ["png", "jpeg"] {
        for scaler in [None, Some("bicubic")] {
            let s = |cause, pic, canvas, fit, mo, crop| Spec {
                cause,
                kind,
                pic,
                canvas,
                fit,
                mo,
                crop,
                scaler,
                fps: 10.0,
            };
            // A Contain frame: the picture alone with bars, one `scale` (the geometry's stage), no crop.
            let contain_pics: [(u32, u32); 8] = [
                (1200, 800),
                (801, 601),
                (160, 90),
                (90, 160),
                (641, 479),
                (400, 3000),
                (64, 64),
                (1920, 1080),
            ];
            for (i, p) in contain_pics.into_iter().enumerate() {
                n += 1;
                let canvas = CANVASES[(n + i) % CANVASES.len()];
                let mv = [OFF_A, OFF_B, OUT_A][i % 3];
                v.push(s("contain", p, canvas, Fit::Contain, Mo::Keys(mv), None));
                // ... behind the clip's static crop: a `crop` besides the guard.
                v.push(s("contain-crop", p, canvas, Fit::Contain, Mo::Keys(mv), Some(i % 6)));
            }
            // A held zoom out of a picture of the frame's own shape (no overhang: a second `scale` and no crop), and one pixel or two off it
            // either way (the cover hangs over the frame by a pixel and is cut).
            for (i, k) in [1u32, 2, 3, 2].into_iter().enumerate() {
                n += 1;
                let canvas = CANVASES[(n + 1) % CANVASES.len()];
                let mv = [HELD_08, HELD_05, HELD_10, HELD_125][i];
                v.push(s(
                    "held-shape",
                    (canvas.0 * k, canvas.1 * k),
                    canvas,
                    Fit::Cover,
                    Mo::Keys(mv),
                    None,
                ));
            }
            for (i, d) in [(1u32, 0u32), (0, 1), (2, 2), (3, 0)].into_iter().enumerate() {
                n += 1;
                let canvas = CANVASES[(n + 2) % CANVASES.len()];
                let k = 2 + i as u32 % 2;
                v.push(s(
                    "held-off-by-a-pixel",
                    (canvas.0 * k + d.0, canvas.1 * k + d.1),
                    canvas,
                    Fit::Cover,
                    Mo::Keys(HELD_08),
                    None,
                ));
                v.push(s(
                    "held-off-by-a-pixel",
                    (canvas.0 * k - d.0.min(1), canvas.1 * k - d.1.min(1)),
                    canvas,
                    Fit::Cover,
                    Mo::Keys(HELD_05),
                    None,
                ));
            }
            // A held zoom out of a picture the cover hangs over (wider, taller): the first scale is cut to the frame before the second.
            for (i, p) in [
                (1200u32, 800u32),
                (3000, 400),
                (400, 3000),
                (801, 601),
                (160, 160),
                (1920, 1080),
            ]
            .into_iter()
            .enumerate()
            {
                n += 1;
                let canvas = CANVASES[(n + i) % CANVASES.len()];
                v.push(s(
                    "held-overhang",
                    p,
                    canvas,
                    Fit::Cover,
                    Mo::Keys([HELD_08, HELD_05, HELD_125][i % 3]),
                    None,
                ));
            }
            // ... behind a static crop.
            for i in 0..5usize {
                n += 1;
                let canvas = CANVASES[(n + i) % CANVASES.len()];
                v.push(s(
                    "held-crop",
                    (canvas.0 * 2, canvas.1 * 2),
                    canvas,
                    Fit::Cover,
                    Mo::Keys(HELD_08),
                    Some(i),
                ));
            }
            // A static zoom (a `scale` ahead of the placement) with the position keyed, Cover and Contain.
            for (i, (z, vertical)) in [(0.8, false), (1.4, true), (0.6, false), (1.0, true)].into_iter().enumerate() {
                n += 1;
                let canvas = CANVASES[(n + i) % CANVASES.len()];
                for fit in [Fit::Cover, Fit::Contain] {
                    v.push(s(
                        "static-zoom",
                        (canvas.0 * 2, canvas.1 * 2),
                        canvas,
                        fit,
                        Mo::StaticZoom(z, vertical),
                        None,
                    ));
                    v.push(s(
                        "static-zoom-wide",
                        (1200, 800),
                        canvas,
                        fit,
                        Mo::StaticZoom(z, vertical),
                        (i % 2 == 0).then_some(i),
                    ));
                }
            }
            // Controls: the engine's own text, behind a crop or not (drawn).
            for (i, p) in [(1200u32, 800u32), (801, 601), (640, 360), (90, 160)].into_iter().enumerate() {
                n += 1;
                let canvas = CANVASES[(n + i) % CANVASES.len()];
                v.push(s("engine", p, canvas, Fit::Cover, Mo::Keys([OFF_A, OFF_B][i % 2]), None));
                v.push(s(
                    "engine-crop",
                    p,
                    canvas,
                    Fit::Cover,
                    Mo::Keys([OFF_A, OFF_B][i % 2]),
                    Some(i),
                ));
            }
        }
    }
    v
}

fn all_pinned(mut s: StillCase, mut e: ExportCase) -> (StillCase, ExportCase) {
    s.family = s.family.replacen("r3s/", "r3p/", 1);
    e.family = e.family.replacen("r3s/", "r3p/", 1);
    s.anyway = true;
    e.anyway = true;
    (s, e)
}

fn summary(what: &str) {
    for class in ["Route", "Engine", "EngineCrop", "Staged", "Cropped", "Unknown"] {
        let (frames, drawn) = tally_of(class);
        eprintln!("R3 CLASS {what} {class:<10} frames with a copy {frames:>6}, of them drawn {drawn:>6}");
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r3_staged_every_shape_is_reached_cropped_is_refused_and_everything_drawn_is_exact() {
    let specs = shape_specs();
    let (mut still_cases, mut export_cases) = (Vec::new(), Vec::new());
    for (i, spec) in specs.iter().enumerate() {
        let (s, e) = spec_case(spec);
        if i % 3 == 0 {
            let (ps, pe) = all_pinned(s.clone(), e.clone());
            still_cases.push(ps);
            export_cases.push(pe);
        }
        if i % 3 == 1 {
            // ... and the same still rendered smaller than the delivery (the fit picture is made again for that size).
            let mut small = s.clone().at_width(128);
            small.family = small.family.replacen("r3s/", "r3w/", 1);
            still_cases.push(small);
        }
        still_cases.push(s);
        export_cases.push(e);
    }
    eprintln!(
        "R3 specs {} ({} cases with the pinned copies)",
        specs.len(),
        still_cases.len()
    );
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    summary("shapes");
    let (cropped_frames, cropped_drawn) = tally_of("Cropped");
    let (staged_frames, staged_drawn) = tally_of("Staged");
    finish("staged shapes", "r3", vec![a, b]);
    assert_eq!(tally_of("Unknown").0, 0, "a copy of a text of no shape");
    assert!(
        cropped_frames >= 200,
        "only {cropped_frames} frames with a Cropped text were reached"
    );
    assert_eq!(
        cropped_drawn, 0,
        "{cropped_drawn} frames with a crop besides the guard were let through"
    );
    assert!(
        staged_frames >= 200,
        "only {staged_frames} frames with a Staged text were reached"
    );
    let (_, e0) = (0, tally_of("Engine"));
    assert!(e0.0 > 0 && tally_of("EngineCrop").0 > 0);
    // Staged is drawn wherever the build lets an rgb24 picture through (and a JPEG where the range is not negotiated).
    assert!(staged_drawn > 0, "no Staged frame was drawn at all");
}

// ---- a seeded sweep weighted to the staged shapes --------------------------------------------------------------------------

fn sweep_case(rng: &mut Lcg, n: usize) -> (StillCase, ExportCase) {
    let kind = if rng.int(0, 9) < 6 { "png" } else { "jpeg" };
    let canvas = CANVASES[rng.int(0, CANVASES.len() as u32 - 1) as usize];
    let k = rng.int(1, 4);
    let picture = match rng.int(0, 9) {
        0 => (canvas.0 * k, canvas.1 * k),
        1 => (canvas.0 * k + 1, canvas.1 * k),
        2 => (canvas.0 * k, canvas.1 * k + 1),
        3 => (canvas.0 * k - 1, canvas.1 * k),
        4 => (canvas.0 * k + 2, canvas.1 * k + 2),
        5 => (canvas.0 * k, canvas.1 * k),
        _ => [
            (1200, 800),
            (801, 601),
            (160, 90),
            (90, 160),
            (641, 479),
            (1920, 1080),
            (3000, 400),
            (400, 3000),
            (64, 64),
            (321, 181),
        ][rng.int(0, 9) as usize],
    };
    let photo = pic(kind, picture.0, picture.1);
    let fit = if rng.int(0, 9) < 5 { Fit::Contain } else { Fit::Cover };
    let mut c = match rng.int(0, 5) {
        0 => moving(&photo, 0.0, 1.0, OFF_A),
        1 => moving(&photo, 0.0, 1.0, OUT_A),
        2 => moving(
            &photo,
            0.0,
            1.0,
            [HELD_08, HELD_05, HELD_10, HELD_125][rng.int(0, 3) as usize],
        ),
        3 => {
            let mut c = Clip::new(photo.id, 0.0, 1.0, 0.0);
            c.transform.scale = rng.range(0.5, 1.5);
            c.set_property_keys(
                kerf_core::Property::PosX,
                vec![
                    kerf_core::PropertyKey::new(0.0, -0.05),
                    kerf_core::PropertyKey::new(1.0, 0.05),
                ],
            );
            c
        }
        _ => {
            let z = rng.range(0.5, 1.4);
            moving(
                &photo,
                0.0,
                1.0,
                (
                    (z, (rng.range(-0.1, 0.0), 0.0)),
                    (z, (rng.range(0.0, 0.1), rng.range(-0.05, 0.05))),
                ),
            )
        }
    };
    let crop = rng.int(0, 3) == 0;
    if crop {
        c.transform.crop_left = rng.range(0.0, 0.3);
        c.transform.crop_top = rng.range(0.0, 0.3);
        c.transform.crop_right = rng.range(0.0, 0.2);
        c.transform.crop_bottom = rng.range(0.0, 0.2);
    }
    let scaler = if rng.int(0, 2) == 0 { Some("bicubic") } else { None };
    let fps = [10.0, 10.0, 24.0, 30.0][rng.int(0, 3) as usize];
    let tl = timeline(vec![vec![c]], delivery(canvas.0, canvas.1, fit, fps));
    let family = format!("r3x/{kind}");
    let name = format!(
        "sweep {n}: {kind} {}x{} on {}x{} {fit:?} crop {crop} {scaler:?} @{fps}",
        picture.0, picture.1, canvas.0, canvas.1
    );
    let assets = std::slice::from_ref(&photo);
    let mut s = stills(&family, &name, tl.clone(), assets, &[0.0, 0.43, 0.99]).scaler(scaler);
    s.opts.fps = Some(fps);
    let e = exports(&family, &name, tl, assets, fps, 1.0).scaler(scaler);
    (s, e)
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r3_sweep_whatever_the_plan_lets_through_has_a_text_a_toy_cut_ran_and_is_exact() {
    let mut rng = Lcg(0x3a7e_55ed);
    let (mut still_cases, mut export_cases) = (Vec::new(), Vec::new());
    for n in 0..90 {
        let (s, e) = sweep_case(&mut rng, n);
        still_cases.push(s);
        export_cases.push(e);
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    summary("sweep");
    let (cropped_frames, cropped_drawn) = tally_of("Cropped");
    finish("sweep", "r3x/", vec![a, b]);
    assert_eq!(tally_of("Unknown").0, 0, "a copy of a text of no shape");
    assert_eq!(
        cropped_drawn, 0,
        "{cropped_drawn} of {cropped_frames} frames with a crop besides the guard were let through"
    );
}

// ---- the Cropped frames drawn anyway (a scratch patch of `src/` lifts the refusal) -------------------------------------------------

/// The refusal of a `Cropped` text, lifted by a scratch patch of `RenderPlan::reasons` and `prepare_stills`: unpatched, every frame of this is refused
/// and nothing is judged. The verdict is the patched run's: how many of the frames the plan refuses for it would have been exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); the plan-level refusal needs the scratch patch"]
fn r3_cropped_drawn_anyway() {
    AUDIT.store(false, std::sync::atomic::Ordering::Relaxed);
    let mut cases_s = Vec::new();
    let mut cases_e = Vec::new();
    for spec in shape_specs().iter().filter(|s| {
        matches!(
            s.cause,
            "contain-crop" | "held-overhang" | "held-crop" | "held-off-by-a-pixel" | "static-zoom-wide"
        )
    }) {
        let (mut s, mut e) = spec_case(spec);
        s.family = s.family.replacen("r3s/", "r3a/", 1);
        e.family = e.family.replacen("r3s/", "r3a/", 1);
        s.anyway = true;
        e.anyway = true;
        cases_s.push(s);
        cases_e.push(e);
    }
    let a = judge_stills_report(&cases_s);
    let b = judge_exports_report(&cases_e);
    let mut total = (0usize, 0usize, 0usize);
    for (what, rows) in [("stills", a), ("exports", b)] {
        for (family, (exact, off, refused)) in rows {
            eprintln!("R3 ANYWAY {what} {family}: exact {exact} off {off} refused {refused}");
            total = (total.0 + exact, total.1 + off, total.2 + refused);
        }
    }
    eprintln!("R3 ANYWAY TOTAL: exact {} off {} refused {}", total.0, total.1, total.2);
    summary("anyway");
}

fn judge_stills_report(cases: &[StillCase]) -> BTreeMap<String, (usize, usize, usize)> {
    let mut out = BTreeMap::new();
    for c in cases {
        for &t in &c.times {
            let (o, _) = still_frame(c, t);
            let e = out.entry(c.family.clone()).or_insert((0, 0, 0));
            match o {
                Out::Exact => e.0 += 1,
                Out::Off(w) => {
                    e.1 += 1;
                    eprintln!(
                        "R3 ANYWAY off {} / {} @ {t}: {}",
                        c.family,
                        c.name,
                        w.chars().take(200).collect::<String>()
                    );
                }
                Out::Failed(w) => {
                    e.1 += 1;
                    eprintln!(
                        "R3 ANYWAY failed {} / {} @ {t}: {}",
                        c.family,
                        c.name,
                        w.chars().take(200).collect::<String>()
                    );
                }
                Out::Refused(..) => e.2 += 1,
            }
        }
    }
    out
}

fn judge_exports_report(cases: &[ExportCase]) -> BTreeMap<String, (usize, usize, usize)> {
    let mut out = BTreeMap::new();
    for c in cases {
        for (k, o, _) in export_frames(c) {
            let e = out.entry(c.family.clone()).or_insert((0, 0, 0));
            match o {
                Out::Exact => e.0 += 1,
                Out::Off(w) => {
                    e.1 += 1;
                    eprintln!(
                        "R3 ANYWAY off {} / {} frame {k}: {}",
                        c.family,
                        c.name,
                        w.chars().take(200).collect::<String>()
                    );
                }
                Out::Failed(w) => {
                    e.1 += 1;
                    eprintln!(
                        "R3 ANYWAY failed {} / {} frame {k}: {}",
                        c.family,
                        c.name,
                        w.chars().take(200).collect::<String>()
                    );
                }
                Out::Refused(..) => e.2 += 1,
            }
        }
    }
    out
}

// ---- stacks: a staged moving still and a cropped one among other layers, inside a fade or a dissolve ---------------------------------

fn fc(f: &Asset, from: f64, to: f64, at: f64) -> Clip {
    Clip::new(f.id, from, to, at)
}

fn dissolving(mut c: Clip, kind: TransitionKind, duration: f64) -> Clip {
    c.transition_in = Some(Transition { kind, duration });
    c
}

/// The staged still (a Contain frame, no crop) and the cropped one (the same behind the clip's static crop) among other layers.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r3_stacks_a_cropped_layer_refuses_the_whole_frame_and_the_staged_one_beside_it_is_exact() {
    let f = clips();
    let (mut still_cases, mut export_cases) = (Vec::new(), Vec::new());
    for kind in ["png", "jpeg"] {
        let photo = pic(kind, 1200, 800);
        let other = pic(kind, 801, 601);
        let plain = |a: &Asset| moving(a, 0.0, 1.0, OFF_A);
        let cropped = |a: &Asset| {
            let mut c = moving(a, 0.0, 1.0, OFF_B);
            c.transform.crop_left = 0.1;
            c.transform.crop_top = 0.05;
            c
        };
        let faded = |mut c: Clip| {
            c.fade_in = 0.3;
            c
        };
        let tracks: Vec<(&str, Vec<Vec<Clip>>, Vec<Asset>)> = vec![
            (
                "staged over footage",
                vec![vec![fc(&f.bt709, 0.0, 1.0, 0.0)], vec![plain(&photo)]],
                vec![photo.clone(), f.bt709.clone()],
            ),
            (
                "cropped over footage",
                vec![vec![fc(&f.bt709, 0.0, 1.0, 0.0)], vec![cropped(&photo)]],
                vec![photo.clone(), f.bt709.clone()],
            ),
            (
                "cropped under footage",
                vec![vec![cropped(&photo)], vec![fc(&f.untagged, 0.0, 1.0, 0.0)]],
                vec![photo.clone(), f.untagged.clone()],
            ),
            (
                "staged over a cropped one",
                vec![vec![cropped(&other)], vec![plain(&photo)]],
                vec![photo.clone(), other.clone()],
            ),
            (
                "a cropped one over a staged",
                vec![vec![plain(&photo)], vec![cropped(&other)]],
                vec![photo.clone(), other.clone()],
            ),
            (
                "staged then a cropped one, dissolving",
                vec![vec![
                    plain(&photo),
                    dissolving(cropped(&other), TransitionKind::Crossfade, 0.3),
                ]],
                vec![photo.clone(), other.clone()],
            ),
            (
                "a cropped one then a staged, dissolving",
                vec![vec![
                    cropped(&photo),
                    dissolving(plain(&other), TransitionKind::Crossfade, 0.3),
                ]],
                vec![photo.clone(), other.clone()],
            ),
            ("staged in a fade", vec![vec![faded(plain(&photo))]], vec![photo.clone()]),
            ("cropped in a fade", vec![vec![faded(cropped(&photo))]], vec![photo.clone()]),
            (
                "staged then staged, dissolving",
                vec![vec![plain(&photo), dissolving(plain(&other), TransitionKind::Crossfade, 0.3)]],
                vec![photo.clone(), other.clone()],
            ),
            (
                "staged sliding in",
                vec![
                    vec![fc(&f.untagged, 0.0, 1.0, 0.0)],
                    vec![dissolving(plain(&photo), TransitionKind::SlideLeft, 0.3)],
                ],
                vec![photo.clone(), f.untagged.clone()],
            ),
            (
                "staged dipping to black",
                vec![vec![dissolving(plain(&photo), TransitionKind::DipToBlack, 0.3)]],
                vec![photo.clone()],
            ),
        ];
        for (name, tr, assets) in tracks {
            // Contain makes the plain move a staged text; the cropped one is the same behind a static crop.
            for fit in [Fit::Contain, Fit::Cover] {
                let tl = timeline(tr.clone(), delivery(320, 180, fit, 10.0));
                let family = format!("r3m/{kind}/{fit:?}");
                let label = format!("{name} {fit:?}");
                still_cases.push(stills(&family, &label, tl.clone(), &assets, &[0.0, 0.1, 0.25, 0.5, 0.99]));
                export_cases.push(exports(&family, &label, tl, &assets, 10.0, 1.0));
            }
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    summary("stacks");
    let (cropped_frames, cropped_drawn) = tally_of("Cropped");
    finish("stacks", "r3m/", vec![a, b]);
    assert_eq!(tally_of("Unknown").0, 0, "a copy of a text of no shape");
    assert!(
        cropped_frames >= 50,
        "only {cropped_frames} frames with a Cropped text were reached"
    );
    assert_eq!(cropped_drawn, 0, "a frame with a crop besides the guard was let through");
}
