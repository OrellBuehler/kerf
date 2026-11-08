//! The independent checker's cases for the zero-tolerance audit: the translucent round trip, `gblur`, the display conversion and the
//! full-range pictures in a still, each held to FFmpeg's own export (motion plans) or still with **no bound**: a frame the plan lets the
//! compositor draw is the export's to the sample, or the case fails.
//!
//! Besides the exactness every case also asks the **refused** frames to be drawn anyway (the compositor is asked for the planes
//! whatever the plan says; a refusal inside the compositor itself is reported as such), which sorts the refusals into the ones that are
//! needed (the frame drawn anyway differs) and the ones that are wider than they have to be (the frame drawn anyway is the export's):
//! those are listed as `WIDE` lines and do not fail the suite. (`Compositor::composite_texts` re-checks the plan with its own caps, so
//! as shipped a plan-level refusal comes back "the compositor refuses too"; the checker's necessity runs lift that check, and the two
//! translucent guards, in a scratch edit of `compositor.rs` and restore it.)
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_z -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
//!
//! `KERF_FFMPEG` may also be a wrapper that adds `-cpuflags ...` (a machine with other CPU kernels): the probes must then refuse
//! what they cannot vouch for, and `z_a_machine_with_other_kernels_is_protected_by_the_probes` shows the refusals needed.

#![allow(
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::cast_possible_truncation,
    clippy::collapsible_if,
    clippy::manual_is_multiple_of,
    clippy::manual_range_contains,
    clippy::precedence,
    clippy::chunks_exact_to_as_chunks
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::{
    export_still, Asset, BlendMode, Clip, Color, CompositeColorPolicy, Container, Delivery, Easing, ExportOptions, Fit,
    ImageFormat, Mask, MaskShape, PlanMode, PlanRequest, Planner, Project, Property, PropertyKey, RateControl, RenderPlan,
    StreamKind, Timeline, Track, Transition, TransitionKind, VideoEffect,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuError, GpuOptions, MotionFrames, YuvFrame};

// ---- environment ------------------------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn prime() {
    let _ = kerf_gpu::grade::kernels();
    let _ = kerf_gpu::scaler::kernels();
    let _ = kerf_gpu::convert::kernels();
    let _ = kerf_core::geq_edge();
    let _ = kerf_gpu::conversion::conversion();
    let _ = kerf_gpu::placement_probe::placement();
    let _ = kerf_gpu::roundtrip_probe::translucency();
    let _ = kerf_gpu::blur_probe::blur();
    let _ = kerf_gpu::text::model();
    let _ = kerf_gpu::blend::kernels();
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| {
        prime();
        Compositor::new(gpu()).expect("a compositor")
    })
}

/// A compositor that believes the probes matched, whatever they said: for drawing a refused frame anyway.
fn trusting() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| {
        prime();
        Compositor::new(gpu())
            .expect("a compositor")
            .with_translucency(kerf_gpu::roundtrip_probe::Translucency::Match)
            .with_blur(kerf_gpu::blur_probe::Blur::Match)
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

fn translucency_matches() -> bool {
    kerf_gpu::roundtrip_probe::translucency() == kerf_gpu::roundtrip_probe::Translucency::Match
}

fn blur_matches() -> bool {
    kerf_gpu::blur_probe::blur() == kerf_gpu::blur_probe::Blur::Match
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

fn ffmpeg_piped(args: &[&str], input: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
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

fn write_png(path: &Path, rgb: &[u8], w: usize, h: usize) {
    let size = format!("{w}x{h}");
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
        rgb,
    );
}

fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

// ---- media -----------------------------------------------------------------------------------

const UNTAGGED: &str = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
const BT709: &str = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
const BT2020: &str = "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt709";
const SMPTE170M: &str = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";
const BT470BG: &str = "setparams=colorspace=bt470bg:color_primaries=bt470bg:color_trc=bt470bg";

/// Noise in all three planes at `w` x `h` (every Y, U, V combination), then `tail` filters.
fn noise_src(w: u32, h: u32, secs: u32, tail: &str) -> String {
    let tail = if tail.is_empty() { String::new() } else { format!(",{tail}") };
    format!(
        "nullsrc=size={w}x{h}:rate=30:duration={secs},format=yuv444p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'{tail}"
    )
}

/// A smooth luma ramp with chroma against it (a picture whose rounding is easy to follow) at `w` x `h`.
fn ramp_src(w: u32, h: u32, secs: u32, tail: &str) -> String {
    let tail = if tail.is_empty() { String::new() } else { format!(",{tail}") };
    format!(
        "nullsrc=size={w}x{h}:rate=30:duration={secs},format=yuv444p,geq=lum='X*255/W':cb='128+(Y-H/2)*100/H':cr='255-X*255/W'{tail}"
    )
}

struct Media {
    dir: PathBuf,
    cache: std::sync::Mutex<BTreeMap<String, Asset>>,
}

impl Media {
    /// A lossless clip made by `lavfi` + `codec`, in pixel format `pix`; `extra` are output options (`-color_range pc`...).
    fn make(&self, name: &str, lavfi: &str, pix: &str, codec: &[&str], extra: &[&str]) -> Asset {
        let mut cache = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(a) = cache.get(name) {
            return a.clone();
        }
        let p = self.dir.join(name);
        let part = self.dir.join(format!(
            "{}.{}.part.{}",
            slug(name),
            std::process::id(),
            name.rsplit('.').next().unwrap()
        ));
        let mut args = vec!["-f", "lavfi", "-i", lavfi];
        args.extend_from_slice(codec);
        args.extend_from_slice(&["-pix_fmt", pix]);
        args.extend_from_slice(extra);
        args.push(part.to_str().unwrap());
        ffmpeg(&args);
        std::fs::rename(&part, &p).expect("move the clip into place");
        let a = probed(&p);
        cache.insert(name.to_string(), a.clone());
        a
    }

    fn x264(&self, name: &str, lavfi: &str, pix: &str, extra: &[&str]) -> Asset {
        self.make(
            name,
            lavfi,
            pix,
            &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"],
            extra,
        )
    }

    fn ffv1(&self, name: &str, lavfi: &str, pix: &str, extra: &[&str]) -> Asset {
        self.make(name, lavfi, pix, &["-c:v", "ffv1"], extra)
    }

    /// A still picture (PNG or JPEG) of noise, `w` x `h`.
    fn still(&self, name: &str, w: u32, h: u32, pix: &str) -> Asset {
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=1:duration=1,format=rgb24,geq=r='clip((X*255/W+Y*255/H)/2+random(1)*70-35,0,255)':g='clip(255-X*255/W+random(2)*60-30,0,255)':b='clip(Y*255/H+random(3)*90-45,0,255)'"
        );
        self.make(name, &lavfi, pix, &["-frames:v", "1"], &[])
    }
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-z-media");
        std::fs::create_dir_all(&dir).unwrap();
        Media {
            dir,
            cache: std::sync::Mutex::new(BTreeMap::new()),
        }
    })
}

/// The ordinary 640x360 pictures every case is made of.
struct Std {
    noise: Asset,
    testsrc: Asset,
    bars: Asset,
    ramp: Asset,
}

fn std_media() -> &'static Std {
    static S: OnceLock<Std> = OnceLock::new();
    S.get_or_init(|| {
        let m = media();
        Std {
            noise: m.x264("z-noise.mp4", &noise_src(640, 360, 2, UNTAGGED), "yuv420p", &[]),
            testsrc: m.x264(
                "z-testsrc.mp4",
                &format!("testsrc2=size=640x360:rate=30:duration=2,{UNTAGGED}"),
                "yuv420p",
                &[],
            ),
            bars: m.x264(
                "z-bars.mp4",
                &format!("smptehdbars=size=640x360:rate=30:duration=2,{UNTAGGED}"),
                "yuv420p",
                &[],
            ),
            ramp: m.x264("z-ramp.mp4", &ramp_src(640, 360, 2, UNTAGGED), "yuv420p", &[]),
        }
    })
}

/// Noise in one pixel format / range / tag, 640x360 (480x270 for the heavy ones), by name.
fn noise_format(name: &str) -> Asset {
    let m = media();
    let n = |tag: &str| noise_src(640, 360, 2, tag);
    match name {
        "yuv420p" => std_media().noise.clone(),
        "yuv422p" => m.ffv1("z-n-422.mkv", &n(UNTAGGED), "yuv422p", &[]),
        "yuv444p" => m.ffv1("z-n-444.mkv", &n(UNTAGGED), "yuv444p", &[]),
        "yuv440p" => m.ffv1("z-n-440.mkv", &n(UNTAGGED), "yuv440p", &[]),
        "yuv411p" => m.ffv1("z-n-411.mkv", &n(UNTAGGED), "yuv411p", &[]),
        "yuv410p" => m.ffv1("z-n-410.mkv", &n(UNTAGGED), "yuv410p", &[]),
        "gray" => m.ffv1("z-n-gray.mkv", &n(UNTAGGED), "gray", &[]),
        "bgr0" => m.ffv1("z-n-bgr0.mkv", &n(UNTAGGED), "bgr0", &[]),
        "yuv420p10le" => m.x264("z-n-10.mp4", &n(UNTAGGED), "yuv420p10le", &[]),
        "yuv422p10le" => m.ffv1("z-n-422-10.mkv", &n(UNTAGGED), "yuv422p10le", &[]),
        "yuv444p10le" => m.ffv1("z-n-444-10.mkv", &n(UNTAGGED), "yuv444p10le", &[]),
        "yuv420p10le-pc" => m.x264("z-n-10-pc.mp4", &n(UNTAGGED), "yuv420p10le", &["-color_range", "pc"]),
        "yuva420p" => m.ffv1(
            "z-n-alpha.mkv",
            "nullsrc=size=640x360:rate=30:duration=2,format=yuva420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255':a='if(lt(X,W/2),255,random(4)*255)'",
            "yuva420p",
            &[],
        ),
        "yuvj420p" => m.make("z-n-j420.mkv", &n(UNTAGGED), "yuvj420p", &["-c:v", "mjpeg", "-qscale:v", "1"], &[]),
        "yuvj422p" => m.make("z-n-j422.mkv", &n(UNTAGGED), "yuvj422p", &["-c:v", "mjpeg", "-qscale:v", "1"], &[]),
        "yuvj444p" => m.make("z-n-j444.mkv", &n(UNTAGGED), "yuvj444p", &["-c:v", "mjpeg", "-qscale:v", "1"], &[]),
        "yuv420p-pc" => m.ffv1("z-n-420-pc.mkv", &n(UNTAGGED), "yuv420p", &["-color_range", "pc"]),
        "yuv422p-pc" => m.ffv1("z-n-422-pc.mkv", &n(UNTAGGED), "yuv422p", &["-color_range", "pc"]),
        "yuv444p-pc" => m.ffv1("z-n-444-pc.mkv", &n(UNTAGGED), "yuv444p", &["-color_range", "pc"]),
        "yuv420p-tv" => m.ffv1("z-n-420-tv.mkv", &n(UNTAGGED), "yuv420p", &["-color_range", "tv"]),
        "bt709" => m.x264("z-n-709.mp4", &n(BT709), "yuv420p", &[]),
        "bt2020" => m.x264("z-n-2020.mp4", &n(BT2020), "yuv420p", &[]),
        "smpte170m" => m.x264("z-n-170m.mp4", &n(SMPTE170M), "yuv420p", &[]),
        "bt470bg" => m.x264("z-n-470bg.mp4", &n(BT470BG), "yuv420p", &[]),
        other => panic!("no such noise: {other}"),
    }
}

/// The test pattern in a tag, as a background of the same matrix.
fn testsrc_tag(name: &str) -> Asset {
    let m = media();
    let src = |tag: &str| format!("testsrc2=size=640x360:rate=30:duration=2,{tag}");
    match name {
        "untagged" => std_media().testsrc.clone(),
        "bt709" => m.x264("z-t-709.mp4", &src(BT709), "yuv420p", &[]),
        "bt2020" => m.x264("z-t-2020.mp4", &src(BT2020), "yuv420p", &[]),
        "smpte170m" => m.x264("z-t-170m.mp4", &src(SMPTE170M), "yuv420p", &[]),
        "bt470bg" => m.x264("z-t-470bg.mp4", &src(BT470BG), "yuv420p", &[]),
        other => panic!("no such tag: {other}"),
    }
}

// ---- timelines -------------------------------------------------------------------------------

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

fn clip(a: &Asset, source_in: f64, source_out: f64, start: f64) -> Clip {
    Clip::new(a.id, source_in, source_out, start)
}

/// `fg` (edited by `edit`) over `bg`, both `secs` long from the start; returns the cut and its assets.
fn over(bg: &Asset, fg: &Asset, secs: f64, edit: &dyn Fn(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut top = clip(fg, 0.0, secs, 0.0);
    edit(&mut top);
    (
        timeline(vec![vec![clip(bg, 0.0, secs, 0.0)], vec![top]], None),
        vec![bg.clone(), fg.clone()],
    )
}

/// `fg` alone on the first track (over the black base).
fn alone(fg: &Asset, secs: f64, format: Option<Delivery>, edit: &dyn Fn(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut c = clip(fg, 0.0, secs, 0.0);
    edit(&mut c);
    (timeline(vec![vec![c]], format), vec![fg.clone()])
}

fn opacity(o: f64) -> impl Fn(&mut Clip) {
    move |c| c.transform.opacity = o
}

fn at_fps(fps: f64) -> ExportOptions {
    ExportOptions {
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

fn mask_of(shape: MaskShape) -> Mask {
    Mask {
        shape,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather: 0.1,
        inverted: false,
    }
}

// ---- the export, read back as planes ---------------------------------------------------------

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
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-z-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    let file = scratch.join(format!("{}.mkv", slug(name)));
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts.clone()
    };
    kerf_core::render_with(tl, assets, &file, &lossless).unwrap_or_else(|e| panic!("{name}: export: {e}"));
    let raw = ffmpeg(&[
        "-i",
        file.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ]);
    let info = probed(&file);
    let video = info
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .expect("a video stream");
    let _ = std::fs::remove_file(&file);
    Played {
        w: video.width.unwrap() as usize,
        h: video.height.unwrap() as usize,
        yuv: raw,
    }
}

// ---- the exact comparator ---------------------------------------------------------------------

#[derive(Clone, Copy, Default, Debug)]
struct Diff {
    n: usize,
    max: i32,
    first: Option<(usize, usize)>,
}

impl Diff {
    fn of(want: &[u8], got: &[u8], width: usize) -> Self {
        assert_eq!(want.len(), got.len(), "the planes are not the same size");
        let mut d = Self::default();
        for (i, (a, b)) in want.iter().zip(got).enumerate() {
            let e = (i32::from(*a) - i32::from(*b)).abs();
            if e != 0 {
                d.n += 1;
                d.max = d.max.max(e);
                d.first.get_or_insert((i % width, i / width));
            }
        }
        d
    }

    fn text(&self, plane: &str) -> String {
        format!("{plane}: {} differ (max {}, first {:?})", self.n, self.max, self.first)
    }
}

/// The three planes of a frame against the export's; `Ok` when no sample differs.
fn compare_planes(want: [&[u8]; 3], got: &YuvFrame, w: usize) -> Result<(), String> {
    let d = [
        Diff::of(want[0], &got.y, w),
        Diff::of(want[1], &got.u, w / 2),
        Diff::of(want[2], &got.v, w / 2),
    ];
    if d.iter().all(|d| d.n == 0) {
        Ok(())
    } else {
        Err([d[0].text("Y"), d[1].text("U"), d[2].text("V")]
            .iter()
            .zip(&d)
            .filter(|(_, d)| d.n > 0)
            .map(|(t, _)| t.clone())
            .collect::<Vec<_>>()
            .join("; "))
    }
}

// ---- the verdicts -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// At least one frame must be drawn (and exact), unless this machine's probe vouches for nothing.
    Drawn,
    /// Every frame must be refused by the plan.
    Refused,
    /// Either: only exactness of the drawn frames is asked.
    Any,
}

#[derive(Default)]
struct Suite {
    name: &'static str,
    failures: Vec<String>,
    wide: Vec<String>,
    needed: BTreeMap<String, usize>,
    guarded: BTreeMap<String, usize>,
    drawn: usize,
    exact_refused: usize,
    refused: usize,
    cases: usize,
}

impl Suite {
    fn new(name: &'static str) -> Self {
        let _ = compositor();
        Self { name, ..Self::default() }
    }

    fn fail(&mut self, msg: String) {
        eprintln!("Z FAIL {msg}");
        self.failures.push(msg);
    }

    /// An export of `tl` (frames `0..n` at `fps`) against the compositor's planes.
    fn motion(&mut self, case: &str, tl: &Timeline, assets: &[Asset], fps: f64, n: u64, expect: Expect) {
        self.cases += 1;
        let case = format!("{}/{case}", self.name);
        let opts = at_fps(fps);
        let played = export(&case, tl, assets, &opts);
        let n = n.min(played.count() as u64);
        let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(policy())).expect("planner");
        assert_eq!(planner.mode(), PlanMode::Motion);
        let size = (played.w as u32, played.h as u32);
        let mut motion = MotionFrames::new(frame_source());
        let (mut drawn, mut refused, mut wide, mut needed, mut unsup) = (0, 0, 0, 0, 0);
        let mut reasons = BTreeSet::new();
        let mut first_bad = None;
        for k in 0..n {
            let plan = planner.at_frame(k).expect("plan");
            let why: Vec<String> = plan
                .reasons(&compositor().caps(), size)
                .iter()
                .map(ToString::to_string)
                .collect();
            let picture = match motion.frames(&plan) {
                Ok(p) => p,
                Err(e) => {
                    if why.is_empty() {
                        self.fail(format!("{case} frame {k}: the frames of a drawn plan: {e}"));
                    }
                    refused += 1;
                    continue;
                }
            };
            let want = played.planes(k as usize);
            let drew = if why.is_empty() {
                trusting_or_not(false).composite_yuv_counted(&plan, &picture, motion.counts(), size)
            } else {
                trusting_or_not(true).composite_yuv_counted(&plan, &picture, motion.counts(), size)
            };
            if why.is_empty() {
                match drew {
                    Ok(got) => {
                        drawn += 1;
                        if let Err(e) = compare_planes(want, &got, played.w) {
                            self.fail(format!("{case} frame {k}: DRAWN AND DIFFERENT: {e}"));
                            first_bad.get_or_insert(k);
                        }
                    }
                    Err(e) => self.fail(format!("{case} frame {k}: the plan draws it, the compositor says: {e}")),
                }
            } else {
                refused += 1;
                for r in &why {
                    reasons.insert(
                        r.split(':')
                            .next_back()
                            .unwrap_or(r)
                            .trim()
                            .chars()
                            .take(70)
                            .collect::<String>(),
                    );
                }
                match drew {
                    Ok(got) => match compare_planes(want, &got, played.w) {
                        Ok(()) => wide += 1,
                        Err(_) => needed += 1,
                    },
                    Err(GpuError::Unsupported(_)) => unsup += 1,
                    Err(e) => self.fail(format!("{case} frame {k}: drawn anyway: {e}")),
                }
            }
        }
        if let Some(k) = first_bad {
            self.montage(&case, k, &played, tl, assets, &opts, &planner);
        }
        self.drawn += drawn;
        self.refused += refused;
        self.exact_refused += wide;
        let line = format!(
            "Z {case:<58} frames 0..{n:<3} drawn {drawn:<3} refused {refused:<3} (anyway: exact {wide}, differs {needed}, compositor refuses {unsup}) {}",
            reasons.iter().cloned().collect::<Vec<_>>().join(" | ")
        );
        eprintln!("{line}");
        if wide > 0 {
            self.wide.push(line);
        }
        for r in &reasons {
            *self.needed.entry(r.clone()).or_default() += needed;
            *self.guarded.entry(r.clone()).or_default() += unsup;
        }
        self.expectation(&case, expect, drawn, refused, n as usize, true);
    }

    #[allow(clippy::too_many_arguments)]
    fn montage(
        &self,
        case: &str,
        k: u64,
        played: &Played,
        _tl: &Timeline,
        _assets: &[Asset],
        _opts: &ExportOptions,
        planner: &Planner,
    ) {
        let plan = planner.at_frame(k).expect("plan");
        let size = (played.w as u32, played.h as u32);
        let mut motion = MotionFrames::new(frame_source());
        let Ok(picture) = motion.frames(&plan) else { return };
        let Ok(got) = compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size) else {
            return;
        };
        let to_rgb = |y: &[u8], u: &[u8], v: &[u8]| {
            let mut raw = y.to_vec();
            raw.extend_from_slice(u);
            raw.extend_from_slice(v);
            ffmpeg_piped(
                &[
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-s",
                    &format!("{}x{}", played.w, played.h),
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
        let want = played.planes(k as usize);
        let a = to_rgb(want[0], want[1], want[2]);
        let b = to_rgb(&got.y, &got.u, &got.v);
        let (w, h) = (played.w, played.h);
        let mut strip = Vec::with_capacity(w * h * 9);
        for row in 0..h {
            strip.extend_from_slice(&a[row * w * 3..(row + 1) * w * 3]);
            strip.extend_from_slice(&b[row * w * 3..(row + 1) * w * 3]);
            for i in 0..w {
                let d = |c: usize| (i32::from(a[(row * w + i) * 3 + c]) - i32::from(b[(row * w + i) * 3 + c])).unsigned_abs();
                let mag = (d(0).max(d(1)).max(d(2)) * 8).min(255) as u8;
                strip.extend_from_slice(&[mag, mag, mag]);
            }
        }
        let dir = target_dir().join("parity").join("z");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}-f{k:03}.png", slug(case)));
        write_png(&path, &strip, w * 3, h);
        eprintln!("Z montage {}", path.display());
    }

    fn expectation(&mut self, case: &str, expect: Expect, drawn: usize, refused: usize, frames: usize, probes_vouch: bool) {
        match expect {
            Expect::Drawn if drawn == 0 && probes_vouch => {
                if self.vouched() {
                    self.fail(format!("{case}: EXPECTED DRAWN, nothing was ({refused} of {frames} frames refused): a refusal wider than it has to be, or a vacuous case"));
                }
            }
            Expect::Refused if drawn > 0 => self.fail(format!("{case}: EXPECTED REFUSED, {drawn} of {frames} frames were drawn")),
            _ => {}
        }
    }

    /// Whether this machine's probes vouch for the round trip and the blur (a case that needs them drawn is vacuous otherwise).
    fn vouched(&self) -> bool {
        translucency_matches() && blur_matches()
    }

    /// A still of `tl` at `t`: planes, PNG and display against FFmpeg's, or the refusal drawn anyway.
    fn still(&mut self, case: &str, tl: &Timeline, assets: &[Asset], t: f64, expect: Expect) {
        self.cases += 1;
        let case = format!("{}/{case}", self.name);
        let opts = ExportOptions::default();
        let plan = RenderPlan::at(tl, assets, &opts, t, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let (w, h) = (size.0 as usize, size.1 as usize);
        let why: Vec<String> = plan
            .reasons(&compositor().caps(), size)
            .iter()
            .map(ToString::to_string)
            .collect();
        let layers = match kerf_gpu::decode_layers(&plan.layers) {
            Ok(l) => l,
            Err(e) => {
                if why.is_empty() {
                    self.fail(format!("{case} t={t}: the plan draws it, the decode says: {e}"));
                } else {
                    self.refused += 1;
                    eprintln!("Z {case:<58} t={t}: refused {why:?}; the decode refuses too: {e}");
                }
                return;
            }
        };
        let want = kerf_core::timeline_frame_yuv_cancellable(tl, assets, &opts, t, u32::MAX, &|| false);
        let Ok((_, _, want444)) = want else {
            eprintln!("Z {case:<58} t={t}: FFmpeg's own still fails; the plan says {why:?}");
            if why.is_empty() {
                self.fail(format!(
                    "{case} t={t}: the plan draws a frame FFmpeg's own still cannot produce"
                ));
            }
            self.refused += 1;
            return;
        };
        let n = w * h;
        let planes_of = |drawn: &YuvFrame| -> Result<(), String> {
            let got444 = ffmpeg_over_planes(drawn, (w, h), "format=yuv444p", "yuv444p");
            let d: Vec<Diff> = (0..3)
                .map(|p| Diff::of(&want444[p * n..(p + 1) * n], &got444[p * n..(p + 1) * n], w))
                .collect();
            if d.iter().all(|d| d.n == 0) {
                Ok(())
            } else {
                Err(["Y", "U", "V"]
                    .iter()
                    .zip(&d)
                    .filter(|(_, d)| d.n > 0)
                    .map(|(p, d)| d.text(p))
                    .collect::<Vec<_>>()
                    .join("; "))
            }
        };
        if !why.is_empty() {
            self.refused += 1;
            let anyway = match trusting().composite_yuv(&plan, &layers, size) {
                Ok(drawn) => match planes_of(&drawn) {
                    Ok(()) => {
                        self.exact_refused += 1;
                        let line = format!("Z {case:<58} t={t}: REFUSED but drawn anyway it is exact: {why:?}");
                        self.wide.push(line);
                        "exact".to_string()
                    }
                    Err(e) => {
                        for r in &why {
                            *self
                                .needed
                                .entry(r.split(':').next_back().unwrap_or(r).trim().chars().take(70).collect())
                                .or_default() += 1;
                        }
                        format!("differs: {e}")
                    }
                },
                Err(GpuError::Unsupported(e)) => {
                    for r in &why {
                        *self
                            .guarded
                            .entry(r.split(':').next_back().unwrap_or(r).trim().chars().take(70).collect())
                            .or_default() += 1;
                    }
                    format!("the compositor refuses too: {e}")
                }
                Err(e) => {
                    self.fail(format!("{case} t={t}: drawn anyway: {e}"));
                    String::new()
                }
            };
            eprintln!(
                "Z {case:<58} t={t}: refused {:?}; anyway: {anyway}",
                why.iter().map(|w| w.chars().take(90).collect::<String>()).collect::<Vec<_>>()
            );
            if expect == Expect::Drawn && self.vouched() {
                self.fail(format!("{case} t={t}: EXPECTED DRAWN, refused: {why:?}"));
            }
            return;
        }
        self.drawn += 1;
        let mut errs = Vec::new();
        let drawn = match compositor().composite_yuv(&plan, &layers, size) {
            Ok(d) => d,
            Err(e) => {
                self.fail(format!("{case} t={t}: the plan draws it, the compositor says: {e}"));
                return;
            }
        };
        if let Err(e) = planes_of(&drawn) {
            errs.push(format!("planes: {e}"));
        }
        let png = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("checker-z-ref")
            .join(format!("{}-{t}.png", slug(&case)));
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        export_still(tl, assets, &opts, t, &png, ImageFormat::Png, 0).expect("FFmpeg still");
        let reference = ffmpeg(&["-i", png.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
        let _ = std::fs::remove_file(&png);
        let got_rgb = ffmpeg_over_planes(
            &drawn,
            (w, h),
            &format!("scale=in_color_matrix={}:in_range=tv", matrix_name(&plan)),
            "rgb24",
        );
        let png_d = Diff::of(&reference, &got_rgb, w * 3);
        if png_d.n > 0 {
            errs.push(format!("png: {}", png_d.text("RGB")));
        }
        if w % 2 == 0
            && h % 2 == 0
            && (kerf_gpu::grade::measured() == kerf_gpu::grade::Kernels::Match
                || std::env::var_os("KERF_Z_FORCE_DISPLAY").is_some())
        {
            let (frame, _) = compositor().render_plan(&plan, size).expect("GPU render");
            let rgb: Vec<u8> = frame
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            let disp = Diff::of(&got_rgb, &rgb, w * 3);
            if disp.n > 0 {
                errs.push(format!("display: {}", disp.text("RGB")));
            }
        }
        if errs.is_empty() {
            self.cases_ok(&case, t, w, h);
        } else {
            self.fail(format!("{case} t={t} {w}x{h}: DRAWN AND DIFFERENT: {}", errs.join("; ")));
        }
        if expect == Expect::Refused {
            self.fail(format!("{case} t={t}: EXPECTED REFUSED, drawn"));
        }
    }

    fn cases_ok(&self, case: &str, t: f64, w: usize, h: usize) {
        eprintln!("Z {case:<58} t={t}: drawn {w}x{h} exact (planes, png, display)");
    }

    fn finish(self) {
        eprintln!(
            "Z SUMMARY {}: {} cases; frames drawn {} (all exact unless listed), refused {} of which exact when drawn anyway {}",
            self.name, self.cases, self.drawn, self.refused, self.exact_refused
        );
        for (r, n) in &self.needed {
            eprintln!(
                "Z NEEDED {r}: {n} frames differ when drawn anyway (and {} more are refused by the compositor itself)",
                self.guarded.get(r).copied().unwrap_or(0)
            );
        }
        for (r, n) in &self.guarded {
            if !self.needed.contains_key(r) {
                eprintln!("Z GUARDED {r}: {n} frames the compositor itself refuses (not drawable anyway)");
            }
        }
        for w in &self.wide {
            eprintln!("Z WIDE {w}");
        }
        assert!(
            self.failures.is_empty(),
            "{} failures:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

fn trusting_or_not(refused: bool) -> &'static Compositor {
    if refused {
        trusting()
    } else {
        compositor()
    }
}

fn matrix_name(plan: &RenderPlan) -> &'static str {
    match plan.canvas.matrix {
        kerf_core::YuvMatrix::Bt601 => "bt601",
        kerf_core::YuvMatrix::Bt709 => "bt709",
        kerf_core::YuvMatrix::Bt2020 => "bt2020nc",
    }
}

fn ffmpeg_over_planes(planes: &YuvFrame, (w, h): (usize, usize), filter: &str, out_pix: &str) -> Vec<u8> {
    let mut raw = planes.y.clone();
    raw.extend_from_slice(&planes.u);
    raw.extend_from_slice(&planes.v);
    ffmpeg_piped(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "pipe:0",
            "-vf",
            filter,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            out_pix,
            "pipe:1",
        ],
        &raw,
    )
}

fn key(time: f64, value: f64) -> PropertyKey {
    PropertyKey::new(time, value)
}

fn eased(time: f64, value: f64, easing: Easing) -> PropertyKey {
    PropertyKey { time, value, easing }
}

fn delivery(w: u32, h: u32, fit: Fit) -> Option<Delivery> {
    Some(Delivery::new(w, h, fit))
}

const SECS: f64 = 0.4;
const N: u64 = 8;

// ---- translucent layers: export frames -----------------------------------------------------------

/// A constant opacity near 0, near 1, at the ties of `lrint(255 * a)` and at odd values.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_translucent_opacity_sweep_in_an_export() {
    let (m, mut s) = (std_media(), Suite::new("opacity"));
    let ties: Vec<f64> = [0.5, 1.5, 2.5, 126.5, 127.5, 128.5, 253.5, 254.5]
        .iter()
        .map(|k| k / 255.0)
        .collect();
    let mut values = vec![
        0.0, 0.0001, 0.001, 0.004, 0.01, 0.02, 0.05, 0.1, 0.1234567, 0.25, 0.3333, 0.4, 0.4999, 0.5, 0.5001, 0.6, 0.75, 0.9,
        0.99, 0.995, 0.9961, 0.998, 0.999, 0.9999, 0.99999,
    ];
    values.extend(ties);
    for o in values {
        let (tl, assets) = over(&m.bars, &m.noise, SECS, &opacity(o));
        s.motion(&format!("noise over bars at {o}"), &tl, &assets, 30.0, N, Expect::Any);
    }
    // The base: a lone translucent clip over the black canvas.
    for o in [0.04, 0.5, 0.96] {
        let (tl, assets) = alone(&m.noise, SECS, None, &opacity(o));
        s.motion(&format!("noise alone at {o}"), &tl, &assets, 30.0, N, Expect::Any);
    }
    let (tl, assets) = over(&m.bars, &m.noise, SECS, &opacity(0.6));
    s.motion("the drawn control", &tl, &assets, 30.0, N, Expect::Drawn);
    s.finish();
}

/// A translucent layer in every pixel format the decode reduces, over bars.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_translucent_every_format_in_an_export() {
    let (m, mut s) = (std_media(), Suite::new("format"));
    for name in [
        "yuv420p",
        "yuv422p",
        "yuv444p",
        "yuv440p",
        "yuv411p",
        "yuv410p",
        "gray",
        "bgr0",
        "yuv420p10le",
        "yuv422p10le",
        "yuv444p10le",
        "yuv420p10le-pc",
        "yuva420p",
        "yuvj420p",
        "yuvj422p",
        "yuvj444p",
        "yuv420p-pc",
        "yuv422p-pc",
        "yuv444p-pc",
        "yuv420p-tv",
    ] {
        let fg = noise_format(name);
        for o in [0.37, 0.9] {
            let (tl, assets) = over(&m.bars, &fg, SECS, &opacity(o));
            s.motion(&format!("{name} at {o}"), &tl, &assets, 30.0, 5, Expect::Any);
        }
    }
    s.finish();
}

/// Odd sizes, the last chroma row, positions and crops.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_translucent_sizes_and_positions_in_an_export() {
    let (m, mut s) = (std_media(), Suite::new("size"));
    for scale in [
        0.05, 0.1, 0.125, 0.2, 0.25, 0.26, 0.27, 0.28, 0.3, 0.33, 0.35, 0.4, 0.4985, 0.5, 0.501, 0.6, 0.75, 0.9, 1.0, 1.2, 1.6,
    ] {
        let (tl, assets) = over(&m.bars, &m.noise, SECS, &|c| {
            c.transform.opacity = 0.43;
            c.transform.scale = scale;
        });
        s.motion(&format!("scale {scale}"), &tl, &assets, 30.0, 4, Expect::Any);
    }
    for (name, edit) in [
        ("offset 0.2,-0.3", (0.2, -0.3)),
        ("offset to the right edge", (0.6, 0.0)),
        ("offset past the bottom edge", (0.0, 0.8)),
        ("offset odd pixels", (0.0133, 0.0271)),
    ] {
        let (tl, assets) = over(&m.bars, &m.noise, SECS, &|c| {
            c.transform.opacity = 0.71;
            c.transform.scale = 0.5;
            c.transform.pos_x = edit.0;
            c.transform.pos_y = edit.1;
        });
        s.motion(name, &tl, &assets, 30.0, 4, Expect::Any);
    }
    for (name, l, r, t, b) in [
        ("crop 0.05 left", 0.05, 0.0, 0.0, 0.0),
        ("crop all sides odd", 0.0117, 0.0233, 0.0311, 0.0123),
        ("crop to 321x181", 0.2, 0.2992, 0.2, 0.2972),
        ("crop top and bottom", 0.0, 0.0, 0.1, 0.35),
    ] {
        let (tl, assets) = over(&m.bars, &m.noise, SECS, &|c| {
            c.transform.opacity = 0.62;
            (
                c.transform.crop_left,
                c.transform.crop_right,
                c.transform.crop_top,
                c.transform.crop_bottom,
            ) = (l, r, t, b);
        });
        s.motion(name, &tl, &assets, 30.0, 4, Expect::Any);
    }
    // A delivery size of its own: the same layer fitted into odd-ish canvases.
    for (w, h, fit) in [
        (480, 270, Fit::Cover),
        (202, 100, Fit::Cover),
        (318, 178, Fit::Contain),
        (360, 640, Fit::Contain),
        (96, 54, Fit::Cover),
    ] {
        let (tl, assets) = alone(&m.noise, SECS, delivery(w, h, fit), &opacity(0.55));
        s.motion(&format!("{w}x{h} {fit:?}"), &tl, &assets, 30.0, 4, Expect::Any);
    }
    s.finish();
}

/// A translucent layer with the other things a layer can have.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_translucent_with_everything_in_an_export() {
    let (m, mut s) = (std_media(), Suite::new("with"));
    let fade = 0.8;
    let drawn = Expect::Any;
    // Grade, effects, mask, key, speed.
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.transform.opacity = 0.6;
        c.color = Color {
            gamma: 1.6,
            saturation: 1.3,
            brightness: 0.05,
            ..Color::default()
        };
    });
    s.motion("graded", &tl, &a, 30.0, 4, drawn);
    for (name, effect) in [
        ("blur", VideoEffect::Blur { sigma: 3.0 }),
        ("sharpen", VideoEffect::Sharpen { amount: 1.2 }),
        ("grayscale", VideoEffect::Grayscale),
        ("invert", VideoEffect::Invert),
        ("vignette", VideoEffect::Vignette),
        (
            "chroma key",
            VideoEffect::ChromaKey {
                color: "0x808080".into(),
                similarity: 0.3,
                blend: 0.1,
            },
        ),
    ] {
        let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
            c.transform.opacity = 0.6;
            c.effects = vec![effect.clone()];
        });
        s.motion(&format!("effect {name}"), &tl, &a, 30.0, 4, drawn);
    }
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.transform.opacity = 0.6;
        c.mask = Some(mask_of(MaskShape::Ellipse));
    });
    s.motion("masked", &tl, &a, 30.0, 4, drawn);
    let (tl, a) = over(&m.bars, &m.noise, 0.8, &|c| {
        c.transform.opacity = 0.6;
        c.speed = 2.0;
    });
    s.motion("speed 2", &tl, &a, 30.0, 6, drawn);
    // The clip's own fades, a dissolve, the dips and the slides.
    let (tl, a) = over(&m.bars, &m.noise, fade, &|c| {
        c.transform.opacity = 0.6;
        c.fade_in = 0.4;
        c.fade_out = 0.4;
    });
    s.motion("fades", &tl, &a, 30.0, 24, drawn);
    for (name, kind) in [
        ("crossfade", TransitionKind::Crossfade),
        ("dip to black", TransitionKind::DipToBlack),
        ("dip to white", TransitionKind::DipToWhite),
        ("slide left", TransitionKind::SlideLeft),
        ("push up", TransitionKind::PushUp),
    ] {
        for (who, in_o, out_o) in [("incoming", 0.55, 1.0), ("outgoing", 1.0, 0.55), ("both", 0.7, 0.35)] {
            let mut out = clip(&m.testsrc, 0.0, 0.8, 0.0);
            out.transform.opacity = out_o;
            let mut inc = clip(&m.noise, 0.0, 0.8, 0.6);
            inc.transform.opacity = in_o;
            inc.transition_in = Some(Transition { kind, duration: 0.5 });
            let tl = timeline(vec![vec![out, inc]], None);
            s.motion(
                &format!("{name} {who}"),
                &tl,
                &[m.testsrc.clone(), m.noise.clone()],
                30.0,
                40,
                drawn,
            );
        }
    }
    // Keyed opacity next to a constant one (a keyed property on the other clip), eased, held, over another translucent layer.
    let (tl, a) = {
        let mut top = clip(&m.noise, 0.0, 0.6, 0.0);
        top.set_property_keys(Property::Opacity, vec![eased(0.0, 0.9, Easing::EaseInOut), key(0.5, 0.15)]);
        let mut mid = clip(&m.testsrc, 0.0, 0.6, 0.0);
        mid.transform.opacity = 0.45;
        (
            timeline(vec![vec![clip(&m.bars, 0.0, 0.6, 0.0)], vec![mid], vec![top]], None),
            vec![m.bars.clone(), m.testsrc.clone(), m.noise.clone()],
        )
    };
    s.motion("three layers, one keyed one constant", &tl, &a, 30.0, 15, drawn);
    // Text over a translucent layer.
    let (mut tl, a) = over(&m.bars, &m.noise, SECS, &opacity(0.5));
    let mut o = kerf_core::TextOverlay::new("Translucent 50%", 0.0, 1.0);
    (o.size, o.pos_y, o.bg) = (0.08, 0.5, Some("black@0.5".into()));
    tl.overlays = vec![o];
    s.motion("text over a translucent layer", &tl, &a, 30.0, 4, drawn);
    // 29.97 and 60.
    for fps in [29.97, 24.0, 60.0] {
        let (tl, a) = over(&m.bars, &m.noise, SECS, &opacity(0.8));
        s.motion(&format!("at {fps}"), &tl, &a, fps, 4, drawn);
    }
    s.finish();
}

/// Every matrix of a translucent layer, over the same matrix and over another one.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_translucent_every_matrix_in_an_export() {
    let mut s = Suite::new("matrix");
    for fg in ["untagged", "bt709", "bt2020", "smpte170m", "bt470bg"] {
        let top = if fg == "untagged" {
            std_media().noise.clone()
        } else {
            noise_format(fg)
        };
        for bg in ["untagged", "bt709", "bt2020", "smpte170m", "bt470bg"] {
            let under = testsrc_tag(bg);
            let (tl, a) = over(&under, &top, SECS, &opacity(0.58));
            s.motion(&format!("{fg} over {bg}"), &tl, &a, 30.0, 3, Expect::Any);
        }
    }
    s.finish();
}

/// A turn of a translucent layer: refused (and shown needed in `z_necessity`).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_translucent_turns_in_an_export() {
    let (m, mut s) = (std_media(), Suite::new("turn"));
    for angle in [0.0, 90.0, 180.0, 270.0, -90.0, -180.0, 360.0, 450.0, 45.0, 179.99] {
        for o in [0.5, 0.97] {
            for scale in [1.0, 0.5] {
                let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
                    c.transform.opacity = o;
                    c.transform.rotation = angle;
                    c.transform.scale = scale;
                });
                let quarter = angle % 90.0 == 0.0 && angle != 0.0 && angle != 360.0;
                s.motion(
                    &format!("{angle} degrees at {o} scale {scale}"),
                    &tl,
                    &a,
                    30.0,
                    3,
                    if quarter { Expect::Refused } else { Expect::Any },
                );
            }
        }
    }
    s.finish();
}

// ---- translucent layers: stills ----------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_translucent_in_a_still() {
    let (m, mut s) = (std_media(), Suite::new("still-opacity"));
    for o in [0.0, 0.01, 0.2, 0.3333, 0.5, 0.6, 0.75, 0.99, 0.999] {
        let (tl, a) = over(&m.bars, &m.noise, 1.0, &opacity(o));
        s.still(&format!("noise over bars at {o}"), &tl, &a, 0.5, Expect::Any);
    }
    for scale in [0.25, 0.26, 0.4985, 0.6, 1.2] {
        let (tl, a) = over(&m.bars, &m.noise, 1.0, &|c| {
            c.transform.opacity = 0.6;
            c.transform.scale = scale;
            c.transform.pos_x = 0.13;
        });
        s.still(&format!("scale {scale}"), &tl, &a, 0.5, Expect::Any);
    }
    for name in [
        "yuv422p",
        "yuv444p",
        "gray",
        "bgr0",
        "yuv420p10le",
        "yuv420p10le-pc",
        "yuvj420p",
        "yuv420p-pc",
        "yuva420p",
    ] {
        let fg = noise_format(name);
        let (tl, a) = over(&m.bars, &fg, 1.0, &opacity(0.6));
        s.still(name, &tl, &a, 0.5, Expect::Any);
    }
    for fg in ["bt709", "bt2020", "smpte170m", "bt470bg"] {
        let top = noise_format(fg);
        let under = testsrc_tag(fg);
        let (tl, a) = over(&under, &top, 1.0, &opacity(0.58));
        s.still(&format!("{fg} over {fg}"), &tl, &a, 0.5, Expect::Any);
    }
    // A keyed opacity is sampled in a still; fades; a dissolve; turned.
    let (tl, a) = over(&m.bars, &m.noise, 1.0, &|c| {
        c.set_property_keys(Property::Opacity, vec![key(0.0, 0.9), key(1.0, 0.2)]);
    });
    for t in [0.0, 0.5, 0.97] {
        s.still(&format!("keyed opacity at {t}"), &tl, &a, t, Expect::Any);
    }
    let (tl, a) = over(&m.bars, &m.noise, 1.0, &|c| {
        c.transform.opacity = 0.7;
        c.fade_in = 0.5;
    });
    s.still("fade-in on a translucent layer", &tl, &a, 0.2, Expect::Any);
    let mut inc = clip(&m.noise, 0.0, 1.0, 0.5);
    inc.transform.opacity = 0.6;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0), inc]], None);
    s.still(
        "dissolve into a translucent clip",
        &tl,
        &[m.testsrc.clone(), m.noise.clone()],
        0.7,
        Expect::Any,
    );
    for angle in [90.0, 180.0, 270.0] {
        let (tl, a) = over(&m.bars, &m.noise, 1.0, &|c| {
            c.transform.opacity = 0.6;
            c.transform.rotation = angle;
        });
        s.still(&format!("turned {angle}"), &tl, &a, 0.5, Expect::Refused);
    }
    s.finish();
}

// ---- gblur ---------------------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blur_in_an_export() {
    let (m, mut s) = (std_media(), Suite::new("blur"));
    let blur = |sigma: f64| move |c: &mut Clip| c.effects = vec![VideoEffect::Blur { sigma }];
    for sigma in [
        0.0, 0.001, 0.01, 0.1, 0.3, 0.5, 0.75, 0.9, 1.0, 1.1, 1.5, 2.0, 2.5, 3.7, 5.0, 8.0, 9.99, 17.0, 33.3, 64.0, 100.0, 250.0,
        333.3, 700.0, 1000.0, 1023.9, 1024.0,
    ] {
        let (tl, a) = alone(&m.noise, SECS, None, &blur(sigma));
        s.motion(&format!("noise sigma {sigma}"), &tl, &a, 30.0, 3, Expect::Any);
    }
    for sigma in [0.5, 4.0, 40.0] {
        let (tl, a) = alone(&m.testsrc, SECS, None, &blur(sigma));
        s.motion(&format!("testsrc sigma {sigma}"), &tl, &a, 30.0, 3, Expect::Any);
        let (tl, a) = alone(&m.ramp, SECS, None, &blur(sigma));
        s.motion(&format!("ramp sigma {sigma}"), &tl, &a, 30.0, 3, Expect::Any);
    }
    let (tl, a) = alone(&m.noise, SECS, None, &blur(3.0));
    s.motion("drawn control", &tl, &a, 30.0, 3, Expect::Drawn);
    s.finish();
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blur_sizes_formats_and_company_in_an_export() {
    let (m, mut s) = (std_media(), Suite::new("blur-co"));
    let blur = |sigma: f64| VideoEffect::Blur { sigma };
    // Odd and tiny sizes: a pip scaled down, a cover/contain delivery.
    for scale in [0.02, 0.05, 0.1, 0.33, 0.4985, 0.6, 1.3] {
        let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
            c.transform.scale = scale;
            c.effects = vec![blur(2.5)];
        });
        s.motion(&format!("pip scale {scale}"), &tl, &a, 30.0, 3, Expect::Any);
    }
    for (w, h, fit) in [
        (202, 100, Fit::Cover),
        (318, 178, Fit::Cover),
        (96, 54, Fit::Cover),
        (360, 640, Fit::Cover),
        (640, 360, Fit::Contain),
        (360, 640, Fit::Contain),
        (34, 18, Fit::Cover),
    ] {
        let (tl, a) = alone(&m.noise, SECS, delivery(w, h, fit), &|c| c.effects = vec![blur(5.0)]);
        s.motion(&format!("{w}x{h} {fit:?}"), &tl, &a, 30.0, 3, Expect::Any);
    }
    // Pixel formats.
    for name in [
        "yuv422p",
        "yuv444p",
        "yuv440p",
        "yuv411p",
        "gray",
        "bgr0",
        "yuv420p10le",
        "yuv422p10le",
        "yuv444p10le",
        "yuv420p10le-pc",
        "yuva420p",
        "yuvj420p",
        "yuv420p-pc",
        "yuv444p-pc",
        "yuv420p-tv",
    ] {
        let fg = noise_format(name);
        let (tl, a) = alone(&fg, SECS, None, &|c| c.effects = vec![blur(3.0)]);
        s.motion(name, &tl, &a, 30.0, 3, Expect::Any);
    }
    // Company: another effect, a grade, an opacity (a blurred translucent layer), a keyed position/scale/opacity, a mask, a dissolve.
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.transform.opacity = 0.6;
        c.effects = vec![blur(4.0)];
    });
    s.motion("blur + opacity", &tl, &a, 30.0, 4, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.effects = vec![blur(4.0), VideoEffect::Sharpen { amount: 1.0 }];
    });
    s.motion("blur then sharpen", &tl, &a, 30.0, 4, Expect::Drawn);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.effects = vec![
            VideoEffect::Sharpen { amount: 5.0 },
            blur(4.0),
            VideoEffect::Sharpen { amount: 2.0 },
            VideoEffect::Invert,
        ];
    });
    s.motion("sharpen blur sharpen invert", &tl, &a, 30.0, 4, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.effects = vec![
            blur(1.0),
            VideoEffect::ChromaKey {
                color: "0x808080".into(),
                similarity: 0.25,
                blend: 0.2,
            },
        ];
    });
    s.motion("blur then chroma key", &tl, &a, 30.0, 4, Expect::Drawn);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.transform.scale = 0.6;
        c.effects = vec![VideoEffect::ChromaKey {
            color: "0x808080".into(),
            similarity: 0.25,
            blend: 0.2,
        }];
    });
    s.motion("chroma key on a scaled layer", &tl, &a, 30.0, 4, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.transform.scale = 0.6;
        c.effects = vec![
            blur(2.0),
            VideoEffect::ChromaKey {
                color: "0x808080".into(),
                similarity: 0.25,
                blend: 0.2,
            },
        ];
    });
    s.motion("blur then chroma key on a scaled layer", &tl, &a, 30.0, 4, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.color = Color {
            gamma: 1.5,
            contrast: 1.2,
            ..Color::default()
        };
        c.effects = vec![blur(6.0)];
    });
    s.motion("graded blur", &tl, &a, 30.0, 4, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &|c| {
        c.effects = vec![blur(6.0)];
        c.mask = Some(mask_of(MaskShape::Rect));
    });
    s.motion("blur + mask", &tl, &a, 30.0, 4, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, 0.8, &|c| {
        c.effects = vec![blur(3.0)];
        c.set_property_keys(Property::PosX, vec![key(0.0, -0.3), key(0.7, 0.3)]);
        c.set_property_keys(Property::Scale, vec![key(0.0, 0.5), key(0.7, 0.8)]);
    });
    s.motion("blur + keyed position and scale", &tl, &a, 30.0, 12, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, 0.8, &|c| {
        c.effects = vec![blur(3.0)];
        c.set_property_keys(Property::Opacity, vec![key(0.0, 0.9), key(0.7, 0.2)]);
    });
    s.motion("blur + keyed opacity", &tl, &a, 30.0, 12, Expect::Any);
    let (tl, a) = over(&m.bars, &m.noise, 0.8, &|c| {
        c.effects = vec![blur(3.0)];
        c.set_property_keys(Property::Gamma, vec![key(0.0, 1.0), key(0.7, 2.0)]);
        c.set_property_keys(Property::Brightness, vec![key(0.0, 0.0), key(0.7, 0.2)]);
    });
    s.motion("blur + keyed grade", &tl, &a, 30.0, 12, Expect::Any);
    let mut inc = clip(&m.noise, 0.0, 0.8, 0.5);
    inc.effects = vec![blur(8.0)];
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 0.8, 0.0), inc]], None);
    s.motion(
        "blur into a dissolve",
        &tl,
        &[m.testsrc.clone(), m.noise.clone()],
        30.0,
        30,
        Expect::Any,
    );
    let (tl, a) = over(&m.bars, &m.noise, 0.8, &|c| {
        c.effects = vec![blur(3.0)];
        c.fade_in = 0.4;
        c.fade_out = 0.3;
    });
    s.motion("blur with fades", &tl, &a, 30.0, 24, Expect::Any);
    s.finish();
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blur_in_a_still() {
    let (m, mut s) = (std_media(), Suite::new("still-blur"));
    for sigma in [0.0, 0.2, 1.0, 2.5, 9.0, 40.0, 333.0, 1024.0] {
        let (tl, a) = alone(&m.noise, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
        s.still(&format!("noise sigma {sigma}"), &tl, &a, 0.5, Expect::Any);
    }
    for name in ["yuv422p", "yuv444p", "gray", "bgr0", "yuv420p10le", "yuvj420p", "yuv420p-pc"] {
        let fg = noise_format(name);
        let (tl, a) = alone(&fg, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma: 3.0 }]);
        s.still(name, &tl, &a, 0.5, Expect::Any);
    }
    let (tl, a) = over(&m.bars, &m.noise, 1.0, &|c| {
        c.transform.scale = 0.4985;
        c.transform.opacity = 0.7;
        c.effects = vec![VideoEffect::Blur { sigma: 2.0 }];
    });
    s.still("blurred translucent odd pip", &tl, &a, 0.5, Expect::Any);
    let png = media().still("z-photo.png", 640, 360, "rgb24");
    let (tl, a) = alone(&png, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma: 4.0 }]);
    s.still("blurred png", &tl, &a, 0.0, Expect::Any);
    s.finish();
}

// ---- the display conversion --------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_the_display_conversion_at_every_width_and_range() {
    let (m, mut s) = (std_media(), Suite::new("display"));
    for (w, h) in [
        (2u32, 2u32),
        (4, 2),
        (6, 4),
        (10, 6),
        (14, 8),
        (18, 10),
        (30, 18),
        (34, 18),
        (62, 34),
        (66, 34),
        (98, 54),
        (130, 66),
        (202, 100),
        (210, 118),
        (246, 138),
        (322, 182),
        (466, 262),
        (642, 362),
        (1282, 722),
    ] {
        let (tl, a) = alone(&m.noise, 1.0, delivery(w, h, Fit::Cover), &|_| {});
        s.still(&format!("noise {w}x{h}"), &tl, &a, 0.5, Expect::Any);
    }
    for name in [
        "yuvj420p",
        "yuv420p-pc",
        "yuv444p-pc",
        "yuv420p10le-pc",
        "yuv420p-tv",
        "bgr0",
        "gray",
    ] {
        let fg = noise_format(name);
        let (tl, a) = alone(&fg, 1.0, delivery(202, 100, Fit::Cover), &|_| {});
        s.still(&format!("{name} 202x100"), &tl, &a, 0.5, Expect::Any);
        let (tl, a) = alone(&fg, 1.0, None, &|_| {});
        s.still(&format!("{name} 640x360"), &tl, &a, 0.5, Expect::Any);
    }
    for tag in ["bt709", "bt2020", "smpte170m", "bt470bg"] {
        let fg = noise_format(tag);
        for (w, h) in [(202u32, 100u32), (640, 360)] {
            let (tl, a) = alone(&fg, 1.0, delivery(w, h, Fit::Cover), &|_| {});
            s.still(&format!("{tag} {w}x{h}"), &tl, &a, 0.5, Expect::Any);
        }
    }
    // Saturated corners: a ramp that visits every table entry of the conversion.
    for (w, h) in [(640u32, 360u32), (330, 186)] {
        let (tl, a) = alone(&m.ramp, 1.0, delivery(w, h, Fit::Cover), &|_| {});
        s.still(&format!("ramp {w}x{h}"), &tl, &a, 0.5, Expect::Any);
    }
    s.finish();
}

// ---- full-range pictures in a still and in an export ----------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_full_range_pictures_in_a_still_under_both_policies() {
    let (m, mut s) = (std_media(), Suite::new("range-still"));
    let full = [
        "yuvj420p",
        "yuvj422p",
        "yuvj444p",
        "yuv420p-pc",
        "yuv422p-pc",
        "yuv444p-pc",
        "yuv420p10le-pc",
        "yuv420p10le",
        "yuv420p-tv",
        "yuv420p",
    ];
    for name in full {
        let fg = noise_format(name);
        // alone, at the canvas's size
        let (tl, a) = alone(&fg, 1.0, None, &|_| {});
        s.still(&format!("{name} alone"), &tl, &a, 0.5, Expect::Any);
        // alone, resized (the cover fit into a smaller / larger canvas)
        let (tl, a) = alone(&fg, 1.0, delivery(480, 270, Fit::Cover), &|_| {});
        s.still(&format!("{name} resized to 480x270"), &tl, &a, 0.5, Expect::Any);
        let (tl, a) = alone(&fg, 1.0, delivery(960, 540, Fit::Cover), &|_| {});
        s.still(&format!("{name} resized to 960x540"), &tl, &a, 0.5, Expect::Any);
        // a pip over limited bars, and bars over it
        let (tl, a) = over(&m.bars, &fg, 1.0, &|c| c.transform.scale = 0.5);
        s.still(&format!("{name} as a half pip"), &tl, &a, 0.5, Expect::Any);
        let (tl, a) = over(&fg, &m.bars, 1.0, &|c| c.transform.scale = 0.5);
        s.still(&format!("{name} under a half pip"), &tl, &a, 0.5, Expect::Any);
        // faded
        let (tl, a) = alone(&fg, 1.0, None, &|c| c.fade_in = 0.8);
        s.still(&format!("{name} fading in"), &tl, &a, 0.3, Expect::Any);
        // a grade and a translucent layer
        let (tl, a) = over(&m.bars, &fg, 1.0, &|c| c.transform.opacity = 0.6);
        s.still(&format!("{name} translucent"), &tl, &a, 0.5, Expect::Any);
    }
    // Stills: a JPEG (full range) and a PNG, at the canvas's size and resized.
    let jpeg = media().still("z-photo.jpg", 640, 360, "yuvj420p");
    let jpeg_big = media().still("z-photo-big.jpg", 1200, 800, "yuvj420p");
    let png = media().still("z-photo.png", 640, 360, "rgb24");
    for (name, a, fmt) in [
        ("jpeg 640x360 at 640x360", &jpeg, None),
        ("jpeg 1200x800 at 640x360", &jpeg_big, None),
        ("jpeg 640x360 at 320x180", &jpeg, delivery(320, 180, Fit::Cover)),
        ("jpeg 640x360 at 1280x720", &jpeg, delivery(1280, 720, Fit::Cover)),
        ("png 640x360 at 640x360", &png, None),
        ("png 640x360 at 320x180", &png, delivery(320, 180, Fit::Cover)),
    ] {
        let (tl, assets) = alone(a, 1.0, fmt, &|_| {});
        s.still(name, &tl, &assets, 0.0, Expect::Any);
    }
    let (tl, a) = over(&m.testsrc, &jpeg, 1.0, &|c| c.transform.scale = 0.5);
    s.still("jpeg half pip over video", &tl, &a, 0.5, Expect::Any);
    let (tl, a) = over(&jpeg, &m.testsrc, 1.0, &|c| c.transform.scale = 0.5);
    s.still("video half pip over jpeg", &tl, &a, 0.5, Expect::Any);
    let (tl, a) = over(&m.testsrc, &png, 1.0, &|c| c.transform.scale = 0.5);
    s.still("png half pip over video", &tl, &a, 0.5, Expect::Any);
    s.finish();
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_full_range_pictures_in_an_export_under_both_policies() {
    let (m, mut s) = (std_media(), Suite::new("range-export"));
    for name in [
        "yuvj420p",
        "yuvj422p",
        "yuvj444p",
        "yuv420p-pc",
        "yuv422p-pc",
        "yuv444p-pc",
        "yuv420p10le-pc",
        "yuv420p10le",
        "yuv420p-tv",
    ] {
        let fg = noise_format(name);
        let (tl, a) = alone(&fg, SECS, None, &|_| {});
        s.motion(&format!("{name} alone"), &tl, &a, 30.0, 4, Expect::Any);
        let (tl, a) = alone(&fg, SECS, delivery(480, 270, Fit::Cover), &|_| {});
        s.motion(&format!("{name} resized"), &tl, &a, 30.0, 4, Expect::Any);
        let (tl, a) = over(&m.bars, &fg, SECS, &|c| c.transform.scale = 0.5);
        s.motion(&format!("{name} half pip"), &tl, &a, 30.0, 4, Expect::Any);
        let (tl, a) = over(&m.bars, &fg, SECS, &|c| c.transform.opacity = 0.6);
        s.motion(&format!("{name} translucent"), &tl, &a, 30.0, 4, Expect::Any);
    }
    s.finish();
}

// ---- blend + opacity -------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blend_with_opacity_is_drawn_and_exact() {
    let (m, mut s) = (std_media(), Suite::new("blend-opacity"));
    for mode in BlendMode::ALL.iter().copied().filter(|m| *m != BlendMode::Normal) {
        for o in [0.3, 0.8, 0.999] {
            let (tl, a) = over(&m.testsrc, &m.noise, SECS, &|c| {
                c.blend_mode = mode;
                c.transform.opacity = o;
            });
            s.motion(&format!("{mode:?} at {o}"), &tl, &a, 30.0, 3, Expect::Drawn);
        }
        let (tl, a) = over(&m.testsrc, &m.noise, 1.0, &|c| {
            c.blend_mode = mode;
            c.transform.opacity = 0.6;
        });
        s.still(&format!("{mode:?} still"), &tl, &a, 0.5, Expect::Drawn);
    }
    // The same modes without an opacity are drawn too (the control).
    let (tl, a) = over(&m.testsrc, &m.noise, SECS, &|c| c.blend_mode = BlendMode::Multiply);
    s.motion("Multiply control", &tl, &a, 30.0, 3, Expect::Any);
    s.finish();
}

// ---- chroma key after a scale ----------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_a_chroma_key_after_a_scale_is_refused_and_shown() {
    let (m, mut s) = (std_media(), Suite::new("key-scale"));
    let key = |similarity: f64, blend: f64| VideoEffect::ChromaKey {
        color: "0x808080".into(),
        similarity,
        blend,
    };
    for scale in [0.5, 0.6, 0.4985, 0.9, 1.3] {
        for (sim, blend) in [(0.25, 0.2), (0.1, 0.0), (0.5, 0.5), (0.00001, 0.0), (1.0, 1.0)] {
            for fg in [&m.noise, &m.ramp, &m.testsrc] {
                let (tl, a) = over(&m.bars, fg, SECS, &|c| {
                    c.transform.scale = scale;
                    c.effects = vec![key(sim, blend)];
                });
                s.motion(
                    &format!("{} scale {scale} sim {sim} blend {blend}", fg.name),
                    &tl,
                    &a,
                    30.0,
                    2,
                    Expect::Any,
                );
            }
        }
    }
    s.finish();
}

// ---- the parameters the fused shader assumes -----------------------------------------------------------

/// `fma.wgsl` takes non-negative finite floats (the sign bit is not looked at): `gblur`'s `nu`, `boundaryscale` and `postscale`
/// must be that for every sigma the plan lets through (0 to 1024). A double-precision cancellation in `set_params` can make `nu` of a
/// tiny sigma negative, which `isnormal` lets through.
#[test]
#[ignore = "pure CPU; fails on the branch: sigmas of about 1.05e-8 to 1.29e-8 leave nu and boundaryscale out of the shader's domain"]
fn z_gblurs_parameters_are_non_negative_for_every_sigma_the_plan_draws() {
    let mut bad = Vec::new();
    let mut sigmas: Vec<f64> = (0..40_000)
        .map(|i| 10f64.powf(-8.0 + 11.0 * f64::from(i) / 40_000.0))
        .collect();
    sigmas.extend([
        1e-9,
        1e-7,
        1e-6,
        1e-5,
        3e-5,
        1e-4,
        2e-4,
        3e-4,
        5e-4,
        1e-3,
        0.002,
        0.005,
        1024.0,
        1023.99,
        f64::from(f32::MIN_POSITIVE),
    ]);
    for sigma in sigmas {
        let Some(g) = kerf_gpu::effects::gaussian(sigma) else {
            continue;
        };
        if !(g.nu >= 0.0 && g.boundary > 0.0 && g.post > 0.0 && g.nu.is_finite() && g.boundary.is_finite() && g.post.is_finite())
        {
            bad.push(format!("sigma {sigma}: nu {} boundary {} post {}", g.nu, g.boundary, g.post));
        }
    }
    eprintln!(
        "Z gaussian: {} sigmas out of the shader domain; first {:?} last {:?}",
        bad.len(),
        bad.first(),
        bad.last()
    );
    assert!(bad.is_empty(), "{} sigmas: {:?}", bad.len(), &bad[..bad.len().min(8)]);
}

/// The sigmas at which `set_params`' double-precision cancellation leaves `nu` and `boundaryscale` out of the shader's domain
/// (1.05e-8 to 1.29e-8 here), and their neighbours, in an export.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blur_at_the_sigmas_where_gblurs_parameters_cancel() {
    let (m, mut s) = (std_media(), Suite::new("blur-tiny"));
    for sigma in [
        1e-9, 5e-9, 1.0e-8, 1.06e-8, 1.1e-8, 1.2e-8, 1.28e-8, 1.3e-8, 1.5e-8, 2e-8, 1e-7, 1e-6, 1e-5,
    ] {
        let (tl, a) = alone(&m.noise, SECS, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
        s.motion(&format!("noise sigma {sigma:e}"), &tl, &a, 30.0, 3, Expect::Any);
    }
    s.finish();
}

/// The recursion's running value decays toward zero over a run of zero samples and goes subnormal (below 2^-126) after roughly 41 samples
/// at sigma 0.5, 71 at 1, 135 at 2: FFmpeg's fused multiply-add gives a subnormal there and then zero. Every other picture of this suite has
/// no run of Y = 0 that long, so this one has: a bright bar or dot on a field of Y = 0 (and one with U = V = 0 as well), swept
/// horizontally, vertically and as a dot.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blur_on_a_field_of_zeros_where_the_recursion_goes_subnormal() {
    let (m, mut s) = (media(), Suite::new("blur-zeros"));
    let src = |lum: &str, chroma: &str| {
        format!("nullsrc=size=640x360:rate=30:duration=2,format=yuv444p,geq=lum='{lum}':{chroma},{UNTAGGED}")
    };
    let flat = "cb=128:cr=128";
    let dead = "cb='if(between(X,300,309)*between(Y,170,179),240,0)':cr='if(between(X,300,309)*between(Y,170,179),240,0)'";
    let vbar = m.x264("z-zero-vbar.mp4", &src("if(between(X,300,309),235,0)", flat), "yuv420p", &[]);
    let hbar = m.x264("z-zero-hbar.mp4", &src("if(between(Y,170,179),235,0)", flat), "yuv420p", &[]);
    let dot = m.x264(
        "z-zero-dot.mp4",
        &src("if(between(X,300,309)*between(Y,170,179),235,0)", flat),
        "yuv420p",
        &[],
    );
    let dead_dot = m.x264(
        "z-zero-dead.mp4",
        &src("if(between(X,300,309)*between(Y,170,179),235,0)", dead),
        "yuv420p",
        &[],
    );
    let pixel = m.x264(
        "z-zero-pixel.mp4",
        &src("if(eq(X,320)*eq(Y,180),255,0)", flat),
        "yuv420p",
        &[],
    );
    for (name, a) in [
        ("vertical bar", &vbar),
        ("horizontal bar", &hbar),
        ("dot", &dot),
        ("dot on dead chroma", &dead_dot),
        ("one pixel", &pixel),
    ] {
        for sigma in [0.3, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 10.0] {
            let (tl, assets) = alone(a, SECS, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.motion(&format!("{name} sigma {sigma}"), &tl, &assets, 30.0, 2, Expect::Any);
        }
    }
    for (name, a) in [("dot", &dot), ("dot on dead chroma", &dead_dot)] {
        for sigma in [0.5, 2.0] {
            let (tl, assets) = alone(a, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.still(&format!("{name} sigma {sigma}"), &tl, &assets, 0.5, Expect::Any);
        }
    }
    s.finish();
}

/// The CPU model (`effects::blur_plane`, `f32::mul_add`, subnormals kept) against FFmpeg's `gblur` on a bright pixel, bar and dot over a
/// field of zeros: if this holds and `z_blur_on_a_field_of_zeros_where_the_recursion_goes_subnormal` does not, the fault is the shader's
/// fused multiply-add (`fma.wgsl`), which has no case for a result below 2^-126.
#[test]
#[ignore = "needs ffmpeg"]
fn z_the_cpu_model_is_gblur_on_a_field_of_zeros() {
    let (w, h) = (640usize, 360usize);
    let mut bad = Vec::new();
    for (name, lit) in [
        (
            "pixel",
            Box::new(|x: usize, y: usize| x == 320 && y == 180) as Box<dyn Fn(usize, usize) -> bool>,
        ),
        ("vertical bar", Box::new(|x, _| (300..310).contains(&x))),
        ("horizontal bar", Box::new(|_, y| (170..180).contains(&y))),
        ("dot", Box::new(|x, y| (300..310).contains(&x) && (170..180).contains(&y))),
    ] {
        let plane: Vec<u8> = (0..w * h).map(|i| if lit(i % w, i / w) { 235 } else { 0 }).collect();
        for sigma in [0.3, 0.5, 0.75, 1.0, 1.5, 2.0, 4.0] {
            let want = ffmpeg_piped(
                &[
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gray",
                    "-s",
                    &format!("{w}x{h}"),
                    "-i",
                    "pipe:0",
                    "-vf",
                    &kerf_core::blur_filter(sigma),
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gray",
                    "pipe:1",
                ],
                &plane,
            );
            let g = kerf_gpu::effects::gaussian(sigma).unwrap();
            let got = kerf_gpu::effects::blur_plane(&plane, w, h, &g);
            let d = Diff::of(&want, &got, w);
            if d.n > 0 {
                bad.push(format!("{name} sigma {sigma}: {}", d.text("Y")));
            }
        }
    }
    eprintln!("Z cpu model on zeros: {} differ; {:?}", bad.len(), bad.first());
    assert!(bad.is_empty(), "{bad:?}");
}

// ---- the probes against pictures they did not use ----------------------------------------------------

/// The translucent round trip's model, as `roundtrip_probe` builds it (`[Y, U, V, A]`), for a picture of any even size.
fn round_trip_model(
    planes: [&[u8]; 3],
    (w, h): (usize, usize),
    opacity: f64,
    forward: kerf_core::YuvMatrix,
    back: kerf_core::YuvMatrix,
) -> [Vec<u8>; 4] {
    let t = kerf_gpu::roundtrip::Yuv2Rgb::new(forward);
    let cw = w / 2;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let c = (y / 2) * cw + x / 2;
            let [r, g, b] = t.rgb(
                i32::from(planes[0][y * w + x]),
                i32::from(planes[1][c]),
                i32::from(planes[2][c]),
            );
            rgb.extend([r as u8, g as u8, b as u8]);
        }
    }
    let [y, u, v] = kerf_gpu::grade::rgb_to_yuv420(&rgb, w, h, back);
    [y, u, v, vec![kerf_core::layer_geometry::ffmpeg_alpha(opacity); w * h]]
}

fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as u8
        })
        .collect()
}

/// The probes decide on small pictures (48x32 for the round trip, 160x96 and some odd ones for the blur). A kernel that differs only on a
/// bigger picture (a vector loop that a 24-sample-wide chroma row never enters, an unaligned tail) would pass them. This holds the model to
/// FFmpeg on pictures of the sizes the harness draws and odd ones, on this machine's CPU: **if a probe says `Match`, every one of them must be
/// exact**. (Run it under a wrapper that adds `-cpuflags ...` as `KERF_FFMPEG` to put it on the other CPUs.)
#[test]
#[ignore = "needs ffmpeg"]
fn z_a_probe_that_says_match_is_not_blind_to_a_bigger_picture() {
    use kerf_core::YuvMatrix;
    let t = kerf_gpu::roundtrip_probe::translucency();
    let b = kerf_gpu::blur_probe::blur();
    eprintln!("Z probes on this machine: translucency {t:?}, blur {b:?}");
    let mut off = Vec::new();
    let mut rt_checked = 0;
    for (w, h) in [
        (48usize, 32usize),
        (64, 64),
        (100, 60),
        (130, 66),
        (202, 100),
        (320, 180),
        (640, 360),
        (642, 362),
        (1280, 720),
    ] {
        let cw = w / 2;
        let ch = h / 2;
        for (seed, opacity, tag, forward) in [
            (1u64, 0.6, None, YuvMatrix::Bt601),
            (2, 0.37, None, YuvMatrix::Bt601),
            (3, 0.999, Some("bt709"), YuvMatrix::Bt709),
            (4, 0.04, Some("bt2020nc"), YuvMatrix::Bt2020),
        ] {
            let (y, u, v) = (
                lcg_bytes(seed, w * h),
                lcg_bytes(seed + 100, cw * ch),
                lcg_bytes(seed + 200, cw * ch),
            );
            let raw: Vec<u8> = [&y, &u, &v].into_iter().flatten().copied().collect();
            let mut graph = Vec::new();
            if let Some(tag) = tag {
                graph.push(format!("setparams=colorspace={tag}"));
            }
            graph.extend([
                "format=yuva420p".to_string(),
                kerf_core::opacity_filter(opacity),
                "format=yuva420p".to_string(),
            ]);
            let graph = graph.join(",");
            let out = ffmpeg_piped(
                &[
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-s",
                    &format!("{w}x{h}"),
                    "-i",
                    "pipe:0",
                    "-vf",
                    &graph,
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuva420p",
                    "pipe:1",
                ],
                &raw,
            );
            let (luma, chroma) = (w * h, cw * ch);
            let got = [
                &out[..luma],
                &out[luma..luma + chroma],
                &out[luma + chroma..luma + 2 * chroma],
                &out[luma + 2 * chroma..],
            ];
            let backs: &[YuvMatrix] = if forward == YuvMatrix::Bt601 {
                &[YuvMatrix::Bt601]
            } else {
                &[YuvMatrix::Bt601, forward]
            };
            let mut best = None;
            for back in backs {
                let want = round_trip_model([&y, &u, &v], (w, h), opacity, forward, *back);
                let apart: Vec<usize> = got
                    .iter()
                    .zip(&want)
                    .map(|(g, w)| g.iter().zip(w).filter(|(a, b)| a != b).count())
                    .collect();
                if apart.iter().all(|n| *n == 0) {
                    best = None;
                    break;
                }
                best = Some(apart);
            }
            rt_checked += 1;
            if let Some(apart) = best {
                off.push(format!(
                    "round trip {w}x{h} at {opacity} {tag:?}: differing samples Y/U/V/A {apart:?}"
                ));
            }
        }
    }
    let rt_off = off.len();
    // The blur: the model against `gblur` on bigger pictures than the probe's, in all three planes.
    let mut blur_checked = 0;
    for (w, h) in [(160usize, 96usize), (200, 100), (333, 187), (640, 360), (1280, 720)] {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        for sigma in [0.5, 1.7, 3.0, 12.0, 60.0, 400.0] {
            for seed in [7u64, 8] {
                let planes = [
                    lcg_bytes(seed * 31 + w as u64, w * h),
                    lcg_bytes(seed * 37 + h as u64, cw * ch),
                    lcg_bytes(seed * 41 + 3, cw * ch),
                ];
                let raw: Vec<u8> = planes.iter().flatten().copied().collect();
                let out = ffmpeg_piped(
                    &[
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "yuv420p",
                        "-s",
                        &format!("{w}x{h}"),
                        "-i",
                        "pipe:0",
                        "-vf",
                        &kerf_core::blur_filter(sigma),
                        "-frames:v",
                        "1",
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "yuv420p",
                        "pipe:1",
                    ],
                    &raw,
                );
                let g = kerf_gpu::effects::gaussian(sigma).unwrap();
                let (luma, rest) = out.split_at(w * h);
                let (u, v) = rest.split_at(cw * ch);
                let mut apart = 0;
                for (got, plane, (pw, ph)) in [
                    (luma, &planes[0], (w, h)),
                    (u, &planes[1], (cw, ch)),
                    (v, &planes[2], (cw, ch)),
                ] {
                    let want = kerf_gpu::effects::blur_plane(plane, pw, ph, &g);
                    apart += got.iter().zip(&want).filter(|(a, b)| a != b).count();
                }
                blur_checked += 1;
                if apart > 0 {
                    off.push(format!("blur {w}x{h} sigma {sigma} seed {seed}: {apart} samples differ"));
                }
            }
        }
    }
    eprintln!(
        "Z blind-spot check: {rt_checked} round-trip pictures ({rt_off} differ from the model), {blur_checked} blur pictures ({} differ)",
        off.len() - rt_off
    );
    if t == kerf_gpu::roundtrip_probe::Translucency::Match && rt_off > 0 {
        panic!(
            "the round-trip probe says Match, and bigger pictures differ: {:?}",
            &off[..rt_off.min(6)]
        );
    }
    if b == kerf_gpu::blur_probe::Blur::Match && off.len() > rt_off {
        panic!(
            "the blur probe says Match, and bigger pictures differ: {:?}",
            &off[rt_off..off.len().min(rt_off + 6)]
        );
    }
    for o in off.iter().take(4) {
        eprintln!("Z (probe did not say Match) {o}");
    }
}

fn needed_total(s: &Suite) -> usize {
    s.needed.values().sum()
}

/// On a machine whose swscale or `gblur` kernels are not the model's (run under `KERF_FFMPEG=<wrapper adding -cpuflags 0>` or
/// `-cpuflags -avx2-avx512`): the caps say so, the plan refuses a translucent layer and a blur, the compositor refuses them when asked,
/// and drawn anyway they are **not** the export's (the refusals are needed). On a machine whose probes match this checks the opposite.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_a_machine_with_other_kernels_is_protected_by_the_probes() {
    let (m, mut s) = (std_media(), Suite::new("other-kernels"));
    let t = kerf_gpu::roundtrip_probe::translucency();
    let b = kerf_gpu::blur_probe::blur();
    let caps = compositor().caps();
    eprintln!(
        "Z probes: translucency {t:?}, blur {b:?}; caps: translucent_approximate {}, blur {}",
        caps.translucent_approximate,
        caps.effects.contains(kerf_core::EffectKinds::BLUR)
    );
    assert_eq!(
        caps.translucent_approximate,
        t != kerf_gpu::roundtrip_probe::Translucency::Match
    );
    assert_eq!(
        caps.effects.contains(kerf_core::EffectKinds::BLUR),
        b == kerf_gpu::blur_probe::Blur::Match
    );
    for o in [0.6, 0.04] {
        let (tl, a) = over(&m.bars, &m.noise, SECS, &opacity(o));
        s.motion(
            &format!("translucent at {o}"),
            &tl,
            &a,
            30.0,
            4,
            if translucency_matches() {
                Expect::Drawn
            } else {
                Expect::Refused
            },
        );
    }
    for sigma in [3.0, 40.0] {
        let (tl, a) = alone(&m.noise, SECS, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
        s.motion(
            &format!("blur {sigma}"),
            &tl,
            &a,
            30.0,
            4,
            if blur_matches() { Expect::Drawn } else { Expect::Refused },
        );
    }
    let (tl, a) = alone(&m.noise, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma: 0.0 }]);
    s.motion("blur sigma 0 stays drawn", &tl, &a, 30.0, 4, Expect::Drawn);
    // Unknown refuses too: a compositor pinned to it.
    let unknown = Compositor::new(gpu())
        .unwrap()
        .with_translucency(kerf_gpu::roundtrip_probe::Translucency::Unknown)
        .with_blur(kerf_gpu::blur_probe::Blur::Unknown);
    let (tl, a) = over(&m.bars, &m.noise, SECS, &opacity(0.6));
    let plan = RenderPlan::at(&tl, &a, &ExportOptions::default(), 0.1, policy()).unwrap();
    let why: Vec<String> = plan
        .reasons(&unknown.caps(), plan.size(u32::MAX))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        why.iter().any(|w| w.contains("opacity below 1")),
        "Unknown must refuse a translucent layer: {why:?}"
    );
    let layers = kerf_gpu::decode_layers(&plan.layers).unwrap();
    assert!(matches!(
        unknown.composite_yuv(&plan, &layers, plan.size(u32::MAX)),
        Err(GpuError::Unsupported(_))
    ));
    let (tl, a) = alone(&m.noise, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma: 3.0 }]);
    let plan = RenderPlan::at(&tl, &a, &ExportOptions::default(), 0.1, policy()).unwrap();
    let why: Vec<String> = plan
        .reasons(&unknown.caps(), plan.size(u32::MAX))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        why.iter().any(|w| w.contains("a blur")),
        "Unknown must refuse a blur: {why:?}"
    );
    let layers = kerf_gpu::decode_layers(&plan.layers).unwrap();
    assert!(matches!(
        unknown.composite_yuv(&plan, &layers, plan.size(u32::MAX)),
        Err(GpuError::Unsupported(_))
    ));
    if !(translucency_matches() && blur_matches()) {
        eprintln!("Z other kernels: {} refused frames are exact when drawn anyway (a one-level difference on a few samples is luck, not a wide refusal)", s.exact_refused);
        assert!(
            needed_total(&s) > 0,
            "the pessimistic probes refused frames that drawn anyway are all exact: the probe is wider than it needs to be here"
        );
    }
    s.finish();
}

// ---- smoke cases for the mutation checks ---------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_smoke_translucent() {
    let (m, mut s) = (std_media(), Suite::new("smoke-translucent"));
    for o in [0.6, 0.04, 126.5 / 255.0, 127.5 / 255.0, 2.5 / 255.0] {
        let (tl, a) = over(&m.bars, &m.noise, SECS, &opacity(o));
        s.motion(&format!("noise over bars at {o}"), &tl, &a, 30.0, 4, Expect::Drawn);
    }
    assert!(s.drawn > 0, "nothing was drawn: the plan refuses a translucent layer");
    s.finish();
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_smoke_blur() {
    let (m, mut s) = (std_media(), Suite::new("smoke-blur"));
    for sigma in [1.0, 9.0, 250.0] {
        let (tl, a) = alone(&m.noise, SECS, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
        s.motion(&format!("noise sigma {sigma}"), &tl, &a, 30.0, 3, Expect::Drawn);
    }
    assert!(s.drawn > 0, "nothing was drawn: the plan refuses a blur");
    s.finish();
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_smoke_display() {
    let (m, mut s) = (std_media(), Suite::new("smoke-display"));
    for (w, h) in [(640u32, 360u32), (202, 100)] {
        let (tl, a) = alone(&m.noise, 1.0, delivery(w, h, Fit::Cover), &|_| {});
        s.still(&format!("noise {w}x{h}"), &tl, &a, 0.5, Expect::Drawn);
    }
    s.finish();
}

// ======================================================================================================
// Round 2: the pictures the blur has to survive, the key after a scale, every RGB entry point, the alpha ramp.
// ======================================================================================================

/// A clip of `frames` copies of one raw frame (`pix`, `w` x `h`), lossless, so a crafted picture reaches the compositor and FFmpeg
/// byte for byte.
fn raw_clip(name: &str, w: usize, h: usize, pix: &str, frame: &[u8], frames: usize) -> Asset {
    let m = media();
    let mut cache = m.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(a) = cache.get(name) {
        return a.clone();
    }
    let p = m.dir.join(name);
    let part = m.dir.join(format!("{}.{}.part.mkv", slug(name), std::process::id()));
    let data: Vec<u8> = frame.iter().copied().cycle().take(frame.len() * frames).collect();
    // (A 10-bit picture goes through lossless x264 with its range recorded: raw video does not carry 10-bit in Matroska, and FFV1 does not
    // record a range, which FFmpeg 9 treats as a picture whose values it keeps raw.)
    let codec: &[&str] = if pix.contains("10") {
        &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-color_range", "tv"]
    } else {
        &["-c:v", "rawvideo"]
    };
    let size = format!("{w}x{h}");
    let mut args = vec![
        "-f",
        "rawvideo",
        "-framerate",
        "30",
        "-pix_fmt",
        pix,
        "-s",
        &size,
        "-i",
        "pipe:0",
    ];
    args.extend_from_slice(codec);
    args.extend_from_slice(&["-pix_fmt", pix, part.to_str().unwrap()]);
    ffmpeg_piped(&args, &data);
    std::fs::rename(&part, &p).expect("move the clip into place");
    let a = probed(&p);
    cache.insert(name.to_string(), a.clone());
    a
}

fn noise_bytes(seed: u64, n: usize) -> Vec<u8> {
    lcg_bytes(seed, n)
}

/// A plane of exact-zero runs of every Fibonacci length between bright samples, along rows (`rows`) or columns.
fn zero_run_plane(w: usize, h: usize, rows: bool, salt: usize) -> Vec<u8> {
    let fib = [1usize, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233];
    let mut p = vec![0u8; w * h];
    let (lines, len) = if rows { (h, w) } else { (w, h) };
    for line in 0..lines {
        let run = fib[(line + salt) % fib.len()];
        let phase = (line * 7 + salt) % run;
        for i in 0..len {
            if (i + phase) % (run + 1) == 0 {
                let v = 200 + ((i * 31 + line * 17 + salt) % 56) as u8;
                if rows {
                    p[line * w + i] = v;
                } else {
                    p[i * w + line] = v;
                }
            }
        }
    }
    p
}

fn dots_plane(w: usize, h: usize, v: u8) -> Vec<u8> {
    let mut p = vec![0u8; w * h];
    for (x, y) in [
        (0, 0),
        (w - 1, 0),
        (0, h - 1),
        (w - 1, h - 1),
        (w / 2, h / 2),
        (0, h / 2),
        (w - 1, h / 2),
        (w / 2, 0),
        (w / 2, h - 1),
    ] {
        p[y * w + x] = v;
    }
    p
}

fn yuv(y: Vec<u8>, u: Vec<u8>, v: Vec<u8>) -> Vec<u8> {
    let mut f = y;
    f.extend(u);
    f.extend(v);
    f
}

/// Crafted pictures: `(name, frame, w, h)` as yuv420p.
fn crafted_pictures() -> Vec<(String, Vec<u8>, usize, usize)> {
    let mut out = Vec::new();
    for (w, h) in [(320usize, 180usize), (202, 100)] {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        out.push((
            format!("zero runs along rows {w}x{h}"),
            yuv(
                zero_run_plane(w, h, true, 0),
                zero_run_plane(cw, ch, true, 3),
                zero_run_plane(cw, ch, true, 5),
            ),
            w,
            h,
        ));
        out.push((
            format!("zero runs along columns {w}x{h}"),
            yuv(
                zero_run_plane(w, h, false, 1),
                zero_run_plane(cw, ch, false, 4),
                zero_run_plane(cw, ch, false, 2),
            ),
            w,
            h,
        ));
        out.push((
            format!("dots at the borders {w}x{h}"),
            yuv(dots_plane(w, h, 255), dots_plane(cw, ch, 255), dots_plane(cw, ch, 240)),
            w,
            h,
        ));
        out.push((
            format!("noise with U 0 and V 255 {w}x{h}"),
            yuv(noise_bytes(1, w * h), vec![0; cw * ch], vec![255; cw * ch]),
            w,
            h,
        ));
        out.push((
            format!("noise with U 255 and V 0 {w}x{h}"),
            yuv(noise_bytes(2, w * h), vec![255; cw * ch], vec![0; cw * ch]),
            w,
            h,
        ));
        out.push((
            format!("noise with U 0 and V 0 {w}x{h}"),
            yuv(noise_bytes(3, w * h), vec![0; cw * ch], vec![0; cw * ch]),
            w,
            h,
        ));
        out.push((
            format!("Y 0 with noise chroma {w}x{h}"),
            yuv(vec![0; w * h], noise_bytes(4, cw * ch), noise_bytes(5, cw * ch)),
            w,
            h,
        ));
        out.push((
            format!("Y 255 chroma 255 {w}x{h}"),
            yuv(vec![255; w * h], vec![255; cw * ch], vec![255; cw * ch]),
            w,
            h,
        ));
        let half = |n: usize, cols: usize, seed: u64| -> Vec<u8> {
            let noise = noise_bytes(seed, n);
            noise
                .into_iter()
                .enumerate()
                .map(|(i, v)| if i % cols < cols / 2 { 0 } else { v })
                .collect()
        };
        out.push((
            format!("left half zero {w}x{h}"),
            yuv(half(w * h, w, 6), half(cw * ch, cw, 7), half(cw * ch, cw, 8)),
            w,
            h,
        ));
    }
    out
}

const CRAFTED_SIGMAS: [f64; 9] = [0.3, 0.5, 1.0, 2.0, 4.0, 9.0, 40.0, 250.0, 1024.0];

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_blur_on_crafted_pictures_in_a_still() {
    let mut s = Suite::new("blur-crafted-still");
    for (name, frame, w, h) in crafted_pictures() {
        let a = raw_clip(&format!("z2-{}.mkv", slug(&name)), w, h, "yuv420p", &frame, 30);
        for sigma in CRAFTED_SIGMAS {
            let (tl, assets) = alone(&a, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.still(&format!("{name} sigma {sigma}"), &tl, &assets, 0.5, Expect::Any);
        }
    }
    s.finish();
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_blur_on_crafted_pictures_in_an_export() {
    let mut s = Suite::new("blur-crafted-export");
    for (name, frame, w, h) in crafted_pictures() {
        let a = raw_clip(&format!("z2-{}.mkv", slug(&name)), w, h, "yuv420p", &frame, 30);
        for sigma in [0.5, 2.0, 9.0, 250.0] {
            let (tl, assets) = alone(&a, 0.2, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.motion(&format!("{name} sigma {sigma}"), &tl, &assets, 30.0, 2, Expect::Any);
        }
    }
    s.finish();
}

/// 10-bit footage: the same pictures with their samples widened to 10 bits (the low bits are not zero), blurred.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_blur_on_ten_bit_crafted_pictures() {
    let mut s = Suite::new("blur-crafted-10bit");
    for (name, frame, w, h) in crafted_pictures()
        .into_iter()
        .filter(|(n, ..)| n.contains("202x100") || n.contains("rows 320"))
    {
        let wide: Vec<u8> = frame
            .iter()
            .flat_map(|v| {
                let x = if *v == 0 {
                    0u16
                } else {
                    (u16::from(*v) << 2) | u16::from(*v & 3)
                };
                x.to_le_bytes()
            })
            .collect();
        let a = raw_clip(&format!("z2-10-{}.mkv", slug(&name)), w, h, "yuv420p10le", &wide, 30);
        for sigma in [0.5, 2.0, 9.0, 250.0] {
            let (tl, assets) = alone(&a, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.still(&format!("{name} sigma {sigma}"), &tl, &assets, 0.5, Expect::Any);
            let (tl, assets) = alone(&a, 0.2, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.motion(&format!("{name} sigma {sigma}"), &tl, &assets, 30.0, 2, Expect::Any);
        }
    }
    s.finish();
}

/// The float sigmas on both sides of each end of the refused run, and a spread, through the whole pipeline.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_blur_at_the_floats_either_side_of_the_refused_run() {
    let mut s = Suite::new("blur-edges");
    let m = std_media();
    let refused = |x: f32| kerf_gpu::effects::gaussian(f64::from(x)).is_none();
    let mut edges = Vec::new();
    let mut prev = false;
    for b in 1.0e-8f32.to_bits()..=1.3e-8f32.to_bits() {
        let r = refused(f32::from_bits(b));
        if r != prev {
            edges.push(b);
        }
        prev = r;
    }
    assert_eq!(edges.len(), 2, "one refused run: {edges:?}");
    let mut sigmas: Vec<f64> = Vec::new();
    for e in edges {
        for d in [-2i64, -1, 0, 1, 2] {
            sigmas.push(f64::from(f32::from_bits((i64::from(e) + d) as u32)));
        }
    }
    for sigma in sigmas {
        let (tl, a) = alone(&m.noise, SECS, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
        s.motion(&format!("noise sigma {sigma:e}"), &tl, &a, 30.0, 2, Expect::Any);
        let (tl, a) = alone(&m.noise, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
        s.still(&format!("noise sigma {sigma:e}"), &tl, &a, 0.5, Expect::Any);
    }
    s.finish();
}

// ---- the chroma key after a scale -----------------------------------------------------------------------

fn chroma_key(color: &str, similarity: f64, blend: f64) -> VideoEffect {
    VideoEffect::ChromaKey {
        color: color.into(),
        similarity,
        blend,
    }
}

/// A picture whose chroma runs smoothly through the key colour, so the key's alpha takes every level (a sample off by one in a chroma
/// plane moves it).
fn chroma_gradient() -> Asset {
    media().x264(
        "z2-chroma-gradient.mp4",
        &format!(
            "nullsrc=size=640x360:rate=30:duration=2,format=yuv444p,geq=lum='40+X*160/W':cb='128+(X-W/2)*140/W+sin(Y/9)*6':cr='128+(Y-H/2)*140/H+cos(X/11)*6',{UNTAGGED}"
        ),
        "yuv420p",
        &[],
    )
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_chroma_key_after_a_scale_is_exact_where_it_is_drawn() {
    let (m, mut s) = (std_media(), Suite::new("key-lift"));
    let grad = chroma_gradient();
    let keys = [
        chroma_key("0x808080", 0.25, 0.2),
        chroma_key("0x808080", 0.1, 0.0),
        chroma_key("0x808080", 0.5, 0.5),
        chroma_key("0x808080", 0.00001, 0.0),
        chroma_key("0x808080", 1.0, 1.0),
        chroma_key("0x00ff00", 0.3, 0.1),
        chroma_key("0xff00ff", 0.15, 0.15),
        chroma_key("0x7f7f81", 0.05, 0.02),
    ];
    // Scales around 1, odd ones and exact ones.
    for scale in [
        0.9999, 0.999, 0.99, 1.001, 1.01, 0.5, 0.5001, 0.7, 0.3333, 0.25, 0.1, 1.5, 2.0, 0.55, 0.4985,
    ] {
        for (i, k) in keys.iter().enumerate() {
            if i % 3 != (scale * 1000.0) as usize % 3 && i > 2 {
                continue;
            }
            for fg in [&grad, &m.noise] {
                let (tl, a) = over(&m.bars, fg, SECS, &|c| {
                    c.transform.scale = scale;
                    c.effects = vec![k.clone()];
                });
                s.motion(&format!("{} scale {scale} key {i}", fg.name), &tl, &a, 30.0, 2, Expect::Any);
            }
        }
    }
    s.finish();
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_chroma_key_after_a_scale_with_every_effect_and_company() {
    let (m, mut s) = (std_media(), Suite::new("key-lift-company"));
    let grad = chroma_gradient();
    let key = || chroma_key("0x808080", 0.3, 0.15);
    let before: Vec<(&str, Vec<VideoEffect>)> = vec![
        ("blur 2", vec![VideoEffect::Blur { sigma: 2.0 }]),
        ("blur 40", vec![VideoEffect::Blur { sigma: 40.0 }]),
        ("sharpen 0.5", vec![VideoEffect::Sharpen { amount: 0.5 }]),
        ("sharpen 3", vec![VideoEffect::Sharpen { amount: 3.0 }]),
        ("sharpen -1", vec![VideoEffect::Sharpen { amount: -1.0 }]),
        ("invert", vec![VideoEffect::Invert]),
        ("grayscale", vec![VideoEffect::Grayscale]),
        ("vignette", vec![VideoEffect::Vignette]),
        (
            "blur sharpen invert grayscale",
            vec![
                VideoEffect::Blur { sigma: 3.0 },
                VideoEffect::Sharpen { amount: 0.4 },
                VideoEffect::Invert,
                VideoEffect::Grayscale,
            ],
        ),
    ];
    for scale in [0.6, 0.9] {
        for (name, fx) in &before {
            for fg in [&grad, &m.noise] {
                let (tl, a) = over(&m.bars, fg, SECS, &|c| {
                    c.transform.scale = scale;
                    c.effects = fx.iter().cloned().chain([key()]).collect();
                });
                s.motion(
                    &format!("{} {name} then key at {scale}", fg.name),
                    &tl,
                    &a,
                    30.0,
                    2,
                    Expect::Any,
                );
            }
        }
    }
    // The key beside a grade, an opacity, a fade, a crop, a rotation, a position, a mask and a keyed scale.
    let mut cases: Vec<(&str, Box<dyn Fn(&mut Clip)>)> = Vec::new();
    cases.push((
        "graded",
        Box::new(|c| {
            c.color = Color {
                gamma: 1.4,
                saturation: 1.2,
                ..Color::default()
            };
        }),
    ));
    cases.push(("opacity 0.6", Box::new(|c| c.transform.opacity = 0.6)));
    cases.push((
        "fade in",
        Box::new(|c| {
            c.fade_in = 0.3;
        }),
    ));
    cases.push((
        "crop",
        Box::new(|c| {
            c.transform.crop_left = 0.1;
            c.transform.crop_top = 0.07;
        }),
    ));
    cases.push(("rotation 15", Box::new(|c| c.transform.rotation = 15.0)));
    cases.push((
        "position",
        Box::new(|c| {
            c.transform.pos_x = 0.2;
            c.transform.pos_y = -0.1;
        }),
    ));
    cases.push(("mask", Box::new(|c| c.mask = Some(mask_of(MaskShape::Ellipse)))));
    cases.push((
        "keyed scale",
        Box::new(|c| c.set_property_keys(Property::Scale, vec![eased(0.0, 0.5, Easing::EaseInOut), key_at(0.3, 0.9)])),
    ));
    cases.push((
        "keyed position",
        Box::new(|c| c.set_property_keys(Property::PosX, vec![key_at(0.0, -0.2), key_at(0.3, 0.2)])),
    ));
    cases.push((
        "keyed opacity",
        Box::new(|c| c.set_property_keys(Property::Opacity, vec![key_at(0.0, 0.9), key_at(0.3, 0.2)])),
    ));
    cases.push(("speed 2", Box::new(|c| c.speed = 2.0)));
    for (name, edit) in &cases {
        for fg in [&grad, &m.noise] {
            let (tl, a) = over(&m.bars, fg, 0.6, &|c| {
                c.transform.scale = 0.6;
                c.effects = vec![key()];
                edit(c);
            });
            s.motion(&format!("{} key with {name}", fg.name), &tl, &a, 30.0, 4, Expect::Any);
        }
    }
    s.finish();
}

fn key_at(time: f64, value: f64) -> PropertyKey {
    PropertyKey::new(time, value)
}

/// Deliveries that scale (Contain and Cover, odd sizes) with a key, in a still and an export.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_chroma_key_after_a_delivery_scale() {
    let (m, mut s) = (std_media(), Suite::new("key-lift-delivery"));
    let grad = chroma_gradient();
    let key = || chroma_key("0x808080", 0.3, 0.15);
    for (w, h) in [(320u32, 180u32), (202, 100), (240, 240), (90, 160), (642, 362), (480, 270)] {
        for fit in [Fit::Contain, Fit::Cover] {
            for fg in [&grad, &m.noise] {
                for scale in [1.0, 0.8] {
                    let (tl, a) = alone(fg, 1.0, delivery(w, h, fit), &|c| {
                        c.effects = vec![key()];
                        c.transform.scale = scale;
                    });
                    s.still(
                        &format!("{} {w}x{h} {fit:?} scale {scale}", fg.name),
                        &tl,
                        &a,
                        0.5,
                        Expect::Any,
                    );
                }
            }
        }
    }
    for (w, h) in [(320u32, 180u32), (202, 100)] {
        for fit in [Fit::Contain, Fit::Cover] {
            let (tl, a) = over(&m.bars, &grad, SECS, &|c| c.effects = vec![key()]);
            let mut tl = tl;
            tl.format = delivery(w, h, fit);
            s.motion(&format!("{w}x{h} {fit:?}"), &tl, &a, 30.0, 2, Expect::Any);
        }
    }
    s.finish();
}

/// Every source format the decode reduces, a key after the fit scale.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_chroma_key_after_a_scale_in_every_format() {
    let mut s = Suite::new("key-lift-format");
    for name in [
        "yuv420p10le",
        "yuv422p10le",
        "yuv444p10le",
        "yuv420p10le-pc",
        "yuvj420p",
        "yuvj444p",
        "yuv420p-pc",
        "yuv422p",
        "yuv444p",
        "yuv440p",
        "bgr0",
        "gray",
        "yuva420p",
        "bt709",
        "bt2020",
        "smpte170m",
    ] {
        let fg = noise_format(name);
        for scale in [0.6, 0.999] {
            let (tl, a) = alone(&fg, SECS, None, &|c| {
                c.transform.scale = scale;
                c.effects = vec![chroma_key("0x808080", 0.3, 0.15)];
            });
            s.motion(&format!("{name} scale {scale}"), &tl, &a, 30.0, 2, Expect::Any);
        }
        let (tl, a) = alone(&fg, 1.0, delivery(202, 100, Fit::Cover), &|c| {
            c.effects = vec![chroma_key("0x808080", 0.3, 0.15)];
        });
        s.still(&format!("{name} cover 202x100"), &tl, &a, 0.5, Expect::Any);
    }
    s.finish();
}

// ---- the alpha rule of a dissolve ------------------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_the_alpha_ramp_of_a_dissolve_and_a_fade_over_many_opacities_and_rates() {
    let (m, mut s) = (std_media(), Suite::new("alpha-ramp"));
    for o in [
        0.004,
        0.02,
        0.1,
        0.2,
        0.37,
        0.5,
        0.63,
        0.8,
        0.9,
        0.99,
        126.5 / 255.0,
        127.5 / 255.0,
    ] {
        let (tl, a) = over(&m.bars, &m.noise, 0.8, &|c| {
            c.transform.opacity = o;
            c.fade_in = 0.5;
            c.fade_out = 0.4;
        });
        s.motion(&format!("fade at {o}"), &tl, &a, 30.0, 24, Expect::Any);
        let mut out = clip(&m.testsrc, 0.0, 0.8, 0.0);
        out.transform.opacity = o;
        let mut inc = clip(&m.noise, 0.0, 0.8, 0.6);
        inc.transform.opacity = 1.0 - o / 2.0;
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        let tl = timeline(vec![vec![out, inc]], None);
        s.motion(
            &format!("crossfade outgoing at {o}"),
            &tl,
            &[m.testsrc.clone(), m.noise.clone()],
            30.0,
            36,
            Expect::Any,
        );
    }
    for fps in [24.0, 25.0, 29.97, 50.0, 60.0] {
        let (tl, a) = over(&m.bars, &m.noise, 0.8, &|c| {
            c.transform.opacity = 0.6;
            c.fade_in = 0.5;
        });
        s.motion(&format!("fade at {fps} fps"), &tl, &a, fps, 16, Expect::Any);
    }
    s.finish();
}

// ---- every way out as RGB --------------------------------------------------------------------------------------------

/// With the display conversion not a match (`Differs`, `Unknown`) every entry point that hands RGB out answers `Unsupported` **before it
/// decodes** (the clip is gone from the disk when they are asked), and `Output::Yuv` is untouched; with `Match` the RGB comes.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_every_entry_point_that_hands_out_rgb_refuses_before_the_decode() {
    use kerf_gpu::display_probe::Display;
    let (w, h) = (320usize, 180usize);
    let frame = yuv(noise_bytes(77, w * h), noise_bytes(78, w * h / 4), noise_bytes(79, w * h / 4));
    let a = raw_clip("z2-entry-private.mkv", w, h, "yuv420p", &frame, 30);
    let (tl, assets) = alone(&a, 1.0, None, &|_| {});
    let opts = ExportOptions::default();
    let plan = RenderPlan::at(&tl, &assets, &opts, 0.5, policy()).expect("plan");
    let size = plan.size(u32::MAX);
    prime();
    let layers = kerf_gpu::decode_layers(&plan.layers).expect("decode while the clip is there");
    let shared: Vec<Option<Arc<YuvFrame>>> = layers.iter().cloned().map(|l| l.map(Arc::new)).collect();
    let counts: Vec<Option<u64>> = vec![Some(0); shared.len()];
    let planner = Planner::new(&tl, &assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
    let motion_plan = planner.at_frame(1).expect("a motion plan");
    let path = assets[0].path.clone();
    let gone = format!("{path}.gone");
    let source = FrameSource::new(FrameSourceConfig::default());
    let hint = kerf_gpu::Hint::Scrub;
    let mut seen = Vec::new();
    for pinned in [
        Display::Differs("a CPU with another kernel".into()),
        Display::Unknown,
        Display::Match,
    ] {
        let c = Compositor::new(gpu()).expect("a compositor").with_display(pinned.clone());
        let mut motion = MotionFrames::new(frame_source());
        std::fs::rename(&path, &gone).expect("hide the clip");
        let results: Vec<(&str, Result<(), GpuError>)> = vec![
            ("composite", c.composite(&plan, &layers, size).map(|_| ())),
            ("composite_shared", c.composite_shared(&plan, &shared, size).map(|_| ())),
            (
                "composite_counted",
                c.composite_counted(&plan, &shared, &counts, size).map(|_| ()),
            ),
            ("render_plan", c.render_plan(&plan, size).map(|_| ())),
            ("render_plan_with", c.render_plan_with(&plan, size, &source, hint).map(|_| ())),
            (
                "render_plan_texture_with",
                c.render_plan_texture_with(&plan, size, &source, hint).map(|_| ()),
            ),
            (
                "render_draft_texture_with",
                c.render_draft_texture_with(&plan, size, &source, hint).map(|_| ()),
            ),
            (
                "render_motion_texture",
                c.render_motion_texture(&motion_plan, size, &mut motion).map(|_| ()),
            ),
        ];
        std::fs::rename(&gone, &path).expect("put the clip back");
        let yuv_ok = c.composite_yuv(&plan, &layers, size).is_ok();
        for (name, r) in results {
            match (&pinned, r) {
                (Display::Match, Ok(())) => {}
                (Display::Match, Err(e)) if !e.to_string().contains("yuv420p -> rgb24") => {}
                (Display::Match, Err(e)) => seen.push(format!("{name} with Match: refused for the display: {e}")),
                (_, Err(GpuError::Unsupported(msg))) if msg.contains("yuv420p -> rgb24") => {}
                (_, Err(e)) => seen.push(format!("{name} with {pinned:?}: {e:?} (not the display refusal)")),
                (_, Ok(())) => seen.push(format!("{name} with {pinned:?}: RGB handed out")),
            }
        }
        if !yuv_ok {
            seen.push(format!(
                "composite_yuv with {pinned:?} was refused: the YUV planes must be unaffected"
            ));
        }
    }
    assert!(seen.is_empty(), "{seen:#?}");
}

// ---- planes too small for FFmpeg's own kernel ---------------------------------------------------------------------

/// A blur on layers scaled down to a handful of pixels: the plane `gblur` runs on has a side of 1 to a dozen samples, where FFmpeg's AVX2
/// kernel crashes (a side of 1) or answers differently from run to run. A plan that draws such a frame draws one FFmpeg has no stable answer
/// for: listed as `NO ANSWER` and failed, since the frame is not provably exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_blur_on_a_layer_scaled_to_a_few_pixels_is_drawn_only_where_ffmpeg_has_an_answer() {
    let mut s = Suite::new("blur-tiny-layer");
    let m = std_media();
    let mut no_answer = Vec::new();
    for scale in [0.0016, 0.002, 0.003, 0.004, 0.006, 0.0078, 0.01, 0.0125, 0.02, 0.03, 0.05] {
        for sigma in [0.5, 3.0] {
            let (tl, a) = over(&m.bars, &m.noise, 1.0, &|c| {
                c.transform.scale = scale;
                c.effects = vec![VideoEffect::Blur { sigma }];
            });
            let opts = ExportOptions::default();
            let plan = RenderPlan::at(&tl, &a, &opts, 0.5, policy()).expect("plan");
            let size = plan.size(u32::MAX);
            let why: Vec<String> = plan
                .reasons(&compositor().caps(), size)
                .iter()
                .map(ToString::to_string)
                .collect();
            let drawn = why.is_empty();
            let mut answers = BTreeSet::new();
            let mut crashes = 0;
            for _ in 0..6 {
                match kerf_core::timeline_frame_yuv_cancellable(&tl, &a, &opts, 0.5, u32::MAX, &|| false) {
                    Ok((_, _, yuv)) => {
                        answers.insert(yuv);
                    }
                    Err(_) => crashes += 1,
                }
            }
            let picture = plan.layers.last().map(|l| (l.stream.width, l.stream.height));
            let state = if crashes > 0 {
                format!("FFmpeg's still fails {crashes} of 6 times")
            } else if answers.len() > 1 {
                format!("FFmpeg gives {} different stills in 6 runs", answers.len())
            } else {
                "FFmpeg's still is stable".to_string()
            };
            eprintln!(
                "Z tiny layer scale {scale} sigma {sigma} (source {picture:?}): plan {}; {state}",
                if drawn {
                    "DRAWS".to_string()
                } else {
                    format!(
                        "refuses {:?}",
                        why.iter()
                            .map(|w| w.chars().take(110).collect::<String>())
                            .collect::<Vec<_>>()
                    )
                }
            );
            if drawn && (crashes > 0 || answers.len() > 1) {
                no_answer.push(format!("scale {scale} sigma {sigma}: {state}"));
            }
            if drawn && crashes == 0 && answers.len() == 1 {
                s.still(&format!("scale {scale} sigma {sigma}"), &tl, &a, 0.5, Expect::Any);
            }
        }
    }
    for n in &no_answer {
        eprintln!("Z NO ANSWER {n}");
        s.fail(format!(
            "NO ANSWER: the plan draws a blurred layer FFmpeg has no stable answer for: {n}"
        ));
    }
    s.finish();
}

/// Pictures a few pixels across, unscaled (the canvas is the clip's size): the plane `gblur` runs on has a side of 2 to 40 samples. FFmpeg's
/// AVX2 kernel is not the filter on every such plane (its scratch buffer is sized from the luma plane; the kernel works a block of columns
/// or rows past what a narrow plane has). A frame the plan draws must be FFmpeg's to the sample, whatever the picture's size.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_blur_on_a_tiny_unscaled_picture() {
    let mut s = Suite::new("blur-tiny-picture");
    let sizes = [
        (2usize, 2usize),
        (2, 4),
        (2, 6),
        (2, 16),
        (2, 40),
        (4, 2),
        (4, 4),
        (4, 6),
        (6, 2),
        (6, 4),
        (6, 6),
        (8, 2),
        (8, 4),
        (8, 6),
        (8, 8),
        (12, 2),
        (12, 4),
        (12, 6),
        (12, 8),
        (16, 2),
        (16, 4),
        (16, 6),
        (16, 8),
        (16, 16),
        (32, 2),
        (32, 4),
        (32, 6),
        (40, 8),
        (40, 40),
    ];
    for (w, h) in sizes {
        let (cw, ch) = (w / 2, h / 2);
        let frame = yuv(
            noise_bytes((w * 97 + h) as u64, w * h),
            noise_bytes((w * 97 + h + 1) as u64, cw * ch),
            noise_bytes((w * 97 + h + 2) as u64, cw * ch),
        );
        let a = raw_clip(&format!("z2-tiny-{w}x{h}.mkv"), w, h, "yuv420p", &frame, 30);
        for sigma in [0.5, 3.0] {
            let (tl, assets) = alone(&a, 1.0, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.still(&format!("{w}x{h} sigma {sigma}"), &tl, &assets, 0.5, Expect::Any);
        }
    }
    s.finish();
}

/// The same pictures through an export, whose filter graph runs with the machine's threads (a still runs with one): `gblur` is sliced
/// across them, and its AVX2 kernel answers differently from the single-slice filter on a plane with few rows per slice. The thread
/// count is the machine's (here 4); pictures up to a handful of rows tall are where it shows.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_blur_on_a_small_picture_in_an_export() {
    let mut s = Suite::new("blur-small-export");
    let sizes = [
        (16usize, 4usize),
        (16, 6),
        (16, 8),
        (16, 10),
        (16, 12),
        (16, 14),
        (16, 16),
        (16, 20),
        (16, 24),
        (32, 6),
        (32, 8),
        (32, 12),
        (64, 4),
        (64, 6),
        (64, 8),
        (64, 12),
        (64, 16),
        (64, 24),
        (64, 32),
        (128, 6),
        (128, 8),
        (128, 16),
    ];
    for (w, h) in sizes {
        let (cw, ch) = (w / 2, h / 2);
        let frame = yuv(
            noise_bytes((w * 91 + h) as u64, w * h),
            noise_bytes((w * 91 + h + 1) as u64, cw * ch),
            noise_bytes((w * 91 + h + 2) as u64, cw * ch),
        );
        let a = raw_clip(&format!("z2-small-{w}x{h}.mkv"), w, h, "yuv420p", &frame, 30);
        for sigma in [0.5, 3.0] {
            let (tl, assets) = alone(&a, 0.2, None, &|c| c.effects = vec![VideoEffect::Blur { sigma }]);
            s.motion(&format!("{w}x{h} sigma {sigma}"), &tl, &assets, 30.0, 2, Expect::Any);
        }
    }
    s.finish();
}

/// Noise tagged `tag` (`setparams=colorspace=`), lossless; `None` where this FFmpeg cannot make it.
fn tagged_clip(name: &str, tag: &str) -> Option<Asset> {
    let m = media();
    let mut cache = m.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(a) = cache.get(name) {
        return Some(a.clone());
    }
    let p = m.dir.join(name);
    let part = m.dir.join(format!("{}.{}.part.mkv", slug(name), std::process::id()));
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-f", "lavfi", "-i"])
        .arg(noise_src(640, 360, 2, &format!("setparams=colorspace={tag}")))
        .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
        .arg(&part)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        return None;
    }
    std::fs::rename(&part, &p).ok()?;
    let a = Project::probe_asset(&p).ok()?;
    cache.insert(name.to_string(), a.clone());
    Some(a)
}

/// Every matrix tag a source can carry, as the composite's tag (a still of the footage alone, and over a differently tagged background):
/// the planes, the PNG and the display RGB are FFmpeg's wherever the plan draws the frame, whatever coefficients the tag names.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_the_display_and_the_composite_under_every_matrix_tag() {
    let (m, mut s) = (std_media(), Suite::new("tags"));
    for tag in [
        "bt709",
        "fcc",
        "bt470bg",
        "smpte170m",
        "smpte240m",
        "bt2020nc",
        "bt2020c",
        "unknown",
        "chroma-derived-nc",
        "chroma-derived-c",
        "ycgco",
        "gbr",
        "ictcp",
    ] {
        let name = format!("z2-tag-{tag}.mkv");
        let Some(fg) = tagged_clip(&name, tag) else {
            eprintln!("Z tags: this FFmpeg cannot make footage tagged {tag}: skipped");
            continue;
        };
        for (w, h) in [(640u32, 360u32), (202, 100)] {
            let (tl, a) = alone(&fg, 1.0, delivery(w, h, Fit::Cover), &|_| {});
            s.still(&format!("{tag} alone {w}x{h}"), &tl, &a, 0.5, Expect::Any);
        }
        let (tl, a) = over(&m.bars, &fg, 1.0, &|c| c.transform.scale = 0.7);
        s.still(&format!("{tag} over untagged bars"), &tl, &a, 0.5, Expect::Any);
        let (tl, a) = over(&fg, &m.testsrc, 1.0, &|c| c.transform.scale = 0.7);
        s.still(&format!("untagged over {tag}"), &tl, &a, 0.5, Expect::Any);
        let (tl, a) = alone(&fg, SECS, None, &|_| {});
        s.motion(&format!("{tag} export"), &tl, &a, 30.0, 2, Expect::Any);
    }
    s.finish();
}

/// A render size the plan's canvas does not come out at (an odd one, or a different even one) is refused whatever the display says: the
/// conversion kernel FFmpeg runs for an odd height is another (the C one), and the compositor's display model is the x86 one.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_a_render_size_the_canvas_does_not_have_is_refused() {
    let m = std_media();
    let (tl, a) = alone(&m.noise, 1.0, None, &|_| {});
    let plan = RenderPlan::at(&tl, &a, &ExportOptions::default(), 0.5, policy()).expect("plan");
    prime();
    let c = Compositor::new(gpu())
        .expect("a compositor")
        .with_display(kerf_gpu::display_probe::Display::Match);
    let layers = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    let right = plan.size(u32::MAX);
    assert!(
        c.composite(&plan, &layers, right).is_ok(),
        "the control: the canvas' own size is drawn"
    );
    let mut drawn_odd = Vec::new();
    for size in [
        (right.0 - 1, right.1),
        (right.0, right.1 - 1),
        (right.0 - 1, right.1 - 1),
        (321, 181),
        (right.0 + 1, right.1 + 1),
        (3, 3),
        (1, 1),
    ] {
        match c.composite(&plan, &layers, size) {
            Ok(f) => drawn_odd.push(format!("{size:?}: drawn {}x{}", f.width, f.height)),
            Err(GpuError::Unsupported(_)) => {}
            Err(e) => drawn_odd.push(format!("{size:?}: {e}")),
        }
    }
    eprintln!("Z render sizes other than the canvas': {drawn_odd:?}");
    assert!(drawn_odd.is_empty(), "{drawn_odd:?}");
}

// ---- the alpha rule's rounding at its ties ---------------------------------------------------------------------------

/// Footage long enough for a dissolve of four seconds: `(under, over)`, 320x180, 7 s.
fn long_footage() -> &'static (Asset, Asset) {
    static L: OnceLock<(Asset, Asset)> = OnceLock::new();
    L.get_or_init(|| {
        let m = media();
        (
            m.x264(
                "z2-long-a.mp4",
                &format!("testsrc2=size=320x180:rate=30:duration=7,{UNTAGGED}"),
                "yuv420p",
                &[],
            ),
            m.x264("z2-long-b.mp4", &noise_src(320, 180, 7, UNTAGGED), "yuv420p", &[]),
        )
    })
}

/// The cut: the first footage, then the second dissolving in over `fade` seconds from 0.6 s, at `mixed / 255` opacity.
fn dissolve_in(mixed: u32, fade: f64) -> (Timeline, Vec<Asset>) {
    let (a, b) = long_footage();
    let mut out = clip(a, 0.0, 6.5, 0.0);
    out.transform.opacity = 1.0;
    let mut inc = clip(b, 0.0, 6.0, 0.6);
    inc.transform.opacity = f64::from(mixed) / 255.0;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: fade,
    });
    (timeline(vec![vec![out, inc]], None), vec![a.clone(), b.clone()])
}

/// The ramp factors `(frame, factor)` of a dissolve-in on a translucent clip, as the plan evaluates them for every frame of the fade.
fn alpha_ramp_factors(fps: f64, fade: f64) -> Vec<(u64, u32)> {
    let (tl, a) = dissolve_in(128, fade);
    let planner = Planner::new(&tl, &a, &at_fps(fps), PlanRequest::motion(policy())).expect("planner");
    let mut out = Vec::new();
    let from = (0.6 * fps) as u64;
    for k in from.saturating_sub(1)..((0.6 + fade) * fps).ceil() as u64 + 2 {
        let Ok(plan) = planner.at_frame(k) else { continue };
        for layer in &plan.layers {
            for op in plan.fade_ops(layer) {
                if op.tint == kerf_core::FadeTint::Alpha && op.edge == kerf_core::FadeEdge::In {
                    out.push((k, op.factor));
                }
            }
        }
    }
    out
}

/// The rounding constant of `ffmpeg_alpha_ramped` matters only at a tie, `(mixed * factor) mod 2^16 = 2^15`, where `+ 32768` and `+ 32767`
/// in `(mixed * ramp + 32768) >> 16` differ; the unit test's own cases and the sweeps above never reach one (a mutation of the constant
/// survives them). This asks the planner for the factors of many fade lengths and rates, finds the opacities whose frames sit on a tie, and
/// draws exactly those.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z2_the_alpha_ramp_at_the_ties_of_its_rounding() {
    let mut s = Suite::new("alpha-ties");
    // (mixed, fps, fade, tie frames)
    let mut found: Vec<(u32, f64, f64, Vec<u64>)> = Vec::new();
    let mut per_mixed: BTreeMap<u32, usize> = BTreeMap::new();
    for fps in [24.0, 30.0, 48.0, 50.0, 60.0] {
        for k in 1..=60 {
            let fade = f64::from(k) * 0.07;
            let factors = alpha_ramp_factors(fps, fade);
            for mixed in 1..=254u32 {
                let ties: Vec<u64> = factors
                    .iter()
                    .filter(|(_, f)| (mixed * f) % 65536 == 32768)
                    .map(|(k, _)| *k)
                    .collect();
                if !ties.is_empty() && *per_mixed.entry(mixed).or_default() < 2 {
                    *per_mixed.entry(mixed).or_default() += 1;
                    found.push((mixed, fps, fade, ties));
                }
            }
        }
    }
    eprintln!(
        "Z alpha ties: {} (opacity, rate, fade) triples with a frame on a tie of the rounding, for {} different opacities: {:?}",
        found.len(),
        per_mixed.len(),
        found.iter().take(10).collect::<Vec<_>>()
    );
    assert!(
        !found.is_empty(),
        "no fade length and rate puts a frame on a tie of the rounding"
    );
    // A spread of them: different opacities, rates and lengths.
    let step = (found.len() / 14).max(1);
    for (mixed, fps, fade, ties) in found.iter().step_by(step).take(14) {
        let (tl, a) = dissolve_in(*mixed, *fade);
        let last = ties.iter().max().copied().unwrap_or(0) + 1;
        s.motion(
            &format!("mixed {mixed} at {fps} fps fade {fade:.2}: ties at {ties:?}"),
            &tl,
            &a,
            *fps,
            last,
            Expect::Any,
        );
    }
    s.finish();
}
