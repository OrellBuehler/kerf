//! Work package T6, fix round 2, checked independently: what the second round of the moving-still work claims (the planner's `placed`
//! rule and the matrix of the cut, the route layers' blend and keyed-colour refusals, the chains the still probe measures, what `prepare`
//! measures and makes, the draw-time refusals moved into the plan), attacked from outside on both FFmpeg builds.
//!
//! The comparison is the project's exact one with no bound: a Still frame is FFmpeg's own still against the compositor's planes, an export
//! frame is the lossless export's planes against the compositor's. Every frame is `Exact`, `Off` (drawn and different: a failure),
//! `Refused` (the plan said `Unsupported`: FFmpeg draws it, not a failure unless the case demands the draw), `Refused("render")` (the plan said
//! yes and the render refused: a draw-time refusal the plan did not know, a failure in the families that demand it) or `Failed`.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_t6_round2 -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
//!
//! Families: `r2m` / `r2m2` (the matrix a moving still decides in the cut: the planner's `placed` rule), `r2k` / `r2b` (a keyed colour and a
//! blend mode on a moving still, on the route and on the placement), `r2c` (every chain of both kinds, the scaler named or not, whole frames:
//! each case audits the text of its copy against the chain the plan says and that the chain was measured), `r2x` (a seeded sweep and the
//! extremes of a move), `r2w` (copies over 8192 a side), `r2p` (the still probe's own toy cuts, whole frames, where the probe skips the partial
//! alpha at the edge), `r2s` (the scaler's spellings). The `*_drawn_anyway` case needs a scratch patch of `src/` that reads `KERF_MUT` (a comma
//! list of the refusals to lift: `blend_route`, `eq_route`, `eq_fit`, `copy_size`): unpatched, every frame it draws is refused and nothing is judged.

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
            "R2 FAMILY {name:<48} drawn {:>4}  exact {:>4}  inexact {:>3}  refused {:>4}  failed {:>2}  [{}]{}",
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
        "R2 TOTAL {prefix}: drawn {}  exact {exact}  inexact {off}  refused {refused}  failed {failed}",
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("r2-t6-media");
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
            eprintln!("R2 still  {:<28} {:<60} t={t:<6} {tag}", c.family, c.name);
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
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("r2-t6-ref");
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
        if !c.anyway {
            kerf_gpu::prepare(&plan);
        }
        let why = reasons_of(&plan, comp, size);
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
            "R2 export {:<28} {:<60} {} frames: exact {exact}, refused {refused}, off/failed {}{}{}",
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
        "R2 {what}: drawn {drawn}, exact {exact}, inexact {off}, refused {refused}, failed {failed}, failing cases {}",
        fails.len()
    );
    for f in &fails {
        eprintln!("R2 FAIL {f}");
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

fn dissolving(mut c: Clip, kind: TransitionKind, duration: f64) -> Clip {
    c.transition_in = Some(Transition { kind, duration });
    c
}

// ---- helpers of this round ------------------------------------------------------------------------------------

/// A limited-range 4:2:0 picture that is a still (a JPEG 2000 file: the kind a WebP is for the compositor).
fn limited_420(w: u32, h: u32) -> Asset {
    let photo = t6_files::photo(
        &format!("limited-{w}x{h}.jp2"),
        &t6_files::grain(w, h),
        &["-c:v", "jpeg2000", "-pix_fmt", "yuv420p"],
    );
    let stream = photo.streams.iter().find(|s| s.kind == StreamKind::Video).expect("a picture");
    assert!(stream.image, "{:?} is not probed as a still image", photo.path);
    photo
}

/// A span of footage `[from, to)` put on the timeline at `at`.
fn fc(f: &Asset, from: f64, to: f64, at: f64) -> Clip {
    Clip::new(f.id, from, to, at)
}

fn with_color(mut c: Clip, brightness: f64, saturation: f64) -> Clip {
    c.color = Color {
        brightness,
        saturation,
        ..Color::default()
    };
    c
}

/// Brightness keyed from `a` to `b` over `secs`.
fn keyed_brightness(mut c: Clip, a: f64, b: f64, secs: f64) -> Clip {
    c.set_property_keys(
        kerf_core::Property::Brightness,
        vec![kerf_core::PropertyKey::new(0.0, a), kerf_core::PropertyKey::new(secs, b)],
    );
    c
}

fn kind_of(plan: &RenderPlan, size: (u32, u32)) -> String {
    plan.layers
        .iter()
        .map(|l| match plan.still_chain(l, size) {
            Some(c) => format!("{}:{:?}", l.stream.pix_fmt.as_deref().unwrap_or("?"), c),
            None => "-".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

// ---- family M: the matrix a moving still decides (the planner's `placed` rule) ---------------------------------------

type Arrange = fn(&Asset, &Asset) -> Timeline;

fn matrix_arrangements() -> Vec<(&'static str, Arrange, bool)> {
    vec![
        (
            "over",
            |f, p| {
                timeline(
                    vec![vec![fc(f, 0.0, 3.0, 0.0)], vec![moving(p, 1.0, 1.0, IN_A)]],
                    cover(320, 180),
                )
            },
            true,
        ),
        (
            "under",
            |f, p| {
                timeline(
                    vec![vec![moving(p, 0.0, 3.0, IN_A)], vec![fc(f, 1.0, 2.0, 1.0)]],
                    cover(320, 180),
                )
            },
            true,
        ),
        (
            "sequence",
            |f, p| {
                timeline(
                    vec![vec![fc(f, 0.0, 1.0, 0.0), moving(p, 1.0, 1.0, IN_A), fc(f, 2.0, 3.0, 2.0)]],
                    cover(320, 180),
                )
            },
            true,
        ),
        (
            "dissolve in",
            |f, p| {
                timeline(
                    vec![vec![
                        fc(f, 0.0, 1.5, 0.0),
                        dissolving(moving(p, 1.0, 1.5, IN_A), TransitionKind::Crossfade, 0.5),
                    ]],
                    cover(320, 180),
                )
            },
            true,
        ),
        (
            "dissolve out",
            |f, p| {
                timeline(
                    vec![vec![
                        moving(p, 0.0, 1.5, IN_A),
                        dissolving(fc(f, 0.0, 2.0, 1.0), TransitionKind::Crossfade, 0.5),
                    ]],
                    cover(320, 180),
                )
            },
            true,
        ),
        (
            "fades over",
            |f, p| {
                let mut s = moving(p, 0.5, 2.0, IN_A);
                s.fade_in = 0.4;
                s.fade_out = 0.4;
                timeline(vec![vec![fc(f, 0.0, 3.0, 0.0)], vec![s]], cover(320, 180))
            },
            true,
        ),
        (
            "route and placed",
            |f, p| {
                timeline(
                    vec![
                        vec![fc(f, 0.0, 3.0, 0.0)],
                        vec![moving(p, 0.0, 3.0, IN_A)],
                        vec![moving(p, 1.0, 1.0, OFF_A)],
                    ],
                    cover(320, 180),
                )
            },
            true,
        ),
        (
            "placed only",
            |f, p| {
                timeline(
                    vec![vec![fc(f, 0.0, 3.0, 0.0)], vec![moving(p, 1.0, 1.0, OFF_A)]],
                    cover(320, 180),
                )
            },
            true,
        ),
        (
            "colour",
            |f, p| {
                timeline(
                    vec![
                        vec![fc(f, 0.0, 3.0, 0.0)],
                        vec![with_color(moving(p, 1.0, 1.0, IN_A), 0.1, 1.2)],
                    ],
                    cover(320, 180),
                )
            },
            false,
        ),
        (
            "keyed colour",
            |f, p| {
                timeline(
                    vec![
                        vec![fc(f, 0.0, 3.0, 0.0)],
                        vec![keyed_brightness(moving(p, 1.0, 1.0, IN_A), -0.3, 0.3, 1.0)],
                    ],
                    cover(320, 180),
                )
            },
            false,
        ),
        (
            "opacity",
            |f, p| {
                let mut s = moving(p, 1.0, 1.0, IN_A);
                s.transform.opacity = 0.5;
                timeline(vec![vec![fc(f, 0.0, 3.0, 0.0)], vec![s]], cover(320, 180))
            },
            false,
        ),
        (
            "slide in",
            |f, p| {
                timeline(
                    vec![vec![
                        fc(f, 0.0, 1.5, 0.0),
                        dissolving(moving(p, 1.0, 1.5, IN_A), TransitionKind::SlideLeft, 0.5),
                    ]],
                    cover(320, 180),
                )
            },
            false,
        ),
        (
            "dip black",
            |f, p| {
                timeline(
                    vec![vec![
                        fc(f, 0.0, 1.5, 0.0),
                        dissolving(moving(p, 1.5, 1.5, IN_A), TransitionKind::DipToBlack, 0.5),
                    ]],
                    cover(320, 180),
                )
            },
            false,
        ),
        (
            "blend",
            |f, p| {
                let mut s = moving(p, 1.0, 1.0, IN_A);
                s.blend_mode = kerf_core::BlendMode::Multiply;
                timeline(vec![vec![fc(f, 0.0, 3.0, 0.0)], vec![s]], cover(320, 180))
            },
            false,
        ),
        (
            "two routes",
            |f, p| {
                timeline(
                    vec![
                        vec![fc(f, 0.0, 3.0, 0.0)],
                        vec![moving(p, 0.0, 2.0, IN_A)],
                        vec![moving(p, 1.0, 2.0, IN_B)],
                    ],
                    cover(320, 180),
                )
            },
            false,
        ),
    ]
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_matrix_a_moving_still_on_the_route_never_decides_the_matrix_and_a_placed_one_does() {
    let f = clips();
    let photos: Vec<(&str, Asset)> = vec![
        ("png", png(1200, 800)),
        ("limited420", limited_420(960, 540)),
        ("jpeg", jpeg(1200, 800)),
    ];
    let footage: Vec<(&str, &Asset)> = vec![
        ("709", &f.bt709),
        ("2020", &f.bt2020),
        ("untagged", &f.untagged),
        ("601", &f.bt601),
    ];
    let mut cases = Vec::new();
    let mut still_cases = Vec::new();
    for (pname, photo) in &photos {
        for (aname, arrange, all_kinds) in matrix_arrangements() {
            if !all_kinds && *pname != "png" {
                continue;
            }
            for (fname, foot) in &footage {
                let assets = vec![photo.clone(), (*foot).clone()];
                let tl = arrange(foot, photo);
                let family = format!("r2m/{pname}/{aname}/{fname}");
                still_cases.push(stills(&family, &family, tl.clone(), &assets, &[0.5, 1.2, 1.7]));
                let secs = tl.duration();
                cases.push(exports(&family, &family, tl, &assets, 10.0, secs));
            }
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&cases);
    finish("matrix decision", "r2m/", vec![a, b]);
}

// ---- the text of the copy, classified independently of the plan -------------------------------------------------------

/// The top-level filters of a filter chain (a comma inside single quotes is the filter's own).
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

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn scale_flags_of(args: &[&str], from: usize) -> Result<bool, String> {
    match &args[from..] {
        [] => Ok(false),
        ["flags=bicubic"] => Ok(true),
        other => Err(format!("scale flags {other:?}")),
    }
}

/// The chain the text of a copy is, read by hand: `(None` for the route's own copy, `Some(shape)` for a fit picture`, bicubic named)`.
fn text_chain(filters: &str) -> Result<(Option<kerf_core::FitShape>, bool), String> {
    use kerf_core::FitShape;
    let mut parts = split_filters(filters);
    if parts.pop().as_deref() != Some("format=yuva420p") {
        return Err(format!("no closing format=yuva420p: {filters}"));
    }
    let engine_scale = |p: &str| -> Result<(String, bool), String> {
        let a: Vec<&str> = p.strip_prefix("scale=").ok_or("not a scale")?.split(':').collect();
        if a.len() < 3 || !all_digits(a[0]) || !all_digits(a[1]) || a[2] != "force_original_aspect_ratio=increase" {
            return Err(format!("not the engine's scale: {p}"));
        }
        Ok((format!("{}:{}", a[0], a[1]), scale_flags_of(&a, 3)?))
    };
    let plain_crop = |p: &str| -> Result<Vec<String>, String> {
        let a: Vec<String> = p
            .strip_prefix("crop=")
            .ok_or("not a crop")?
            .split(':')
            .map(String::from)
            .collect();
        if a.iter().all(|x| all_digits(x)) {
            Ok(a)
        } else {
            Err(format!("not a crop of numbers: {p}"))
        }
    };
    match parts.as_slice() {
        [g, rest @ ..] if g.starts_with("crop=w='if(eq(iw,") && g.ends_with(":exact=1") => {
            let (mut scales, mut flagged) = (0, 0);
            for p in rest {
                if p.starts_with("scale=") {
                    let a: Vec<&str> = p.strip_prefix("scale=").unwrap().split(':').collect();
                    if a.len() < 2 || !all_digits(a[0]) || !all_digits(a[1]) {
                        return Err(format!("staged scale {p}"));
                    }
                    scales += 1;
                    flagged += usize::from(scale_flags_of(&a, 2)?);
                } else if plain_crop(p)?.len() != 4 {
                    return Err(format!("staged crop {p}"));
                }
            }
            if scales == 0 || (flagged != 0 && flagged != scales) {
                return Err(format!("staged scales {scales}, flagged {flagged}: {filters}"));
            }
            Ok((Some(FitShape::Staged), flagged != 0))
        }
        [s, c] if s.starts_with("scale=") && !s.contains("force_original") => {
            let a: Vec<&str> = s.strip_prefix("scale=").unwrap().split(':').collect();
            let crop = plain_crop(c)?;
            if a.len() != 3 || !all_digits(a[0]) || !all_digits(a[1]) || crop.len() != 4 {
                return Err(format!("not the route's text: {filters}"));
            }
            match a[2] {
                "flags=lanczos" => Ok((None, false)),
                "flags=bicubic" => Ok((None, true)),
                other => Err(format!("route scaler flags {other}")),
            }
        }
        [s, c] => {
            let (size, bicubic) = engine_scale(s)?;
            let crop = plain_crop(c)?;
            if crop.join(":") != size {
                return Err(format!("the engine's crop {crop:?} is not the scale's {size}"));
            }
            Ok((Some(FitShape::Engine), bicubic))
        }
        [k, s, c] if k.starts_with("crop=w=iw*") => {
            let a: Vec<&str> = k.strip_prefix("crop=").unwrap().split(':').collect();
            if a.len() != 4
                || !a[0].starts_with("w=iw*")
                || !a[1].starts_with("h=ih*")
                || !a[2].starts_with("x=iw*")
                || !a[3].starts_with("y=ih*")
            {
                return Err(format!("not the engine's static crop: {k}"));
            }
            for (x, prefix) in a.iter().zip(["w=iw*", "h=ih*", "x=iw*", "y=ih*"]) {
                x[prefix.len()..]
                    .parse::<f64>()
                    .map_err(|e| format!("crop number {x}: {e}"))?;
            }
            let (size, bicubic) = engine_scale(s)?;
            let crop = plain_crop(c)?;
            if crop.join(":") != size {
                return Err(format!("the engine's crop {crop:?} is not the scale's {size}"));
            }
            Ok((Some(FitShape::EngineCrop), bicubic))
        }
        _ => Err(format!("a text of no shape the probe knows: {filters}")),
    }
}

/// Whether the cases audit the text of the copy against the chain the plan says (off only where a case draws a plan it knows to disagree).
static AUDIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Every layer the plan draws a copy for has the text of the chain the plan says, and that chain is measured.
fn chain_audit(plan: &RenderPlan, size: (u32, u32), measured: bool) -> Result<(), String> {
    if !AUDIT.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    for layer in &plan.layers {
        let Ok(Some(copy)) = kerf_gpu::source::copy_of(layer) else {
            continue;
        };
        let read = text_chain(&copy.filters)?;
        let said = plan
            .still_chain(layer, size)
            .ok_or_else(|| format!("a layer with a copy and no chain: {}", copy.filters))?;
        if (said.fit, said.bicubic) != read {
            return Err(format!("the plan says {said:?}, the text is {read:?}: {}", copy.filters));
        }
        let kind = plan.still_kind(layer).ok_or("a copy for a layer of no measured kind")?;
        if measured && !kerf_gpu::still_probe::measured().contains_chain(kind, said) {
            return Err(format!("{kind:?} {said:?} is drawn and was not measured"));
        }
    }
    Ok(())
}

// ---- family K / B: a keyed colour and a blend mode on a moving still -----------------------------------------------------

/// Colour keyed through neutral, in every property the `eq` takes, and every blend mode, on the route and on the placement.
fn keyed_and_blended(anyway: bool) -> (Vec<StillCase>, Vec<ExportCase>) {
    let f = clips();
    let p = png(1200, 800);
    let j = jpeg(1200, 800);
    let (mut stills_, mut exports_) = (Vec::new(), Vec::new());
    let keys: [(&str, kerf_core::Property, f64, f64); 7] = [
        ("brightness", kerf_core::Property::Brightness, -0.3, 0.3),
        ("contrast", kerf_core::Property::Contrast, 0.7, 1.3),
        ("saturation", kerf_core::Property::Saturation, 0.5, 1.5),
        ("gamma", kerf_core::Property::Gamma, 0.8, 1.2),
        ("temperature", kerf_core::Property::Temperature, -0.3, 0.3),
        ("brightness constant", kerf_core::Property::Brightness, 0.2, 0.2),
        ("brightness neutral", kerf_core::Property::Brightness, 0.0, 0.0),
    ];
    for (kname, photo) in [("png", &p), ("jpeg", &j)] {
        for (route, mv) in [("route", IN_A), ("placed", OFF_A), ("route b", IN_B), ("placed b", OFF_B)] {
            for (name, prop, a, b) in keys {
                // alone, and over footage
                for (over, base) in [
                    ("alone", None),
                    ("over 709", Some(&f.bt709)),
                    ("over untagged", Some(&f.untagged)),
                ] {
                    let mut c = moving(photo, 0.0, 1.0, mv);
                    c.set_property_keys(
                        prop,
                        vec![kerf_core::PropertyKey::new(0.0, a), kerf_core::PropertyKey::new(1.0, b)],
                    );
                    let (tl, assets) = match base {
                        None => (timeline(vec![vec![c]], cover(320, 180)), vec![photo.clone()]),
                        Some(foot) => (
                            timeline(vec![vec![fc(foot, 0.0, 1.0, 0.0)], vec![c]], cover(320, 180)),
                            vec![photo.clone(), foot.clone()],
                        ),
                    };
                    let family = format!("r2k/{kname}/{route}/{name}/{over}");
                    let mut s = stills(&family, &family, tl.clone(), &assets, &[0.0, 0.25, 0.5, 0.75, 0.99]);
                    s.anyway = anyway;
                    let mut e = exports(&family, &family, tl, &assets, 10.0, 1.0);
                    e.anyway = anyway;
                    stills_.push(s);
                    exports_.push(e);
                }
            }
            for mode in kerf_core::BlendMode::ALL.into_iter().filter(|m| !m.is_normal()) {
                if kname == "jpeg" && mode != kerf_core::BlendMode::Multiply {
                    continue;
                }
                for foot in [&f.bt709, &f.untagged] {
                    let mut c = moving(photo, 0.0, 1.0, mv);
                    c.blend_mode = mode;
                    let tl = timeline(vec![vec![fc(foot, 0.0, 1.0, 0.0)], vec![c]], cover(320, 180));
                    let assets = vec![photo.clone(), foot.clone()];
                    let family = format!(
                        "r2b/{kname}/{route}/{mode:?}/{}",
                        if foot.id == f.bt709.id { "709" } else { "untagged" }
                    );
                    let mut s = stills(&family, &family, tl.clone(), &assets, &[0.0, 0.5, 0.99]);
                    s.anyway = anyway;
                    let mut e = exports(&family, &family, tl, &assets, 10.0, 1.0);
                    e.anyway = anyway;
                    stills_.push(s);
                    exports_.push(e);
                }
            }
        }
    }
    (stills_, exports_)
}

/// Whether every frame of the case was refused (and none drawn).
fn all_refused(c: &ExportCase) -> Result<(), String> {
    // (Planned only: the plan of every frame is asked with the compositor's own caps, as the export would be judged; nothing is rendered.)
    let planner = Planner::new(
        &c.tl,
        &c.told,
        &c.opts,
        PlanRequest::motion(policy()).with_media(&OriginalMedia),
    )
    .map_err(|e| format!("planner: {e}"))?;
    let comp = if c.anyway { compositor_all() } else { compositor() };
    let mut drawn = Vec::new();
    for k in c.frames.clone() {
        let plan = planner.at_frame(k).map_err(|e| format!("plan: {e}"))?;
        let size = plan.size(u32::MAX);
        if !c.anyway {
            kerf_gpu::prepare(&plan);
        }
        if reasons_of(&plan, comp, size).is_empty() {
            drawn.push(k);
        }
    }
    if drawn.is_empty() {
        Ok(())
    } else {
        Err(format!("{} / {}: frames {drawn:?} were not refused", c.family, c.name))
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_keyed_colour_and_every_blend_on_a_moving_still_are_refused_in_every_export_frame_and_the_stills_that_are_drawn_are_exact() {
    let (stills_, exports_) = keyed_and_blended(false);
    let mut fails = Vec::new();
    for c in &exports_ {
        // A keyed colour on a still: the engine's `eq` is in the chain of every frame of the clip.
        if let Err(e) = all_refused(c) {
            fails.push(e);
        }
    }
    let a = judge_stills(&stills_);
    finish("keyed colour and blends", "r2k/", vec![a]);
    assert!(fails.is_empty(), "{} export cases drew a frame: {fails:#?}", fails.len());
}

/// The same cuts with every refusal of the plan lifted by the scratch patch (`KERF_MUT=skip_blend_route,skip_eq_route,skip_eq_fit`): the verdict is
/// the patched run's. Unpatched, every frame is refused and nothing is judged.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); the plan-level refusals need the scratch patch"]
fn r2_keyed_colour_and_blends_drawn_anyway() {
    let (stills_, exports_) = keyed_and_blended(true);
    let a = judge_stills_report(&stills_);
    let b = judge_exports_report(&exports_);
    for (what, rows) in [("stills", a), ("exports", b)] {
        for (family, (exact, off, refused)) in rows {
            eprintln!("R2 ANYWAY {what} {family}: exact {exact} off {off} refused {refused}");
        }
    }
}

fn judge_stills_report(cases: &[StillCase]) -> BTreeMap<String, (usize, usize, usize)> {
    let mut out = BTreeMap::new();
    for c in cases {
        for &t in &c.times {
            let (o, _) = still_frame(c, t);
            let e = out.entry(c.family.clone()).or_insert((0, 0, 0));
            match o {
                Out::Exact => e.0 += 1,
                Out::Off(_) | Out::Failed(_) => e.1 += 1,
                Out::Refused(..) => e.2 += 1,
            }
        }
    }
    out
}

fn judge_exports_report(cases: &[ExportCase]) -> BTreeMap<String, (usize, usize, usize)> {
    let mut out = BTreeMap::new();
    for c in cases {
        for (_, o, _) in export_frames(c) {
            let e = out.entry(c.family.clone()).or_insert((0, 0, 0));
            match o {
                Out::Exact => e.0 += 1,
                Out::Off(w) | Out::Failed(w) => {
                    e.1 += 1;
                    eprintln!("R2 ANYWAY off {}: {}", c.family, w.chars().take(200).collect::<String>());
                }
                Out::Refused(..) => e.2 += 1,
            }
        }
    }
    out
}

// ---- family C: every chain, every kind, the scaler named or not, whole frames --------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Shape {
    /// The move inside the frame (the route).
    Inside,
    /// Panned off the picture's edge under Cover: the engine's own fit.
    Engine,
    /// The same behind a static crop (the index picks the crop).
    Crop(usize),
    /// A Contain frame, panned off: the geometry's stages.
    Contain,
    /// A held zoom out of a picture of the frame's shape under Cover: a second stage.
    Held,
    /// Contain behind a static crop, and a held zoom behind one.
    ContainCrop(usize),
    HeldCrop(usize),
}

const CROPS: [(f64, f64, f64, f64); 5] = [
    (0.1, 0.05, 0.0, 0.0),
    (0.0, 0.0, 0.2, 0.13),
    (0.07, 0.11, 0.03, 0.09),
    (0.3, 0.0, 0.0, 0.3),
    (0.013, 0.007, 0.011, 0.017),
];

struct ChainSpec {
    kind: &'static str,
    shape: Shape,
    scaler: Option<&'static str>,
    pic: (u32, u32),
    canvas: (u32, u32),
    mv: usize,
    fps: f64,
}

fn chain_label(plan: &RenderPlan, size: (u32, u32)) -> String {
    kind_of(plan, size)
}

fn chain_case(spec: &ChainSpec) -> (StillCase, ExportCase, String) {
    let photo = pic(spec.kind, spec.pic.0, spec.pic.1);
    let (cw, ch) = spec.canvas;
    let (mv, fit): ((Mv, Mv), Fit) = match spec.shape {
        Shape::Inside => ([IN_A, IN_B][spec.mv % 2], Fit::Cover),
        Shape::Engine | Shape::Crop(_) => ([OFF_A, OFF_B][spec.mv % 2], Fit::Cover),
        Shape::Contain | Shape::ContainCrop(_) => ([OFF_A, OFF_B, OUT_A][spec.mv % 3], Fit::Contain),
        Shape::Held | Shape::HeldCrop(_) => (((0.8, (-0.04, 0.0)), (0.8, (0.04, -0.02))), Fit::Cover),
    };
    let mut c = moving(&photo, 0.0, 1.0, mv);
    if let Shape::Crop(i) | Shape::ContainCrop(i) | Shape::HeldCrop(i) = spec.shape {
        let (l, t, r, b) = CROPS[i % CROPS.len()];
        c.transform.crop_left = l;
        c.transform.crop_top = t;
        c.transform.crop_right = r;
        c.transform.crop_bottom = b;
    }
    let tl = timeline(vec![vec![c]], delivery(cw, ch, fit, spec.fps));
    let mut opts = at_fps(spec.fps);
    opts.scaler = spec.scaler.map(String::from);
    let planner = Planner::new(
        &tl,
        std::slice::from_ref(&photo),
        &opts,
        PlanRequest::motion(policy()).with_media(&OriginalMedia),
    )
    .expect("a planner");
    let plan = planner.at_frame(3).expect("a plan");
    let label = chain_label(&plan, (cw, ch));
    let family = format!("r2c/{label}/{}", spec.scaler.unwrap_or("default"));
    let name = format!(
        "{:?} {} {}x{} on {cw}x{ch} mv{} @{}",
        spec.shape, spec.kind, spec.pic.0, spec.pic.1, spec.mv, spec.fps
    );
    let assets = std::slice::from_ref(&photo);
    let mut s = stills(&family, &name, tl.clone(), assets, &[0.0, 0.31, 0.5, 0.99]).scaler(spec.scaler);
    s.opts.fps = Some(spec.fps);
    let e = exports(&family, &name, tl, assets, spec.fps, 1.0).scaler(spec.scaler);
    (s, e, label)
}

fn chain_specs() -> Vec<ChainSpec> {
    let mut v = Vec::new();
    let scalers = [None, Some("bicubic")];
    let pics: [(u32, u32); 6] = [(1200, 800), (801, 601), (160, 90), (90, 160), (641, 479), (1920, 1080)];
    let canvases: [(u32, u32); 6] = [(320, 180), (426, 240), (322, 182), (180, 320), (256, 256), (640, 360)];
    let mut n = 0usize;
    for kind in ["png", "jpeg"] {
        for scaler in scalers {
            for shape in [
                Shape::Inside,
                Shape::Engine,
                Shape::Crop(0),
                Shape::Crop(1),
                Shape::Crop(2),
                Shape::Crop(3),
                Shape::Crop(4),
                Shape::Contain,
                Shape::Held,
                Shape::ContainCrop(0),
                Shape::ContainCrop(2),
                Shape::HeldCrop(1),
                Shape::HeldCrop(3),
            ] {
                for variant in 0..3usize {
                    n += 1;
                    let canvas = canvases[(n + variant) % canvases.len()];
                    // A held zoom out is a second stage only for a picture of the frame's own shape.
                    let pic = if matches!(shape, Shape::Held | Shape::HeldCrop(_)) {
                        let k = [1, 2, 3][variant];
                        (canvas.0 * k, canvas.1 * k)
                    } else {
                        pics[(n * 5 + variant) % pics.len()]
                    };
                    v.push(ChainSpec {
                        kind,
                        shape,
                        scaler,
                        pic,
                        canvas,
                        mv: n + variant,
                        fps: [10.0, 10.0, 25.0][variant],
                    });
                }
            }
        }
    }
    v
}

/// The exact export frames of a family whose label contains `part` (a kind and a chain).
fn drawn_in(part: &str) -> usize {
    let all = tallies().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    all.iter().filter(|(n, _)| n.contains(part)).map(|(_, t)| t.exact).sum()
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_chains_every_kind_shape_and_scaler_is_drawn_exact_whole_frames_edges_included() {
    let specs = chain_specs();
    let (mut still_cases, mut export_cases) = (Vec::new(), Vec::new());
    for spec in &specs {
        let (s, e, _) = chain_case(spec);
        still_cases.push(s);
        export_cases.push(e);
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    // How many frames of each kind and chain were drawn and exact (the grid reaches every chain of both kinds).
    let chains = kerf_core::StillChain::ALL;
    let mut missing = Vec::new();
    for kind in ["rgb24", "yuvj420p"] {
        for chain in chains {
            let part = format!("{kind}:{chain:?}/");
            let n = drawn_in(&part);
            eprintln!("R2 CHAIN {kind:<9} {chain:?}: {n} exact frames");
            let can_draw = kind == "rgb24" || !negotiated();
            if can_draw && n == 0 {
                missing.push(format!("{kind} {chain:?}"));
            }
        }
    }
    finish("chains", "r2c/", vec![a, b]);
    assert!(missing.is_empty(), "no frame was drawn exact for {missing:?}");
}

// ---- family X: sweeps and extremes; whatever the plan accepts, the render takes and draws exactly ---------------------------

fn sweep_case(rng: &mut Lcg, n: usize) -> (StillCase, ExportCase) {
    let kind = if rng.int(0, 9) < 6 { "png" } else { "jpeg" };
    let canvases: [(u32, u32); 7] = [
        (320, 180),
        (426, 240),
        (322, 182),
        (180, 320),
        (256, 256),
        (640, 360),
        (160, 90),
    ];
    let pics: [(u32, u32); 9] = [
        (1200, 800),
        (801, 601),
        (160, 90),
        (90, 160),
        (641, 479),
        (1920, 1080),
        (3000, 400),
        (400, 3000),
        (64, 64),
    ];
    let canvas = canvases[rng.int(0, canvases.len() as u32 - 1) as usize];
    let picture = pics[rng.int(0, pics.len() as u32 - 1) as usize];
    let photo = pic(kind, picture.0, picture.1);
    let fit = if rng.int(0, 9) < 7 { Fit::Cover } else { Fit::Contain };
    let keys = rng.int(2, 3);
    let secs = 1.0;
    let mut c = Clip::new(photo.id, 0.0, secs, 0.0);
    // The extremes of a move: a zoom out to 0.2x, in to 8x, pans well off the frame.
    let mut ks = Vec::new();
    for i in 0..keys {
        let t = secs * f64::from(i) / f64::from(keys - 1);
        let scale = match rng.int(0, 5) {
            0 => rng.range(0.2, 0.9),
            1 => rng.range(4.0, 8.0),
            _ => rng.range(0.95, 2.2),
        };
        let pan = if rng.int(0, 4) == 0 { 0.6 } else { 0.08 };
        ks.push(key(t, scale, rng.range(-pan, pan), rng.range(-pan, pan)));
    }
    c.keyframes = ks;
    if rng.int(0, 3) == 0 {
        c.transform.crop_left = rng.range(0.0, 0.3);
        c.transform.crop_top = rng.range(0.0, 0.3);
        c.transform.crop_right = rng.range(0.0, 0.2);
        c.transform.crop_bottom = rng.range(0.0, 0.2);
    }
    let scaler = if rng.int(0, 2) == 0 { Some("bicubic") } else { None };
    let fps = [10.0, 10.0, 24.0, 30.0][rng.int(0, 3) as usize];
    let tl = timeline(vec![vec![c]], delivery(canvas.0, canvas.1, fit, fps));
    let family = format!("r2x/{kind}");
    let name = format!(
        "sweep {n}: {kind} {}x{} on {}x{} {fit:?} {scaler:?} @{fps}",
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
fn r2_sweep_whatever_the_plan_accepts_is_drawn_exact_and_never_refused_at_the_draw() {
    let mut rng = Lcg(0x7ee1_c0de);
    let (mut still_cases, mut export_cases) = (Vec::new(), Vec::new());
    for n in 0..56 {
        let (s, e) = sweep_case(&mut rng, n);
        still_cases.push(s);
        export_cases.push(e);
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("sweep", "r2x/", vec![a, b]);
}

// ---- family W: a copy the compositor cannot take (over 8192 a side), refused by the plan -----------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_wide_copies_are_refused_by_the_plan_and_never_by_the_draw() {
    let wide = png(2000, 500);
    let tall = png(500, 2000);
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    let near: Vec<(&str, &Asset, (u32, u32), (Mv, Mv))> = vec![
        // 2160 x 4.2 x 1.02 = 9253 wide: over the 8192 the compositor takes.
        ("wide 4.2x pan", &wide, (960, 540), ((4.2, (1.5, 0.0)), (4.2, (-1.5, 0.0)))),
        // ... and under it: 4.0 x 2160 x 1.02 = 8813; 3.5 x 2160 x 1.02 = 7711
        ("wide 3.5x pan", &wide, (960, 540), ((3.5, (1.2, 0.0)), (3.5, (-1.2, 0.0)))),
        ("tall 4.2x pan", &tall, (540, 960), ((4.2, (0.0, 1.5)), (4.2, (0.0, -1.5)))),
        ("tall 3.5x pan", &tall, (540, 960), ((3.5, (0.0, 1.2)), (3.5, (0.0, -1.2)))),
        ("wide 8x", &wide, (960, 540), ((8.0, (3.0, 0.0)), (8.0, (-3.0, 0.0)))),
        ("wide 3.7x", &wide, (960, 540), ((3.7, (1.3, 0.0)), (3.7, (-1.3, 0.0)))),
        ("wide 3.9x", &wide, (960, 540), ((3.9, (1.4, 0.0)), (3.9, (-1.4, 0.0)))),
        // The window any frame reads is 0.44 of the 2160 px fit a side (the frame over the picture), so it passes 8192 once the copy does 18600.
        ("wide 12x pan", &wide, (960, 540), ((12.0, (5.4, 0.0)), (12.0, (-5.4, 0.0)))),
        ("wide 9x pan", &wide, (960, 540), ((9.0, (4.0, 0.0)), (9.0, (-4.0, 0.0)))),
        ("wide 8.6x pan", &wide, (960, 540), ((8.6, (3.8, 0.0)), (8.6, (-3.8, 0.0)))),
        ("tall 12x pan", &tall, (540, 960), ((12.0, (0.0, 5.4)), (12.0, (0.0, -5.4)))),
        ("tall 9x pan", &tall, (540, 960), ((9.0, (0.0, 4.0)), (9.0, (0.0, -4.0)))),
    ];
    for (name, photo, canvas, mv) in near {
        let tl = timeline(vec![vec![moving(photo, 0.0, 1.0, mv)]], cover(canvas.0, canvas.1));
        {
            let planner = Planner::new(
                &tl,
                std::slice::from_ref(photo),
                &at_fps(10.0),
                PlanRequest::motion(policy()).with_media(&OriginalMedia),
            )
            .expect("a planner");
            let plan = planner.at_frame(5).expect("a plan");
            let window = plan.layers[0]
                .still
                .as_ref()
                .and_then(|s| s.export.as_ref())
                .map(|m| m.window);
            let why: Vec<String> = reasons_of(&plan, compositor_all(), canvas)
                .into_iter()
                .map(|(k, t)| format!("{k}: {}", t.chars().take(120).collect::<String>()))
                .collect();
            eprintln!(
                "R2 WIDE {name}: route {} window {window:?} still_chain {:?} reasons {why:?}",
                plan.takes_still_route(&plan.layers[0]),
                plan.still_chain(&plan.layers[0], canvas)
            );
        }
        let family = format!("r2w/{name}");
        still_cases.push(stills(
            &family,
            name,
            tl.clone(),
            std::slice::from_ref(photo),
            &[0.0, 0.5, 0.99],
        ));
        export_cases.push(exports(&family, name, tl, std::slice::from_ref(photo), 10.0, 1.0));
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("wide copies", "r2w/", vec![a, b]);
}

// ---- family P: the probe's own toy cuts, whole frames (the partial-alpha samples the probe skips included) ---------------

fn toy_noise(kind: &str, w: u32, h: u32) -> Asset {
    let busy = "(X*255/W+Y*255/H)/2+random(1)*70-35";
    let (graph, ext, args): (String, &str, Vec<&str>) = if kind == "jpeg" {
        (
            format!("nullsrc=size={w}x{h}:rate=1:duration=1,format=rgb24,geq=r='clip({busy},0,255)':g='clip(255-X*255/W+random(2)*60-30,0,255)':b='clip(Y*255/H+random(3)*90-45,0,255)'"),
            "jpg",
            vec!["-pix_fmt", "yuvj420p", "-q:v", "2"],
        )
    } else {
        (
            format!("nullsrc=size={w}x{h}:rate=1:duration=1,format=rgb24,geq=r='random(1)*255':g='clip({busy},0,255)':b='random(3)*255'"),
            "png",
            vec!["-pix_fmt", "rgb24"],
        )
    };
    t6_files::photo(&format!("toy-{kind}-{w}x{h}.{ext}"), &graph, &args)
}

/// The probe's toy cuts (`still_probe::cases` and `toy`): 64x36 at 30 fps, twelve frames, as the probe has each chain, drawn through the
/// compositor and held to FFmpeg's whole frames with no sample skipped.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_probe_toys_whole_frames_where_the_probe_skips_the_edge() {
    let shapes: [(u32, u32); 4] = [(96, 64), (64, 96), (40, 28), (128, 72)];
    let end = 12.0 / 30.0;
    let mut still_cases = Vec::new();
    let mut export_cases = Vec::new();
    for kind in ["png", "jpeg"] {
        for bicubic in [false, true] {
            let scaler = bicubic.then_some("bicubic");
            // (shape index, fit, held zoom, crop, route)
            let toys: [(usize, Fit, Option<f64>, bool, bool); 10] = [
                (0, Fit::Cover, None, false, true),
                (1, Fit::Cover, None, false, true),
                (0, Fit::Cover, None, false, false),
                (1, Fit::Cover, None, false, false),
                (2, Fit::Cover, None, false, false),
                (0, Fit::Cover, None, true, false),
                (1, Fit::Cover, None, true, false),
                (0, Fit::Contain, None, false, false),
                (1, Fit::Contain, None, false, false),
                (3, Fit::Cover, Some(0.5), false, false),
            ];
            for (i, (shape, fit, held, crop, route)) in toys.into_iter().enumerate() {
                let (w, h) = shapes[shape];
                let photo = toy_noise(kind, w, h);
                let mut c = Clip::new(photo.id, 0.0, end, 0.0);
                c.keyframes = match (route, held) {
                    (true, _) => vec![key(0.0, 1.3, 0.03, 0.02), key(end, 1.05, -0.01, 0.0)],
                    (false, Some(held)) => vec![key(0.0, held, -0.04, 0.0), key(end, held, 0.04, -0.02)],
                    (false, None) => vec![key(0.0, 1.0, -0.04, 0.0), key(end, 1.2, 0.04, -0.02)],
                };
                if crop {
                    c.transform.crop_left = 0.1;
                    c.transform.crop_top = 0.05;
                }
                let tl = timeline(vec![vec![c]], delivery(64, 36, fit, 30.0));
                let family = format!("r2p/{kind}/{}", scaler.unwrap_or("default"));
                let name = format!("toy {i}: {w}x{h} {fit:?} held {held:?} crop {crop} route {route}");
                let assets = std::slice::from_ref(&photo);
                let mut s = stills(&family, &name, tl.clone(), assets, &[0.0, end * 0.5, end * 0.95]).scaler(scaler);
                s.opts.fps = Some(30.0);
                still_cases.push(s);
                let mut e = exports(&family, &name, tl, assets, 30.0, end).scaler(scaler);
                e.frames = 0..12;
                export_cases.push(e);
            }
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("probe toys", "r2p/", vec![a, b]);
}

// ---- family M2: two kinds of footage and HD footage the graph labels, with a route still among them ----------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_matrix_two_matrices_and_labelled_hd_footage_with_a_route_still_among_them() {
    let f = clips();
    let photos: Vec<(&str, Asset)> = vec![
        ("png", png(1200, 800)),
        ("limited420", limited_420(960, 540)),
        ("jpeg", jpeg(1200, 800)),
    ];
    let mut export_cases = Vec::new();
    let mut still_cases = Vec::new();
    for (pname, photo) in &photos {
        let sets: Vec<(&str, Vec<&Asset>)> = vec![
            ("hd untagged", vec![&f.hd_untagged]),
            ("hd 709", vec![&f.hd_709]),
            ("hd untagged + 709", vec![&f.hd_untagged, &f.bt709]),
            ("709 + 2020", vec![&f.bt709, &f.bt2020]),
            ("untagged + 709", vec![&f.untagged, &f.bt709]),
            ("601 + hd untagged", vec![&f.bt601, &f.hd_untagged]),
        ];
        for (sname, set) in &sets {
            let a = set[0];
            let b = set.get(1).copied();
            let mut assets = vec![photo.clone()];
            assets.extend(set.iter().map(|x| (*x).clone()));
            // (1) a route still in the middle of three tracks
            let mut tracks = vec![vec![fc(a, 0.0, 3.0, 0.0)], vec![moving(photo, 1.0, 1.0, IN_A)]];
            if let Some(b) = b {
                tracks.push(vec![fc(b, 1.5, 2.5, 1.5)]);
            }
            // (2) the still first on its own track, footage after it on the same track and above
            let mut seq = vec![vec![moving(photo, 0.0, 1.0, IN_A), fc(a, 1.0, 2.0, 1.0)]];
            if let Some(b) = b {
                seq.push(vec![fc(b, 0.0, 3.0, 0.0)]);
            }
            // (3) a placed still among them
            let mut placed = vec![vec![fc(a, 0.0, 3.0, 0.0)], vec![moving(photo, 1.0, 1.0, OFF_A)]];
            if let Some(b) = b {
                placed.push(vec![fc(b, 1.5, 2.5, 1.5)]);
            }
            for (how, t) in [("middle", tracks), ("first", seq), ("placed", placed)] {
                let tl = timeline(t, cover(320, 180));
                let family = format!("r2m2/{pname}/{how}/{sname}");
                let secs = tl.duration();
                still_cases.push(stills(&family, &family, tl.clone(), &assets, &[0.5, 1.2, 1.7, 2.0]));
                export_cases.push(exports(&family, &family, tl, &assets, 10.0, secs));
            }
        }
    }
    let a = judge_stills(&still_cases);
    let b = judge_exports(&export_cases);
    finish("matrix decision 2", "r2m2/", vec![a, b]);
}

// ---- family S: the scaler named in other spellings, and the scalers the compositor does not implement --------------------------

fn scaler_stills(spellings: &[&str]) -> Vec<StillCase> {
    let p = png(1200, 800);
    let assets = std::slice::from_ref(&p);
    let mut out = Vec::new();
    for scaler in spellings {
        for (route, mv) in [("route", IN_A), ("placed", OFF_A)] {
            let tl = timeline(vec![vec![moving(&p, 0.0, 1.0, mv)]], cover(320, 180));
            let family = format!("r2s/{route}/{scaler:?}");
            out.push(stills(&family, &family, tl, assets, &[0.0, 0.5, 0.99]).scaler(Some(scaler)));
        }
    }
    out
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_scalers_other_than_bicubic_are_refused_in_every_still_and_bicubic_is_drawn_exact() {
    let refused = scaler_stills(&[
        "lanczos", "bilinear", "neighbor", "spline", "area", "", " bicubic", "bicubic ",
    ]);
    for c in &refused {
        for &t in &c.times {
            let (o, _) = still_frame(c, t);
            assert!(matches!(o, Out::Refused(..)), "{} @ {t}: {o:?}", c.family);
        }
    }
    let a = judge_stills(&scaler_stills(&["bicubic"]));
    finish("scalers", "r2s/", vec![a]);
}

/// The engine writes a scaler's flags only for a name in its own list, spelled as it is (`SCALERS`, `scale_flags`); the plan's
/// `scaler_named_bicubic` ignores the case. A Still plan told `Bicubic` is drawn from the chain of a named bicubic scaler (`StillChain::bicubic`)
/// while the text FFmpeg is given names no flags (the route's `scale` runs with Lanczos, the fit's with the default): the chain the caps are asked
/// for is not the chain the text is. Exact on both builds (the audit is what shows it), and unreachable from an export (it validates the name).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_repro_a_scaler_spelled_bicubic_in_capitals_is_asked_for_as_a_chain_it_is_not() {
    // The drawn frames are the still's, to the sample (the chain audit off: it is what this case shows).
    AUDIT.store(false, std::sync::atomic::Ordering::Relaxed);
    let a = judge_stills(&scaler_stills(&["Bicubic", "BICUBIC"]));
    AUDIT.store(true, std::sync::atomic::Ordering::Relaxed);
    finish("scalers spelled (drawn)", "r2s/", vec![a]);
    // ... and the chain the plan asks the caps for is not the chain of the text it hands FFmpeg.
    let p = png(1200, 800);
    let mut wrong = Vec::new();
    for scaler in ["Bicubic", "BICUBIC"] {
        for (route, mv) in [("route", IN_A), ("placed", OFF_A)] {
            let tl = timeline(vec![vec![moving(&p, 0.0, 1.0, mv)]], cover(320, 180));
            let opts = ExportOptions {
                scaler: Some(scaler.to_string()),
                ..ExportOptions::default()
            };
            let plan = RenderPlan::at(&tl, std::slice::from_ref(&p), &opts, 0.5, policy()).expect("plan");
            let size = plan.size(u32::MAX);
            let plan = plan.for_render(size);
            if let Err(e) = chain_audit(&plan, size, false) {
                wrong.push(format!("{route} {scaler}: {e}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn r2_a_blended_moving_still_is_refused_in_a_still_plan_for_the_move_on_both_routes() {
    let f = clips();
    let p = png(1200, 800);
    for (route, mv) in [("route", IN_A), ("placed", OFF_A)] {
        for mode in kerf_core::BlendMode::ALL.into_iter().filter(|m| !m.is_normal()) {
            let mut c = moving(&p, 0.0, 1.0, mv);
            c.blend_mode = mode;
            let tl = timeline(vec![vec![fc(&f.bt709, 0.0, 1.0, 0.0)], vec![c]], cover(320, 180));
            let assets = vec![p.clone(), f.bt709.clone()];
            let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
            let size = plan.size(u32::MAX);
            let plan = plan.for_render(size);
            kerf_gpu::prepare(&plan);
            let why = reasons_of(&plan, compositor(), size);
            assert!(
                why.iter()
                    .any(|(k, t)| k == "blend_mode" && t.contains("a moving zoom or position")),
                "{route} {mode:?}: not refused for the move: {why:?}"
            );
        }
    }
}
