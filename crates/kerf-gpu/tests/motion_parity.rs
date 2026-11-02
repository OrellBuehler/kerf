//! Motion parity: the export graph's own frames against the compositor's, frame by frame.
//!
//! `tests/parity.rs` holds the compositor to FFmpeg's *still*; this holds it to the *export*: every
//! case builds a short cut, renders it losslessly through `kerf_core::render_with` (the real
//! `build_filter_complex`, `xfade`-free: fades, dips, alpha ramps and `overlay` travel), reads every
//! frame back as `yuv420p` **planes** (no colour conversion in between: the planes `overlay` blended
//! are the ones compared) and draws the same output frames from a [`PlanMode::Motion`] plan — the
//! pick of each source frame is the `fps` filter's, served by one [`MotionFrames`] cursor per clip.
//! The comparison is plane for plane, in levels of 255, with a stated bound per case; the RGB
//! montage `FFmpeg | GPU | diff x8` of the frames worth a look goes to `target/parity/motion/`
//! (`KERF_PARITY_KEEP=1` keeps it for passing frames too).
//!
//! Needs `ffmpeg` and an adapter (lavapipe is enough); `#[ignore]`d. Run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test motion_parity -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
//!
//! # What "match" means here
//!
//! Where the source is drawn at its own size (these cases are: a 1:1 `scale` is skipped and the
//! compositor's scaler is not in play) the planes agree **to the level**: `fade` is integer
//! arithmetic ([`kerf_core::fade_luma`], `fade_chroma`, `fade_alpha`: the filter is identical on 6.1.1 and 9.0.2,
//! checked by [`fade_arithmetic_is_ffmpegs`] on every factor; what FFmpeg 9 does around it to a full-range
//! picture is not, and is refused), the dissolve is `overlay`'s own
//! `FAST_DIV255` on the ramped alpha, a slide is a position the overlay truncates, a tail is a
//! clip that plays on. A bound above zero is a measured, named thing (a rotated edge, a
//! translucent layer's RGB round trip) and says so next to the case.

#![allow(clippy::print_stderr)]

mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    fade_alpha, fade_chroma, fade_luma, Asset, Clip, Color, CompositeColorPolicy, Container, Delivery, DipWhy, ExportOptions,
    FadeEdge, FadeStep, FadeTint, Fit, PlanMode, PlanRequest, Planner, Project, RateControl, StreamKind, Timeline, Track,
    Transition, TransitionKind, Unsupported, FADE_FULL,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames, YuvFrame};

// ---- the machinery -------------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

thread_local! {
    /// A case that draws with every blend mode in the caps, whatever this FFmpeg's probe said ([`forced_blends`]).
    static FORCED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    static FORCING: OnceLock<Compositor> = OnceLock::new();
    if FORCED.with(std::cell::Cell::get) {
        return FORCING.get_or_init(|| {
            let _ = kerf_gpu::grade::kernels();
            Compositor::new(gpu())
                .expect("a compositor")
                .with_blend_kinds(kerf_gpu::blend::all_modes())
        });
    }
    C.get_or_init(|| {
        // The grade is in the caps once this FFmpeg's conversions are known to be the model's (the app's `prepare` asks
        // ahead of a frame): the harness measures them once, up front, and its grade cases hold the answer to `Match`.
        let _ = kerf_gpu::grade::kernels();
        // ... and so is the layout of its `drawtext`, which decides whether text whose bitmaps share samples is drawn.
        let _ = kerf_gpu::text::model();
        // ...and the blend chain's, which the blend modes are in the caps by.
        let _ = kerf_gpu::blend::kernels();
        Compositor::new(gpu()).expect("a compositor")
    })
}

fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

/// How this FFmpeg picks the composite's matrix (probed from the real graph, once). The harness
/// never runs against a guess.
fn policy() -> CompositeColorPolicy {
    let measured = kerf_core::composite_color_policy();
    assert_ne!(
        measured,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg ({})",
        kerf_core::ffmpeg_path()
    );
    measured
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

// ---- media -----------------------------------------------------------------------------

struct Media {
    /// Moving test pattern with a running frame counter: every source frame is told apart.
    testsrc: Asset,
    /// Colour bars (untagged), a still pattern: a wrong *picture* shows, a wrong frame of it does not.
    bars: Asset,
    /// A 25 fps source, for a cut that mixes rates.
    testsrc25: Asset,
    /// A 640x360 still image.
    image: Asset,
    /// A vertical 360x640 source, for a letterbox.
    portrait: Asset,
    /// Per-sample noise in all three planes: every Y, U, V combination, clipped RGB included.
    noise: Asset,
    /// A smooth gradient over black: pictures whose only strong edge is a layer's own, for the
    /// cases with a turned layer.
    smooth: Asset,
    black: Asset,
    /// 8-pixel blocks of limited-range black (16) and white (235): hard edges, whose scaled overshoot runs
    /// down to the bottom of the range and clips there, where a table's first entries are steepest.
    edges: Asset,
    /// Luma 0 to 255 left to right, chroma ramps against it: every entry of every table, smoothly.
    ramp: Asset,
    /// `testsrc` whose picture starts 0.3 s after its sound (`StreamInfo::lead`), muxed into MP4
    /// with an edit list as a phone writes it.
    late: Asset,
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media");
        std::fs::create_dir_all(&dir).unwrap();
        // Lossless H.264 (the pixels FFmpeg and the GPU decode are then the synthesized ones), a short
        // GOP, four seconds so that a clip cut from the middle has a handle either side for a tail.
        let video = |name: &str, lavfi: &str| -> PathBuf {
            let p = dir.join(name);
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                lavfi,
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            p
        };
        let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        let testsrc = video("testsrc.mp4", &format!("testsrc2=size=640x360:rate=30:duration=4,{untagged}"));
        let bars = video("bars.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=4,{untagged}"));
        let testsrc25 = video(
            "testsrc25.mp4",
            &format!("testsrc2=size=640x360:rate=25:duration=4,{untagged}"),
        );
        let portrait = video(
            "portrait.mp4",
            &format!("testsrc2=size=360x640:rate=30:duration=4,{untagged}"),
        );
        let noise = video(
            "noise.mp4",
            "nullsrc=size=640x360:rate=30:duration=4,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
        );
        let smooth = video(
            "smooth.mp4",
            &format!("gradients=size=640x360:rate=30:duration=4:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=640:y1=360:speed=0.00001,format=yuv420p,{untagged}"),
        );
        let black = video("black.mp4", "color=c=black:s=640x360:r=30:d=4");
        let edges = video(
            "edges.mp4",
            "nullsrc=size=640x360:rate=30:duration=4,format=yuv420p,geq=lum='if(gt(mod(floor(X/8)+floor(Y/8),2),0.5),16,235)':cb=128:cr=128",
        );
        let ramp = video(
            "ramp.mp4",
            "nullsrc=size=640x360:rate=30:duration=4,format=yuv420p,geq=lum='X*255/640':cb='X*255/640':cr='255-X*255/640'",
        );
        let late = dir.join("late.mp4");
        ffmpeg(&[
            "-itsoffset",
            "0.3",
            "-i",
            testsrc.to_str().unwrap(),
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4.5",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            late.to_str().unwrap(),
        ]);
        let image = dir.join("image.png");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "gradients=size=640x360:rate=1:duration=1:c0=0x20a040:c1=0xc04080:nb_colors=2:seed=3:speed=0.00001",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            image.to_str().unwrap(),
        ]);
        Media {
            testsrc: probed(&testsrc),
            bars: probed(&bars),
            testsrc25: probed(&testsrc25),
            image: probed(&image),
            portrait: probed(&portrait),
            noise: probed(&noise),
            smooth: probed(&smooth),
            black: probed(&black),
            edges: probed(&edges),
            ramp: probed(&ramp),
            late: probed(&late),
        }
    })
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

fn clip(a: &Asset, source_in: f64, source_out: f64, start: f64) -> Clip {
    Clip::new(a.id, source_in, source_out, start)
}

fn at_fps(fps: f64) -> ExportOptions {
    ExportOptions {
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

// ---- the export, read back as planes --------------------------------------------------

/// The export of a cut: every frame, as `yuv420p`.
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

    /// The Y, U and V planes of frame `k`.
    fn planes(&self, k: usize) -> [&[u8]; 3] {
        let f = &self.yuv[k * self.frame_len()..(k + 1) * self.frame_len()];
        let (y, uv) = f.split_at(self.w * self.h);
        let (u, v) = uv.split_at(self.w * self.h / 4);
        [y, u, v]
    }

    /// Frame `k` as RGB (FFmpeg's own conversion, untagged: BT.601), for the montage.
    fn rgb(&self, k: usize) -> Vec<u8> {
        let size = format!("{}x{}", self.w, self.h);
        let f = &self.yuv[k * self.frame_len()..(k + 1) * self.frame_len()];
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
            f,
        )
    }
}

fn export(slug: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Played {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    let file = scratch.join(format!("{slug}.mkv"));
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts.clone()
    };
    kerf_core::render_with(tl, assets, &file, &lossless).expect("export");
    let probe = ffmpeg(&[
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
    Played {
        w: video.width.unwrap() as usize,
        h: video.height.unwrap() as usize,
        yuv: probe,
    }
}

// ---- the comparison ------------------------------------------------------------------------

/// How far a plane may be from the export's, in levels of 255.
#[derive(Clone, Copy)]
struct Limits {
    y: i32,
    uv: i32,
}

/// Exact: the planes the export wrote.
const EXACT: Limits = Limits { y: 0, uv: 0 };

/// One level: a layer the compositor scales. swscale's x86 scalers are not bit-exact with the C
/// arithmetic the shader follows, and agree to one level up to about 4:1 on every plane
/// (`parity.rs`, `the_scaler_matches_ffmpegs_scale_plane_by_plane`); the fades, the alpha ramp and
/// the blend on top of it add nothing.
const SCALED: Limits = Limits { y: 1, uv: 1 };

/// A translucent layer: the RGB round trip `colorchannelmixer` makes it take (`roundtrip.rs`: luma
/// exact, chroma within one level of swscale's x86 vertical scaler) and a blend on top.
const TRANSLUCENT: Limits = Limits { y: 0, uv: 1 };

/// Two levels: a translucent layer the compositor also scales (the 2:1 shrink of an HD picture into
/// the canvas): the scaler's one level and the round trip's.
const HD_TRANSLUCENT: Limits = Limits { y: 2, uv: 2 };

/// Two levels: a layer the compositor scales *and* grades, with tables that step by at most two between neighbouring entries
/// (`eq::table_steps`, `MAX_AMPLIFIED_ERROR`). The scaler's one level (`SCALED`) is mapped through the tables, so a pair of
/// values a level apart come out as far apart as the tables' steepest step; the plan draws the frames inside that bound and
/// refuses the others (`GradeSteep`: contrast past 2, saturation past 2, gamma past about 1.2, a black point parked above 0 under a
/// gamma above 1; measured Y 3 to 19 and UV 3 on them), exact or not. A bound the grade is judged by, not a number measured and blessed.
const GRADED_SCALED: Limits = Limits { y: 2, uv: 2 };

/// A dip to white: two levels in every plane on the pictures it is drawn for — 8-bit limited-range
/// untagged 4:2:0, test patterns and noise in all three planes (every Y, U, V combination, clipped RGB
/// included) — identically on 6.1.1 and 9.0.2, on x86-64 (the arm64 kernel is not measured).
/// `fade c=white` only takes RGB, so FFmpeg converts `yuv420p -> rgb24 -> yuv420p`
/// around it, and the way out of YUV is **not** the C table converter the translucent round trip
/// is exact against (`yuva420p -> argb`): `yuv420p -> rgb24` runs an x86 kernel whose 13-bit
/// coefficients land up to two levels from the tables' (random planes against the tables of
/// `roundtrip.rs`: 2189 of 6144 samples differ, none by more than 2). The compositor follows the
/// tables, which are the portable arithmetic; the plane error is the kernel's. A dip to white is
/// the one transition drawn within a bound rather than to the level.
const WHITE: Limits = Limits { y: 2, uv: 2 };

/// A BT.709-tagged picture in a dip to white (both clips tagged): three levels of Y and two of UV,
/// the same kernel read through BT.709's coefficients, identically on 6.1.1 and 9.0.2.
const WHITE_709: Limits = Limits { y: 3, uv: 2 };

#[derive(Default, Clone, Copy)]
struct PlaneStats {
    max: i32,
    differing: usize,
    sum: u64,
    sq: u64,
    n: usize,
}

impl PlaneStats {
    fn of(a: &[u8], b: &[u8]) -> Self {
        let mut s = Self {
            n: a.len(),
            ..Self::default()
        };
        for (x, y) in a.iter().zip(b) {
            let d = (i32::from(*x) - i32::from(*y)).abs();
            s.max = s.max.max(d);
            s.differing += usize::from(d != 0);
            s.sum += d as u64;
            s.sq += (d * d) as u64;
        }
        s
    }

    fn add(&mut self, o: Self) {
        self.max = self.max.max(o.max);
        self.differing += o.differing;
        self.sum += o.sum;
        self.sq += o.sq;
        self.n += o.n;
    }

    fn psnr(&self) -> f64 {
        if self.sq == 0 {
            99.0
        } else {
            10.0 * (255.0 * 255.0 / (self.sq as f64 / self.n as f64)).log10()
        }
    }
}

fn report() -> &'static Mutex<BTreeMap<String, String>> {
    static R: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn write_report() {
    let mut text = String::new();
    for line in report().lock().unwrap().values() {
        let _ = writeln!(text, "{line}");
    }
    let dir = target_dir().join("parity");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("motion-report.txt"), text);
}

fn planes_of(f: &YuvFrame) -> [&[u8]; 3] {
    [&f.y, &f.u, &f.v]
}

/// A `FFmpeg | GPU | diff x8` strip of one frame.
fn montage(
    case: &str,
    k: u64,
    played: &Played,
    plan: &kerf_core::RenderPlan,
    frames: &[Option<Arc<YuvFrame>>],
    counts: &[Option<u64>],
) {
    let (w, h) = (played.w, played.h);
    let reference = played.rgb(k as usize);
    let gpu = compositor()
        .composite_counted(plan, frames, counts, (w as u32, h as u32))
        .expect("the GPU's RGB");
    let gpu_rgb: Vec<u8> = gpu.data.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut strip = Vec::with_capacity(w * h * 9);
    for row in 0..h {
        strip.extend_from_slice(&reference[row * w * 3..(row + 1) * w * 3]);
        strip.extend_from_slice(&gpu_rgb[row * w * 3..(row + 1) * w * 3]);
        for i in 0..w {
            let d = |c: usize| {
                (i32::from(reference[(row * w + i) * 3 + c]) - i32::from(gpu_rgb[(row * w + i) * 3 + c])).unsigned_abs()
            };
            let mag = (d(0).max(d(1)).max(d(2)) * 8).min(255) as u8;
            strip.extend_from_slice(&[mag, mag, mag]);
        }
    }
    let dir = target_dir().join("parity").join("motion");
    std::fs::create_dir_all(&dir).unwrap();
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    write_png(&dir.join(format!("{slug}-f{k:03}.png")), &strip, w * 3, h);
}

/// What a case asserts about the frames it draws.
struct Check<'a> {
    case: &'a str,
    tl: &'a Timeline,
    assets: &'a [Asset],
    opts: ExportOptions,
    /// The output frames to compare.
    frames: Range<u64>,
    limits: Limits,
    /// Frames to write a montage of (and every frame over the bound).
    look: &'a [u64],
    /// The frames the plan refuses for this reason (FFmpeg draws them) are not compared, and at
    /// least one must be: the case is about a refusal. Every other frame is compared with the
    /// export, so a refusal that is wider than it has to be shows too. A frame the plan refuses
    /// for another reason is a failure.
    refusing: Option<&'a Refusing>,
}

/// Plan, draw and compare every frame of `check`; returns the worst `(y, uv)` error. The planes of
/// each frame are compared with the export's, and a frame the compositor refuses is a failure
/// (a case that is about a refusal is [`refused`]).
fn check_motion(check: Check) -> (i32, i32) {
    let Check {
        case,
        tl,
        assets,
        opts,
        frames,
        limits,
        look,
        refusing,
    } = check;
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let played = export(&slug, tl, assets, &opts);
    assert!(
        played.count() as u64 >= frames.end,
        "{case}: the export has {} frames, the case looks at {}",
        played.count(),
        frames.end
    );
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(policy())).expect("planner");
    assert_eq!(planner.mode(), PlanMode::Motion);
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let (mut ys, mut us, mut vs) = (PlaneStats::default(), PlaneStats::default(), PlaneStats::default());
    let mut worst: Vec<(u64, i32, i32)> = Vec::new();
    let mut layers_seen = 0;
    let mut refused_frames = 0usize;
    for k in frames.clone() {
        let plan = planner.at_frame(k).expect("plan");
        assert_eq!((plan.canvas.width, plan.canvas.height), (played.w as u32, played.h as u32));
        let why = plan.reasons(&compositor().caps(), size);
        if refusing.is_some_and(|r| why.iter().any(|w| (r.is)(w))) {
            refused_frames += 1;
            continue;
        }
        assert!(
            why.is_empty(),
            "{case}: frame {k}: the plan says the GPU cannot draw this: {why:?}"
        );
        layers_seen = layers_seen.max(plan.layers.len());
        let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let drawn = compositor()
            .composite_yuv_counted(&plan, &picture, motion.counts(), size)
            .unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let want = played.planes(k as usize);
        let got = planes_of(&drawn);
        let (y, u, v) = (
            PlaneStats::of(want[0], got[0]),
            PlaneStats::of(want[1], got[1]),
            PlaneStats::of(want[2], got[2]),
        );
        let (my, muv) = (y.max, u.max.max(v.max));
        if my > limits.y || muv > limits.uv {
            worst.push((k, my, muv));
        }
        ys.add(y);
        us.add(u);
        vs.add(v);
        if look.contains(&k)
            || !worst.is_empty() && worst.last().is_some_and(|w| w.0 == k)
            || std::env::var_os("KERF_PARITY_KEEP").is_some()
        {
            montage(case, k, &played, &plan, &picture, motion.counts());
        }
    }
    let uv = {
        let mut s = us;
        s.add(vs);
        s
    };
    if let Some(r) = refusing {
        assert!(refused_frames > 0, "{case}: no frame was refused for {}", r.label);
    }
    let banded = match refusing {
        Some(r) => format!(" | {refused_frames} frames refused for {}, FFmpeg draws them", r.label),
        None => String::new(),
    };
    let line = format!(
        "{case:<44} frames {:>3}..{:<3} layers<={layers_seen} | Y: max {:>2} differing {:>7} PSNR {:>5.1} dB | UV: max {:>2} differing {:>7} PSNR {:>5.1} dB{banded}",
        frames.start,
        frames.end,
        ys.max,
        ys.differing,
        ys.psnr(),
        uv.max,
        uv.differing,
        uv.psnr()
    );
    eprintln!("{line}");
    report().lock().unwrap().insert(case.to_string(), line);
    write_report();
    if std::env::var_os("KERF_PARITY_EXPLORE").is_some() {
        for (k, y, uv) in worst.iter().take(4) {
            eprintln!(
                "WOULD FAIL: {case} frame {k}: Y {y} (<= {}), UV {uv} (<= {})",
                limits.y, limits.uv
            );
        }
        return (ys.max, uv.max);
    }
    assert!(
        worst.is_empty(),
        "{case}: frames over the bound (Y <= {}, UV <= {}): {:?} (montages in target/parity/motion)",
        limits.y,
        limits.uv,
        &worst[..worst.len().min(12)]
    );
    (ys.max, uv.max)
}

/// A moving zoom or position: the export places the picture on the canvas at sub-pixel positions
/// (a `perspective`), which no compositor pass draws, so its frames are refused.
fn run_placed(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) {
    refused(case, tl, assets, &at_fps(fps), upto(secs, fps), "a moving zoom or position");
}

/// A plan the compositor must refuse, for `reason`, at every frame of the range — with FFmpeg's
/// export unaffected (the case builds it all the same, so the graph is known to render).
fn refused(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, frames: Range<u64>, reason: &str) {
    let planner = Planner::new(tl, assets, opts, PlanRequest::motion(policy())).expect("planner");
    let mut refusals = 0;
    for k in frames {
        let plan = planner.at_frame(k).expect("plan");
        let size = plan.size(u32::MAX);
        let why: Vec<String> = plan
            .reasons(&compositor().caps(), size)
            .iter()
            .map(ToString::to_string)
            .collect();
        if why.iter().any(|r| r.contains(reason)) {
            refusals += 1;
        }
    }
    assert!(refusals > 0, "{case}: no frame was refused for `{reason}`");
    eprintln!("{case:<44} refused for `{reason}` on {refusals} frames, FFmpeg renders them");
}

// ---- the fade filter itself -------------------------------------------------------------

/// `fade`'s integer arithmetic, on every factor of a fade of every kind, checked against the filter
/// itself on a ramp of every luma, chroma and alpha value (`yuv420p`: black 16; `yuva420p`: black 0;
/// `alpha=1`: the alpha plane) and `FadeStep::factor_at_frame` against the frames it runs on
/// (the rate, a start off the frame grid, a duration of a frame or less). The compositor's shader
/// is these functions; this is the part of the Motion parity that does not need a GPU.
#[test]
#[ignore = "needs ffmpeg"]
fn fade_arithmetic_is_ffmpegs() {
    const W: usize = 256;
    const H: usize = 4;
    let ramp: Vec<u8> = (0..H).flat_map(|_| 0..=255u8).collect();
    let u: Vec<u8> = (0..H / 2).flat_map(|_| (0..128u8).map(|i| i * 2)).collect();
    let v: Vec<u8> = (0..H / 2).flat_map(|_| (0..128u8).map(|i| 255 - i * 2)).collect();
    let mut checked = 0usize;
    let cases: [(f64, f64, f64); 8] = [
        (25.0, 0.0, 1.0),
        (30.0, 0.4, 0.35),
        (24.0, 1.0, 0.5),
        (29.97, 0.2, 0.61),
        (60.0, 0.1, 0.0833),
        (30.0, 0.0167, 1.0),
        (25.0, 0.0, 0.03),
        (23.976, 0.5, 0.2),
    ];
    for (fps, st, d) in cases {
        for (pix, bytes_per_frame) in [("yuv420p", W * H * 3 / 2), ("yuva420p", W * H * 5 / 2)] {
            for edge in [FadeEdge::In, FadeEdge::Out] {
                for alpha in [false, true] {
                    if alpha && pix == "yuv420p" {
                        continue;
                    }
                    let frames = ((st + d + 0.3) * fps) as usize + 3;
                    let mut input = Vec::with_capacity(frames * bytes_per_frame);
                    for _ in 0..frames {
                        input.extend_from_slice(&ramp);
                        input.extend_from_slice(&u);
                        input.extend_from_slice(&v);
                        if pix == "yuva420p" {
                            input.extend_from_slice(&ramp);
                        }
                    }
                    let kind = if edge == FadeEdge::In { "in" } else { "out" };
                    let filter = format!("fade=t={kind}:st={st}:d={d}{}", if alpha { ":alpha=1" } else { "" });
                    let rate = fps.to_string();
                    let size = format!("{W}x{H}");
                    let out = ffmpeg_piped(
                        &[
                            "-f", "rawvideo", "-pix_fmt", pix, "-s", &size, "-r", &rate, "-i", "pipe:0", "-vf", &filter, "-f",
                            "rawvideo", "-pix_fmt", pix, "pipe:1",
                        ],
                        &input,
                    );
                    assert_eq!(out.len(), frames * bytes_per_frame, "{filter} on {pix} at {rate}");
                    let rate_q = kerf_core::Rational::from_fps(fps).expect("a rate");
                    let step = FadeStep {
                        edge,
                        tint: if alpha { FadeTint::Alpha } else { FadeTint::Black },
                        st,
                        d,
                    };
                    let black = if pix == "yuv420p" { 16 } else { 0 };
                    for k in 0..frames {
                        let factor = step.factor_at_frame(k as i64, rate_q.num, rate_q.den);
                        let frame = &out[k * bytes_per_frame..(k + 1) * bytes_per_frame];
                        let (yy, rest) = frame.split_at(W * H);
                        let (uu, rest) = rest.split_at(W * H / 4);
                        let (vv, aa) = rest.split_at(W * H / 4);
                        let id = format!("{filter} {pix} {rate} fps, frame {k}, factor {factor}");
                        if alpha {
                            assert_eq!(yy, &ramp[..], "{id}: `alpha=1` leaves luma alone");
                            assert_eq!(uu, &u[..], "{id}");
                            let want: Vec<u8> = ramp
                                .iter()
                                .map(|&a| if factor == FADE_FULL { a } else { fade_alpha(a, factor) })
                                .collect();
                            assert_eq!(aa, &want[..], "{id}: alpha");
                        } else {
                            let ey: Vec<u8> = ramp
                                .iter()
                                .map(|&p| if factor == FADE_FULL { p } else { fade_luma(p, factor, black) })
                                .collect();
                            let eu: Vec<u8> = u
                                .iter()
                                .map(|&p| if factor == FADE_FULL { p } else { fade_chroma(p, factor) })
                                .collect();
                            let ev: Vec<u8> = v
                                .iter()
                                .map(|&p| if factor == FADE_FULL { p } else { fade_chroma(p, factor) })
                                .collect();
                            assert_eq!(yy, &ey[..], "{id}: luma");
                            assert_eq!(uu, &eu[..], "{id}: U");
                            assert_eq!(vv, &ev[..], "{id}: V");
                            if pix == "yuva420p" {
                                assert_eq!(aa, &ramp[..], "{id}: a plain fade leaves alpha alone");
                            }
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    eprintln!("fade arithmetic: {checked} frames of fades equal to the filter's, every sample value");
}

// ---- cases ---------------------------------------------------------------------------------

/// The output frames of `secs` seconds at `fps`, the last one included: a fade-out's final frame is
/// the one that is black.
fn upto(secs: f64, fps: f64) -> Range<u64> {
    0..(secs * fps).floor() as u64
}

/// Two clips back to back (a second each, cut from the middle of four seconds of footage so that
/// a tail has a handle to play on), the second entering through `kind` over `secs`. `scale` is the
/// incoming clip's size: below 1 the outgoing clip shows around it and the whole transition
/// is visible.
fn pair(m: &Media, kind: Option<TransitionKind>, secs: f64, scale: f64) -> (Timeline, Vec<Asset>) {
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transform.scale = scale;
    inc.transition_in = kind.map(|kind| Transition { kind, duration: secs });
    (timeline(vec![vec![out, inc]], None), vec![m.bars.clone(), m.testsrc.clone()])
}

/// The frames a montage is made of: inside the transition of a `pair` (1.3 s), or the middle of the
/// fade-in and the fade-out of a one-second clip.
fn look_at(secs: f64, fps: f64) -> Vec<u64> {
    let at = |t: f64| (t * fps).round() as u64;
    if secs >= 1.5 {
        vec![at(1.3)]
    } else {
        vec![at(0.2), at(0.85)]
    }
}

fn run(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, limits: Limits) {
    check_motion(Check {
        case,
        tl,
        assets,
        opts: at_fps(fps),
        frames: upto(secs, fps),
        limits,
        look: &look_at(secs, fps),
        refusing: None,
    });
}

/// A refusal a case is about, matched by the [`Unsupported`] **variant** the plan gives (a message
/// can be shared by two reasons; a variant cannot).
struct Refusing {
    label: String,
    is: Box<dyn Fn(&Unsupported) -> bool>,
}

fn refusing(label: &str, is: impl Fn(&Unsupported) -> bool + 'static) -> Refusing {
    Refusing {
        label: label.to_string(),
        is: Box::new(is),
    }
}

/// A dip to white refused for `why`.
fn dip_refused(why: DipWhy) -> Refusing {
    refusing(
        &format!("DipToWhite({why:?})"),
        move |u| matches!(u, Unsupported::DipToWhite(_, w) if *w == why),
    )
}

/// A rotation the plan does not draw: any angle `rotate` does not land on whole samples for (`kerf_core::quarter_turn`), a keyed one included.
fn rotation_refused() -> Refusing {
    refusing("Rotation", |u| matches!(u, Unsupported::Rotation(..)))
}

fn translucent_fade_refused() -> Refusing {
    refusing("FadeOnTranslucent", |u| matches!(u, Unsupported::FadeOnTranslucent(_)))
}

fn full_range_cut_refused() -> Refusing {
    refusing("FullRangeCut", |u| matches!(u, Unsupported::FullRangeCut))
}

fn full_range_resized_refused() -> Refusing {
    refusing("FullRangeResized", |u| matches!(u, Unsupported::FullRangeResized(..)))
}

/// [`run`] for a case about a refusal: the frames the plan refuses for it are FFmpeg's, the
/// others are compared with the export as usual.
fn run_refusing(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, limits: Limits, reason: &Refusing) {
    check_motion(Check {
        case,
        tl,
        assets,
        opts: at_fps(fps),
        frames: upto(secs, fps),
        limits,
        look: &[],
        refusing: Some(reason),
    });
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn export_frames_are_drawn_without_a_transition() {
    let m = media();
    // A hard cut, at the sources' rate and at others: the `fps` filter drops and repeats frames,
    // and the pick says which.
    for fps in [30.0, 24.0, 25.0, 29.97, 60.0] {
        let (tl, assets) = pair(m, None, 0.0, 1.0);
        run(&format!("plain/cut @{fps}"), &tl, &assets, fps, 2.0, EXACT);
    }
    // A 25 fps source in a 30 fps cut, and the reverse.
    let tl = timeline(
        vec![vec![clip(&m.testsrc25, 0.5, 1.5, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0)]],
        None,
    );
    let assets = [m.testsrc25.clone(), m.testsrc.clone()];
    run("plain/25-then-30 @30", &tl, &assets, 30.0, 2.0, EXACT);
    run("plain/25-then-30 @25", &tl, &assets, 25.0, 2.0, EXACT);
    // Speed, in both directions, and reversed.
    let mut fast = clip(&m.testsrc, 0.5, 2.5, 0.0);
    fast.speed = 2.0;
    let mut slow = clip(&m.testsrc, 0.5, 1.0, 1.0);
    slow.speed = 0.5;
    let mut back = clip(&m.testsrc, 0.5, 1.5, 2.0);
    back.speed = -1.0;
    let tl = timeline(vec![vec![fast, slow, back]], None);
    let assets = [m.testsrc.clone()];
    run("plain/speed-2-0.5-reversed", &tl, &assets, 30.0, 3.0, EXACT);
    run("plain/speed-2-0.5-reversed @24", &tl, &assets, 24.0, 3.0, EXACT);
    // A still image is a stream like another one in the export.
    let tl = timeline(
        vec![vec![clip(&m.image, 0.0, 1.0, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0)]],
        None,
    );
    let assets = [m.image.clone(), m.testsrc.clone()];
    run("plain/still-image", &tl, &assets, 30.0, 2.0, EXACT);
    // A smaller layer over another track, and a graded one.
    let mut pip = clip(&m.testsrc, 0.5, 1.5, 0.2);
    pip.transform.scale = 0.4;
    pip.transform.pos_x = 0.25;
    pip.transform.pos_y = -0.2;
    let mut graded = clip(&m.bars, 0.5, 1.5, 0.0);
    graded.color = Color {
        contrast: 1.2,
        saturation: 1.3,
        ..Color::default()
    };
    let tl = timeline(vec![vec![graded], vec![pip]], None);
    let assets = [m.bars.clone(), m.testsrc.clone()];
    run("plain/pip-over-graded", &tl, &assets, 30.0, 1.2, SCALED);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_late_starting_source_is_drawn_as_the_export_draws_it() {
    let m = media();
    assert!(
        (m.late.streams[0].lead - 0.3).abs() < 0.002,
        "the probe saw {}",
        m.late.streams[0].lead
    );
    // Over bars, so the gap before the first picture shows the layer below, not black alone.
    let over = |edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.late, 0.0, 1.5, 0.0);
        edit(&mut c);
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 2.5, 0.0)], vec![c]], None);
        (tl, vec![m.bars.clone(), m.late.clone()])
    };
    // From the head (the picture 0.3 s in), from inside the lead (unseeked, 0.15 s in), past it
    // (seeked as ever), at speed 2 and 0.5, reversed, and with a fade-in that starts on the first
    // picture.
    for fps in [30.0, 25.0] {
        let (tl, assets) = over(&|_| {});
        run(&format!("late/head @{fps}"), &tl, &assets, fps, 1.5, EXACT);
    }
    let (tl, assets) = over(&|c| c.source_in = 0.15);
    run("late/inside-the-lead", &tl, &assets, 30.0, 1.5, EXACT);
    let (tl, assets) = over(&|c| c.source_in = 0.6);
    run("late/past-the-lead", &tl, &assets, 30.0, 0.9, EXACT);
    let (tl, assets) = over(&|c| {
        c.source_out = 3.0;
        c.speed = 2.0;
    });
    run("late/speed-2", &tl, &assets, 30.0, 1.5, EXACT);
    let (tl, assets) = over(&|c| {
        c.source_out = 0.75;
        c.speed = 0.5;
    });
    run("late/speed-0.5", &tl, &assets, 30.0, 1.5, EXACT);
    let (tl, assets) = over(&|c| c.speed = -1.0);
    run("late/reversed", &tl, &assets, 30.0, 1.5, EXACT);
    // A fade-in from the first picture, at 30 fps and at 25, where the 0.3 s lead is half a frame.
    for fps in [30.0, 25.0] {
        let (tl, assets) = over(&|c| c.fade_in = 0.4);
        run(&format!("late/fade-in @{fps}"), &tl, &assets, fps, 1.5, EXACT);
    }
    // Entering through a dissolve (the ramp from the first picture to the end of the outgoing
    // tail), one no longer than the lead (no ramp: the picture comes in whole once the tail is
    // gone) and a dip to black (up from black on the first picture).
    let enter = |kind, secs: f64| {
        let out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.late, 0.0, 1.0, 1.0);
        inc.transition_in = Some(Transition { kind, duration: secs });
        (timeline(vec![vec![out, inc]], None), vec![m.bars.clone(), m.late.clone()])
    };
    for (name, kind, secs) in [
        ("dissolve", TransitionKind::Crossfade, 0.6),
        ("dissolve-inside-the-lead", TransitionKind::Crossfade, 0.2),
        ("dip", TransitionKind::DipToBlack, 0.6),
    ] {
        for fps in [30.0, 25.0] {
            let (tl, assets) = enter(kind, secs);
            run(&format!("late/{name} @{fps}"), &tl, &assets, fps, 2.0, EXACT);
        }
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn clip_fades_are_drawn_as_the_export_draws_them() {
    let m = media();
    let one = |fade_in: f64, fade_out: f64, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.fade_in = fade_in;
        c.fade_out = fade_out;
        edit(&mut c);
        (timeline(vec![vec![c]], None), vec![m.testsrc.clone()])
    };
    for fps in [24.0, 25.0, 29.97, 30.0, 60.0] {
        let (tl, assets) = one(0.4, 0.3, &|_| {});
        run(&format!("fade/in+out @{fps}"), &tl, &assets, fps, 1.0, EXACT);
    }
    // A fade of a frame, of an odd length, and longer than the clip (clamped to it).
    let (tl, assets) = one(1.0 / 30.0, 0.0333, &|_| {});
    run("fade/one-frame", &tl, &assets, 30.0, 1.0, EXACT);
    let (tl, assets) = one(0.61, 0.0, &|_| {});
    run("fade/0.61s @30", &tl, &assets, 30.0, 1.0, EXACT);
    let (tl, assets) = one(5.0, 0.0, &|_| {});
    run("fade/longer-than-the-clip", &tl, &assets, 30.0, 1.0, EXACT);
    // The fade-in and the fade-out overlap on a clip shorter than the two.
    let (tl, assets) = one(0.8, 0.8, &|_| {});
    run("fade/overlapping-in-and-out", &tl, &assets, 30.0, 1.0, EXACT);
    // Under a graded, a sped-up and a reversed clip: the fade is the last stage but the pick.
    let (tl, assets) = one(0.4, 0.3, &|c| {
        c.color = Color {
            brightness: 0.1,
            contrast: 1.2,
            ..Color::default()
        }
    });
    run("fade/graded", &tl, &assets, 30.0, 1.0, EXACT);
    let (tl, assets) = one(0.3, 0.3, &|c| {
        c.source_out = 2.5;
        c.speed = 2.0;
    });
    run("fade/speed-2", &tl, &assets, 30.0, 1.0, EXACT);
    let (tl, assets) = one(0.3, 0.3, &|c| c.speed = -1.0);
    run("fade/reversed", &tl, &assets, 30.0, 1.0, EXACT);
    // A smaller layer over another: the fade is the layer's, the layer below is not touched.
    let under = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut over = clip(&m.testsrc, 0.5, 1.5, 0.0);
    over.transform.scale = 0.5;
    over.fade_in = 0.4;
    over.fade_out = 0.4;
    let tl = timeline(vec![vec![under], vec![over]], None);
    let assets = [m.bars.clone(), m.testsrc.clone()];
    run("fade/small-layer-over-another", &tl, &assets, 30.0, 1.0, SCALED);
    // A letterboxed layer: `pad`'s bars are in the picture that fades.
    let mut tall = clip(&m.portrait, 0.5, 1.5, 0.0);
    tall.fade_in = 0.4;
    tall.fade_out = 0.4;
    let tl = timeline(vec![vec![tall]], Some(Delivery::new(640, 360, Fit::Contain)));
    let assets = [m.portrait.clone()];
    run("fade/letterboxed", &tl, &assets, 30.0, 1.0, SCALED);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn dips_are_drawn_as_the_export_draws_them() {
    let m = media();
    for fps in [24.0, 25.0, 29.97, 30.0, 60.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::DipToBlack), 0.8, 1.0);
        run(&format!("dip/black covering @{fps}"), &tl, &assets, fps, 2.0, EXACT);
    }
    // The incoming clip smaller than the frame: the outgoing one fades out under nothing.
    let (tl, assets) = pair(m, Some(TransitionKind::DipToBlack), 0.8, 0.5);
    run("dip/black partial", &tl, &assets, 30.0, 2.0, SCALED);
    // A dip of a frame, of an odd length and one longer than either clip.
    for (name, secs) in [("one-frame", 1.0 / 30.0), ("0.61s", 0.61), ("2s", 2.0)] {
        let (tl, assets) = pair(m, Some(TransitionKind::DipToBlack), secs, 1.0);
        run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0, EXACT);
    }
    // A dip beside the clips' own fades, and over another track (the track below shows through).
    let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
    out.fade_in = 0.3;
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::DipToBlack,
        duration: 0.6,
    });
    inc.fade_out = 0.4;
    let tl = timeline(vec![vec![out, inc]], None);
    let assets = [m.bars.clone(), m.testsrc.clone()];
    run("dip/black with the clips' own fades", &tl, &assets, 30.0, 2.0, EXACT);
    let base = clip(&m.bars, 0.5, 1.5, 0.0);
    let (over, assets) = pair(m, Some(TransitionKind::DipToBlack), 0.6, 0.6);
    let mut tracks = vec![vec![base, clip(&m.bars, 0.5, 1.5, 1.0)]];
    tracks.extend(over.tracks.iter().map(|t| t.clips.clone()));
    let tl = timeline(tracks, None);
    run("dip/black over another track", &tl, &assets, 30.0, 2.0, SCALED);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn dissolves_are_drawn_as_the_export_draws_them() {
    let m = media();
    for fps in [24.0, 25.0, 29.97, 30.0, 60.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61, 1.0);
        run(&format!("dissolve/covering @{fps}"), &tl, &assets, fps, 2.0, EXACT);
    }
    // The incoming clip smaller than the frame: the outgoing clip plays on around it, past the ramp.
    for fps in [24.0, 30.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61, 0.5);
        run(&format!("dissolve/partial @{fps}"), &tl, &assets, fps, 2.0, SCALED);
    }
    for (name, secs) in [("one-frame", 1.0 / 30.0), ("0.5s", 0.5), ("1s", 1.0)] {
        let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), secs, 1.0);
        run(&format!("dissolve/{name}"), &tl, &assets, 30.0, 2.0, EXACT);
    }
    // Up from black: no clip before it to cross.
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 0.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![inc]], None);
    run("dissolve/from-black", &tl, std::slice::from_ref(&m.testsrc), 30.0, 1.0, EXACT);
    // The outgoing clip reversed (its tail plays below its in point) and the incoming one sped up.
    let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
    out.speed = -1.0;
    let mut inc = clip(&m.testsrc, 0.5, 2.5, 1.0);
    inc.speed = 2.0;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run(
        "dissolve/reversed-out sped-up-in",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // A dissolve and then a fade out of the same clip: its chain is on an alpha plane from the dissolve
    // on, where `fade` takes a black of 0 and not 16.
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.4,
    });
    inc.fade_out = 0.4;
    let tl = timeline(vec![vec![out, inc]], None);
    run(
        "dissolve/then-fade-out (alpha plane black)",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // Three clips, two dissolves, the middle one entered and left through them.
    let a = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut b = clip(&m.testsrc, 0.5, 1.5, 1.0);
    b.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.4,
    });
    let mut c = clip(&m.testsrc25, 0.5, 1.5, 2.0);
    c.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![a, b, c]], None);
    let assets = [m.bars.clone(), m.testsrc.clone(), m.testsrc25.clone()];
    run("dissolve/chained", &tl, &assets, 30.0, 3.0, EXACT);
    // Still images either side: a still loops, so its handle never runs out.
    let mut inc = clip(&m.image, 0.0, 1.0, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), inc]], None);
    run(
        "dissolve/into-a-still-image",
        &tl,
        &[m.bars.clone(), m.image.clone()],
        30.0,
        2.0,
        EXACT,
    );
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![clip(&m.image, 0.0, 1.0, 0.0), inc]], None);
    run(
        "dissolve/out-of-a-still-image",
        &tl,
        &[m.image.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // The outgoing clip sped up: its tail is the handle at its own speed.
    let mut out = clip(&m.bars, 0.5, 2.5, 0.0);
    out.speed = 2.0;
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run(
        "dissolve/sped-up-outgoing",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // No handle to borrow (the outgoing clip ends on the last frame of its footage): a hard cut.
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![clip(&m.bars, 3.0, 4.0, 0.0), inc]], None);
    run(
        "dissolve/no-handle",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // Over another track: the track below shows through the ramp and around the layer.
    let base = clip(&m.testsrc25, 0.5, 2.5, 0.0);
    let (over, mut assets) = pair(m, Some(TransitionKind::Crossfade), 0.6, 0.6);
    let mut tracks = vec![vec![base]];
    tracks.extend(over.tracks.iter().map(|t| t.clips.clone()));
    let tl = timeline(tracks, None);
    assets.push(m.testsrc25.clone());
    run("dissolve/over-another-track", &tl, &assets, 30.0, 2.0, SCALED);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn slides_and_pushes_are_drawn_as_the_export_draws_them() {
    let m = media();
    let kinds = [
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ];
    for kind in kinds {
        for (cover, scale, limits) in [("covering", 1.0, EXACT), ("partial", 0.5, SCALED)] {
            let (tl, assets) = pair(m, Some(kind), 0.5, scale);
            run(&format!("{}/{cover}", kind.as_str()), &tl, &assets, 30.0, 2.0, limits);
        }
    }
    // The other rates, an odd length, a frame, and a travel longer than the frame can show.
    for fps in [24.0, 25.0, 29.97, 60.0] {
        for kind in [TransitionKind::SlideLeft, TransitionKind::PushUp] {
            let (tl, assets) = pair(m, Some(kind), 0.61, 1.0);
            run(&format!("{}/0.61s @{fps}", kind.as_str()), &tl, &assets, fps, 2.0, EXACT);
        }
    }
    for (name, secs) in [("one-frame", 1.0 / 30.0), ("1s", 1.0)] {
        for kind in [TransitionKind::SlideDown, TransitionKind::PushRight] {
            let (tl, assets) = pair(m, Some(kind), secs, 1.0);
            run(&format!("{}/{name}", kind.as_str()), &tl, &assets, 30.0, 2.0, EXACT);
        }
    }
    // A slide into a clip with its own fade-in and a translucent one.
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.fade_in = 0.4;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::SlideLeft,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run(
        "slide_left/with-a-fade-in",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // ... into a translucent clip: the round trip, then the travel (partly off the canvas) blended in integers.
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transform.opacity = 0.7;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::SlideLeft,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run(
        "slide_left/translucent-incoming",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        TRANSLUCENT,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn dips_to_white_are_drawn_through_the_rgb_round_trip() {
    let m = media();
    for fps in [24.0, 30.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::DipToWhite), 0.8, 1.0);
        run(&format!("dip/white covering @{fps}"), &tl, &assets, fps, 2.0, WHITE);
    }
    // Noise in every plane: every Y, U, V combination, the clipped RGB ones included.
    let mut out = clip(&m.noise, 0.5, 1.5, 0.0);
    out.fade_in = 0.0;
    let mut inc = clip(&m.noise, 1.5, 2.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.8,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run("dip/white noise", &tl, std::slice::from_ref(&m.noise), 30.0, 2.0, WHITE);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_shapes_of_a_real_cut_are_drawn_as_the_export_draws_them() {
    let m = media();
    // A graded clip entering through a dissolve (`eq` runs ahead of the alpha plane).
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.color = Color {
        contrast: 1.2,
        saturation: 1.3,
        brightness: 0.05,
        ..Color::default()
    };
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run(
        "dissolve/graded-incoming",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );

    // The montage of the coverage report: a dissolve, a dip and a slide, graded, in a 16:9 frame the
    // footage is scaled into (Cover), the first clip fading in, the last out.
    let kinds = [
        None,
        Some((TransitionKind::Crossfade, 0.5)),
        Some((TransitionKind::DipToBlack, 0.4)),
        Some((TransitionKind::SlideLeft, 0.4)),
        Some((TransitionKind::Crossfade, 0.5)),
    ];
    // (The scaler is one level off on its own; the contrast of a grade scales that difference, so
    // graded and scaled is two.)
    for (name, graded, limits) in [("ungraded", false, SCALED), ("graded", true, GRADED_SCALED)] {
        let mut clips = Vec::new();
        for (i, kind) in kinds.iter().enumerate() {
            let mut c = clip(&m.testsrc, 0.3 + 0.2 * i as f64, 1.8 + 0.2 * i as f64, 1.5 * i as f64);
            if graded {
                c.color = Color {
                    contrast: 1.1,
                    saturation: 1.2,
                    ..Color::default()
                };
            }
            c.transition_in = kind.map(|(kind, duration)| Transition { kind, duration });
            clips.push(c);
        }
        clips[0].fade_in = 0.5;
        clips[4].fade_out = 0.5;
        let tl = timeline(vec![clips], Some(Delivery::new(960, 540, Fit::Cover)));
        run(
            &format!("montage/{name} 960x540 cover"),
            &tl,
            std::slice::from_ref(&m.testsrc),
            30.0,
            7.5,
            limits,
        );
    }

    // The talking head of the coverage report: a 9:16 frame cut out of 16:9 footage, cropped, with
    // fades on both sides of every clip.
    let mut clips = Vec::new();
    for i in 0..3 {
        let mut c = clip(&m.testsrc, 0.3 + 0.2 * f64::from(i), 1.3 + 0.2 * f64::from(i), f64::from(i));
        c.fade_in = 0.2;
        c.fade_out = 0.2;
        c.transform.crop_left = 0.2;
        c.transform.crop_right = 0.2;
        clips.push(c);
    }
    let tl = timeline(vec![clips], Some(Delivery::new(360, 640, Fit::Cover)));
    run(
        "talking-head/9:16 cover with fades",
        &tl,
        std::slice::from_ref(&m.testsrc),
        30.0,
        3.0,
        SCALED,
    );

    // A title over a dissolve: the text is on `between(t, start, end)` in an export, end included.
    let (mut tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.5, 1.0);
    let mut title = kerf_core::TextOverlay::new("Summer 2026", 0.5, 1.5);
    title.bg = Some("black@0.5".into());
    tl.overlays.push(title);
    run("text/over a dissolve", &tl, &assets, 30.0, 2.0, EXACT);
}

/// A family installed here with no bold face (a bold request in it is `drawtext` thickened with a border).
fn regular_only_family() -> Option<String> {
    kerf_core::list_system_fonts()
        .into_iter()
        .find(|family| kerf_core::fonts::resolve_font_file(family, true).is_some_and(|(_, bold)| !bold))
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_whose_bitmaps_share_samples_in_an_export_frame() {
    // `drawtext` blends each glyph bitmap in turn: a synthetic-bold border's bitmaps overlap along every
    // letter gap and print under 12 px shares samples (and chroma blocks) all along the line. The export's
    // own planes are held to the compositor's, to the level, over video, inside fades and with a moving
    // title.
    let m = media();
    let one = std::slice::from_ref(&m.testsrc);
    let summed = kerf_gpu::text::sum_fallbacks();
    let base = || {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.fade_in = 0.4;
        c.fade_out = 0.4;
        timeline(vec![vec![c]], None)
    };
    let key = |time: f64, pos_y: f64, opacity: f64| kerf_core::TextKeyframe {
        time,
        pos_x: 0.5,
        pos_y,
        opacity,
    };
    // Print and a channel handle, 5 to 11 px at the 360 px the clip is.
    for (name, size, bg) in [
        ("print 5 px", 5.0, None),
        ("print 9 px", 9.0, None),
        ("handle 11 px, boxed", 11.0, Some("black@0.5")),
    ] {
        let mut tl = base();
        let mut o = kerf_core::TextOverlay::new("@kerf.studio  *Results vary. Terms apply.", 0.2, 1.8);
        (o.size, o.pos_x, o.pos_y, o.bg) = (size / 360.0, 0.37, 0.93, bg.map(String::from));
        o.color = "white@0.8".into();
        tl.overlays.push(o);
        run(&format!("text/{name}, over a fade"), &tl, one, 30.0, 2.0, EXACT);
    }
    // A caption fading in and sliding up, small and thickened.
    let mut tl = base();
    let mut o = kerf_core::TextOverlay::new("Sliding, fading: fi ffi", 0.1, 1.9);
    o.size = 10.0 / 360.0;
    o.keyframes = vec![key(0.0, 0.9, 0.0), key(0.5, 0.8, 1.0), key(1.7, 0.8, 1.0)];
    tl.overlays.push(o);
    run("text/small, sliding and fading", &tl, one, 30.0, 2.0, EXACT);
    match regular_only_family() {
        None => eprintln!("SKIPPED the synthetic-bold export cases: every installed family has a bold face"),
        Some(family) => {
            for (name, size, bg, moving) in [
                ("caption", 0.07, None, false),
                ("boxed", 0.06, Some("black@0.5"), false),
                ("title", 0.14, None, false),
                ("moving", 0.08, Some("white@0.4"), true),
            ] {
                let mut tl = base();
                let mut o = kerf_core::TextOverlay::new("Bold, thickened with a border: fi ffi Ay", 0.2, 1.8);
                (o.bold, o.font, o.size, o.pos_y, o.bg) = (true, Some(family.clone()), size, 0.6, bg.map(String::from));
                if moving {
                    o.keyframes = vec![key(0.0, 0.9, 0.0), key(0.4, 0.5, 1.0), key(1.6, 0.4, 1.0)];
                }
                tl.overlays.push(o);
                run(&format!("text/bold-border {name}, over a fade"), &tl, one, 30.0, 2.0, EXACT);
            }
            eprintln!("synthetic bold in {family}");
        }
    }
    assert_eq!(
        kerf_gpu::text::sum_fallbacks(),
        summed,
        "a text was blended as the sum of its bitmaps"
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn keyframed_position_and_rotation_in_an_export_frame() {
    let m = media();
    let key = |time: f64, pos_x: f64, rotation: f64| kerf_core::Keyframe {
        time,
        scale: 0.5,
        pos_x,
        pos_y: 0.0,
        rotation,
        opacity: 1.0,
        easing: Default::default(),
    };
    let under = clip(&m.black, 0.5, 1.5, 0.0);
    let mut moving = clip(&m.testsrc, 0.5, 1.5, 0.0);
    moving.keyframes = vec![key(0.0, -0.25, 0.0), key(1.0, 0.25, 0.0)];
    let tl = timeline(vec![vec![under.clone()], vec![moving]], None);
    run_placed("keyed/position", &tl, &[m.black.clone(), m.testsrc.clone()], 30.0, 1.0);
    // The easing of a segment is the polyline both renderers read (`Clip::transform_at`).
    for (name, easing) in [
        ("ease-in-out", kerf_core::Easing::EaseInOut),
        ("hold", kerf_core::Easing::Hold),
        (
            "bezier",
            kerf_core::Easing::Bezier {
                x1: 0.2,
                y1: 0.9,
                x2: 0.7,
                y2: 0.1,
            },
        ),
    ] {
        let mut eased = clip(&m.testsrc, 0.5, 1.5, 0.0);
        eased.keyframes = vec![
            kerf_core::Keyframe {
                easing,
                ..key(0.0, -0.25, 0.0)
            },
            key(1.0, 0.25, 0.0),
        ];
        // (The case is only meaningful if the easing moves the picture off the straight line.)
        let mut straight = eased.clone();
        straight.keyframes[0].easing = Default::default();
        assert_ne!(
            eased.transform_at(0.3).pos_x,
            straight.transform_at(0.3).pos_x,
            "{name} must change the pose"
        );
        let tl = timeline(vec![vec![under.clone()], vec![eased]], None);
        run_placed(
            &format!("keyed/position {name}"),
            &tl,
            &[m.black.clone(), m.testsrc.clone()],
            30.0,
            1.0,
        );
    }
    let mut turning = clip(&m.smooth, 0.5, 1.5, 0.0);
    turning.keyframes = vec![key(0.0, 0.0, 0.0), key(1.0, 0.0, 30.0)];
    let tl = timeline(vec![vec![under], vec![turning]], None);
    // (A rotation a key moves is a box of its own turned by an expression per frame, with an edge that is a fixed-point sample against a float
    // one -- Y 117 / UV 169 levels off at the edge: every frame of it is FFmpeg's.)
    run_refusing(
        "keyed/rotation",
        &tl,
        &[m.black.clone(), m.smooth.clone()],
        30.0,
        1.0,
        EXACT,
        &rotation_refused(),
    );
}

/// A keyed opacity is a `geq` alpha, `(int)(opacity(T) * 255)`, which the plan evaluates in the expression's own
/// arithmetic (`PlanLayer::keyed_alpha`): blended at the picture's size it is exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_keyed_opacity_is_the_geq_alpha_of_each_frame() {
    use kerf_core::{Easing, Property, PropertyKey};
    let m = media();
    let fading = |asset: &Asset, keys: Vec<PropertyKey>| {
        let mut c = clip(asset, 0.5, 1.5, 0.0);
        c.set_property_keys(Property::Opacity, keys);
        c
    };
    let key = |time: f64, value: f64| PropertyKey::new(time, value);
    let eased = |time: f64, value: f64, easing: Easing| PropertyKey { time, value, easing };
    let over_the_bars = |c: Clip| timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
    let assets = [m.bars.clone(), m.testsrc.clone()];
    for (name, keys) in [
        ("linear", vec![key(0.0, 1.0), key(1.0, 0.2)]),
        ("from nothing", vec![key(0.1, 0.0), key(0.9, 1.0)]),
        ("ease in-out", vec![eased(0.0, 0.9, Easing::EaseInOut), key(1.0, 0.1)]),
        ("hold", vec![eased(0.0, 0.7, Easing::Hold), key(0.5, 0.3), key(1.0, 0.8)]),
        (
            "bezier, off the frame grid",
            vec![
                eased(
                    0.013,
                    0.95,
                    Easing::Bezier {
                        x1: 0.2,
                        y1: 0.9,
                        x2: 0.7,
                        y2: 0.1,
                    },
                ),
                key(0.517, 0.05),
                key(0.871, 0.6),
            ],
        ),
        (
            "many eased keys",
            (0..10)
                .map(|i| eased(f64::from(i) * 0.1, 0.15 + 0.08 * f64::from(i % 9), Easing::EaseInOut))
                .collect(),
        ),
    ] {
        let tl = over_the_bars(fading(&m.testsrc, keys));
        run(&format!("keyed opacity/{name}"), &tl, &assets, 30.0, 1.0, EXACT);
    }
    // At 29.97 the clip's time is another grid of doubles.
    let tl = over_the_bars(fading(&m.testsrc, vec![key(0.0, 1.0), key(1.0, 0.0)]));
    run("keyed opacity/29.97", &tl, &assets, 30000.0 / 1001.0, 1.0, EXACT);
    // Through the clip's fades and the dissolve it enters by: both act on the `geq`'s alpha plane.
    let mut faded = fading(&m.testsrc, vec![key(0.0, 0.4), key(1.0, 0.9)]);
    faded.fade_in = 0.3;
    faded.fade_out = 0.3;
    run("keyed opacity/with fades", &over_the_bars(faded), &assets, 30.0, 1.0, EXACT);
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.set_property_keys(Property::Opacity, vec![key(0.0, 0.3), key(1.0, 0.8)]);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    run(
        "keyed opacity/dissolve",
        &timeline(vec![vec![out, inc]], None),
        &assets,
        30.0,
        2.0,
        EXACT,
    );
    // Scaled: the scaler's level, as for any scaled layer; moving, it is placed by the export's `perspective`.
    let mut pip = fading(&m.testsrc, vec![key(0.0, 0.2), key(1.0, 0.9)]);
    pip.transform.scale = 0.6;
    pip.set_property_keys(Property::PosX, vec![key(0.0, -0.2), key(1.0, 0.2)]);
    run_placed("keyed opacity/moving pip", &over_the_bars(pip), &assets, 30.0, 1.0);
    let mut odd = fading(&m.testsrc, vec![key(0.0, 0.5), key(1.0, 0.7)]);
    odd.transform.scale = 0.33;
    run("keyed opacity/odd 211x118", &over_the_bars(odd), &assets, 30.0, 1.0, SCALED);
    // Under a mask one `geq` multiplies the mask by the opacity: refused below 1.
    let mut masked = fading(&m.testsrc, vec![key(0.0, 0.5), key(1.0, 0.7)]);
    masked.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.1, false));
    refused(
        "keyed opacity/under a mask",
        &over_the_bars(masked),
        &assets,
        &at_fps(30.0),
        upto(1.0, 30.0),
        "below full opacity",
    );
    // ... and drawn while the curve holds 1 (its `geq` writes the mask), refused once it falls.
    let mut masked = fading(&m.testsrc, vec![key(0.0, 1.0), key(0.5, 1.0), key(1.0, 0.3)]);
    masked.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.1, false));
    run_refusing(
        "keyed opacity/under a mask, 1 then falling",
        &over_the_bars(masked),
        &assets,
        30.0,
        1.0,
        EXACT,
        &refusing("MaskWith", |u| matches!(u, Unsupported::MaskWith(..))),
    );
    // Under a chroma key the same `geq` reads the key's alpha plane (`alpha(X,Y)`), and on a build whose `geq` reads a
    // plane's last column and row one in (6.1) that is the key's alpha one in: measured 21 levels of luma at the
    // edge, on the frames where the curve holds 1. Refused for the clip, on those frames too (3 is at 255).
    let mut keyed_out = fading(&m.testsrc, vec![key(0.0, 1.0), key(0.5, 1.0), key(1.0, 0.3)]);
    keyed_out.effects = vec![crate::key("green", 0.2, 0.2)];
    let tl = over_the_bars(keyed_out);
    for frames in [3..4, upto(1.0, 30.0)] {
        refused(
            "keyed opacity/under a chroma key",
            &tl,
            &assets,
            &at_fps(30.0),
            frames,
            "under a keyed opacity",
        );
    }
    // Turned: the `rotate` that follows the `geq` turns its alpha plane with the picture.
    let mut turned = fading(&m.smooth, vec![key(0.0, 0.3), key(1.0, 0.9)]);
    turned.transform.rotation = 12.0;
    turned.transform.scale = 0.6;
    run_refusing(
        "keyed opacity/rotated",
        &timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0)], vec![turned]], None),
        &[m.black.clone(), m.smooth.clone()],
        30.0,
        1.0,
        EXACT,
        &rotation_refused(),
    );
}

/// The `geq` that writes a keyed opacity runs on the picture **before** the `rotate`, and on a build whose `geq` reads a plane's last column and
/// row one in (FFmpeg 6.1) the row and the column it replaces land on whatever edge of the turned box the turn gives them: the bottom row of a
/// picture turned a quarter lands on the left column, the last column of a half turn on the first. A picture with detail at its edges (the test
/// pattern, over the bars) turned by every quarter, alone and moved; held to the export to the sample, and a turn that is not a quarter is refused
/// by the plan. (A clip with a blend mode is `checker_e_a_turned_blended_clip_with_a_keyed_opacity`'s.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_keyed_opacity_on_a_turned_layer_is_shifted_before_the_turn() {
    use kerf_core::{Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let turned = |angle: f64, at: (f64, f64)| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.set_property_keys(
            Property::Opacity,
            vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 0.2)],
        );
        c.transform.rotation = angle;
        c.transform.pos_x = at.0;
        c.transform.pos_y = at.1;
        timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None)
    };
    for angle in [90.0, 180.0, 270.0, -90.0, 450.0] {
        for at in [(0.0, 0.0), (0.2, 0.1)] {
            run(
                &format!("keyed opacity/turned {angle} at {at:?}"),
                &turned(angle, at),
                &assets,
                30.0,
                1.0,
                EXACT,
            );
        }
    }
    for angle in [12.0, -30.0, 100.0] {
        run_refusing(
            &format!("keyed opacity/rotated {angle} over detail"),
            &turned(angle, (0.0, 0.0)),
            &assets,
            30.0,
            1.0,
            EXACT,
            &rotation_refused(),
        );
    }
}

// ---- a keyed colour: `eq eval=frame` builds its tables again for every frame ------------------------------------
//
// A keyed colour number is a quoted expression of the frame's time in the clip's `eq`, which FFmpeg evaluates
// per frame and clamps (`av_clipf`: through a `float`) before it builds the luma, U and V tables. The plan carries
// the numbers the expressions evaluate to (`PlanLayer::color`, `keyframe_value` at the `fps` grid's time) and the
// compositor builds its tables from them as it does a static grade's, so the planes are the export's to the level.

/// The span each colour number moves over in the cases below.
fn colour_span(prop: kerf_core::Property) -> (f64, f64) {
    use kerf_core::Property;
    match prop {
        Property::Brightness => (-0.3, 0.3),
        Property::Contrast => (0.5, 1.8),
        Property::Saturation => (0.0, 2.0),
        Property::Gamma => (0.5, 2.0),
        Property::Temperature => (-1.0, 1.0),
        _ => unreachable!("not a colour number"),
    }
}

/// A second of `asset` with each of `props` keyed along `shape(low, high)`.
fn colour_keyed(asset: &Asset, props: &[kerf_core::Property], shape: &dyn Fn(f64, f64) -> Vec<kerf_core::PropertyKey>) -> Clip {
    let mut c = clip(asset, 0.5, 1.5, 0.0);
    for &prop in props {
        let (lo, hi) = colour_span(prop);
        c.set_property_keys(prop, shape(lo, hi));
    }
    c
}

fn linear_keys(lo: f64, hi: f64) -> Vec<kerf_core::PropertyKey> {
    vec![kerf_core::PropertyKey::new(0.0, lo), kerf_core::PropertyKey::new(1.0, hi)]
}

/// That the colour the plan carries for the top layer of a cut moves (at least `moves` different ones over the
/// frames): a case whose colour stood still would pass anything.
fn colour_moves(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, moves: usize) {
    let planner = Planner::new(tl, assets, &at_fps(fps), PlanRequest::motion(policy())).expect("planner");
    let mut seen: Vec<Color> = Vec::new();
    for k in upto(secs, fps) {
        if let Some(layer) = planner.at_frame(k).expect("plan").layers.last() {
            if !seen.contains(&layer.color) {
                seen.push(layer.color);
            }
        }
    }
    assert!(
        seen.len() >= moves,
        "{case}: only {} different colours over the cut",
        seen.len()
    );
}

/// [`run`] for a cut whose top layer has a keyed colour that moves.
fn run_colour(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, moves: usize, limits: Limits) {
    colour_moves(case, tl, assets, fps, secs, moves);
    run(case, tl, assets, fps, secs, limits);
}

fn grade_steep_refused() -> Refusing {
    refusing("GradeSteep", |u| matches!(u, Unsupported::GradeSteep(..)))
}

/// How many frames of the first `secs` of a cut the plan refuses for `GradeSteep`, and how many it draws.
fn grade_steep_split(tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) -> (usize, usize) {
    let planner = Planner::new(tl, assets, &at_fps(fps), PlanRequest::motion(policy())).expect("planner");
    let (mut refused, mut drawn) = (0, 0);
    for k in upto(secs, fps) {
        let plan = planner.at_frame(k).expect("plan");
        let why = plan.reasons(&compositor().caps(), plan.size(u32::MAX));
        if why.iter().any(|u| matches!(u, Unsupported::GradeSteep(..))) {
            refused += 1;
        } else if why.is_empty() {
            drawn += 1;
        }
    }
    (refused, drawn)
}

/// [`run_refusing`] for a grade whose tables step past two on some frames of a layer the scaler resizes: those frames are
/// FFmpeg's and the others are held to the export at `limits`. `refused` is how many frames that is (`None`: some, not all).
fn run_grade_steep(case: &str, tl: &Timeline, assets: &[Asset], secs: f64, limits: Limits, refused: Option<usize>) {
    let (n, drawn) = grade_steep_split(tl, assets, 30.0, secs);
    match refused {
        Some(want) => assert_eq!(n, want, "{case}: {n} frames refused, {drawn} drawn"),
        None => assert!(n > 0 && drawn > 0, "{case}: {n} frames refused, {drawn} drawn"),
    }
    run_refusing(case, tl, assets, 30.0, secs, limits, &grade_steep_refused());
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_keyed_colour_is_the_eq_of_each_frame() {
    use kerf_core::{Easing, Property, PropertyKey};
    let m = media();
    let noise = std::slice::from_ref(&m.noise);
    let alone = |c: Clip| timeline(vec![vec![c]], None);
    fn eased(time: f64, value: f64, easing: Easing) -> PropertyKey {
        PropertyKey { time, value, easing }
    }
    let all = Property::COLOR;
    type Shape = Box<dyn Fn(f64, f64) -> Vec<PropertyKey>>;
    let shapes: [(&str, Shape, usize); 5] = [
        ("linear", Box::new(linear_keys), 20),
        (
            "ease in-out",
            Box::new(|lo, hi| vec![eased(0.0, lo, Easing::EaseInOut), PropertyKey::new(1.0, hi)]),
            20,
        ),
        (
            "hold",
            Box::new(|lo, hi| {
                vec![
                    eased(0.0, lo, Easing::Hold),
                    eased(0.5, hi, Easing::Hold),
                    PropertyKey::new(1.0, lo + (hi - lo) * 0.25),
                ]
            }),
            2,
        ),
        (
            "bezier, off the frame grid",
            Box::new(|lo, hi| {
                vec![
                    eased(
                        0.013,
                        lo,
                        Easing::Bezier {
                            x1: 0.2,
                            y1: 0.9,
                            x2: 0.7,
                            y2: 0.1,
                        },
                    ),
                    PropertyKey::new(0.517, hi),
                    PropertyKey::new(0.871, lo + (hi - lo) * 0.4),
                ]
            }),
            20,
        ),
        (
            // Over 24 points a keyframe expression is a balanced tree, not a chain.
            "many eased keys",
            Box::new(|lo, hi| {
                (0..10)
                    .map(|i| {
                        eased(
                            f64::from(i) * 0.1,
                            lo + (hi - lo) * f64::from((i * 7) % 9) / 8.0,
                            Easing::EaseInOut,
                        )
                    })
                    .collect()
            }),
            20,
        ),
    ];
    // Every number alone, and all five together, along each shape of curve, over noise in all three planes: every
    // entry of every table is looked at.
    for (name, shape, moves) in &shapes {
        for prop in all {
            run_colour(
                &format!("keyed colour/{prop:?}, {name}"),
                &alone(colour_keyed(&m.noise, &[prop], shape.as_ref())),
                noise,
                30.0,
                1.0,
                *moves,
                EXACT,
            );
        }
        run_colour(
            &format!("keyed colour/all five, {name}"),
            &alone(colour_keyed(&m.noise, &all, shape.as_ref())),
            noise,
            30.0,
            1.0,
            *moves,
            EXACT,
        );
    }
    // The temperature crossing zero: on a frame (-1 to 1 over a second is 0 at frame 15, where `gamma_r` and
    // `gamma_b` are both 1 and the chroma tables are the identity) and between two.
    let crossing = |lo: f64, hi: f64, end: f64| {
        let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
        c.set_property_keys(
            Property::Temperature,
            vec![PropertyKey::new(0.0, lo), PropertyKey::new(end, hi)],
        );
        c
    };
    run_colour(
        "keyed colour/temperature crossing 0 on a frame",
        &alone(crossing(-1.0, 1.0, 1.0)),
        noise,
        30.0,
        1.0,
        20,
        EXACT,
    );
    run_colour(
        "keyed colour/temperature crossing 0 between two",
        &alone(crossing(-0.6, 0.3, 0.9)),
        noise,
        30.0,
        1.0,
        20,
        EXACT,
    );
    run_colour(
        "keyed colour/temperature 1 to 0",
        &alone(crossing(1.0, 0.0, 1.0)),
        noise,
        30.0,
        1.0,
        20,
        EXACT,
    );
    // The frame grids of other rates: 29.97 spelled as the text (2997/100) and as the ratio, 24 and 25 fps.
    for (name, fps) in [
        ("29.97", 29.97),
        ("30000/1001", 30000.0 / 1001.0),
        ("24", 24.0),
        ("25", 25.0),
        ("60", 60.0),
    ] {
        run_colour(
            &format!("keyed colour/all five @{name}"),
            &alone(colour_keyed(&m.noise, &all, &linear_keys)),
            noise,
            fps,
            1.0,
            20,
            EXACT,
        );
    }
    // Late on the timeline, sped up, slowed down, reversed: only the clip's own clock reads the curve.
    let mut late = colour_keyed(&m.noise, &all, &linear_keys);
    late.timeline_start = 1.25;
    run_colour(
        "keyed colour/late on the timeline",
        &alone(late),
        noise,
        30.0,
        2.25,
        20,
        EXACT,
    );
    // ... and late on the grid of 29.97 fps, spelled as the text and as the ratio: the start is off the frame grid, and the
    // time the curve is read at is the clip's own on the `fps` filter's grid.
    for (name, fps) in [("29.97", 29.97), ("30000/1001", 30000.0 / 1001.0)] {
        let mut late = colour_keyed(&m.noise, &all, &linear_keys);
        late.timeline_start = 1.25;
        run_colour(
            &format!("keyed colour/late on the timeline @{name}"),
            &alone(late),
            noise,
            fps,
            2.25,
            20,
            EXACT,
        );
    }
    let mut fast = colour_keyed(&m.noise, &all, &linear_keys);
    (fast.source_out, fast.speed, fast.timeline_start) = (2.5, 2.0, 0.4);
    run_colour("keyed colour/speed 2", &alone(fast), noise, 30.0, 1.4, 20, EXACT);
    let mut slow = colour_keyed(&m.noise, &all, &linear_keys);
    (slow.source_out, slow.speed) = (1.0, 0.5);
    run_colour("keyed colour/speed 0.5", &alone(slow), noise, 30.0, 1.0, 20, EXACT);
    let mut back = colour_keyed(&m.noise, &all, &linear_keys);
    back.speed = -1.0;
    run_colour("keyed colour/reversed", &alone(back), noise, 30.0, 1.0, 20, EXACT);
    // Beside a static grade: the numbers that are not keyed are the clip's own, to the last digit.
    let mut beside = colour_keyed(&m.noise, &[Property::Brightness, Property::Gamma], &linear_keys);
    beside.color = Color {
        contrast: 1.3,
        saturation: 0.8,
        temperature: -0.35,
        ..Color::default()
    };
    run_colour(
        "keyed colour/beside a static grade",
        &alone(beside),
        noise,
        30.0,
        1.0,
        20,
        EXACT,
    );
    let mut warm = colour_keyed(&m.noise, &[Property::Contrast], &linear_keys);
    warm.color.temperature = 0.4;
    run_colour(
        "keyed colour/beside a static temperature",
        &alone(warm),
        noise,
        30.0,
        1.0,
        20,
        EXACT,
    );

    // The clip's fades and the dissolve it enters by act after the `eq`.
    let mut faded = colour_keyed(&m.noise, &all, &linear_keys);
    faded.fade_in = 0.3;
    faded.fade_out = 0.3;
    run_colour("keyed colour/with fades", &alone(faded), noise, 30.0, 1.0, 20, EXACT);
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = colour_keyed(&m.noise, &all, &linear_keys);
    inc.timeline_start = 1.0;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    run_colour(
        "keyed colour/dissolve",
        &timeline(vec![vec![out, inc]], None),
        &[m.bars.clone(), m.noise.clone()],
        30.0,
        2.0,
        20,
        EXACT,
    );
    let mut dipped = colour_keyed(&m.testsrc, &all, &linear_keys);
    dipped.timeline_start = 1.0;
    dipped.transition_in = Some(Transition {
        kind: TransitionKind::DipToBlack,
        duration: 0.6,
    });
    run_colour(
        "keyed colour/dip to black",
        &timeline(vec![vec![clip(&m.testsrc, 0.5, 1.5, 0.0), dipped]], None),
        std::slice::from_ref(&m.testsrc),
        30.0,
        2.0,
        20,
        EXACT,
    );
    // The outgoing clip of a dissolve plays on past its end (its tail) with its `eq` still running: the curve holds its
    // last value there.
    let mut leaving = colour_keyed(&m.testsrc, &all, &linear_keys);
    leaving.timeline_start = 0.0;
    let mut entering = clip(&m.bars, 0.5, 1.5, 1.0);
    entering.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    run_colour(
        "keyed colour/outgoing clip on its tail",
        &timeline(vec![vec![leaving, entering]], None),
        &[m.testsrc.clone(), m.bars.clone()],
        30.0,
        2.0,
        20,
        EXACT,
    );
    // Beside a keyed opacity: one clip, an `eq` and a `geq` alpha each written per frame.
    let over_the_bars = |c: Clip| timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
    let two = [m.bars.clone(), m.testsrc.clone()];
    let mut both = colour_keyed(&m.testsrc, &all, &linear_keys);
    both.set_property_keys(Property::Opacity, linear_keys(1.0, 0.4));
    run_colour(
        "keyed colour/with a keyed opacity",
        &over_the_bars(both),
        &two,
        30.0,
        1.0,
        20,
        EXACT,
    );
    let mut both = colour_keyed(&m.testsrc, &[Property::Brightness, Property::Temperature], &linear_keys);
    both.set_property_keys(Property::Opacity, linear_keys(0.2, 0.9));
    both.fade_in = 0.3;
    run_colour(
        "keyed colour/with a keyed opacity and a fade",
        &over_the_bars(both),
        &two,
        30.0,
        1.0,
        20,
        EXACT,
    );
    // A layer the compositor scales: the scaler is a level off swscale's x86 kernels, and the grade's tables multiply that by
    // their steepest step (`eq::table_steps`: contrast, gamma's power toward black, a black point parked above 0). A frame is
    // drawn while that step is at most two (`GRADED_SCALED`) and FFmpeg's past it (`GradeSteep`), so a keyed ramp is judged one
    // frame at a time: the frames inside are held to the export, the others are refused (and the export renders them all the same).
    let gentle = |c: &mut Clip, gamma_to: f64| {
        for (prop, lo, hi) in [
            (Property::Brightness, -0.1, 0.1),
            (Property::Contrast, 0.9, 1.25),
            (Property::Saturation, 0.8, 1.3),
            (Property::Gamma, 0.85, gamma_to),
            (Property::Temperature, -0.5, 0.5),
        ] {
            c.set_property_keys(prop, linear_keys(lo, hi));
        }
    };
    // (A picture-in-picture at a static offset: a moving one is placed by the export's `perspective`, refused.)
    let moving = |c: &mut Clip| {
        c.transform.scale = 0.6;
        c.transform.pos_x = 0.2;
    };
    let mut gentle_pip = clip(&m.testsrc, 0.5, 1.5, 0.0);
    gentle(&mut gentle_pip, 1.1);
    moving(&mut gentle_pip);
    run_colour(
        "keyed colour/pip, gentle",
        &over_the_bars(gentle_pip),
        &two,
        30.0,
        1.0,
        20,
        GRADED_SCALED,
    );
    // ... with the gamma going on to 1.2 the ramp's tables step by 3 on one frame (a cliff at the dark end: the first entry
    // above 0 is 3), which is that frame's alone.
    let mut gentler_pip = clip(&m.testsrc, 0.5, 1.5, 0.0);
    gentle(&mut gentler_pip, 1.2);
    moving(&mut gentler_pip);
    run_grade_steep(
        "keyed colour/pip, gamma to 1.2",
        &over_the_bars(gentler_pip),
        &two,
        1.0,
        GRADED_SCALED,
        Some(1),
    );
    // The far ends of every range: the early frames of the ramp (contrast 0.5, saturation 0, gamma 0.5) are inside, the rest steepen.
    let mut steep_pip = colour_keyed(&m.testsrc, &all, &linear_keys);
    moving(&mut steep_pip);
    run_grade_steep(
        "keyed colour/pip, steep",
        &over_the_bars(steep_pip),
        &two,
        1.0,
        GRADED_SCALED,
        None,
    );
    // Letterboxed: `pad`'s bars are in the picture `eq` sees, and go through the tables of the frame (the portrait
    // picture is shrunk into the frame: the scaler's level again).
    let delivery = Some(Delivery::new(640, 360, Fit::Contain));
    let mut boxed = clip(&m.portrait, 0.5, 1.5, 0.0);
    gentle(&mut boxed, 1.1);
    run_colour(
        "keyed colour/letterboxed, gentle",
        &timeline(vec![vec![boxed]], delivery),
        std::slice::from_ref(&m.portrait),
        30.0,
        1.0,
        20,
        GRADED_SCALED,
    );
    let boxed = colour_keyed(&m.portrait, &all, &linear_keys);
    run_grade_steep(
        "keyed colour/letterboxed, steep",
        &timeline(vec![vec![boxed]], delivery),
        std::slice::from_ref(&m.portrait),
        1.0,
        GRADED_SCALED,
        None,
    );
    // Turned (`rotate` follows the `eq`, on the alpha plane) and translucent (the RGB round trip a constant opacity takes
    // reads the graded picture). A turn that is not a quarter is refused whole (its edge is a fixed-point sample), whatever the grade.
    let mut turned = clip(&m.smooth, 0.5, 1.5, 0.0);
    turned.set_property_keys(Property::Brightness, linear_keys(-0.1, 0.1));
    turned.set_property_keys(Property::Temperature, linear_keys(-0.5, 0.5));
    turned.transform.rotation = 12.0;
    turned.transform.scale = 0.6;
    let on_black = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0)], vec![turned]], None);
    let smooth = [m.black.clone(), m.smooth.clone()];
    colour_moves("keyed colour/rotated", &on_black, &smooth, 30.0, 1.0, 20);
    run_refusing(
        "keyed colour/rotated",
        &on_black,
        &smooth,
        30.0,
        1.0,
        EXACT,
        &rotation_refused(),
    );
    let mut translucent = colour_keyed(&m.testsrc, &all, &linear_keys);
    translucent.transform.opacity = 0.6;
    run_colour(
        "keyed colour/translucent",
        &over_the_bars(translucent),
        &two,
        30.0,
        1.0,
        20,
        TRANSLUCENT,
    );
    // The tables of the frame ahead of the effects that follow: composed with a grayscale or an invert, and
    // ahead of a blur.
    for (name, effects, limits) in [
        ("invert", vec![kerf_core::VideoEffect::Invert], EXACT),
        (
            "grayscale then invert",
            vec![kerf_core::VideoEffect::Grayscale, kerf_core::VideoEffect::Invert],
            EXACT,
        ),
        (
            "blur then grayscale",
            vec![kerf_core::VideoEffect::Blur { sigma: 3.0 }, kerf_core::VideoEffect::Grayscale],
            EFFECT,
        ),
        ("sharpen", vec![kerf_core::VideoEffect::Sharpen { amount: 1.0 }], EXACT),
        ("vignette", vec![kerf_core::VideoEffect::Vignette], EXACT),
    ] {
        let mut c = colour_keyed(&m.testsrc, &all, &linear_keys);
        c.effects = effects;
        run_colour(
            &format!("keyed colour/then {name}"),
            &over_the_bars(c),
            &two,
            30.0,
            1.0,
            20,
            limits,
        );
    }
    // A mask is cut after the grade.
    let mut masked = colour_keyed(&m.testsrc, &all, &linear_keys);
    masked.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.1, false));
    run_colour(
        "keyed colour/under a mask",
        &over_the_bars(masked),
        &two,
        30.0,
        1.0,
        20,
        EXACT,
    );
    // A moving zoom comes last: the grade is at the fit size, magnified with the picture.
    let mut zoomed = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
    for prop in all {
        zoomed.set_property_keys(prop, linear_keys(colour_span(prop).0, colour_span(prop).1));
    }
    let tl = over_the_bars(zoomed);
    run_placed("keyed colour/under a moving zoom", &tl, &two, 30.0, 1.0);
}

/// A grade on a layer the scaler resizes: the scaler is a level off swscale's and the tables multiply that by their steepest
/// step (`eq::table_steps`), so a frame is drawn while that step is at most two and FFmpeg's past it (`GradeSteep`), static
/// grades included. The drawn ones are held to **the step itself** (the bound the plan judged them by) on sources that look at
/// every table entry: noise, a luma ramp, and hard 16 / 235 edges whose scaled overshoot reaches the bottom of the range. The
/// refused ones are the grades measured far past it with the refusal bypassed (noise, edges and the ramp, Y / UV off the export:
/// gamma 1.3 3 / 1, 1.4 4 / 1, 2 6 / 1; contrast 3 3 / 1; contrast 1.8 and gamma 2 12 / 1, with a brightness of 0.3 19 / 1;
/// saturation 3 1 / 3), here refused on every frame with the export rendering them.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_grade_on_a_layer_the_scaler_resizes_is_drawn_while_its_tables_step_by_two_and_refused_past_it() {
    use kerf_core::eq::table_steps;
    let m = media();
    let grade = |brightness: f64, contrast: f64, saturation: f64, gamma: f64, temperature: f64| Color {
        brightness,
        contrast,
        saturation,
        gamma,
        temperature,
    };
    let pip = |src: &Asset, color: Color| {
        let mut c = clip(src, 0.5, 1.5, 0.0);
        c.transform.scale = 0.6;
        c.color = color;
        (
            timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
            [m.bars.clone(), src.clone()],
        )
    };
    let (noise, edges, ramp) = (("noise", &m.noise), ("edges", &m.edges), ("ramp", &m.ramp));
    for (name, color, sources) in [
        // Fixed point: contrast steps by its ceiling and a brightness has no cliff.
        ("brightness 0.3", grade(0.3, 1.0, 1.0, 1.0, 0.0), vec![ramp, edges]),
        ("contrast 2", grade(0.0, 2.0, 1.0, 1.0, 0.0), vec![ramp, edges]),
        // The pow table: a gamma above 1 steepens toward black (2 at 1.2); a brightness that lifts the dark end off the
        // table's cliff takes the steepness away again (gamma 2, step 1).
        ("gamma 1.2", grade(0.0, 1.0, 1.0, 1.2, 0.0), vec![ramp, edges, noise]),
        ("gamma 0.5", grade(0.0, 1.0, 1.0, 0.5, 0.0), vec![ramp]),
        (
            "brightness 0.3 and gamma 2",
            grade(0.3, 1.0, 1.0, 2.0, 0.0),
            vec![ramp, edges],
        ),
        // Chroma: saturation is the tables' contrast, a temperature a power of the chroma.
        ("saturation 2", grade(0.0, 1.0, 2.0, 1.0, 0.0), vec![noise, ramp]),
        ("temperature 1", grade(0.0, 1.0, 1.0, 1.0, 1.0), vec![noise]),
    ] {
        let [y, u, v] = table_steps(&color);
        assert!(y.max(u).max(v) <= 2, "{name}: steps {y}/{u}/{v} are past what is drawn");
        let limits = Limits {
            y: y as i32,
            uv: u.max(v) as i32,
        };
        for (source, asset) in sources {
            let (tl, assets) = pip(asset, color);
            run(
                &format!("static colour/pip, {name}, {source}"),
                &tl,
                &assets,
                30.0,
                0.4,
                limits,
            );
        }
    }
    // Past two, every frame is FFmpeg's. The dark end by gamma (the first table entry above 0 steps by 256 * (1/255)^(1/gamma)),
    // by a brightness that parks the black point above 0 under a gamma, by contrast, by saturation, and at the far end of every range.
    for (name, color) in [
        ("gamma 1.3", grade(0.0, 1.0, 1.0, 1.3, 0.0)),
        ("gamma 1.4", grade(0.0, 1.0, 1.0, 1.4, 0.0)),
        ("gamma 2", grade(0.0, 1.0, 1.0, 2.0, 0.0)),
        ("brightness -0.3 and gamma 1.5", grade(-0.3, 1.0, 1.0, 1.5, 0.0)),
        ("contrast 3", grade(0.0, 3.0, 1.0, 1.0, 0.0)),
        ("contrast 1.8 and gamma 2", grade(0.0, 1.8, 1.0, 2.0, 0.0)),
        ("saturation 3", grade(0.0, 1.0, 3.0, 1.0, 0.0)),
        ("the far end of every range", grade(0.3, 1.8, 2.0, 2.0, 1.0)),
    ] {
        let steps = table_steps(&color);
        assert!(steps.into_iter().max().unwrap() > 2, "{name}: {steps:?}");
        let (tl, assets) = pip(&m.ramp, color);
        let case = format!("static colour/pip, {name}, refused");
        assert_eq!(grade_steep_split(&tl, &assets, 30.0, 0.4), (12, 0), "{case}");
        run_refusing(&case, &tl, &assets, 30.0, 0.4, GRADED_SCALED, &grade_steep_refused());
    }
    // The same far end at the picture's size is exact (nothing is resized): drawn, to the level.
    let mut alone = clip(&m.ramp, 0.5, 1.5, 0.0);
    alone.color = grade(0.3, 1.8, 2.0, 2.0, 1.0);
    run(
        "static colour/the far end of every range, unscaled",
        &timeline(vec![vec![alone]], None),
        std::slice::from_ref(&m.ramp),
        30.0,
        0.4,
        EXACT,
    );
}

/// The colour is graded in the picture the chain has made 4:2:0 and 8-bit by then, so a keyed colour is exact
/// on a 10-bit, 4:2:2, 4:4:4, gray or RGB source and on a BT.709 one, as a static grade is. (A deeper-than-8-bit
/// picture may be a full-range one, which FFmpeg 9 grades before it converts: it is FFmpeg's there.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_keyed_colour_is_drawn_on_pictures_that_are_not_plain_8_bit_420() {
    use kerf_core::Property;
    let v = variants();
    for (name, a) in [
        ("10-bit", &v.tenbit),
        ("4:4:4", &v.yuv444),
        ("4:2:2", &v.yuv422),
        ("gray", &v.gray),
        ("rgb png", &v.rgb_png),
        ("bt709", &v.testsrc709),
    ] {
        let (lo, hi) = if a.streams[0].image { (0.0, 1.0) } else { (0.5, 1.5) };
        let mut keyed = clip(a, lo, hi, 0.0);
        for prop in Property::COLOR {
            let (from, to) = colour_span(prop);
            keyed.set_property_keys(prop, linear_keys(from, to));
        }
        let tl = timeline(vec![vec![keyed]], None);
        let case = format!("keyed colour/{name}");
        let one = std::slice::from_ref(a);
        if name == "10-bit" && policy() != CompositeColorPolicy::FixedBt601 {
            run_refusing(&case, &tl, one, 30.0, 1.0, EXACT, &full_range_cut_refused());
        } else {
            run_colour(&case, &tl, one, 30.0, 1.0, 20, EXACT);
        }
    }
}

/// A keyed colour is `eq` in the clip's chain on every frame, so a refusal about grading a picture judges
/// the clip: on a full-range picture FFmpeg 9 grades before it converts the range, and the compositor
/// converts first, so every frame is FFmpeg's, the ones where the curve passes through neutral included.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_keyed_colour_on_a_full_range_picture_is_refused_on_every_frame() {
    use kerf_core::{Property, PropertyKey};
    let v = variants();
    let m = media();
    for (name, a) in [("mjpeg", &v.mjpeg), ("jpeg-still", &v.jpeg)] {
        let (lo, hi) = if a.streams[0].image { (0.0, 1.0) } else { (0.5, 1.5) };
        let mut c = clip(a, lo, hi, 0.0);
        // Brightness 0 at the first frame: the numbers there are the identity.
        c.set_property_keys(
            Property::Brightness,
            vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(1.0, 0.3)],
        );
        let tl = timeline(vec![vec![c]], None);
        let opts = at_fps(30.0);
        let planner = Planner::new(&tl, std::slice::from_ref(a), &opts, PlanRequest::motion(policy())).expect("planner");
        for k in upto(1.0, 30.0) {
            let plan = planner.at_frame(k).expect("plan");
            let why = plan.reasons(&compositor().caps(), plan.size(u32::MAX));
            assert!(
                why.iter().any(|u| matches!(u, Unsupported::GradedFullRange(..))),
                "keyed colour/full-range {name}: frame {k} (colour {:?}) is not refused for grading a full-range picture: {why:?}",
                plan.layers[0].color
            );
        }
        eprintln!(
            "{:<44} every frame refused for grading a full-range picture, FFmpeg renders them",
            format!("keyed colour/full-range {name}")
        );
        // The same colour on limited-range footage is drawn (the cases above), and so is the full-range clip ungraded.
        let plain = timeline(vec![vec![clip(a, lo, hi, 0.0)]], None);
        let plan = Planner::new(&plain, std::slice::from_ref(a), &opts, PlanRequest::motion(policy()))
            .expect("planner")
            .at_frame(3)
            .expect("plan");
        assert!(
            !plan
                .reasons(&compositor().caps(), plan.size(u32::MAX))
                .iter()
                .any(|u| matches!(u, Unsupported::GradedFullRange(..))),
            "{name}: an ungraded full-range clip is not a graded one"
        );
    }
    // Where range is negotiated (FFmpeg 9), a limited-range layer with a keyed colour above a full-range one.
    let mut over = colour_keyed(&m.testsrc, &[Property::Brightness], &|_, hi| linear_keys(0.0, hi));
    over.transform.scale = 0.5;
    let tl = timeline(vec![vec![clip(&v.mjpeg, 0.5, 1.5, 0.0)], vec![over]], None);
    let assets = [v.mjpeg.clone(), m.testsrc.clone()];
    let opts = at_fps(30.0);
    let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let refused = upto(1.0, 30.0)
        .filter(|&k| {
            let plan = planner.at_frame(k).expect("plan");
            plan.reasons(&compositor().caps(), plan.size(u32::MAX))
                .iter()
                .any(|u| matches!(u, Unsupported::GradedInFullRangeStack(_) | Unsupported::FullRangeCut))
        })
        .count();
    match policy() {
        CompositeColorPolicy::FixedBt601 => assert_eq!(
            refused, 0,
            "FFmpeg 6 converts the range at the decode, as the compositor does"
        ),
        _ => assert_eq!(refused, 30, "where range is negotiated the whole stack is FFmpeg's"),
    }
    eprintln!(
        "keyed colour/over a full-range layer: {refused} of 30 frames refused under {:?}",
        policy()
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_rotated_layer_fades_on_an_alpha_plane() {
    let m = media();
    let assets = [m.black.clone(), m.smooth.clone()];
    // A static rotation puts the chain on `yuva420p` for good: its fades take a black of 0.
    let under = clip(&m.black, 0.5, 1.5, 0.0);
    let mut turned = clip(&m.smooth, 0.5, 1.5, 0.0);
    turned.transform.rotation = 20.0;
    turned.transform.scale = 0.6;
    turned.fade_in = 0.4;
    turned.fade_out = 0.4;
    let tl = timeline(vec![vec![under.clone()], vec![turned]], None);
    run_refusing("fade/rotated", &tl, &assets, 30.0, 1.0, EXACT, &rotation_refused());
    // (A quarter turn is drawn, to the sample: the chain is on the same alpha plane.)
    let mut quarter = clip(&m.smooth, 0.5, 1.5, 0.0);
    quarter.transform.rotation = 90.0;
    quarter.fade_in = 0.4;
    quarter.fade_out = 0.4;
    run(
        "fade/rotated a quarter",
        &timeline(vec![vec![under.clone()], vec![quarter]], None),
        &assets,
        30.0,
        1.0,
        EXACT,
    );
    // ... and a dissolve into a rotated clip, over the clip it replaces.
    let out = clip(&m.black, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.smooth, 0.5, 1.5, 1.0);
    inc.transform.rotation = -15.0;
    inc.transform.scale = 0.6;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run_refusing(
        "dissolve/rotated-incoming",
        &tl,
        &assets,
        30.0,
        2.0,
        EXACT,
        &rotation_refused(),
    );
    let out = clip(&m.black, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.smooth, 0.5, 1.5, 1.0);
    inc.transform.rotation = -90.0;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    run(
        "dissolve/rotated-incoming a quarter",
        &timeline(vec![vec![out, inc]], None),
        &assets,
        30.0,
        2.0,
        EXACT,
    );
    // A rotated clip with no fade at all, as an export draws it.
    let mut still_turned = clip(&m.smooth, 0.5, 1.5, 0.0);
    still_turned.transform.rotation = 20.0;
    still_turned.transform.scale = 0.6;
    let tl = timeline(vec![vec![under], vec![still_turned]], None);
    run_refusing("plain/rotated", &tl, &assets, 30.0, 1.0, EXACT, &rotation_refused());
    let mut quarter = clip(&m.smooth, 0.5, 1.5, 0.0);
    quarter.transform.rotation = 270.0;
    let tl = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0)], vec![quarter]], None);
    run("plain/rotated a quarter", &tl, &assets, 30.0, 1.0, EXACT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_translucent_layer_dissolves_and_is_refused_its_fades_to_black() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    // A dissolve into a translucent clip: the ramp is on the alpha of the RGBA picture
    // `colorchannelmixer` leaves, ahead of the conversion back to `yuva420p`.
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transform.opacity = 0.7;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run("dissolve/translucent-incoming", &tl, &assets, 30.0, 2.0, TRANSLUCENT);
    // A fade to black runs on that RGBA picture, alpha included, which the compositor does not
    // reproduce: the plan refuses the frames it runs on (FFmpeg draws them), and the others are drawn.
    let under = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut glass = clip(&m.testsrc, 0.5, 1.5, 0.0);
    glass.transform.opacity = 0.6;
    glass.fade_in = 0.4;
    glass.fade_out = 0.4;
    let tl = timeline(vec![vec![under], vec![glass]], None);
    run_refusing(
        "fade/translucent",
        &tl,
        &assets,
        30.0,
        1.0,
        TRANSLUCENT,
        &translucent_fade_refused(),
    );
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transform.opacity = 0.7;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::DipToBlack,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run_refusing(
        "dip/black translucent-incoming",
        &tl,
        &assets,
        30.0,
        2.0,
        TRANSLUCENT,
        &translucent_fade_refused(),
    );
}

// ---- other pictures: pixel formats, tags, full range -----------------------------------------

/// The test pattern in the pictures a dip to white or a fade treats differently: what the round
/// trip of a dip was not measured on, what FFmpeg 9 fades before converting its range, a BT.709 tag.
struct Variants {
    tenbit: Asset,
    /// 10-bit tagged full range (`-color_range pc`): ffprobe names it `yuv420p10le`, like the limited
    /// one. The probe records the tag now, but a limited file may say nothing, so under FFmpeg 9 the
    /// plan still refuses a deeper picture by its depth; the tag settles it where it is there.
    tenbit_pc: Asset,
    yuv444: Asset,
    yuv422: Asset,
    gray: Asset,
    /// A busy RGB PNG (a still image, `rgb24`).
    rgb_png: Asset,
    /// A full-range (`yuvj420p`) MJPEG clip and a JPEG still.
    mjpeg: Asset,
    jpeg: Asset,
    bars709: Asset,
    testsrc709: Asset,
    /// The test pattern tagged as an NTSC (`smpte170m`) and a PAL (`bt470bg`) file is: BT.601's coefficients under a tag.
    testsrc170m: Asset,
    testsrc470bg: Asset,
    bars470bg: Asset,
}

/// The file's range tag as ffprobe reads it (`pc`, `tv`, or empty when untagged).
fn color_range_tag(path: &Path) -> String {
    let probe = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
    let out = Command::new(probe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_range"])
        .args(["-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn variants() -> &'static Variants {
    static V: OnceLock<Variants> = OnceLock::new();
    V.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-variants");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, lavfi: &str, pix: &str, codec: &[&str]| -> Asset {
            let p = dir.join(name);
            let mut args = vec!["-f", "lavfi", "-i", lavfi];
            args.extend_from_slice(codec);
            args.extend_from_slice(&["-pix_fmt", pix, p.to_str().unwrap()]);
            ffmpeg(&args);
            probed(&p)
        };
        let ts = "testsrc2=size=640x360:rate=30:duration=4";
        let bt709 = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
        let smpte170m = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";
        let bt470bg = "setparams=colorspace=bt470bg:color_primaries=bt470bg:color_trc=bt470bg";
        let x264 = ["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
        let still = ["-frames:v", "1"];
        Variants {
            tenbit: make("tenbit.mp4", ts, "yuv420p10le", &x264),
            tenbit_pc: {
                let pc = [
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
                ];
                let a = make("tenbit_pc.mp4", ts, "yuv420p10le", &pc);
                assert_eq!(
                    a.streams[0].pix_fmt.as_deref(),
                    Some("yuv420p10le"),
                    "it probes as an ordinary 10-bit picture"
                );
                assert_eq!(
                    color_range_tag(Path::new(&a.path)),
                    "pc",
                    "and it really is tagged full range"
                );
                a
            },
            yuv444: make("yuv444.mkv", ts, "yuv444p", &["-c:v", "ffv1"]),
            yuv422: make("yuv422.mkv", ts, "yuv422p", &["-c:v", "ffv1"]),
            gray: make("gray.mkv", ts, "gray", &["-c:v", "ffv1"]),
            rgb_png: make("rgb.png", "testsrc2=size=640x360:rate=1:duration=1", "rgb24", &still),
            mjpeg: make("fullrange.mkv", ts, "yuvj420p", &["-c:v", "mjpeg", "-qscale:v", "1"]),
            jpeg: make("still.jpg", "testsrc2=size=640x360:rate=1:duration=1", "yuvj420p", &still),
            bars709: make(
                "bars709.mp4",
                &format!("smptehdbars=size=640x360:rate=30:duration=4,{bt709}"),
                "yuv420p",
                &x264,
            ),
            testsrc709: make("testsrc709.mp4", &format!("{ts},{bt709}"), "yuv420p", &x264),
            testsrc170m: make("testsrc170m.mp4", &format!("{ts},{smpte170m}"), "yuv420p", &x264),
            testsrc470bg: make("testsrc470bg.mp4", &format!("{ts},{bt470bg}"), "yuv420p", &x264),
            bars470bg: make(
                "bars470bg.mp4",
                &format!("smptehdbars=size=640x360:rate=30:duration=4,{bt470bg}"),
                "yuv420p",
                &x264,
            ),
        }
    })
}

/// The cut every case of this section is made of: the black clip, then `incoming` (the picture
/// under test) entering through `kind` over `secs`, `scale` of the frame, in `format`.
fn after_black(
    incoming: &Asset,
    kind: TransitionKind,
    secs: f64,
    scale: f64,
    format: Option<Delivery>,
) -> (Timeline, Vec<Asset>) {
    let m = media();
    let (lo, hi) = if incoming.streams[0].image { (0.0, 1.0) } else { (0.5, 1.5) };
    let mut inc = clip(incoming, lo, hi, 1.0);
    inc.transform.scale = scale;
    inc.transition_in = Some(Transition { kind, duration: secs });
    (
        timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0), inc]], format),
        vec![m.black.clone(), incoming.clone()],
    )
}

/// A dip to white is `yuv420p -> rgb24 -> yuv420p` around the fade, drawn for the pictures the round
/// trip was measured on (8-bit limited-range 4:2:0, untagged: [`WHITE`]; BT.709: [`WHITE_709`]) and
/// FFmpeg's for every other. What it is refused for was measured, and each refusal is held here:
/// a letterbox (FFmpeg pads before the round trip, so the bars whiten as well: the first probe read Y 126
/// against 16 at half way), a picture with an odd side (the round trip reads past it), a layer the
/// scaler resizes (the `scale` filter converts to RGB itself) and every picture that is not `yuv420p` (RGB 11 / 12 levels of Y / UV off, 10-bit 9 / 16, 4:4:4 16 / 17,
/// 4:2:2 9 / 13, gray 2, full range up to 21). The dip to black on the same pictures is drawn
/// exactly, which is how the frames either side of a refusal are held.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_dip_to_white_is_drawn_only_where_its_round_trip_was_measured() {
    let m = media();
    let v = variants();
    let (white, black) = (TransitionKind::DipToWhite, TransitionKind::DipToBlack);
    // Drawn: BT.709 footage (both clips tagged, so the stack has one matrix), and a half-size layer.
    let mut inc = clip(&v.testsrc709, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: white,
        duration: 0.8,
    });
    let tl = timeline(vec![vec![clip(&v.bars709, 0.5, 1.5, 0.0), inc]], None);
    run(
        "dip/white bt709",
        &tl,
        &[v.bars709.clone(), v.testsrc709.clone()],
        30.0,
        2.0,
        WHITE_709,
    );
    // A layer smaller than the frame that is not resized (a crop, a Cover fit at the footage's scale)
    // is drawn, its edge included.
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transform.crop_left = 0.25;
    inc.transform.crop_right = 0.25;
    inc.transition_in = Some(Transition {
        kind: white,
        duration: 0.8,
    });
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), inc]], None);
    let assets = [m.bars.clone(), m.testsrc.clone()];
    run("dip/white cropped layer", &tl, &assets, 30.0, 2.0, WHITE);
    let cover = Some(Delivery::new(480, 360, Fit::Cover));
    let (tl, assets) = after_black(&m.testsrc, white, 0.8, 1.0, cover);
    run("dip/white cover 480x360", &tl, &assets, 30.0, 2.0, WHITE);
    // Refused: a layer the scaler resizes, which the scale filter converts to RGB itself (the first
    // measurement of a half-size layer: Y 19 / UV 28 off at the picture's sharp edges, and 11 / 10 for
    // an enlargement). The dip to black on it is drawn.
    for (name, scale, format) in [
        ("half-size", 0.5, None),
        ("three-quarter-size", 0.75, None),
        ("shrunk to 320x180", 1.0, Some(Delivery::new(320, 180, Fit::Contain))),
        ("enlarged to 960x540", 1.0, Some(Delivery::new(960, 540, Fit::Contain))),
    ] {
        let (tl, assets) = after_black(&m.testsrc, white, 0.8, scale, format);
        run_refusing(
            &format!("dip/white {name}"),
            &tl,
            &assets,
            30.0,
            2.0,
            EXACT,
            &dip_refused(DipWhy::Resized),
        );
        let (tl, assets) = after_black(&m.testsrc, black, 0.8, scale, format);
        run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0, SCALED);
    }
    // Refused: a letterboxed layer.
    let tall = Some(Delivery::new(640, 360, Fit::Contain));
    let (tl, assets) = after_black(&m.portrait, white, 0.8, 1.0, tall);
    run_refusing(
        "dip/white letterboxed",
        &tl,
        &assets,
        30.0,
        2.0,
        EXACT,
        &dip_refused(DipWhy::Letterboxed),
    );
    let (tl, assets) = after_black(&m.portrait, black, 0.8, 1.0, tall);
    run("dip/black letterboxed", &tl, &assets, 30.0, 2.0, SCALED);
    // ... a layer of odd size (0.4985 of 640x360 is 319x179), which a dip to black draws.
    let (tl, assets) = after_black(&m.testsrc, white, 0.8, 0.4985, None);
    run_refusing(
        "dip/white odd layer",
        &tl,
        &assets,
        30.0,
        2.0,
        EXACT,
        &dip_refused(DipWhy::OddSize(319, 179)),
    );
    let (tl, assets) = after_black(&m.testsrc, black, 0.8, 0.4985, None);
    run("dip/black odd layer", &tl, &assets, 30.0, 2.0, SCALED);
    // ... every picture that is not 8-bit limited-range 4:2:0.
    for (name, a) in [
        ("rgb-png", &v.rgb_png),
        ("10-bit", &v.tenbit),
        ("4:4:4", &v.yuv444),
        ("4:2:2", &v.yuv422),
        ("gray", &v.gray),
    ] {
        // A 10-bit picture is maybe full range where FFmpeg 9 works on raw values (its tag is not recorded):
        // the cut is refused whole there, whatever the dip.
        let negotiated = name == "10-bit" && policy() != CompositeColorPolicy::FixedBt601;
        let (tl, assets) = after_black(a, white, 0.8, 1.0, None);
        let refused_for = if negotiated {
            full_range_cut_refused()
        } else {
            dip_refused(DipWhy::Format)
        };
        run_refusing(&format!("dip/white {name}"), &tl, &assets, 30.0, 2.0, EXACT, &refused_for);
        let (tl, assets) = after_black(a, black, 0.8, 1.0, None);
        if negotiated {
            run_refusing(
                &format!("dip/black {name}"),
                &tl,
                &assets,
                30.0,
                2.0,
                EXACT,
                &full_range_cut_refused(),
            );
        } else {
            run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0, EXACT);
        }
    }
}

/// Under FFmpeg 6 a full-range (`yuvj420p`) picture is converted to limited range at the decode, as
/// the compositor does, and everything below is drawn to the level. FFmpeg 9 keeps its raw
/// values through the graph (the export's planes hold them: 17 levels of Y and 16 of UV off a
/// compositor that converts first, in every frame, and the stream is tagged full range as a
/// whole) and `fade` darkens them toward the limited-range black 16 before the range is converted
/// (frame 0 of a fade-in reads Y 30 where black is 16), so the plan refuses the picture in an
/// export frame of the cut that holds it and inside a fade, a dip or a dissolve in a still, on that FFmpeg.
fn run_full_range(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, limits: Limits) {
    match policy() {
        CompositeColorPolicy::FixedBt601 => run(case, tl, assets, fps, secs, limits),
        _ => run_refusing(case, tl, assets, fps, secs, limits, &full_range_cut_refused()),
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_full_range_picture_is_drawn_as_the_ffmpeg_in_use_converts_its_range() {
    let m = media();
    let v = variants();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    for (name, a) in [("mjpeg", &v.mjpeg), ("jpeg-still", &v.jpeg)] {
        let (lo, hi) = if a.streams[0].image { (0.0, 1.0) } else { (0.5, 1.5) };
        assert_eq!(a.streams[0].pix_fmt.as_deref(), Some("yuvj420p"));
        let mut fading = clip(a, lo, hi, 0.0);
        fading.fade_in = 0.5;
        fading.fade_out = 0.5;
        let tl = timeline(vec![vec![fading]], None);
        let one = std::slice::from_ref(a);
        run_full_range(&format!("full-range/fade {name}"), &tl, one, 30.0, 1.0, EXACT);
        let tl = timeline(vec![vec![clip(a, lo, hi, 0.0)]], None);
        run_full_range(&format!("full-range/plain {name}"), &tl, one, 30.0, 1.0, EXACT);
        // A cut between two full-range clips: FFmpeg 9 flags the export as full range as a whole,
        // so a limited-range clip in the same cut is read 16 levels off too (the black before it).
        let cuts = [
            ("dip-black", TransitionKind::DipToBlack, 0.8),
            ("dissolve", TransitionKind::Crossfade, 0.6),
        ];
        for (label, kind, secs) in cuts {
            let mut inc = clip(a, lo, hi, 1.0);
            inc.transition_in = Some(Transition { kind, duration: secs });
            let tl = timeline(vec![vec![clip(a, lo, hi, 0.0), inc]], None);
            run_full_range(
                &format!("full-range/{label} {name}"),
                &tl,
                &[a.clone(), a.clone()],
                30.0,
                2.0,
                EXACT,
            );
        }
        // A dip to white on it is a picture the round trip was not measured on, whatever FFmpeg does with the range.
        let mut inc = clip(a, lo, hi, 1.0);
        inc.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 0.8,
        });
        let tl = timeline(vec![vec![clip(a, lo, hi, 0.0), inc]], None);
        let reason = match policy() {
            CompositeColorPolicy::FixedBt601 => dip_refused(DipWhy::Format),
            _ => full_range_cut_refused(),
        };
        run_refusing(
            &format!("full-range/dip-white {name}"),
            &tl,
            &[a.clone(), a.clone()],
            30.0,
            2.0,
            EXACT,
            &reason,
        );
    }
    // The scaler resizes it (the clip into a 960x540 frame): swscale converts the range inside the resize,
    // which reads up to 15 levels of chroma off whichever FFmpeg this is, so it is refused under both.
    let enlarged = Some(Delivery::new(960, 540, Fit::Contain));
    let tl = timeline(vec![vec![clip(&v.mjpeg, 0.5, 1.5, 0.0)]], enlarged);
    run_refusing(
        "full-range/plain mjpeg enlarged",
        &tl,
        std::slice::from_ref(&v.mjpeg),
        30.0,
        1.0,
        EXACT,
        &full_range_resized_refused(),
    );
    // A 10-bit file tagged full range probes as `yuv420p10le`, like the limited one, and 9.0.2 handles it as it
    // does `yuvj`: raw values kept, the export flagged full range, every other frame of the cut converted
    // (measured Y 16 off). The plan cannot tell the two apart, so under FFmpeg 9 every deeper-than-8-bit
    // picture counts as maybe full range, the limited one included; FFmpeg 6 draws both to the level.
    let tenbit = &v.tenbit;
    for (name, a) in [("pc-tagged 10-bit", &v.tenbit_pc), ("limited 10-bit", tenbit)] {
        let tl = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0), clip(a, 0.5, 1.5, 1.0)]], None);
        run_full_range(
            &format!("full-range/black then {name}"),
            &tl,
            &[m.black.clone(), a.clone()],
            30.0,
            2.0,
            EXACT,
        );
        let mut fading = clip(a, 0.5, 1.5, 0.0);
        fading.fade_in = 0.5;
        fading.fade_out = 0.5;
        let tl = timeline(vec![vec![fading]], None);
        run_full_range(
            &format!("full-range/fade {name}"),
            &tl,
            std::slice::from_ref(a),
            30.0,
            1.0,
            EXACT,
        );
    }
    // The cut is what FFmpeg 9 flags full range, not the frame: the limited-range frames of a cut that
    // holds a full-range clip somewhere read 16 levels off too (black Y 0 against 16, the test pattern
    // 76 against 81), before the clip, after it, and on another track. Every frame of these is refused
    // there, whatever it shows; under FFmpeg 6 all of it is drawn to the level.
    let (black, testsrc, mjpeg, jpeg) = (&m.black, &m.testsrc, &v.mjpeg, &v.jpeg);
    let tl = timeline(vec![vec![clip(black, 0.5, 1.5, 0.0), clip(mjpeg, 0.5, 1.5, 1.0)]], None);
    run_full_range(
        "full-range/black then full-range",
        &tl,
        &[black.clone(), mjpeg.clone()],
        30.0,
        2.0,
        EXACT,
    );
    let tl = timeline(vec![vec![clip(mjpeg, 0.5, 1.5, 0.0), clip(testsrc, 0.5, 1.5, 1.0)]], None);
    run_full_range(
        "full-range/full-range then limited",
        &tl,
        &[testsrc.clone(), mjpeg.clone()],
        30.0,
        2.0,
        EXACT,
    );
    let tl = timeline(
        vec![vec![clip(testsrc, 0.5, 2.5, 0.0)], vec![clip(jpeg, 0.0, 1.0, 1.0)]],
        None,
    );
    run_full_range(
        "full-range/on another track, later",
        &tl,
        &[testsrc.clone(), jpeg.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // A limited-range layer fading over a full-range one: the stack's export is the raw values.
    let mut over = clip(&m.testsrc, 0.5, 1.5, 0.0);
    over.transform.scale = 0.5;
    over.fade_in = 0.4;
    over.fade_out = 0.4;
    let tl = timeline(vec![vec![clip(&v.mjpeg, 0.5, 1.5, 0.0)], vec![over]], None);
    run_full_range(
        "full-range/under a fading layer",
        &tl,
        &[v.mjpeg.clone(), m.testsrc.clone()],
        30.0,
        1.0,
        SCALED,
    );
}

// ---- a moving zoom comes last; masks; effects -----------------------------------------------------
//
// The export runs a clip with a moving zoom as `crop, fit, setsar, fps, eq, effects, format=yuva420p,
// mask, rotate, fades`, then places the picture on the canvas at sub-pixel positions (`pad`, a per-frame
// `perspective`, `crop`: `video_clip_chain`). No compositor pass draws that placement, so every export
// frame of a moving zoom (or position) is refused (`KeyedPlacement`) and FFmpeg renders it; the cases
// below hold the plan to that. (The compositor's zoom-last order, `GpuCaps::keyed_zoom`, is the scrubbed
// still's, whose chain still ends in the zoom's `scale`.)

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

/// A one-second clip of `asset` with a zoom from `from` to `to` (and the position it ends at).
fn zooming(asset: &Asset, from: f64, to: f64, pos: (f64, f64)) -> Clip {
    let mut c = clip(asset, 0.5, 1.5, 0.0);
    c.keyframes = vec![zoom_key(0.0, from, 0.0, 0.0), zoom_key(1.0, to, pos.0, pos.1)];
    c
}

fn mask_of(shape: kerf_core::MaskShape, feather: f64, inverted: bool) -> kerf_core::Mask {
    kerf_core::Mask {
        shape,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather,
        inverted,
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_zoom_in_an_export_frame_is_refused() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    // Zoom in (the picture grows past the frame) and out (the layer below shows around it).
    for (name, from, to) in [("in", 1.0, 1.6), ("out", 1.0, 0.55), ("in from small", 0.5, 1.2)] {
        let tl = timeline(
            vec![
                vec![clip(&m.bars, 0.5, 1.5, 0.0)],
                vec![zooming(&m.testsrc, from, to, (0.0, 0.0))],
            ],
            None,
        );
        run_placed(&format!("zoom/{name}"), &tl, &assets, 30.0, 1.0);
    }
    // The position moves with it, and the easing of the segment is the polyline both read.
    let tl = timeline(
        vec![
            vec![clip(&m.bars, 0.5, 1.5, 0.0)],
            vec![zooming(&m.testsrc, 1.0, 0.7, (0.2, -0.1))],
        ],
        None,
    );
    run_placed("zoom/with position", &tl, &assets, 30.0, 1.0);
    let mut eased = zooming(&m.testsrc, 1.0, 1.7, (0.0, 0.0));
    eased.keyframes[0].easing = kerf_core::Easing::EaseInOut;
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![eased]], None);
    run_placed("zoom/eased", &tl, &assets, 30.0, 1.0);
    // Three keys: in, then out again.
    let mut back = zooming(&m.testsrc, 1.0, 1.5, (0.0, 0.0));
    back.keyframes = vec![
        zoom_key(0.0, 1.0, 0.0, 0.0),
        zoom_key(0.4, 1.5, 0.1, 0.0),
        zoom_key(1.0, 0.8, -0.1, 0.05),
    ];
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![back]], None);
    run_placed("zoom/three keys", &tl, &assets, 30.0, 1.0);
    // A zoom that starts late on the timeline, speed 2, reversed.
    let mut late = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
    late.timeline_start = 0.5;
    let tl = timeline(vec![vec![late]], None);
    refused(
        "zoom/late start",
        &tl,
        &assets[1..],
        &at_fps(30.0),
        15..44,
        "a moving zoom or position",
    );
    for (name, speed) in [("speed 2", 2.0), ("reversed", -1.0)] {
        let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.0, 0.0));
        c.source_out = 2.5;
        c.speed = speed;
        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 1.5, 0.0, 0.0)];
        let tl = timeline(vec![vec![c]], None);
        refused(
            &format!("zoom/{name}"),
            &tl,
            &assets[1..],
            &at_fps(30.0),
            0..14,
            "a moving zoom or position",
        );
    }
    // Cover into a frame the footage is scaled to: two scalers in cascade, the second one zooming.
    let tl = timeline(
        vec![vec![zooming(&m.testsrc, 1.0, 1.25, (-0.05, 0.0))]],
        Some(Delivery::new(960, 540, Fit::Cover)),
    );
    run_placed("zoom/cover 960x540", &tl, &assets[1..], 30.0, 1.0);
    let tl = timeline(
        vec![vec![zooming(&m.testsrc, 1.0, 1.3, (0.0, 0.0))]],
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    run_placed("zoom/cover 9:16", &tl, &assets[1..], 30.0, 1.0);
    // A still image is RGB: FFmpeg scales it in RGB, and any resize of such a picture is refused (the zoom
    // is one), whatever the frame does after it.
    let tl = timeline(vec![vec![zooming(&m.image, 1.0, 1.5, (0.0, 0.0))]], None);
    refused(
        "zoom/still image (rgb)",
        &tl,
        std::slice::from_ref(&m.image),
        &at_fps(30.0),
        upto(1.0, 30.0),
        "rgb24",
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_zoom_behind_a_grade_mask_fade_or_effect_is_refused() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let with = |edit: &dyn Fn(&mut Clip)| {
        let mut z = zooming(&m.testsrc, 1.0, 1.6, (0.0, 0.0));
        edit(&mut z);
        timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![z]], None)
    };
    // The grade runs on the fit-size picture (its tables are applied before the scaler's second stage).
    let tl = with(&|c| {
        c.color = Color {
            contrast: 1.2,
            saturation: 1.3,
            brightness: 0.05,
            ..Color::default()
        }
    });
    run_placed("zoom+grade", &tl, &assets, 30.0, 1.0);
    // The clip's own fades: faded at the fit size, then zoomed.
    let tl = with(&|c| {
        c.fade_in = 0.4;
        c.fade_out = 0.4;
    });
    run_placed("zoom+fades", &tl, &assets, 30.0, 1.0);
    // A mask: the alpha plane is cut at the fit size and scaled with the zoom, so its edge grows soft.
    for (name, shape, feather, inverted) in [
        ("rect hard", kerf_core::MaskShape::Rect, 0.0, false),
        ("rect feathered", kerf_core::MaskShape::Rect, 0.15, false),
        ("ellipse feathered", kerf_core::MaskShape::Ellipse, 0.2, false),
        ("ellipse inverted", kerf_core::MaskShape::Ellipse, 0.1, true),
    ] {
        let tl = with(&|c| c.mask = Some(mask_of(shape, feather, inverted)));
        run_placed(&format!("zoom+mask {name}"), &tl, &assets, 30.0, 1.0);
    }
    // The effects: the tables are exact (the scaler's level is all there is); a blur carries the scaler's
    // level through its own. The fit scale ahead of the effects is the identity here, so a sharpen of any
    // amount is of the exact picture and only the zoom's scale follows it.
    for (name, effect) in [
        ("blur", kerf_core::VideoEffect::Blur { sigma: 4.0 }),
        ("sharpen", kerf_core::VideoEffect::Sharpen { amount: 1.5 }),
        ("grayscale", kerf_core::VideoEffect::Grayscale),
        ("invert", kerf_core::VideoEffect::Invert),
        ("chroma key", key("green", 0.2, 0.2)),
        ("vignette", kerf_core::VideoEffect::Vignette),
    ] {
        let tl = with(&|c| c.effects = vec![effect.clone()]);
        run_placed(&format!("zoom+{name}"), &tl, &assets, 30.0, 1.0);
    }
    // The tutorial's clip: a feathered mask over a blurred picture, zooming, in a Cover frame.
    let mut c = zooming(&m.testsrc, 1.0, 1.25, (-0.05, 0.0));
    c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.1, false));
    c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 6.0 }];
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]],
        Some(Delivery::new(960, 540, Fit::Cover)),
    );
    run_placed("zoom+mask+blur cover 960x540", &tl, &assets, 30.0, 1.0);
    // Everything at once: grade, an effect, a mask, a fade-in.
    let tl = with(&|c| {
        c.color.contrast = 1.15;
        c.effects = vec![kerf_core::VideoEffect::Invert, kerf_core::VideoEffect::Blur { sigma: 2.0 }];
        c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.25, false));
        c.fade_in = 0.3;
    });
    run_placed("zoom+grade+effects+mask+fade", &tl, &assets, 30.0, 1.0);
}

/// Blur's bound: `gblur` is floating point, and the x86 build runs its horizontal pass in AVX2 /
/// AVX-512 with another association of the same sums (and a fused multiply-add), so FFmpeg's own
/// answer depends on the CPU by the last bits of a `float` — a level on a sample that sits on a half.
/// The compositor follows the portable C order: measured, 0 to 59 samples in 3.2 million are one level
/// off, none more. Everything else an effect does (the tables, the sharpen) is exact, 0 differing.
const EFFECT: Limits = Limits { y: 1, uv: 1 };

/// Two levels: a blur, grayscale or invert on a picture the compositor scaled first (or took through the
/// translucent round trip), the scaler's level (`SCALED`) and the blur's own (`EFFECT`) on a sample where both
/// fall; and a blur followed by a sharpen, or a scale by one, within a worst-case error of two. `unsharp` is
/// exact but amplifies the one-level difference upstream of it by `1 + 2 * amount` at an edge sample (measured
/// after a scale: 3 at 1.5, 7 at 3, 11 at 5; after a blur on 6.1.1: 2 at 1, 3 at 1.5, 5-6 at 5), and its `>> 16`
/// rounds up a level more with each further sharpen. So the plan refuses what passes two (one sharpen above 0.5,
/// or a second one) after a scale or a blur, and nothing past it is held to a bound: see `render_plan.rs`,
/// `a_sharpen_that_amplifies_a_scale_or_a_blur_past_two_levels_is_refused`.
const EFFECT_SCALED: Limits = Limits { y: 2, uv: 2 };

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_dissolve_dip_or_slide_into_a_moving_zoom_is_refused() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    for (name, mask) in [
        ("", None),
        (" with a mask", Some(mask_of(kerf_core::MaskShape::Rect, 0.15, false))),
    ] {
        let out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
        inc.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.4, 0.0, 0.0)];
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        inc.mask = mask;
        let tl = timeline(vec![vec![out, inc]], None);
        run_placed(&format!("zoom/dissolve{name}"), &tl, &assets, 30.0, 2.0);
    }
    // A slide in with a zoom, and a dip to black.
    for (name, kind) in [("dip", TransitionKind::DipToBlack), ("slide", TransitionKind::SlideLeft)] {
        let out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
        inc.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.4, 0.0, 0.0)];
        inc.transition_in = Some(Transition { kind, duration: 0.5 });
        let tl = timeline(vec![vec![out, inc]], None);
        run_placed(&format!("zoom/{name}"), &tl, &assets, 30.0, 2.0);
    }
}

/// The outgoing clip zooms while the incoming one comes over it: the tail of a zooming clip (its keys run
/// on past its end under the transition), a dissolve's ramp on the other layer, a slide's travel over a
/// layer that is itself being scaled, and a mask on the zoomer.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_zoom_on_the_outgoing_clip_under_a_transition() {
    let m = media();
    let assets = [m.testsrc.clone(), m.bars.clone()];
    for (name, kind, mask) in [
        ("dissolve", TransitionKind::Crossfade, None),
        ("dip", TransitionKind::DipToBlack, None),
        ("slide", TransitionKind::SlideLeft, None),
        ("push", TransitionKind::PushRight, None),
        (
            "dissolve, masked",
            TransitionKind::Crossfade,
            Some(mask_of(kerf_core::MaskShape::Rect, 0.15, false)),
        ),
    ] {
        let mut out = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
        out.mask = mask;
        let mut inc = clip(&m.bars, 0.5, 1.5, 1.0);
        inc.transition_in = Some(Transition { kind, duration: 0.5 });
        let tl = timeline(vec![vec![out, inc]], None);
        run_placed(&format!("zoom out/{name}"), &tl, &assets, 30.0, 2.0);
    }
}

/// A rotation, an opacity below 1 or a dip to white in front of a moving zoom is the order the compositor
/// does not have: the rotated picture's alpha (or the RGB round trip) would have to be scaled with the zoom,
/// and the zoom's `scale` takes the RGB frame `fade c=white` leaves and converts it in the same pass (up to
/// 14 levels off a draw that scales the converted picture on 6.1.1, 3 on 9.0.2). (Every export frame of a
/// moving zoom is refused for its placement as well.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_zoom_behind_a_rotation_or_an_opacity_is_refused() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    // (A keyed clip takes every number from its keys: the turn and the opacity are keyed here too.)
    let mut turning = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
    turning.keyframes.iter_mut().for_each(|k| k.rotation = 15.0);
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![turning]], None);
    refused("zoom+rotation", &tl, &assets, &at_fps(30.0), upto(1.0, 30.0), "a rotation");
    let mut faint = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
    faint.keyframes.iter_mut().for_each(|k| k.opacity = 0.6);
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![faint]], None);
    refused(
        "zoom+opacity",
        &tl,
        &assets,
        &at_fps(30.0),
        upto(1.0, 30.0),
        "an opacity below 1",
    );
    // A keyed opacity that holds 1 and then falls (the keys take every number): while it holds, its `geq` is
    // the identity on the alpha plane, copying Y, U and V (one in at the edge on 6.1) ahead of the zoom, and the
    // frame is drawn; once it falls the alpha plane would be scaled with the zoom, and the frame is refused.
    let mut fading = zooming(&m.testsrc, 1.0, 1.6, (0.0, 0.0));
    fading.keyframes = vec![
        zoom_key(0.0, 1.0, 0.0, 0.0),
        zoom_key(0.5, 1.3, 0.0, 0.0),
        kerf_core::Keyframe {
            opacity: 0.3,
            ..zoom_key(1.0, 1.6, 0.0, 0.0)
        },
    ];
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![fading]], None);
    run_placed(
        "keyed opacity + moving zoom (curve holds 1, then falls)",
        &tl,
        &assets,
        30.0,
        1.0,
    );
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
    inc.timeline_start = 1.0;
    inc.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    refused(
        "zoom+dip to white",
        &tl,
        &assets,
        &at_fps(30.0),
        upto(2.0, 30.0),
        "a dip to white",
    );
}

// ---- masks and effects, with nothing zooming ----------------------------------------------------

/// A clip on a layer of bars: `edit` decides what it carries.
fn key(color: &str, similarity: f64, blend: f64) -> kerf_core::VideoEffect {
    kerf_core::VideoEffect::ChromaKey {
        color: color.into(),
        similarity,
        blend,
    }
}

fn over_bars(m: &Media, edit: &dyn Fn(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
    edit(&mut c);
    (
        timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
        vec![m.bars.clone(), m.testsrc.clone()],
    )
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_mask_cuts_the_alpha_as_geq_does() {
    let m = media();
    // A picture drawn at its own size is the picture plane for plane, so what differs is the alpha
    // alone: the mask's own samples, blended per pixel in luma and averaged over each 2x2 block
    // for chroma, as `overlay` does.
    for shape in [kerf_core::MaskShape::Rect, kerf_core::MaskShape::Ellipse] {
        for (feather, inverted) in [
            (0.0, false),
            (0.15, false),
            (0.5, false),
            (1.0, false),
            (0.1, true),
            (0.0, true),
        ] {
            let (tl, assets) = over_bars(m, &|c| c.mask = Some(mask_of(shape, feather, inverted)));
            run(
                &format!("mask/{shape:?} feather {feather}{}", if inverted { " inverted" } else { "" }),
                &tl,
                &assets,
                30.0,
                1.0,
                EXACT,
            );
        }
    }
    // Off-centre, larger than the frame (the edge leaves it), and a thin one.
    for (name, mask) in [
        (
            "off-centre",
            kerf_core::Mask {
                shape: kerf_core::MaskShape::Ellipse,
                x: 0.3,
                y: 0.62,
                width: 0.45,
                height: 0.8,
                feather: 0.25,
                inverted: false,
            },
        ),
        (
            "bigger than the frame",
            kerf_core::Mask {
                shape: kerf_core::MaskShape::Rect,
                x: 0.5,
                y: 0.5,
                width: 2.0,
                height: 1.3,
                feather: 0.1,
                inverted: false,
            },
        ),
        (
            "thin",
            kerf_core::Mask {
                shape: kerf_core::MaskShape::Rect,
                x: 0.5,
                y: 0.5,
                width: 0.02,
                height: 0.9,
                feather: 0.0,
                inverted: false,
            },
        ),
        // The edge of the shape is the edge of the picture: the alpha ramps along the last column
        // and row, whose chroma `overlay` averages over fewer neighbours than the rest.
        (
            "ramp along the border",
            kerf_core::Mask {
                shape: kerf_core::MaskShape::Rect,
                x: 0.5,
                y: 0.5,
                width: 1.0,
                height: 1.0,
                feather: 0.08,
                inverted: false,
            },
        ),
        (
            "ellipse touching the border",
            kerf_core::Mask {
                shape: kerf_core::MaskShape::Ellipse,
                x: 0.5,
                y: 0.5,
                width: 1.0,
                height: 1.0,
                feather: 0.3,
                inverted: true,
            },
        ),
    ] {
        let (tl, assets) = over_bars(m, &|c| c.mask = Some(mask));
        run(&format!("mask/{name}"), &tl, &assets, 30.0, 1.0, EXACT);
    }
    // Over noise: every chroma block has its own alpha average to get right.
    let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
    c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, false));
    let tl = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0)], vec![c]], None);
    run("mask/over noise", &tl, &[m.black.clone(), m.noise.clone()], 30.0, 1.0, EXACT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_mask_on_a_smaller_odd_sized_or_moved_layer() {
    let m = media();
    // A scaled, moved picture-in-picture (the compositor scales it: one level), an odd-sized one
    // (211x118: the last chroma column and row have fewer neighbours to average) and one that
    // leaves the frame.
    for (name, scale, pos) in [
        ("pip", 0.5, (0.2, -0.15)),
        ("odd 211x118", 0.33, (0.1, 0.1)),
        ("off the frame", 0.8, (0.45, 0.4)),
    ] {
        let (tl, assets) = over_bars(m, &|c| {
            c.transform.scale = scale;
            c.transform.pos_x = pos.0;
            c.transform.pos_y = pos.1;
            c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false));
        });
        run(&format!("mask/{name}"), &tl, &assets, 30.0, 1.0, SCALED);
    }
    // The same edge-of-the-picture ramp on an odd-sized layer: its last chroma column and row hold one luma
    // column and row fewer than a whole block.
    for (name, scale) in [
        ("odd 211x118, ramp to the border", 0.33),
        ("even 320x180, ramp to the border", 0.5),
    ] {
        let (tl, assets) = over_bars(m, &|c| {
            c.transform.scale = scale;
            c.mask = Some(kerf_core::Mask {
                shape: kerf_core::MaskShape::Rect,
                x: 0.5,
                y: 0.5,
                width: 1.0,
                height: 1.0,
                feather: 0.25,
                inverted: false,
            });
        });
        run(&format!("mask/{name}"), &tl, &assets, 30.0, 1.0, SCALED);
    }
    // A letterboxed picture: `pad` hands `geq` the whole padded frame, bars included, so the mask is the
    // canvas's size and cuts the bars too (a feathered ellipse on a 16:9 picture in a 9:16 frame).
    let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
    c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false));
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]],
        Some(Delivery::new(360, 640, Fit::Contain)),
    );
    run(
        "mask/letterboxed 9:16",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        1.0,
        SCALED,
    );
    // A cropped clip in a frame of another shape.
    let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
    c.transform.crop_left = 0.2;
    c.transform.crop_right = 0.2;
    c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.12, false));
    let tl = timeline(vec![vec![c]], Some(Delivery::new(360, 640, Fit::Cover)));
    run(
        "mask/9:16 cover with a crop",
        &tl,
        std::slice::from_ref(&m.testsrc),
        30.0,
        1.0,
        SCALED,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_mask_fades_and_dissolves_on_its_own_alpha() {
    let m = media();
    // The clip's own fades (an alpha plane: black is 0), and a dissolve in: the ramp acts on the
    // mask's alpha sample by sample.
    let (tl, assets) = over_bars(m, &|c| {
        c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false));
        c.fade_in = 0.4;
        c.fade_out = 0.4;
    });
    run("mask/fades", &tl, &assets, 30.0, 1.0, EXACT);
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.2, false));
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run("mask/dissolve", &tl, &[m.bars.clone(), m.testsrc.clone()], 30.0, 2.0, EXACT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn effects_are_drawn_as_their_filters_draw_them() {
    let m = media();
    use kerf_core::VideoEffect as E;
    // Grayscale and invert are tables: exact. A sharpen is integer arithmetic: exact. A blur is the
    // portable order of a float recursion that FFmpeg runs in SIMD (see `EFFECT`).
    for (name, effects, limits) in [
        ("grayscale", vec![E::Grayscale], EXACT),
        ("invert", vec![E::Invert], EXACT),
        ("gray then invert", vec![E::Grayscale, E::Invert], EXACT),
        ("sharpen 1", vec![E::Sharpen { amount: 1.0 }], EXACT),
        ("sharpen 2.5", vec![E::Sharpen { amount: 2.5 }], EXACT),
        ("sharpen -1 (a blur)", vec![E::Sharpen { amount: -1.0 }], EXACT),
        ("sharpen 0.7", vec![E::Sharpen { amount: 0.7 }], EXACT),
        ("blur 0.5", vec![E::Blur { sigma: 0.5 }], EFFECT),
        ("blur 2", vec![E::Blur { sigma: 2.0 }], EFFECT),
        ("blur 6", vec![E::Blur { sigma: 6.0 }], EFFECT),
        ("blur 20", vec![E::Blur { sigma: 20.0 }], EFFECT),
        ("blur 6.3", vec![E::Blur { sigma: 6.3 }], EFFECT),
        ("blur then invert", vec![E::Blur { sigma: 3.0 }, E::Invert], EFFECT),
        ("invert then blur", vec![E::Invert, E::Blur { sigma: 3.0 }], EFFECT),
        // A sharpen after a blur amplifies the blur's own level (FFmpeg's SIMD order against ours) by its gain
        // (`1 + 2 * amount`): at half strength that is two levels (`EFFECT_SCALED`), past it the plan refuses
        // (below). Sharpens with nothing inexact ahead of them are exact, chained too.
        (
            "blur then sharpen 0.5",
            vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 0.5 }],
            EFFECT_SCALED,
        ),
        (
            "sharpen twice",
            vec![E::Sharpen { amount: 1.0 }, E::Sharpen { amount: 2.0 }],
            EXACT,
        ),
        // A blur straight after the sharpen, the slowest pass to draw in front of it: the compute pass that
        // runs the blur must read the finished sharpen (`Compositor::settle`: lavapipe 23.2 did not wait).
        (
            "sharpen 2.5 then blur",
            vec![E::Sharpen { amount: 2.5 }, E::Blur { sigma: 3.0 }],
            EFFECT,
        ),
        (
            "sharpen 0.5 then blur",
            vec![E::Sharpen { amount: 0.5 }, E::Blur { sigma: 3.0 }],
            EFFECT,
        ),
        (
            "sharpen, gray, blur, invert",
            vec![E::Sharpen { amount: 1.0 }, E::Grayscale, E::Blur { sigma: 2.0 }, E::Invert],
            EFFECT,
        ),
        // A vignette is exact: its dither runs on from the clip's first frame, which the cursor counts.
        ("vignette", vec![E::Vignette], EXACT),
        ("gray, vignette, invert", vec![E::Grayscale, E::Vignette, E::Invert], EXACT),
        ("blur then vignette", vec![E::Blur { sigma: 2.0 }, E::Vignette], EFFECT),
    ] {
        let (tl, assets) = over_bars(m, &|c| c.effects = effects.clone());
        run(&format!("effect/{name}"), &tl, &assets, 30.0, 0.5, limits);
    }
    // A graded clip: `eq` runs ahead of every effect.
    let (tl, assets) = over_bars(m, &|c| {
        c.color.contrast = 1.2;
        c.color.saturation = 1.4;
        c.effects = vec![E::Blur { sigma: 3.0 }, E::Grayscale];
    });
    run("effect/graded then blur, gray", &tl, &assets, 30.0, 0.5, EFFECT);
    // Noise in all three planes, so every sample of a plane matters to its neighbours.
    for (name, effects) in [
        ("noise blur", vec![E::Blur { sigma: 2.5 }]),
        ("noise sharpen", vec![E::Sharpen { amount: 1.5 }]),
        ("noise invert", vec![E::Invert]),
    ] {
        let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
        c.effects = effects;
        let tl = timeline(vec![vec![c]], None);
        run(
            &format!("effect/{name}"),
            &tl,
            std::slice::from_ref(&m.noise),
            30.0,
            0.4,
            EFFECT,
        );
    }
    // A blur then a sharpen of half strength over noise (every sample on a tie of the blur's rounding is a
    // chance for the blur's level to be amplified): held to two levels.
    let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
    c.effects = vec![E::Blur { sigma: 2.5 }, E::Sharpen { amount: 0.5 }];
    run(
        "effect/noise blur then sharpen 0.5",
        &timeline(vec![vec![c]], None),
        std::slice::from_ref(&m.noise),
        30.0,
        0.4,
        EFFECT_SCALED,
    );
    // After a scale: the effect sees the picture at the size it is drawn at (the zoom comes first
    // when it does not move).
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.6;
        c.transform.pos_x = 0.15;
        c.effects = vec![E::Blur { sigma: 3.0 }];
    });
    run("effect/blur on a pip", &tl, &assets, 30.0, 0.5, EFFECT_SCALED);
    // Odd sizes: the chroma planes round up and are blurred at their own size.
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.33;
        c.effects = vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 0.5 }];
    });
    run("effect/odd 211x118", &tl, &assets, 30.0, 0.5, EFFECT_SCALED);
    // A vignette's dither is the clip's own run: counted from its first output frame, whatever the clip's
    // place on the timeline, its speed or direction, and with the frames of a transition counted in it.
    let vignetted = |edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.effects = vec![E::Vignette];
        edit(&mut c);
        c
    };
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.6;
        c.transform.pos_x = 0.15;
        c.effects = vec![E::Vignette];
    });
    run("effect/vignette on a pip", &tl, &assets, 30.0, 0.5, SCALED);
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.33;
        c.effects = vec![E::Vignette];
    });
    run("effect/vignette odd 211x118", &tl, &assets, 30.0, 0.5, SCALED);
    let late = vignetted(&|c| c.timeline_start = 0.5);
    check_motion(Check {
        case: "effect/vignette late start",
        tl: &timeline(vec![vec![late]], None),
        assets: std::slice::from_ref(&m.testsrc),
        opts: at_fps(30.0),
        frames: 15..44,
        limits: EXACT,
        look: &[],
        refusing: None,
    });
    for (name, speed) in [("speed 2", 2.0), ("speed 0.5", 0.5), ("reversed", -1.0)] {
        let c = vignetted(&|c| {
            c.source_out = 2.5;
            c.speed = speed;
        });
        check_motion(Check {
            case: &format!("effect/vignette {name}"),
            tl: &timeline(vec![vec![c]], None),
            assets: std::slice::from_ref(&m.testsrc),
            opts: at_fps(30.0),
            frames: 0..20,
            limits: EXACT,
            look: &[],
            refusing: None,
        });
    }
    let out = vignetted(&|_| {});
    let inc = {
        let mut c = clip(&m.bars, 0.5, 1.5, 1.0);
        c.effects = vec![E::Vignette];
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        c
    };
    run(
        "effect/vignette dissolve",
        &timeline(vec![vec![out, inc]], None),
        &[m.testsrc.clone(), m.bars.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // A chroma key: an alpha plane at the picture's size, from the chroma (exact), blended as a mask's is.
    for (name, effects) in [
        ("chroma key green", vec![key("green", 0.15, 0.1)]),
        ("chroma key hard", vec![key("0x2050a0", 0.2, 0.0)]),
        ("chroma key after tables", vec![E::Invert, key("magenta", 0.25, 0.05)]),
    ] {
        let (tl, assets) = over_bars(m, &|c| c.effects = effects.clone());
        run(&format!("effect/{name}"), &tl, &assets, 30.0, 0.5, EXACT);
    }
    let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
    c.effects = vec![key("0x808080", 0.05, 0.3)];
    run(
        "effect/chroma key noise",
        &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
        &[m.bars.clone(), m.noise.clone()],
        30.0,
        0.4,
        EXACT,
    );
    // Through its fades and a dissolve (the ramp acts on the key's alpha).
    let (tl, assets) = over_bars(m, &|c| {
        c.effects = vec![key("green", 0.2, 0.2)];
        c.fade_in = 0.3;
        c.fade_out = 0.3;
    });
    run("effect/chroma key with fades", &tl, &assets, 30.0, 1.0, EXACT);
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.effects = vec![key("green", 0.2, 0.2)];
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    run(
        "effect/chroma key dissolve",
        &timeline(vec![vec![out, inc]], None),
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    // A sharpen after a scale, up to half strength: the scaler's level through the filter's gain (2).
    for (name, effects) in [
        ("sharpen 0.5", vec![E::Sharpen { amount: 0.5 }]),
        (
            "blur then sharpen 0.5",
            vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 0.5 }],
        ),
        (
            "sharpen 0.5 then blur",
            vec![E::Sharpen { amount: 0.5 }, E::Blur { sigma: 3.0 }],
        ),
    ] {
        let (tl, assets) = over_bars(m, &|c| {
            c.transform.scale = 0.6;
            c.transform.pos_x = 0.15;
            c.effects = effects.clone();
        });
        run(&format!("effect/{name} on a pip"), &tl, &assets, 30.0, 0.5, EFFECT_SCALED);
    }
    // ...and past a gain of 2 refused, after a scale or a blur: FFmpeg renders them.
    for (name, effects) in [
        (
            "sharpen 0.5 twice",
            vec![E::Sharpen { amount: 0.5 }, E::Sharpen { amount: 0.5 }],
        ),
        // Each further sharpen rounds up a level of its own, however small its amount.
        (
            "sharpen 0.2 twice",
            vec![E::Sharpen { amount: 0.2 }, E::Sharpen { amount: 0.2 }],
        ),
        (
            "sharpen 0.001 twice",
            vec![E::Sharpen { amount: 0.001 }, E::Sharpen { amount: 0.001 }],
        ),
        ("sharpen 1.5", vec![E::Sharpen { amount: 1.5 }]),
        ("sharpen 3", vec![E::Sharpen { amount: 3.0 }]),
        ("sharpen 5", vec![E::Sharpen { amount: 5.0 }]),
        ("sharpen -1", vec![E::Sharpen { amount: -1.0 }]),
        (
            "blur then sharpen 1.5",
            vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 1.5 }],
        ),
        (
            "sharpen 1.5 then blur",
            vec![E::Sharpen { amount: 1.5 }, E::Blur { sigma: 3.0 }],
        ),
    ] {
        let (tl, assets) = over_bars(m, &|c| {
            c.transform.scale = 0.6;
            c.transform.pos_x = 0.15;
            c.effects = effects.clone();
        });
        refused(
            &format!("effect/{name} on a pip"),
            &tl,
            &assets,
            &at_fps(30.0),
            upto(1.0, 30.0),
            "sharpen after a scale or a blur",
        );
    }
    // After a blur, unscaled: the blur's level is amplified the same way.
    for (name, effects) in [
        (
            "blur then sharpen 0.6",
            vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 0.6 }],
        ),
        (
            "blur then sharpen 1",
            vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 1.0 }],
        ),
        (
            "blur then sharpen 5",
            vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 5.0 }],
        ),
        (
            "blur then sharpen 0.2 twice",
            vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 0.2 }, E::Sharpen { amount: 0.2 }],
        ),
        (
            "blur then sharpen 0.5 twice",
            vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 0.5 }, E::Sharpen { amount: 0.5 }],
        ),
    ] {
        let (tl, assets) = over_bars(m, &|c| c.effects = effects.clone());
        refused(
            &format!("effect/{name}"),
            &tl,
            &assets,
            &at_fps(30.0),
            upto(1.0, 30.0),
            "sharpen after a scale or a blur",
        );
    }
    // A table on a letterbox: `pad`'s bars are part of the frame the effect sees, so they go through it
    // (invert makes them 239, 127, 127) and cover what is below.
    for (name, effect) in [("invert", E::Invert), ("grayscale", E::Grayscale)] {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.effects = vec![effect];
        let tl = timeline(
            vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]],
            Some(Delivery::new(360, 640, Fit::Contain)),
        );
        run(
            &format!("effect/{name} on a letterbox"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            0.5,
            SCALED,
        );
    }
    // Rotated and translucent layers: the effect comes ahead of both.
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.6;
        c.transform.opacity = 0.7;
        c.effects = vec![E::Blur { sigma: 2.0 }];
    });
    run(
        "effect/blur on a translucent layer",
        &tl,
        &assets,
        30.0,
        0.5,
        Limits { y: 2, uv: 2 },
    );
    // In and out of a fade and a dissolve.
    let (tl, assets) = over_bars(m, &|c| {
        c.effects = vec![E::Blur { sigma: 2.0 }];
        c.fade_in = 0.3;
        c.fade_out = 0.3;
    });
    run("effect/blur with fades", &tl, &assets, 30.0, 1.0, EFFECT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_the_compositor_does_not_draw_of_an_effect_or_a_mask_is_refused() {
    let m = media();
    let opts = at_fps(30.0);
    use kerf_core::VideoEffect as E;
    // A vignette on what FFmpeg vignettes in another form: a letterbox's padded frame, an RGB picture.
    let mut boxed = clip(&m.testsrc, 0.5, 1.5, 0.0);
    boxed.effects = vec![E::Vignette];
    refused(
        "effect/vignette on a letterbox",
        &timeline(vec![vec![boxed]], Some(Delivery::new(360, 640, Fit::Contain))),
        std::slice::from_ref(&m.testsrc),
        &opts,
        upto(1.0, 30.0),
        "letterboxed",
    );
    let mut still = clip(&m.image, 0.0, 1.0, 0.0);
    still.effects = vec![E::Vignette];
    refused(
        "effect/vignette on an rgb picture",
        &timeline(vec![vec![still]], None),
        std::slice::from_ref(&m.image),
        &opts,
        upto(1.0, 30.0),
        "yuv420p",
    );
    // A chroma key on chroma that is not FFmpeg's to the level: after a scale (a level moves its alpha by up to
    // 1/blend levels).
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.6;
        c.effects = vec![key("green", 0.15, 0.1)];
    });
    refused(
        "effect/chroma key on a pip",
        &tl,
        &assets,
        &opts,
        upto(1.0, 30.0),
        "chroma key after a scale or a blur",
    );
    let (tl, assets) = over_bars(m, &|c| c.effects = vec![E::Blur { sigma: 2000.0 }]);
    refused(
        "effect/blur sigma 2000",
        &tl,
        &assets,
        &opts,
        upto(1.0, 30.0),
        "gblur refuses",
    );
    let (tl, assets) = over_bars(m, &|c| c.effects = vec![E::Sharpen { amount: 9.0 }]);
    refused("effect/sharpen 9", &tl, &assets, &opts, upto(1.0, 30.0), "unsharp refuses");
    // A mask on a turned layer (the alpha plane turns with the picture, soft edge and all) and on one
    // below full opacity (its alpha goes through `colorchannelmixer`).
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.rotation = 12.0;
        c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.1, false));
    });
    refused("mask/rotated", &tl, &assets, &opts, upto(1.0, 30.0), "rotated layer");
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.opacity = 0.6;
        c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.1, false));
    });
    refused("mask/translucent", &tl, &assets, &opts, upto(1.0, 30.0), "below full opacity");
    let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
    c.effects = vec![E::Blur { sigma: 3.0 }];
    let tl = timeline(vec![vec![c]], Some(Delivery::new(360, 640, Fit::Contain)));
    refused(
        "effect/blur letterboxed",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &opts,
        upto(1.0, 30.0),
        "letterboxed",
    );
}

// ---- the BT.709 label ----------------------------------------------------------------------

/// A 1280x720 gradient, untagged (HD that every player shows as BT.709) and the same picture tagged
/// BT.709, two seconds each.
struct HdPair {
    untagged: Asset,
    phone: Asset,
}

fn hd_pair() -> &'static HdPair {
    static M: OnceLock<HdPair> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-hd");
        std::fs::create_dir_all(&dir).unwrap();
        let encode = |name: &str, label: &str| -> Asset {
            let p = dir.join(name);
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "gradients=size=1280x720:rate=30:duration=2:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:speed=0.3,\
                     format=gbrp,scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,{label}"
                ),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            probed(&p)
        };
        HdPair {
            untagged: encode("hd-untagged.mp4", "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown"),
            phone: encode("hd-709.mp4", "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709"),
        }
    })
}

/// The export labels untagged HD for **every frame of its graph**, not for the frames a matrix-naming
/// clip is on screen in: a translucent untagged HD clip that is alone in its first second, and a
/// BT.709 clip alone in its second, are drawn as the export draws them, and a cut without the
/// BT.709 clip draws the same HD frames as BT.601 (the compositor takes the layer out of YUV with the
/// matrix the plan says, which differs: the same frames are not the same picture).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn untagged_hd_is_labelled_for_the_whole_graph_that_holds_a_matrix() {
    let h = hd_pair();
    let assets = [h.untagged.clone(), h.phone.clone()];
    let canvas = Some(Delivery::new(640, 360, Fit::Contain));
    let glass = |h: &HdPair| {
        let mut c = clip(&h.untagged, 0.0, 1.0, 0.0);
        c.transform.opacity = 0.5;
        c
    };
    let tl = timeline(vec![vec![glass(h)], vec![clip(&h.phone, 0.0, 1.0, 1.0)]], canvas);
    let planner = kerf_core::Planner::new(&tl, &assets, &at_fps(30.0), kerf_core::PlanRequest::motion(policy())).unwrap();
    let plan = planner.at_frame(10).unwrap();
    assert_eq!(plan.layers.len(), 1);
    assert!(
        plan.layers[0].stream.label_bt709,
        "the graph holds a BT.709 clip: the HD clip is labelled"
    );
    run(
        "label/translucent-untagged-hd-before-a-709-clip",
        &tl,
        &assets,
        30.0,
        2.0,
        HD_TRANSLUCENT,
    );
    // Over the BT.709 clip's own frames too: the translucent HD clip on a second track.
    let mut over = glass(h);
    over.timeline_start = 0.0;
    over.source_out = 2.0;
    let tl = timeline(
        vec![
            vec![clip(&h.phone, 0.0, 2.0, 0.0)],
            vec![{
                let mut c = over;
                c.transform.scale = 0.6;
                c
            }],
        ],
        canvas,
    );
    run(
        "label/translucent-untagged-hd-over-a-709-clip",
        &tl,
        &assets,
        30.0,
        2.0,
        HD_TRANSLUCENT,
    );
    // The control: the same HD frames with no BT.709 clip in the graph are not labelled, and drawn.
    let tl = timeline(vec![vec![glass(h)]], canvas);
    let plan = kerf_core::Planner::new(&tl, &assets, &at_fps(30.0), kerf_core::PlanRequest::motion(policy()))
        .unwrap()
        .at_frame(10)
        .unwrap();
    assert!(!plan.layers[0].stream.label_bt709);
    run("label/translucent-untagged-hd-alone", &tl, &assets, 30.0, 1.0, HD_TRANSLUCENT);
}

/// A dip to white is `yuv420p -> rgb24 -> yuv420p` around the fade, out of YUV with the clip's own matrix, so
/// the label is read there too: an untagged HD clip that dips through white in a cut that holds a BT.709 clip
/// is taken out of YUV as BT.709, exactly as the file tagged BT.709 is. The label is the first filter of its
/// chain for this: behind the fit `scale` (the filter FFmpeg has convert the picture for the fade) it was
/// BT.601 in and BT.709 out, Y 17 / UV 9 off on 9.0.2 and Y 30 / UV 16 on 6.1.1. The HD clips are drawn at
/// their own size (a 1280x720 canvas: a layer the scaler resizes is refused its dip), and the bound is
/// BT.709's (`WHITE_709`).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_labelled_hd_clip_dips_through_white_with_the_bt709_round_trip() {
    let h = hd_pair();
    let assets = [h.untagged.clone(), h.phone.clone()];
    let first = clip(&h.phone, 0.0, 1.0, 0.0);
    let mut dipping = clip(&h.untagged, 0.0, 1.0, 1.0);
    dipping.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![first, dipping]], Some(Delivery::new(1280, 720, Fit::Contain)));
    let plan = kerf_core::Planner::new(&tl, &assets, &at_fps(30.0), kerf_core::PlanRequest::motion(policy()))
        .unwrap()
        .at_frame(36)
        .unwrap();
    assert!(
        plan.layers[0].stream.label_bt709,
        "the graph holds a BT.709 clip: the dipping HD clip is labelled"
    );
    run("label/dip-white-labelled-hd", &tl, &assets, 30.0, 2.0, WHITE_709);
}

// ---- a yuv420p flagged full range ---------------------------------------------------------------

/// The same pictures as `tests/parity.rs`' (`range_media`), long enough for a clip cut from the middle: lossless
/// FFV1, which keeps the format `yuv420p` whatever the flag (H.264 would call a full-range stream `yuvj420p`).
struct RangeMedia {
    /// The test pattern flagged `-color_range pc`, the same flagged `tv` and not flagged, and a gradient flagged `pc`.
    pc: Asset,
    tv: Asset,
    unflagged: Asset,
    pc_gradient: Asset,
}

fn range_media() -> &'static RangeMedia {
    static R: OnceLock<RangeMedia> = OnceLock::new();
    R.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-range");
        std::fs::create_dir_all(&dir).unwrap();
        let testsrc = "testsrc2=size=640x360:rate=30:duration=4";
        let gradients = "gradients=size=640x360:rate=30:duration=4:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=640:y1=360:speed=0.00001,format=yuv420p";
        let make = |name: &str, lavfi: &str, flag: Option<&str>| -> Asset {
            let p = dir.join(name);
            let mut args = vec!["-f", "lavfi", "-i", lavfi, "-c:v", "ffv1", "-pix_fmt", "yuv420p"];
            if let Some(flag) = flag {
                args.extend(["-color_range", flag]);
            }
            args.push(p.to_str().unwrap());
            ffmpeg(&args);
            let a = probed(&p);
            assert_eq!(a.streams[0].pix_fmt.as_deref(), Some("yuv420p"), "{name}");
            // (An unflagged FFV1 file reads `tv` from FFmpeg 9's muxer and nothing from 6.1.1's: neither is full range.)
            match flag {
                Some(flag) => assert_eq!(a.streams[0].color_range.as_deref(), Some(flag), "{name}"),
                None => assert_ne!(a.streams[0].color_range.as_deref(), Some("pc"), "{name}"),
            }
            a
        };
        RangeMedia {
            pc: make("pc.mkv", testsrc, Some("pc")),
            tv: make("tv.mkv", testsrc, Some("tv")),
            unflagged: make("unflagged.mkv", testsrc, None),
            pc_gradient: make("pc-gradient.mkv", gradients, Some("pc")),
        }
    })
}

/// A `yuv420p` flagged full range (a VP9, HEVC or AV1 file) is, in an export, what a `yuvj420p` is under FFmpeg 9 and
/// what it is not under FFmpeg 6: FFmpeg 9 flags the whole export full range and keeps the clip's raw
/// samples (every frame of the cut 16 levels off, a black one included), so the cut is refused as for a `yuvj` clip;
/// FFmpeg 6 hands its raw samples to the filters too (no conversion ahead of them: a format that is already the
/// overlay's), so a plain frame, a cut and a layer over it are exact there while a fade (14 levels), a vignette (10),
/// an invert (4), `eq` (6), a sharpen (20), a blur (1) and a resize (UV 8) are not, and those are refused under both.
/// The same pictures flagged `tv`, or not at all, are drawn as ever.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_yuv420p_flagged_full_range_is_drawn_or_refused_as_the_export_reads_its_raw_samples() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let r = range_media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let one = |a: &Asset, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(a, 0.5, 1.5, 0.0);
        edit(&mut c);
        timeline(vec![vec![c]], None)
    };
    let over_bars = |a: &Asset, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(a, 0.5, 1.5, 0.0);
        edit(&mut c);
        (
            timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
            vec![m.bars.clone(), a.clone()],
        )
    };
    let pc = std::slice::from_ref(&r.pc);

    // Plain: exact under FFmpeg 6; under FFmpeg 9 the cut is the export's flag. A cut that holds the clip too, either side of it.
    run_full_range("range/pc-plain", &one(&r.pc, &|_| {}), pc, 30.0, 1.0, EXACT);
    let tl = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0), clip(&r.pc, 0.5, 1.5, 1.0)]], None);
    run_full_range("range/black-then-pc", &tl, &[m.black.clone(), r.pc.clone()], 30.0, 2.0, EXACT);
    let tl = timeline(vec![vec![clip(&r.pc, 0.5, 1.5, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0)]], None);
    run_full_range(
        "range/pc-then-limited",
        &tl,
        &[r.pc.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );

    // A fade: refused for the frames it runs in under FFmpeg 6 (the rest of the clip, plain, is exact there), and the
    // cut under FFmpeg 9.
    let fade_refused = match policy() {
        CompositeColorPolicy::FixedBt601 => refusing("FullRangeNegotiated", |u| matches!(u, Unsupported::FullRangeNegotiated(_))),
        _ => full_range_cut_refused(),
    };
    let tl = one(&r.pc, &|c| {
        c.fade_in = 0.5;
        c.fade_out = 0.5;
    });
    run_refusing("range/pc-fade", &tl, pc, 30.0, 1.0, EXACT, &fade_refused);
    // The scaler resizes it (an enlargement, or a shrink over the bars): swscale converts the range inside the resize,
    // under either FFmpeg.
    let (tl, assets) = over_bars(&r.pc, &|c| c.transform.scale = 0.5);
    run_refusing(
        "range/pc-shrunk-over-bars",
        &tl,
        &assets,
        30.0,
        1.0,
        SCALED,
        &full_range_resized_refused(),
    );
    let enlarged = timeline(
        vec![vec![clip(&r.pc, 0.5, 1.5, 0.0)]],
        Some(Delivery::new(960, 540, Fit::Contain)),
    );
    run_refusing(
        "range/pc-enlarged",
        &enlarged,
        pc,
        30.0,
        1.0,
        EXACT,
        &full_range_resized_refused(),
    );

    // What reads the values: refused on every frame, under both.
    let at = at_fps(30.0);
    for (name, effects, why) in [
        ("vignette", vec![E::Vignette], "vignette"),
        ("invert", vec![E::Invert], "full-range"),
        ("blur", vec![E::Blur { sigma: 3.0 }], "full-range"),
        ("sharpen", vec![E::Sharpen { amount: 1.0 }], "full-range"),
    ] {
        let tl = one(&r.pc, &|c| c.effects = effects.clone());
        refused(&format!("range/pc-{name}"), &tl, pc, &at, 0..30, why);
    }
    refused(
        "range/pc-graded",
        &one(&r.pc, &|c| c.color.gamma = 1.4),
        pc,
        &at,
        0..30,
        "colour correction",
    );
    let (tl, assets) = over_bars(&r.pc_gradient, &|c| c.effects = vec![key("green", 0.3, 0.3)]);
    refused("range/pc-chroma-key", &tl, &assets, &at, 0..30, "chroma key");

    // Flagged limited, or not flagged: the same graphs are exact, as ever.
    for (label, a) in [("tv", &r.tv), ("unflagged", &r.unflagged)] {
        let one_a = std::slice::from_ref(a);
        for (name, effects) in [("vignette", vec![E::Vignette]), ("invert", vec![E::Invert])] {
            let tl = one(a, &|c| c.effects = effects.clone());
            run(&format!("range/{label}-{name}"), &tl, one_a, 30.0, 1.0, EXACT);
        }
        let tl = one(a, &|c| {
            c.fade_in = 0.5;
            c.fade_out = 0.5;
        });
        run(&format!("range/{label}-fade"), &tl, one_a, 30.0, 1.0, EXACT);
        let tl = one(a, &|c| c.color.gamma = 1.4);
        run(&format!("range/{label}-graded"), &tl, one_a, 30.0, 1.0, EXACT);
    }
}

// ---- the grade: LUT, curves and wheels ----------------------------------------------------------------

/// A `.cube` of `size`^3 rows, written to the target directory; `f` maps a lattice point in `0..=1` to its row.
fn grade_cube(name: &str, size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-grade-luts");
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

fn curve(p: &[(f64, f64)]) -> Vec<kerf_core::CurvePoint> {
    p.iter().map(|&(x, y)| kerf_core::CurvePoint { x, y }).collect()
}

fn puck(x: f64, y: f64, lightness: f64) -> kerf_core::Wheel {
    kerf_core::Wheel { x, y, lightness }
}

/// The grades of this section, by name: each part alone and the three together.
fn grades() -> Vec<(&'static str, kerf_core::Grade)> {
    use kerf_core::{ClipLut, Curves, Grade, Wheels};
    let look = |c: [f32; 3]| {
        [
            0.92 * c[0] + 0.08 * c[1] * c[2] + 0.03,
            0.05 * c[0] * c[0] + 0.9 * c[1] + 0.04 * c[2],
            (0.1 + 0.8 * c[2] + 0.15 * c[0] * c[1]).min(1.0),
        ]
    };
    let look17 = grade_cube("look17", 17, look);
    let look33 = grade_cube("look33", 33, look);
    let twist = grade_cube("twist2", 2, |c| [1.0 - c[0], c[1], c[2] * 0.5]);
    let wheels = Wheels {
        shadows: puck(0.4, -0.2, 0.1),
        midtones: puck(-0.3, 0.5, -0.05),
        highlights: puck(0.1, 0.7, 0.2),
    };
    let curves = Curves {
        master: curve(&[(0.0, 0.03), (0.25, 0.15), (0.75, 0.85), (1.0, 0.98)]),
        red: curve(&[(0.0, 0.0), (0.5, 0.58), (1.0, 1.0)]),
        green: Vec::new(),
        blue: curve(&[(0.0, 0.1), (1.0, 0.8)]),
    };
    vec![
        (
            "wheels midtone lift",
            Grade {
                wheels: Some(Wheels {
                    midtones: puck(0.0, 0.0, 0.1),
                    ..Wheels::default()
                }),
                ..Grade::default()
            },
        ),
        (
            "wheels all three",
            Grade {
                wheels: Some(wheels),
                ..Grade::default()
            },
        ),
        (
            "curves master S",
            Grade {
                curves: Some(Curves {
                    master: curve(&[(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)]),
                    ..Curves::default()
                }),
                ..Grade::default()
            },
        ),
        (
            "curves red invert",
            Grade {
                curves: Some(Curves {
                    red: curve(&[(0.0, 1.0), (1.0, 0.0)]),
                    ..Curves::default()
                }),
                ..Grade::default()
            },
        ),
        (
            "curves three channels and a master",
            Grade {
                curves: Some(curves.clone()),
                ..Grade::default()
            },
        ),
        (
            "lut look 17",
            Grade {
                lut: Some(ClipLut {
                    path: look17.clone(),
                    intensity: 1.0,
                }),
                ..Grade::default()
            },
        ),
        (
            "lut look 33 at 40%",
            Grade {
                lut: Some(ClipLut {
                    path: look33,
                    intensity: 0.4,
                }),
                ..Grade::default()
            },
        ),
        (
            "lut two points",
            Grade {
                lut: Some(ClipLut {
                    path: twist,
                    intensity: 1.0,
                }),
                ..Grade::default()
            },
        ),
        (
            "lut, curves and wheels",
            Grade {
                lut: Some(ClipLut {
                    path: look17,
                    intensity: 0.75,
                }),
                curves: Some(curves),
                wheels: Some(wheels),
            },
        ),
    ]
}

/// A clip of `asset` alone, graded.
fn graded_alone(asset: &Asset, grade: &kerf_core::Grade, edit: &dyn Fn(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut c = clip(asset, 0.5, 1.5, 0.0);
    c.grade = grade.clone();
    edit(&mut c);
    (timeline(vec![vec![c]], None), vec![asset.clone()])
}

/// A graded layer drawn at its own size is the export's, plane for plane: the picture goes `yuv420p -> rgb24`
/// (swscale's x86 kernel), through the LUT, the curves and the wheels, and back (the x86 vertical scaler's
/// chroma, the C kernel on the last row), and every sample of every plane is FFmpeg's, on 6.1.1 and on 9.0.2.
/// (`tests/grade.rs` holds the arithmetic to FFmpeg on noise of many sizes; this is the plan, the frames and
/// the compositor on top of it.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_grade_at_the_pictures_size_is_the_export_to_the_level() {
    let m = media();
    for (name, grade) in grades() {
        for (source, asset) in [
            ("noise", &m.noise),
            ("testsrc", &m.testsrc),
            ("bars", &m.bars),
            ("ramp", &m.ramp),
        ] {
            let (tl, assets) = graded_alone(asset, &grade, &|_| {});
            run(&format!("grade/{name}, {source}"), &tl, &assets, 30.0, 0.4, EXACT);
        }
    }
}

/// A picture that names its matrix goes out of the grade through 4:4:4 (an explicit `scale`) and into 4:2:0 by a
/// 2:1 scale of the chroma: exact, with BT.709 coefficients; and so is an untagged HD clip the graph labels.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_grade_on_a_tagged_picture_goes_through_444() {
    let v = variants();
    for (name, grade) in grades() {
        for (source, asset) in [("testsrc709", &v.testsrc709), ("bars709", &v.bars709)] {
            let (tl, assets) = graded_alone(asset, &grade, &|_| {});
            let planner = Planner::new(&tl, &assets, &at_fps(30.0), PlanRequest::motion(policy())).unwrap();
            let route = planner.at_frame(3).unwrap().layers[0].grade_route().unwrap();
            assert!(route.via_444 && route.matrix == kerf_core::YuvMatrix::Bt709);
            run(&format!("grade/{name}, {source}"), &tl, &assets, 30.0, 0.4, EXACT);
        }
    }
    // Untagged HD beside a clip that names a matrix is labelled BT.709 by the graph, and graded as the file tagged so.
    let h = hd_pair();
    let assets = [h.untagged.clone(), h.phone.clone()];
    let (_, grade) = grades().remove(8);
    let mut graded = clip(&h.untagged, 0.0, 1.0, 1.0);
    graded.grade = grade;
    let tl = timeline(
        vec![vec![clip(&h.phone, 0.0, 1.0, 0.0), graded]],
        Some(Delivery::new(1280, 720, Fit::Contain)),
    );
    let plan = Planner::new(&tl, &assets, &at_fps(30.0), PlanRequest::motion(policy()))
        .unwrap()
        .at_frame(40)
        .unwrap();
    assert!(
        plan.layers[0].stream.label_bt709,
        "the graph holds a BT.709 clip: the HD clip is labelled"
    );
    run("grade/labelled hd", &tl, &assets, 30.0, 2.0, EXACT);
}

/// What follows the grade in the chain is as it was: a colour correction ahead of it (the `eq`, static and keyed),
/// the alpha plane (an opacity, a mask, a rotation, a dissolve, a chroma key), a position, a layer under it, a
/// speed. The grade's output is exact, so the layers on top of it are held to what they are held to without a grade.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_follows_a_grade_is_drawn_as_it_is_without_one() {
    use kerf_core::{Property, PropertyKey};
    let m = media();
    let all = grades();
    let wheels_and_lut = &all[8].1;
    let curves = &all[2].1;
    let on = |asset: &Asset, grade: &kerf_core::Grade, edit: &dyn Fn(&mut Clip)| graded_alone(asset, grade, edit);

    // Ahead of the grade.
    let (tl, a) = on(&m.testsrc, wheels_and_lut, &|c| {
        c.color.contrast = 1.2;
        c.color.saturation = 1.3;
        c.color.gamma = 1.1;
    });
    run("grade/after an eq", &tl, &a, 30.0, 0.4, EXACT);
    let (tl, a) = on(&m.testsrc, curves, &|c| {
        c.set_property_keys(
            Property::Brightness,
            vec![PropertyKey::new(0.0, -0.1), PropertyKey::new(0.5, 0.1)],
        );
    });
    run("grade/after a keyed eq", &tl, &a, 30.0, 0.4, EXACT);
    let (tl, a) = on(&m.testsrc, wheels_and_lut, &|c| {
        c.speed = 2.0;
        c.source_out = 2.5;
    });
    run("grade/at speed 2", &tl, &a, 30.0, 0.4, EXACT);
    // (A crop that the canvas is cut to, so that nothing is resized.)
    let (mut tl, a) = on(&m.testsrc, wheels_and_lut, &|c| {
        c.transform.crop_left = 0.125;
        c.transform.crop_top = 0.25;
    });
    tl.format = Some(Delivery::new(560, 270, Fit::Contain));
    run("grade/a cropped picture", &tl, &a, 30.0, 0.4, EXACT);

    // The alpha plane: `format=yuva420p` follows the grade.
    let (tl, a) = on(&m.testsrc, wheels_and_lut, &|c| c.transform.opacity = 0.6);
    run("grade/translucent", &tl, &a, 30.0, 0.4, TRANSLUCENT);
    let (tl, a) = on(&m.noise, curves, &|c| {
        c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false));
    });
    run("grade/under a soft ellipse mask", &tl, &a, 30.0, 0.4, EXACT);
    let (tl, a) = on(&m.bars, wheels_and_lut, &|c| c.effects = vec![key("green", 0.3, 0.3)]);
    run("grade/under a chroma key", &tl, &a, 30.0, 0.4, EXACT);

    // On a layer: placed by the overlay (no resize), over another track, and dissolving in and out (its alpha ramp).
    let mut upper = clip(&m.noise, 0.5, 1.5, 0.0);
    upper.grade = wheels_and_lut.clone();
    upper.transform.crop_left = 0.25;
    upper.transform.crop_right = 0.25;
    upper.transform.pos_x = 0.2;
    let under = clip(&m.bars, 0.5, 1.5, 0.0);
    let tl = timeline(vec![vec![under], vec![upper]], None);
    run(
        "grade/cropped and moved over another track",
        &tl,
        &[m.bars.clone(), m.noise.clone()],
        30.0,
        0.4,
        EXACT,
    );

    let (out, mut inc) = (clip(&m.bars, 0.5, 1.5, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0));
    inc.grade = all[4].1.clone();
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run(
        "grade/dissolving in",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );
    let mut leaving = clip(&m.testsrc, 0.5, 1.5, 0.0);
    leaving.grade = all[4].1.clone();
    let mut next = clip(&m.bars, 0.5, 1.5, 1.0);
    next.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![leaving, next]], None);
    run(
        "grade/dissolving out",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        2.0,
        EXACT,
    );

    let (tl, a) = on(&m.smooth, wheels_and_lut, &|c| c.transform.rotation = 20.0);
    run_refusing("grade/rotated", &tl, &a, 30.0, 0.4, EXACT, &rotation_refused());
    let (tl, a) = on(&m.smooth, wheels_and_lut, &|c| c.transform.rotation = 90.0);
    run("grade/rotated a quarter", &tl, &a, 30.0, 0.4, EXACT);

    // A fade that is not running leaves the picture where it is: the frames after a fade-in are drawn.
    let (tl, a) = on(&m.testsrc, wheels_and_lut, &|c| c.fade_in = 0.2);
    run_refusing(
        "grade/after its fade-in",
        &tl,
        &a,
        30.0,
        0.8,
        EXACT,
        &refusing("GradeWith", |u| matches!(u, Unsupported::GradeWith(..))),
    );
}

/// What is refused for a graded layer, each held: the frames the plan refuses are FFmpeg's (its export
/// renders them), the others are compared with it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_the_compositor_does_not_draw_of_a_grade_is_refused() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let v = variants();
    let all = grades();
    let grade = &all[8].1;
    let at = at_fps(30.0);
    let frames = 0..12;
    let one = |asset: &Asset, edit: &dyn Fn(&mut Clip)| graded_alone(asset, grade, edit);

    // A layer the scaler resizes: the picture-in-picture, an enlargement, a source that is not the canvas's size.
    let (tl, a) = one(&m.testsrc, &|c| c.transform.scale = 0.6);
    refused("grade/pip", &tl, &a, &at, frames.clone(), "resizes");
    let tl = timeline(
        vec![vec![{
            let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
            c.grade = grade.clone();
            c
        }]],
        Some(Delivery::new(960, 540, Fit::Contain)),
    );
    refused(
        "grade/enlarged",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &at,
        frames.clone(),
        "resizes",
    );
    // A letterbox: the bars are part of the frame the grade sees.
    let tl = timeline(
        vec![vec![{
            let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
            c.grade = grade.clone();
            c
        }]],
        Some(Delivery::new(1280, 360, Fit::Contain)),
    );
    refused(
        "grade/letterboxed",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &at,
        frames.clone(),
        "letterboxed",
    );
    // Every other picture than 8-bit limited-range 4:2:0.
    for (name, asset) in [
        ("10-bit", &v.tenbit),
        ("4:4:4", &v.yuv444),
        ("4:2:2", &v.yuv422),
        ("gray", &v.gray),
        ("rgb", &v.rgb_png),
    ] {
        let (tl, a) = graded_alone(asset, grade, &|c| {
            if asset.streams[0].image {
                c.source_in = 0.0;
                c.source_out = 1.0;
            }
        });
        refused(&format!("grade/{name}"), &tl, &a, &at, frames.clone(), "8-bit 4:2:0");
    }
    let (tl, a) = one(&v.mjpeg, &|_| {});
    refused("grade/full range", &tl, &a, &at, frames.clone(), "colour correction");
    // What would see the RGB (or 4:4:4) picture the grade leaves.
    for (name, effect) in [
        ("blur", E::Blur { sigma: 2.0 }),
        ("sharpen", E::Sharpen { amount: 0.5 }),
        ("grayscale", E::Grayscale),
        ("invert", E::Invert),
        ("vignette", E::Vignette),
    ] {
        let (tl, a) = one(&m.testsrc, &|c| c.effects = vec![effect.clone()]);
        refused(
            &format!("grade/{name} behind it"),
            &tl,
            &a,
            &at,
            frames.clone(),
            "video effects",
        );
    }
    // A fade to black or white that is running (the frames after it are drawn: see above), a dip to white anywhere.
    let (tl, a) = one(&m.testsrc, &|c| c.fade_out = 0.3);
    refused("grade/fading out", &tl, &a, &at, 24..30, "fade to black or white");
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.grade = grade.clone();
    inc.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), inc]], None);
    refused(
        "grade/dipping through white",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &at,
        0..60,
        "fade to black or white",
    );
    // A moving zoom behind the grade.
    let (tl, a) = one(&m.testsrc, &|c| {
        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.4, 0.0, 0.0)];
    });
    refused("grade/moving zoom", &tl, &a, &at, frames.clone(), "a grade");
    // A matrix with no measured round trip.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-grade");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bt2020.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=30:duration=2,setparams=colorspace=bt2020nc",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-qp",
        "0",
        "-pix_fmt",
        "yuv420p",
        p.to_str().unwrap(),
    ]);
    let bt2020 = probed(&p);
    let (tl, a) = one(&bt2020, &|_| {});
    refused("grade/bt2020", &tl, &a, &at, frames, "colour matrix");

    // A machine whose FFmpeg does not convert as the model does (another CPU's kernels), or whose probe could not tell, has no
    // grade in its caps: the plan refuses the layer up front (nothing is decoded for it), and the frame is FFmpeg's.
    let (tl, a) = one(&m.testsrc, &|_| {});
    let planner = Planner::new(&tl, &a, &at, PlanRequest::motion(policy())).expect("planner");
    let plan = planner.at_frame(3).expect("plan");
    let size = plan.size(u32::MAX);
    assert!(plan.reasons(&compositor().caps(), size).is_empty());
    let picture = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    for kernels in [kerf_gpu::grade::Kernels::Differs("a test"), kerf_gpu::grade::Kernels::Unknown] {
        let unmatched = Compositor::new(gpu()).expect("a compositor").with_grade_kernels(kernels);
        assert_eq!(unmatched.caps().grade, kerf_core::GradeKinds::NONE, "{kernels:?}");
        let why = plan.reasons(&unmatched.caps(), size);
        assert!(
            why.iter().any(|u| matches!(u, Unsupported::Grade(..))),
            "{kernels:?}: the plan should refuse the grade: {why:?}"
        );
        let err = unmatched
            .composite_yuv(&plan, &picture, size)
            .expect_err("the render should refuse");
        assert!(matches!(&err, kerf_gpu::GpuError::Unsupported(_)), "{kernels:?}: {err}");
    }
    let measured = Compositor::new(gpu())
        .expect("a compositor")
        .with_grade_kernels(kerf_gpu::grade::Kernels::Match);
    assert_eq!(measured.caps().grade, kerf_core::GradeKinds::ALL);
    measured.composite_yuv(&plan, &picture, size).expect("pinned to match: drawn");
    eprintln!("grade/kernels                                a machine whose conversions differ is refused by the plan");
}

/// What follows the grade in the chain that is the layer's own: a keyed opacity (a `geq` on the alpha plane, the value the
/// plan evaluates) and a keyed position (the overlay's, per frame), on a layer over another track, are held to what they
/// are held to without a grade (a dissolve's alpha ramp is in `what_follows_a_grade_is_drawn_as_it_is_without_one`). A clip's own
/// `fade_in` / `fade_out` is not an alpha ramp: it is
/// `fade` to black of the picture, whatever the track, which runs on the RGB picture the grade leaves, so the frames it
/// runs on are FFmpeg's (and the others are compared).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_graded_layer_is_keyed_in_opacity_and_position_over_another_and_its_fades_are_refused() {
    use kerf_core::{Property, PropertyKey};
    let m = media();
    let all = grades();
    let heavy = all[8].1.clone();
    let wheels = all[1].1.clone();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let over = |c: Clip| timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
    let graded = |grade: &kerf_core::Grade| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.grade = grade.clone();
        c
    };
    let key = |time: f64, value: f64| PropertyKey::new(time, value);

    // A keyed opacity.
    for (name, grade) in [("all three", &heavy), ("wheels", &wheels)] {
        let mut c = graded(grade);
        c.set_property_keys(Property::Opacity, vec![key(0.0, 1.0), key(1.0, 0.2)]);
        run(&format!("grade/keyed opacity, {name}"), &over(c), &assets, 30.0, 1.0, EXACT);
    }
    let mut c = graded(&heavy);
    c.set_property_keys(Property::Opacity, vec![key(0.1, 0.0), key(0.9, 1.0)]);
    run("grade/keyed opacity from nothing", &over(c), &assets, 30.0, 1.0, EXACT);
    // A keyed position that holds: the picture stays its own size and the overlay puts it off the canvas' edge.
    let mut c = graded(&heavy);
    c.set_property_keys(Property::PosX, vec![key(0.0, 0.3), key(1.0, 0.3)]);
    c.set_property_keys(Property::PosY, vec![key(0.0, -0.2), key(1.0, -0.2)]);
    run("grade/keyed position, held", &over(c), &assets, 30.0, 1.0, EXACT);
    // One that moves is placed by the export's `perspective`, refused; with the opacity too.
    let mut c = graded(&heavy);
    c.set_property_keys(Property::PosX, vec![key(0.0, -0.1), key(1.0, 0.1)]);
    run_placed("grade/keyed position", &over(c), &assets, 30.0, 1.0);
    let mut c = graded(&heavy);
    c.set_property_keys(Property::Opacity, vec![key(0.0, 0.3), key(1.0, 0.9)]);
    c.set_property_keys(Property::PosX, vec![key(0.0, -0.1), key(1.0, 0.1)]);
    run_placed("grade/keyed opacity and position", &over(c), &assets, 30.0, 1.0);

    // Its own fade-in and fade-out, on a layer over another track: refused where they run, drawn after and before.
    let mut c = graded(&heavy);
    c.fade_in = 0.3;
    c.fade_out = 0.3;
    run_refusing(
        "grade/fades over another track",
        &over(c),
        &assets,
        30.0,
        1.0,
        EXACT,
        &refusing("GradeWith", |u| matches!(u, Unsupported::GradeWith(..))),
    );
}

/// A picture that names BT.601's coefficients (`smpte170m`, `bt470bg`) is carried through the grade like BT.709's: out of
/// YCbCr with the tag's matrix, graded, back through an explicit `scale=out_color_matrix=bt470` to 4:4:4 and into 4:2:0 by the
/// next filter's conversion, tag set again. The route is BT.601's constants and the tagged way out (`GradeRoute`): exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_grade_on_a_picture_tagged_smpte170m_or_bt470bg_is_the_601_matrix_the_tagged_way() {
    let v = variants();
    for (name, grade) in grades() {
        for (source, asset) in [("testsrc170m", &v.testsrc170m), ("testsrc470bg", &v.testsrc470bg)] {
            let (tl, assets) = graded_alone(asset, &grade, &|_| {});
            let planner = Planner::new(&tl, &assets, &at_fps(30.0), PlanRequest::motion(policy())).unwrap();
            let route = planner.at_frame(3).unwrap().layers[0].grade_route().unwrap();
            assert!(
                route.via_444 && route.matrix == kerf_core::YuvMatrix::Bt601,
                "{source}: {route:?}"
            );
            run(&format!("grade/{name}, {source}"), &tl, &assets, 30.0, 0.4, EXACT);
        }
    }
    let (_, heavy) = grades().remove(8);
    let (tl, assets) = graded_alone(&v.bars470bg, &heavy, &|_| {});
    run("grade/lut, curves and wheels, bars470bg", &tl, &assets, 30.0, 0.4, EXACT);
}

/// A graded clip beside a clip of another colour matrix in the same cut (an untagged standard-definition one, which FFmpeg
/// reads as BT.601, beside a BT.709 one). FFmpeg 9 negotiates the matrix across the whole graph: it converts the grade's
/// way back into the bottom layer's matrix on every frame of the clip, whether or not the two are on screen together
/// (22 to 24 levels off the model on the untagged clip's frames, which the per-frame stack check did not see), so under
/// `BottomLayerTag` the plan refuses the clip in a cut that mixes matrices (`mixed_matrices_cut`), and FFmpeg draws it.
/// Under `FixedBt601` (6.1) every layer is taken as it is and the cut is drawn to the level, in either order.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_graded_clip_beside_a_clip_of_another_matrix_is_refused_where_ffmpeg_converts_it_across_the_graph() {
    let m = media();
    let v = variants();
    let all = grades();
    let negotiates = policy() == CompositeColorPolicy::BottomLayerTag;
    let mixed = refusing(
        "GradeWith(mixes colour matrices)",
        |u| matches!(u, Unsupported::GradeWith(_, why) if why.contains("mixes colour matrices")),
    );
    let case = |name: &str, tl: &Timeline, assets: &[Asset], secs: f64| {
        if negotiates {
            run_refusing(name, tl, assets, 30.0, secs, EXACT, &mixed);
        } else {
            run(name, tl, assets, 30.0, secs, EXACT);
        }
    };
    for (name, grade) in [&all[1], &all[8]] {
        let graded = |start: f64| {
            let mut c = clip(&m.testsrc, 0.5, 1.5, start);
            c.grade = grade.clone();
            c
        };
        let assets = [v.bars709.clone(), m.testsrc.clone()];
        // After the BT.709 clip (the bottom layer's matrix is BT.709; the frames of the BT.709 clip are drawn).
        let tl = timeline(vec![vec![clip(&v.bars709, 0.5, 1.5, 0.0), graded(1.0)]], None);
        case(&format!("grade/untagged SD after a BT.709 clip, {name}"), &tl, &assets, 2.0);
        // Stacked: the graded one over the BT.709 one, and under it.
        let tl = timeline(vec![vec![clip(&v.bars709, 0.5, 1.5, 0.0)], vec![graded(0.0)]], None);
        case(&format!("grade/untagged SD over a BT.709 clip, {name}"), &tl, &assets, 1.0);
        let tl = timeline(vec![vec![graded(0.0)], vec![clip(&v.testsrc709, 0.5, 1.5, 0.0)]], None);
        let stacked = [m.testsrc.clone(), v.testsrc709.clone()];
        case(&format!("grade/untagged SD under a BT.709 clip, {name}"), &tl, &stacked, 1.0);
        // The other way round (the graded clip is the bottom layer, and the BT.709 clip is converted into BT.601, which
        // is the ungraded clip's own matter, unmeasured: `core-render-plan.md`'s A7 concern): drawn under 6.1 only.
        if !negotiates {
            let tl = timeline(vec![vec![graded(0.0), clip(&v.bars709, 0.5, 1.5, 1.0)]], None);
            case(&format!("grade/untagged SD before a BT.709 clip, {name}"), &tl, &assets, 2.0);
        }
    }
}

/// What is one matrix is not a mix: a graded clip beside clips of its own matrix is drawn to the level on both FFmpegs, in
/// either order (untagged beside untagged and beside a BT.601 tag, a BT.709 clip beside another, a BT.709-tagged one
/// beside an untagged high-definition clip, which the graph labels BT.709).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_graded_clip_beside_a_clip_of_its_own_matrix_is_drawn_in_either_order() {
    let m = media();
    let v = variants();
    let (_, grade) = grades().remove(8);
    let pairs: [(&str, &Asset, &Asset); 3] = [
        ("untagged and untagged", &m.testsrc, &m.bars),
        ("untagged and smpte170m", &m.testsrc, &v.testsrc170m),
        ("bt709 and bt709", &v.testsrc709, &v.bars709),
    ];
    for (name, graded, other) in pairs {
        let assets = [graded.clone(), other.clone()];
        let mut g = clip(graded, 0.5, 1.5, 1.0);
        g.grade = grade.clone();
        let tl = timeline(vec![vec![clip(other, 0.5, 1.5, 0.0), g]], None);
        run(&format!("grade/{name}, graded second"), &tl, &assets, 30.0, 2.0, EXACT);
        let mut g = clip(graded, 0.5, 1.5, 0.0);
        g.grade = grade.clone();
        let tl = timeline(vec![vec![g, clip(other, 0.5, 1.5, 1.0)]], None);
        run(&format!("grade/{name}, graded first"), &tl, &assets, 30.0, 2.0, EXACT);
    }
    // The tagged way out (smpte170m / bt470bg: BT.601's coefficients through 4:4:4) beside an untagged clip.
    let mut g = clip(&v.testsrc170m, 0.5, 1.5, 1.0);
    g.grade = grade;
    let assets = [m.bars.clone(), v.testsrc170m.clone()];
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), g]], None);
    run("grade/smpte170m after untagged", &tl, &assets, 30.0, 2.0, EXACT);
}

// ---- blend modes ---------------------------------------------------------------------------

/// Run `body` with the compositor that has every blend mode in its caps, however this FFmpeg's probe went.
fn forced_blends(body: impl FnOnce()) {
    FORCED.with(|f| f.set(true));
    body();
    FORCED.with(|f| f.set(false));
}

/// A blend case: drawn and compared exactly where this FFmpeg's chain is the model's (`blend::kernels`); where the probe refused
/// the build (FFmpeg 4.4's `overlay` loses the alpha rows of a clip above the frame when it runs on several threads) the plan must
/// refuse every frame, FFmpeg draws them, and the case is drawn anyway with the caps forced (`forced_ok`: unless the clip is above
/// the frame's top edge, which is the bug) to hold the arithmetic itself to the export's planes.
fn blend_run(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, forced_ok: bool) {
    if compositor().caps().blend == kerf_core::BlendKinds::ALL {
        return run(case, tl, assets, fps, secs, EXACT);
    }
    refused(
        case,
        tl,
        assets,
        &at_fps(fps),
        upto(secs, fps),
        "no compositor pass draws this mode",
    );
    if forced_ok {
        forced_blends(|| run(&format!("{case} (forced)"), tl, assets, fps, secs, EXACT));
    }
}

const BLEND_MODES: [kerf_core::BlendMode; 9] = {
    use kerf_core::BlendMode::*;
    [
        Multiply, Screen, Overlay, HardLight, Add, Darken, Lighten, Difference, Exclusion,
    ]
};

/// A blended clip is FFmpeg's `blend` in RGB between two conversions (`engine/cli/blend.rs`), drawn in integers
/// (`src/blend.rs`, held to the filters over every value by `tests/blend_kernels.rs`): every mode, every picture (the
/// noise has every Y, U, V combination, clipped RGB included), exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn every_blend_mode_is_drawn_as_the_chain_draws_it() {
    let m = media();
    for mode in BLEND_MODES {
        let (tl, assets) = over_bars(m, &|c| c.blend_mode = mode);
        blend_run(&format!("blend/{mode:?}"), &tl, &assets, 30.0, 0.5, true);
        // The other way round: the bars on the test pattern.
        let mut top = clip(&m.bars, 0.5, 1.5, 0.0);
        top.blend_mode = mode;
        blend_run(
            &format!("blend/{mode:?} bars over testsrc"),
            &timeline(vec![vec![clip(&m.testsrc, 0.5, 1.5, 0.0)], vec![top]], None),
            &[m.testsrc.clone(), m.bars.clone()],
            30.0,
            0.5,
            true,
        );
        // Noise over a smooth picture, and a smooth picture over noise: the clip's colour and the composite's are
        // out of gamut in both directions, and every sample is another triple.
        let mut noise = clip(&m.noise, 0.5, 1.5, 0.0);
        noise.blend_mode = mode;
        blend_run(
            &format!("blend/{mode:?} noise over smooth"),
            &timeline(vec![vec![clip(&m.smooth, 0.5, 1.5, 0.0)], vec![noise]], None),
            &[m.smooth.clone(), m.noise.clone()],
            30.0,
            0.4,
            true,
        );
        let mut smooth = clip(&m.smooth, 0.5, 1.5, 0.0);
        smooth.blend_mode = mode;
        blend_run(
            &format!("blend/{mode:?} smooth over noise"),
            &timeline(vec![vec![clip(&m.noise, 0.5, 1.5, 0.0)], vec![smooth]], None),
            &[m.noise.clone(), m.smooth.clone()],
            30.0,
            0.4,
            true,
        );
    }
}

/// A blended clip's alpha is `overlay`'s on the last step: its opacity (keyed: a `geq`), a mask's, a chroma key's, its fades
/// (alpha fades in and out: a multiplied clip must not go through black) and a dissolve's ramp weigh the mix, per luma sample
/// and per 2x2 block of chroma, exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blended_clip_is_laid_by_its_own_alpha() {
    use kerf_core::{BlendMode, Easing, Property, PropertyKey};
    let m = media();
    for mode in [
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Difference,
    ] {
        let label = |what: &str| format!("blend/{mode:?} {what}");
        // The clip's own fades, in and out.
        let (tl, assets) = over_bars(m, &|c| {
            c.blend_mode = mode;
            c.fade_in = 0.4;
            c.fade_out = 0.4;
        });
        blend_run(&label("fades"), &tl, &assets, 30.0, 1.0, true);
        // A keyed opacity: a `geq` alpha, colours untouched.
        for (name, keys) in [
            ("linear", vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 0.2)]),
            (
                "from nothing, eased",
                vec![
                    PropertyKey {
                        time: 0.1,
                        value: 0.0,
                        easing: Easing::EaseInOut,
                    },
                    PropertyKey::new(0.9, 1.0),
                ],
            ),
        ] {
            let (tl, assets) = over_bars(m, &|c| {
                c.blend_mode = mode;
                c.set_property_keys(Property::Opacity, keys.clone());
            });
            blend_run(&label(&format!("keyed opacity {name}")), &tl, &assets, 30.0, 1.0, true);
        }
        // A mask is an alpha plane (`geq`), a feathered one has every level.
        let (tl, assets) = over_bars(m, &|c| {
            c.blend_mode = mode;
            c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, false));
        });
        blend_run(&label("masked"), &tl, &assets, 30.0, 0.5, true);
        let (tl, assets) = over_bars(m, &|c| {
            c.blend_mode = mode;
            c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.2, true));
            c.fade_in = 0.3;
        });
        blend_run(&label("inverted mask and a fade"), &tl, &assets, 30.0, 0.8, true);
        // A chroma key is the other source of alpha.
        let (tl, assets) = over_bars(m, &|c| {
            c.blend_mode = mode;
            c.effects = vec![key("green", 0.15, 0.1)];
        });
        blend_run(&label("keyed"), &tl, &assets, 30.0, 0.5, true);
        // Into a dissolve: the incoming blended clip's alpha is the ramp, the outgoing one is plain.
        let out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
        inc.blend_mode = mode;
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        blend_run(
            &label("dissolving in"),
            &timeline(vec![vec![out, inc]], None),
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            2.0,
            true,
        );
    }
}

/// Where a blended clip lies: moved and partly off the frame, with nothing below it, over two layers, over another blended clip,
/// part of the time; then on BT.709 and BT.601-tagged footage, where the chain names the composite's matrix.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blended_clip_is_laid_where_it_lies() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    for mode in [BlendMode::Multiply, BlendMode::HardLight, BlendMode::Exclusion] {
        let label = |what: &str| format!("blend/{mode:?} {what}");
        for (name, x, y) in [
            ("moved", 0.2, -0.1),
            ("moved off the frame", -0.45, 0.4),
            ("moved by odd pixels", 0.0123, 0.0071),
        ] {
            let (tl, assets) = over_bars(m, &|c| {
                c.blend_mode = mode;
                c.transform.pos_x = x;
                c.transform.pos_y = y;
            });
            blend_run(&label(name), &tl, &assets, 30.0, 0.5, y >= 0.0);
        }
        // Nothing below: the composite is the black canvas.
        let mut alone = clip(&m.testsrc, 0.5, 1.5, 0.0);
        alone.blend_mode = mode;
        blend_run(
            &label("over nothing"),
            &timeline(vec![vec![alone]], None),
            std::slice::from_ref(&m.testsrc),
            30.0,
            0.5,
            true,
        );
        // A blended clip over a blended one, over the plain bars; and the plain clip over the lot.
        let mut mid = clip(&m.testsrc, 0.5, 1.5, 0.0);
        mid.blend_mode = mode;
        let mut top = clip(&m.noise, 0.5, 1.5, 0.0);
        top.blend_mode = BlendMode::Screen;
        let plain = clip(&m.edges, 0.5, 1.5, 0.0);
        blend_run(
            &label("two blended clips over bars"),
            &timeline(
                vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![mid.clone()], vec![top.clone()]],
                None,
            ),
            &[m.bars.clone(), m.testsrc.clone(), m.noise.clone()],
            30.0,
            0.5,
            true,
        );
        let mut late = mid.clone();
        late.timeline_start = 0.4;
        blend_run(
            &label("starting late, with a plain clip over it"),
            &timeline(vec![vec![clip(&m.bars, 0.5, 2.0, 0.0)], vec![late], vec![plain]], None),
            &[m.bars.clone(), m.testsrc.clone(), m.edges.clone()],
            30.0,
            1.2,
            true,
        );
        // The composite's matrix is the bottom layer's tag: BT.709 and BT.601-tagged footage convert in theirs.
        for (name, base, top) in [
            ("bt709", &v.bars709, &v.testsrc709),
            ("smpte170m", &v.testsrc170m, &v.testsrc170m),
            ("bt470bg", &v.bars470bg, &v.testsrc470bg),
        ] {
            let mut c = clip(top, 0.5, 1.5, 0.0);
            c.blend_mode = mode;
            blend_run(
                &label(&format!("on {name} footage")),
                &timeline(vec![vec![clip(base, 0.5, 1.5, 0.0)], vec![c]], None),
                &[base.clone(), top.clone()],
                30.0,
                0.5,
                true,
            );
        }
    }
}

/// What the chain is not exact for is refused, in the plan, and FFmpeg draws it: a layer the compositor scales (the scaler's level
/// is not swscale's and the blend carries it into the composite), a rotated one (a fixed-point edge), one below full opacity (the RGB
/// round trip's chroma is a level off), a letterboxed one (its bars are transparent in the blend), an RGB picture (the decode's 4:2:0
/// is a few levels from the graph's conversion).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_a_blend_is_not_exact_for_is_refused() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let blended = |asset: &Asset, start: f64, secs: f64, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(asset, start, start + secs, 0.0);
        c.blend_mode = BlendMode::Multiply;
        edit(&mut c);
        c
    };
    let over_bars = |top: Clip, asset: &Asset| {
        (
            timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![top]], None),
            [m.bars.clone(), asset.clone()],
        )
    };
    for (name, reason, top, asset) in [
        (
            "scaled",
            "scaler resizes",
            blended(&m.testsrc, 0.5, 1.0, &|c| c.transform.scale = 0.6),
            &m.testsrc,
        ),
        (
            "rotated",
            "rotated layer",
            blended(&m.testsrc, 0.5, 1.0, &|c| c.transform.rotation = 12.0),
            &m.testsrc,
        ),
        (
            "translucent",
            "below full opacity",
            blended(&m.testsrc, 0.5, 1.0, &|c| c.transform.opacity = 0.6),
            &m.testsrc,
        ),
        (
            "letterboxed",
            "letterboxed layer",
            blended(&m.portrait, 0.5, 1.0, &|_| {}),
            &m.portrait,
        ),
        (
            "a graded clip",
            "a grade under a blend",
            blended(&m.testsrc, 0.5, 1.0, &|c| {
                c.grade.wheels = Some(kerf_core::Wheels {
                    shadows: kerf_core::Wheel {
                        x: 0.3,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }),
            &m.testsrc,
        ),
        (
            "a rotation that is not a quarter turn",
            "rotated layer",
            blended(&m.testsrc, 0.5, 1.0, &|c| c.transform.rotation = 91.0),
            &m.testsrc,
        ),
    ] {
        let (tl, assets) = over_bars(top, asset);
        refused(
            &format!("blend/refused {name}"),
            &tl,
            &assets,
            &at_fps(30.0),
            upto(0.5, 30.0),
            reason,
        );
    }
    // An RGB picture is refused where the probe found the graph's `format=yuva420p` is not the decode's conversion (FFmpeg 4.4's is), and
    // drawn exactly where it is (6.1, 9.0).
    let (tl, assets) = over_bars(blended(&v.rgb_png, 0.0, 1.0, &|_| {}), &v.rgb_png);
    if compositor().caps().blend_extras.contains(kerf_core::BlendExtras::RGB_PICTURE) {
        blend_run("blend/an RGB picture", &tl, &assets, 30.0, 0.5, true);
    } else {
        refused(
            "blend/refused an RGB picture",
            &tl,
            &assets,
            &at_fps(30.0),
            upto(0.5, 30.0),
            "RGB picture",
        );
    }
}

// ==== checker/e: blend modes, checked independently of the implementer's cases ======================================
//
// Every case below is drawn through `blend_run`: where this FFmpeg's blend chain is the model's the compositor's frames
// are compared with the export's planes at zero tolerance, and where the probe refused the build (FFmpeg 4.4) the plan must
// refuse every frame and the arithmetic is held to the export with the caps forced.

/// A cut of `bottom` (the composite) with `top` blended over it on a second track; `edit` decides what the blended clip carries.
fn ce_stack(bottom: &Asset, top: &Asset, edit: &dyn Fn(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut c = clip(top, 0.5, 1.5, 0.0);
    edit(&mut c);
    (
        timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0)], vec![c]], None),
        vec![bottom.clone(), top.clone()],
    )
}

/// A blended clip as large as the frame at every pixel offset that matters on a 640x360 frame: next to each edge and corner, on the
/// half way lines, on and off the frame, odd offsets (`overlay` floors them to even) and offsets a hair short of the float.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clip_at_every_edge_offset() {
    let m = media();
    #[rustfmt::skip]
    let offsets: [(f64, f64); 32] = [
        (1.0, 0.0), (-1.0, 0.0), (2.0, 0.0), (3.0, 0.0), (-2.0, 0.0), (-3.0, 0.0),
        (638.0, 0.0), (639.0, 0.0), (640.0, 0.0), (-638.0, 0.0), (-639.0, 0.0), (-640.0, 0.0),
        (0.0, 1.0), (0.0, -1.0), (0.0, 3.0), (0.0, -3.0), (0.0, 358.0), (0.0, 359.0), (0.0, 360.0),
        (0.0, -358.0), (0.0, -359.0), (0.0, -360.0),
        (318.0, 178.0), (-318.0, -178.0), (319.0, 179.0), (-321.0, 181.0), (637.0, 357.0), (-637.0, -357.0),
        (639.0, 359.0), (-639.0, -359.0), (7.0, 5.0), (-7.0, -5.0),
    ];
    let tops = [("testsrc", &m.testsrc), ("noise", &m.noise), ("edges", &m.edges)];
    let bottoms = [&m.bars, &m.smooth, &m.testsrc];
    for (i, (dx, dy)) in offsets.iter().enumerate() {
        for (k, mode) in BLEND_MODES.iter().enumerate() {
            // Every mode at the first dozen offsets, a third of them at the rest.
            if i >= 12 && (k + i) % 3 != 0 {
                continue;
            }
            let (name, top) = tops[(i + k) % 3];
            let bottom = bottoms[(i + 2 * k) % 3];
            let (tl, assets) = ce_stack(bottom, top, &|c| {
                c.blend_mode = *mode;
                c.transform.pos_x = dx / 640.0;
                c.transform.pos_y = dy / 360.0;
            });
            ce_blend_run(
                &format!("checker/e/offset {dx},{dy} {mode:?} {name}"),
                &tl,
                &assets,
                30.0,
                0.4,
                *dy >= 0.0,
            );
        }
    }
}

/// The alpha a blended clip lays its mix by: fades of a frame and of the whole clip, overlapping fades, an opacity key that holds, steps and
/// sits at either end, masks of every shape and feather, a chroma key with every similarity and blend, and several at once.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_alpha_extremes_of_a_blended_clip() {
    use kerf_core::{BlendMode, Easing, Property, PropertyKey};
    let m = media();
    let modes = [
        BlendMode::Screen,
        BlendMode::Exclusion,
        BlendMode::Multiply,
        BlendMode::HardLight,
        BlendMode::Difference,
    ];
    for (n, mode) in modes.into_iter().enumerate() {
        let label = |what: &str| format!("checker/e/{mode:?} {what}");
        let go = |what: &str, secs: f64, edit: &dyn Fn(&mut Clip)| {
            let (tl, assets) = ce_stack(&m.bars, &m.testsrc, &|c| {
                c.blend_mode = mode;
                edit(c);
            });
            ce_blend_run(&label(what), &tl, &assets, 30.0, secs, true);
        };
        // Fades: one frame, less than a frame, the whole clip, both at once and overlapping.
        for (fi, fo) in [
            (0.034, 0.0),
            (0.0, 0.034),
            (0.01, 0.01),
            (1.0, 0.0),
            (0.0, 1.0),
            (0.7, 0.7),
            (1.0, 1.0),
            (0.3, 0.5),
        ] {
            go(&format!("fade {fi}/{fo}"), 1.0, &|c| {
                c.fade_in = fi;
                c.fade_out = fo;
            });
        }
        // The opacity key at its extremes.
        for (what, keys) in [
            ("opacity 1 to 0", vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 0.0)]),
            ("opacity 0 to 1", vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(1.0, 1.0)]),
            (
                "opacity held at 0.5",
                vec![PropertyKey::new(0.0, 0.5), PropertyKey::new(1.0, 0.5)],
            ),
            (
                "opacity stepping",
                vec![
                    PropertyKey {
                        time: 0.0,
                        value: 0.9,
                        easing: Easing::Hold,
                    },
                    PropertyKey {
                        time: 0.3,
                        value: 0.1,
                        easing: Easing::Hold,
                    },
                    PropertyKey::new(0.7, 1.0),
                ],
            ),
            (
                "opacity eased near 1",
                vec![
                    PropertyKey {
                        time: 0.0,
                        value: 0.996,
                        easing: Easing::EaseOut,
                    },
                    PropertyKey::new(1.0, 0.004),
                ],
            ),
            (
                "opacity bezier",
                vec![
                    PropertyKey {
                        time: 0.0,
                        value: 0.2,
                        easing: Easing::Bezier {
                            x1: 0.9,
                            y1: 0.0,
                            x2: 0.1,
                            y2: 1.0,
                        },
                    },
                    PropertyKey::new(1.0, 0.8),
                ],
            ),
        ] {
            go(what, 1.0, &|c| c.set_property_keys(Property::Opacity, keys.clone()));
        }
        // Masks: every shape and feather, inverted, off centre.
        for (shape, feather, inverted) in [
            (kerf_core::MaskShape::Rect, 0.0, false),
            (kerf_core::MaskShape::Ellipse, 0.0, false),
            (kerf_core::MaskShape::Rect, 1.0, false),
            (kerf_core::MaskShape::Ellipse, 1.0, true),
            (kerf_core::MaskShape::Ellipse, 0.05, true),
            (kerf_core::MaskShape::Rect, 0.5, true),
        ] {
            go(&format!("mask {shape:?} {feather} {inverted}"), 0.4, &|c| {
                c.mask = Some(mask_of(shape, feather, inverted));
            });
        }
        let off_centre = kerf_core::Mask {
            shape: kerf_core::MaskShape::Ellipse,
            x: 0.9,
            y: 0.1,
            width: 0.7,
            height: 0.5,
            feather: 0.3,
            inverted: false,
        };
        go("mask off centre", 0.4, &|c| c.mask = Some(off_centre));
        // Chroma keys: similarity and blend at 0, middle and 1, three colours.
        for (colour, similarity, blend) in [
            ("green", 0.01, 0.0),
            ("green", 0.3, 0.2),
            ("green", 1.0, 1.0),
            ("0xff0000", 0.4, 0.5),
            ("blue", 0.25, 0.0),
            ("0x808080", 0.5, 0.05),
        ] {
            go(&format!("key {colour} {similarity}/{blend}"), 0.4, &|c| {
                c.effects = vec![key(colour, similarity, blend)];
            });
        }
        // Everything at once: a key, a mask, a keyed opacity, both fades.
        go("key, mask, keyed opacity and fades", 1.0, &|c| {
            c.effects = vec![key("green", 0.3, 0.3)];
            c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.4, true));
            c.set_property_keys(
                Property::Opacity,
                vec![PropertyKey::new(0.0, 0.3), PropertyKey::new(1.0, 1.0)],
            );
            c.fade_in = 0.3;
            c.fade_out = 0.3;
        });
        // A mask and a fade on a clip that is moved, partly off the frame.
        go("moved mask and fade", 0.8, &|c| {
            c.transform.pos_x = 0.31;
            c.transform.pos_y = 0.27;
            c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.3, false));
            c.fade_in = 0.4;
        });
        let _ = n;
    }
}

/// A blended clip into and out of every transition, as the incoming clip and as the outgoing one, at durations of a frame to a clip:
/// the fades a transition adds are alpha fades on a blended clip, and the slides and pushes move it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clip_in_every_transition() {
    use kerf_core::BlendMode;
    let m = media();
    let kinds = [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        // (DipToWhite is left out: the plain dip is not exact at the base commit either, `WHITE`.)
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ];
    let modes = [
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Difference,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        for (j, secs) in [0.034, 0.2, 0.5, 1.0].into_iter().enumerate() {
            let mode = modes[(i + j) % modes.len()];
            // Incoming, blended.
            let out = clip(&m.bars, 0.5, 1.5, 0.0);
            let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
            inc.blend_mode = mode;
            inc.transition_in = Some(Transition { kind, duration: secs });
            // The slide / push travels from off the frame above (SlideDown, PushDown) or the left: a clip above the frame's top
            // edge is where FFmpeg 4.4's overlay drops the alpha rows.
            let above = matches!(kind, TransitionKind::SlideDown | TransitionKind::PushDown);
            ce_blend_run(
                &format!("checker/e/incoming {kind:?} {secs}s {mode:?}"),
                &timeline(vec![vec![out, inc]], None),
                &[m.bars.clone(), m.testsrc.clone()],
                30.0,
                2.0,
                !above,
            );
            // Outgoing, blended: the cut's second clip plain, entering by the transition over the blended one's tail.
            let mut out = clip(&m.testsrc, 0.5, 1.5, 0.0);
            out.blend_mode = mode;
            let mut inc = clip(&m.bars, 0.5, 1.5, 1.0);
            inc.transition_in = Some(Transition { kind, duration: secs });
            // (Over a plain clip underneath so that the blend has a composite that is not black.)
            let under = clip(&m.smooth, 0.5, 2.5, 0.0);
            ce_blend_run(
                &format!("checker/e/outgoing {kind:?} {secs}s {mode:?}"),
                &timeline(vec![vec![under], vec![out, inc]], None),
                &[m.smooth.clone(), m.testsrc.clone(), m.bars.clone()],
                30.0,
                2.0,
                true,
            );
        }
    }
    // Two blended clips back to back through every fade a transition makes (a dip to white is an alpha fade on a blended clip, so
    // this is exact where a plain clip's dip is not).
    for (i, kind) in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::SlideLeft,
        TransitionKind::PushUp,
    ]
    .into_iter()
    .enumerate()
    {
        for secs in [0.2, 0.6] {
            let mut out = clip(&m.testsrc, 0.5, 1.5, 0.0);
            out.blend_mode = modes[i % modes.len()];
            let mut inc = clip(&m.noise, 0.5, 1.5, 1.0);
            inc.blend_mode = modes[(i + 1) % modes.len()];
            inc.transition_in = Some(Transition { kind, duration: secs });
            ce_blend_run(
                &format!("checker/e/two blended clips through {kind:?} {secs}s"),
                &timeline(vec![vec![clip(&m.bars, 0.5, 2.5, 0.0)], vec![out, inc]], None),
                &[m.bars.clone(), m.testsrc.clone(), m.noise.clone()],
                30.0,
                2.0,
                true,
            );
        }
    }
    // A clip's own fade-in together with a transition's: two alpha fades on the same edge. (All three are run, then judged.)
    let mut doubled: Vec<String> = Vec::new();
    for (what, kind, fade_in) in [
        ("a fade-in and a dissolve", TransitionKind::Crossfade, 0.3),
        ("a fade-in and a dip to black", TransitionKind::DipToBlack, 0.3),
        ("a fade-in and a dip to white", TransitionKind::DipToWhite, 0.3),
    ] {
        // (The outgoing clip is blended too, so that no plain clip's dip to white, which is not exact at the base commit, is in the frame.)
        let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
        out.blend_mode = BlendMode::Multiply;
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
        inc.blend_mode = BlendMode::Screen;
        inc.fade_in = fade_in;
        inc.transition_in = Some(Transition { kind, duration: 0.5 });
        let tl = timeline(vec![vec![clip(&m.smooth, 0.5, 2.5, 0.0)], vec![out, inc]], None);
        let assets = [m.smooth.clone(), m.bars.clone(), m.testsrc.clone()];
        let scan = ce_scan(
            &format!("checker/e/{what}"),
            &tl,
            &assets,
            &at_fps(30.0),
            upto(2.0, 30.0),
            false,
        );
        eprintln!("{}", scan.line(&format!("checker/e/{what}")));
        if !scan.exact() {
            doubled.push(format!(
                "{what}: drawn {}, Y differ {}, UV differ {}, guards {:?}",
                scan.drawn, scan.y.differing, scan.uv.differing, scan.guards
            ));
        }
    }
    assert!(
        doubled.is_empty(),
        "the plan lets these through and the compositor cannot draw them (or draws them wrong): {doubled:?}"
    );
}

/// How a blended clip stands to the others: over and under blended and plain clips, ending before the composite does, starting late, with gaps
/// between blended clips on a track and between frames of its window, at times between two output frames.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clip_among_other_clips() {
    use kerf_core::BlendMode;
    let m = media();
    // The blended clip ends well before the composite does: nothing may be blended after it, in the export either.
    for (i, mode) in BLEND_MODES.iter().enumerate() {
        let mut top = clip(&m.testsrc, 0.5, 1.0, 0.25);
        top.blend_mode = *mode;
        ce_blend_run(
            &format!("checker/e/{mode:?} ends before the composite"),
            &timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None),
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            2.0,
            true,
        );
        // Two blended clips on one track with a gap between them, over a plain clip that plays through.
        let mut a = clip(&m.testsrc, 0.5, 1.0, 0.1);
        a.blend_mode = *mode;
        let mut b = clip(&m.noise, 0.5, 1.0, 0.9 + 0.01 * i as f64);
        b.blend_mode = BLEND_MODES[(i + 4) % 9];
        ce_blend_run(
            &format!("checker/e/{mode:?} then another with a gap"),
            &timeline(vec![vec![clip(&m.smooth, 0.0, 2.0, 0.0)], vec![a, b]], None),
            &[m.smooth.clone(), m.testsrc.clone(), m.noise.clone()],
            30.0,
            1.8,
            true,
        );
    }
    // Start and end between two output frames.
    for start in [0.0166, 0.0334, 0.05, 0.1, 0.3333, 0.34] {
        let mut top = clip(&m.testsrc, 0.5, 1.0, start);
        top.blend_mode = BlendMode::Exclusion;
        ce_blend_run(
            &format!("checker/e/starting at {start}"),
            &timeline(vec![vec![clip(&m.bars, 0.0, 1.5, 0.0)], vec![top]], None),
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            1.2,
            true,
        );
    }
    // Pairs of blended clips, one over the other over bars.
    for (i, a) in BLEND_MODES.iter().enumerate() {
        for b in [BLEND_MODES[(i + 3) % 9], BLEND_MODES[(i + 5) % 9]] {
            let mut lower = clip(&m.testsrc, 0.5, 1.5, 0.0);
            lower.blend_mode = *a;
            let mut upper = clip(&m.noise, 0.5, 1.5, 0.0);
            upper.blend_mode = b;
            ce_blend_run(
                &format!("checker/e/{a:?} under {b:?}"),
                &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![lower], vec![upper]], None),
                &[m.bars.clone(), m.testsrc.clone(), m.noise.clone()],
                30.0,
                0.4,
                true,
            );
        }
    }
    // A blended clip under a plain clip that dissolves in over it, and over a clip that is itself dissolving.
    for mode in [BlendMode::Screen, BlendMode::Difference] {
        let mut blended = clip(&m.testsrc, 0.5, 1.5, 0.0);
        blended.blend_mode = mode;
        let mut plain = clip(&m.edges, 0.5, 1.5, 0.0);
        plain.transition_in = None;
        plain.fade_in = 0.5;
        ce_blend_run(
            &format!("checker/e/{mode:?} under a plain clip fading in"),
            &timeline(
                vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![blended.clone()], vec![plain]],
                None,
            ),
            &[m.bars.clone(), m.testsrc.clone(), m.edges.clone()],
            30.0,
            1.0,
            true,
        );
        let out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.smooth, 0.5, 1.5, 1.0);
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.6,
        });
        let mut over = clip(&m.noise, 0.5, 2.5, 0.0);
        over.blend_mode = mode;
        ce_blend_run(
            &format!("checker/e/{mode:?} over a dissolve"),
            &timeline(vec![vec![out, inc], vec![over]], None),
            &[m.bars.clone(), m.smooth.clone(), m.noise.clone()],
            30.0,
            2.0,
            true,
        );
    }
    // A blended clip alone on the bottom track, a plain clip over it.
    let mut bottom = clip(&m.testsrc, 0.5, 1.5, 0.0);
    bottom.blend_mode = BlendMode::Multiply;
    ce_blend_run(
        "checker/e/blended at the bottom, a plain clip over it",
        &timeline(vec![vec![bottom], vec![clip(&m.edges, 0.5, 1.0, 0.2)]], None),
        &[m.testsrc.clone(), m.edges.clone()],
        30.0,
        1.0,
        true,
    );
}

/// The matrix the chain names is the bottom layer's tag: every pairing of untagged, BT.709, `smpte170m` and `bt470bg` footage, the labelled
/// untagged HD, and a few modes each; and the tags the plan has no tables for must be refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_the_matrix_of_the_bottom_layer_names_the_chain() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let bottoms = [("untagged", &m.bars), ("bt709", &v.bars709), ("bt470bg", &v.bars470bg)];
    let tops = [
        ("untagged", &m.testsrc),
        ("bt709", &v.testsrc709),
        ("smpte170m", &v.testsrc170m),
        ("bt470bg", &v.testsrc470bg),
    ];
    for (i, (bname, bottom)) in bottoms.iter().enumerate() {
        for (j, (tname, top)) in tops.iter().enumerate() {
            for mode in [
                BlendMode::Multiply,
                BlendMode::Difference,
                BlendMode::Overlay,
                BlendMode::Screen,
            ] {
                if (i + j + mode.label().len()) % 2 == 1 {
                    continue;
                }
                let (tl, assets) = ce_stack(bottom, top, &|c| c.blend_mode = mode);
                ce_blend_run(
                    &format!("checker/e/{mode:?} {tname} over {bname}"),
                    &tl,
                    &assets,
                    30.0,
                    0.4,
                    true,
                );
            }
        }
    }
    // Untagged HD is labelled BT.709 by a graph that holds a clip that names a matrix.
    let h = hd_pair();
    for (bname, bottom, tname, top) in [
        ("untagged HD", &h.untagged, "tagged HD", &h.phone),
        ("tagged HD", &h.phone, "untagged HD", &h.untagged),
        ("untagged HD", &h.untagged, "untagged HD", &h.untagged),
        ("tagged HD", &h.phone, "tagged HD", &h.phone),
    ] {
        for mode in [BlendMode::Difference, BlendMode::Exclusion] {
            let mut c = clip(top, 0.5, 1.5, 0.0);
            c.blend_mode = mode;
            ce_blend_run(
                &format!("checker/e/{mode:?} {tname} over {bname}"),
                &timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0)], vec![c]], None),
                &[bottom.clone(), top.clone()],
                30.0,
                0.3,
                true,
            );
        }
    }
}

/// Footage of other pixel formats and ranges as the blended clip: the graph converts it to `yuva420p` where the compositor has the decode's
/// 4:2:0, so each is drawn exactly or refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clip_of_another_pixel_format_or_range() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let r = range_media();
    for (name, asset) in [
        ("10-bit", &v.tenbit),
        ("10-bit full range", &v.tenbit_pc),
        ("4:4:4", &v.yuv444),
        ("4:2:2", &v.yuv422),
        ("gray", &v.gray),
        ("yuvj420p mjpeg", &v.mjpeg),
        ("yuvj420p still", &v.jpeg),
        ("yuv420p flagged pc", &r.pc),
        ("yuv420p flagged pc gradient", &r.pc_gradient),
        ("yuv420p flagged tv", &r.tv),
        ("yuv420p unflagged", &r.unflagged),
        ("rgb png", &v.rgb_png),
        ("rgb noise png", &ce_rgb_noise()),
    ] {
        for mode in [BlendMode::Multiply, BlendMode::Difference] {
            // The picture as the blended clip, over the bars; and under a blended testsrc.
            let mut top = clip(asset, 0.0, 1.0, 0.0);
            top.blend_mode = mode;
            ce_blend_run(
                &format!("checker/e/{mode:?} {name} over bars"),
                &timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0)], vec![top]], None),
                &[m.bars.clone(), asset.clone()],
                30.0,
                0.4,
                true,
            );
            let mut top = clip(&m.testsrc, 0.0, 1.0, 0.0);
            top.blend_mode = mode;
            ce_blend_run(
                &format!("checker/e/{mode:?} testsrc over {name}"),
                &timeline(vec![vec![clip(asset, 0.0, 1.0, 0.0)], vec![top]], None),
                &[asset.clone(), m.testsrc.clone()],
                30.0,
                0.4,
                true,
            );
        }
    }
}

/// What the clip carries before the blend: colour correction, effects, a grade, a speed, a reverse, a 25 fps source on a 30 fps cut, a late source.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_what_a_blended_clip_carries_before_the_blend() {
    use kerf_core::{BlendMode, VideoEffect as E};
    let m = media();
    for (n, mode) in [BlendMode::Multiply, BlendMode::Screen, BlendMode::Overlay]
        .into_iter()
        .enumerate()
    {
        let label = |what: &str| format!("checker/e/{mode:?} {what}");
        let go = |what: &str, top: &Asset, edit: &dyn Fn(&mut Clip), secs: f64| {
            let (tl, assets) = ce_stack(&m.bars, top, &|c| {
                c.blend_mode = mode;
                edit(c);
            });
            ce_blend_run(&label(what), &tl, &assets, 30.0, secs, true);
        };
        for (what, color) in [
            (
                "brightness",
                kerf_core::Color {
                    brightness: 0.15,
                    ..Default::default()
                },
            ),
            (
                "contrast and saturation",
                kerf_core::Color {
                    contrast: 1.4,
                    saturation: 0.3,
                    ..Default::default()
                },
            ),
            (
                "gamma and temperature",
                kerf_core::Color {
                    gamma: 1.6,
                    temperature: 0.4,
                    ..Default::default()
                },
            ),
        ] {
            go(what, &m.testsrc, &|c| c.color = color, 0.4);
        }
        for (what, effects) in [
            ("grayscale", vec![E::Grayscale]),
            ("invert", vec![E::Invert]),
            ("vignette", vec![E::Vignette]),
            ("sharpen", vec![E::Sharpen { amount: 1.0 }]),
            ("grayscale and invert", vec![E::Grayscale, E::Invert]),
            ("vignette and a key", vec![E::Vignette, key("green", 0.3, 0.2)]),
        ] {
            go(what, &m.testsrc, &|c| c.effects = effects.clone(), 0.4);
        }
        // (A grade has its own test: `checker_e_a_graded_clip_under_a_blend`.)
        for speed in [0.25, 0.5, 2.0, 3.0, -1.0, -2.0] {
            go(&format!("speed {speed}"), &m.testsrc, &|c| c.speed = speed, 0.8);
        }
        go("a 25 fps source", &m.testsrc25, &|_| {}, 0.8);
        go("a late source", &m.late, &|_| {}, 0.6);
        go(
            "a source offset between frames",
            &m.testsrc,
            &|c| {
                c.source_in = 0.51;
                c.source_out = 1.51;
            },
            0.6,
        );
        let _ = n;
    }
}

/// Frames of sizes that are no multiple of a vector, a block or a thread count; the blended clip as large as the frame, moved by a few pixels.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_frames_of_other_sizes() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-sizes");
    std::fs::create_dir_all(&dir).unwrap();
    let sizes = [
        (642, 362),
        (322, 182),
        (194, 110),
        (130, 66),
        (98, 50),
        (66, 34),
        (34, 18),
        (18, 10),
        (16, 16),
        (10, 6),
        (6, 4),
        (2, 2),
        (320, 180),
        (1280, 720),
    ];
    for (n, (w, h)) in sizes.into_iter().enumerate() {
        let make = |name: &str, seed: u32| -> Asset {
            let p = dir.join(format!("{name}-{w}x{h}.mp4"));
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "nullsrc=size={w}x{h}:rate=30:duration=2,format=yuv420p,geq=lum='random({seed})*255':cb='random({})*255':cr='random({})*255',setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown",
                    seed + 1,
                    seed + 2
                ),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            probed(&p)
        };
        let (bottom, top) = (make("bottom", 1), make("top", 7));
        let smooth = {
            let p = dir.join(format!("smooth-{w}x{h}.mp4"));
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "gradients=size={w}x{h}:rate=30:duration=2:c0=0xd03020:c1=0x2040e0:nb_colors=2:seed=5:speed=0.00001,format=yuv420p,setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown"
                ),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            probed(&p)
        };
        for (k, mode) in BLEND_MODES.iter().enumerate() {
            if w * h > 100_000 && k % 3 != 0 {
                continue;
            }
            for (name, below, over) in [("noise over noise", &bottom, &top), ("smooth over noise", &bottom, &smooth)] {
                let mut c = clip(over, 0.2, 1.0, 0.0);
                c.blend_mode = *mode;
                ce_blend_run(
                    &format!("checker/e/{w}x{h} {mode:?} {name}"),
                    &timeline(vec![vec![clip(below, 0.2, 1.0, 0.0)], vec![c]], None),
                    &[below.clone(), over.clone()],
                    30.0,
                    0.3,
                    true,
                );
            }
        }
        // Moved by a few pixels, two ways: the chroma blocks the clip touches at its edges.
        for (dx, dy) in [(2i32, 2i32), (-2, 2), (4, 0), (0, -4)] {
            if w < 8 || h < 8 {
                continue;
            }
            let mut c = clip(&top, 0.2, 1.0, 0.0);
            c.blend_mode = BLEND_MODES[(n + dx.unsigned_abs() as usize) % 9];
            c.transform.pos_x = f64::from(dx) / w as f64;
            c.transform.pos_y = f64::from(dy) / h as f64;
            ce_blend_run(
                &format!("checker/e/{w}x{h} moved {dx},{dy}"),
                &timeline(vec![vec![clip(&bottom, 0.2, 1.0, 0.0)], vec![c]], None),
                &[bottom.clone(), top.clone()],
                30.0,
                0.3,
                dy >= 0,
            );
        }
    }
}

/// Fits: a clip that shares a delivery frame of its own shape is the plain fit; one that does not is refused (a letterbox, a cover that crops).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clip_in_a_delivery_frame_of_another_shape() {
    let m = media();
    for (name, w, h, fit) in [
        ("same shape, contain", 640, 360, Fit::Contain),
        ("same shape, cover", 640, 360, Fit::Cover),
        ("same shape, blur", 640, 360, Fit::Blur),
        ("9:16 contain", 360, 640, Fit::Contain),
        ("9:16 cover", 360, 640, Fit::Cover),
        ("9:16 blur", 360, 640, Fit::Blur),
        ("square cover", 360, 360, Fit::Cover),
    ] {
        let mut top = clip(&m.testsrc, 0.5, 1.5, 0.0);
        top.blend_mode = kerf_core::BlendMode::Multiply;
        let tl = timeline(
            vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![top]],
            Some(Delivery::new(w, h, fit)),
        );
        let assets = [m.bars.clone(), m.testsrc.clone()];
        // Whatever the plan lets through must be exact; what it refuses is FFmpeg's.
        let planner = Planner::new(&tl, &assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
        let plan = planner.at_frame(3).expect("plan");
        let size = (plan.canvas.width, plan.canvas.height);
        let why = plan.reasons(&compositor().caps(), size);
        let blend_refusals = why.iter().filter(|u| u.kind() == "blend_mode").count();
        let others = why.iter().filter(|u| u.kind() != "blend_mode").count();
        eprintln!("checker/e/fit {name}: {size:?}, {blend_refusals} blend refusals, {others} others: {why:?}");
        if why.is_empty() {
            ce_blend_run(&format!("checker/e/fit {name}"), &tl, &assets, 30.0, 0.4, true);
        }
    }
    // A cover that only crops (the frame shares a side with the picture), panned by a position so that the layer hangs over the frame's edges:
    // a layer larger than the canvas, unscaled.
    for (n, (w, h)) in [(480, 360), (318, 360), (640, 200), (640, 358), (360, 360)]
        .into_iter()
        .enumerate()
    {
        for (k, (px, py)) in [(0.0, 0.0), (0.1, 0.0), (-0.1, 0.0), (0.0, 0.1), (0.0, -0.1), (0.05, 0.03)]
            .into_iter()
            .enumerate()
        {
            let mut top = clip(&m.testsrc, 0.5, 1.5, 0.0);
            top.blend_mode = BLEND_MODES[(n + k) % 9];
            top.transform.pos_x = px;
            top.transform.pos_y = py;
            let tl = timeline(
                vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![top]],
                Some(Delivery::new(w, h, Fit::Cover)),
            );
            let assets = [m.bars.clone(), m.testsrc.clone()];
            ce_blend_run(
                &format!("checker/e/cover {w}x{h} panned {px},{py}"),
                &tl,
                &assets,
                30.0,
                0.4,
                py >= 0.0,
            );
        }
    }
}

// ---- checker/e: a scan that tells a refusal from a difference ----------------------------------------------------

/// What a scan of a cut's frames found: those the plan refuses (FFmpeg draws them), those the compositor drew and the samples that differ.
#[derive(Default)]
struct CeScan {
    frames: usize,
    drawn: usize,
    refused: BTreeMap<String, usize>,
    /// Frames the compositor itself would not draw (its own guards, past the plan's).
    guards: BTreeMap<String, usize>,
    y: PlaneStats,
    uv: PlaneStats,
    /// The first differing sample: (frame, plane, x, y).
    first: Option<(u64, usize, usize, usize)>,
    bad_frames: Vec<u64>,
}

impl CeScan {
    fn exact(&self) -> bool {
        self.y.differing == 0 && self.uv.differing == 0 && self.guards.is_empty()
    }

    fn line(&self, case: &str) -> String {
        format!(
            "{case:<60} frames {:>3} drawn {:>3} refused {:>3} {:?} guards {:?} | Y differ {:>7} max {:>2} | UV differ {:>7} max {:>2} | first {:?}",
            self.frames,
            self.drawn,
            self.refused.values().sum::<usize>(),
            self.refused
                .keys()
                .map(|k| k.chars().take(60).collect::<String>())
                .collect::<Vec<_>>(),
            self.guards
                .keys()
                .map(|k| k.chars().take(70).collect::<String>())
                .collect::<Vec<_>>(),
            self.y.differing,
            self.y.max,
            self.uv.differing,
            self.uv.max,
            self.first
        )
    }
}

fn ce_first_diff(a: &[u8], b: &[u8], w: usize) -> Option<(usize, usize)> {
    a.iter().zip(b).position(|(x, y)| x != y).map(|i| (i % w, i / w))
}

/// Plan every frame of the cut, draw those the plan lets through and compare them with the export's planes. With `anyway` the
/// plan's reasons are ignored and every frame is drawn (the compositor's own guards still stand, and are counted).
fn ce_scan(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, frames: Range<u64>, anyway: bool) -> CeScan {
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let played = export(&slug, tl, assets, opts);
    assert!(
        played.count() as u64 >= frames.end,
        "{case}: the export has {} frames, the case looks at {}",
        played.count(),
        frames.end
    );
    let planner = Planner::new(tl, assets, opts, PlanRequest::motion(policy())).expect("planner");
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let mut scan = CeScan::default();
    for k in frames {
        scan.frames += 1;
        let plan = planner.at_frame(k).expect("plan");
        assert_eq!((plan.canvas.width, plan.canvas.height), (played.w as u32, played.h as u32));
        let why = plan.reasons(&compositor().caps(), size);
        if !anyway && !why.is_empty() {
            for u in &why {
                *scan.refused.entry(u.to_string()).or_default() += 1;
            }
            continue;
        }
        let picture = match motion.frames(&plan) {
            Ok(p) => p,
            Err(e) => {
                *scan.guards.entry(format!("frames: {e}")).or_default() += 1;
                continue;
            }
        };
        let drawn = match compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size) {
            Ok(d) => d,
            Err(e) => {
                *scan.guards.entry(e.to_string()).or_default() += 1;
                continue;
            }
        };
        scan.drawn += 1;
        let want = played.planes(k as usize);
        let got = planes_of(&drawn);
        let (y, u, v) = (
            PlaneStats::of(want[0], got[0]),
            PlaneStats::of(want[1], got[1]),
            PlaneStats::of(want[2], got[2]),
        );
        if scan.first.is_none() {
            for (p, w) in [(0, played.w), (1, played.w / 2), (2, played.w / 2)] {
                if let Some((x, y)) = ce_first_diff(want[p], got[p], w) {
                    scan.first = Some((k, p, x, y));
                    break;
                }
            }
        }
        if y.differing + u.differing + v.differing > 0 {
            scan.bad_frames.push(k);
            if scan.bad_frames.len() <= 2 {
                montage(case, k, &played, &plan, &picture, motion.counts());
            }
        }
        scan.y.add(y);
        let mut uv = u;
        uv.add(v);
        scan.uv.add(uv);
    }
    scan
}

/// The frames the plan lets through are the export's, to the sample; the others are FFmpeg's. A case that nothing draws says so.
fn ce_exact_or_refused(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) -> CeScan {
    let scan = ce_scan(case, tl, assets, &at_fps(fps), upto(secs, fps), false);
    eprintln!("{}", scan.line(case));
    assert!(
        scan.exact(),
        "{case}: {} frames drawn, Y {} samples differ (max {}), UV {} (max {}), first {:?}, frames {:?}; guards {:?} (montages in target/parity/motion)",
        scan.drawn,
        scan.y.differing,
        scan.y.max,
        scan.uv.differing,
        scan.uv.max,
        scan.first,
        &scan.bad_frames[..scan.bad_frames.len().min(8)],
        scan.guards
    );
    scan
}

/// [`blend_run`] for a case whose frames the plan may refuse for a reason of its own (a picture it never draws): drawn exactly or refused.
fn ce_blend_run(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, forced_ok: bool) {
    ce_exact_or_refused(case, tl, assets, fps, secs);
    if compositor().caps().blend != kerf_core::BlendKinds::ALL && forced_ok {
        forced_blends(|| {
            ce_exact_or_refused(&format!("{case} (forced)"), tl, assets, fps, secs);
        });
    }
}

// ---- checker/e: the settle before a blur ---------------------------------------------------------------------------

/// FNV-1a of a drawn frame, to compare runs (a serial rasteriser's and a threaded one's) across processes.
fn ce_hash(planes: [&[u8]; 3]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in planes {
        for b in p {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// A blur reads what the passes before it drew: the same frame drawn again and again is the same frame, whatever pass came before the blur
/// (the slowest one in front is where a software rasteriser let the compute pass read half-drawn rows). The hash of each frame is printed so that
/// a run with `LP_NUM_THREADS=0` (a serial rasteriser, which has no race) can be held to the threaded one's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blur_after_other_passes_draws_the_same_frame_every_time() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let cases: Vec<(&str, f64, Vec<E>)> = vec![
        (
            "sharpen 0.5 then blur on a pip",
            0.6,
            vec![E::Sharpen { amount: 0.5 }, E::Blur { sigma: 3.0 }],
        ),
        (
            "sharpen 2.5 then blur",
            1.0,
            vec![E::Sharpen { amount: 2.5 }, E::Blur { sigma: 3.0 }],
        ),
        (
            "sharpen 1 then blur 8",
            1.0,
            vec![E::Sharpen { amount: 1.0 }, E::Blur { sigma: 8.0 }],
        ),
        ("blur on a pip", 0.6, vec![E::Blur { sigma: 3.0 }]),
        ("blur then blur", 1.0, vec![E::Blur { sigma: 2.0 }, E::Blur { sigma: 3.0 }]),
        (
            "blur then sharpen 0.5 on a pip",
            0.6,
            vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 0.5 }],
        ),
        (
            "invert then blur then grayscale",
            1.0,
            vec![E::Invert, E::Blur { sigma: 2.5 }, E::Grayscale],
        ),
        ("vignette then blur", 1.0, vec![E::Vignette, E::Blur { sigma: 2.5 }]),
        (
            "grayscale, sharpen, blur, invert",
            1.0,
            vec![E::Grayscale, E::Sharpen { amount: 1.0 }, E::Blur { sigma: 2.0 }, E::Invert],
        ),
        ("blur after a key", 1.0, vec![key("green", 0.2, 0.2), E::Blur { sigma: 3.0 }]),
    ];
    let mut unstable: Vec<String> = Vec::new();
    for (name, scale, effects) in cases {
        let (tl, assets) = over_bars(m, &|c| {
            c.transform.scale = scale;
            c.transform.pos_x = if scale < 1.0 { 0.15 } else { 0.0 };
            c.effects = effects.clone();
        });
        let opts = at_fps(30.0);
        let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(policy())).expect("planner");
        let plan = planner.at_frame(6).expect("plan");
        let size = (plan.canvas.width, plan.canvas.height);
        let why = plan.reasons(&compositor().caps(), size);
        if !why.is_empty() {
            eprintln!("checker/e/p0 {name}: refused by the plan ({why:?}), not drawn");
            continue;
        }
        let played = export(&format!("ce-p0-{}", name.replace(' ', "_")), &tl, &assets, &opts);
        let mut motion = MotionFrames::new(frame_source());
        let picture = motion.frames(&plan).expect("frames");
        let mut first: Option<Vec<u8>> = None;
        let mut varying = 0;
        for round in 0..12 {
            let drawn = compositor()
                .composite_yuv_counted(&plan, &picture, motion.counts(), size)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let bytes: Vec<u8> = planes_of(&drawn).concat();
            match &first {
                None => first = Some(bytes),
                Some(f) => {
                    if *f != bytes {
                        varying += 1;
                        eprintln!("checker/e/p0 {name}: draw {round} is not the first draw");
                    }
                }
            }
        }
        let last = compositor()
            .composite_yuv_counted(&plan, &picture, motion.counts(), size)
            .unwrap();
        let got = planes_of(&last);
        let want = played.planes(6);
        let (y, u, v) = (
            PlaneStats::of(want[0], got[0]),
            PlaneStats::of(want[1], got[1]),
            PlaneStats::of(want[2], got[2]),
        );
        eprintln!(
            "checker/e/p0 {name:<36} hash {:016x} | vs FFmpeg Y differ {:>6} max {} UV differ {:>6} max {} | draws unlike the first: {varying}",
            ce_hash(got),
            y.differing,
            y.max,
            u.differing + v.differing,
            u.max.max(v.max)
        );
        if varying > 0 {
            unstable.push(format!("{name}: {varying} of 12 draws unlike the first"));
        }
    }
    assert!(
        unstable.is_empty(),
        "the same plan drawn twelve times gave different frames: {unstable:?}"
    );
}

// ---- checker/e: a refusal is necessary only if drawing the frame anyway is not exact -----------------------------------------

/// A clip tagged `tag` as its colour matrix (the test pattern, lossless).
fn ce_tagged(name: &str, tag: &str, pattern: &str) -> Asset {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-tagged");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}-{tag}.mp4"));
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!(
            "{pattern}=size=640x360:rate=30:duration=4,format=yuv420p,setparams=colorspace={tag}:color_primaries=bt709:color_trc=bt709"
        ),
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-qp",
        "0",
        "-g",
        "10",
        "-colorspace",
        tag,
        "-pix_fmt",
        "yuv420p",
        p.to_str().unwrap(),
    ]);
    probed(&p)
}

/// The refusals the first pass found wider than they had to be, now lifted (`checker_e_each_refused_blend_is_refused_for_a_reason` holds the ones
/// that stay): a quarter turn, an RGB picture and a BT.2020 composite. Each must be **drawn**, every frame and to the sample, where the caps
/// say this FFmpeg draws it (the quarter turn with the blend modes, RGB and BT.2020 with `caps.blend_extras`), and refused on every frame where
/// they do not.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_the_lifted_blend_refusals_draw_exactly() {
    use kerf_core::{BlendExtras, BlendMode};
    let m = media();
    let v = variants();
    let caps = compositor().caps();
    let modes_on = caps.blend == kerf_core::BlendKinds::ALL;
    let rgb_on = modes_on && caps.blend_extras.contains(BlendExtras::RGB_PICTURE);
    let bt2020_on = modes_on && caps.blend_extras.contains(BlendExtras::BT2020);
    eprintln!("checker/e/lifted: modes {modes_on}, RGB pictures {rgb_on}, BT.2020 {bt2020_on}");
    let rgb_noise = ce_rgb_noise();
    let mut failures: Vec<String> = Vec::new();
    for mode in [BlendMode::Multiply, BlendMode::Screen, BlendMode::Difference] {
        let mut add = |name: &str, bottom: &Asset, top: &Asset, draws: bool, edit: &dyn Fn(&mut Clip)| {
            let (tl, assets) = ce_stack(bottom, top, &|c| {
                c.blend_mode = mode;
                edit(c);
            });
            ce_check(
                &format!("checker/e/lifted {name} {mode:?}"),
                &tl,
                &assets,
                30.0,
                0.4,
                false,
                Some(draws),
                &mut failures,
            );
        };
        add("rotated 90", &m.bars, &m.smooth, modes_on, &|c| c.transform.rotation = 90.0);
        add("rotated 90 noise", &m.bars, &m.noise, modes_on, &|c| {
            c.transform.rotation = 90.0;
        });
        add("rotated 180 smooth", &m.bars, &m.smooth, modes_on, &|c| {
            c.transform.rotation = 180.0;
        });
        add("rotated 270 noise", &m.bars, &m.noise, modes_on, &|c| {
            c.transform.rotation = 270.0;
        });
        add("rotated 90 moved", &m.bars, &m.noise, modes_on, &|c| {
            c.transform.rotation = 90.0;
            c.transform.pos_x = 0.2;
        });
        add("rgb picture", &m.bars, &v.rgb_png, rgb_on, &|_| {});
        add("rgb noise picture", &m.bars, &rgb_noise, rgb_on, &|_| {});
        // (The picture is the composite here, a plain layer: only the modes have to be in the caps.)
        add("rgb picture, bars blended over it", &v.rgb_png, &m.bars, modes_on, &|_| {});
        let bottom = ce_tagged("bars", "bt2020nc", "smptehdbars");
        let top = ce_tagged("testsrc", "bt2020nc", "testsrc2");
        add("bt2020nc footage", &bottom, &top, bt2020_on, &|_| {});
        // The same three with an alpha of their own (a fade and a feathered mask give every level, where the cases above are opaque).
        let alpha = |c: &mut Clip| {
            c.fade_in = 0.3;
            c.fade_out = 0.2;
            c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.6, false));
        };
        // (A mask on a turned layer is refused for a reason of its own, which `checker_e_a_blended_clip_turned_by_quarter_turns` holds.)
        add("rotated 270 faded", &m.bars, &m.noise, modes_on, &|c| {
            c.transform.rotation = 270.0;
            c.fade_in = 0.3;
            c.fade_out = 0.2;
        });
        add("rgb noise picture faded and masked", &m.bars, &rgb_noise, rgb_on, &alpha);
        add("bt2020nc footage faded and masked", &bottom, &top, bt2020_on, &alpha);
    }
    ce_expect_drawn("checker_e_the_lifted_blend_refusals_draw_exactly", &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// Every refusal the blend adds or keeps, drawn anyway (the plan's reasons ignored, so with the compositor's own guards lifted by a scratch
/// patch): a refusal that is not needed would draw exactly. Without the patch the compositor's guards stand and the case says so. The
/// cases whose frames are exact when drawn are the refusals wider than they have to be.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_each_refused_blend_is_refused_for_a_reason() {
    use kerf_core::BlendMode;
    let m = media();
    let mut cases: Vec<(String, Timeline, Vec<Asset>)> = Vec::new();
    for mode in [BlendMode::Multiply, BlendMode::Screen, BlendMode::Difference] {
        let mut add = |name: &str, bottom: &Asset, top: &Asset, edit: &dyn Fn(&mut Clip)| {
            let (tl, assets) = ce_stack(bottom, top, &|c| {
                c.blend_mode = mode;
                edit(c);
            });
            cases.push((format!("{name} {mode:?}"), tl, assets));
        };
        add("scaled down 0.6", &m.bars, &m.testsrc, &|c| c.transform.scale = 0.6);
        add("scaled down 0.5 moved", &m.bars, &m.noise, &|c| {
            c.transform.scale = 0.5;
            c.transform.pos_x = 0.25;
            c.transform.pos_y = 0.25;
        });
        add("scaled up 1.25", &m.bars, &m.testsrc, &|c| c.transform.scale = 1.25);
        add("scaled smooth", &m.bars, &m.smooth, &|c| c.transform.scale = 0.7);
        add("rotated 12", &m.bars, &m.smooth, &|c| c.transform.rotation = 12.0);
        add("opacity 0.6", &m.bars, &m.testsrc, &|c| c.transform.opacity = 0.6);
        add("opacity 0.97", &m.bars, &m.noise, &|c| c.transform.opacity = 0.97);
        add("opacity 0.6 flat", &m.bars, &m.black, &|c| c.transform.opacity = 0.6);
        add("letterboxed portrait", &m.bars, &m.portrait, &|_| {});
        // (A rotation that is not a quarter turn, one a key moves, and a grade, which stay refused; the quarter turns, RGB pictures and BT.2020 are
        // lifted and held to FFmpeg in `checker_e_the_lifted_blend_refusals_draw_exactly`.)
        add("rotated 91", &m.bars, &m.noise, &|c| c.transform.rotation = 91.0);
        add("rotated by a key through 90", &m.bars, &m.noise, &|c| {
            c.set_property_keys(
                kerf_core::Property::Rotation,
                vec![kerf_core::PropertyKey::new(0.0, 0.0), kerf_core::PropertyKey::new(1.0, 90.0)],
            );
        });
        add("graded", &m.bars, &m.testsrc, &|c| {
            c.grade = grades().remove(0).1;
        });
        add("graded noise", &m.bars, &m.noise, &|c| {
            c.grade = grades().remove(3).1;
        });
    }
    for tag in ["fcc", "smpte240m"] {
        let bottom = ce_tagged("bars", tag, "smptehdbars");
        let top = ce_tagged("testsrc", tag, "testsrc2");
        for mode in [BlendMode::Multiply, BlendMode::Difference] {
            let (tl, assets) = ce_stack(&bottom, &top, &|c| c.blend_mode = mode);
            cases.push((format!("matrix {tag} {mode:?}"), tl, assets));
        }
        // The same matrix on the top clip only, over untagged footage (the bottom layer names it: untagged).
        let (tl, assets) = ce_stack(&m.bars, &top, &|c| c.blend_mode = BlendMode::Multiply);
        cases.push((format!("matrix {tag} on top of untagged"), tl, assets));
        let (tl, assets) = ce_stack(&bottom, &m.testsrc, &|c| c.blend_mode = BlendMode::Multiply);
        cases.push((format!("untagged on top of {tag}"), tl, assets));
    }
    let mut necessary = Vec::new();
    let mut undrawn = Vec::new();
    let mut degenerate = Vec::new();
    let mut wider = Vec::new();
    for (name, tl, assets) in &cases {
        let case = format!("checker/e/anyway {name}");
        let planner = Planner::new(tl, assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
        let plan = planner.at_frame(3).expect("plan");
        let size = (plan.canvas.width, plan.canvas.height);
        let blend_why: Vec<String> = plan
            .reasons(&Compositor::CAPS, size)
            .iter()
            .filter(|u| u.kind() == "blend_mode")
            .map(ToString::to_string)
            .collect();
        let scan = ce_scan(&case, tl, assets, &at_fps(30.0), 0..12, true);
        eprintln!(
            "{}\n    plan: {:?}",
            scan.line(&case),
            blend_why
                .iter()
                .map(|w| w.split("): ").last().unwrap_or("").to_string())
                .collect::<Vec<_>>()
        );
        if blend_why.is_empty() {
            eprintln!("    (the plan does not refuse this one with every mode in the caps)");
            continue;
        }
        if scan.drawn == 0 {
            // Another refusal of the plan, or the compositor's own guard, stops the draw (a zoom under Contain, a matrix FFmpeg 9 cannot be asked about):
            // nothing was shown either way.
            undrawn.push(name.clone());
        } else if scan.exact() && name.contains("flat") {
            // A source with no texture and no chroma has nothing for the round trip to get wrong; the rule cannot know the content.
            degenerate.push(name.clone());
        } else if scan.exact() {
            wider.push(name.clone());
        } else {
            necessary.push(name.clone());
        }
    }
    eprintln!("refusals confirmed necessary ({}): {necessary:?}", necessary.len());
    eprintln!(
        "refusals not drawn at all, so not shown either way ({}): {undrawn:?}",
        undrawn.len()
    );
    eprintln!(
        "refusals that draw exactly only for a flat source ({}): {degenerate:?}",
        degenerate.len()
    );
    eprintln!("refusals that drew EXACTLY anyway ({}): {wider:?}", wider.len());
    assert!(wider.is_empty(), "refusals wider than they have to be: {wider:?}");
}

/// A clip edit, to be applied to a blended clip in a case.
type ClipEdit = Box<dyn Fn(&mut Clip)>;

/// A graded clip under a blend, held beside the same clip plain and with a keyed alpha: the grade's RGB round trip ends in a conversion to
/// `yuva420p` when the clip needs an alpha plane (a blended clip always does: its bars are transparent), where a plain clip's ends in `yuv420p`,
/// and FFmpeg's RGB filters run `yuva420p -> argb -> yuva420p` there, which the grade's model is not. Plain and keyed-alpha clips are drawn exactly;
/// under a blend every frame is refused by the plan (the cause is named), and none is drawn inexactly.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_graded_clip_under_a_blend() {
    use kerf_core::{BlendMode, Property, PropertyKey};
    let m = media();
    let mut findings = Vec::new();
    for (gname, grade) in grades() {
        for (sname, asset) in [("testsrc", &m.testsrc), ("noise", &m.noise), ("bars", &m.bars)] {
            let variants: [(&str, ClipEdit); 4] = [
                ("plain", Box::new(|_| {})),
                (
                    "keyed alpha",
                    Box::new(|c| {
                        c.set_property_keys(
                            Property::Opacity,
                            vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 0.5)],
                        );
                    }),
                ),
                ("multiply", Box::new(|c| c.blend_mode = BlendMode::Multiply)),
                ("difference", Box::new(|c| c.blend_mode = BlendMode::Difference)),
            ];
            for (vname, edit) in variants {
                let (tl, assets) = ce_stack(&m.bars, asset, &|c| {
                    c.grade = grade.clone();
                    edit(c);
                });
                let case = format!("checker/e/graded {gname} {sname} {vname}");
                let scan = ce_scan(&case, &tl, &assets, &at_fps(30.0), 0..6, false);
                eprintln!("{}", scan.line(&case));
                let blended = vname == "multiply" || vname == "difference";
                if !scan.exact() {
                    findings.push(format!("{case}: drawn {} and not exact", scan.drawn));
                } else if blended {
                    let named = scan.refused.keys().any(|k| k.contains("a grade under a blend"));
                    if scan.drawn > 0 || !named {
                        findings.push(format!(
                            "{case}: drawn {} frames, refused {:?}",
                            scan.drawn,
                            scan.refused.keys()
                        ));
                    }
                } else if scan.drawn == 0 {
                    findings.push(format!("{case}: nothing drawn, refused {:?}", scan.refused.keys()));
                }
            }
        }
    }
    assert!(findings.is_empty(), "graded clips: {findings:?}");
}

/// Where a graded blended clip goes wrong: FFmpeg's own plain graded clip (laid by hand in the legal range, as `lutyuv=a=255` does) put through the model
/// is compared with FFmpeg's blended export. If they agree, the chain is the model's and it is the compositor's laid clip that is not FFmpeg's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_graded_blend_is_the_model_of_the_plain_graded_clip() {
    use kerf_core::{BlendMode, YuvMatrix};
    use kerf_gpu::blend::{reference, Planes};
    let m = media();
    let grades = grades();
    let (gname, grade) = grades.first().cloned().expect("a grade");
    for (sname, asset) in [("testsrc", &m.testsrc), ("noise", &m.noise)] {
        let grab = |slug: &str, edit: &dyn Fn(&mut Clip), top: bool| {
            let (tl, assets) = if top {
                ce_stack(&m.bars, asset, &|c| {
                    c.grade = grade.clone();
                    edit(c);
                })
            } else {
                (timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)]], None), vec![m.bars.clone()])
            };
            export(&format!("ce-diag-{slug}-{sname}"), &tl, &assets, &at_fps(30.0))
        };
        let plain = grab("plain", &|_| {}, true);
        let bottom = grab("bottom", &|_| {}, false);
        let blended = grab("blend", &|c| c.blend_mode = BlendMode::Multiply, true);
        let (w, h) = (plain.w, plain.h);
        for k in [0usize, 5] {
            let planes = |p: &Played| {
                let [y, u, v] = p.planes(k);
                Planes {
                    w,
                    h,
                    y: y.to_vec(),
                    u: u.to_vec(),
                    v: v.to_vec(),
                }
            };
            let (mut laid, base, got) = (planes(&plain), planes(&bottom), planes(&blended));
            // `lutyuv=a=255`'s `clipval`: luma 16..235, chroma 16..240.
            laid.y.iter_mut().for_each(|v| *v = (*v).clamp(16, 235));
            laid.u.iter_mut().for_each(|v| *v = (*v).clamp(16, 240));
            laid.v.iter_mut().for_each(|v| *v = (*v).clamp(16, 240));
            let want = reference(BlendMode::Multiply, YuvMatrix::Bt601, &base, &laid, &vec![255u8; w * h]);
            let stats = |a: &[u8], b: &[u8]| PlaneStats::of(a, b);
            let (y, u, v) = (stats(&want.y, &got.y), stats(&want.u, &got.u), stats(&want.v, &got.v));
            eprintln!(
                "checker/e/diag {gname} {sname} frame {k}: model(bottom, plain graded clip) vs FFmpeg's blend: Y differ {} max {} | UV differ {} max {}",
                y.differing,
                y.max,
                u.differing + v.differing,
                u.max.max(v.max)
            );
            let below = |p: &Planes| p.y.iter().filter(|v| **v < 16 || **v > 235).count();
            let raw = planes(&plain);
            eprintln!("    plain graded clip: {} luma samples outside 16..235", below(&raw));
        }
    }
}

/// The matrix the chain names is the bottom layer's of what is rendered: a hidden (muted) track, a track that is not the lowest one soloed, and a
/// disabled clip do not take part, in the export or in the plan.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_hidden_or_disabled_bottom_clip_does_not_name_the_matrix() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    for mode in [BlendMode::Difference, BlendMode::Multiply] {
        // A muted track of BT.709 footage under the cut: untagged footage is the bottom layer that is rendered.
        let mut blended = clip(&m.testsrc, 0.5, 1.5, 0.0);
        blended.blend_mode = mode;
        let mut tl = timeline(
            vec![
                vec![clip(&v.bars709, 0.5, 1.5, 0.0)],
                vec![clip(&m.bars, 0.5, 1.5, 0.0)],
                vec![blended.clone()],
            ],
            None,
        );
        tl.tracks[0].muted = true;
        ce_blend_run(
            &format!("checker/e/{mode:?} under a muted BT.709 track"),
            &tl,
            &[v.bars709.clone(), m.bars.clone(), m.testsrc.clone()],
            30.0,
            0.4,
            true,
        );
        // The same with the BT.709 clip disabled, on the lowest track.
        let mut disabled = clip(&v.bars709, 0.5, 1.5, 0.0);
        disabled.enabled = false;
        let tl = timeline(
            vec![vec![disabled], vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![blended.clone()]],
            None,
        );
        ce_blend_run(
            &format!("checker/e/{mode:?} under a disabled BT.709 clip"),
            &tl,
            &[v.bars709.clone(), m.bars.clone(), m.testsrc.clone()],
            30.0,
            0.4,
            true,
        );
        // The BT.709 footage is on the lowest track but starts later than the untagged clip of the same track.
        let tl = timeline(
            vec![
                vec![clip(&v.bars709, 0.5, 1.5, 0.5), clip(&m.bars, 0.5, 1.0, 0.0)],
                vec![blended.clone()],
            ],
            None,
        );
        ce_blend_run(
            &format!("checker/e/{mode:?} over a track whose first picture is untagged but listed second"),
            &tl,
            &[v.bars709.clone(), m.bars.clone(), m.testsrc.clone()],
            30.0,
            1.0,
            true,
        );
    }
}

/// The blended clip's window on cuts at other frame rates, a clip starting and ending between two frames of each.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clip_window_at_other_frame_rates() {
    let m = media();
    for (i, fps) in [24.0, 25.0, 60.0, 23.976, 29.97, 12.0].into_iter().enumerate() {
        for (j, (start, len)) in [(0.1, 0.7), (0.0333, 0.5), (0.2, 0.2501), (0.0, 1.0)].into_iter().enumerate() {
            let mode = BLEND_MODES[(i + j) % 9];
            let mut top = clip(&m.testsrc, 0.5, 0.5 + len, start);
            top.blend_mode = mode;
            top.fade_in = 0.05;
            ce_blend_run(
                &format!("checker/e/{fps} fps {mode:?} from {start} for {len}"),
                &timeline(vec![vec![clip(&m.bars, 0.0, 1.5, 0.0)], vec![top]], None),
                &[m.bars.clone(), m.testsrc.clone()],
                fps,
                1.2,
                true,
            );
        }
    }
}

/// A 640x360 PNG of per-sample RGB noise (a still image in `rgb24`): every R, G, B triple.
fn ce_rgb_noise() -> Asset {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-rgb");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("noise.png");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=size=640x360:rate=1:duration=1,format=gbrp,geq=r='random(1)*255':g='random(2)*255':b='random(3)*255'",
        "-frames:v",
        "1",
        "-pix_fmt",
        "rgb24",
        p.to_str().unwrap(),
    ]);
    probed(&p)
}

/// A dissolve into a blended clip that is already fading out: the two alpha ramps are rounded one after the other, and the chain applies the
/// fade-out first and the dissolve's after it (`ClipTiming::fades`: fade in, fade out, dips, dissolve), where the ramps' order is part of the
/// answer to the level. (With a feathered mask as well its alpha plane has every level.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_dissolve_that_overlaps_the_fade_out_of_a_blended_clip() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures: Vec<String> = Vec::new();
    for (fade_out, dissolve, masked) in [
        (0.6, 0.6, false),
        (0.3, 0.8, false),
        (0.9, 0.2, false),
        (0.6, 0.6, true),
        (0.9, 0.3, true),
        (0.3, 0.9, true),
    ] {
        let out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 0.7);
        inc.blend_mode = BlendMode::Screen;
        inc.fade_out = fade_out;
        if masked {
            inc.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 1.0, false));
        }
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: dissolve,
        });
        let case = format!(
            "checker/e/dissolve {dissolve}s with a fade-out of {fade_out}s{}",
            if masked { " and a feathered mask" } else { "" }
        );
        let tl = timeline(vec![vec![out, inc]], None);
        let assets = [m.bars.clone(), m.testsrc.clone()];
        let scan = ce_scan(&case, &tl, &assets, &at_fps(30.0), upto(1.65, 30.0), false);
        eprintln!("{}", scan.line(&case));
        if !scan.exact() {
            failures.push(format!(
                "{case}: Y {} differ (max {}), UV {} (max {}), first {:?}, frames {:?}",
                scan.y.differing,
                scan.y.max,
                scan.uv.differing,
                scan.uv.max,
                scan.first,
                &scan.bad_frames[..scan.bad_frames.len().min(8)]
            ));
        }
    }
    // The same with a dip to white (an alpha ramp on a blended clip, last in the chain's order like the dissolve's), the outgoing clip blended
    // too so that no plain clip's dip to white (not exact at the base commit) is in the frame.
    for (fade_out, dip, masked) in [(0.6, 0.6, false), (0.9, 0.3, true)] {
        let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
        out.blend_mode = BlendMode::Multiply;
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 0.7);
        inc.blend_mode = BlendMode::Screen;
        inc.fade_out = fade_out;
        if masked {
            inc.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 1.0, false));
        }
        inc.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: dip,
        });
        let case = format!(
            "checker/e/dip to white {dip}s with a fade-out of {fade_out}s{}",
            if masked { " and a feathered mask" } else { "" }
        );
        let tl = timeline(vec![vec![clip(&m.smooth, 0.5, 2.5, 0.0)], vec![out, inc]], None);
        let assets = [m.smooth.clone(), m.bars.clone(), m.testsrc.clone()];
        let scan = ce_scan(&case, &tl, &assets, &at_fps(30.0), upto(1.65, 30.0), false);
        eprintln!("{}", scan.line(&case));
        if !scan.exact() {
            failures.push(format!(
                "{case}: Y {} differ (max {}), UV {} (max {}), first {:?}, frames {:?}",
                scan.y.differing,
                scan.y.max,
                scan.uv.differing,
                scan.uv.max,
                scan.first,
                &scan.bad_frames[..scan.bad_frames.len().min(8)]
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// A quarter turn lands on whole samples (the `rotate` filter's fixed-point sample is exact there): a blended clip turned by 90, 180 or 270
/// degrees is drawn exactly, moved and not, over the bars and over noise.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blended_clip_turned_a_quarter_is_laid_where_it_lies() {
    use kerf_core::BlendMode;
    let m = media();
    for mode in [BlendMode::Multiply, BlendMode::Overlay, BlendMode::Difference] {
        for turn in [90.0, 180.0, 270.0] {
            for (name, asset, x) in [
                ("testsrc", &m.testsrc, 0.0),
                ("noise", &m.noise, 0.0),
                ("testsrc moved", &m.testsrc, 0.2),
            ] {
                let mut c = clip(asset, 0.5, 1.5, 0.0);
                c.blend_mode = mode;
                c.transform.rotation = turn;
                c.transform.pos_x = x;
                blend_run(
                    &format!("blend/{mode:?} {name} turned {turn}"),
                    &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
                    &[m.bars.clone(), asset.clone()],
                    30.0,
                    0.4,
                    // (A quarter turn upright puts the clip's box above the frame's top edge, the 4.4 overlay bug the probe found.)
                    turn == 180.0,
                );
            }
        }
    }
}

/// A BT.2020 composite is drawn where the probe matched it in every mode (`BlendExtras::BT2020`), refused where it did not.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blend_on_bt2020_footage_is_drawn_where_the_probe_matched() {
    use kerf_core::BlendMode;
    let bottom = ce_tagged("bars", "bt2020nc", "smptehdbars");
    let top = ce_tagged("testsrc", "bt2020nc", "testsrc2");
    for mode in [
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Difference,
    ] {
        let (tl, assets) = ce_stack(&bottom, &top, &|c| c.blend_mode = mode);
        let case = format!("blend/{mode:?} on bt2020 footage");
        if compositor().caps().blend_extras.contains(kerf_core::BlendExtras::BT2020) {
            blend_run(&case, &tl, &assets, 30.0, 0.5, true);
        } else {
            refused(&case, &tl, &assets, &at_fps(30.0), upto(0.5, 30.0), "BT.2020");
        }
    }
}

/// A blended clip's fades are alpha fades run in the order the chain has them (the clip's own, a dip's halves, then a dissolve's ramp):
/// a fade-in beside a crossfade on one edge is two steps and a dissolve that overlaps the fade-out ramps after it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blended_clips_fades_run_in_the_chains_order() {
    use kerf_core::BlendMode;
    let m = media();
    for mode in [BlendMode::Multiply, BlendMode::Screen] {
        let assets = [m.bars.clone(), m.testsrc.clone()];
        // A fade-in and a crossfade into the same clip.
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
        inc.blend_mode = mode;
        inc.fade_in = 0.4;
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.6,
        });
        blend_run(
            &format!("blend/{mode:?} fade-in and crossfade on one edge"),
            &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), inc]], None),
            &assets,
            30.0,
            2.0,
            true,
        );
        // A dissolve into a clip whose fade-out is already running.
        for (fade_out, dissolve) in [(0.6, 0.6), (0.9, 0.3)] {
            let mut inc = clip(&m.testsrc, 0.5, 1.5, 0.7);
            inc.blend_mode = mode;
            inc.fade_out = fade_out;
            inc.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: dissolve,
            });
            blend_run(
                &format!("blend/{mode:?} dissolve {dissolve}s with a fade-out of {fade_out}s"),
                &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), inc]], None),
                &assets,
                30.0,
                1.65,
                true,
            );
        }
    }
}

// ==== checker/e, second pass: the fades' order, quarter turns, RGB pictures and BT.2020 =====================================

impl CeScan {
    /// What a case wanted of its scan, or `None` when it holds. `draws`: `Some(true)` every frame drawn and exact, `Some(false)` none drawn
    /// (the plan refuses them all), `None` exact wherever drawn.
    fn verdict(&self, draws: Option<bool>) -> Option<String> {
        if !self.exact() {
            return Some(format!(
                "drawn {} of {}: Y {} differ (max {}), UV {} (max {}), first {:?}, frames {:?}, guards {:?}",
                self.drawn,
                self.frames,
                self.y.differing,
                self.y.max,
                self.uv.differing,
                self.uv.max,
                self.first,
                &self.bad_frames[..self.bad_frames.len().min(8)],
                self.guards
            ));
        }
        match draws {
            Some(true) if self.drawn != self.frames => Some(format!(
                "drawn {} of {} frames, refused {:?}",
                self.drawn,
                self.frames,
                self.refused.keys().collect::<Vec<_>>()
            )),
            Some(false) if self.drawn != 0 => Some(format!("drawn {} frames of a case that should be refused", self.drawn)),
            _ => None,
        }
    }
}

/// [`ce_scan`] of a cut, the verdict kept (a case never stops the test) and, where this FFmpeg's blend chain was refused (4.4), the same cut
/// with the caps forced (`forced_ok`) held to the export too.
#[allow(clippy::too_many_arguments)]
fn ce_check(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    fps: f64,
    secs: f64,
    forced_ok: bool,
    draws: Option<bool>,
    failures: &mut Vec<String>,
) -> CeScan {
    let scan = ce_scan(case, tl, assets, &at_fps(fps), upto(secs, fps), false);
    eprintln!("{}", scan.line(case));
    CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
    if let Some(why) = scan.verdict(draws) {
        failures.push(format!("{case}: {why}"));
    }
    if forced_ok && compositor().caps().blend != kerf_core::BlendKinds::ALL {
        forced_blends(|| {
            let name = format!("{case} (forced)");
            let forced = ce_scan(&name, tl, assets, &at_fps(fps), upto(secs, fps), false);
            eprintln!("{}", forced.line(&name));
            CE_DRAWN.with(|d| d.set(d.get() + forced.drawn));
            if let Some(why) = forced.verdict(None) {
                failures.push(format!("{name}: {why}"));
            }
        });
    }
    scan
}

thread_local! {
    /// The frames the test on this thread has had drawn through [`ce_check`].
    static CE_DRAWN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A test whose cases all passed because the plan refused every frame has checked nothing: it must have had frames drawn.
fn ce_expect_drawn(what: &str, failures: &mut Vec<String>) {
    let drawn = CE_DRAWN.with(std::cell::Cell::get);
    eprintln!("{what}: {drawn} frames drawn and held to the export");
    if drawn == 0 {
        failures.push(format!(
            "{what}: no frame was drawn, the plan refused them all (nothing was checked)"
        ));
    }
}

/// The blended clips of a chain through the fades the chain stacks on one clip: its own fade-in and fade-out (a dip to black adds to them), a
/// dip to white's halves and a dissolve's ramp. They are alpha fades run one after another, each rounding for itself, so the order is part of the
/// answer: this holds the compositor to it with up to three running on a frame (`checker_e_three_and_four_ramps_run_on_one_frame` has the busiest), 1-frame and half-frame ramps, ramps that begin and end on
/// a frame, and a feathered mask that gives the alpha plane every level.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clips_fades_stack_in_the_chains_order() {
    use kerf_core::BlendMode;
    let m = media();
    let kinds = [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::SlideLeft,
        TransitionKind::PushRight,
    ];
    let frame = 1.0 / 30.0;
    // (fade-in, fade-out, the transition into the clip, the transition out of it), in seconds.
    let timings: [(f64, f64, f64, f64); 7] = [
        (0.5, 0.5, 0.8, 0.8),
        (frame, frame, frame, frame),
        (frame / 2.0, frame / 2.0, frame / 2.0, frame / 2.0),
        (2.0 * frame, 2.0 * frame, 2.0 * frame, 2.0 * frame),
        (10.0 * frame, 10.0 * frame, 10.0 * frame, 10.0 * frame),
        (1.0, 1.0, 0.5, 0.5),
        (0.7, 0.7, 0.45, 0.45),
    ];
    let modes = [
        BlendMode::Screen,
        BlendMode::Multiply,
        BlendMode::Difference,
        BlendMode::Overlay,
        BlendMode::Exclusion,
    ];
    let mut failures: Vec<String> = Vec::new();
    // Three blended clips in a row over the bars: the middle one has a fade at each end and a transition at each end.
    let cut =
        |kind_in: TransitionKind, kind_out: TransitionKind, (fi, fo, din, dout): (f64, f64, f64, f64), mask: bool, n: usize| {
            let mut first = clip(&m.noise, 0.5, 1.0, 0.0);
            first.blend_mode = modes[n % 5];
            let mut mid = clip(&m.testsrc, 0.5, 1.5, 0.5);
            mid.blend_mode = modes[(n + 1) % 5];
            mid.fade_in = fi;
            mid.fade_out = fo;
            mid.transition_in = Some(Transition {
                kind: kind_in,
                duration: din,
            });
            if mask {
                mid.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 1.0, false));
            }
            let mut last = clip(&m.edges, 0.5, 1.2, 1.5);
            last.blend_mode = modes[(n + 2) % 5];
            last.transition_in = Some(Transition {
                kind: kind_out,
                duration: dout,
            });
            (
                timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![first, mid, last]], None),
                vec![m.bars.clone(), m.noise.clone(), m.testsrc.clone(), m.edges.clone()],
            )
        };
    let mut n = 0;
    for kin in kinds {
        for kout in kinds {
            n += 1;
            // (Every third pair: the busiest cuts are `checker_e_three_and_four_ramps_run_on_one_frame`'s.)
            if n % 3 != 1 {
                continue;
            }
            let timing = timings[n % timings.len()];
            let (tl, assets) = cut(kin, kout, timing, n % 3 == 1, n);
            let up = matches!(kin, TransitionKind::SlideDown | TransitionKind::PushDown)
                || matches!(kout, TransitionKind::SlideDown | TransitionKind::PushDown);
            ce_check(
                &format!("checker/e/ramps in {kin:?} out {kout:?} {timing:.3?}"),
                &tl,
                &assets,
                30.0,
                2.2,
                !up,
                None,
                &mut failures,
            );
        }
    }
    // The slides and pushes the first loop leaves out, in and out of the clip, with its fades.
    for (i, kind) in [
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ]
    .into_iter()
    .enumerate()
    {
        let (other, timing) = if i % 2 == 0 {
            (TransitionKind::DipToWhite, timings[0])
        } else {
            (TransitionKind::Crossfade, timings[6])
        };
        let up = matches!(kind, TransitionKind::SlideDown | TransitionKind::PushDown);
        let (kin, kout) = if i % 2 == 0 { (kind, other) } else { (other, kind) };
        let (tl, assets) = cut(kin, kout, timing, i % 2 == 1, i + 3);
        ce_check(
            &format!("checker/e/ramps in {kin:?} out {kout:?} {timing:.3?}"),
            &tl,
            &assets,
            30.0,
            2.2,
            !up,
            None,
            &mut failures,
        );
    }
    // Every timing against a dissolve into the clip and a dip to white out of it, and the other way round, with and without the mask.
    for (i, timing) in timings.into_iter().enumerate().filter(|(i, _)| i % 2 == 0) {
        for (kin, kout) in [
            (TransitionKind::Crossfade, TransitionKind::DipToWhite),
            (TransitionKind::DipToWhite, TransitionKind::Crossfade),
        ] {
            let (tl, assets) = cut(kin, kout, timing, i % 2 == 0, i + 7);
            ce_check(
                &format!("checker/e/ramps {kin:?} / {kout:?} {timing:.3?} masked {}", i % 2 == 0),
                &tl,
                &assets,
                30.0,
                2.2,
                true,
                None,
                &mut failures,
            );
        }
    }
    // A clip that starts, and whose fades start and end, on whole frames and between them.
    for (start, fi, fo) in [
        (5.0 * frame, 6.0 * frame, 6.0 * frame),
        (5.4 * frame, 6.5 * frame, 6.5 * frame),
        (0.0, 1.0, 1.0),
        (frame, frame, frame),
    ] {
        let mut top = clip(&m.testsrc, 0.5, 1.5, start);
        top.blend_mode = BlendMode::Screen;
        top.fade_in = fi;
        top.fade_out = fo;
        top.mask = Some(mask_of(kerf_core::MaskShape::Rect, 1.0, false));
        ce_check(
            &format!("checker/e/fades {fi:.4}/{fo:.4} from {start:.4}"),
            &timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None),
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            1.9,
            true,
            None,
            &mut failures,
        );
    }
    ce_expect_drawn("checker_e_a_blended_clips_fades_stack_in_the_chains_order", &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// A blended clip can run as many alpha fades as its chain has (`ClipTiming::fades` makes at most five steps and a cut reaches four); a plan with more
/// than the compositor's eight is refused at the draw, not drawn with some of them dropped. (The first half checks the busiest clip of a cut.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_more_fades_than_a_blend_runs_are_refused_not_drawn() {
    use kerf_core::{BlendMode, FadeEdge, FadeStep, FadeTint};
    let m = media();
    // The busiest clip a cut can make: both own fades, a dip to white into it and another out of it, and a dissolve on the next.
    let mut first = clip(&m.noise, 0.5, 1.0, 0.0);
    first.blend_mode = BlendMode::Screen;
    let mut mid = clip(&m.testsrc, 0.5, 1.5, 0.5);
    mid.blend_mode = BlendMode::Screen;
    mid.fade_in = 0.6;
    mid.fade_out = 0.6;
    mid.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.8,
    });
    let mut last = clip(&m.edges, 0.5, 1.2, 1.5);
    last.blend_mode = BlendMode::Screen;
    last.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.8,
    });
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![first, mid, last]], None);
    let assets = [m.bars.clone(), m.noise.clone(), m.testsrc.clone(), m.edges.clone()];
    let opts = at_fps(30.0);
    let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let busiest = (0..90u64)
        .map(|k| {
            planner
                .at_frame(k)
                .expect("plan")
                .layers
                .iter()
                .map(|l| l.fx.fades.len())
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0);
    eprintln!("checker/e/ramps: the busiest layer of the cut has {busiest} fades");
    assert!(busiest <= 5, "a clip's chain has at most five fades, the cut made {busiest}");
    // The same frame with the fades of the blended layer multiplied: up to eight are drawn, nine and more are an error.
    let k = 20;
    let size = {
        let plan = planner.at_frame(k).expect("plan");
        (plan.canvas.width, plan.canvas.height)
    };
    let mut verdicts: Vec<(usize, String)> = Vec::new();
    let inject = |verdicts: &mut Vec<(usize, String)>| {
        for total in [1usize, 5, 8, 9, 12, 40] {
            let mut plan = planner.at_frame(k).expect("plan");
            let top = plan.layers.len() - 1;
            let step = FadeStep {
                edge: FadeEdge::In,
                tint: FadeTint::Alpha,
                st: 0.0,
                d: 1.0,
            };
            plan.layers[top].fx.fades = vec![step; total];
            let mut motion = MotionFrames::new(frame_source());
            let picture = motion.frames(&plan).expect("frames");
            let drawn = compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size);
            verdicts.push((
                total,
                match &drawn {
                    Ok(_) => "drawn".to_string(),
                    Err(kerf_gpu::GpuError::Unsupported(why)) => format!("refused: {}", why.chars().take(90).collect::<String>()),
                    Err(other) => format!("ERROR {other}"),
                },
            ));
        }
    };
    // (Where the probe refused this FFmpeg's chain the caps are forced, or every draw would be refused for that.)
    if compositor().caps().blend == kerf_core::BlendKinds::ALL {
        inject(&mut verdicts);
    } else {
        forced_blends(|| inject(&mut verdicts));
    }
    eprintln!("checker/e/ramps: fades injected into a blended layer: {verdicts:?}");
    for (total, verdict) in &verdicts {
        if *total <= 8 {
            assert_eq!(verdict, "drawn", "{total} fades");
        } else {
            assert!(
                verdict.starts_with("refused"),
                "{total} fades are not drawn with some dropped: {verdict}"
            );
        }
    }
}

/// A clip of `w`x`h` noise (lossless, untagged) and a smooth gradient of the same size, for the cuts of a frame of that shape.
fn ce_sized(w: u32, h: u32) -> (Asset, Asset) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-turn");
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str, lavfi: String| -> Asset {
        let p = dir.join(format!("{name}-{w}x{h}.mp4"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("{lavfi},setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "10",
            "-pix_fmt",
            "yuv420p",
            p.to_str().unwrap(),
        ]);
        probed(&p)
    };
    (
        make(
            "noise",
            format!(
                "nullsrc=size={w}x{h}:rate=30:duration=2,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
            ),
        ),
        make(
            "smooth",
            format!(
                "gradients=size={w}x{h}:rate=30:duration=2:c0=0xd03020:c1=0x2040e0:nb_colors=2:seed=5:speed=0.00001,format=yuv420p"
            ),
        ),
    )
}

/// A blended clip turned by a multiple of a quarter turn lands on whole samples (`rotate`'s fast path at 90, 180 and 270 degrees, its general one
/// at the others), and its box swaps sides, so it hangs over the frame's edges: held to the export on frames of several shapes, moved and off the
/// frame, with a mask, fades, a dissolve, a key and a keyed opacity; and what is not a quarter turn, or is turned by a key, is refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_blended_clip_turned_by_quarter_turns() {
    use kerf_core::{BlendMode, Property, PropertyKey};
    let m = media();
    let mut failures: Vec<String> = Vec::new();
    let angles = [
        90.0, 180.0, 270.0, -90.0, -180.0, -270.0, 360.0, 450.0, 540.0, 630.0, 720.0, -360.0, -450.0,
    ];
    let places = [(0.0, 0.0), (0.2, 0.1), (-0.45, 0.4), (0.0123, 0.0071)];
    let mut n = 0;
    for angle in angles {
        for (px, py) in places {
            let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
            c.blend_mode = BLEND_MODES[n % 9];
            c.transform.rotation = angle;
            c.transform.pos_x = px;
            c.transform.pos_y = py;
            n += 1;
            // (A turned box hangs over the frame's top edge, where FFmpeg 4.4's overlay drops the alpha rows.)
            let above = (angle.rem_euclid(180.0) - 90.0).abs() < 1.0 || py < 0.0;
            ce_check(
                &format!("checker/e/turned {angle} at {px},{py} {:?}", c.blend_mode),
                &timeline(vec![vec![clip(&m.smooth, 0.5, 1.5, 0.0)], vec![c]], None),
                &[m.smooth.clone(), m.noise.clone()],
                30.0,
                0.4,
                !above,
                None,
                &mut failures,
            );
        }
    }
    // Frames of other shapes, taller than wide and wider than tall, small and not a multiple of four: the turned box swaps sides.
    for (w, h) in [
        (130, 66),
        (66, 130),
        (322, 182),
        (182, 322),
        (98, 50),
        (50, 98),
        (34, 18),
        (18, 34),
        (10, 6),
        (6, 10),
        (2, 2),
        (4, 2),
    ] {
        let (noise, smooth) = ce_sized(w, h);
        for (k, angle) in [90.0, 180.0, 270.0, -90.0, 450.0].into_iter().enumerate() {
            for (px, py) in [(0.0, 0.0), (0.1, 0.2)] {
                let mut c = clip(&noise, 0.2, 1.0, 0.0);
                c.blend_mode = BLEND_MODES[(n + k) % 9];
                c.transform.rotation = angle;
                c.transform.pos_x = px;
                c.transform.pos_y = py;
                n += 1;
                ce_check(
                    &format!("checker/e/{w}x{h} turned {angle} at {px},{py} {:?}", c.blend_mode),
                    &timeline(vec![vec![clip(&smooth, 0.2, 1.0, 0.0)], vec![c]], None),
                    &[smooth.clone(), noise.clone()],
                    30.0,
                    0.3,
                    false,
                    None,
                    &mut failures,
                );
            }
        }
    }
    // A turned clip with an alpha of its own, at the angles the fast path takes and one it does not.
    for angle in [90.0, 180.0, -90.0, 450.0] {
        let go = |k: usize, what: &str, edit: &dyn Fn(&mut Clip), secs: f64, failures: &mut Vec<String>| {
            let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
            c.blend_mode = BLEND_MODES[k % 9];
            c.transform.rotation = angle;
            edit(&mut c);
            ce_check(
                &format!("checker/e/turned {angle} {what} {:?}", c.blend_mode),
                &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
                &[m.bars.clone(), m.testsrc.clone()],
                30.0,
                secs,
                false,
                None,
                failures,
            );
        };
        go(
            n,
            "with an ellipse mask",
            &|c| c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, false)),
            0.4,
            &mut failures,
        );
        n += 1;
        go(
            n,
            "with an inverted rect mask",
            &|c| c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 1.0, true)),
            0.4,
            &mut failures,
        );
        n += 1;
        go(
            n,
            "with fades",
            &|c| {
                c.fade_in = 0.4;
                c.fade_out = 0.4;
            },
            1.0,
            &mut failures,
        );
        n += 1;
        go(
            n,
            "with fades, a mask and a position",
            &|c| {
                c.fade_in = 0.3;
                c.fade_out = 0.5;
                c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 1.0, false));
                c.transform.pos_x = -0.3;
                c.transform.pos_y = 0.3;
            },
            1.0,
            &mut failures,
        );
        n += 1;
        go(
            n,
            "with a keyed opacity",
            &|c| {
                c.set_property_keys(
                    Property::Opacity,
                    vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 0.2)],
                );
            },
            1.0,
            &mut failures,
        );
        n += 1;
        go(
            n,
            "with a chroma key",
            &|c| c.effects = vec![key("green", 0.3, 0.2)],
            0.4,
            &mut failures,
        );
        n += 1;
        go(
            n,
            "with a mask moved off the frame",
            &|c| {
                c.transform.pos_x = 0.4;
                c.transform.pos_y = -0.4;
                c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.3, false));
            },
            0.4,
            &mut failures,
        );
        n += 1;
    }
    // Into a dissolve and over a plain clip, and two turned blended clips.
    for angle in [90.0, 270.0] {
        let out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
        inc.blend_mode = BlendMode::Screen;
        inc.transform.rotation = angle;
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        ce_check(
            &format!("checker/e/turned {angle} dissolving in"),
            &timeline(vec![vec![out, inc]], None),
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            2.0,
            false,
            None,
            &mut failures,
        );
        let mut lower = clip(&m.testsrc, 0.5, 1.5, 0.0);
        lower.blend_mode = BlendMode::Multiply;
        lower.transform.rotation = angle;
        let mut upper = clip(&m.noise, 0.5, 1.5, 0.0);
        upper.blend_mode = BlendMode::Difference;
        upper.transform.rotation = 180.0;
        ce_check(
            &format!("checker/e/two turned blended clips {angle}/180"),
            &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![lower], vec![upper]], None),
            &[m.bars.clone(), m.testsrc.clone(), m.noise.clone()],
            30.0,
            0.4,
            false,
            None,
            &mut failures,
        );
    }
    // A mask on a turned layer is refused by the plan (its `geq` runs on the picture, before the turn), on every frame, at every quarter.
    for angle in [90.0, 180.0, 270.0, -90.0] {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.blend_mode = BlendMode::Screen;
        c.transform.rotation = angle;
        c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, false));
        ce_check(
            &format!("checker/e/turned {angle} with a mask is refused"),
            &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            0.4,
            false,
            Some(false),
            &mut failures,
        );
    }
    // Angles a hair off a quarter turn: within `is_quarter_turn`'s tolerance (and the `rotate` fast path's, `FLT_EPSILON` radians) they are drawn
    // exactly or refused, never drawn wrong.
    for angle in [
        90.000_000_01,
        -90.000_000_01,
        180.000_000_01,
        89.999_999_99,
        450.000_000_01,
        270.000_000_01,
    ] {
        let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
        c.blend_mode = BLEND_MODES[n % 9];
        n += 1;
        c.transform.rotation = angle;
        ce_check(
            &format!("checker/e/turned {angle:.9} {:?}", c.blend_mode),
            &timeline(vec![vec![clip(&m.smooth, 0.5, 1.5, 0.0)], vec![c]], None),
            &[m.smooth.clone(), m.noise.clone()],
            30.0,
            0.4,
            false,
            None,
            &mut failures,
        );
    }
    // What is not a quarter turn is refused, a rotation a key moves is refused (even one that passes through 90), and a quarter turn off by less
    // than the filter's own tolerance (`FLT_EPSILON` radians) is not.
    for (what, angle) in [
        ("91", 91.0),
        ("89.99", 89.99),
        ("90.00001", 90.00001),
        ("45", 45.0),
        ("0.001", 0.001),
    ] {
        let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
        c.blend_mode = BlendMode::Multiply;
        c.transform.rotation = angle;
        ce_check(
            &format!("checker/e/turned {what}"),
            &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
            &[m.bars.clone(), m.noise.clone()],
            30.0,
            0.4,
            false,
            Some(false),
            &mut failures,
        );
    }
    for (what, keys) in [
        ("0 to 90", vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(1.0, 90.0)]),
        ("90 to 180", vec![PropertyKey::new(0.0, 90.0), PropertyKey::new(1.0, 180.0)]),
        ("90 held", vec![PropertyKey::new(0.0, 90.0), PropertyKey::new(1.0, 90.0)]),
    ] {
        let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
        c.blend_mode = BlendMode::Multiply;
        c.set_property_keys(Property::Rotation, keys);
        ce_check(
            &format!("checker/e/rotation keyed {what}"),
            &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
            &[m.bars.clone(), m.noise.clone()],
            30.0,
            0.8,
            false,
            None,
            &mut failures,
        );
    }
    // A source of odd sides (the compositor's scaler resizes it into the frame; any turn of it is refused or exact).
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-turn");
    std::fs::create_dir_all(&dir).unwrap();
    let odd = dir.join("odd-321x181.mkv");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=size=321x181:rate=30:duration=2,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
        "-c:v",
        "ffv1",
        "-pix_fmt",
        "yuv420p",
        odd.to_str().unwrap(),
    ]);
    let odd = probed(&odd);
    for angle in [90.0, 180.0] {
        let mut c = clip(&odd, 0.2, 1.0, 0.0);
        c.blend_mode = BlendMode::Screen;
        c.transform.rotation = angle;
        ce_check(
            &format!("checker/e/odd source turned {angle}"),
            &timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None),
            &[m.bars.clone(), odd.clone()],
            30.0,
            0.3,
            false,
            None,
            &mut failures,
        );
    }
    ce_expect_drawn("checker_e_a_blended_clip_turned_by_quarter_turns", &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// A 640x360 clip of `source` (an lavfi chain ending in `yuv420p`), lossless and tagged with the matrix, primaries and transfer given.
fn ce_tagged_src(name: &str, matrix: &str, primaries: &str, trc: &str, source: &str) -> Asset {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-tagged");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}-{matrix}-{trc}.mp4"));
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("{source},setparams=colorspace={matrix}:color_primaries={primaries}:color_trc={trc}"),
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-qp",
        "0",
        "-g",
        "10",
        "-colorspace",
        matrix,
        "-color_primaries",
        primaries,
        "-color_trc",
        trc,
        "-pix_fmt",
        "yuv420p",
        p.to_str().unwrap(),
    ]);
    probed(&p)
}

/// RGB pictures: still images and videos whose pixel format is RGB, so that the graph's `format=yuva420p` converts them where the compositor
/// has the decode's conversion. Noise (every triple), saturated and nearly saturated colours, a gradient, 16-bit, gray and a picture with an
/// alpha plane (which the plan refuses for its own reasons).
fn ce_rgb_pictures() -> Vec<(&'static str, Asset)> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-rgb");
    std::fs::create_dir_all(&dir).unwrap();
    let still = |name: &str, lavfi: &str, pix: &str| -> Asset {
        let p = dir.join(name);
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            lavfi,
            "-frames:v",
            "1",
            "-pix_fmt",
            pix,
            p.to_str().unwrap(),
        ]);
        probed(&p)
    };
    let video = |name: &str, lavfi: &str, pix: &str| -> Asset {
        let p = dir.join(name);
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            pix,
            p.to_str().unwrap(),
        ]);
        probed(&p)
    };
    let planar = "nullsrc=size=640x360:rate=30:duration=2,format=gbrp";
    let noise = "geq=r='random(1)*255':g='random(2)*255':b='random(3)*255'";
    let sat = "geq=r='if(lt(mod(X,24),8),255,if(lt(mod(X,24),16),254,1))':g='if(lt(mod(Y,24),8),255,if(lt(mod(Y,24),16),254,1))':b='if(lt(mod(X+Y,24),12),255,0)'";
    vec![
        ("rgb noise png", still("noise.png", &format!("{planar},{noise}"), "rgb24")),
        (
            "rgb saturated png",
            still("saturated.png", &format!("{planar},{sat}"), "rgb24"),
        ),
        (
            "rgb gradient png",
            still(
                "gradient.png",
                &format!("{planar},geq=r='X*255/W':g='Y*255/H':b='(X+Y)*255/(W+H)'"),
                "rgb24",
            ),
        ),
        (
            "rgb 16-bit png",
            still("sixteen.png", &format!("{planar},{noise}"), "rgb48be"),
        ),
        (
            "gray png",
            still("gray.png", &format!("{planar},{noise},format=gray"), "gray"),
        ),
        (
            "rgba png",
            still(
                "alpha.png",
                &format!("{planar},{noise},format=gbrap,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='mod(X*2,256)'"),
                "rgba",
            ),
        ),
        ("rgb24 video", video("rgb24.mkv", &format!("{planar},{noise}"), "rgb24")),
        ("gbrp video", video("gbrp.mkv", &format!("{planar},{sat}"), "gbrp")),
        ("bgr0 video", video("bgr0.mkv", &format!("{planar},{noise}"), "bgr0")),
        ("x264rgb video", {
            let p = dir.join("x264rgb.mp4");
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!("{planar},{noise}"),
                "-c:v",
                "libx264rgb",
                "-qp",
                "0",
                "-pix_fmt",
                "rgb24",
                p.to_str().unwrap(),
            ]);
            probed(&p)
        }),
        ("bgr0 video tagged bt709", {
            let p = dir.join("bgr0-709.mkv");
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!("{planar},{noise}"),
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "bgr0",
                "-colorspace",
                "bt709",
                p.to_str().unwrap(),
            ]);
            probed(&p)
        }),
    ]
}

/// RGB pictures under a blend: the graph converts them with `format=yuva420p` where the compositor has the decode's `scale=out_range=tv`, and
/// the probe says whether this FFmpeg's two are the same picture (`caps.blend_extras`). Every picture is held to the export over untagged and
/// BT.709 footage, under and over, moved, with an alpha of its own and in a dissolve; where the caps say RGB pictures are drawn the plain
/// ones must be (all frames, exact), where they do not every frame must be refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_rgb_pictures_under_a_blend() {
    use kerf_core::{BlendExtras, BlendMode};
    let m = media();
    let v = variants();
    let on = compositor().caps().blend_extras.contains(BlendExtras::RGB_PICTURE);
    let drawn_blends = compositor().caps().blend == kerf_core::BlendKinds::ALL;
    eprintln!("checker/e/rgb: caps say RGB pictures are drawn: {on}");
    let bars2020 = ce_tagged("bars", "bt2020nc", "smptehdbars");
    let mut pictures = ce_rgb_pictures();
    pictures.push(("rgb testsrc png", v.rgb_png.clone()));
    let mut failures: Vec<String> = Vec::new();
    for (i, (name, picture)) in pictures.iter().enumerate() {
        let plain_rgb = matches!(
            *name,
            "rgb noise png"
                | "rgb saturated png"
                | "rgb gradient png"
                | "rgb testsrc png"
                | "rgb24 video"
                | "bgr0 video"
                | "x264rgb video"
                | "bgr0 video tagged bt709"
        );
        for (k, mode) in [
            BlendMode::Multiply,
            BlendMode::Difference,
            BlendMode::Screen,
            BlendMode::Overlay,
        ]
        .into_iter()
        .enumerate()
        {
            let mut top = clip(picture, 0.0, 1.0, 0.0);
            top.blend_mode = mode;
            // The picture over bars (untagged): where the caps say so, drawn.
            let want = (plain_rgb && drawn_blends).then_some(on);
            ce_check(
                &format!("checker/e/rgb {name} over bars {mode:?}"),
                &timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0)], vec![top.clone()]], None),
                &[m.bars.clone(), picture.clone()],
                30.0,
                0.4,
                false,
                if k == 0 { want } else { None },
                &mut failures,
            );
            if k > 1 {
                continue;
            }
            // Over BT.709 footage (the composite is converted in BT.709, the picture's RGB is converted by the graph's own default).
            ce_check(
                &format!("checker/e/rgb {name} over bt709 bars {mode:?}"),
                &timeline(vec![vec![clip(&v.bars709, 0.0, 1.0, 0.0)], vec![top.clone()]], None),
                &[v.bars709.clone(), picture.clone()],
                30.0,
                0.4,
                false,
                None,
                &mut failures,
            );
            // Over BT.2020 footage (FFmpeg 9 converts the picture into the bottom layer's matrix).
            ce_check(
                &format!("checker/e/rgb {name} over bt2020 bars {mode:?}"),
                &timeline(vec![vec![clip(&bars2020, 0.0, 1.0, 0.0)], vec![top.clone()]], None),
                &[bars2020.clone(), picture.clone()],
                30.0,
                0.4,
                false,
                None,
                &mut failures,
            );
            // Under a blended clip: the RGB picture is the composite.
            let mut over = clip(&m.testsrc, 0.0, 1.0, 0.0);
            over.blend_mode = mode;
            ce_check(
                &format!("checker/e/rgb testsrc over {name} {mode:?}"),
                &timeline(vec![vec![clip(picture, 0.0, 1.0, 0.0)], vec![over]], None),
                &[picture.clone(), m.testsrc.clone()],
                30.0,
                0.4,
                false,
                None,
                &mut failures,
            );
        }
        // Its own alpha: a mask and fades, moved, and into a dissolve.
        let mut top = clip(picture, 0.0, 1.0, 0.0);
        top.blend_mode = BLEND_MODES[(i * 2 + 1) % 9];
        top.fade_in = 0.3;
        top.fade_out = 0.3;
        top.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.5, false));
        top.transform.pos_x = 0.15;
        top.transform.pos_y = 0.1;
        ce_check(
            &format!("checker/e/rgb {name} masked, faded and moved"),
            &timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0)], vec![top]], None),
            &[m.bars.clone(), picture.clone()],
            30.0,
            0.9,
            false,
            None,
            &mut failures,
        );
        let mut inc = clip(picture, 0.0, 1.0, 1.0);
        inc.blend_mode = BLEND_MODES[(i * 2 + 4) % 9];
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        ce_check(
            &format!("checker/e/rgb {name} dissolving in"),
            &timeline(
                vec![vec![clip(&m.bars, 0.0, 1.0, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0)], vec![inc]],
                None,
            ),
            &[m.bars.clone(), m.testsrc.clone(), picture.clone()],
            30.0,
            1.9,
            false,
            None,
            &mut failures,
        );
    }
    // A stack of two RGB pictures, one blended over the other, over a plain one.
    let (a, b) = (&pictures[0].1, &pictures[1].1);
    let mut lower = clip(a, 0.0, 1.0, 0.0);
    lower.blend_mode = BlendMode::Exclusion;
    let mut upper = clip(b, 0.0, 1.0, 0.0);
    upper.blend_mode = BlendMode::HardLight;
    ce_check(
        "checker/e/rgb two blended pictures over a picture",
        &timeline(
            vec![vec![clip(&pictures[2].1, 0.0, 1.0, 0.0)], vec![lower], vec![upper]],
            None,
        ),
        &[pictures[2].1.clone(), a.clone(), b.clone()],
        30.0,
        0.4,
        false,
        None,
        &mut failures,
    );
    ce_expect_drawn("checker_e_rgb_pictures_under_a_blend", &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// A BT.2020 composite: the chain names `bt2020` and the compositor has the tables; the probe says whether this FFmpeg's chain is the model's with
/// them (`caps.blend_extras`). Held to the export with noise (blue overflows at other values than in BT.601), saturated colours and smooth pictures,
/// the `nc` tag (FFmpeg 9 cannot export a `bt2020c` cut at all), SDR and PQ, and 2020 beside untagged and BT.709 footage; where the caps say BT.2020 is drawn a 2020 pair must be.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_bt2020_composite_under_a_blend() {
    use kerf_core::{BlendExtras, BlendMode};
    let m = media();
    let v = variants();
    let on = compositor().caps().blend_extras.contains(BlendExtras::BT2020);
    let all = compositor().caps().blend == kerf_core::BlendKinds::ALL;
    eprintln!("checker/e/2020: caps say BT.2020 is drawn: {on}");
    let tag = |name: &str, matrix: &str, trc: &str, source: &str| {
        let prim = if trc == "smpte2084" { "bt2020" } else { "bt709" };
        ce_tagged_src(name, matrix, prim, trc, source)
    };
    let size = "size=640x360:rate=30:duration=4";
    let bars_src = format!("smptehdbars={size},format=yuv420p");
    let test_src = format!("testsrc2={size},format=yuv420p");
    let noise_src = format!("nullsrc={size},format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'");
    let smooth_src = format!(
        "gradients={size}:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=640:y1=360:speed=0.00001,format=yuv420p"
    );
    // Blues and nearly-blues: the corner of the YUV cube where the 32-bit sums wrap.
    let blue_src = format!("nullsrc={size},format=yuv420p,geq=lum='160+random(1)*95':cb='200+random(2)*55':cr='random(3)*80'");
    let nc = "bt709";
    let bars = tag("bars", "bt2020nc", nc, &bars_src);
    let test = tag("testsrc", "bt2020nc", nc, &test_src);
    let noise = tag("noise", "bt2020nc", nc, &noise_src);
    let smooth = tag("smooth", "bt2020nc", nc, &smooth_src);
    let blue = tag("blue", "bt2020nc", nc, &blue_src);
    let pq_bars = tag("bars", "bt2020nc", "smpte2084", &bars_src);
    let pq_test = tag("testsrc", "bt2020nc", "smpte2084", &test_src);
    let mut failures: Vec<String> = Vec::new();
    let run =
        |name: &str, bottom: &Asset, top: &Asset, draws: Option<bool>, edit: &dyn Fn(&mut Clip), failures: &mut Vec<String>| {
            let mut c = clip(top, 0.5, 1.5, 0.0);
            edit(&mut c);
            ce_check(
                &format!("checker/e/2020 {name} {:?}", c.blend_mode),
                &timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0)], vec![c]], None),
                &[bottom.clone(), top.clone()],
                30.0,
                0.4,
                false,
                draws,
                failures,
            );
        };
    for (k, mode) in BLEND_MODES.into_iter().enumerate() {
        // Pairs of 2020 footage: drawn where the caps say so, for every mode.
        let want = (all && on).then_some(true);
        run(
            "testsrc over bars",
            &bars,
            &test,
            want,
            &|c| c.blend_mode = mode,
            &mut failures,
        );
        run(
            "noise over smooth",
            &smooth,
            &noise,
            want,
            &|c| c.blend_mode = mode,
            &mut failures,
        );
        run(
            "blues over noise",
            &noise,
            &blue,
            want,
            &|c| c.blend_mode = mode,
            &mut failures,
        );
        run(
            "noise over blues",
            &blue,
            &noise,
            want,
            &|c| c.blend_mode = mode,
            &mut failures,
        );
        if k % 3 == 0 {
            run("PQ pair", &pq_bars, &pq_test, None, &|c| c.blend_mode = mode, &mut failures);
            run(
                "2020 over untagged",
                &m.bars,
                &test,
                None,
                &|c| c.blend_mode = mode,
                &mut failures,
            );
            run(
                "untagged over 2020",
                &bars,
                &m.testsrc,
                None,
                &|c| c.blend_mode = mode,
                &mut failures,
            );
            run(
                "2020 over bt709",
                &v.bars709,
                &test,
                None,
                &|c| c.blend_mode = mode,
                &mut failures,
            );
            run(
                "bt709 over 2020",
                &bars,
                &v.testsrc709,
                None,
                &|c| c.blend_mode = mode,
                &mut failures,
            );
        }
    }
    // With an alpha of its own: fades, a mask and a position.
    for mode in [BlendMode::Screen, BlendMode::Difference] {
        run(
            "noise faded, masked and moved",
            &smooth,
            &noise,
            None,
            &|c| {
                c.blend_mode = mode;
                c.fade_in = 0.2;
                c.fade_out = 0.2;
                c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.5, true));
                c.transform.pos_x = -0.2;
                c.transform.pos_y = 0.1;
            },
            &mut failures,
        );
    }
    ce_expect_drawn("checker_e_a_bt2020_composite_under_a_blend", &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// The most alpha fades a layer of the cut has running (a factor below [`kerf_core::FADE_FULL`]) on one frame, and how many frames have at
/// least `three`.
fn ce_overlapping_ramps(tl: &Timeline, assets: &[Asset], frames: Range<u64>) -> (usize, usize) {
    let planner = Planner::new(tl, assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
    let (mut most, mut crowded) = (0, 0);
    for k in frames {
        let plan = planner.at_frame(k).expect("plan");
        let running = plan
            .layers
            .iter()
            .filter(|l| !l.blend.is_normal())
            .map(|l| {
                plan.fade_ops(l)
                    .iter()
                    .filter(|op| op.tint == kerf_core::FadeTint::Alpha && op.factor < kerf_core::FADE_FULL)
                    .count()
            })
            .max()
            .unwrap_or(0);
        most = most.max(running);
        crowded += usize::from(running >= 3);
    }
    (most, crowded)
}

/// Three (and, on a clip shorter than its transitions' halves, four) of a blended clip's alpha fades running on the same frames (its own fade-in and
/// fade-out and the two transitions on its edges), each a rounding step of its own, so the order of the chain decides the sample: the case asserts
/// that frames with three (and with four, where the cut can) running are in the cut, then holds them to the export. A feathered mask gives the alpha plane every level.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_three_and_four_ramps_run_on_one_frame() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures: Vec<String> = Vec::new();
    let mut crowded_cuts = 0;
    for (n, (kin, kout, fi, fo, din, dout)) in [
        (TransitionKind::DipToWhite, TransitionKind::DipToWhite, 0.9, 0.9, 0.6, 0.6),
        (TransitionKind::Crossfade, TransitionKind::DipToWhite, 0.9, 0.7, 0.8, 0.5),
        (TransitionKind::DipToBlack, TransitionKind::Crossfade, 0.8, 0.8, 0.7, 0.7),
        (TransitionKind::DipToWhite, TransitionKind::DipToBlack, 1.0, 1.0, 0.9, 0.9),
        (TransitionKind::Crossfade, TransitionKind::Crossfade, 0.6, 0.6, 0.5, 0.5),
        (TransitionKind::SlideLeft, TransitionKind::PushRight, 0.9, 0.9, 0.6, 0.6),
    ]
    .into_iter()
    .enumerate()
    {
        for masked in [false, true] {
            let mut first = clip(&m.noise, 0.5, 1.0, 0.0);
            first.blend_mode = BlendMode::Screen;
            let mut mid = clip(&m.testsrc, 0.5, 1.5, 0.5);
            mid.blend_mode = [BlendMode::Multiply, BlendMode::Difference, BlendMode::Overlay][n % 3];
            mid.fade_in = fi;
            mid.fade_out = fo;
            mid.transition_in = Some(Transition {
                kind: kin,
                duration: din,
            });
            if masked {
                mid.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 1.0, false));
            }
            let mut last = clip(&m.edges, 0.5, 1.2, 1.5);
            last.blend_mode = BlendMode::Exclusion;
            last.transition_in = Some(Transition {
                kind: kout,
                duration: dout,
            });
            let tl = timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![first, mid, last]], None);
            let assets = [m.bars.clone(), m.noise.clone(), m.testsrc.clone(), m.edges.clone()];
            let (most, crowded) = ce_overlapping_ramps(&tl, &assets, 0..66);
            eprintln!(
                "checker/e/crowded {kin:?}/{kout:?} masked {masked}: up to {most} ramps on a frame, {crowded} frames with three or more"
            );
            if most >= 3 {
                crowded_cuts += 1;
            }
            let up = matches!(kin, TransitionKind::SlideDown | TransitionKind::PushDown)
                || matches!(kout, TransitionKind::SlideDown | TransitionKind::PushDown);
            ce_check(
                &format!("checker/e/crowded {kin:?}/{kout:?} {fi}/{fo}/{din}/{dout} masked {masked}"),
                &tl,
                &assets,
                30.0,
                2.2,
                !up,
                None,
                &mut failures,
            );
        }
    }
    // A clip shorter than its transitions' halves: the clip's two dips (or a dissolve and a dip) and its own two fades all run on the same frames.
    let mut four = 0;
    for (n, (len, kin, kout, fade, din, dout)) in [
        (0.2, TransitionKind::DipToWhite, TransitionKind::DipToWhite, 0.2, 0.4, 0.4),
        (0.3, TransitionKind::DipToWhite, TransitionKind::DipToWhite, 0.3, 0.6, 0.6),
        (0.2, TransitionKind::Crossfade, TransitionKind::DipToWhite, 0.2, 0.4, 0.4),
        (0.4, TransitionKind::DipToWhite, TransitionKind::DipToBlack, 0.4, 0.8, 0.8),
    ]
    .into_iter()
    .enumerate()
    {
        let mut first = clip(&m.noise, 0.5, 1.0, 0.0);
        first.blend_mode = BlendMode::Screen;
        let mut mid = clip(&m.testsrc, 0.5, 0.5 + len, 0.5);
        mid.blend_mode = [BlendMode::Multiply, BlendMode::Difference, BlendMode::Overlay, BlendMode::Add][n];
        mid.fade_in = fade;
        mid.fade_out = fade;
        mid.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 1.0, false));
        mid.transition_in = Some(Transition {
            kind: kin,
            duration: din,
        });
        let mut last = clip(&m.edges, 0.5, 1.2, 0.5 + len);
        last.blend_mode = BlendMode::Exclusion;
        last.transition_in = Some(Transition {
            kind: kout,
            duration: dout,
        });
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![first, mid, last]], None);
        let assets = [m.bars.clone(), m.noise.clone(), m.testsrc.clone(), m.edges.clone()];
        let (most, crowded) = ce_overlapping_ramps(&tl, &assets, 0..60);
        eprintln!(
            "checker/e/crowded short clip {len} {kin:?}/{kout:?}: up to {most} ramps on a frame, {crowded} frames with three or more"
        );
        four += usize::from(most >= 4);
        ce_check(
            &format!("checker/e/crowded short clip {len} {kin:?}/{kout:?} fades {fade} transitions {din}/{dout}"),
            &tl,
            &assets,
            30.0,
            2.0,
            true,
            None,
            &mut failures,
        );
    }
    eprintln!("checker/e/crowded: {four} of the short clips have four ramps on a frame");
    assert!(
        crowded_cuts >= 6,
        "the cuts must have three ramps on a frame: only {crowded_cuts} of 12 do"
    );
    ce_expect_drawn("checker_e_three_and_four_ramps_run_on_one_frame", &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// What the plan says of the three lifts when the caps hold only part of what a probe measures: an RGB picture needs `RGB_PICTURE`, a BT.2020
/// composite needs `BT2020` (the other one does not stand in), a mode missing from either matrix's answer is refused, `fcc` and `smpte240m` are
/// refused with every cap, and a quarter turn is not a cause at all. (No picture is drawn: this is the gating, which is the part of the lift
/// that the builds here, where the probe says yes, cannot show. FFmpeg 4.4.2 itself is not on this machine.)
#[test]
#[ignore = "needs ffmpeg (for the assets)"]
fn checker_e_the_lifts_are_refused_where_the_probe_did_not_measure_them() {
    use kerf_core::{BlendExtras, BlendKinds, BlendMode};
    use kerf_gpu::grade::Kernels;
    let m = media();
    let v = variants();
    let caps = |blend: [BlendKinds; 2], extras: BlendExtras| {
        Compositor::caps_with(Kernels::Match, Some(kerf_gpu::text::Model::Shaped), blend, extras)
    };
    let causes = |tl: &Timeline, assets: &[Asset], caps: &kerf_core::GpuCaps| -> Vec<String> {
        let planner = Planner::new(tl, assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
        let plan = planner.at_frame(5).expect("plan");
        plan.reasons(caps, (plan.canvas.width, plan.canvas.height))
            .iter()
            .filter(|u| u.kind() == "blend_mode")
            .map(ToString::to_string)
            .collect()
    };
    let all = [BlendKinds::ALL; 2];
    let mut failures: Vec<String> = Vec::new();
    let mut expect = |what: &str, got: Vec<String>, cause: Option<&str>| {
        let ok = match cause {
            None => got.is_empty(),
            Some(c) => !got.is_empty() && got.iter().all(|w| w.contains(c)),
        };
        eprintln!("checker/e/gating {what}: {got:?}");
        if !ok {
            failures.push(format!("{what}: wanted {cause:?}, the plan says {got:?}"));
        }
    };
    let bt2020 = (
        ce_tagged("bars", "bt2020nc", "smptehdbars"),
        ce_tagged("testsrc", "bt2020nc", "testsrc2"),
    );
    for mode in [BlendMode::Multiply, BlendMode::Exclusion] {
        let rgb = ce_stack(&m.bars, &v.rgb_png, &|c| c.blend_mode = mode);
        let (tl2020, assets2020) = ce_stack(&bt2020.0, &bt2020.1, &|c| c.blend_mode = mode);
        let (turned, turned_assets) = ce_stack(&m.bars, &m.noise, &|c| {
            c.blend_mode = mode;
            c.transform.rotation = 90.0;
        });
        for (extras, name) in [
            (BlendExtras::NONE, "no extras"),
            (BlendExtras::RGB_PICTURE, "RGB pictures only"),
            (BlendExtras::BT2020, "BT.2020 only"),
            (BlendExtras::ALL, "all extras"),
        ] {
            let c = caps(all, extras);
            let rgb_on = extras.contains(BlendExtras::RGB_PICTURE);
            let b2020_on = extras.contains(BlendExtras::BT2020);
            expect(
                &format!("{mode:?} rgb picture, {name}"),
                causes(&rgb.0, &rgb.1, &c),
                (!rgb_on).then_some("an RGB picture"),
            );
            expect(
                &format!("{mode:?} bt2020 pair, {name}"),
                causes(&tl2020, &assets2020, &c),
                (!b2020_on).then_some("BT.2020"),
            );
            expect(
                &format!("{mode:?} quarter turn, {name}"),
                causes(&turned, &turned_assets, &c),
                None,
            );
        }
    }
    // A mode in one matrix's answer and not the other's is not in the caps; the cut that holds it is refused in either matrix.
    let screen = BlendKinds::of(BlendMode::Screen);
    let (screen_cut, screen_assets) = ce_stack(&m.bars, &m.testsrc, &|c| c.blend_mode = BlendMode::Screen);
    let (add_cut, add_assets) = ce_stack(&m.bars, &m.testsrc, &|c| c.blend_mode = BlendMode::Add);
    for (blend, name) in [
        (
            [BlendKinds::ALL.without(screen), BlendKinds::ALL],
            "Screen missing from BT.601",
        ),
        (
            [BlendKinds::ALL, BlendKinds::ALL.without(screen)],
            "Screen missing from BT.709",
        ),
        ([BlendKinds::NONE; 2], "nothing measured"),
    ] {
        let c = caps(blend, BlendExtras::ALL);
        expect(
            &format!("{name}: screen"),
            causes(&screen_cut, &screen_assets, &c),
            Some("no compositor pass draws this mode"),
        );
        let add_kept = blend[0].contains(BlendKinds::of(BlendMode::Add)) && blend[1].contains(BlendKinds::of(BlendMode::Add));
        expect(
            &format!("{name}: add"),
            causes(&add_cut, &add_assets, &c),
            (!add_kept).then_some("no compositor pass draws this mode"),
        );
    }
    // No tables for fcc and smpte240m, whatever the caps say.
    for tag in ["fcc", "smpte240m"] {
        let bottom = ce_tagged("bars", tag, "smptehdbars");
        let top = ce_tagged("testsrc", tag, "testsrc2");
        let (tl, assets) = ce_stack(&bottom, &top, &|c| c.blend_mode = BlendMode::Screen);
        expect(
            &format!("{tag} with every cap"),
            causes(&tl, &assets, &caps(all, BlendExtras::ALL)),
            Some("the composite's matrix is not BT.601, BT.709 or BT.2020"),
        );
    }
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// A quarter-turned blended clip whose opacity is keyed: the `geq` that makes the alpha runs on the picture before the turn, so whatever it does
/// at the picture's last column and row (it reads them one in on some FFmpeg builds, `GeqEdge`) lands on another edge of the turned box. Held to
/// the export at every quarter, in several modes, moved and not, over the first frames (the opacity runs from 1 to 0.2 across the clip). The same
/// clip plain (no blend mode) is the control, printed and not judged: it says whether a mismatch is the blend's or the turn's. (On FFmpeg 6.1.1 the
/// 90, 180 and 450 degree cases differ, the control's chroma too: the plan lets the turned layer's `geq` alpha through.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_turned_blended_clip_with_a_keyed_opacity() {
    use kerf_core::{BlendMode, Property, PropertyKey};
    let m = media();
    let mut failures: Vec<String> = Vec::new();
    for (k, angle) in [90.0, 180.0, 270.0, -90.0, 450.0].into_iter().enumerate() {
        for (j, (px, py)) in [(0.0, 0.0), (0.2, 0.1)].into_iter().enumerate() {
            for mode in [
                BlendMode::Normal,
                BlendMode::Darken,
                BlendMode::HardLight,
                BlendMode::Exclusion,
                BlendMode::Multiply,
            ] {
                if mode == BlendMode::Normal && j == 1 {
                    continue;
                }
                let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
                c.blend_mode = mode;
                c.transform.rotation = angle;
                c.transform.pos_x = px;
                c.transform.pos_y = py;
                c.set_property_keys(
                    Property::Opacity,
                    vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 0.2)],
                );
                let case = format!("checker/e/turned {angle} keyed opacity at {px},{py} {mode:?}");
                let scan = ce_scan(
                    &case,
                    &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
                    &[m.bars.clone(), m.testsrc.clone()],
                    &at_fps(30.0),
                    0..30,
                    false,
                );
                eprintln!("{}", scan.line(&case));
                CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
                if let Some(why) = scan.verdict(None) {
                    if mode == BlendMode::Normal {
                        eprintln!("CONTROL (no blend mode, the plain layer's own path) {case}: {why} (round {k})");
                    } else {
                        failures.push(format!("{case}: {why} (round {k})"));
                    }
                }
            }
        }
    }
    ce_expect_drawn("checker_e_a_turned_blended_clip_with_a_keyed_opacity", &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

// ---- checker/e/r3: a keyed opacity (a `geq` ahead of the `rotate`) on a turned layer -----------------------------------

/// A keyed opacity that falls from 1 across the clip (every alpha level a frame can have).
fn r3_keyed(c: &mut Clip, to: f64) {
    use kerf_core::{Property, PropertyKey};
    c.set_property_keys(Property::Opacity, vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(0.5, to)]);
}

/// `ce_check` for a layer whose picture the compositor's scaler or a crop touches: a plain layer it resizes is the scaler's ladder, not this
/// change's, so when the keyed cut differs the same cut with a constant opacity (the control) says whether the difference was already there.
/// A difference the control does not have is a failure; one it has is `known` (printed, not judged).
#[allow(clippy::too_many_arguments)]
fn r3_check(
    case: &str,
    tl: &Timeline,
    control: Option<&Timeline>,
    assets: &[Asset],
    fps: f64,
    secs: f64,
    draws: Option<bool>,
    failures: &mut Vec<String>,
    known: &mut Vec<String>,
) -> CeScan {
    let scan = ce_scan(case, tl, assets, &at_fps(fps), upto(secs, fps), false);
    eprintln!("{}", scan.line(case));
    CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
    let Some(why) = scan.verdict(draws) else { return scan };
    if let (Some(control), true) = (control, !scan.exact()) {
        let name = format!("{case} (control: constant opacity)");
        let c = ce_scan(&name, control, assets, &at_fps(fps), upto(secs, fps), false);
        eprintln!("{}", c.line(&name));
        if !c.exact() {
            known.push(format!(
                "{case}: {why} -- the control differs too (Y {} UV {})",
                c.y.differing, c.uv.differing
            ));
            return scan;
        }
    }
    failures.push(format!("{case}: {why}"));
    scan
}

fn r3_finish(what: &str, mut failures: Vec<String>, known: &[String]) {
    for k in known {
        eprintln!("KNOWN (not this change's) {k}");
    }
    ce_expect_drawn(what, &mut failures);
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// Odd sizes and odd offsets: frames of several shapes (the turned box swaps sides and hangs over the edges), the layer turned by every quarter
/// and by 450 degrees, at offsets that are not on the even grid, plain and under a blend mode, its opacity keyed. The plan must draw every one
/// (a keyed opacity on a turned layer is not refused) and the frame must be the export's to the sample.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_turned_keyed_opacity_at_odd_sizes_and_offsets() {
    let mut failures = Vec::new();
    let mut known = Vec::new();
    let mut n = 0;
    for (w, h) in [
        (130, 66),
        (66, 130),
        (322, 182),
        (98, 50),
        (50, 98),
        (34, 18),
        (18, 34),
        (10, 6),
        (6, 10),
    ] {
        let (noise, smooth) = ce_sized(w, h);
        for angle in [90.0, 180.0, 270.0, -90.0, 450.0] {
            for (px, py) in [(0.0, 0.0), (0.1, 0.2), (0.0123, 0.0071), (-0.37, 0.41)] {
                for mode in [None, Some(BLEND_MODES[n % 9])] {
                    n += 1;
                    let mut c = clip(&noise, 0.2, 1.0, 0.0);
                    if let Some(mode) = mode {
                        c.blend_mode = mode;
                    }
                    c.transform.rotation = angle;
                    c.transform.pos_x = px;
                    c.transform.pos_y = py;
                    r3_keyed(&mut c, 0.1 + (n % 7) as f64 * 0.1);
                    let case = format!("checker/e/r3/odd {w}x{h} turned {angle} at {px},{py} {mode:?}");
                    // (A frame two chroma samples across or less has nothing to read one in from: the guard may refuse it.)
                    let draws = (w.min(h) > 10).then_some(true);
                    r3_check(
                        &case,
                        &timeline(vec![vec![clip(&smooth, 0.2, 1.0, 0.0)], vec![c]], None),
                        None,
                        &[smooth.clone(), noise.clone()],
                        30.0,
                        0.3,
                        draws,
                        &mut failures,
                        &mut known,
                    );
                }
            }
        }
    }
    r3_finish(
        "checker_e_r3_a_turned_keyed_opacity_at_odd_sizes_and_offsets",
        failures,
        &known,
    );
}

/// A keyed opacity on a turned layer with a mask (refused: a mask on a rotated layer), with a crop (odd picture sizes), with a static zoom (a
/// picture smaller and larger than the frame), with a keyed zoom (refused behind a rotation), and the unturned layers with a keyed zoom or a mask
/// (the planes shifted ahead of a late zoom) as controls of the path the change touched.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_turned_keyed_opacity_with_a_mask_a_crop_or_a_zoom() {
    use kerf_core::{BlendMode, Property, PropertyKey};
    let m = media();
    let mut failures = Vec::new();
    let mut known = Vec::new();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let modes = [None, Some(BlendMode::Multiply), Some(BlendMode::Exclusion)];
    let stack = |edit: &dyn Fn(&mut Clip), mode: Option<BlendMode>, keyed: bool| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        if let Some(mode) = mode {
            c.blend_mode = mode;
        }
        edit(&mut c);
        if keyed {
            r3_keyed(&mut c, 0.25);
        }
        timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None)
    };
    // A crop: pictures of odd sizes, turned.
    for (l, r, t, b) in [
        (0.1, 0.2, 0.15, 0.05),
        (0.013, 0.0171, 0.033, 0.0291),
        (0.25, 0.0, 0.0, 0.0),
        (0.0, 0.31, 0.27, 0.0),
    ] {
        for angle in [90.0, 180.0, 270.0] {
            for mode in modes {
                for at in [(0.0, 0.0), (0.0301, -0.0173)] {
                    let edit = |c: &mut Clip| {
                        c.transform.crop_left = l;
                        c.transform.crop_right = r;
                        c.transform.crop_top = t;
                        c.transform.crop_bottom = b;
                        c.transform.rotation = angle;
                        c.transform.pos_x = at.0;
                        c.transform.pos_y = at.1;
                    };
                    r3_check(
                        &format!("checker/e/r3/crop {l},{r},{t},{b} turned {angle} at {at:?} {mode:?}"),
                        &stack(&edit, mode, true),
                        Some(&stack(&edit, mode, false)),
                        &assets,
                        30.0,
                        0.5,
                        None,
                        &mut failures,
                        &mut known,
                    );
                }
            }
        }
    }
    // A static zoom: the picture smaller than the frame (odd sizes) and larger.
    for scale in [0.37, 0.53, 0.61, 1.0, 1.4] {
        for angle in [90.0, 180.0, 270.0, -90.0] {
            for mode in modes {
                let edit = |c: &mut Clip| {
                    c.transform.scale = scale;
                    c.transform.rotation = angle;
                    c.transform.pos_x = 0.07;
                    c.transform.pos_y = -0.05;
                };
                r3_check(
                    &format!("checker/e/r3/static zoom {scale} turned {angle} {mode:?}"),
                    &stack(&edit, mode, true),
                    Some(&stack(&edit, mode, false)),
                    &assets,
                    30.0,
                    0.5,
                    None,
                    &mut failures,
                    &mut known,
                );
            }
        }
    }
    // A keyed zoom behind a turn is refused (`ZoomBehind`), and drawn anyway is the refusal's necessity (printed): the unturned layer is drawn.
    let keyed_zoom = |c: &mut Clip| {
        c.set_property_keys(Property::Scale, vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 1.6)]);
    };
    for mode in modes {
        for angle in [0.0, 90.0, 180.0, 270.0] {
            let edit = |c: &mut Clip| {
                keyed_zoom(c);
                c.transform.rotation = angle;
            };
            let case = format!("checker/e/r3/keyed zoom turned {angle} {mode:?}");
            let scan = r3_check(
                &case,
                &stack(&edit, mode, true),
                Some(&stack(&edit, mode, false)),
                &assets,
                30.0,
                0.5,
                (angle != 0.0).then_some(false),
                &mut failures,
                &mut known,
            );
            if angle != 0.0 {
                let any = ce_scan(
                    &format!("{case} (drawn anyway)"),
                    &stack(&edit, mode, true),
                    &assets,
                    &at_fps(30.0),
                    0..15,
                    true,
                );
                eprintln!("refusal necessity: {}", any.line(&format!("{case} (drawn anyway)")));
                let _ = scan;
            }
        }
    }
    // A mask on a turned layer is refused; drawn anyway is the compositor's own guard (printed).
    for angle in [0.0, 90.0, 180.0] {
        for mode in modes {
            let edit = |c: &mut Clip| {
                c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, false));
                c.transform.rotation = angle;
            };
            let case = format!("checker/e/r3/mask turned {angle} {mode:?}");
            r3_check(
                &case,
                &stack(&edit, mode, true),
                None,
                &assets,
                30.0,
                0.5,
                (angle != 0.0).then_some(false),
                &mut failures,
                &mut known,
            );
            let any = ce_scan(
                &format!("{case} (drawn anyway)"),
                &stack(&edit, mode, true),
                &assets,
                &at_fps(30.0),
                0..15,
                true,
            );
            eprintln!("refusal necessity: {}", any.line(&format!("{case} (drawn anyway)")));
        }
    }
    r3_finish(
        "checker_e_r3_a_turned_keyed_opacity_with_a_mask_a_crop_or_a_zoom",
        failures,
        &known,
    );
}

/// A keyed opacity on a turned layer inside a transition (the incoming clip, the outgoing, both), plain and blended, with a fade of its own.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_turned_keyed_opacity_in_a_transition() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures = Vec::new();
    let mut known = Vec::new();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut n = 0;
    for kind in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::PushRight,
    ] {
        for (dur, which) in [(0.6, "in"), (0.3, "out"), (0.6, "both"), (0.5, "in+fade")] {
            for angle in [90.0, 180.0, 270.0] {
                for blended in [false, true] {
                    n += 1;
                    let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
                    let mut inc = clip(&m.testsrc, 0.5, 1.5, 0.7);
                    if blended {
                        inc.blend_mode = BLEND_MODES[n % 9];
                        out.blend_mode = BlendMode::Screen;
                    }
                    if which != "out" {
                        inc.transform.rotation = angle;
                        r3_keyed(&mut inc, 0.2);
                    }
                    if which == "out" || which == "both" {
                        out.transform.rotation = angle + 180.0;
                        r3_keyed(&mut out, 0.3);
                    }
                    if which == "in+fade" {
                        inc.fade_out = 0.4;
                    }
                    inc.transition_in = Some(Transition { kind, duration: dur });
                    r3_check(
                        &format!("checker/e/r3/transition {kind:?} {dur}s {which} turned {angle} blended {blended}"),
                        &timeline(vec![vec![out, inc]], None),
                        None,
                        &assets,
                        30.0,
                        1.65,
                        None,
                        &mut failures,
                        &mut known,
                    );
                }
            }
        }
    }
    r3_finish("checker_e_r3_a_turned_keyed_opacity_in_a_transition", failures, &known);
}

/// The turned layer's source is 4:4:4, 4:2:2, gray, full range, a JPEG, 10-bit, an RGB still: each is drawn exactly or refused, plain and under a
/// blend mode.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_turned_keyed_opacity_of_other_formats() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let r = range_media();
    let mut failures = Vec::new();
    let mut known = Vec::new();
    for (name, asset) in [
        ("4:4:4", &v.yuv444),
        ("4:2:2", &v.yuv422),
        ("gray", &v.gray),
        ("yuvj420p mjpeg", &v.mjpeg),
        ("yuvj420p still", &v.jpeg),
        ("10-bit", &v.tenbit),
        ("rgb png", &v.rgb_png),
        ("flagged pc", &r.pc),
        ("flagged tv", &r.tv),
        ("unflagged", &r.unflagged),
        ("bt709", &v.testsrc709),
        ("smpte170m", &v.testsrc170m),
    ] {
        for angle in [90.0, 180.0, 270.0] {
            for mode in [None, Some(BlendMode::Multiply), Some(BlendMode::Difference)] {
                let mut c = clip(asset, 0.0, 1.0, 0.0);
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                c.transform.rotation = angle;
                c.transform.pos_x = 0.0301;
                r3_keyed(&mut c, 0.2);
                r3_check(
                    &format!("checker/e/r3/format {name} turned {angle} {mode:?} over bars"),
                    &timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0)], vec![c]], None),
                    None,
                    &[m.bars.clone(), asset.clone()],
                    30.0,
                    0.5,
                    None,
                    &mut failures,
                    &mut known,
                );
            }
        }
    }
    r3_finish("checker_e_r3_a_turned_keyed_opacity_of_other_formats", failures, &known);
}

/// Partly or wholly off the canvas, and two turned layers stacked (each with its own keyed opacity), plain and blended.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_turned_keyed_opacities_off_the_canvas_and_stacked() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures = Vec::new();
    let mut known = Vec::new();
    let assets = [m.bars.clone(), m.testsrc.clone(), m.noise.clone()];
    let mut n = 0;
    for (px, py) in [
        (-0.6, 0.5),
        (0.8, -0.7),
        (0.95, 0.95),
        (-0.9, -0.9),
        (-1.1, 0.0),
        (0.0, 1.3),
        (0.5, 0.0),
        (0.0, -0.5),
    ] {
        for angle in [90.0, 180.0, 270.0] {
            for mode in [None, Some(BLEND_MODES[n % 9])] {
                n += 1;
                let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                c.transform.rotation = angle;
                c.transform.pos_x = px;
                c.transform.pos_y = py;
                r3_keyed(&mut c, 0.3);
                r3_check(
                    &format!("checker/e/r3/off canvas turned {angle} at {px},{py} {mode:?}"),
                    &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
                    None,
                    &assets[..2],
                    30.0,
                    0.5,
                    None,
                    &mut failures,
                    &mut known,
                );
            }
        }
    }
    for (a1, a2) in [(90.0, 180.0), (180.0, 270.0), (270.0, 90.0), (90.0, 90.0), (180.0, 450.0)] {
        for (mode1, mode2) in [
            (None, None),
            (Some(BlendMode::Multiply), None),
            (None, Some(BlendMode::Screen)),
            (Some(BlendMode::Darken), Some(BlendMode::Difference)),
        ] {
            let mut mid = clip(&m.testsrc, 0.5, 1.5, 0.0);
            mid.transform.rotation = a1;
            mid.transform.pos_x = 0.1;
            mid.transform.pos_y = 0.05;
            r3_keyed(&mut mid, 0.4);
            let mut top = clip(&m.noise, 0.5, 1.5, 0.0);
            top.transform.rotation = a2;
            top.transform.pos_x = -0.2;
            top.transform.pos_y = 0.15;
            r3_keyed(&mut top, 0.15);
            if let Some(mode) = mode1 {
                mid.blend_mode = mode;
            }
            if let Some(mode) = mode2 {
                top.blend_mode = mode;
            }
            r3_check(
                &format!("checker/e/r3/stacked turned {a1} {mode1:?} under {a2} {mode2:?}"),
                &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![mid], vec![top]], None),
                None,
                &assets,
                30.0,
                0.5,
                None,
                &mut failures,
                &mut known,
            );
        }
    }
    r3_finish(
        "checker_e_r3_turned_keyed_opacities_off_the_canvas_and_stacked",
        failures,
        &known,
    );
}

/// A keyed opacity on a rotation that is not a quarter turn: refused or drawn exactly. (Plain, drawn: the rotated layer's edge is a ladder's
/// level; a blended one is refused.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_keyed_opacity_on_a_rotation_that_is_not_a_quarter_is_refused_or_exact() {
    use kerf_core::{BlendMode, Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut failures = Vec::new();
    let mut tally: Vec<String> = Vec::new();
    for angle in [12.0, -30.0, 45.0, 89.5, 90.5, 100.0, 179.9, 200.0, 359.5] {
        for mode in [None, Some(BlendMode::Multiply)] {
            for keyed in [true, false] {
                let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                c.transform.rotation = angle;
                if keyed {
                    r3_keyed(&mut c, 0.25);
                }
                let case = format!("checker/e/r3/rotation {angle} keyed {keyed} {mode:?}");
                let scan = ce_scan(
                    &case,
                    &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
                    &assets,
                    &at_fps(30.0),
                    0..15,
                    false,
                );
                eprintln!("{}", scan.line(&case));
                CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
                tally.push(format!(
                    "{case}: drawn {} of {}, Y {} UV {} differ",
                    scan.drawn, scan.frames, scan.y.differing, scan.uv.differing
                ));
                if let Some(why) = scan.verdict(None) {
                    failures.push(format!("{case}: {why}"));
                }
            }
        }
    }
    // A keyed rotation with a keyed opacity.
    let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
    c.set_property_keys(
        Property::Rotation,
        vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(1.0, 90.0)],
    );
    r3_keyed(&mut c, 0.25);
    let case = "checker/e/r3/keyed rotation with a keyed opacity";
    let scan = ce_scan(
        case,
        &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c.clone()]], None),
        &assets,
        &at_fps(30.0),
        0..15,
        false,
    );
    eprintln!("{}", scan.line(case));
    if let Some(why) = scan.verdict(None) {
        failures.push(format!("{case}: {why}"));
    }
    // The control (printed, not judged): the same keyed rotation with a constant opacity, to say whether a difference is the opacity's.
    c.set_property_keys(Property::Opacity, Vec::new());
    let case = "checker/e/r3/keyed rotation alone (control)";
    let control = ce_scan(
        case,
        &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
        &assets,
        &at_fps(30.0),
        0..15,
        false,
    );
    eprintln!("{}", control.line(case));
    for t in &tally {
        eprintln!("TALLY {t}");
    }
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

// ---- checker/e/r3: the coordinator's three read-through targets ------------------------------------------------------

/// The resolved layer of the last track at frame 3, for a case's line: its picture, the layer box and the rotation.
fn r3_geometry(tl: &Timeline, assets: &[Asset], fps: f64) -> String {
    let opts = at_fps(fps);
    let Ok(planner) = Planner::new(tl, assets, &opts, PlanRequest::motion(policy())) else {
        return "no plan".into();
    };
    let Ok(plan) = planner.at_frame(3) else {
        return "no frame".into();
    };
    let size = (plan.canvas.width, plan.canvas.height);
    plan.layers
        .last()
        .map_or("no layer".into(), |l| match plan.layer_geometry(l, size) {
            Ok(g) => format!(
                "picture {:?} layer {:?} stages {} rotation {}",
                g.picture,
                g.layer,
                g.stages.len(),
                g.rotation.map_or("none".into(), |r| format!("{:.4}", r.angle))
            ),
            Err(e) => format!("geometry refused: {e}"),
        })
}

/// Odd-sided layers that nothing resizes: a 4:4:4, 4:2:2 or gray source cropped to an odd width / height (their native chroma grids do not round
/// a crop to even), turned by every quarter, opaque and faded, plain and blended. Drawn exactly or refused; the geometry of each is printed so
/// that a case that is not odd says so.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_odd_sided_unscaled_layers_turned_plain_and_blended() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let mut failures = Vec::new();
    let mut known = Vec::new();
    let mut tally: Vec<String> = Vec::new();
    for (name, asset) in [
        ("4:4:4", &v.yuv444),
        ("4:2:2", &v.yuv422),
        ("gray", &v.gray),
        ("4:2:0", &m.testsrc),
    ] {
        for (l, r, t, b) in [
            (0.0, 1.0 / 640.0, 0.0, 0.0),
            (0.0, 0.0, 0.0, 1.0 / 360.0),
            (0.0, 1.0 / 640.0, 0.0, 1.0 / 360.0),
            (3.0 / 640.0, 0.0, 5.0 / 360.0, 0.0),
            (2.0 / 640.0, 5.0 / 640.0, 4.0 / 360.0, 3.0 / 360.0),
        ] {
            for angle in [0.0, 90.0, 180.0, 270.0] {
                for mode in [None, Some(BlendMode::Multiply), Some(BlendMode::Screen)] {
                    for faded in [false, true] {
                        let mut c = clip(asset, 0.2, 1.0, 0.0);
                        if let Some(mode) = mode {
                            c.blend_mode = mode;
                        }
                        c.transform.crop_left = l;
                        c.transform.crop_right = r;
                        c.transform.crop_top = t;
                        c.transform.crop_bottom = b;
                        c.transform.rotation = angle;
                        if faded {
                            c.fade_in = 0.15;
                            c.fade_out = 0.15;
                        }
                        let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
                        let assets = [m.bars.clone(), asset.clone()];
                        let case = format!(
                            "checker/e/r3/odd crop {name} {:.0},{:.0},{:.0},{:.0} turned {angle} {mode:?} faded {faded}",
                            l * 640.0,
                            r * 640.0,
                            t * 360.0,
                            b * 360.0
                        );
                        let geom = r3_geometry(&tl, &assets, 30.0);
                        let scan = r3_check(&case, &tl, None, &assets, 30.0, 0.4, None, &mut failures, &mut known);
                        tally.push(format!("{case}: {geom}: drawn {} of {}", scan.drawn, scan.frames));
                    }
                }
            }
        }
    }
    for t in &tally {
        eprintln!("TALLY {t}");
    }
    r3_finish(
        "checker_e_r3_odd_sided_unscaled_layers_turned_plain_and_blended",
        failures,
        &known,
    );
}

/// A clip tagged as its colour matrix, if this FFmpeg will write that tag (`None` when it will not): `tag` is `setparams`' name for it,
/// `encoder` the encoder option's.
fn r3_try_tagged(name: &str, (tag, encoder): (&str, &str), source: &str) -> Option<Asset> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-r3-tagged");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}-{tag}.mkv"));
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("{source},format=yuv420p,setparams=colorspace={tag}"))
        .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p", "-colorspace", encoder])
        .arg(&p)
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!(
            "tag {tag}: ffmpeg will not write it: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    Project::probe_asset(&p).ok()
}

/// Matrix tags the plan has no tables for (`bt2020c`, `ycgco`, `gbr`, `ictcp`, `chroma-derived-nc`, ...) on the bottom layer, the blended one and
/// both: `frame_matrix_tag` collapses them to no tag, which `blend_matrix` would draw as BT.601; FFmpeg 9 may follow the frame's tag instead.
/// Drawn exactly or refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_exotic_matrix_tags_under_a_blend() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures = Vec::new();
    let known: Vec<String> = Vec::new();
    let mut tally: Vec<String> = Vec::new();
    let tags = [
        ("bt2020c", "bt2020c"),
        ("ycgco", "ycgco"),
        ("gbr", "rgb"),
        ("ictcp", "ictcp"),
        ("chroma-derived-nc", "chroma-derived-nc"),
        ("chroma-derived-c", "chroma-derived-c"),
        ("smpte2085", "smpte2085"),
        ("unknown", "unknown"),
        ("fcc", "fcc"),
        ("smpte240m", "smpte240m"),
    ];
    for (tag, encoder) in tags {
        let bars = r3_try_tagged("bars", (tag, encoder), "smptehdbars=size=640x360:rate=30:duration=4");
        let top = r3_try_tagged("testsrc", (tag, encoder), "testsrc2=size=640x360:rate=30:duration=4");
        let (Some(bars), Some(top)) = (bars, top) else { continue };
        eprintln!(
            "tag {tag}: probed color_space bars {:?} top {:?}",
            bars.streams.iter().find_map(|s| s.color_space.clone()),
            top.streams.iter().find_map(|s| s.color_space.clone())
        );
        for mode in [BlendMode::Multiply, BlendMode::Difference, BlendMode::Screen, BlendMode::Add] {
            for (bname, bottom, tname, upper) in [
                ("untagged", &m.bars, tag, &top),
                (tag, &bars, "untagged", &m.testsrc),
                (tag, &bars, tag, &top),
            ] {
                let mut c = clip(upper, 0.5, 1.5, 0.0);
                c.blend_mode = mode;
                let tl = timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0)], vec![c]], None);
                let assets = [bottom.clone(), upper.clone()];
                let case = format!("checker/e/r3/matrix {mode:?} {tname} over {bname}");
                let cut = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let scan = ce_scan(&case, &tl, &assets, &at_fps(30.0), 0..12, false);
                    eprintln!("{}", scan.line(&case));
                    let anyway = ce_scan(&format!("{case} (drawn anyway)"), &tl, &assets, &at_fps(30.0), 0..12, true);
                    eprintln!("anyway: {}", anyway.line(&format!("{case} (drawn anyway)")));
                    (scan, anyway)
                }));
                match cut {
                    Ok((scan, anyway)) => {
                        CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
                        tally.push(format!(
                            "{case}: drawn {} of {}, exact {}; drawn anyway {} of {}, exact {}",
                            scan.drawn,
                            scan.frames,
                            scan.exact(),
                            anyway.drawn,
                            anyway.frames,
                            anyway.exact()
                        ));
                        if let Some(why) = scan.verdict(None) {
                            failures.push(format!("{case}: {why}"));
                        }
                    }
                    Err(_) => tally.push(format!(
                        "{case}: FFmpeg cannot export this cut at all (no frames to hold the GPU to)"
                    )),
                }
            }
        }
    }
    for t in &tally {
        eprintln!("TALLY {t}");
    }
    r3_finish("checker_e_r3_exotic_matrix_tags_under_a_blend", failures, &known);
}

/// A rotation too large for its angle to mean anything (`is_quarter_turn` of a radian count whose float has no fraction left): the layer must be
/// refused (or drawn as exactly as FFmpeg's `sin` / `cos` of that number turn it), blended and plain.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_huge_rotation_is_refused_or_exact() {
    use kerf_core::BlendMode;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut failures: Vec<String> = Vec::new();
    let mut tally: Vec<String> = Vec::new();
    for angle in [
        1e300,
        -1e300,
        2f64.powi(60),
        1e18,
        8e17,
        1e17,
        1e16,
        9e15,
        1e12,
        4e9,
        90.0 * 2f64.powi(40),
        90.0 * 2f64.powi(40) + 0.0625,
        1e7,
        f64::MAX,
    ] {
        for mode in [None, Some(BlendMode::Multiply), Some(BlendMode::Difference)] {
            let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
            if let Some(mode) = mode {
                c.blend_mode = mode;
            }
            c.transform.rotation = angle;
            let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
            let case = format!("checker/e/r3/huge rotation {angle:e} {mode:?}");
            let geom = r3_geometry(&tl, &assets, 30.0);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ce_scan(&case, &tl, &assets, &at_fps(30.0), 0..12, false)
            }));
            match result {
                Ok(scan) => {
                    eprintln!("{}", scan.line(&case));
                    tally.push(format!("{case}: {geom}: drawn {} of {}", scan.drawn, scan.frames));
                    if let Some(why) = scan.verdict(None) {
                        failures.push(format!("{case}: {geom}: {why}"));
                    }
                }
                Err(_) => tally.push(format!(
                    "{case}: {geom}: the export or the plan itself failed (a panic in the harness)"
                )),
            }
        }
    }
    for t in &tally {
        eprintln!("TALLY {t}");
    }
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// Many whole turns: `is_quarter_turn` calls every multiple of 90 degrees a quarter turn, but `rotate` evaluates its angle in a precision that
/// stops landing on whole samples a few turns out (FFmpeg's `rotate` of 3690 degrees is not its rotate of 90). A blended layer turned by 3600
/// degrees (10 turns) or less is drawn exactly; one turned by more is refused or exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_blended_clip_turned_by_many_whole_turns() {
    use kerf_core::BlendMode;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut failures: Vec<String> = Vec::new();
    let mut tally: Vec<String> = Vec::new();
    for quarters in [
        -41, -400, 36, 40, 41, 42, 43, 45, 46, 80, 81, 400, 401, 4001, 40000, 400001, 4_000_000,
    ] {
        let angle = f64::from(quarters) * 90.0;
        for mode in [None, Some(BlendMode::Multiply), Some(BlendMode::Difference)] {
            let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
            if let Some(mode) = mode {
                c.blend_mode = mode;
            }
            c.transform.rotation = angle;
            let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
            let case = format!("checker/e/r3/many turns {quarters} quarters {mode:?}");
            let geom = r3_geometry(&tl, &assets, 30.0);
            let scan = ce_scan(&case, &tl, &assets, &at_fps(30.0), 0..12, false);
            eprintln!("{}", scan.line(&case));
            tally.push(format!("{case}: {geom}: drawn {} of {}", scan.drawn, scan.frames));
            if let Some(why) = scan.verdict(None) {
                failures.push(format!("{case}: {geom}: {why}"));
            }
        }
    }
    for t in &tally {
        eprintln!("TALLY {t}");
    }
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// The short one (what a scratch edit of the edge shift is checked against): a picture with detail at its edges, its opacity keyed, turned by each
/// quarter, plain and blended, at the first frames where the alpha is a level of its own.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_keyed_opacity_edge_on_each_quarter() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures = Vec::new();
    let mut known = Vec::new();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    for angle in [90.0, 180.0, 270.0, -90.0] {
        for mode in [None, Some(BlendMode::Multiply)] {
            let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
            if let Some(mode) = mode {
                c.blend_mode = mode;
            }
            c.transform.rotation = angle;
            c.transform.pos_x = 0.0123;
            r3_keyed(&mut c, 0.3);
            r3_check(
                &format!("checker/e/r3/edge quarter {angle} {mode:?}"),
                &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
                None,
                &assets,
                30.0,
                0.5,
                Some(true),
                &mut failures,
                &mut known,
            );
        }
    }
    r3_finish("checker_e_r3_a_keyed_opacity_edge_on_each_quarter", failures, &known);
}
