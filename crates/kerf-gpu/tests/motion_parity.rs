//! Motion parity: the export graph's own frames against the compositor's, frame by frame.
//!
//! `tests/parity.rs` holds the compositor to FFmpeg's *still*; this holds it to the *export*: every
//! case builds a short cut, renders it losslessly through `kerf_core::render_with` (the real
//! `build_filter_complex`, `xfade`-free: fades, dips, alpha ramps and `overlay` travel), reads every
//! frame back as `yuv420p` **planes** (no colour conversion in between: the planes `overlay` blended
//! are the ones compared) and draws the same output frames from a [`PlanMode::Motion`] plan — the
//! pick of each source frame is the `fps` filter's, served by one [`MotionFrames`] cursor per clip.
//! The comparison is plane for plane, sample for sample, with **no tolerance and no bound a case can name**: a frame the plan lets the
//! compositor draw is the export's with 0 differing samples in Y, U and V, or the case fails; the RGB montage `FFmpeg | GPU | diff x8` of the frames worth a look goes to `target/parity/motion/`
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
//! Every drawn frame agrees **to the sample**, a resize included (the compositor's scaler is swscale's own, measured: a plan refuses a
//! resize where it is not): `fade` is integer
//! arithmetic ([`kerf_core::fade_luma`], `fade_chroma`, `fade_alpha`: the filter is identical on 6.1.1 and 9.0.2,
//! checked by [`fade_arithmetic_is_ffmpegs`] on every factor; what FFmpeg 9 does around it to a full-range
//! picture is not, and is refused), the dissolve is `overlay`'s own
//! `FAST_DIV255` on the ramped alpha, a slide is a position the overlay truncates, a tail is a
//! clip that plays on. What the compositor does not draw to the sample (a rotated edge, a turn of a translucent layer, a blend with
//! an opacity) is refused by the plan, and the case that says so draws the frames anyway to show the refusal is needed.

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
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::convert::kernels();
        // ... and how its `geq` reads a plane's edge, which a mask and a keyed opacity need (a render only reads it).
        let _ = kerf_core::geq_edge();
        // ... and whether this FFmpeg converts a 4:4:4 / 4:2:2 picture in a graph as the decode does (a placed one).
        let _ = kerf_gpu::conversion::conversion();
        // ... and whether this FFmpeg places a picture as the model does (the moving zoom and position are in the caps only then).
        let _ = kerf_gpu::placement_probe::placement();
        // ... and whether it takes a translucent layer through RGB, and blurs, as the model does (a layer below full opacity and a blur are in the caps only then).
        let _ = kerf_gpu::roundtrip_probe::translucency();
        let _ = kerf_gpu::blur_probe::blur();
        let _ = kerf_gpu::display_probe::display();
        // ... and so is the layout of its `drawtext`, which decides whether text whose bitmaps share samples is drawn.
        let _ = kerf_gpu::text::model();
        // ... and for which kinds of picture, in which chains, a moving still is drawn as this FFmpeg draws it (every chain: the cases below take
        // them all, where the app measures what a plan needs).
        let _ = kerf_gpu::still_probe::all();
        // ...and the blend chain's, which the blend modes are in the caps by.
        let _ = kerf_gpu::blend::kernels();
        // ...and whether its `overlay` of a floating title is the model (a title is in the caps only then).
        assert_eq!(
            kerf_gpu::svg::svg(),
            kerf_gpu::svg::Overlay::Match,
            "this FFmpeg's overlay of a title is not the model the compositor blends with"
        );
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
            // Written beside it and renamed over it: `checker_a_a_placed_clip_in_an_export_in_parts` runs this binary again,
            // whose `media()` makes these files while the sibling tests read them, and a file rewritten in place is seen
            // truncated and half-written, then with the size and time of nothing that was probed (a rename leaves a reader
            // the file it opened and gives the next one the whole new one).
            let part = dir.join(format!("{name}.{}.part.mp4", std::process::id()));
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
                part.to_str().unwrap(),
            ]);
            std::fs::rename(&part, &p).expect("move the finished clip into place");
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

// Every drawn frame is held to the export with **no tolerance**: 0 differing samples in Y, U and V. There is no bound a case can name.

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
        assert_eq!(a.len(), b.len(), "the planes are not the same size");
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
    /// Frames to write a montage of (and every frame that differs).
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
        if my > 0 || muv > 0 {
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
            eprintln!("WOULD FAIL: {case} frame {k}: Y {y} (max 0), UV {uv} (max 0)");
        }
        return (ys.max, uv.max);
    }
    assert!(
        worst.is_empty(),
        "{case}: frames that differ from the export (Y 0, UV 0): {:?} (montages in target/parity/motion)",
        &worst[..worst.len().min(12)]
    );
    (ys.max, uv.max)
}

/// A moving zoom or position: the export places the picture on the canvas at sub-pixel positions
/// (a `perspective`), which the compositor reproduces (`kerf_core::perspective`, `placement.wgsl`): exact,
/// every plane of every frame.
fn run_placed(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) {
    run(case, tl, assets, fps, secs);
}

/// A plan the compositor must refuse, for `reason`, at every frame of the range — with FFmpeg's
/// export unaffected (the case builds it all the same, so the graph is known to render).
fn refused(case: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions, frames: Range<u64>, reason: &str) {
    let planner = Planner::new(tl, assets, opts, PlanRequest::motion(policy())).expect("planner");
    let mut refusals = 0;
    let mut other = Vec::new();
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
        } else {
            other = why;
        }
    }
    assert!(
        refusals > 0,
        "{case}: no frame was refused for `{reason}` (the last frame's reasons: {other:?})"
    );
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

fn run(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) {
    check_motion(Check {
        case,
        tl,
        assets,
        opts: at_fps(fps),
        frames: upto(secs, fps),
        look: &look_at(secs, fps),
        refusing: None,
    });
}

/// [`run`] over the output frames `frames`.
fn run_range(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Range<u64>) {
    check_motion(Check {
        case,
        tl,
        assets,
        opts: at_fps(fps),
        frames,
        look: &[],
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
fn run_refusing(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, reason: &Refusing) {
    check_motion(Check {
        case,
        tl,
        assets,
        opts: at_fps(fps),
        frames: upto(secs, fps),
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
        run(&format!("plain/cut @{fps}"), &tl, &assets, fps, 2.0);
    }
    // A 25 fps source in a 30 fps cut, and the reverse.
    let tl = timeline(
        vec![vec![clip(&m.testsrc25, 0.5, 1.5, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0)]],
        None,
    );
    let assets = [m.testsrc25.clone(), m.testsrc.clone()];
    run("plain/25-then-30 @30", &tl, &assets, 30.0, 2.0);
    run("plain/25-then-30 @25", &tl, &assets, 25.0, 2.0);
    // Speed, in both directions, and reversed.
    let mut fast = clip(&m.testsrc, 0.5, 2.5, 0.0);
    fast.speed = 2.0;
    let mut slow = clip(&m.testsrc, 0.5, 1.0, 1.0);
    slow.speed = 0.5;
    let mut back = clip(&m.testsrc, 0.5, 1.5, 2.0);
    back.speed = -1.0;
    let tl = timeline(vec![vec![fast, slow, back]], None);
    let assets = [m.testsrc.clone()];
    run("plain/speed-2-0.5-reversed", &tl, &assets, 30.0, 3.0);
    run("plain/speed-2-0.5-reversed @24", &tl, &assets, 24.0, 3.0);
    // A still image is a stream like another one in the export.
    let tl = timeline(
        vec![vec![clip(&m.image, 0.0, 1.0, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0)]],
        None,
    );
    let assets = [m.image.clone(), m.testsrc.clone()];
    run("plain/still-image", &tl, &assets, 30.0, 2.0);
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
    run("plain/pip-over-graded", &tl, &assets, 30.0, 1.2);
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
        run(&format!("late/head @{fps}"), &tl, &assets, fps, 1.5);
    }
    let (tl, assets) = over(&|c| c.source_in = 0.15);
    run("late/inside-the-lead", &tl, &assets, 30.0, 1.5);
    let (tl, assets) = over(&|c| c.source_in = 0.6);
    run("late/past-the-lead", &tl, &assets, 30.0, 0.9);
    let (tl, assets) = over(&|c| {
        c.source_out = 3.0;
        c.speed = 2.0;
    });
    run("late/speed-2", &tl, &assets, 30.0, 1.5);
    let (tl, assets) = over(&|c| {
        c.source_out = 0.75;
        c.speed = 0.5;
    });
    run("late/speed-0.5", &tl, &assets, 30.0, 1.5);
    let (tl, assets) = over(&|c| c.speed = -1.0);
    run("late/reversed", &tl, &assets, 30.0, 1.5);
    // A fade-in from the first picture, at 30 fps and at 25, where the 0.3 s lead is half a frame.
    for fps in [30.0, 25.0] {
        let (tl, assets) = over(&|c| c.fade_in = 0.4);
        run(&format!("late/fade-in @{fps}"), &tl, &assets, fps, 1.5);
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
            run(&format!("late/{name} @{fps}"), &tl, &assets, fps, 2.0);
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
        run(&format!("fade/in+out @{fps}"), &tl, &assets, fps, 1.0);
    }
    // A fade of a frame, of an odd length, and longer than the clip (clamped to it).
    let (tl, assets) = one(1.0 / 30.0, 0.0333, &|_| {});
    run("fade/one-frame", &tl, &assets, 30.0, 1.0);
    let (tl, assets) = one(0.61, 0.0, &|_| {});
    run("fade/0.61s @30", &tl, &assets, 30.0, 1.0);
    let (tl, assets) = one(5.0, 0.0, &|_| {});
    run("fade/longer-than-the-clip", &tl, &assets, 30.0, 1.0);
    // The fade-in and the fade-out overlap on a clip shorter than the two.
    let (tl, assets) = one(0.8, 0.8, &|_| {});
    run("fade/overlapping-in-and-out", &tl, &assets, 30.0, 1.0);
    // Under a graded, a sped-up and a reversed clip: the fade is the last stage but the pick.
    let (tl, assets) = one(0.4, 0.3, &|c| {
        c.color = Color {
            brightness: 0.1,
            contrast: 1.2,
            ..Color::default()
        }
    });
    run("fade/graded", &tl, &assets, 30.0, 1.0);
    let (tl, assets) = one(0.3, 0.3, &|c| {
        c.source_out = 2.5;
        c.speed = 2.0;
    });
    run("fade/speed-2", &tl, &assets, 30.0, 1.0);
    let (tl, assets) = one(0.3, 0.3, &|c| c.speed = -1.0);
    run("fade/reversed", &tl, &assets, 30.0, 1.0);
    // A smaller layer over another: the fade is the layer's, the layer below is not touched.
    let under = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut over = clip(&m.testsrc, 0.5, 1.5, 0.0);
    over.transform.scale = 0.5;
    over.fade_in = 0.4;
    over.fade_out = 0.4;
    let tl = timeline(vec![vec![under], vec![over]], None);
    let assets = [m.bars.clone(), m.testsrc.clone()];
    run("fade/small-layer-over-another", &tl, &assets, 30.0, 1.0);
    // A letterboxed layer: `pad`'s bars are in the picture that fades.
    let mut tall = clip(&m.portrait, 0.5, 1.5, 0.0);
    tall.fade_in = 0.4;
    tall.fade_out = 0.4;
    let tl = timeline(vec![vec![tall]], Some(Delivery::new(640, 360, Fit::Contain)));
    let assets = [m.portrait.clone()];
    run("fade/letterboxed", &tl, &assets, 30.0, 1.0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn dips_are_drawn_as_the_export_draws_them() {
    let m = media();
    for fps in [24.0, 25.0, 29.97, 30.0, 60.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::DipToBlack), 0.8, 1.0);
        run(&format!("dip/black covering @{fps}"), &tl, &assets, fps, 2.0);
    }
    // The incoming clip smaller than the frame: the outgoing one fades out under nothing.
    let (tl, assets) = pair(m, Some(TransitionKind::DipToBlack), 0.8, 0.5);
    run("dip/black partial", &tl, &assets, 30.0, 2.0);
    // A dip of a frame, of an odd length and one longer than either clip.
    for (name, secs) in [("one-frame", 1.0 / 30.0), ("0.61s", 0.61), ("2s", 2.0)] {
        let (tl, assets) = pair(m, Some(TransitionKind::DipToBlack), secs, 1.0);
        run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0);
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
    run("dip/black with the clips' own fades", &tl, &assets, 30.0, 2.0);
    let base = clip(&m.bars, 0.5, 1.5, 0.0);
    let (over, assets) = pair(m, Some(TransitionKind::DipToBlack), 0.6, 0.6);
    let mut tracks = vec![vec![base, clip(&m.bars, 0.5, 1.5, 1.0)]];
    tracks.extend(over.tracks.iter().map(|t| t.clips.clone()));
    let tl = timeline(tracks, None);
    run("dip/black over another track", &tl, &assets, 30.0, 2.0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn dissolves_are_drawn_as_the_export_draws_them() {
    let m = media();
    for fps in [24.0, 25.0, 29.97, 30.0, 60.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61, 1.0);
        run(&format!("dissolve/covering @{fps}"), &tl, &assets, fps, 2.0);
    }
    // The incoming clip smaller than the frame: the outgoing clip plays on around it, past the ramp.
    for fps in [24.0, 30.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61, 0.5);
        run(&format!("dissolve/partial @{fps}"), &tl, &assets, fps, 2.0);
    }
    for (name, secs) in [("one-frame", 1.0 / 30.0), ("0.5s", 0.5), ("1s", 1.0)] {
        let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), secs, 1.0);
        run(&format!("dissolve/{name}"), &tl, &assets, 30.0, 2.0);
    }
    // Up from black: no clip before it to cross.
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 0.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![inc]], None);
    run("dissolve/from-black", &tl, std::slice::from_ref(&m.testsrc), 30.0, 1.0);
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
    run("dissolve/chained", &tl, &assets, 30.0, 3.0);
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
    );
    // No handle to borrow (the outgoing clip ends on the last frame of its footage): a hard cut.
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![clip(&m.bars, 3.0, 4.0, 0.0), inc]], None);
    run("dissolve/no-handle", &tl, &[m.bars.clone(), m.testsrc.clone()], 30.0, 2.0);
    // Over another track: the track below shows through the ramp and around the layer.
    let base = clip(&m.testsrc25, 0.5, 2.5, 0.0);
    let (over, mut assets) = pair(m, Some(TransitionKind::Crossfade), 0.6, 0.6);
    let mut tracks = vec![vec![base]];
    tracks.extend(over.tracks.iter().map(|t| t.clips.clone()));
    let tl = timeline(tracks, None);
    assets.push(m.testsrc25.clone());
    run("dissolve/over-another-track", &tl, &assets, 30.0, 2.0);
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
        for (cover, scale) in [("covering", 1.0), ("partial", 0.5)] {
            let (tl, assets) = pair(m, Some(kind), 0.5, scale);
            run(&format!("{}/{cover}", kind.as_str()), &tl, &assets, 30.0, 2.0);
        }
    }
    // The other rates, an odd length, a frame, and a travel longer than the frame can show.
    for fps in [24.0, 25.0, 29.97, 60.0] {
        for kind in [TransitionKind::SlideLeft, TransitionKind::PushUp] {
            let (tl, assets) = pair(m, Some(kind), 0.61, 1.0);
            run(&format!("{}/0.61s @{fps}", kind.as_str()), &tl, &assets, fps, 2.0);
        }
    }
    for (name, secs) in [("one-frame", 1.0 / 30.0), ("1s", 1.0)] {
        for kind in [TransitionKind::SlideDown, TransitionKind::PushRight] {
            let (tl, assets) = pair(m, Some(kind), secs, 1.0);
            run(&format!("{}/{name}", kind.as_str()), &tl, &assets, 30.0, 2.0);
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
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn dips_to_white_are_drawn_through_the_rgb_round_trip() {
    let m = media();
    for fps in [24.0, 30.0] {
        let (tl, assets) = pair(m, Some(TransitionKind::DipToWhite), 0.8, 1.0);
        run(&format!("dip/white covering @{fps}"), &tl, &assets, fps, 2.0);
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
    run("dip/white noise", &tl, std::slice::from_ref(&m.noise), 30.0, 2.0);
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
    // (The scaler is swscale's to the sample, and so is the grade on it.)
    for (name, graded) in [("ungraded", false), ("graded", true)] {
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
    );

    // A title over a dissolve: the text is on `between(t, start, end)` in an export, end included.
    let (mut tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.5, 1.0);
    let mut title = kerf_core::TextOverlay::new("Summer 2026", 0.5, 1.5);
    title.bg = Some("black@0.5".into());
    tl.overlays.push(title);
    run("text/over a dissolve", &tl, &assets, 30.0, 2.0);
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
        run(&format!("text/{name}, over a fade"), &tl, one, 30.0, 2.0);
    }
    // A caption fading in and sliding up, small and thickened.
    let mut tl = base();
    let mut o = kerf_core::TextOverlay::new("Sliding, fading: fi ffi", 0.1, 1.9);
    o.size = 10.0 / 360.0;
    o.keyframes = vec![key(0.0, 0.9, 0.0), key(0.5, 0.8, 1.0), key(1.7, 0.8, 1.0)];
    tl.overlays.push(o);
    run("text/small, sliding and fading", &tl, one, 30.0, 2.0);
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
                run(&format!("text/bold-border {name}, over a fade"), &tl, one, 30.0, 2.0);
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
    // (The keys hold a scale of 0.5, which is a `scale` ahead of the placement: the compositor's scaler is in
    // play, swscale's to the sample. A scale that moves is the placement's own: `placement/*`.)
    run("keyed/position", &tl, &[m.black.clone(), m.testsrc.clone()], 30.0, 1.0);
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
        run(
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
        run(&format!("keyed opacity/{name}"), &tl, &assets, 30.0, 1.0);
    }
    // At 29.97 the clip's time is another grid of doubles.
    let tl = over_the_bars(fading(&m.testsrc, vec![key(0.0, 1.0), key(1.0, 0.0)]));
    run("keyed opacity/29.97", &tl, &assets, 30000.0 / 1001.0, 1.0);
    // Through the clip's fades and the dissolve it enters by: both act on the `geq`'s alpha plane.
    let mut faded = fading(&m.testsrc, vec![key(0.0, 0.4), key(1.0, 0.9)]);
    faded.fade_in = 0.3;
    faded.fade_out = 0.3;
    run("keyed opacity/with fades", &over_the_bars(faded), &assets, 30.0, 1.0);
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
    );
    // Scaled: the scaler's level, as for any scaled layer; moving, it is placed by the export's `perspective`,
    // which acts on the faded picture's alpha plane: refused while the opacity is below 1.
    let mut pip = fading(&m.testsrc, vec![key(0.0, 0.2), key(1.0, 0.9)]);
    pip.transform.scale = 0.6;
    pip.set_property_keys(Property::PosX, vec![key(0.0, -0.2), key(1.0, 0.2)]);
    refused(
        "keyed opacity/moving pip",
        &over_the_bars(pip),
        &assets,
        &at_fps(30.0),
        upto(1.0, 30.0),
        "an opacity below 1",
    );
    let mut odd = fading(&m.testsrc, vec![key(0.0, 0.5), key(1.0, 0.7)]);
    odd.transform.scale = 0.33;
    run("keyed opacity/odd 211x118", &over_the_bars(odd), &assets, 30.0, 1.0);
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
fn run_colour(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, moves: usize) {
    colour_moves(case, tl, assets, fps, secs, moves);
    run(case, tl, assets, fps, secs);
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
/// FFmpeg's and the others are held to the export at `limits` (on a machine whose scaler is not measured exact; where it is, every frame is drawn and exact). `refused` is how many frames that is (`None`: some, not all).
fn run_grade_steep(case: &str, tl: &Timeline, assets: &[Asset], secs: f64, refused: Option<usize>) {
    let (n, drawn) = grade_steep_split(tl, assets, 30.0, secs);
    // Where the scaler is measured to be swscale's there is no level for the tables to multiply: every frame is drawn, to the level.
    if kerf_gpu::scaler::kernels().exact() {
        assert_eq!(n, 0, "{case}: {n} frames refused for GradeSteep with an exact scaler");
        run(case, tl, assets, 30.0, secs);
        return;
    }
    match refused {
        Some(want) => assert_eq!(n, want, "{case}: {n} frames refused, {drawn} drawn"),
        None => assert!(n > 0 && drawn > 0, "{case}: {n} frames refused, {drawn} drawn"),
    }
    run_refusing(case, tl, assets, 30.0, secs, &grade_steep_refused());
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
            );
        }
        run_colour(
            &format!("keyed colour/all five, {name}"),
            &alone(colour_keyed(&m.noise, &all, shape.as_ref())),
            noise,
            30.0,
            1.0,
            *moves,
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
    );
    run_colour(
        "keyed colour/temperature crossing 0 between two",
        &alone(crossing(-0.6, 0.3, 0.9)),
        noise,
        30.0,
        1.0,
        20,
    );
    run_colour(
        "keyed colour/temperature 1 to 0",
        &alone(crossing(1.0, 0.0, 1.0)),
        noise,
        30.0,
        1.0,
        20,
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
        );
    }
    // Late on the timeline, sped up, slowed down, reversed: only the clip's own clock reads the curve.
    let mut late = colour_keyed(&m.noise, &all, &linear_keys);
    late.timeline_start = 1.25;
    run_colour("keyed colour/late on the timeline", &alone(late), noise, 30.0, 2.25, 20);
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
        );
    }
    let mut fast = colour_keyed(&m.noise, &all, &linear_keys);
    (fast.source_out, fast.speed, fast.timeline_start) = (2.5, 2.0, 0.4);
    run_colour("keyed colour/speed 2", &alone(fast), noise, 30.0, 1.4, 20);
    let mut slow = colour_keyed(&m.noise, &all, &linear_keys);
    (slow.source_out, slow.speed) = (1.0, 0.5);
    run_colour("keyed colour/speed 0.5", &alone(slow), noise, 30.0, 1.0, 20);
    let mut back = colour_keyed(&m.noise, &all, &linear_keys);
    back.speed = -1.0;
    run_colour("keyed colour/reversed", &alone(back), noise, 30.0, 1.0, 20);
    // Beside a static grade: the numbers that are not keyed are the clip's own, to the last digit.
    let mut beside = colour_keyed(&m.noise, &[Property::Brightness, Property::Gamma], &linear_keys);
    beside.color = Color {
        contrast: 1.3,
        saturation: 0.8,
        temperature: -0.35,
        ..Color::default()
    };
    run_colour("keyed colour/beside a static grade", &alone(beside), noise, 30.0, 1.0, 20);
    let mut warm = colour_keyed(&m.noise, &[Property::Contrast], &linear_keys);
    warm.color.temperature = 0.4;
    run_colour("keyed colour/beside a static temperature", &alone(warm), noise, 30.0, 1.0, 20);

    // The clip's fades and the dissolve it enters by act after the `eq`.
    let mut faded = colour_keyed(&m.noise, &all, &linear_keys);
    faded.fade_in = 0.3;
    faded.fade_out = 0.3;
    run_colour("keyed colour/with fades", &alone(faded), noise, 30.0, 1.0, 20);
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
    );
    // Beside a keyed opacity: one clip, an `eq` and a `geq` alpha each written per frame.
    let over_the_bars = |c: Clip| timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
    let two = [m.bars.clone(), m.testsrc.clone()];
    let mut both = colour_keyed(&m.testsrc, &all, &linear_keys);
    both.set_property_keys(Property::Opacity, linear_keys(1.0, 0.4));
    run_colour("keyed colour/with a keyed opacity", &over_the_bars(both), &two, 30.0, 1.0, 20);
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
    );
    // A layer the compositor scales: where the scaler is not measured to be swscale's it is a level off the x86 kernels, and the
    // grade's tables multiply that by their steepest step (`eq::table_steps`: contrast, gamma's power toward black, a black point
    // parked above 0), so there a frame is drawn while that step is at most two and FFmpeg's past it (`GradeSteep`). A keyed ramp is
    // judged one frame at a time either way: the frames the plan lets through are held to the export, the others are refused (and
    // the export renders them all the same).
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
    run_colour("keyed colour/pip, gentle", &over_the_bars(gentle_pip), &two, 30.0, 1.0, 20);
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
        Some(1),
    );
    // The far ends of every range: the early frames of the ramp (contrast 0.5, saturation 0, gamma 0.5) are inside, the rest steepen.
    let mut steep_pip = colour_keyed(&m.testsrc, &all, &linear_keys);
    moving(&mut steep_pip);
    run_grade_steep("keyed colour/pip, steep", &over_the_bars(steep_pip), &two, 1.0, None);
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
    );
    let boxed = colour_keyed(&m.portrait, &all, &linear_keys);
    run_grade_steep(
        "keyed colour/letterboxed, steep",
        &timeline(vec![vec![boxed]], delivery),
        std::slice::from_ref(&m.portrait),
        1.0,
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
    run_refusing("keyed colour/rotated", &on_black, &smooth, 30.0, 1.0, &rotation_refused());
    let mut translucent = colour_keyed(&m.testsrc, &all, &linear_keys);
    translucent.transform.opacity = 0.6;
    run_colour("keyed colour/translucent", &over_the_bars(translucent), &two, 30.0, 1.0, 20);
    // The tables of the frame ahead of the effects that follow: composed with a grayscale or an invert, and
    // ahead of a blur.
    for (name, effects) in [
        ("invert", vec![kerf_core::VideoEffect::Invert]),
        (
            "grayscale then invert",
            vec![kerf_core::VideoEffect::Grayscale, kerf_core::VideoEffect::Invert],
        ),
        (
            "blur then grayscale",
            vec![kerf_core::VideoEffect::Blur { sigma: 3.0 }, kerf_core::VideoEffect::Grayscale],
        ),
        ("sharpen", vec![kerf_core::VideoEffect::Sharpen { amount: 1.0 }]),
        ("vignette", vec![kerf_core::VideoEffect::Vignette]),
    ] {
        let mut c = colour_keyed(&m.testsrc, &all, &linear_keys);
        c.effects = effects;
        run_colour(&format!("keyed colour/then {name}"), &over_the_bars(c), &two, 30.0, 1.0, 20);
    }
    // A mask is cut after the grade.
    let mut masked = colour_keyed(&m.testsrc, &all, &linear_keys);
    masked.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.1, false));
    run_colour("keyed colour/under a mask", &over_the_bars(masked), &two, 30.0, 1.0, 20);
    // A moving zoom comes last: the grade is at the fit size, magnified with the picture.
    let mut zoomed = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
    for prop in all {
        zoomed.set_property_keys(prop, linear_keys(colour_span(prop).0, colour_span(prop).1));
    }
    let tl = over_the_bars(zoomed);
    run_placed("keyed colour/under a moving zoom", &tl, &two, 30.0, 1.0);
}

/// `eq`'s tables on a layer the scaler resizes. The scaler used to be a level off swscale's and the tables multiplied that by their
/// steepest step (`eq::table_steps`), so a frame was drawn while that step was at most two and FFmpeg's past it (`GradeSteep`), static
/// grades included. Where the scaler is measured to be swscale's to the level (both FFmpegs of CI on x86-64: `scaler::kernels()`)
/// there is no level to multiply and **every grade is drawn to the level, whatever its tables step**: held on sources that look at
/// every table entry (noise, a luma ramp, hard 16 / 235 edges whose scaled overshoot reaches the bottom of the range) with the grades
/// that measured 3 to 19 levels off the export before (gamma 1.3 3 / 1, 1.4 4 / 1, 2 6 / 1; contrast 3 3 / 1; contrast 1.8 and gamma 2
/// 12 / 1, with a brightness of 0.3 19 / 1; saturation 3 1 / 3).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn eq_tables_of_any_step_are_the_export_on_a_layer_the_scaler_resizes() {
    use kerf_core::eq::table_steps;
    let m = media();
    assert!(kerf_gpu::scaler::kernels().exact(), "this FFmpeg's scaler is not the model");
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
    let mut steepest = 0;
    for (name, color, sources) in [
        // Fixed point: contrast steps by its ceiling and a brightness has no cliff.
        ("brightness 0.3", grade(0.3, 1.0, 1.0, 1.0, 0.0), vec![ramp, edges]),
        ("contrast 2", grade(0.0, 2.0, 1.0, 1.0, 0.0), vec![ramp, edges]),
        ("contrast 3", grade(0.0, 3.0, 1.0, 1.0, 0.0), vec![ramp, edges, noise]),
        // The pow table: a gamma above 1 steepens toward black; a brightness that lifts the dark end off the table's cliff
        // takes the steepness away again (gamma 2, step 1).
        ("gamma 1.2", grade(0.0, 1.0, 1.0, 1.2, 0.0), vec![ramp, edges, noise]),
        ("gamma 1.3", grade(0.0, 1.0, 1.0, 1.3, 0.0), vec![ramp, edges, noise]),
        ("gamma 1.4", grade(0.0, 1.0, 1.0, 1.4, 0.0), vec![ramp, edges, noise]),
        ("gamma 2", grade(0.0, 1.0, 1.0, 2.0, 0.0), vec![ramp, edges, noise]),
        ("gamma 0.5", grade(0.0, 1.0, 1.0, 0.5, 0.0), vec![ramp]),
        (
            "brightness -0.3 and gamma 1.5",
            grade(-0.3, 1.0, 1.0, 1.5, 0.0),
            vec![ramp, edges],
        ),
        (
            "contrast 1.8 and gamma 2",
            grade(0.0, 1.8, 1.0, 2.0, 0.0),
            vec![ramp, edges, noise],
        ),
        (
            "brightness 0.3 and gamma 2",
            grade(0.3, 1.0, 1.0, 2.0, 0.0),
            vec![ramp, edges],
        ),
        // Chroma: saturation is the tables' contrast, a temperature a power of the chroma.
        ("saturation 2", grade(0.0, 1.0, 2.0, 1.0, 0.0), vec![noise, ramp]),
        ("saturation 3", grade(0.0, 1.0, 3.0, 1.0, 0.0), vec![noise, ramp]),
        ("temperature 1", grade(0.0, 1.0, 1.0, 1.0, 1.0), vec![noise]),
        (
            "the far end of every range",
            grade(0.3, 1.8, 2.0, 2.0, 1.0),
            vec![ramp, edges, noise],
        ),
    ] {
        let [y, u, v] = table_steps(&color);
        steepest = steepest.max(y.max(u).max(v));
        for (source, asset) in sources {
            let (tl, assets) = pip(asset, color);
            run(&format!("static colour/pip, {name}, {source}"), &tl, &assets, 30.0, 0.4);
        }
    }
    assert!(
        steepest > 2,
        "the cases must reach tables the plan used to refuse (steepest {steepest})"
    );
    // The same far end at the picture's size is exact (nothing is resized).
    let mut alone = clip(&m.ramp, 0.5, 1.5, 0.0);
    alone.color = grade(0.3, 1.8, 2.0, 2.0, 1.0);
    run(
        "static colour/the far end of every range, unscaled",
        &timeline(vec![vec![alone]], None),
        std::slice::from_ref(&m.ramp),
        30.0,
        0.4,
    );
}

/// A clip's grade (LUT, curves, wheels) on a layer the scaler resizes, **with an `eq` ahead of it**: the picture the conversions to RGB
/// and back read is the exact scaler's, so the whole is the export's to the level — a picture-in-picture, an enlargement, a shrink, a
/// Cover crop, noise, a picture tagged BT.709 (through 4:4:4 on the way back) — the T8 shape of `tests/coverage.rs` (a 2:1 shrink under
/// contrast, saturation and warmth, curves and wheels). Without an `eq` FFmpeg's format negotiation has the fit `scale` output RGB
/// itself (the filters between it and the grade pass any format and the grade asks for RGB), which scales and converts in one pass from
/// chroma the compositor has already scaled: refused, and shown necessary by drawing the frame anyway.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_grade_on_a_layer_the_scaler_resizes_is_the_export_where_an_eq_precedes_it() {
    let m = media();
    let v = variants();
    assert!(kerf_gpu::scaler::kernels().exact(), "this FFmpeg's scaler is not the model");
    let all = grades();
    let eq = Color {
        contrast: 1.08,
        saturation: 1.15,
        temperature: 0.1,
        ..Color::default()
    };
    // Every part of the grade and their sums, picture-in-picture over another track.
    for (name, grade) in &all {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.transform.scale = 0.6;
        c.grade = grade.clone();
        c.color = eq;
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
        run(
            &format!("grade/pip with eq, {name}"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            30.0,
            0.4,
        );
    }
    let heavy = &all[8].1;
    let alone = |asset: &Asset, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(asset, 0.5, 1.5, 0.0);
        c.grade = heavy.clone();
        c.color = eq;
        edit(&mut c);
        (timeline(vec![vec![c]], delivery), vec![asset.clone()])
    };
    // The shapes of a resize.
    for (name, delivery) in [
        ("enlarged to 960x540", Delivery::new(960, 540, Fit::Contain)),
        ("shrunk to 320x180 (the 2:1 of T8)", Delivery::new(320, 180, Fit::Contain)),
        ("shrunk to 160x90", Delivery::new(160, 90, Fit::Contain)),
        ("cover into 360x640", Delivery::new(360, 640, Fit::Cover)),
        ("cover into 480x270", Delivery::new(480, 270, Fit::Cover)),
        ("enlarged cover into 1280x720", Delivery::new(1280, 720, Fit::Cover)),
    ] {
        let (tl, assets) = alone(&m.testsrc, Some(delivery), &|_| {});
        run(&format!("grade/{name} with eq"), &tl, &assets, 30.0, 0.4);
    }
    let (tl, assets) = alone(&m.noise, Some(Delivery::new(960, 540, Fit::Contain)), &|_| {});
    run("grade/noise enlarged with eq", &tl, &assets, 30.0, 0.4);
    let (tl, assets) = alone(&m.noise, Some(Delivery::new(320, 180, Fit::Contain)), &|_| {});
    run("grade/noise shrunk with eq", &tl, &assets, 30.0, 0.4);
    let (tl, assets) = alone(&m.edges, Some(Delivery::new(320, 180, Fit::Contain)), &|_| {});
    run("grade/hard edges shrunk with eq", &tl, &assets, 30.0, 0.4);
    // After the eq's own keys, at speed 2, cropped, over another track, dissolving in and out.
    let (tl, assets) = alone(&m.testsrc, Some(Delivery::new(320, 180, Fit::Contain)), &|c| c.speed = 2.0);
    run("grade/shrunk at speed 2 with eq", &tl, &assets, 30.0, 0.4);
    let (tl, assets) = alone(&m.testsrc, None, &|c| {
        c.transform.scale = 0.75;
        c.transform.crop_left = 0.125;
        c.transform.crop_right = 0.125;
    });
    run("grade/cropped and shrunk with eq", &tl, &assets, 30.0, 0.4);
    let mut keyed = colour_keyed(
        &m.testsrc,
        &[kerf_core::Property::Brightness, kerf_core::Property::Saturation],
        &linear_keys,
    );
    keyed.transform.scale = 0.6;
    keyed.grade = heavy.clone();
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![keyed]], None);
    run_colour(
        "grade/pip under a keyed eq",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        30.0,
        1.0,
        10,
    );
    for (name, kind) in [("in", TransitionKind::Crossfade), ("dip", TransitionKind::DipToBlack)] {
        let mut from = clip(&m.testsrc, 0.5, 1.5, 0.0);
        from.grade = heavy.clone();
        from.color = eq;
        let mut to = clip(&m.testsrc, 1.0, 2.0, 1.0);
        to.grade = heavy.clone();
        to.color = eq;
        to.transition_in = Some(Transition { kind, duration: 0.6 });
        let tl = timeline(vec![vec![from, to]], Some(Delivery::new(320, 180, Fit::Contain)));
        let case = format!("grade/shrunk, dissolving {name}, with eq");
        if kind == TransitionKind::DipToBlack {
            // A fade to black running on a graded layer is FFmpeg's (the grade leaves RGB the fade runs on).
            run_refusing(
                &case,
                &tl,
                std::slice::from_ref(&m.testsrc),
                30.0,
                2.0,
                &refusing("GradeWith", |u| matches!(u, Unsupported::GradeWith(..))),
            );
        } else {
            run(&case, &tl, std::slice::from_ref(&m.testsrc), 30.0, 2.0);
        }
    }
    // A picture tagged BT.709 goes back to 4:2:0 through 4:4:4; a stack of its own matrix.
    let mut c = clip(&v.testsrc709, 0.5, 1.5, 0.0);
    c.transform.scale = 0.6;
    c.grade = heavy.clone();
    c.color = eq;
    let tl = timeline(vec![vec![clip(&v.bars709, 0.5, 1.5, 0.0)], vec![c]], None);
    run(
        "grade/pip with eq, bt709",
        &tl,
        &[v.bars709.clone(), v.testsrc709.clone()],
        30.0,
        0.4,
    );
    // Refused: no eq between the fit scale and the grade; a letterbox (the bars are the picture the grade sees).
    let no_eq = |asset: &Asset, delivery: Option<Delivery>, scale: f64| {
        let mut c = clip(asset, 0.5, 1.5, 0.0);
        c.grade = heavy.clone();
        c.transform.scale = scale;
        (timeline(vec![vec![c]], delivery), vec![asset.clone()])
    };
    let no_eq_refused = refusing(
        "GradeWith (no eq)",
        |u| matches!(u, Unsupported::GradeWith(_, w) if w.contains("no `eq`")),
    );
    for (name, delivery, scale) in [
        ("pip", None, 0.6),
        ("enlarged", Some(Delivery::new(960, 540, Fit::Contain)), 1.0),
        ("shrunk", Some(Delivery::new(320, 180, Fit::Contain)), 1.0),
    ] {
        let (tl, assets) = no_eq(&m.testsrc, delivery, scale);
        run_refusing(&format!("grade/{name}, no eq"), &tl, &assets, 30.0, 0.4, &no_eq_refused);
        // The refusal is needed: the same frames drawn anyway (the plan is told an `eq` of no effect stands there) are not the export's.
        let mut lied = tl.clone();
        for c in lied.tracks.iter_mut().flat_map(|t| &mut t.clips) {
            c.color.brightness = 1e-6;
        }
        let (dy, duv) = drawn_anyway_planes(&format!("grade/{name}, no eq"), &tl, &lied, &assets);
        assert!(dy > 0 || duv > 0, "grade/{name}, no eq: the refusal is not needed");
    }
    let (tl, assets) = alone(&m.portrait, Some(Delivery::new(640, 360, Fit::Contain)), &|_| {});
    run_refusing(
        "grade/letterbox with eq",
        &tl,
        &assets,
        30.0,
        0.4,
        &refusing("Matte", |u| matches!(u, Unsupported::Matte(..))),
    );
}

/// The planes the compositor draws for `lied` (whose plan has no refusal) against the export of `real`: how far a refused frame's
/// drawing is from FFmpeg's. Nine frames at 30 fps.
fn drawn_anyway_planes(case: &str, real: &Timeline, lied: &Timeline, assets: &[Asset]) -> (i32, i32) {
    let opts = at_fps(30.0);
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let played = export(&format!("anyway-{slug}"), real, assets, &opts);
    let planner = Planner::new(lied, assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let (mut y, mut uv) = (0, 0);
    for k in 0..9u64 {
        let plan = planner.at_frame(k).expect("plan");
        let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let drawn = compositor()
            .composite_yuv_counted(&plan, &picture, motion.counts(), size)
            .unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let want = played.planes(k as usize);
        let got = planes_of(&drawn);
        y = y.max(PlaneStats::of(want[0], got[0]).max);
        uv = uv.max(PlaneStats::of(want[1], got[1]).max.max(PlaneStats::of(want[2], got[2]).max));
    }
    eprintln!("{case:<44} drawn anyway: Y max {y}, UV max {uv}");
    (y, uv)
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
            run_refusing(&case, &tl, one, 30.0, 1.0, &full_range_cut_refused());
        } else {
            run_colour(&case, &tl, one, 30.0, 1.0, 20);
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

/// A blend over a moving zoom or position: the placement ends the clip's chain in `pad`, `perspective` and `crop`, and no case holds the blend's
/// two placements of that picture to FFmpeg's, so every frame of the clip is refused (`BlendMode`) and FFmpeg's; the same clip held still is
/// drawn, to the sample.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blended_clip_whose_zoom_or_position_moves_is_ffmpegs() {
    use kerf_core::BlendMode;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let key = |time: f64, scale: f64, pos_x: f64| kerf_core::Keyframe {
        time,
        scale,
        pos_x,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    let blend_refused = refusing("BlendMode", |u| matches!(u, Unsupported::BlendMode(..)));
    for (what, keys) in [
        ("zoom", vec![key(0.0, 1.0, 0.0), key(1.0, 1.5, 0.0)]),
        ("position", vec![key(0.0, 1.0, -0.1), key(1.0, 1.0, 0.1)]),
        ("zoom and position", vec![key(0.0, 1.0, -0.1), key(1.0, 0.7, 0.1)]),
    ] {
        for mode in [BlendMode::Multiply, BlendMode::Screen] {
            let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
            c.blend_mode = mode;
            c.keyframes = keys.clone();
            let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
            run_refusing(
                &format!("blend/moving {what} {mode:?}"),
                &tl,
                &assets,
                30.0,
                1.0,
                &blend_refused,
            );
        }
    }
    let mut still = clip(&m.testsrc, 0.5, 1.5, 0.0);
    still.blend_mode = BlendMode::Multiply;
    still.keyframes = vec![key(0.0, 1.0, 0.1), key(1.0, 1.0, 0.1)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![still]], None);
    run("blend/held position Multiply", &tl, &assets, 30.0, 1.0);
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
    run_refusing("fade/rotated", &tl, &assets, 30.0, 1.0, &rotation_refused());
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
    run_refusing("dissolve/rotated-incoming", &tl, &assets, 30.0, 2.0, &rotation_refused());
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
    );
    // A rotated clip with no fade at all, as an export draws it.
    let mut still_turned = clip(&m.smooth, 0.5, 1.5, 0.0);
    still_turned.transform.rotation = 20.0;
    still_turned.transform.scale = 0.6;
    let tl = timeline(vec![vec![under], vec![still_turned]], None);
    run_refusing("plain/rotated", &tl, &assets, 30.0, 1.0, &rotation_refused());
    let mut quarter = clip(&m.smooth, 0.5, 1.5, 0.0);
    quarter.transform.rotation = 270.0;
    let tl = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0)], vec![quarter]], None);
    run("plain/rotated a quarter", &tl, &assets, 30.0, 1.0);
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
    run("dissolve/translucent-incoming", &tl, &assets, 30.0, 2.0);
    // A fade to black runs on that RGBA picture, alpha included, which the compositor does not
    // reproduce: the plan refuses the frames it runs on (FFmpeg draws them), and the others are drawn.
    let under = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut glass = clip(&m.testsrc, 0.5, 1.5, 0.0);
    glass.transform.opacity = 0.6;
    glass.fade_in = 0.4;
    glass.fade_out = 0.4;
    let tl = timeline(vec![vec![under], vec![glass]], None);
    run_refusing("fade/translucent", &tl, &assets, 30.0, 1.0, &translucent_fade_refused());
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
        &translucent_fade_refused(),
    );
}

/// A layer below full opacity is `colorchannelmixer` (RGBA), `rotate` (on the RGBA picture) and only then the conversion to 4:2:0, whose chroma
/// pairs two pixels of a row and filters over the rows: after a quarter turn it pairs other samples than the round trip the compositor makes
/// ahead of the turn (chroma up to 19 levels off the export in the first frames measured, luma exact). A quarter and three quarters of a turn
/// and a half of a turn are refused for a constant opacity below 1 (the rows are filtered the other way round: one level of chroma on 4,631 samples of 30 frames at 180); the upright layer is exact, and the layers that do not
/// take the round trip are exact turned a quarter: an opaque one (alone, faded in and out), and one whose opacity is keyed (a `geq` alpha on
/// `yuva420p`, pointwise).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_translucent_layer_turned_is_refused_and_the_upright_one_is_drawn() {
    use kerf_core::{Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let under = || clip(&m.bars, 0.5, 1.5, 0.0);
    let on_top = |edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        edit(&mut c);
        timeline(vec![vec![under()], vec![c]], None)
    };
    for angle in [90.0, 180.0, 270.0, -90.0, -180.0, 450.0] {
        for scale in [1.0, 0.5625] {
            let tl = on_top(&|c| {
                c.transform.rotation = angle;
                c.transform.opacity = 0.6;
                c.transform.scale = scale;
            });
            run_refusing(
                &format!("opacity/turned {angle} scale {scale}"),
                &tl,
                &assets,
                30.0,
                1.0,
                &rotation_refused(),
            );
        }
        let tl = on_top(&|c| {
            c.transform.rotation = angle;
            c.transform.opacity = 0.6;
            c.fade_in = 0.4;
            c.fade_out = 0.4;
        });
        run_refusing(
            &format!("opacity/turned {angle} and faded"),
            &tl,
            &assets,
            30.0,
            1.0,
            &rotation_refused(),
        );
        let tl = on_top(&|c| {
            c.transform.rotation = angle;
            c.fade_in = 0.4;
            c.fade_out = 0.4;
        });
        run(&format!("fade/turned {angle}"), &tl, &assets, 30.0, 1.0);
        let tl = on_top(&|c| {
            c.transform.rotation = angle;
            c.set_property_keys(
                Property::Opacity,
                vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 0.3)],
            );
        });
        run(&format!("keyed opacity/turned {angle}"), &tl, &assets, 30.0, 1.0);
    }
    for angle in [0.0, 360.0] {
        let tl = on_top(&|c| {
            c.transform.rotation = angle;
            c.transform.opacity = 0.6;
        });
        run(&format!("opacity/turned {angle}"), &tl, &assets, 30.0, 1.0);
    }
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
/// trip was measured on (8-bit limited-range 4:2:0, untagged or BT.709, to the sample) and
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
    run("dip/white bt709", &tl, &[v.bars709.clone(), v.testsrc709.clone()], 30.0, 2.0);
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
    run("dip/white cropped layer", &tl, &assets, 30.0, 2.0);
    let cover = Some(Delivery::new(480, 360, Fit::Cover));
    let (tl, assets) = after_black(&m.testsrc, white, 0.8, 1.0, cover);
    run("dip/white cover 480x360", &tl, &assets, 30.0, 2.0);
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
            &dip_refused(DipWhy::Resized),
        );
        let (tl, assets) = after_black(&m.testsrc, black, 0.8, scale, format);
        run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0);
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
        &dip_refused(DipWhy::Letterboxed),
    );
    let (tl, assets) = after_black(&m.portrait, black, 0.8, 1.0, tall);
    run("dip/black letterboxed", &tl, &assets, 30.0, 2.0);
    // ... a layer of odd size (0.4985 of 640x360 is 319x179), which a dip to black draws.
    let (tl, assets) = after_black(&m.testsrc, white, 0.8, 0.4985, None);
    run_refusing(
        "dip/white odd layer",
        &tl,
        &assets,
        30.0,
        2.0,
        &dip_refused(DipWhy::OddSize(319, 179)),
    );
    let (tl, assets) = after_black(&m.testsrc, black, 0.8, 0.4985, None);
    run("dip/black odd layer", &tl, &assets, 30.0, 2.0);
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
        run_refusing(&format!("dip/white {name}"), &tl, &assets, 30.0, 2.0, &refused_for);
        let (tl, assets) = after_black(a, black, 0.8, 1.0, None);
        if negotiated {
            run_refusing(
                &format!("dip/black {name}"),
                &tl,
                &assets,
                30.0,
                2.0,
                &full_range_cut_refused(),
            );
        } else {
            run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0);
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
fn run_full_range(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) {
    match policy() {
        CompositeColorPolicy::FixedBt601 => run(case, tl, assets, fps, secs),
        _ => run_refusing(case, tl, assets, fps, secs, &full_range_cut_refused()),
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
        run_full_range(&format!("full-range/fade {name}"), &tl, one, 30.0, 1.0);
        let tl = timeline(vec![vec![clip(a, lo, hi, 0.0)]], None);
        run_full_range(&format!("full-range/plain {name}"), &tl, one, 30.0, 1.0);
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
            run_full_range(&format!("full-range/{label} {name}"), &tl, &[a.clone(), a.clone()], 30.0, 2.0);
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
        );
        let mut fading = clip(a, 0.5, 1.5, 0.0);
        fading.fade_in = 0.5;
        fading.fade_out = 0.5;
        let tl = timeline(vec![vec![fading]], None);
        run_full_range(&format!("full-range/fade {name}"), &tl, std::slice::from_ref(a), 30.0, 1.0);
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
    );
    let tl = timeline(vec![vec![clip(mjpeg, 0.5, 1.5, 0.0), clip(testsrc, 0.5, 1.5, 1.0)]], None);
    run_full_range(
        "full-range/full-range then limited",
        &tl,
        &[testsrc.clone(), mjpeg.clone()],
        30.0,
        2.0,
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
    );
}

// ---- a moving zoom comes last; masks; effects -----------------------------------------------------
//
// The export runs a clip with a moving zoom as `crop, fit, setsar, fps, eq, effects, format=yuva420p,
// mask, rotate, fades`, then places the picture on the canvas at sub-pixel positions (`pad`, a per-frame
// `perspective`, `crop`: `video_clip_chain`). The compositor reproduces that placement
// (`kerf_core::perspective`, `placement.wgsl`), so the cases below are exact: 0 differing samples in every
// plane of every frame (`run_placed`), where the scaler is also in play and with a blur ahead of it too, and
// what is refused (a rotation, an opacity below 1, a dip to white or a grade ahead of the placement).

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
fn a_moving_zoom_is_placed_as_the_export_places_it() {
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
    // A zoom that starts late on the timeline, speed 2, reversed: the pose is the clip's own time, whatever
    // frame of the source the `fps` filter picks for it.
    let mut late = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
    late.timeline_start = 0.5;
    let tl = timeline(vec![vec![late]], None);
    run_range("zoom/late start", &tl, &assets[1..], 30.0, 15..44);
    for (name, speed) in [("speed 2", 2.0), ("reversed", -1.0)] {
        let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.0, 0.0));
        c.source_out = 2.5;
        c.speed = speed;
        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 1.5, 0.0, 0.0)];
        let tl = timeline(vec![vec![c]], None);
        run_range(&format!("zoom/{name}"), &tl, &assets[1..], 30.0, 0..14);
    }
    // A position alone moves (the zoom keys hold one value, so there is no zoom to place), over bars and
    // over black.
    let key = |time: f64, pos_x: f64, pos_y: f64| zoom_key(time, 1.0, pos_x, pos_y);
    let mut moving = clip(&m.testsrc, 0.5, 1.5, 0.0);
    moving.keyframes = vec![key(0.0, -0.3, 0.1), key(1.0, 0.25, -0.15)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![moving]], None);
    run_placed("position/over bars", &tl, &assets, 30.0, 1.0);
    // Cover into a frame the footage is scaled to: the compositor's scaler is in play ahead of the placement,
    // and it is swscale's to the sample where it resizes; the placement adds nothing to it.
    let tl = timeline(
        vec![vec![zooming(&m.testsrc, 1.0, 1.25, (-0.05, 0.0))]],
        Some(Delivery::new(960, 540, Fit::Cover)),
    );
    run("zoom/cover 960x540", &tl, &assets[1..], 30.0, 1.0);
    let tl = timeline(
        vec![vec![zooming(&m.testsrc, 1.0, 1.3, (0.0, 0.0))]],
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    run("zoom/cover 9:16", &tl, &assets[1..], 30.0, 1.0);
    // A still image that moves is drawn by FFmpeg in one `perspective` of a copy of the still scaled once (`StillMotion`), which is
    // not this placement but the moving still's own route (`a_moving_still_is_drawn_as_the_export_draws_it`): drawn from the copy
    // FFmpeg makes.
    let tl = timeline(vec![vec![zooming(&m.image, 1.0, 1.5, (0.0, 0.0))]], None);
    run("zoom/still image (rgb)", &tl, std::slice::from_ref(&m.image), 30.0, 1.0);
}

/// Pictures of other shapes than the frame, at the size the fit leaves them (no resize, so no scaler): a 4:3 clip
/// in a 16:9 frame (pillarboxed: 480x360), a picture of an odd width (203x360) and one of an odd height (640x203).
struct Shapes {
    pillar: Asset,
    odd_width: Asset,
    odd_height: Asset,
    /// 10-bit 4:2:0 tagged limited range: the range is recorded, so nothing about the cut is full range.
    tenbit_tv: Asset,
}

fn shapes() -> &'static Shapes {
    static S: OnceLock<Shapes> = OnceLock::new();
    S.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-shapes");
        std::fs::create_dir_all(&dir).unwrap();
        // (FFV1: x264 rounds an odd side of a 4:2:0 picture down to even, FFV1 keeps it; `testsrc2` makes even
        // sides, so the picture is cut to the odd size with `crop ... exact=1`.)
        let make = |name: &str, size: &str| -> Asset {
            let p = dir.join(name);
            let want: Vec<u32> = size.split('x').map(|n| n.parse().unwrap()).collect();
            let source = format!("{}x{}", want[0].next_multiple_of(2), want[1].next_multiple_of(2));
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={source}:rate=30:duration=4,crop=w={}:h={}:x=0:y=0:exact=1,setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown", want[0], want[1]),
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            let a = probed(&p);
            let v = a.streams.iter().find(|s| s.kind == StreamKind::Video).expect("a video stream");
            assert_eq!((v.width, v.height), (Some(want[0]), Some(want[1])), "{name} keeps its size");
            a
        };
        let tenbit_tv = dir.join("tenbit-tv.mp4");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=4,setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "10",
            "-color_range",
            "tv",
            "-pix_fmt",
            "yuv420p10le",
            tenbit_tv.to_str().unwrap(),
        ]);
        Shapes {
            pillar: make("pillar.mkv", "480x360"),
            odd_width: make("odd-width.mkv", "203x360"),
            odd_height: make("odd-height.mkv", "640x203"),
            tenbit_tv: probed(&tenbit_tv),
        }
    })
}

/// A picture that is not the frame's shape is the placement's own picture, padded with transparency: the layer
/// below shows around it, and the zoom moves its edges across the samples (`pad`'s colour mixes into the fringe
/// the bilinear read makes). An odd width loses its last column to the pad's right border, on every FFmpeg; an odd
/// height is refused (whether the last row survives depends on how the graph allocated the frame, and the builds differ).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_picture_of_another_shape_is_placed_as_the_export_places_it() {
    let m = media();
    let s = shapes();
    for (name, asset) in [("pillarboxed 480x360", &s.pillar), ("odd width 203x360", &s.odd_width)] {
        let assets = [m.bars.clone(), asset.clone()];
        for (case, from, to, pos) in [
            ("zoom in", 1.0, 1.5, (0.0, 0.0)),
            ("zoom out", 1.0, 0.6, (0.1, -0.05)),
            ("pan", 1.0, 1.001, (-0.3, 0.2)),
        ] {
            let tl = timeline(
                vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![zooming(asset, from, to, pos)]],
                None,
            );
            run_placed(&format!("shape/{name}/{case}"), &tl, &assets, 30.0, 1.0);
        }
        // Over nothing but black: the transparent surround is the base's black. (In the 16:9 frame: a frame of the
        // picture's own odd size is one the encoder rounds down, and the fit then resizes it.)
        let tl = timeline(
            vec![vec![zooming(asset, 1.0, 0.7, (0.05, 0.05))]],
            Some(Delivery::new(640, 360, Fit::Contain)),
        );
        run_placed(&format!("shape/{name}/alone"), &tl, &assets[1..], 30.0, 1.0);
    }
    let tl = timeline(
        vec![
            vec![clip(&m.bars, 0.5, 1.5, 0.0)],
            vec![zooming(&s.odd_height, 1.0, 1.4, (0.0, 0.0))],
        ],
        None,
    );
    refused(
        "shape/odd height 640x203",
        &tl,
        &[m.bars.clone(), s.odd_height.clone()],
        &at_fps(30.0),
        upto(1.0, 30.0),
        "an odd-height picture",
    );
}

/// A picture of another format under a placement. FFmpeg converts it to `yuva420p` itself at the head of the
/// placement chain, where the compositor's decode converted it to 4:2:0 ahead of the graph, and the two agree where the
/// picture is already 4:2:0 (8 and 10 bit, limited and full range, any matrix tag) or gray, on both builds. A 4:4:4 or 4:2:2
/// picture is another matter: the two conversions are swscale's own paths, exact on 9.0.2 and 24 (4:4:4) and 16 (4:2:2) levels
/// of chroma off on 4.4.2 (measured), so those are refused (`PlacedFormat`) on every build, and so is a picture that may be full
/// range where the composite's range is negotiated (9.0.2: 3 to 5 levels off in a still). What the composite colour policy
/// refuses for the cut (a full-range picture on a build that negotiates the range, a mix of matrices) is refused as it always is.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_placed_picture_is_drawn_for_the_formats_both_conversions_agree_on() {
    let m = media();
    let v = variants();
    let s = shapes();
    let zoom_over = |under: &Asset, asset: &Asset| {
        (
            timeline(
                vec![vec![clip(under, 0.5, 1.5, 0.0)], vec![zooming(asset, 1.0, 1.6, (0.0, 0.0))]],
                None,
            ),
            [under.clone(), asset.clone()],
        )
    };
    let cases: [(&str, &Asset, &Asset); 11] = [
        ("8-bit", &m.testsrc, &m.bars),
        ("10-bit, range not recorded", &v.tenbit, &m.bars),
        ("10-bit tagged tv", &s.tenbit_tv, &m.bars),
        ("10-bit tagged full range", &v.tenbit_pc, &m.bars),
        ("gray", &v.gray, &m.bars),
        ("full-range yuvj420p", &v.mjpeg, &m.bars),
        ("bt709 over bt709", &v.testsrc709, &v.bars709),
        ("bt709 over untagged", &v.testsrc709, &m.bars),
        ("smpte170m", &v.testsrc170m, &m.bars),
        ("bt470bg", &v.testsrc470bg, &m.bars),
        ("bt709 under untagged", &m.testsrc, &v.bars709),
    ];
    for (name, asset, under) in cases {
        let (tl, assets) = zoom_over(under, asset);
        let planner = Planner::new(&tl, &assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
        let plan = planner.at_frame(10).expect("plan");
        let why: Vec<String> = plan
            .reasons(&compositor().caps(), plan.size(u32::MAX))
            .iter()
            .map(ToString::to_string)
            .collect();
        let case = format!("format/{name}");
        if why.is_empty() {
            run_placed(&case, &tl, &assets, 30.0, 1.0);
        } else {
            // Only what the composite colour policy says about the cut.
            let policy_says = [
                "holds a full-range picture",
                "different YCbCr matrices",
                "position on a full-range",
            ];
            assert!(
                why.iter().all(|w| policy_says.iter().any(|p| w.contains(p))),
                "{case}: refused for something else: {why:?}"
            );
            refused(&case, &tl, &assets, &at_fps(30.0), upto(1.0, 30.0), &why[0]);
        }
    }
    // 4:4:4, 4:2:2: drawn where the probe says this FFmpeg converts them in a graph as the decode does (6.1.1, 9.0.2: exact),
    // refused where it does not (4.4.2's `scale` defaults to bilinear: chroma 24 and 16 levels off), FFmpeg renders them.
    for (name, asset) in [("yuv444p", &v.yuv444), ("yuv422p", &v.yuv422)] {
        let (tl, assets) = zoom_over(&m.bars, asset);
        if compositor().caps().placed_chroma {
            run_placed(&format!("format/{name}"), &tl, &assets, 30.0, 1.0);
        } else {
            refused(
                &format!("format/{name}"),
                &tl,
                &assets,
                &at_fps(30.0),
                upto(1.0, 30.0),
                "a moving zoom or position on a",
            );
        }
    }
}

/// A live breath, drift or shake is a moving zoom or position whose numbers the graph writes as expressions of time
/// (`driven_expr`: `sin` and `cos` included) and the plan evaluates in the same arithmetic (`Clip::transform_at`): the placement
/// is the same pass, so they are drawn, exactly. A sway or a spin turns the picture, which no compositor draws to the sample.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn live_breath_drift_and_shake_are_placed_as_the_export_places_them() {
    use kerf_core::Behavior as B;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let with = |behaviors: Vec<B>| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.behaviors = behaviors;
        timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None)
    };
    let breathe = B::Breathe {
        amount: 0.15,
        speed: 1.3,
        phase: 0.2,
    };
    let drift = B::Drift {
        speed: 0.2,
        angle: 30.0,
        phase: 0.1,
    };
    let shake = B::Shake {
        amount: 0.03,
        speed: 2.5,
        seed: 7,
        phase: 0.0,
    };
    for (name, behaviors) in [
        ("breathe", vec![breathe]),
        ("drift", vec![drift]),
        ("shake", vec![shake]),
        ("all three", vec![breathe, drift, shake]),
    ] {
        run_placed(&format!("behavior/{name}"), &with(behaviors), &assets, 30.0, 1.0);
    }
    // Over a zoom that is keyed too, and a clip that starts late (the behaviour's clock is the clip's).
    let mut keyed = with(vec![breathe, drift]);
    keyed.tracks[1].clips[0].keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.4, 0.1, 0.0)];
    run_placed("behavior/over keyed zoom", &keyed, &assets, 30.0, 1.0);
    for (name, behaviors) in [
        (
            "sway",
            vec![B::Sway {
                amount: 5.0,
                speed: 1.0,
                phase: 0.0,
            }],
        ),
        ("spin", vec![B::Spin { speed: 0.5, phase: 0.0 }]),
    ] {
        refused(
            &format!("behavior/{name}"),
            &with(behaviors),
            &assets,
            &at_fps(30.0),
            upto(1.0, 30.0),
            "live behaviours",
        );
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_zoom_is_placed_after_a_grade_mask_fade_or_effect() {
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
    // The effects: the tables, the sharpen, the vignette and the key are exact; a blur is `gblur`'s fused-multiply-add
    // recursion, exact where `blur_probe` says this FFmpeg runs that (and not in the caps otherwise), on either side of the zoom.
    // The fit scale ahead of the effects is the identity here, so a sharpen of any amount is of the exact picture.
    for (name, effect) in [
        ("blur", kerf_core::VideoEffect::Blur { sigma: 4.0 }),
        ("sharpen", kerf_core::VideoEffect::Sharpen { amount: 1.5 }),
        ("grayscale", kerf_core::VideoEffect::Grayscale),
        ("invert", kerf_core::VideoEffect::Invert),
        ("chroma key", key("green", 0.2, 0.2)),
        ("vignette", kerf_core::VideoEffect::Vignette),
    ] {
        let tl = with(&|c| c.effects = vec![effect.clone()]);
        run(&format!("zoom+{name}"), &tl, &assets, 30.0, 1.0);
    }
    // The tutorial's clip: a feathered mask over a blurred picture, zooming, in a Cover frame.
    let mut c = zooming(&m.testsrc, 1.0, 1.25, (-0.05, 0.0));
    c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.1, false));
    c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 6.0 }];
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]],
        Some(Delivery::new(960, 540, Fit::Cover)),
    );
    // (The fit scale to 960x540 is swscale's to the sample, and so is the blur.)
    run("zoom+mask+blur cover 960x540", &tl, &assets, 30.0, 1.0);
    // Everything at once: grade, an effect, a mask, a fade-in.
    let tl = with(&|c| {
        c.color.contrast = 1.15;
        c.effects = vec![kerf_core::VideoEffect::Invert, kerf_core::VideoEffect::Blur { sigma: 2.0 }];
        c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.25, false));
        c.fade_in = 0.3;
    });
    run("zoom+grade+effects+mask+fade", &tl, &assets, 30.0, 1.0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_dissolve_dip_or_slide_into_a_moving_zoom_is_placed() {
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

/// A rotation, an opacity below 1 or a dip to white in front of a moving zoom (or position) is refused: the
/// placement would have to act on the rotated picture's alpha, or on the RGB round trip's, which neither has a
/// case that holds it to FFmpeg's.
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
    run_refusing(
        "keyed opacity + moving zoom (curve holds 1, then falls)",
        &tl,
        &assets,
        30.0,
        1.0,
        &refusing(
            "ZoomBehind(an opacity below 1)",
            |u| matches!(u, Unsupported::ZoomBehind(_, what) if what.contains("opacity")),
        ),
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
        run(&format!("mask/{name}"), &tl, &assets, 30.0, 1.0);
    }
    // Over noise: every chroma block has its own alpha average to get right.
    let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
    c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, false));
    let tl = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0)], vec![c]], None);
    run("mask/over noise", &tl, &[m.black.clone(), m.noise.clone()], 30.0, 1.0);
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
        run(&format!("mask/{name}"), &tl, &assets, 30.0, 1.0);
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
        run(&format!("mask/{name}"), &tl, &assets, 30.0, 1.0);
    }
    // A letterboxed picture: `pad` hands `geq` the whole padded frame, bars included, so the mask is the
    // canvas's size and cuts the bars too (a feathered ellipse on a 16:9 picture in a 9:16 frame).
    let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
    c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false));
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]],
        Some(Delivery::new(360, 640, Fit::Contain)),
    );
    run("mask/letterboxed 9:16", &tl, &[m.bars.clone(), m.testsrc.clone()], 30.0, 1.0);
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
    run("mask/fades", &tl, &assets, 30.0, 1.0);
    let out = clip(&m.bars, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
    inc.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.2, false));
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run("mask/dissolve", &tl, &[m.bars.clone(), m.testsrc.clone()], 30.0, 2.0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn effects_are_drawn_as_their_filters_draw_them() {
    let m = media();
    use kerf_core::VideoEffect as E;
    // Grayscale and invert are tables: exact. A sharpen is integer arithmetic: exact. A blur is the fused-multiply-add
    // recursion FFmpeg runs in SIMD, in integers, to the sample.
    for (name, effects) in [
        ("grayscale", vec![E::Grayscale]),
        ("invert", vec![E::Invert]),
        ("gray then invert", vec![E::Grayscale, E::Invert]),
        ("sharpen 1", vec![E::Sharpen { amount: 1.0 }]),
        ("sharpen 2.5", vec![E::Sharpen { amount: 2.5 }]),
        ("sharpen -1 (a blur)", vec![E::Sharpen { amount: -1.0 }]),
        ("sharpen 0.7", vec![E::Sharpen { amount: 0.7 }]),
        ("blur 0.5", vec![E::Blur { sigma: 0.5 }]),
        ("blur 2", vec![E::Blur { sigma: 2.0 }]),
        ("blur 6", vec![E::Blur { sigma: 6.0 }]),
        ("blur 20", vec![E::Blur { sigma: 20.0 }]),
        ("blur 6.3", vec![E::Blur { sigma: 6.3 }]),
        ("blur then invert", vec![E::Blur { sigma: 3.0 }, E::Invert]),
        ("invert then blur", vec![E::Invert, E::Blur { sigma: 3.0 }]),
        // A sharpen after a blur is exact at any gain, the blur being `gblur`'s to the sample; so are sharpens chained.
        (
            "blur then sharpen 0.5",
            vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 0.5 }],
        ),
        ("sharpen twice", vec![E::Sharpen { amount: 1.0 }, E::Sharpen { amount: 2.0 }]),
        // A blur straight after the sharpen, the slowest pass to draw in front of it: the compute pass that
        // runs the blur must read the finished sharpen (`Compositor::settle`: lavapipe 23.2 did not wait).
        (
            "sharpen 2.5 then blur",
            vec![E::Sharpen { amount: 2.5 }, E::Blur { sigma: 3.0 }],
        ),
        (
            "sharpen 0.5 then blur",
            vec![E::Sharpen { amount: 0.5 }, E::Blur { sigma: 3.0 }],
        ),
        (
            "sharpen, gray, blur, invert",
            vec![E::Sharpen { amount: 1.0 }, E::Grayscale, E::Blur { sigma: 2.0 }, E::Invert],
        ),
        // A vignette is exact: its dither runs on from the clip's first frame, which the cursor counts.
        ("vignette", vec![E::Vignette]),
        ("gray, vignette, invert", vec![E::Grayscale, E::Vignette, E::Invert]),
        ("blur then vignette", vec![E::Blur { sigma: 2.0 }, E::Vignette]),
    ] {
        let (tl, assets) = over_bars(m, &|c| c.effects = effects.clone());
        run(&format!("effect/{name}"), &tl, &assets, 30.0, 0.5);
    }
    // A graded clip: `eq` runs ahead of every effect.
    let (tl, assets) = over_bars(m, &|c| {
        c.color.contrast = 1.2;
        c.color.saturation = 1.4;
        c.effects = vec![E::Blur { sigma: 3.0 }, E::Grayscale];
    });
    run("effect/graded then blur, gray", &tl, &assets, 30.0, 0.5);
    // Noise in all three planes, so every sample of a plane matters to its neighbours.
    for (name, effects) in [
        ("noise blur", vec![E::Blur { sigma: 2.5 }]),
        ("noise sharpen", vec![E::Sharpen { amount: 1.5 }]),
        ("noise invert", vec![E::Invert]),
    ] {
        let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
        c.effects = effects;
        let tl = timeline(vec![vec![c]], None);
        run(&format!("effect/{name}"), &tl, std::slice::from_ref(&m.noise), 30.0, 0.4);
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
    );
    // After a scale: the effect sees the picture at the size it is drawn at (the zoom comes first
    // when it does not move).
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.6;
        c.transform.pos_x = 0.15;
        c.effects = vec![E::Blur { sigma: 3.0 }];
    });
    run("effect/blur on a pip", &tl, &assets, 30.0, 0.5);
    // Odd sizes: the chroma planes round up and are blurred at their own size.
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.33;
        c.effects = vec![E::Blur { sigma: 2.0 }, E::Sharpen { amount: 0.5 }];
    });
    run("effect/odd 211x118", &tl, &assets, 30.0, 0.5);
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
    run("effect/vignette on a pip", &tl, &assets, 30.0, 0.5);
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.33;
        c.effects = vec![E::Vignette];
    });
    run("effect/vignette odd 211x118", &tl, &assets, 30.0, 0.5);
    let late = vignetted(&|c| c.timeline_start = 0.5);
    check_motion(Check {
        case: "effect/vignette late start",
        tl: &timeline(vec![vec![late]], None),
        assets: std::slice::from_ref(&m.testsrc),
        opts: at_fps(30.0),
        frames: 15..44,
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
    );
    // A chroma key: an alpha plane at the picture's size, from the chroma (exact), blended as a mask's is.
    for (name, effects) in [
        ("chroma key green", vec![key("green", 0.15, 0.1)]),
        ("chroma key hard", vec![key("0x2050a0", 0.2, 0.0)]),
        ("chroma key after tables", vec![E::Invert, key("magenta", 0.25, 0.05)]),
    ] {
        let (tl, assets) = over_bars(m, &|c| c.effects = effects.clone());
        run(&format!("effect/{name}"), &tl, &assets, 30.0, 0.5);
    }
    let mut c = clip(&m.noise, 0.5, 1.5, 0.0);
    c.effects = vec![key("0x808080", 0.05, 0.3)];
    run(
        "effect/chroma key noise",
        &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None),
        &[m.bars.clone(), m.noise.clone()],
        30.0,
        0.4,
    );
    // Through its fades and a dissolve (the ramp acts on the key's alpha).
    let (tl, assets) = over_bars(m, &|c| {
        c.effects = vec![key("green", 0.2, 0.2)];
        c.fade_in = 0.3;
        c.fade_out = 0.3;
    });
    run("effect/chroma key with fades", &tl, &assets, 30.0, 1.0);
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
    );
    // A key after a blur: the blur is `gblur`'s to the sample, so the chroma the key reads is FFmpeg's.
    for sigma in [1.0, 4.0] {
        let (tl, assets) = over_bars(m, &|c| c.effects = vec![E::Blur { sigma }, key("green", 0.2, 0.2)]);
        run(&format!("effect/chroma key after a blur {sigma}"), &tl, &assets, 30.0, 0.5);
    }
    // A sharpen after a scale: the scaler is swscale's to the sample (`caps.exact_scaler`) and a blur is `gblur`'s, so the filter's
    // gain has no difference to amplify, at any amount, chained.
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
        run(&format!("effect/{name} on a pip"), &tl, &assets, 30.0, 0.5);
    }
    for (name, effects) in [
        (
            "sharpen 0.5 twice",
            vec![E::Sharpen { amount: 0.5 }, E::Sharpen { amount: 0.5 }],
        ),
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
        run(&format!("effect/{name} on a pip"), &tl, &assets, 30.0, 0.5);
    }
    // After a blur, unscaled.
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
        run(&format!("effect/{name}"), &tl, &assets, 30.0, 0.5);
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
        );
    }
    // Rotated and translucent layers: the effect comes ahead of both.
    let (tl, assets) = over_bars(m, &|c| {
        c.transform.scale = 0.6;
        c.transform.opacity = 0.7;
        c.effects = vec![E::Blur { sigma: 2.0 }];
    });
    run("effect/blur on a translucent layer", &tl, &assets, 30.0, 0.5);
    // In and out of a fade and a dissolve.
    let (tl, assets) = over_bars(m, &|c| {
        c.effects = vec![E::Blur { sigma: 2.0 }];
        c.fade_in = 0.3;
        c.fade_out = 0.3;
    });
    run("effect/blur with fades", &tl, &assets, 30.0, 1.0);
}

/// A chroma key on a layer the compositor scales is drawn, as `chromakey` draws it, to the sample: its alpha is a distance in the chroma
/// over `blend` (a level of chroma moves it by up to 1/blend levels, a blend of 0 flips it from 0 to 255), and the chroma it reads is what
/// the scaler leaves, which is swscale's to the sample where the scaler is measured (the plan refuses the key after a scale only where it
/// is not: `caps.exact_scaler`). Several scales (a half, an odd one just under it, a mild shrink), keys from a hard one to a wide one, a
/// key colour of the picture's and one that is not, noise and a test card, and the key at every position among the other effects
/// (the green-screen archetype puts it first).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_chroma_key_after_a_scale_is_drawn_as_chromakey_draws_it() {
    let m = media();
    use kerf_core::VideoEffect as E;
    let over_in = |fg: &Asset, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(fg, 0.5, 1.5, 0.0);
        edit(&mut c);
        (
            timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], delivery),
            vec![m.bars.clone(), fg.clone()],
        )
    };
    let over = |fg: &Asset, edit: &dyn Fn(&mut Clip)| over_in(fg, None, edit);
    for scale in [0.6, 0.4985, 0.9] {
        for (similarity, blend) in [(0.15, 0.1), (0.1, 0.0), (0.5, 0.5), (0.00001, 0.0), (1.0, 1.0)] {
            for (name, fg) in [("testsrc", &m.testsrc), ("noise", &m.noise)] {
                for color in ["green", "0x808080"] {
                    let (tl, assets) = over(fg, &|c| {
                        c.transform.scale = scale;
                        c.effects = vec![key(color, similarity, blend)];
                    });
                    run(
                        &format!("key-scale/{name} {color} scale {scale} sim {similarity} blend {blend}"),
                        &tl,
                        &assets,
                        30.0,
                        1.0,
                    );
                }
            }
        }
    }
    for (name, effects) in [
        ("blur", vec![E::Blur { sigma: 2.0 }]),
        ("sharpen", vec![E::Sharpen { amount: 1.5 }]),
        ("invert", vec![E::Invert]),
        ("grayscale", vec![E::Grayscale]),
        ("vignette", vec![E::Vignette]),
        ("blur, sharpen", vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 0.5 }]),
        (
            "sharpen, invert, grayscale",
            vec![E::Sharpen { amount: 2.0 }, E::Invert, E::Grayscale],
        ),
        ("blur, invert, vignette", vec![E::Blur { sigma: 1.5 }, E::Invert, E::Vignette]),
    ] {
        for fg in [&m.testsrc, &m.noise] {
            for scale in [0.6, 0.4985] {
                for at in 0..=effects.len() {
                    let (tl, assets) = over(fg, &|c| {
                        c.transform.scale = scale;
                        let mut list = effects.clone();
                        list.insert(at, key("0x808080", 0.25, 0.2));
                        c.effects = list;
                    });
                    run(
                        &format!("key-scale/{name}, the key at {at}, {} scale {scale}", fg.name),
                        &tl,
                        &assets,
                        30.0,
                        1.0,
                    );
                }
            }
        }
    }
    // With a grade ahead of the key, in a dissolve, in and out of a fade, and moved.
    let (tl, assets) = over(&m.testsrc, &|c| {
        c.transform.scale = 0.6;
        c.color.contrast = 1.2;
        c.color.saturation = 1.3;
        c.effects = vec![key("green", 0.15, 0.1)];
    });
    run("key-scale/with an eq ahead", &tl, &assets, 30.0, 1.0);
    // The green-screen shape: scale 0.9, the key first, a grade (an `eq`) on the clip, and each effect after it.
    for (name, after) in [
        ("alone", vec![]),
        ("blur", vec![E::Blur { sigma: 1.5 }]),
        ("vignette", vec![E::Vignette]),
        ("sharpen", vec![E::Sharpen { amount: 0.8 }]),
    ] {
        let (tl, assets) = over(&m.testsrc, &|c| {
            c.transform.scale = 0.9;
            c.color.contrast = 1.15;
            c.color.saturation = 1.2;
            c.effects = [key("0x00b140", 0.25, 0.1)].into_iter().chain(after.clone()).collect();
        });
        run(&format!("key-scale/green screen, {name}"), &tl, &assets, 30.0, 1.0);
    }
    let (tl, assets) = over(&m.testsrc, &|c| {
        c.transform.scale = 0.6;
        c.fade_in = 0.3;
        c.fade_out = 0.3;
        c.effects = vec![key("0x808080", 0.25, 0.2)];
    });
    run("key-scale/with fades", &tl, &assets, 30.0, 1.0);
    let (tl, assets) = over(&m.noise, &|c| {
        c.transform.scale = 0.5;
        c.transform.pos_x = 0.25;
        c.transform.pos_y = -0.25;
        c.effects = vec![key("0x808080", 0.25, 0.2)];
    });
    run("key-scale/moved", &tl, &assets, 30.0, 1.0);
    // Enlargements: the picture drawn bigger than it is (a 640x360 clip into a 960x540 frame, 1.5x, into 1280x720, 2x, under Cover and under
    // Contain, and shrunk back a little after the fit: 1.35x), every key position among the other effects, noise and a test card. (A zoom
    // in on top of a fit is the plan's own refusal, "a Cover clip that zooms" and "a zoom in under Contain", and is not drawn.)
    for (frame, scale) in [
        (Delivery::new(960, 540, Fit::Cover), 1.0),
        (Delivery::new(1280, 720, Fit::Cover), 1.0),
        (Delivery::new(960, 540, Fit::Contain), 1.0),
        (Delivery::new(960, 540, Fit::Contain), 0.9),
    ] {
        for fg in [&m.testsrc, &m.noise] {
            for (name, effects) in [
                ("alone", vec![]),
                ("blur", vec![E::Blur { sigma: 2.0 }]),
                ("sharpen", vec![E::Sharpen { amount: 1.5 }]),
                ("invert, grayscale", vec![E::Invert, E::Grayscale]),
            ] {
                for at in 0..=effects.len() {
                    let (tl, assets) = over_in(fg, Some(frame), &|c| {
                        c.transform.scale = scale;
                        let mut list = effects.clone();
                        list.insert(at, key("0x808080", 0.25, 0.2));
                        c.effects = list;
                    });
                    run(
                        &format!(
                            "key-scale/enlarged to {}x{} {:?}, {name}, the key at {at}, {} scale {scale}",
                            frame.width, frame.height, frame.fit, fg.name
                        ),
                        &tl,
                        &assets,
                        30.0,
                        1.0,
                    );
                }
            }
        }
    }
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
    run("label/translucent-untagged-hd-before-a-709-clip", &tl, &assets, 30.0, 2.0);
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
    run("label/translucent-untagged-hd-over-a-709-clip", &tl, &assets, 30.0, 2.0);
    // The control: the same HD frames with no BT.709 clip in the graph are not labelled, and drawn.
    let tl = timeline(vec![vec![glass(h)]], canvas);
    let plan = kerf_core::Planner::new(&tl, &assets, &at_fps(30.0), kerf_core::PlanRequest::motion(policy()))
        .unwrap()
        .at_frame(10)
        .unwrap();
    assert!(!plan.layers[0].stream.label_bt709);
    run("label/translucent-untagged-hd-alone", &tl, &assets, 30.0, 1.0);
}

/// A dip to white is `yuv420p -> rgb24 -> yuv420p` around the fade, out of YUV with the clip's own matrix, so
/// the label is read there too: an untagged HD clip that dips through white in a cut that holds a BT.709 clip
/// is taken out of YUV as BT.709, exactly as the file tagged BT.709 is. The label is the first filter of its
/// chain for this: behind the fit `scale` (the filter FFmpeg has convert the picture for the fade) it was
/// BT.601 in and BT.709 out, Y 17 / UV 9 off on 9.0.2 and Y 30 / UV 16 on 6.1.1. The HD clips are drawn at
/// their own size (a 1280x720 canvas: a layer the scaler resizes is refused its dip), and the planes are exact.
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
    run("label/dip-white-labelled-hd", &tl, &assets, 30.0, 2.0);
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
    run_full_range("range/pc-plain", &one(&r.pc, &|_| {}), pc, 30.0, 1.0);
    let tl = timeline(vec![vec![clip(&m.black, 0.5, 1.5, 0.0), clip(&r.pc, 0.5, 1.5, 1.0)]], None);
    run_full_range("range/black-then-pc", &tl, &[m.black.clone(), r.pc.clone()], 30.0, 2.0);
    let tl = timeline(vec![vec![clip(&r.pc, 0.5, 1.5, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0)]], None);
    run_full_range("range/pc-then-limited", &tl, &[r.pc.clone(), m.testsrc.clone()], 30.0, 2.0);

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
    run_refusing("range/pc-fade", &tl, pc, 30.0, 1.0, &fade_refused);
    // The scaler resizes it (an enlargement, or a shrink over the bars): swscale converts the range inside the resize,
    // under either FFmpeg.
    let (tl, assets) = over_bars(&r.pc, &|c| c.transform.scale = 0.5);
    run_refusing(
        "range/pc-shrunk-over-bars",
        &tl,
        &assets,
        30.0,
        1.0,
        &full_range_resized_refused(),
    );
    let enlarged = timeline(
        vec![vec![clip(&r.pc, 0.5, 1.5, 0.0)]],
        Some(Delivery::new(960, 540, Fit::Contain)),
    );
    run_refusing("range/pc-enlarged", &enlarged, pc, 30.0, 1.0, &full_range_resized_refused());

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
            run(&format!("range/{label}-{name}"), &tl, one_a, 30.0, 1.0);
        }
        let tl = one(a, &|c| {
            c.fade_in = 0.5;
            c.fade_out = 0.5;
        });
        run(&format!("range/{label}-fade"), &tl, one_a, 30.0, 1.0);
        let tl = one(a, &|c| c.color.gamma = 1.4);
        run(&format!("range/{label}-graded"), &tl, one_a, 30.0, 1.0);
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
            run(&format!("grade/{name}, {source}"), &tl, &assets, 30.0, 0.4);
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
            run(&format!("grade/{name}, {source}"), &tl, &assets, 30.0, 0.4);
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
    run("grade/labelled hd", &tl, &assets, 30.0, 2.0);
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
    run("grade/after an eq", &tl, &a, 30.0, 0.4);
    let (tl, a) = on(&m.testsrc, curves, &|c| {
        c.set_property_keys(
            Property::Brightness,
            vec![PropertyKey::new(0.0, -0.1), PropertyKey::new(0.5, 0.1)],
        );
    });
    run("grade/after a keyed eq", &tl, &a, 30.0, 0.4);
    let (tl, a) = on(&m.testsrc, wheels_and_lut, &|c| {
        c.speed = 2.0;
        c.source_out = 2.5;
    });
    run("grade/at speed 2", &tl, &a, 30.0, 0.4);
    // (A crop that the canvas is cut to, so that nothing is resized.)
    let (mut tl, a) = on(&m.testsrc, wheels_and_lut, &|c| {
        c.transform.crop_left = 0.125;
        c.transform.crop_top = 0.25;
    });
    tl.format = Some(Delivery::new(560, 270, Fit::Contain));
    run("grade/a cropped picture", &tl, &a, 30.0, 0.4);

    // The alpha plane: `format=yuva420p` follows the grade.
    let (tl, a) = on(&m.testsrc, wheels_and_lut, &|c| c.transform.opacity = 0.6);
    run("grade/translucent", &tl, &a, 30.0, 0.4);
    let (tl, a) = on(&m.noise, curves, &|c| {
        c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false));
    });
    run("grade/under a soft ellipse mask", &tl, &a, 30.0, 0.4);
    let (tl, a) = on(&m.bars, wheels_and_lut, &|c| c.effects = vec![key("green", 0.3, 0.3)]);
    run("grade/under a chroma key", &tl, &a, 30.0, 0.4);

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
    );

    let (out, mut inc) = (clip(&m.bars, 0.5, 1.5, 0.0), clip(&m.testsrc, 0.5, 1.5, 1.0));
    inc.grade = all[4].1.clone();
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    run("grade/dissolving in", &tl, &[m.bars.clone(), m.testsrc.clone()], 30.0, 2.0);
    let mut leaving = clip(&m.testsrc, 0.5, 1.5, 0.0);
    leaving.grade = all[4].1.clone();
    let mut next = clip(&m.bars, 0.5, 1.5, 1.0);
    next.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![leaving, next]], None);
    run("grade/dissolving out", &tl, &[m.bars.clone(), m.testsrc.clone()], 30.0, 2.0);

    let (tl, a) = on(&m.smooth, wheels_and_lut, &|c| c.transform.rotation = 20.0);
    run_refusing("grade/rotated", &tl, &a, 30.0, 0.4, &rotation_refused());
    let (tl, a) = on(&m.smooth, wheels_and_lut, &|c| c.transform.rotation = 90.0);
    run("grade/rotated a quarter", &tl, &a, 30.0, 0.4);

    // A fade that is not running leaves the picture where it is: the frames after a fade-in are drawn.
    let (tl, a) = on(&m.testsrc, wheels_and_lut, &|c| c.fade_in = 0.2);
    run_refusing(
        "grade/after its fade-in",
        &tl,
        &a,
        30.0,
        0.8,
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
    // A moving zoom behind the grade: the placement reads the 4:2:0 planes the grade hands on.
    let (tl, a) = one(&m.testsrc, &|c| {
        c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.4, 0.0, 0.0)];
    });
    run_range("grade/moving zoom", &tl, &a, 30.0, frames.clone());
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
        run(&format!("grade/keyed opacity, {name}"), &over(c), &assets, 30.0, 1.0);
    }
    let mut c = graded(&heavy);
    c.set_property_keys(Property::Opacity, vec![key(0.1, 0.0), key(0.9, 1.0)]);
    run("grade/keyed opacity from nothing", &over(c), &assets, 30.0, 1.0);
    // A keyed position that holds: the picture stays its own size and the overlay puts it off the canvas' edge.
    let mut c = graded(&heavy);
    c.set_property_keys(Property::PosX, vec![key(0.0, 0.3), key(1.0, 0.3)]);
    c.set_property_keys(Property::PosY, vec![key(0.0, -0.2), key(1.0, -0.2)]);
    run("grade/keyed position, held", &over(c), &assets, 30.0, 1.0);
    // One that moves is placed by the export's `perspective` after the grade: the placement reads the planes the grade
    // hands on, exact; with a keyed opacity below 1 it is the alpha plane's RGB round trip that is not drawn behind it.
    let mut c = graded(&heavy);
    c.set_property_keys(Property::PosX, vec![key(0.0, -0.1), key(1.0, 0.1)]);
    run_placed("grade/keyed position", &over(c), &assets, 30.0, 1.0);
    let mut c = graded(&heavy);
    c.set_property_keys(Property::Opacity, vec![key(0.0, 0.3), key(1.0, 0.9)]);
    c.set_property_keys(Property::PosX, vec![key(0.0, -0.1), key(1.0, 0.1)]);
    refused(
        "grade/keyed opacity and position",
        &over(c),
        &assets,
        &at_fps(30.0),
        upto(1.0, 30.0),
        "an opacity below 1",
    );

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
            run(&format!("grade/{name}, {source}"), &tl, &assets, 30.0, 0.4);
        }
    }
    let (_, heavy) = grades().remove(8);
    let (tl, assets) = graded_alone(&v.bars470bg, &heavy, &|_| {});
    run("grade/lut, curves and wheels, bars470bg", &tl, &assets, 30.0, 0.4);
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
            run_refusing(name, tl, assets, 30.0, secs, &mixed);
        } else {
            run(name, tl, assets, 30.0, secs);
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
        run(&format!("grade/{name}, graded second"), &tl, &assets, 30.0, 2.0);
        let mut g = clip(graded, 0.5, 1.5, 0.0);
        g.grade = grade.clone();
        let tl = timeline(vec![vec![g, clip(other, 0.5, 1.5, 1.0)]], None);
        run(&format!("grade/{name}, graded first"), &tl, &assets, 30.0, 2.0);
    }
    // The tagged way out (smpte170m / bt470bg: BT.601's coefficients through 4:4:4) beside an untagged clip.
    let mut g = clip(&v.testsrc170m, 0.5, 1.5, 1.0);
    g.grade = grade;
    let assets = [m.bars.clone(), v.testsrc170m.clone()];
    let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), g]], None);
    run("grade/smpte170m after untagged", &tl, &assets, 30.0, 2.0);
}

// ---- layers of another matrix than the bottom's -----------------------------------------------------------
//
// FFmpeg 9 makes the first clip of the graph's tag the composite's for the whole cut and converts every clip of another
// matrix into it, wherever on the timeline the two are: swscale's two-step conversion through RGB, a `scale` of its own after
// the chain's last `format` (`kerf_gpu::convert`, probed). The compositor converts the picture the chain has made (after
// the fit `scale` and `pad`, so a phone clip shrunk into a landscape frame is scaled, then converted) and the planes are the
// export's to the sample (no case here names a bound). Under a fixed-matrix FFmpeg nothing converts, and the same
// cases are the layers drawn as they are.

/// Footage tagged and untagged for the conversion cases: a phone-shaped pair (540x960, 960x540 canvas: the picture
/// 304 wide, the bars 328 each side), tagged BT.709 and `smpte170m`, and per-pixel noise tagged BT.709. Lossless H.264.
struct MatrixMedia {
    portrait709: Asset,
    portrait601: Asset,
    noise709: Asset,
    wide709: Asset,
    wide601: Asset,
}

fn matrix_media() -> &'static MatrixMedia {
    static M: OnceLock<MatrixMedia> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-matrix");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, lavfi: &str| -> Asset {
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
            probed(&p)
        };
        let bt709 = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
        // (BT.601 by a tag: an untagged picture this size would be labelled BT.709 by the graph beside a tagged one, and be
        // the bottom's matrix, with nothing to convert.)
        let smpte170m = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";
        let noise = "nullsrc=size=640x360:rate=30:duration=4,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'";
        MatrixMedia {
            portrait709: make("portrait709.mp4", &format!("testsrc2=size=540x960:rate=30:duration=4,{bt709}")),
            portrait601: make("portrait601.mp4", &format!("testsrc2=size=540x960:rate=30:duration=4,{smpte170m}")),
            noise709: make("noise709.mp4", &format!("{noise},{bt709}")),
            wide709: make("wide709.mp4", &format!("smptehdbars=size=960x540:rate=30:duration=4,{bt709}")),
            wide601: make("wide601.mp4", &format!("smptehdbars=size=960x540:rate=30:duration=4,{smpte170m}")),
        }
    })
}

/// A layer the conversion does not cover (`MixedMatricesWith`), or a cut it does not cover at all (`MixedMatrices`: a clip on an
/// alpha plane in it, an opacity or a rotation among them).
fn matrix_conversion_refused() -> Refusing {
    refusing("MixedMatrices(With)", |u| {
        matches!(u, Unsupported::MixedMatrices | Unsupported::MixedMatricesWith(..))
    })
}

/// The cascade of a dissolving clip's fit `scale` at the ledger's own ratio, and the fades of a converted clip. A phone vlog
/// (1080x1920 clips in a 540-wide frame) is a 6:1 shrink: the chains of phone clips into a canvas they fall to a sixth of, the
/// picture 90x160 between bars of 100 (even, on the 4:2:0 grid). A fade to black on a plain chain runs ahead of its converter `scale`:
/// the compositor fades the planes and converts them (every frame drawn, bars included); a dissolving clip's own fade runs behind
/// its ramp on the alpha plane, which is not measured: refused while it runs.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_phone_vlog_at_the_ledgers_ratio_and_the_fades_of_a_converted_clip_are_the_exports() {
    let m = media();
    let v = variants();
    let x = matrix_media();
    let negotiates = policy() != CompositeColorPolicy::FixedBt601;
    let landscape = Some(Delivery::new(960, 540, Fit::Contain));
    let sixth = Some(Delivery::new(290, 160, Fit::Contain));
    let (p709, p601) = (&x.portrait709, &x.portrait601);
    let dissolving = |a: &Asset, start: f64| {
        let mut c = clip(a, 0.5, 1.5, start);
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.4,
        });
        c
    };
    let assets = [p709.clone(), p601.clone()];
    // (1) every clip dissolving into the one before; (2) a plain 709 after a dissolving 601; (3) a plain 601 last.
    let vlogs: [(&str, Vec<Clip>); 3] = [
        (
            "four, every one dissolving",
            vec![
                clip(p709, 0.5, 1.5, 0.0),
                dissolving(p601, 1.0),
                dissolving(p709, 2.0),
                dissolving(p601, 3.0),
            ],
        ),
        (
            "709, 601 dissolving, 709 plain",
            vec![clip(p709, 0.5, 1.5, 0.0), dissolving(p601, 1.0), clip(p709, 0.5, 1.5, 2.0)],
        ),
        (
            "709, 709 dissolving, 601 dissolving",
            vec![clip(p709, 0.5, 1.5, 0.0), dissolving(p709, 1.0), dissolving(p601, 2.0)],
        ),
    ];
    for (name, clips) in vlogs {
        let secs = clips.len() as f64;
        let tl = timeline(vec![clips], sixth);
        run(&format!("convert/vlog at 6:1: {name}"), &tl, &assets, 30.0, secs);
    }
    // A fade to black on the converted clip, in and out: a plain chain, drawn at every frame; so is a letterboxed phone clip's.
    let mut faded = clip(&m.testsrc, 0.5, 1.5, 1.0);
    faded.fade_in = 0.4;
    faded.fade_out = 0.4;
    let tl = timeline(vec![vec![clip(&v.testsrc709, 0.5, 1.5, 0.0), faded]], None);
    run(
        "convert/fade-in and out on the converted clip",
        &tl,
        &[v.testsrc709.clone(), m.testsrc.clone()],
        30.0,
        2.0,
    );
    // ... and with a table effect on it (invert, grayscale: the engine runs them ahead of its fades; the compositor applies the tables in
    // the scale, then fades, then converts).
    for (name, effect) in [
        ("invert", kerf_core::VideoEffect::Invert),
        ("grayscale", kerf_core::VideoEffect::Grayscale),
    ] {
        let mut faded = clip(&m.testsrc, 0.5, 1.5, 1.0);
        faded.fade_in = 0.4;
        faded.fade_out = 0.4;
        faded.effects = vec![effect];
        let tl = timeline(vec![vec![clip(&v.testsrc709, 0.5, 1.5, 0.0), faded]], None);
        run(
            &format!("convert/fade-in and out on the converted clip, {name}"),
            &tl,
            &[v.testsrc709.clone(), m.testsrc.clone()],
            30.0,
            2.0,
        );
    }
    let mut faded = clip(p601, 0.5, 1.5, 1.0);
    faded.fade_in = 0.5;
    faded.fade_out = 0.5;
    let tl = timeline(vec![vec![clip(p709, 0.5, 1.5, 0.0), faded]], landscape);
    run(
        "convert/fade-in and out on a letterboxed converted clip",
        &tl,
        &assets,
        30.0,
        2.0,
    );
    // ... and a dissolving clip's own fade is refused while it runs. (The converted clip is the third: the second, the first to
    // dissolve, decides the matrix and is not converted.)
    let mut converted = dissolving(p601, 2.0);
    converted.fade_out = 0.5;
    let tl = timeline(
        vec![vec![clip(p709, 0.5, 1.5, 0.0), dissolving(p709, 1.0), converted]],
        landscape,
    );
    let case = "convert/fade-out on a dissolving converted clip";
    if negotiates {
        run_refusing(case, &tl, &assets, 30.0, 3.0, &matrix_conversion_refused());
    } else {
        run(case, &tl, &assets, 30.0, 3.0);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn layers_of_another_matrix_are_converted_into_the_bottom_ones() {
    use CompositeColorPolicy::FixedBt601;
    let m = media();
    let v = variants();
    let b = blur_media();
    let x = matrix_media();
    let negotiates = policy() != FixedBt601;
    let one = |a: &Asset| std::slice::from_ref(a).to_vec();
    // Two clips back to back on one track, every order of the matrices: the first clip's tag is the composite's for the
    // whole cut, so the second is converted in its own second, and the first is as it is.
    let pairs: [(&str, &Asset, &Asset); 8] = [
        ("709 then untagged", &v.testsrc709, &m.testsrc),
        ("untagged then 709", &m.testsrc, &v.testsrc709),
        ("709 then 2020", &v.testsrc709, &b.bt2020),
        ("2020 then 709", &b.bt2020, &v.testsrc709),
        ("untagged then 2020", &m.testsrc, &b.bt2020),
        ("smpte170m then 709", &v.testsrc170m, &v.testsrc709),
        ("709 then smpte170m", &v.testsrc709, &v.testsrc170m),
        ("709 then bt470bg", &v.testsrc709, &v.testsrc470bg),
    ];
    for (name, first, second) in pairs {
        let tl = timeline(vec![vec![clip(first, 0.5, 1.5, 0.0), clip(second, 0.5, 1.5, 1.0)]], None);
        run(&format!("convert/{name}"), &tl, &[first.clone(), second.clone()], 30.0, 2.0);
    }
    // One matrix is not a mix (BT.601's coefficients under a tag, untagged beside it): nothing converted, drawn as ever.
    let tl = timeline(
        vec![vec![clip(&m.testsrc, 0.5, 1.5, 0.0), clip(&v.testsrc170m, 0.5, 1.5, 1.0)]],
        None,
    );
    run(
        "convert/untagged then smpte170m",
        &tl,
        &[m.testsrc.clone(), v.testsrc170m.clone()],
        30.0,
        2.0,
    );
    // Busy: every Y, U, V and the clipped RGB, converted from a tag to another.
    let tl = timeline(
        vec![vec![clip(&x.noise709, 0.5, 1.5, 0.0), clip(&m.noise, 0.5, 1.5, 1.0)]],
        None,
    );
    run(
        "convert/noise, 709 then untagged",
        &tl,
        &[x.noise709.clone(), m.noise.clone()],
        30.0,
        2.0,
    );
    // A clip over another track, whole frame and shrunk (a picture-in-picture: the picture is scaled, then converted), over a
    // bottom layer of the other matrix and under one.
    for (name, bottom, top) in [
        ("untagged pip over 709", &v.bars709, &m.testsrc),
        ("709 pip over untagged", &m.bars, &v.testsrc709),
        ("2020 pip over 709", &v.bars709, &b.bt2020),
    ] {
        let mut pip = clip(top, 0.5, 1.5, 0.0);
        pip.transform.scale = 0.5;
        pip.transform.pos_x = 0.2;
        pip.transform.pos_y = -0.1;
        let tl = timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0)], vec![pip]], None);
        run(&format!("convert/{name}"), &tl, &[bottom.clone(), top.clone()], 30.0, 1.0);
        let tl = timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0)], vec![clip(top, 0.5, 1.5, 0.0)]], None);
        run(
            &format!("convert/{}, whole frame", name.replace("pip", "layer")),
            &tl,
            &[bottom.clone(), top.clone()],
            30.0,
            1.0,
        );
    }
    // The graph's order, not the timeline's: the bottom track's clip is later than the upper track's, so for the first second the
    // clip alone on screen is of the other matrix than the composite's, and converted all the same.
    for (name, bottom, top) in [
        ("709 bottom, untagged first", &v.testsrc709, &m.testsrc),
        ("untagged bottom, 709 first", &m.testsrc, &v.testsrc709),
    ] {
        let tl = timeline(vec![vec![clip(bottom, 0.5, 1.5, 1.0)], vec![clip(top, 0.5, 1.5, 0.0)]], None);
        run(&format!("convert/{name}"), &tl, &[bottom.clone(), top.clone()], 30.0, 2.0);
    }
    // A phone clip shrunk into a landscape frame (bars at the sides, the picture scaled and then converted), in a sequence
    // of the two matrices, and over a wide clip of the other.
    let landscape = Some(Delivery::new(960, 540, Fit::Contain));
    for (name, first, second) in [
        ("phone 709 then phone 601", &x.portrait709, &x.portrait601),
        ("phone 601 then phone 709", &x.portrait601, &x.portrait709),
    ] {
        let tl = timeline(vec![vec![clip(first, 0.5, 1.5, 0.0), clip(second, 0.5, 1.5, 1.0)]], landscape);
        run(
            &format!("convert/{name}, letterboxed"),
            &tl,
            &[first.clone(), second.clone()],
            30.0,
            2.0,
        );
    }
    let tl = timeline(
        vec![
            vec![clip(&x.wide709, 0.5, 1.5, 0.0)],
            vec![clip(&x.portrait601, 0.5, 1.5, 0.0)],
        ],
        landscape,
    );
    run(
        "convert/phone 601 over wide 709",
        &tl,
        &[x.wide709.clone(), x.portrait601.clone()],
        30.0,
        1.0,
    );
    let tl = timeline(
        vec![
            vec![clip(&x.wide601, 0.5, 1.5, 0.0)],
            vec![clip(&x.portrait709, 0.5, 1.5, 0.0)],
        ],
        landscape,
    );
    run(
        "convert/phone 709 over wide 601",
        &tl,
        &[x.wide601.clone(), x.portrait709.clone()],
        30.0,
        1.0,
    );
    // Dissolving into a clip of the other matrix, the way a phone vlog does. The incoming clip is on an alpha plane
    // (`format=yuva420p`, the alpha `fade`), which makes it the one that decides the composite's matrix (the first such clip's):
    // the **bottom** clip's frames are converted into the dissolving clip's matrix (a plain clip: drawn), and the dissolving clip itself is not. The slides
    // and the pushes place the incoming clip with `overlay`'s own expressions on a plain picture, and the dip to black fades it ahead
    // of the conversion (a plain chain's fade, then its converter `scale`).
    for (name, first, second) in [
        ("709 into 601", &x.portrait709, &x.portrait601),
        ("601 into 709", &x.portrait601, &x.portrait709),
    ] {
        let assets = [first.clone(), second.clone()];
        let mut incoming = clip(second, 0.5, 1.5, 1.0);
        incoming.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.6,
        });
        let tl = timeline(vec![vec![clip(first, 0.5, 1.5, 0.0), incoming]], landscape);
        run(
            &format!("convert/{name} through Crossfade, letterboxed"),
            &tl,
            &assets,
            30.0,
            2.0,
        );
        for kind in [TransitionKind::SlideLeft, TransitionKind::PushUp] {
            let mut incoming = clip(second, 0.5, 1.5, 1.0);
            incoming.transition_in = Some(Transition { kind, duration: 0.6 });
            let tl = timeline(vec![vec![clip(first, 0.5, 1.5, 0.0), incoming]], landscape);
            run(
                &format!("convert/{name} through {kind:?}, letterboxed"),
                &tl,
                &assets,
                30.0,
                2.0,
            );
        }
        let mut incoming = clip(second, 0.5, 1.5, 1.0);
        incoming.transition_in = Some(Transition {
            kind: TransitionKind::DipToBlack,
            duration: 0.6,
        });
        let tl = timeline(vec![vec![clip(first, 0.5, 1.5, 0.0), incoming]], landscape);
        // (A dip to black is a fade to black on a plain chain, ahead of the converter `scale`: the compositor fades the planes and then
        // converts them, every frame.)
        run(
            &format!("convert/{name} through DipToBlack, letterboxed"),
            &tl,
            &assets,
            30.0,
            2.0,
        );
    }
    // The chain of a phone vlog: four clips of alternating matrices, each dissolving into the one before, and every combination of
    // plain and dissolving over three clips. The first dissolving clip decides the matrix (BT.601 in the vlog), the others are
    // converted into it once, wherever they are. A plain clip is converted by a `scale` of its own after its chain and drawn to
    // the level; a **dissolving clip's chain ends in `format=yuva420p`, and its last `scale` converts the matrix itself, in one
    // pass with its resize**: swscale's cascade, `yuv420p -> bgr24` at the destination's size in one scaler and `ff_rgb24toyv12` back
    // (a shrunk phone clip is 22 % of its luma off, up to 17 levels, when the conversion is done after the resize instead). The
    // compositor reproduces the cascade for a picture that shrinks (`kerf_gpu::convert::scaled_to_rgb24`, probed): all of it is drawn
    // here, every sample.
    let crossfade = |mut c: Clip| {
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.4,
        });
        c
    };
    let sequence = |clips: &[(&Asset, bool)], delivery: Option<Delivery>| {
        let track: Vec<Clip> = clips
            .iter()
            .enumerate()
            .map(|(i, (a, dissolving))| {
                let c = clip(a, 0.5, 1.5, i as f64);
                if *dissolving {
                    crossfade(c)
                } else {
                    c
                }
            })
            .collect();
        let mut assets: Vec<Asset> = vec![];
        for (a, _) in clips {
            if !assets.iter().any(|b| b.id == a.id) {
                assets.push((*a).clone());
            }
        }
        (timeline(vec![track], delivery), assets)
    };
    let (p709, p601) = (&x.portrait709, &x.portrait601);
    type Chain<'a> = (&'a str, Vec<(&'a Asset, bool)>);
    let chains: [Chain; 7] = [
        (
            "vlog of four, every one dissolving",
            vec![(p709, false), (p601, true), (p709, true), (p601, true)],
        ),
        (
            "709, 601 dissolving, 709 plain",
            vec![(p709, false), (p601, true), (p709, false)],
        ),
        (
            "709, 601 dissolving, 709 dissolving",
            vec![(p709, false), (p601, true), (p709, true)],
        ),
        (
            "709, 709 dissolving, 601 plain",
            vec![(p709, false), (p709, true), (p601, false)],
        ),
        (
            "709, 709 plain, 601 dissolving",
            vec![(p709, false), (p709, false), (p601, true)],
        ),
        (
            "601, 709 dissolving, 601 dissolving",
            vec![(p601, false), (p709, true), (p601, true)],
        ),
        (
            "709, 601 plain, 601 dissolving",
            vec![(p709, false), (p601, false), (p601, true)],
        ),
    ];
    for (name, clips) in &chains {
        let (tl, assets) = sequence(clips, landscape);
        let secs = clips.len() as f64;
        run(&format!("convert/chain: {name}"), &tl, &assets, 30.0, secs);
    }
    // The same chains of pictures of the canvas's size (the chain does not resize them): a dissolving clip that is converted is
    // converted as a plain one is, and all of it is drawn.
    let (w709, w2020, w601) = (&v.testsrc709, &b.bt2020, &m.testsrc);
    for (name, clips) in [
        (
            "unscaled: 709, 601 dissolving, 709 dissolving",
            vec![(w709, false), (w601, true), (w709, true)],
        ),
        (
            "unscaled: 709, 2020 dissolving, 601 dissolving, 709 plain",
            vec![(w709, false), (w2020, true), (w601, true), (w709, false)],
        ),
    ] {
        let (tl, assets) = sequence(&clips, None);
        run(&format!("convert/chain: {name}"), &tl, &assets, 30.0, clips.len() as f64);
    }
    let assets = [v.testsrc709.clone(), m.testsrc.clone()];
    // ... and on the bottom clip, which is not converted: drawn throughout.
    let mut faded = clip(&v.testsrc709, 0.5, 1.5, 0.0);
    faded.fade_in = 0.4;
    faded.fade_out = 0.4;
    let tl = timeline(vec![vec![faded, clip(&m.testsrc, 0.5, 1.5, 1.0)]], None);
    run("convert/fade-in and out on the bottom clip", &tl, &assets, 30.0, 2.0);
    // Speed does not matter to the conversion.
    let mut quick = clip(&m.testsrc, 0.5, 2.5, 1.0);
    quick.speed = 2.0;
    let tl = timeline(vec![vec![clip(&v.testsrc709, 0.5, 1.5, 0.0), quick]], None);
    run("convert/speed 2", &tl, &assets, 30.0, 2.0);
    // Drawn, converted, with what comes before the conversion in the chain: a pointwise effect (a table the scale applies), a 4:2:2 or
    // 4:4:4 picture (the decode makes the 4:2:0 a `format=yuv420p` makes of it), a plain layer over a blurred fill (the whole filled
    // frame is converted).
    for (name, effect) in [
        ("invert", kerf_core::VideoEffect::Invert),
        ("grayscale", kerf_core::VideoEffect::Grayscale),
    ] {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 1.0);
        c.effects = vec![effect];
        let tl = timeline(vec![vec![clip(&v.testsrc709, 0.5, 1.5, 0.0), c]], None);
        run(&format!("convert/{name} on the converted clip"), &tl, &assets, 30.0, 2.0);
    }
    for (name, picture) in [("4:4:4", &v.yuv444), ("4:2:2", &v.yuv422)] {
        let tl = timeline(
            vec![vec![clip(&v.testsrc709, 0.5, 1.5, 0.0), clip(picture, 0.5, 1.5, 1.0)]],
            None,
        );
        run(
            &format!("convert/{name} after a 709 clip"),
            &tl,
            &[v.testsrc709.clone(), picture.clone()],
            30.0,
            2.0,
        );
        let tl = timeline(
            vec![vec![clip(&v.testsrc709, 0.5, 1.5, 0.0)], vec![clip(picture, 0.5, 1.5, 0.0)]],
            None,
        );
        run(
            &format!("convert/{name} over a 709 clip"),
            &tl,
            &[v.testsrc709.clone(), picture.clone()],
            30.0,
            1.0,
        );
    }
    let blurred = Some(Delivery::new(960, 540, Fit::Blur));
    for (name, first, second) in [
        ("phone 709 then phone 601", &x.portrait709, &x.portrait601),
        ("phone 601 then phone 709", &x.portrait601, &x.portrait709),
    ] {
        let tl = timeline(vec![vec![clip(first, 0.5, 1.5, 0.0), clip(second, 0.5, 1.5, 1.0)]], blurred);
        run(
            &format!("convert/{name}, over a blurred fill"),
            &tl,
            &[first.clone(), second.clone()],
            30.0,
            2.0,
        );
    }
    // What the conversion does not cover keeps the frame FFmpeg's, in every frame of the cut: a colour correction (its `eq` may carry a
    // converter ahead of the overlay), an opacity or a rotation (an RGB round trip, a `rotate`: a clip so put on the alpha plane may
    // be the one that decides the composite's matrix, and which it is has not been measured: the deciding clip is not read), an effect.
    type Edit<'a> = &'a dyn Fn(&mut Clip);
    let cases: [(&str, Edit); 4] = [
        ("colour correction", &|c| {
            c.color = Color {
                contrast: 1.2,
                ..Color::default()
            }
        }),
        ("an opacity", &|c| c.transform.opacity = 0.6),
        ("an effect", &|c| {
            c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 2.0 }];
        }),
        ("a rotation", &|c| c.transform.rotation = 10.0),
    ];
    for (name, edit) in cases {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 1.0);
        edit(&mut c);
        let tl = timeline(vec![vec![clip(&v.testsrc709, 0.5, 1.5, 0.0), c]], None);
        let case = format!("convert/refused: {name}");
        if negotiates {
            run_refusing(&case, &tl, &assets, 30.0, 2.0, &matrix_conversion_refused());
        } else {
            // (Under a fixed matrix nothing is converted and these are drawn as they are, a translucent layer exact like any other;
            // a turn that is not a quarter is no layer's to draw under any matrix.)
            match name {
                "an opacity" => run(&case, &tl, &assets, 30.0, 2.0),
                "a rotation" => run_refusing(&case, &tl, &assets, 30.0, 2.0, &rotation_refused()),
                _ => run(&case, &tl, &assets, 30.0, 2.0),
            }
        }
    }
    let _ = one(&m.testsrc);
}

// ---- the blurred fill (`Fit::Blur`) -------------------------------------------------------------------
//
// A clip placed by the plain fit whose picture is not the frame's shape sits over a copy of itself, blurred: the
// thumbnail chain of `engine::cli::blur_fill` (`split`, `scale` to cover an eighth of the frame on each side with
// `flags=bilinear`, `crop`, `boxblur=r:2`, `scale` back up, `overlay` of the picture's own fit). The compositor draws it
// pass for pass, on swscale's own tables and kernels (`scaler.rs`) and `boxblur`'s own integers (`fill.rs`), and the
// planes are the export's to the sample (no case here names a bound).

/// Odd-sized, thin and tagged sources for the fill: lossless FFV1 (H.264 refuses odd sides), `yuv420p`.
struct BlurMedia {
    /// 361x641 and 643x363: a thumbnail cover and a fit with odd sides, and a chroma plane of odd size.
    odd_portrait: Asset,
    odd_landscape: Asset,
    /// 480x24 and 24x480: a cover that crops all but a sliver, an aspect ratio of 20:1.
    thin_wide: Asset,
    thin_tall: Asset,
    /// The test pattern tagged BT.2020 (non-constant).
    bt2020: Asset,
}

fn blur_media() -> &'static BlurMedia {
    static B: OnceLock<BlurMedia> = OnceLock::new();
    B.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-blur");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, lavfi: &str| -> Asset {
            let p = dir.join(name);
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                lavfi,
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            let a = probed(&p);
            assert_eq!(a.streams[0].pix_fmt.as_deref(), Some("yuv420p"), "{name}");
            a
        };
        let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        BlurMedia {
            odd_portrait: make("odd-portrait.mkv", &format!("testsrc2=size=361x641:rate=30:duration=4,{untagged}")),
            odd_landscape: make("odd-landscape.mkv", &format!("testsrc2=size=643x363:rate=30:duration=4,{untagged}")),
            thin_wide: make("thin-wide.mkv", &format!("testsrc2=size=480x24:rate=30:duration=4,{untagged}")),
            thin_tall: make("thin-tall.mkv", &format!("testsrc2=size=24x480:rate=30:duration=4,{untagged}")),
            bt2020: make(
                "bt2020.mkv",
                "testsrc2=size=640x360:rate=30:duration=4,setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
            ),
        }
    })
}

fn blurred(w: u32, h: u32) -> Option<Delivery> {
    Some(Delivery::new(w, h, Fit::Blur))
}

/// Half a second of `asset` from the middle of its four, alone in a `w` x `h` frame that fits over a blurred copy.
fn filled(case: &str, asset: &Asset, w: u32, h: u32) {
    let tl = timeline(vec![vec![clip(asset, 0.5, 1.5, 0.0)]], blurred(w, h));
    run(case, &tl, std::slice::from_ref(asset), 30.0, 0.5);
}

/// The fill in the shapes a delivery has: a portrait phone clip in a landscape frame, the other way round, a square,
/// the frame sizes whose eighth is not a whole number (a thumbnail of `2*floor(w/16)`), the radius from its smallest
/// (1) to a large one (16 at 4K), a frame that is the shape of the picture (nothing to fill).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_is_drawn_as_the_export_draws_it() {
    let m = media();
    // The shapes: portrait in landscape (the T5b vlog), landscape in portrait, both in a square.
    filled("blur/portrait in 640x360", &m.portrait, 640, 360);
    filled("blur/landscape in 360x640", &m.testsrc, 360, 640);
    filled("blur/portrait in 480x480", &m.portrait, 480, 480);
    filled("blur/landscape in 480x480", &m.testsrc, 480, 480);
    // Busy pictures: the thumbnail's scale and the blur see every sample (noise in all three planes), hard edges.
    filled("blur/noise in 360x640", &m.noise, 360, 640);
    filled("blur/edges in 360x640", &m.edges, 360, 640);
    filled("blur/bars in 360x640", &m.bars, 360, 640);
    filled("blur/ramp in 360x640", &m.ramp, 360, 640);
    // A picture the shape of the frame has no bars: the fill is behind all of it, and not there.
    filled("blur/same shape (640x360 in 640x360)", &m.testsrc, 640, 360);
    filled("blur/same shape, enlarged (640x360 in 1280x720)", &m.testsrc, 1280, 720);
    // The frame sizes: a thumbnail of 2*floor(w/16) by 2*floor(h/16), a radius of a sixteenth of its smaller side.
    for (w, h) in [
        (646, 358),
        (358, 646),
        (320, 180),
        (180, 320),
        (240, 136),
        (96, 64),
        (1280, 720),
    ] {
        filled(&format!("blur/portrait in {w}x{h}"), &m.portrait, w, h);
        // (640x360 into a thumbnail under 16 samples wide is a shrink of more than 40:1, which the plan leaves to FFmpeg.)
        if w.min(h) >= 136 {
            filled(&format!("blur/noise in {w}x{h}"), &m.noise, w, h);
        }
    }
    // The largest radii: 1920x1080 is the T5b shape (a radius of 8), 4K a radius of 16 (the picture is enlarged to it).
    filled("blur/portrait in 1920x1080", &m.portrait, 1920, 1080);
    filled("blur/noise in 1080x1920", &m.noise, 1080, 1920);
    let tl = timeline(vec![vec![clip(&m.portrait, 0.5, 1.5, 0.0)]], blurred(3840, 2160));
    run(
        "blur/portrait in 3840x2160",
        &tl,
        std::slice::from_ref(&m.portrait),
        10.0,
        0.3,
    );
}

/// Odd sides everywhere: a picture with odd sides (its cover, its fit and its chroma planes), a frame whose fit comes
/// out odd (the half block `overlay` copies at the picture's edge), thin pictures (a 20:1 cover crops all but a
/// sliver of it).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_behind_odd_and_thin_pictures() {
    let m = media();
    let b = blur_media();
    for (name, a) in [
        ("odd portrait 361x641", &b.odd_portrait),
        ("odd landscape 643x363", &b.odd_landscape),
    ] {
        for (w, h) in [(640, 360), (360, 640), (480, 480), (320, 180), (646, 358)] {
            filled(&format!("blur/{name} in {w}x{h}"), a, w, h);
        }
    }
    for (name, a) in [("thin wide 480x24", &b.thin_wide), ("thin tall 24x480", &b.thin_tall)] {
        for (w, h) in [(640, 360), (360, 640), (480, 480)] {
            filled(&format!("blur/{name} in {w}x{h}"), a, w, h);
        }
    }
    // A fit with odd sides in an even frame: 640x360 into 358x646 is 358x201 (odd), centred by `overlay` at an even offset.
    filled("blur/odd fit (640x360 in 358x646)", &m.testsrc, 358, 646);
    filled(
        "blur/odd fit (640x360 in 360x641 is refused: odd frame)",
        &m.testsrc,
        360,
        640,
    );
}

/// The clip's own fades, a dip, a dissolve and the eight slides and pushes: the fill is part of the padded frame the
/// filters after it see (`fade` acts on it, `overlay` travels it, an alpha ramp blends it).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_fades_dissolves_and_travels_with_its_clip() {
    let m = media();
    let assets = [m.portrait.clone(), m.testsrc.clone(), m.bars.clone(), m.noise.clone()];
    let frame = blurred(640, 360);
    // The clip's own fades (a fade to black of the whole padded frame, bars included).
    let mut c = clip(&m.portrait, 0.5, 1.5, 0.0);
    c.fade_in = 0.4;
    c.fade_out = 0.4;
    run("blur/fade in and out", &timeline(vec![vec![c]], frame), &assets, 30.0, 1.0);
    // Dissolves: a fill into a fill, a fill into a picture of the frame's shape, and the other way round.
    for (name, from, to) in [
        ("portrait to portrait", &m.portrait, &m.portrait),
        ("portrait to landscape", &m.portrait, &m.testsrc),
        ("landscape to portrait", &m.testsrc, &m.portrait),
        ("portrait to noise", &m.portrait, &m.noise),
    ] {
        for (kind_name, kind, secs) in [
            ("dissolve", TransitionKind::Crossfade, 0.6),
            ("short dissolve", TransitionKind::Crossfade, 0.1),
            ("dip to black", TransitionKind::DipToBlack, 0.6),
        ] {
            let out = clip(from, 0.5, 1.5, 0.0);
            let mut inc = clip(to, 0.5, 1.5, 1.0);
            inc.transition_in = Some(Transition { kind, duration: secs });
            let tl = timeline(vec![vec![out, inc]], frame);
            run(&format!("blur/{kind_name} {name}"), &tl, &assets, 30.0, 2.0);
        }
    }
    // The slides and pushes move the padded frame as a whole.
    for kind in [
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ] {
        let out = clip(&m.portrait, 0.5, 1.5, 0.0);
        let mut inc = clip(&m.portrait, 1.0, 2.0, 1.0);
        inc.transition_in = Some(Transition { kind, duration: 0.5 });
        let tl = timeline(vec![vec![out, inc]], frame);
        run(&format!("blur/{kind:?} portrait to portrait"), &tl, &assets, 30.0, 2.0);
    }
    // A dip to white whitens the bars too, through an RGB round trip the compositor draws for the picture alone: refused
    // (the plan says why: a letterbox), and the frames either side of it are the export's.
    let out = clip(&m.portrait, 0.5, 1.5, 0.0);
    let mut inc = clip(&m.portrait, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 0.6,
    });
    run_refusing(
        "blur/dip to white",
        &timeline(vec![vec![out, inc]], frame),
        &assets,
        30.0,
        2.0,
        &dip_refused(DipWhy::Letterboxed),
    );
}

/// Over and under other tracks: the fill is a whole frame, so what is below a filled clip is covered, and what is above
/// it is drawn on it as on any frame.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_covers_what_is_below_it_and_is_drawn_on_by_what_is_above() {
    let m = media();
    let assets = [m.bars.clone(), m.portrait.clone(), m.testsrc.clone(), m.noise.clone()];
    let frame = blurred(640, 360);
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![clip(&m.portrait, 0.5, 1.5, 0.0)]],
        frame,
    );
    run("blur/over bars", &tl, &assets, 30.0, 1.0);
    // A picture-in-picture over a filled clip (the PiP is placed with Contain's chain, whatever the delivery's fit).
    let mut pip = clip(&m.testsrc, 0.5, 1.5, 0.0);
    pip.transform = kerf_core::Transform {
        scale: 0.4,
        pos_x: 0.3,
        pos_y: -0.25,
        ..kerf_core::Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.portrait, 0.5, 1.5, 0.0)], vec![pip]], frame);
    run("blur/under a pip", &tl, &assets, 30.0, 1.0);
    // A filled clip that starts later and ends earlier than the one below.
    let tl = timeline(
        vec![vec![clip(&m.noise, 0.0, 2.0, 0.0)], vec![clip(&m.portrait, 0.5, 1.0, 0.5)]],
        frame,
    );
    run("blur/over noise, in the middle", &tl, &assets, 30.0, 2.0);
    // Two filled clips, one over the other: the upper covers the lower.
    let tl = timeline(
        vec![vec![clip(&m.portrait, 0.5, 1.5, 0.0)], vec![clip(&m.portrait, 1.5, 2.5, 0.0)]],
        frame,
    );
    run("blur/over another fill", &tl, &assets, 30.0, 1.0);
    // A clip the fill does not suit is placed with Contain's chain, byte for byte: a zoomed one, a moved one, a rotated one,
    // a translucent one, a masked one.
    for (name, edit) in [
        (
            "zoomed out",
            Box::new(|c: &mut Clip| c.transform.scale = 0.5) as Box<dyn Fn(&mut Clip)>,
        ),
        ("moved", Box::new(|c: &mut Clip| c.transform.pos_x = 0.2)),
    ] {
        let mut c = clip(&m.portrait, 0.5, 1.5, 0.0);
        edit(&mut c);
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], frame);
        run(&format!("blur/not taken: {name}"), &tl, &assets, 30.0, 1.0);
    }
}

/// Colour, tables, speed and the kinds of source: `eq` acts on the whole padded frame (the fill included, its tables
/// applied in the vertical pass that scales it up), a keyed colour builds them again every frame, grayscale and invert
/// are tables too; a clip at another speed, reversed, entering late or a still picture is the same fill.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_goes_through_the_clips_colour_and_timing() {
    use kerf_core::{Property, VideoEffect as E};
    let m = media();
    let assets = [m.portrait.clone(), m.testsrc.clone(), m.late.clone()];
    let frame = blurred(640, 360);
    let one = |a: &Asset, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(a, 0.5, 1.5, 0.0);
        edit(&mut c);
        timeline(vec![vec![c]], frame)
    };
    for (name, color) in [
        (
            "bright",
            Color {
                brightness: 0.2,
                ..Color::default()
            },
        ),
        (
            "dark and flat",
            Color {
                brightness: -0.25,
                contrast: 0.6,
                ..Color::default()
            },
        ),
        (
            "contrast",
            Color {
                contrast: 1.6,
                ..Color::default()
            },
        ),
        (
            "grey",
            Color {
                saturation: 0.0,
                ..Color::default()
            },
        ),
        (
            "vivid",
            Color {
                saturation: 2.0,
                ..Color::default()
            },
        ),
        (
            "gamma",
            Color {
                gamma: 1.15,
                ..Color::default()
            },
        ),
        (
            "warm",
            Color {
                temperature: 0.7,
                ..Color::default()
            },
        ),
        (
            "all five",
            Color {
                brightness: 0.1,
                contrast: 1.3,
                saturation: 1.4,
                gamma: 1.1,
                temperature: -0.4,
            },
        ),
    ] {
        run(
            &format!("blur/eq {name}"),
            &one(&m.portrait, &|c| c.color = color),
            &assets,
            30.0,
            0.5,
        );
    }
    // (`negate` maps limited range to limited range since FFmpeg 5; 4.4's inverts the byte, which no `invert` case of this
    // suite is exact against: the cases that invert are skipped where the build's `negate` does that.)
    let negate_ranged = negate_keeps_limited_range();
    for (name, effects) in [
        ("grayscale", vec![E::Grayscale]),
        ("invert", vec![E::Invert]),
        ("gray then invert", vec![E::Grayscale, E::Invert]),
    ] {
        if !negate_ranged && effects.contains(&E::Invert) {
            eprintln!("blur/{name}: skipped, this FFmpeg's `negate` inverts the byte (255 - v)");
            continue;
        }
        run(
            &format!("blur/{name}"),
            &one(&m.portrait, &|c| c.effects = effects.clone()),
            &assets,
            30.0,
            0.5,
        );
    }
    let mut keyed = colour_keyed(&m.portrait, &[Property::Brightness, Property::Contrast], &linear_keys);
    keyed.transform = kerf_core::Transform::default();
    run_colour(
        "blur/keyed colour",
        &timeline(vec![vec![keyed]], frame),
        &assets,
        30.0,
        1.0,
        10,
    );
    // Timing.
    run("blur/speed 2", &one(&m.portrait, &|c| c.speed = 2.0), &assets, 30.0, 0.5);
    run("blur/speed 0.5", &one(&m.portrait, &|c| c.speed = 0.5), &assets, 30.0, 0.5);
    run("blur/reversed", &one(&m.portrait, &|c| c.speed = -1.0), &assets, 30.0, 0.5);
    run("blur/at 24 fps", &one(&m.portrait, &|_| {}), &assets, 24.0, 0.5);
    // A source whose picture starts late (a still image is RGB or full range: refused, below).
    let tl = timeline(vec![vec![clip(&m.late, 0.5, 1.5, 0.0)]], blurred(360, 640));
    run("blur/late-starting source in 360x640", &tl, &assets, 30.0, 1.0);
}

/// Whether this FFmpeg's `negate` takes limited-range black (16) to limited-range white (235), as the compositor's table
/// does, and not to 239 (`255 - v`, FFmpeg 4.4's).
fn negate_keeps_limited_range() -> bool {
    let out = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=16x16:d=1,format=yuv420p,negate",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ]);
    out.first() == Some(&235)
}

/// Every matrix and range a stack can have: the fill has no colour conversion, so the planes are the export's whatever the
/// tag — BT.601 (untagged, `smpte170m`, `bt470bg`), BT.709, BT.2020 — and limited range, a `tv` flag on a plain
/// `yuv420p`. A `yuv420p` flagged `pc` is FFmpeg's raw samples (refused: the decode converts the range ahead of the fill).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_in_every_matrix_and_range() {
    let m = media();
    let v = variants();
    let b = blur_media();
    let r = range_media();
    let frame = blurred(360, 640);
    for (name, a) in [
        ("untagged", &m.testsrc),
        ("bt709", &v.testsrc709),
        ("smpte170m", &v.testsrc170m),
        ("bt470bg", &v.testsrc470bg),
        ("bt2020", &b.bt2020),
        ("flagged tv", &r.tv),
        ("flagged nothing", &r.unflagged),
    ] {
        let tl = timeline(vec![vec![clip(a, 0.5, 1.5, 0.0)]], frame);
        run(&format!("blur/{name}"), &tl, std::slice::from_ref(a), 30.0, 0.5);
    }
    // A BT.709 clip beside an untagged one: one matrix under FFmpeg 6, two under FFmpeg 9, where the layer of the other matrix is
    // converted into the first clip's after the whole frame (the fill included: a plain layer over a blurred fill is converted
    // with its fill, `FixedBt601` draws both as they are), in either order.
    for (name, first, second) in [
        ("709 over untagged", &m.bars, &v.testsrc709),
        ("untagged over 709", &v.testsrc709, &m.bars),
    ] {
        let tl = timeline(
            vec![vec![clip(first, 0.5, 1.5, 0.0)], vec![clip(second, 0.5, 1.5, 0.0)]],
            frame,
        );
        let assets = [first.clone(), second.clone()];
        run(&format!("blur/{name}"), &tl, &assets, 30.0, 0.5);
    }
}

/// How far the compositor's planes are from the export's when the plan is told the pictures are plain `yuv420p`,
/// whatever they are: a refusal's frames drawn anyway, to show the refusal is needed and not just cautious. (The decode
/// reduces every format to 8-bit limited-range 4:2:0 whatever the plan says.)
fn blur_drawn_anyway(case: &str, tl: &Timeline, assets: &[Asset]) -> (i32, i32) {
    let opts = at_fps(30.0);
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let played = export(&format!("anyway-{slug}"), tl, assets, &opts);
    let lied: Vec<Asset> = assets
        .iter()
        .map(|a| {
            let mut a = a.clone();
            for s in a.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
                s.pix_fmt = Some("yuv420p".into());
                s.color_range = None;
            }
            a
        })
        .collect();
    let planner = Planner::new(tl, &lied, &opts, PlanRequest::motion(policy())).expect("planner");
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let (mut y, mut uv) = (0, 0);
    for k in 0..9u64 {
        let plan = planner.at_frame(k).expect("plan");
        let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let drawn = compositor()
            .composite_yuv_counted(&plan, &picture, motion.counts(), size)
            .unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let want = played.planes(k as usize);
        let got = planes_of(&drawn);
        y = y.max(PlaneStats::of(want[0], got[0]).max);
        uv = uv.max(PlaneStats::of(want[1], got[1]).max.max(PlaneStats::of(want[2], got[2]).max));
    }
    eprintln!("{case:<44} drawn anyway: Y max {y}, UV max {uv}");
    (y, uv)
}

/// What the fill is refused for: pictures that are not 8-bit `yuv420p` (FFmpeg scales and blurs the thumbnail in the
/// picture's own format: 10-bit, 4:4:4, 4:2:2, gray, RGB, `yuvj`), a `yuv420p` flagged full range (raw samples), a frame
/// too small for the box blur, an HDR picture. Each is refused by the plan with FFmpeg rendering it, and each refusal is
/// shown necessary: the frame drawn anyway is not the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_is_refused_where_it_is_not_the_exports() {
    let m = media();
    let v = variants();
    let r = range_media();
    let frame = blurred(360, 640);
    let fill_refused = refusing("BlurFillWith", |u| matches!(u, Unsupported::BlurFillWith(..)));
    let negotiated = policy() != CompositeColorPolicy::FixedBt601;
    for (name, a) in [
        ("10-bit", &v.tenbit),
        ("4:4:4", &v.yuv444),
        ("4:2:2", &v.yuv422),
        ("gray", &v.gray),
        ("rgb png", &v.rgb_png),
        ("rgb png still image", &m.image),
        ("full-range mjpeg", &v.mjpeg),
        ("full-range jpeg", &v.jpeg),
        ("yuv420p flagged pc", &r.pc),
    ] {
        let image = a.streams[0].image;
        let tl = timeline(
            vec![vec![clip(
                a,
                if image { 0.0 } else { 0.5 },
                if image { 1.0 } else { 1.5 },
                0.0,
            )]],
            frame,
        );
        let assets = std::slice::from_ref(a);
        // (A picture FFmpeg 9 may read raw makes the whole export a different one: the cut is refused as such there.)
        let cut_refused = negotiated && matches!(name, "10-bit" | "full-range mjpeg" | "full-range jpeg" | "yuv420p flagged pc");
        let planner = Planner::new(&tl, assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
        let plan = planner.at_frame(3).expect("plan");
        let why = plan.reasons(&compositor().caps(), plan.size(u32::MAX));
        assert!(
            why.iter().any(|u| (fill_refused.is)(u))
                || (cut_refused && why.iter().any(|u| matches!(u, Unsupported::FullRangeCut))),
            "blur/{name}: {why:?}"
        );
        // (An RGB picture has no matrix: the plan refuses the stack for that as well, so it cannot be drawn anyway.)
        if name.starts_with("rgb") {
            continue;
        }
        let (y, uv) = blur_drawn_anyway(&format!("blur/{name}"), &tl, assets);
        // (FFmpeg's thumbnail of a 4:4:4 or RGB picture is scaled from its own chroma; the others read raw or high-bit samples.)
        assert!(
            y > 0 || uv > 0,
            "blur/{name}: the refusal is not needed: the frames drawn anyway are the export's"
        );
    }
    // A thumbnail the picture is shrunk into at 45:1 (360x640 into an 8x14 cover): past the 40:1 the plan used to stop at, and
    // drawn since the scaler comparison went on to 63:1.
    for (w, h) in [(64, 64), (64, 96)] {
        let tl = timeline(vec![vec![clip(&m.portrait, 0.5, 1.5, 0.0)]], blurred(w, h));
        run(
            &format!("blur/portrait in {w}x{h} (a 45:1 thumbnail)"),
            &tl,
            std::slice::from_ref(&m.portrait),
            30.0,
            0.5,
        );
    }
    // A frame too small for the thumbnail's box blur (a 4x4 thumbnail has 2-sample chroma lines; the box reads past them).
    let tl = timeline(vec![vec![clip(&m.portrait, 0.5, 1.5, 0.0)]], blurred(32, 32));
    let planner = Planner::new(
        &tl,
        std::slice::from_ref(&m.portrait),
        &at_fps(30.0),
        PlanRequest::motion(policy()),
    )
    .expect("planner");
    let plan = planner.at_frame(3).expect("plan");
    let why = plan.reasons(&compositor().caps(), plan.size(u32::MAX));
    assert!(why.iter().any(|u| (fill_refused.is)(u)), "blur/32x32: {why:?}");
    eprintln!("blur/32x32                                   refused: {why:?}");
}

/// What runs on the padded frame and is not drawn for a letterbox stays FFmpeg's under a fill: a blur, a sharpen, a vignette
/// or a chroma key (they would see the fill as picture), a grade (the bars go through the RGB round trip with the
/// picture and meet its chroma in the way back). The frames beside the refusal are the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_runs_on_the_padded_frame_stays_ffmpegs_under_a_fill() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let frame = blurred(640, 360);
    let matte = refusing("Matte", |u| matches!(u, Unsupported::Matte(..)));
    for (name, effect) in [
        ("blur", E::Blur { sigma: 3.0 }),
        ("sharpen", E::Sharpen { amount: 0.5 }),
        ("vignette", E::Vignette),
    ] {
        let mut c = clip(&m.portrait, 0.5, 1.5, 0.0);
        c.effects = vec![effect];
        run_refusing(
            &format!("blur/{name} on a filled clip"),
            &timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], frame),
            &[m.bars.clone(), m.portrait.clone()],
            30.0,
            0.5,
            &matte,
        );
    }
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
        return run(case, tl, assets, fps, secs);
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
        forced_blends(|| run(&format!("{case} (forced)"), tl, assets, fps, secs));
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

/// What no case holds to the export is refused, in the plan, and FFmpeg draws it: a layer the compositor scales, a rotated one (a
/// fixed-point edge), one below full opacity (the blend chain converts the clip's colour and alpha its own way after the round trip,
/// which is itself exact), a letterboxed one (its bars are transparent in the blend), an RGB picture (the decode's 4:2:0 is a few
/// levels from the graph's conversion).
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
        // (DipToWhite is left out: its round trip is another case's, `a_dip_to_white_...`.)
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
    // Angles a hair off a quarter turn: within `quarter_turn`'s window (the `rotate` fast path's, `FLT_EPSILON` radians) they are drawn
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
/// clip plain (no blend mode) is held to the export too, and says in its failure whether a mismatch is the blend's or the turn's.
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
                    failures.push(format!("{case}: {why} (round {k})"));
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

/// `ce_check` for a layer whose picture the compositor's scaler or a crop touches. Every drawn frame must be the export's; when the keyed cut
/// differs the same cut with a constant opacity (the control) is run too, and the failure says whether the control differs as well (which
/// tells a fault of the keyed path from one of the plain layer under it). Either way it is a failure.
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
) -> CeScan {
    let scan = ce_scan(case, tl, assets, &at_fps(fps), upto(secs, fps), false);
    eprintln!("{}", scan.line(case));
    CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
    let Some(why) = scan.verdict(draws) else { return scan };
    if let (Some(control), true) = (control, !scan.exact()) {
        let name = format!("{case} (control: constant opacity)");
        let c = ce_scan(&name, control, assets, &at_fps(fps), upto(secs, fps), false);
        eprintln!("{}", c.line(&name));
        failures.push(format!(
            "{case}: {why} -- the control {} (Y {} UV {})",
            if c.exact() { "is exact" } else { "differs too" },
            c.y.differing,
            c.uv.differing
        ));
        return scan;
    }
    failures.push(format!("{case}: {why}"));
    scan
}

fn r3_finish(what: &str, mut failures: Vec<String>) {
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
                    );
                }
            }
        }
    }
    r3_finish("checker_e_r3_a_turned_keyed_opacity_at_odd_sizes_and_offsets", failures);
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
    r3_finish("checker_e_r3_a_turned_keyed_opacity_with_a_mask_a_crop_or_a_zoom", failures);
}

/// A keyed opacity on a turned layer inside a transition (the incoming clip, the outgoing, both), plain and blended, with a fade of its own.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_a_turned_keyed_opacity_in_a_transition() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures = Vec::new();
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
                    );
                }
            }
        }
    }
    r3_finish("checker_e_r3_a_turned_keyed_opacity_in_a_transition", failures);
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
                );
            }
        }
    }
    r3_finish("checker_e_r3_a_turned_keyed_opacity_of_other_formats", failures);
}

/// Partly or wholly off the canvas, and two turned layers stacked (each with its own keyed opacity), plain and blended.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_turned_keyed_opacities_off_the_canvas_and_stacked() {
    use kerf_core::BlendMode;
    let m = media();
    let mut failures = Vec::new();
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
            );
        }
    }
    r3_finish("checker_e_r3_turned_keyed_opacities_off_the_canvas_and_stacked", failures);
}

/// A keyed opacity on a rotation that is not a quarter turn: refused or drawn exactly. (A plain rotated layer is refused by the plan, its
/// edge being a fixed-point sample; a blended one is refused too.)
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
    // The control, held to the export too: the same keyed rotation with a constant opacity, to say whether a difference is the opacity's.
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
    if let Some(why) = control.verdict(None) {
        failures.push(format!("{case}: {why}"));
    }
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

/// Whether the compositor's scaler resizes the picture of the last layer at frame 3: a stage whose window is not the size it is scaled to.
fn r3_resized(tl: &Timeline, assets: &[Asset], fps: f64) -> bool {
    let opts = at_fps(fps);
    let Ok(planner) = Planner::new(tl, assets, &opts, PlanRequest::motion(policy())) else {
        return false;
    };
    let Ok(plan) = planner.at_frame(3) else { return false };
    let size = (plan.canvas.width, plan.canvas.height);
    plan.layers.last().is_some_and(|l| {
        plan.layer_geometry(l, size)
            .is_ok_and(|g| g.stages.iter().any(|s| (s.src.w, s.src.h) != s.scaled))
    })
}

/// Odd-sided layers: a 4:4:4, 4:2:2, gray or 4:2:0 source cropped to an odd width / height (their native chroma grids do not round a crop to
/// even), turned by every quarter, opaque and faded, plain and blended. `resized` selects the cases whose picture the compositor's scaler
/// resizes (a crop of one column, or both, is fitted back to the frame): those are the scaler's (lane B's), and the plain ones among them are
/// held to the export by [`checker_e_r3_odd_sided_layers_the_scaler_resizes_plain`], the rest here. The geometry of each is printed so that a
/// case that is not odd says so.
fn r3_odd_sided(what: &str, resized: bool) -> (Vec<String>, usize) {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let mut failures = Vec::new();
    let mut tally: Vec<String> = Vec::new();
    let mut cases = 0;
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
                        // (The scaler's cases: a plain layer it resizes. A blended one is refused whatever the scaler draws, and stays here.)
                        if (resized && (mode.is_some() || !r3_resized(&tl, &assets, 30.0)))
                            || (!resized && mode.is_none() && r3_resized(&tl, &assets, 30.0))
                        {
                            continue;
                        }
                        cases += 1;
                        let case = format!(
                            "checker/e/r3/odd crop {name} {:.0},{:.0},{:.0},{:.0} turned {angle} {mode:?} faded {faded}",
                            l * 640.0,
                            r * 640.0,
                            t * 360.0,
                            b * 360.0
                        );
                        let geom = r3_geometry(&tl, &assets, 30.0);
                        let scan = r3_check(&case, &tl, None, &assets, 30.0, 0.4, None, &mut failures);
                        tally.push(format!("{case}: {geom}: drawn {} of {}", scan.drawn, scan.frames));
                    }
                }
            }
        }
    }
    for t in &tally {
        eprintln!("TALLY {t}");
    }
    eprintln!("{what}: {cases} cases");
    (failures, cases)
}

/// Odd-sided layers that nothing resizes (or that a blend refuses whatever the scaler draws), turned by every quarter, opaque and faded, plain
/// and blended: drawn exactly or refused. The turn and the blend are judged here; a plain layer the scaler resizes is
/// [`checker_e_r3_odd_sided_layers_the_scaler_resizes_plain`]'s.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_odd_sided_unscaled_layers_turned_plain_and_blended() {
    let (failures, cases) = r3_odd_sided("not resized", false);
    assert!(cases > 100, "{cases} cases");
    r3_finish("checker_e_r3_odd_sided_unscaled_layers_turned_plain_and_blended", failures);
}

/// The plain odd-sided layers the compositor's scaler resizes (a crop of a column or a row fitted back to the frame), turned by every quarter
/// (and not at all): drawn to the sample where the scaler is measured to be swscale's (the exact scaler of the filters lane, merged), refused
/// where it is not (`ScalerApproximate`) or where the picture's format is one the plan does not let the scaler resize (`FormatResize`). Held to
/// the export to the sample, a refusal counted as fine and nothing relaxed.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r3_odd_sided_layers_the_scaler_resizes_plain() {
    let (failures, cases) = r3_odd_sided("resized, plain", true);
    assert!(cases > 40, "{cases} cases");
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
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
    r3_finish("checker_e_r3_exotic_matrix_tags_under_a_blend", failures);
}

/// A rotation too large for its angle to mean anything (a radian count whose float has no fraction left, past what `quarter_turn` looks at): the layer must be
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

/// Many whole turns: `rotate` evaluates its angle in a precision that stops landing on whole samples a few turns out (FFmpeg's `rotate` of
/// 3690 degrees is not its rotate of 90), so `quarter_turn` accepts only the quarters its integer sine is exact for (-47, -46, -43 to 40, 43, 44).
/// A blended layer turned by one of those is drawn exactly; one turned by more is refused.
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
            );
        }
    }
    r3_finish("checker_e_r3_a_keyed_opacity_edge_on_each_quarter", failures);
}

// ---- checker/e/r4: the turns around the edges of the exact set, keyed turns and turns that stay put ---------------------

/// One case of this family: every frame the plan lets through is the export's to the sample, and `draws` says whether all of them must be drawn
/// (`Some(true)`), none (`Some(false)`) or either. A frame the plan refuses is drawn anyway when the compositor will (its own guards stand), and
/// one that comes out exact is a refusal wider than it has to be: `wider` collects them.
#[allow(clippy::too_many_arguments)]
fn r4_case(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    frames: Range<u64>,
    draws: Option<bool>,
    failures: &mut Vec<String>,
    wider: &mut Vec<String>,
    drawn_anyway: &mut Vec<String>,
) -> CeScan {
    let scan = ce_scan(case, tl, assets, &at_fps(30.0), frames.clone(), false);
    eprintln!("{}", scan.line(case));
    CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
    if let Some(why) = scan.verdict(draws) {
        failures.push(format!("{case}: {why}"));
    }
    if scan.drawn < scan.frames {
        let name = format!("{case} (drawn anyway)");
        let anyway = ce_scan(&name, tl, assets, &at_fps(30.0), frames, true);
        eprintln!("anyway: {}", anyway.line(&name));
        drawn_anyway.push(format!(
            "{case}: anyway drawn {} of {}, guards {}, Y {} UV {} differ",
            anyway.drawn,
            anyway.frames,
            anyway.guards.values().sum::<usize>(),
            anyway.y.differing,
            anyway.uv.differing
        ));
        if anyway.drawn > scan.drawn && anyway.exact() {
            wider.push(format!("{case}: refused and drawn exactly anyway ({} frames)", anyway.drawn));
        }
    }
    scan
}

fn r4_finish(what: &str, failures: Vec<String>, wider: &[String], drawn_anyway: &[String], need_drawn: bool) {
    for w in wider {
        eprintln!("WIDER {w}");
    }
    for d in drawn_anyway {
        eprintln!("ANYWAY {d}");
    }
    let mut failures = failures;
    if need_drawn {
        ce_expect_drawn(what, &mut failures);
    }
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// The quarters at either edge of the exact set (`-47, -46, -43..=40, 43, 44`) and far outside it, a picture of even sides, one with an odd
/// width (a column cropped off, unscaled: its chroma tail reaches past the turned box) and the same blended: the exact ones are drawn on every
/// frame to the sample and the others are not drawn at all.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r4_the_quarters_around_the_edges_of_the_exact_set_are_drawn_or_refused() {
    use kerf_core::BlendMode;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let exact = |q: i32| (-47..=-46).contains(&q) || (-43..=40).contains(&q) || (43..=44).contains(&q);
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let quarters = [
        -49, -48, -47, -46, -45, -44, -43, -42, -41, -40, -39, -5, -1, 1, 2, 3, 4, 5, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47,
        48, 52, 60, 100, 301, 636, 637, 640, 800,
    ];
    for q in quarters {
        for (what, odd, mode) in [
            ("even", false, None),
            ("odd", true, None),
            ("odd blended", true, Some(BlendMode::Multiply)),
        ] {
            let mut c = clip(&m.testsrc, 0.2, 1.0, 0.0);
            if odd {
                c.transform.crop_right = 1.0 / 640.0;
            }
            if let Some(mode) = mode {
                c.blend_mode = mode;
            }
            c.transform.rotation = f64::from(q) * 90.0;
            let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
            r4_case(
                &format!("checker/e/r4/quarter {q} {what}"),
                &tl,
                &assets,
                0..4,
                Some(exact(q)),
                &mut failures,
                &mut wider,
                &mut anyway,
            );
        }
    }
    r4_finish(
        "checker_e_r4_the_quarters_around_the_edges_of_the_exact_set",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// Angles a hair off a quarter (degrees, so that the plan's `to_radians` and the compositor's angle are the very numbers the export prints):
/// the layer is drawn on every frame exactly when `quarter_turn` of the same number says it is, and then to the sample.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r4_a_hair_off_a_quarter_is_drawn_exactly_or_refused() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let mut accepted = 0;
    for degrees in [
        90.0 + 1e-10,
        90.0 - 1e-10,
        90.0 + 1e-8,
        90.0 - 1e-8,
        90.000_000_1,
        90.000_01,
        89.999_99,
        180.0 + 5e-11,
        180.0 - 5e-11,
        270.0 - 1e-10,
        -90.0 + 1e-10,
        360.0 + 1e-10,
        1e-12,
        -1e-12,
        1e-9,
        1e-7,
        1e-5,
        1e-3,
        1e-300,
        5e-324,
        0.01,
        450.0 + 1e-10,
        3600.0 - 1e-10,
        3690.0,
        89.5,
        90.5,
    ] {
        let want = kerf_core::quarter_turn(f64::to_radians(degrees)).is_some();
        accepted += usize::from(want);
        let mut c = clip(&m.testsrc, 0.2, 1.0, 0.0);
        c.transform.rotation = degrees;
        let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
        r4_case(
            &format!("checker/e/r4/hair {degrees:e} degrees"),
            &tl,
            &assets,
            0..4,
            Some(want),
            &mut failures,
            &mut wider,
            &mut anyway,
        );
    }
    assert!(accepted >= 8, "{accepted} accepted: the hairs inside the window are drawn");
    r4_finish("checker_e_r4_a_hair_off_a_quarter", failures, &wider, &anyway, true);
}

/// A rotation that is keyed or driven. The graph of a clip whose rotation keys never leave 0 has no `rotate` in it at all (drawn, and what a
/// static rotation beside them says does not turn it); any key off 0 or a live behaviour puts a `rotate` in a box of its own (`hypot(iw,ih)`)
/// with a per-frame angle: refused, whether the keys pass through a quarter, sit on one, or turn the picture by a hair, plain and blended.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r4_keyed_rotations_that_never_turn_are_drawn_and_the_rest_are_refused() {
    use kerf_core::{Behavior, BlendMode, Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let keys = |a: f64, b: f64| vec![PropertyKey::new(0.0, a), PropertyKey::new(0.5, b)];
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    let cases: Vec<(&str, Edit<'_>, Option<bool>)> = vec![
        (
            "keys 0 to 0",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, keys(0.0, 0.0))),
            Some(true),
        ),
        (
            "keys 0 to 0 over a static 90",
            Box::new(|c: &mut Clip| {
                c.transform.rotation = 90.0;
                c.set_property_keys(Property::Rotation, keys(0.0, 0.0));
            }),
            None,
        ),
        (
            "keys 0 to 0 over a static 12",
            Box::new(|c: &mut Clip| {
                c.transform.rotation = 12.0;
                c.set_property_keys(Property::Rotation, keys(0.0, 0.0));
            }),
            None,
        ),
        (
            "a single key at 0",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, vec![PropertyKey::new(0.2, 0.0)])),
            Some(true),
        ),
        (
            "keys 90 to 90",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, keys(90.0, 90.0))),
            Some(false),
        ),
        (
            "keys 360 to 360",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, keys(360.0, 360.0))),
            Some(false),
        ),
        (
            "a single key at 90",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, vec![PropertyKey::new(0.2, 90.0)])),
            Some(false),
        ),
        (
            "keys 0 to 90",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, keys(0.0, 90.0))),
            Some(false),
        ),
        (
            "keys -90 to 90",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, keys(-90.0, 90.0))),
            Some(false),
        ),
        (
            "keys 0 to 180",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, keys(0.0, 180.0))),
            Some(false),
        ),
        (
            "keys 0 to 1e-9",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::Rotation, keys(0.0, 1e-9))),
            Some(false),
        ),
        (
            "keys 0 to 0 with an opacity key",
            Box::new(|c: &mut Clip| {
                c.set_property_keys(Property::Rotation, keys(0.0, 0.0));
                c.set_property_keys(Property::Opacity, keys(1.0, 0.4));
            }),
            None,
        ),
        (
            "a live spin",
            Box::new(|c: &mut Clip| c.behaviors = vec![Behavior::Spin { speed: 0.4, phase: 0.0 }]),
            Some(false),
        ),
        (
            "a spin of no speed over a static 90",
            Box::new(|c: &mut Clip| {
                c.transform.rotation = 90.0;
                c.behaviors = vec![Behavior::Spin { speed: 0.0, phase: 0.0 }];
            }),
            Some(true),
        ),
        (
            "a sway of no amount",
            Box::new(|c: &mut Clip| {
                c.behaviors = vec![Behavior::Sway {
                    amount: 0.0,
                    speed: 1.0,
                    phase: 0.0,
                }];
            }),
            Some(true),
        ),
        (
            "a live sway",
            Box::new(|c: &mut Clip| {
                c.behaviors = vec![Behavior::Sway {
                    amount: 5.0,
                    speed: 1.0,
                    phase: 0.0,
                }];
            }),
            Some(false),
        ),
    ];
    for (name, edit, draws) in &cases {
        for mode in [None, Some(BlendMode::Multiply)] {
            let mut c = clip(&m.testsrc, 0.2, 1.0, 0.0);
            edit(&mut c);
            if let Some(mode) = mode {
                c.blend_mode = mode;
            }
            let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
            r4_case(
                &format!("checker/e/r4/keyed {name} {mode:?}"),
                &tl,
                &assets,
                0..24,
                *draws,
                &mut failures,
                &mut wider,
                &mut anyway,
            );
        }
    }
    r4_finish(
        "checker_e_r4_keyed_rotations_that_never_turn_are_drawn_and_the_rest_are_refused",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// A still is judged elsewhere by a PNG's RGB (`tests/parity.rs`), a conversion of the planes. A cut whose pictures
/// hold still for the frames looked at is the same picture in a still and in an export, so the compositor's frame for the still's plan has to be
/// the export's frame to the sample: a quarter turn (even and odd sides, plain and blended) is drawn exactly, and any other turn is refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r4_a_still_of_a_turned_layer_is_the_exports_frame_to_the_sample() {
    use kerf_core::{BlendMode, RenderPlan};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let opts = at_fps(30.0);
    let mut failures: Vec<String> = Vec::new();
    let (mut drawn, mut refused) = (0, 0);
    for angle in [90.0, 180.0, 270.0, -90.0, 450.0, 12.0, 91.0] {
        for (what, odd, mode) in [
            ("even", false, None),
            ("odd", true, None),
            ("odd blended", true, Some(BlendMode::Multiply)),
            ("blended", false, Some(BlendMode::Screen)),
        ] {
            let mut c = clip(&m.testsrc, 0.2, 1.0, 0.0);
            if odd {
                c.transform.crop_right = 1.0 / 640.0;
            }
            if let Some(mode) = mode {
                c.blend_mode = mode;
            }
            c.transform.rotation = angle;
            let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
            let case = format!("checker/e/r4/still turned {angle} {what}");
            let slug: String = case
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let played = export(&slug, &tl, &assets, &opts);
            for k in [2u64, 9] {
                let t = k as f64 / 30.0 - 1e-4;
                let plan = RenderPlan::at(&tl, &assets, &opts, t, policy()).expect("still plan");
                let size = plan.size(u32::MAX);
                assert_eq!(size, (played.w as u32, played.h as u32), "{case}");
                let why = plan.reasons(&compositor().caps(), size);
                let quarter = kerf_core::quarter_turn(angle.to_radians()).is_some();
                if !why.is_empty() {
                    refused += 1;
                    eprintln!("{case} @ frame {k}: refused: {}", why[0]);
                    if quarter {
                        failures.push(format!("{case} @ frame {k}: a quarter turn is refused: {why:?}"));
                    }
                    continue;
                }
                if !quarter {
                    failures.push(format!("{case} @ frame {k}: a turn that is not a quarter is drawn"));
                }
                let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
                let got = compositor().composite_yuv(&plan, &frames, size).expect("composite");
                drawn += 1;
                let want = played.planes(k as usize);
                let got = planes_of(&got);
                let stats: Vec<PlaneStats> = (0..3).map(|p| PlaneStats::of(want[p], got[p])).collect();
                eprintln!(
                    "{case} @ frame {k}: drawn, Y {} U {} V {} differ",
                    stats[0].differing, stats[1].differing, stats[2].differing
                );
                if stats.iter().any(|s| s.differing > 0) {
                    failures.push(format!(
                        "{case} @ frame {k}: Y {} (max {}) U {} (max {}) V {} (max {}) differ",
                        stats[0].differing, stats[0].max, stats[1].differing, stats[1].max, stats[2].differing, stats[2].max
                    ));
                }
            }
        }
    }
    eprintln!("{drawn} still frames drawn and held to the export, {refused} refused");
    assert!(drawn >= 40, "{drawn} frames drawn");
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// An odd-sided box turned by a quarter reaches one chroma sample past itself like any odd layer: `overlay` blends whole chroma blocks, so the
/// last block of a 639-wide or 359-high layer carries its chroma onto the base's luma beyond the box (the layer is a picture like another once it
/// is turned by a quarter). A column or a row cropped off (the picture is then fitted back unscaled), turned by every quarter, moved by an odd
/// offset, plain and blended, 4:2:0 and gray, held to the export to the sample.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r4_an_odd_box_turned_by_a_quarter_carries_the_overlays_chroma_tail() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    for (name, asset) in [("4:2:0", &m.testsrc), ("noise", &m.noise), ("gray", &v.gray)] {
        for (crop, (r, b)) in [
            ("a column", (1.0 / 640.0, 0.0)),
            ("a row", (0.0, 1.0 / 360.0)),
            ("both", (1.0 / 640.0, 1.0 / 360.0)),
        ] {
            for angle in [90.0, 180.0, 270.0, -90.0] {
                for (mode, at) in [
                    (None, (0.0, 0.0)),
                    (Some(BlendMode::Multiply), (0.0123, -0.0171)),
                    (Some(BlendMode::Add), (0.0, 0.0)),
                ] {
                    let mut c = clip(asset, 0.2, 1.0, 0.0);
                    c.transform.crop_right = r;
                    c.transform.crop_bottom = b;
                    c.transform.rotation = angle;
                    c.transform.pos_x = at.0;
                    c.transform.pos_y = at.1;
                    if let Some(mode) = mode {
                        c.blend_mode = mode;
                    }
                    let assets = [m.bars.clone(), asset.clone()];
                    let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
                    // (Cropping both a column and a row fits the picture back with the scaler: not drawn exactly plain, and refused blended.)
                    let scaled = r3_resized(&tl, &assets, 30.0);
                    let draws = if scaled && mode.is_none() { None } else { Some(!scaled) };
                    let case = format!("checker/e/r4/odd tail {name} cropped {crop} turned {angle} {mode:?} at {at:?}");
                    if scaled && mode.is_none() {
                        continue;
                    }
                    r4_case(&case, &tl, &assets, 0..4, draws, &mut failures, &mut wider, &mut anyway);
                }
            }
        }
    }
    r4_finish(
        "checker_e_r4_an_odd_box_turned_by_a_quarter_carries_the_overlays_chroma_tail",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// Refusal necessity of the matrix allow-list: a blend over a clip or a bottom layer stating a tag no table is proven for (`bt2020c`, `ycgco`,
/// `ictcp`, `chroma-derived-*`, `smpte2085`, `gbr`) is refused. Where this FFmpeg can export the cut at all, its planes are compared with the same
/// cut untagged (the control, which the compositor does draw exactly): equal planes would make the refusal wider than it has to be on this build.
/// Printed (`WIDER` / `NECESSARY` / `NO EXPORT`), never judged: the allow-list is the contract, this is its price.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_r4_what_the_matrix_allow_list_costs() {
    use kerf_core::BlendMode;
    let opts = at_fps(30.0);
    let source = |name: &str| {
        if name == "bars" {
            "smptehdbars=size=640x360:rate=30:duration=4"
        } else {
            "testsrc2=size=640x360:rate=30:duration=4"
        }
    };
    let control = |name: &str| r3_try_tagged(name, ("unknown", "unknown"), source(name)).expect("an untagged ffv1 control");
    let (bars_u, top_u) = (control("bars"), control("testsrc"));
    let (mut necessary, mut wider, mut none) = (0, 0, 0);
    for tag in [
        "bt2020c",
        "ycgco",
        "ictcp",
        "chroma-derived-nc",
        "chroma-derived-c",
        "smpte2085",
        "gbr",
    ] {
        let enc = if tag == "gbr" { "rgb" } else { tag };
        let (Some(bars_t), Some(top_t)) = (
            r3_try_tagged("bars", (tag, enc), source("bars")),
            r3_try_tagged("testsrc", (tag, enc), source("testsrc")),
        ) else {
            eprintln!("NO WRITE {tag}: this FFmpeg will not write the tag");
            continue;
        };
        for mode in [BlendMode::Multiply, BlendMode::Add] {
            for (what, bottom, top, cb, ct) in [
                ("bottom", &bars_t, &top_u, &bars_u, &top_u),
                ("top", &bars_u, &top_t, &bars_u, &top_u),
                ("both", &bars_t, &top_t, &bars_u, &top_u),
            ] {
                let make = |b: &Asset, t: &Asset| {
                    let mut c = clip(t, 0.5, 1.5, 0.0);
                    c.blend_mode = mode;
                    (
                        timeline(vec![vec![clip(b, 0.5, 1.5, 0.0)], vec![c]], None),
                        [b.clone(), t.clone()],
                    )
                };
                let (tl_t, as_t) = make(bottom, top);
                let (tl_c, as_c) = make(cb, ct);
                let case = format!("{mode:?} {tag} {what}");
                let tagged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    export(&format!("r4_tag_t_{mode:?}_{tag}_{what}"), &tl_t, &as_t, &opts)
                }));
                let Ok(tagged) = tagged else {
                    none += 1;
                    eprintln!("NO EXPORT {case}: FFmpeg cannot export the tagged cut");
                    continue;
                };
                let plain = export(&format!("r4_tag_c_{mode:?}_{tag}_{what}"), &tl_c, &as_c, &opts);
                let frames = tagged.count().min(plain.count()).min(12);
                let equal = (0..frames).all(|k| tagged.planes(k) == plain.planes(k));
                if equal {
                    wider += 1;
                } else {
                    necessary += 1;
                }
                eprintln!(
                    "{} {case}: the tagged cut's export is {} the untagged control's over {frames} frames",
                    if equal { "WIDER" } else { "NECESSARY" },
                    if equal { "equal to" } else { "not equal to" },
                );
            }
        }
    }
    eprintln!("allow-list cost: {necessary} cut(s) where the refusal is necessary, {wider} where this FFmpeg draws it as the control, {none} FFmpeg cannot export");
}

// ---- checker/a: keyed placement, cases the implementer did not write ------------------------------------------
//
// The independent check of work package A. Every case compares the planes of every drawn frame with the export's, to the
// sample (no bound), and never edits a case above. `checker_motion` returns what it saw instead of asserting so that a case can
// say which frames the plan refused (and why), and so that the refusals can be drawn anyway when the compositor is patched
// to allow it (`KERF_CHECKER_ANYWAY`, see `checker_a_refusals_drawn_anyway`).

/// Pictures of 512x256 at 32 frames a second: a power-of-two canvas whose half-sample positions are dyadic rationals, and a
/// frame time (`k / 32`) that `f64` holds exactly, so a position can sit exactly on a rounding tie.
struct CheckerMedia {
    noise: Asset,
    testsrc: Asset,
    bars: Asset,
    /// The test pattern tagged BT.2020 (non-constant luminance) and BT.709.
    testsrc2020: Asset,
    testsrc709: Asset,
}

fn checker_media() -> &'static CheckerMedia {
    static M: OnceLock<CheckerMedia> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-media");
        std::fs::create_dir_all(&dir).unwrap();
        let video = |name: &str, lavfi: &str| -> Asset {
            let p = dir.join(name);
            ffmpeg(&[
                "-f", "lavfi", "-i", lavfi, "-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "8", "-pix_fmt", "yuv420p",
                p.to_str().unwrap(),
            ]);
            probed(&p)
        };
        let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        CheckerMedia {
            noise: video(
                "noise.mp4",
                "nullsrc=size=512x256:rate=32:duration=4,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
            ),
            testsrc: video("testsrc.mp4", &format!("testsrc2=size=512x256:rate=32:duration=4,{untagged}")),
            bars: video("bars.mp4", &format!("smptehdbars=size=512x256:rate=32:duration=4,{untagged}")),
            testsrc2020: video(
                "testsrc2020.mp4",
                "testsrc2=size=512x256:rate=32:duration=4,setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
            ),
            testsrc709: video(
                "testsrc709.mp4",
                "testsrc2=size=512x256:rate=32:duration=4,setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709",
            ),
        }
    })
}

/// What a run of `checker_motion` saw.
#[derive(Default)]
struct Seen {
    drawn: usize,
    /// Frames the plan (or, under `anyway`, the compositor) refused, with the first reason.
    refused: Vec<(u64, String)>,
    /// Frames whose planes differ from the export's: `(frame, plane, differing samples, max difference, first (x, y))`.
    off: Vec<Off>,
}

/// A plane that differs: `(frame, plane, differing samples, max difference, first (x, y))`.
type Off = (u64, &'static str, usize, i32, (usize, usize));

impl Seen {
    fn line(&self, case: &str) -> String {
        let why: std::collections::BTreeSet<&str> = self.refused.iter().map(|(_, w)| w.as_str()).collect();
        format!(
            "{case:<56} drawn {:>3} frames, {} differ | refused {:>3}{}",
            self.drawn,
            self.off.iter().map(|o| o.0).collect::<std::collections::BTreeSet<_>>().len(),
            self.refused.len(),
            if why.is_empty() {
                String::new()
            } else {
                format!(": {}", why.into_iter().collect::<Vec<_>>().join(" | "))
            }
        )
    }
}

/// Plan and draw frames `frames` of `tl`, compare each drawn one with the export's planes **exactly**. With `anyway` the
/// compositor is asked for a frame the plan refuses too (it answers only if the refusal has been patched out of the build).
fn checker_motion(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Range<u64>, anyway: bool) -> Seen {
    checker_motion_as(case, tl, assets, assets, fps, frames, anyway)
}

/// [`checker_motion`] where the plan is made from `plan_assets` (what the plan is told of the footage) and the export from
/// `assets` (the footage): a way to draw a frame the plan refuses for what it knows of a stream, without a patch.
fn checker_motion_as(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    plan_assets: &[Asset],
    fps: f64,
    frames: Range<u64>,
    anyway: bool,
) -> Seen {
    checker_motion_with(case, tl, assets, plan_assets, &at_fps(fps), frames, anyway)
}

/// [`checker_motion_as`] with the export's options (a range, a rate).
fn checker_motion_with(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    plan_assets: &[Asset],
    opts: &ExportOptions,
    frames: Range<u64>,
    anyway: bool,
) -> Seen {
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let opts = opts.clone();
    let played = export(&slug, tl, assets, &opts);
    assert!(
        played.count() as u64 >= frames.end,
        "{case}: the export has {} frames, the case looks at {}",
        played.count(),
        frames.end
    );
    let planner = Planner::new(tl, plan_assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let mut seen = Seen::default();
    for k in frames {
        let plan = planner.at_frame(k).expect("plan");
        let why = plan.reasons(&compositor().caps(), size);
        if !why.is_empty() && !anyway {
            seen.refused
                .push((k, why.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")));
            continue;
        }
        let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let drawn = match compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size) {
            Ok(d) => d,
            Err(kerf_gpu::GpuError::Unsupported(w)) if anyway => {
                seen.refused.push((k, w));
                continue;
            }
            Err(e) => panic!("{case}: frame {k}: {e}"),
        };
        seen.drawn += 1;
        let want = played.planes(k as usize);
        let got = planes_of(&drawn);
        let mut bad = false;
        for (p, name) in ["Y", "U", "V"].into_iter().enumerate() {
            let s = PlaneStats::of(want[p], got[p]);
            if s.differing > 0 {
                bad = true;
                let w = if p == 0 { played.w } else { played.w / 2 };
                let first = want[p].iter().zip(got[p]).position(|(a, b)| a != b).unwrap();
                seen.off.push((k, name, s.differing, s.max, (first % w, first / w)));
            }
        }
        if bad && !anyway {
            montage(case, k, &played, &plan, &picture, motion.counts());
        }
    }
    eprintln!("{}", seen.line(case));
    seen
}

/// A case whose every frame must be drawn and equal to the export's; the failure is returned, not raised.
fn checker_judge(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Range<u64>) -> Result<Seen, String> {
    let seen = checker_motion(case, tl, assets, fps, frames, false);
    if !seen.refused.is_empty() {
        return Err(format!("{case}: refused: {:?}", &seen.refused[..seen.refused.len().min(3)]));
    }
    if seen.drawn == 0 {
        return Err(format!("{case}: nothing was drawn"));
    }
    if let Some(o) = seen.off.first() {
        let frames: std::collections::BTreeSet<u64> = seen.off.iter().map(|o| o.0).collect();
        return Err(format!(
            "{case}: {} frame(s) differ ({:?}); first: frame {} plane {}: {} samples differ (max {}, first at {:?}); montages in target/parity/motion",
            frames.len(),
            frames.iter().take(12).collect::<Vec<_>>(),
            o.0,
            o.1,
            o.2,
            o.3,
            o.4
        ));
    }
    Ok(seen)
}

/// [`checker_judge`], failing at once.
fn checker_exact(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Range<u64>) -> Seen {
    checker_judge(case, tl, assets, fps, frames).unwrap_or_else(|e| panic!("{e}"))
}

/// Judge many cases, then fail with all of the failures.
fn checker_all(what: &str, fails: Vec<String>) {
    for f in &fails {
        eprintln!("FAIL {f}");
    }
    assert!(
        fails.is_empty(),
        "{what}: {} case(s) failed, first: {}",
        fails.len(),
        fails[0]
    );
}

fn pos_keys(asset: &Asset, from: f64, to: f64, start: f64, keys: &[(f64, f64, f64, f64)]) -> Clip {
    let mut c = clip(asset, from, to, start);
    c.keyframes = keys.iter().map(|&(t, s, x, y)| zoom_key(t, s, x, y)).collect();
    c
}

/// Positions that put 256 * (the sample position) exactly half way between two integers on every frame: at 32 frames a
/// second the frame time is dyadic, the canvas is 512x256, so a position of `k * 2^-18` of a width is `k / 2` of a sub-sample.
/// Whether `lrint` of a tie rounds to even is then the whole difference between a right frame and one level of noise.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_ties_in_export_frames() {
    let c = checker_media();
    let assets = [c.bars.clone(), c.noise.clone(), c.testsrc.clone()];
    let under = || clip(&c.bars, 0.5, 1.5, 0.0);
    // The position alone (no zoom: the placement's own pose), noise picture over bars and alone over black.
    let tie = (1.0 / 8192.0, 1.0 / 4096.0);
    let moving = pos_keys(&c.noise, 0.5, 1.5, 0.0, &[(0.0, 1.0, 0.0, 0.0), (1.0, 1.0, tie.0, tie.1)]);
    checker_exact(
        "checker/a/tie position over bars",
        &timeline(vec![vec![under()], vec![moving.clone()]], None),
        &assets,
        32.0,
        0..32,
    );
    checker_exact(
        "checker/a/tie position alone",
        &timeline(vec![vec![moving]], None),
        &assets[1..],
        32.0,
        0..32,
    );
    // The same, the other way and a ten times longer lap, so the sub-sample part runs through every fraction.
    let moving = pos_keys(
        &c.noise,
        0.5,
        1.5,
        0.0,
        &[(0.0, 1.0, 0.0, 0.0), (1.0, 1.0, -tie.0 * 10.0, tie.1 * 7.0)],
    );
    checker_exact(
        "checker/a/tie position, ten laps",
        &timeline(vec![vec![under()], vec![moving]], None),
        &assets,
        32.0,
        0..32,
    );
    // A zoom that moves by one part in 2^30 on top of the tie: the zoom is not a power of two, so the numbers are no longer
    // exact, and the frame is a coin toss for any arithmetic that is not the filter's own, operation for operation.
    let moving = pos_keys(
        &c.noise,
        0.5,
        1.5,
        0.0,
        &[(0.0, 1.0, 0.0, 0.0), (1.0, 1.0 + 1.0 / 1_073_741_824.0, tie.0, tie.1)],
    );
    checker_exact(
        "checker/a/tie position and a hair of zoom",
        &timeline(vec![vec![under()], vec![moving]], None),
        &assets,
        32.0,
        0..32,
    );
    let moving = pos_keys(
        &c.noise,
        0.5,
        1.5,
        0.0,
        &[(0.0, 2.0, 0.0, 0.0), (1.0, 2.0 + 1.0 / 1_048_576.0, tie.0 * 3.0, tie.1)],
    );
    checker_exact(
        "checker/a/tie position at a zoom of 2",
        &timeline(vec![vec![under()], vec![moving]], None),
        &assets,
        32.0,
        0..32,
    );
    // Whole-sample positions (k / 512 of a width): the fraction is 0 and the rounding never matters; the clamp does.
    let moving = pos_keys(
        &c.testsrc,
        0.5,
        1.5,
        0.0,
        &[(0.0, 1.0, 0.0, 0.0), (1.0, 1.0, 64.0 / 512.0, -32.0 / 256.0)],
    );
    checker_exact(
        "checker/a/whole-sample position",
        &timeline(vec![vec![under()], vec![moving]], None),
        &assets,
        32.0,
        0..32,
    );
}

/// Which frame's pose is drawn: a fast zoom and pan on a counter picture, at every common frame rate, with the clip starting
/// on the output frame grid (at 0, and at a whole number of frames), after another clip, at speed 2, 0.5 and reversed. One
/// frame of difference in `eval=frame`'s count is several samples of motion here.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_the_pose_is_that_of_the_frame_at_every_rate() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut fails = Vec::new();
    for (name, fps) in [
        ("23.976", 24000.0f64 / 1001.0),
        ("24", 24.0),
        ("25", 25.0),
        ("29.97", 30000.0 / 1001.0),
        ("30", 30.0),
        ("48", 48.0),
        ("50", 50.0),
        ("59.94", 60000.0 / 1001.0),
        ("60", 60.0),
        ("12", 12.0),
        ("120", 120.0),
    ] {
        // (Whole frames: 9 or 13 of them in, so the start is on the output grid at this rate.)
        for frames_in in [0u64, 9, 13] {
            let start = frames_in as f64 / fps;
            let mut c = clip(&m.testsrc, 0.5, 1.5, start);
            c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 2.2, 0.45, -0.3)];
            let tl = timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![c]], None);
            let to = frames_in + fps.ceil() as u64;
            fails.extend(
                checker_judge(
                    &format!("checker/a/pose timing @{name} starting at frame {frames_in}"),
                    &tl,
                    &assets,
                    fps,
                    frames_in..to,
                )
                .err(),
            );
        }
    }
    // After another clip on its own track (the `fps` filter's first slot is the clip's start), at speeds that retime the frames.
    for (name, speed, secs) in [
        ("speed 2", 2.0, 2.0),
        ("speed 0.5", 0.5, 1.0),
        ("reversed", -1.0, 1.0),
        ("speed 1.5 reversed", -1.5, 1.5),
    ] {
        for fps in [25.0f64, 30000.0 / 1001.0, 60.0] {
            let mut c = clip(&m.testsrc, 0.5, 0.5 + secs, 1.0);
            c.speed = speed;
            c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.9, 0.3, 0.2)];
            let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0), c]], None);
            let from = (1.0 * fps).floor() as u64;
            fails.extend(
                checker_judge(
                    &format!("checker/a/pose timing after a clip, {name} @{fps:.3}"),
                    &tl,
                    &assets,
                    fps,
                    from..from + (fps * 0.6) as u64,
                )
                .err(),
            );
        }
    }
    checker_all("pose timing", fails);
}

/// A moving zoom or position on a clip that starts **between** two output frames. The placed clip's graph pins its `fps`
/// to the clip's start (`fps=fps=24:start_time=0.37`, `keyed_crop_fps`), and with `start_time` that filter rounds the
/// input times relative to the start (`round((pts - start) * rate) + round(start * rate)`), which is not the plain
/// `round(pts * rate)` the plan's frame pick follows (`frame_pick::fps_pick`): at some rates the export draws another source
/// frame than the compositor does. The control is the same clip with a pose that does not move (a plain `fps`), exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_a_placed_clip_starting_between_frames_is_drawn_from_the_frame_the_export_picks() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut fails = Vec::new();
    for (name, fps) in [
        ("23.976", 24000.0f64 / 1001.0),
        ("24", 24.0),
        ("25", 25.0),
        ("29.97", 30000.0 / 1001.0),
        ("30", 30.0),
        ("48", 48.0),
        ("50", 50.0),
        ("59.94", 60000.0 / 1001.0),
        ("60", 60.0),
        ("12", 12.0),
        ("120", 120.0),
    ] {
        for start in [0.37f64, 1.013] {
            let from = (start * fps).floor() as u64;
            let to = ((start + 1.0) * fps).ceil() as u64;
            let moving = |moves: bool| {
                let mut c = clip(&m.testsrc, 0.5, 1.5, start);
                if moves {
                    c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 2.2, 0.45, -0.3)];
                }
                timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![c]], None)
            };
            let control = checker_judge(
                &format!("checker/a/CONTROL plain clip @{name} starting {start}"),
                &moving(false),
                &assets,
                fps,
                from..to,
            );
            let placed = checker_judge(
                &format!("checker/a/placed clip @{name} starting {start}"),
                &moving(true),
                &assets,
                fps,
                from..to,
            );
            eprintln!(
                "  -> control {} / placed {}",
                if control.is_ok() { "exact" } else { "DIFFERS" },
                if placed.is_ok() { "exact" } else { "DIFFERS" }
            );
            fails.extend(placed.err());
            fails.extend(control.err().map(|e| format!("(control, not a placement) {e}")));
        }
    }
    checker_all("placed clips starting between frames", fails);
}

/// Live behaviours with a spread of seeds, amounts, speeds, phases and angles, alone and stacked, on a clip that starts late.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_behaviours_with_seeds_are_placed_as_the_export_places_them() {
    use kerf_core::Behavior as B;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut fails = Vec::new();
    let mut go = |case: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Range<u64>| {
        fails.extend(checker_judge(case, tl, assets, fps, frames).err());
    };
    let with = |start: f64, behaviors: Vec<B>| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, start);
        c.behaviors = behaviors;
        timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![c]], None)
    };
    for seed in [0u32, 1, 2, 7, 255, 4096, 123_456_789, u32::MAX] {
        for (amount, speed) in [(0.001, 0.1), (0.03, 2.5), (0.25, 30.0), (0.12, 7.77)] {
            let shake = B::Shake {
                amount,
                speed,
                seed,
                phase: 0.0,
            };
            go(
                &format!("checker/a/shake seed {seed} {amount}@{speed}Hz"),
                &with(0.0, vec![shake]),
                &assets,
                30.0,
                0..30,
            );
        }
    }
    for phase in [-3.3, 0.0, 0.123, 99.5] {
        let shake = B::Shake {
            amount: 0.1,
            speed: 3.3,
            seed: 31,
            phase,
        };
        go(
            &format!("checker/a/shake phase {phase}, late start"),
            &with(0.4, vec![shake]),
            &assets,
            30.0,
            12..42,
        );
    }
    for (amount, speed, phase) in [(0.9, 0.5, 0.0), (0.05, 11.0, 0.3), (0.5, 1.0, -0.25), (0.0001, 5.0, 0.0)] {
        let b = B::Breathe { amount, speed, phase };
        go(
            &format!("checker/a/breathe {amount}@{speed}Hz phase {phase}"),
            &with(0.0, vec![b]),
            &assets,
            30.0,
            0..30,
        );
    }
    for (speed, angle) in [
        (0.2, 0.0),
        (0.2, 90.0),
        (-0.3, 180.0),
        (1.0, 270.0),
        (0.5, 359.9),
        (4.9, 45.0),
        (0.7, -30.0),
        (0.1, 36_000.0),
    ] {
        let b = B::Drift {
            speed,
            angle,
            phase: 0.05,
        };
        go(
            &format!("checker/a/drift {speed} at {angle} degrees"),
            &with(0.0, vec![b]),
            &assets,
            30.0,
            0..30,
        );
    }
    let all = vec![
        B::Breathe {
            amount: 0.2,
            speed: 0.9,
            phase: 0.0,
        },
        B::Drift {
            speed: 0.3,
            angle: 200.0,
            phase: 0.0,
        },
        B::Shake {
            amount: 0.04,
            speed: 4.0,
            seed: 99,
            phase: 0.0,
        },
    ];
    go(
        "checker/a/all three behaviours",
        &with(0.0, all.clone()),
        &assets,
        30.0,
        0..30,
    );
    // At other rates and speeds: the behaviour's clock is the clip's.
    for fps in [24.0f64, 25.0, 30000.0 / 1001.0, 60.0] {
        // (Starting on the output grid: 9 frames in. Between two frames is `checker_a_a_placed_clip_starting_between_frames_*`.)
        let start = 9.0 / fps;
        go(
            &format!("checker/a/all three behaviours @{fps:.3}"),
            &with(start, all.clone()),
            &assets,
            fps,
            9..(fps.ceil() as u64 + 9),
        );
    }
    let mut fast = clip(&m.testsrc, 0.5, 2.5, 0.0);
    fast.speed = 2.0;
    fast.behaviors = all.clone();
    go(
        "checker/a/behaviours at speed 2",
        &timeline(vec![vec![fast]], None),
        &assets[1..],
        30.0,
        0..30,
    );
    let mut backwards = clip(&m.testsrc, 0.5, 1.5, 0.0);
    backwards.speed = -1.0;
    backwards.behaviors = all.clone();
    go(
        "checker/a/behaviours reversed",
        &timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![backwards]], None),
        &assets,
        30.0,
        0..30,
    );
    // On top of keys that move the zoom and the position.
    let mut keyed = clip(&m.testsrc, 0.5, 1.5, 0.0);
    keyed.keyframes = vec![
        zoom_key(0.0, 1.0, -0.2, 0.0),
        zoom_key(0.5, 1.6, 0.0, 0.2),
        zoom_key(1.0, 0.8, 0.2, -0.2),
    ];
    keyed.behaviors = all;
    go(
        "checker/a/behaviours over keys",
        &timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], vec![keyed]], None),
        &assets,
        30.0,
        0..30,
    );
    checker_all("behaviours", fails);
}

/// Zooms out to a speck and in to a pixel, positions several canvases away: every frame the plan draws is the export's, to the
/// sample; the frames it refuses (`PlacementFrame::reachable`) are FFmpeg's and are counted.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_extreme_keyed_poses_are_placed_as_the_export_places_them() {
    let c = checker_media();
    let assets = [c.bars.clone(), c.noise.clone()];
    let mut fails = Vec::new();
    let mut drawn_total = 0;
    for (name, keys) in [
        // (The move takes a tenth of the clip and the pose then holds, so most of the frames are drawn at the extreme.)
        ("zoom out to 0.0004", vec![(0.0, 1.0, 0.0, 0.0), (0.1, 0.0004, 0.0, 0.0)]),
        ("zoom out to 0.0001", vec![(0.0, 1.0, 0.0, 0.0), (0.1, 0.0001, 0.0, 0.0)]),
        (
            "zoom out to 0.0005 (just inside the reach)",
            vec![(0.0, 1.0, 0.0, 0.0), (0.1, 0.0005, 0.0, 0.0)],
        ),
        (
            "zoom out to 0.02 off to a corner",
            vec![(0.0, 1.0, 0.0, 0.0), (0.1, 0.02, 0.45, 0.45)],
        ),
        ("zoom in to 60", vec![(0.0, 1.0, 0.0, 0.0), (0.1, 60.0, 0.0, 0.0)]),
        (
            "zoom in to 400 off centre",
            vec![(0.0, 1.0, 0.0, 0.0), (0.1, 400.0, 0.3, -0.2)],
        ),
        ("zoom in to 5000", vec![(0.0, 2.0, 0.0, 0.0), (0.1, 5000.0, 0.0, 0.0)]),
        ("pan four canvases right", vec![(0.0, 1.0, 0.0, 0.0), (0.1, 1.0, 4.0, 0.0)]),
        (
            "pan twenty canvases up-left",
            vec![(0.0, 1.0, 0.0, 0.0), (0.1, 1.0, -20.0, -20.0)],
        ),
        (
            "zoom 3 and pan 1.5 canvases",
            vec![(0.0, 3.0, -1.5, 1.5), (1.0, 3.2, 1.5, -1.5)],
        ),
        (
            "zoom out and a huge pan",
            vec![(0.0, 1.0, 0.0, 0.0), (0.1, 0.05, 1500.0, 0.0)],
        ),
        (
            "zoom out and a huge pan, 800 canvases",
            vec![(0.0, 1.0, 0.0, 0.0), (0.1, 0.05, 800.0, 0.0)],
        ),
    ] {
        let tl = timeline(
            vec![
                vec![clip(&c.bars, 0.5, 1.5, 0.0)],
                vec![pos_keys(&c.noise, 0.5, 1.5, 0.0, &keys)],
            ],
            None,
        );
        let seen = checker_motion(&format!("checker/a/extreme {name}"), &tl, &assets, 32.0, 0..32, false);
        drawn_total += seen.drawn;
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
        for (k, why) in &seen.refused {
            if !why.contains("fixed-point range") {
                fails.push(format!("{name}: frame {k} refused for something else: {why}"));
            }
        }
    }
    assert!(drawn_total > 100, "almost nothing was drawn ({drawn_total} frames)");
    checker_all("extreme keyed poses", fails);
}

/// Pictures the fit and a static crop leave at other shapes than the frame: odd widths, a crop that starts between two chroma
/// samples, a picture much smaller than the canvas; each moving zoom or pan. An odd height is the plan's refusal.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_cropped_and_fitted_pictures_are_placed_as_the_export_places_them() {
    let m = media();
    let sh = shapes();
    let assets = [
        m.bars.clone(),
        m.testsrc.clone(),
        m.portrait.clone(),
        sh.odd_height.clone(),
        sh.odd_width.clone(),
        sh.pillar.clone(),
    ];
    let mut fails = Vec::new();
    // Every frame: if the picture the plan says the chain makes is odd in height the frame must be refused for it, else it is drawn
    // and equal to the export's.
    let mut expect = |name: &str, tl: Timeline, assets: &[Asset]| {
        let opts = at_fps(30.0);
        let planner = Planner::new(&tl, assets, &opts, PlanRequest::motion(policy())).expect("planner");
        let mut sizes = std::collections::BTreeSet::new();
        let mut odd = Vec::new();
        for k in 0..30 {
            let plan = planner.at_frame(k).expect("plan");
            let size = plan.size(u32::MAX);
            let layer = plan.layers.last().expect("a layer");
            if let Ok(g) = plan.layer_geometry(layer, size) {
                sizes.insert(g.picture);
                if g.picture.1 % 2 == 1 {
                    odd.push(k);
                }
            }
        }
        let seen = checker_motion(&format!("checker/a/{name}"), &tl, assets, 30.0, 0..30, false);
        eprintln!("   picture(s) {sizes:?}");
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
        let refused: Vec<u64> = seen.refused.iter().map(|r| r.0).collect();
        for (k, why) in &seen.refused {
            if !(odd.contains(k) && why.contains("odd-height")) {
                fails.push(format!("{name}: frame {k} refused for something it should not: {why}"));
            }
        }
        for k in &odd {
            if !refused.contains(k) {
                fails.push(format!(
                    "{name}: frame {k}: an odd-height picture {sizes:?} was drawn, not refused"
                ));
            }
        }
        if odd.is_empty() && seen.drawn != 30 {
            fails.push(format!("{name}: only {} of 30 frames were drawn", seen.drawn));
        }
    };
    // (Crops that leave one side of the box full: the fit does not enlarge them, so the compositor's scaler is not in play.)
    for (name, crop) in [
        ("crop right 0.0013 (639 wide)", (0.0, 0.0013, 0.0, 0.0)),
        ("crop left 0.003 (an odd x offset)", (0.003, 0.0, 0.0, 0.0)),
        ("crop left and right 0.0058 (631 wide, x 3)", (0.0058, 0.0058, 0.0, 0.0)),
        ("crop top and bottom 0.1 (288 high)", (0.0, 0.0, 0.1, 0.1)),
        ("crop top 0.0083 (357 high)", (0.0, 0.0, 0.0083, 0.0)),
        ("crop bottom 0.0042 and right 0.0021", (0.0, 0.0021, 0.0, 0.0042)),
        ("crop top 0.2 (288 high), left 0.01", (0.01, 0.0, 0.2, 0.0)),
    ] {
        let mut z = zooming(&m.testsrc, 1.0, 1.5, (0.1, -0.05));
        z.transform.crop_left = crop.0;
        z.transform.crop_right = crop.1;
        z.transform.crop_top = crop.2;
        z.transform.crop_bottom = crop.3;
        expect(
            name,
            timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![z.clone()]], None),
            &assets[..2],
        );
        // The position alone moves (the zoom holds): the same picture, a different pose.
        let mut p = clip(&m.testsrc, 0.5, 1.5, 0.0);
        p.transform = z.transform;
        p.keyframes = vec![zoom_key(0.0, 1.0, -0.2, 0.1), zoom_key(1.0, 1.0, 0.2, -0.1)];
        expect(
            &format!("{name}, a pan"),
            timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![p]], None),
            &assets[..2],
        );
    }
    // The fit leaves a picture of its own shape in a frame of another: pillarboxed, letterboxed, a thin strip, odd in a side.
    for (name, asset, frame) in [
        ("portrait in 640x640", &m.portrait, (640, 640)),
        ("landscape in 640x480", &m.testsrc, (640, 480)),
        ("landscape in 640x400", &m.testsrc, (640, 400)),
        ("landscape in 800x360", &m.testsrc, (800, 360)),
        ("portrait in 360x720", &m.portrait, (360, 720)),
        ("640x203 in 640x360", &sh.odd_height, (640, 360)),
        ("640x203 in 640x400", &sh.odd_height, (640, 400)),
        ("203x360 in 640x360", &sh.odd_width, (640, 360)),
        ("203x360 in 400x360", &sh.odd_width, (400, 360)),
        ("480x360 in 640x360", &sh.pillar, (640, 360)),
    ] {
        let z = zooming(asset, 1.0, 1.7, (0.05, 0.05));
        let tl = timeline(vec![vec![z]], Some(Delivery::new(frame.0, frame.1, Fit::Contain)));
        expect(&format!("{name} zoom"), tl, std::slice::from_ref(asset));
        let mut p = clip(asset, 0.5, 1.5, 0.0);
        p.keyframes = vec![zoom_key(0.0, 1.0, -0.3, 0.0), zoom_key(1.0, 1.0, 0.3, 0.1)];
        let tl = timeline(vec![vec![p]], Some(Delivery::new(frame.0, frame.1, Fit::Contain)));
        expect(&format!("{name} pan"), tl, std::slice::from_ref(asset));
    }
    checker_all("cropped and fitted pictures", fails);
}

/// Both clips of a transition move (the outgoing clip's tail and the incoming clip's head each end in a placement), and a
/// placed clip fading in and out on its own, over another placed layer.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_two_moving_clips_through_every_transition_are_placed_as_the_export_places_them() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut fails = Vec::new();
    for kind in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::SlideUp,
        TransitionKind::PushRight,
        TransitionKind::PushDown,
    ] {
        for (label, out_moves, in_moves) in [("both", true, true), ("outgoing", true, false), ("incoming", false, true)] {
            for fps in [30.0, 24.0] {
                let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
                let mut inc = clip(&m.testsrc, 0.5, 1.5, 1.0);
                if out_moves {
                    out.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.5, -0.1, 0.1)];
                }
                if in_moves {
                    inc.keyframes = vec![zoom_key(0.0, 1.0, 0.1, -0.1), zoom_key(1.0, 1.4, 0.0, 0.0)];
                }
                inc.transition_in = Some(Transition { kind, duration: 0.6 });
                let tl = timeline(vec![vec![out, inc]], None);
                let secs = 2.0;
                fails.extend(
                    checker_judge(
                        &format!("checker/a/{kind:?} {label} moving @{fps}"),
                        &tl,
                        &assets,
                        fps,
                        0..(secs * fps) as u64,
                    )
                    .err(),
                );
            }
        }
    }
    // A placed clip fading in and out, a dissolve in, over a second placed layer.
    let under = {
        let mut u = clip(&m.bars, 0.5, 1.5, 0.0);
        u.keyframes = vec![zoom_key(0.0, 1.3, 0.05, 0.0), zoom_key(1.0, 1.0, -0.05, 0.0)];
        u
    };
    let mut top = zooming(&m.testsrc, 0.6, 1.2, (0.2, 0.1));
    top.fade_in = 0.3;
    top.fade_out = 0.3;
    top.set_property_keys(
        kerf_core::Property::PosY,
        vec![kerf_core::PropertyKey::new(0.0, -0.2), kerf_core::PropertyKey::new(1.0, 0.2)],
    );
    let tl = timeline(vec![vec![under], vec![top]], None);
    fails.extend(checker_judge("checker/a/a placed clip fading over a placed clip", &tl, &assets, 30.0, 0..30).err());
    checker_all("transitions between moving clips", fails);
}

/// The refusals of the placement that are left, drawn anyway: the frame the plan refuses is planned and drawn all the same and compared
/// with the export. A refusal is *necessary* when the drawn frame is not the export's; one that would have been exact is a
/// refusal wider than it has to be. The refusals that live in the plan's judgement of the cut need the compositor to be
/// built without them (a scratch patch, `KERF_CHECKER_ANYWAY`: the plan's `reasons` of those kinds are dropped and the
/// compositor's own checks in `placement_of` skipped); the `PlacedFormat` ones are drawn by telling the plan the footage is
/// 4:2:0 (what the decode makes of it either way). Without the patch the case says what is still refused and judges nothing.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); the plan-level refusals need KERF_CHECKER_ANYWAY and the scratch patch"]
fn checker_a_refusals_drawn_anyway() {
    let m = media();
    let sh = shapes();
    let v = variants();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let patched = std::env::var_os("KERF_CHECKER_ANYWAY").is_some();
    let mut report = Vec::new();
    let mut judge = |name: &str, seen: Seen| {
        let verdict = if seen.drawn == 0 {
            format!(
                "still refused (nothing drawn): {}",
                seen.refused.first().map_or("", |r| r.1.as_str())
            )
        } else if seen.off.is_empty() {
            format!("EXACT on {} frames: the refusal is wider than it has to be", seen.drawn)
        } else {
            let frames: std::collections::BTreeSet<u64> = seen.off.iter().map(|o| o.0).collect();
            let worst = seen.off.iter().max_by_key(|o| o.3).unwrap();
            format!(
                "not exact: {} of {} frames differ, worst plane {} max {} ({} samples, first at {:?})",
                frames.len(),
                seen.drawn,
                worst.1,
                worst.3,
                worst.2,
                worst.4
            )
        };
        eprintln!("ANYWAY {name:<58} {verdict}");
        report.push((name.to_string(), seen.drawn, seen.off.is_empty()));
    };
    let over = |c: Clip| timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
    let moving = |edit: &dyn Fn(&mut Clip)| {
        let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.1, -0.05));
        edit(&mut c);
        over(c)
    };
    let panning = |edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.keyframes = vec![zoom_key(0.0, 1.0, -0.2, 0.1), zoom_key(1.0, 1.0, 0.2, -0.1)];
        edit(&mut c);
        over(c)
    };
    let mut run = |name: &str, tl: Timeline, assets: &[Asset]| {
        judge(
            name,
            checker_motion(&format!("checker/a/anyway {name}"), &tl, assets, 30.0, 0..30, true),
        );
    };
    // ZoomBehind: a rotation, an opacity below 1, a dip to white ahead of the placement. (A grade is not one any more:
    // `checker_a_a_grade_ahead_of_a_placement_is_the_exports`.)
    // (The keys carry the rotation and the opacity: a keyed clip's transform is its keys'.)
    run(
        "rotation 8 + zoom",
        moving(&|c| c.keyframes.iter_mut().for_each(|k| k.rotation = 8.0)),
        &assets,
    );
    run(
        "rotation 8 + pan",
        panning(&|c| c.keyframes.iter_mut().for_each(|k| k.rotation = 8.0)),
        &assets,
    );
    run(
        "opacity 0.7 + zoom",
        moving(&|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.7)),
        &assets,
    );
    run(
        "opacity 0.7 + pan",
        panning(&|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.7)),
        &assets,
    );
    run(
        "opacity 0.97 + zoom",
        moving(&|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.97)),
        &assets,
    );
    {
        let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
        let mut inc = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.0));
        inc.timeline_start = 1.0;
        inc.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 0.6,
        });
        out.keyframes = Vec::new();
        run("dip to white into a zoom", timeline(vec![vec![out, inc]], None), &assets);
    }
    // Geometry: an odd-height picture (on a canvas the fit does not resize it for), by a zoom and by a pan.
    {
        let a = [m.bars.clone(), sh.odd_height.clone()];
        let tl = timeline(
            vec![
                vec![clip(&m.bars, 0.5, 1.5, 0.0)],
                vec![zooming(&sh.odd_height, 1.0, 1.4, (0.0, 0.0))],
            ],
            None,
        );
        run("odd height 640x203 + zoom", tl, &a);
        let mut p = clip(&sh.odd_height, 0.5, 1.5, 0.0);
        p.keyframes = vec![zoom_key(0.0, 1.0, -0.2, 0.1), zoom_key(1.0, 1.0, 0.2, -0.1)];
        run(
            "odd height 640x203 + pan",
            timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![p]], None),
            &a,
        );
    }
    // PlacedFormat: told that the footage is 4:2:0 (the decode converts it ahead of the graph either way).
    let as_420 = |a: &Asset| {
        let mut a = a.clone();
        for s in &mut a.streams {
            if s.kind == StreamKind::Video {
                s.pix_fmt = Some("yuv420p".into());
            }
        }
        a
    };
    // (4:4:4 and 4:2:2 are not refusals any more, bar the full-range ones: `checker_a_4_4_4_and_4_2_2_under_a_placement_*`.)
    {
        let (name, asset) = ("full-range yuvj420p", &v.mjpeg);
        let tl = timeline(
            vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![zooming(asset, 1.0, 1.6, (0.0, 0.0))]],
            None,
        );
        let truth = [m.bars.clone(), asset.clone()];
        let told = [m.bars.clone(), as_420(asset)];
        judge(
            &format!("format {name} + zoom"),
            checker_motion_as(
                &format!("checker/a/anyway format {name}"),
                &tl,
                &truth,
                &told,
                30.0,
                0..30,
                true,
            ),
        );
    }
    if patched {
        let wide: Vec<_> = report.iter().filter(|(_, drawn, exact)| *drawn > 0 && *exact).collect();
        eprintln!(
            "refusals wider than they have to be: {:?}",
            wide.iter().map(|w| &w.0).collect::<Vec<_>>()
        );
    } else {
        eprintln!("(not patched: only what the plan lets through the compositor was drawn)");
    }
}

/// The matrix tags and the BT.709 label, in a placed frame: BT.2020 and BT.709 tagged pictures (alone, over each other, over
/// untagged), untagged high definition (the graph labels it BT.709) over tagged high definition. The placement moves planes and
/// converts nothing, so every drawn frame is the export's; the frames the composite colour policy refuses (a mix of matrices, a
/// range it negotiates) are FFmpeg's and are listed.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_matrices_and_the_709_label_are_placed_as_the_export_places_them() {
    let c = checker_media();
    let h = hd_pair();
    let mut fails = Vec::new();
    let moving = |a: &Asset, secs: (f64, f64), pos: (f64, f64)| {
        let mut z = clip(a, secs.0, secs.1, 0.0);
        z.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.6, pos.0, pos.1)];
        z
    };
    let mut run = |name: &str, tl: Timeline, assets: Vec<Asset>, fps: f64, frames: Range<u64>| {
        let seen = checker_motion(&format!("checker/a/{name}"), &tl, &assets, fps, frames, false);
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
        for (k, why) in &seen.refused {
            let policy_says = ["matrices", "full-range", "BT.2020", "colour"];
            if !policy_says.iter().any(|p| why.contains(p)) {
                fails.push(format!("{name}: frame {k} refused for something else: {why}"));
            }
        }
    };
    let (u, b) = (&c.testsrc, &c.bars);
    for (name, top, under) in [
        ("bt2020 over untagged", &c.testsrc2020, b),
        ("bt709 over untagged", &c.testsrc709, b),
        ("untagged over bt2020", u, &c.testsrc2020),
        ("bt2020 over bt709", &c.testsrc2020, &c.testsrc709),
        ("bt709 over bt709", &c.testsrc709, &c.testsrc709),
    ] {
        let tl = timeline(
            vec![vec![clip(under, 0.5, 1.5, 0.0)], vec![moving(top, (0.5, 1.5), (0.2, -0.1))]],
            None,
        );
        run(name, tl, vec![under.clone(), top.clone()], 32.0, 0..32);
    }
    for (name, a) in [("bt2020 alone", &c.testsrc2020), ("bt709 alone", &c.testsrc709)] {
        run(
            name,
            timeline(vec![vec![moving(a, (0.5, 1.5), (0.0, 0.2))]], None),
            vec![a.clone()],
            32.0,
            0..32,
        );
    }
    // High definition: untagged is labelled BT.709 for the whole graph; the placement of the 1280x720 canvas.
    for (name, top, under) in [
        ("hd untagged over hd 709", &h.untagged, &h.phone),
        ("hd 709 over hd untagged", &h.phone, &h.untagged),
    ] {
        let tl = timeline(
            vec![vec![clip(under, 0.0, 1.5, 0.0)], vec![moving(top, (0.0, 1.0), (0.1, 0.1))]],
            None,
        );
        run(name, tl, vec![under.clone(), top.clone()], 30.0, 0..30);
    }
    checker_all("matrices and the 709 label", fails);
}

/// What else keys on a clip whose zoom moves: a colour (the `eq` of each frame, at the fit size ahead of the placement), a mask
/// (cut at the fit size, moved with the picture), both with a position; and a keyed crop or keyed opacity, refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_keyed_colour_and_mask_ahead_of_a_moving_zoom() {
    use kerf_core::{Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut fails = Vec::new();
    // `refused`: the reason every refused frame must give (some frames of a key that starts neutral are drawn); None: all drawn.
    let mut run = |name: &str, c: Clip, refused: Option<&str>| {
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
        let seen = checker_motion(&format!("checker/a/{name}"), &tl, &assets, 30.0, 0..30, false);
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
        match refused {
            None if seen.drawn != 30 => fails.push(format!(
                "{name}: {} of 30 frames drawn: {:?}",
                seen.drawn,
                seen.refused.first()
            )),
            Some(why) => {
                if seen.refused.is_empty() {
                    fails.push(format!("{name}: nothing was refused"));
                }
                for (k, w) in &seen.refused {
                    if !w.contains(why) {
                        fails.push(format!("{name}: frame {k} refused for {w}, not {why}"));
                    }
                }
            }
            None => {}
        }
    };
    for prop in [
        Property::Brightness,
        Property::Contrast,
        Property::Saturation,
        Property::Gamma,
        Property::Temperature,
    ] {
        let (lo, hi) = colour_span(prop);
        let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.1, 0.05));
        c.set_property_keys(prop, linear_keys(lo, hi));
        run(&format!("keyed {prop:?} + zoom"), c, None);
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.keyframes = vec![zoom_key(0.0, 1.0, -0.2, 0.1), zoom_key(1.0, 1.0, 0.2, -0.1)];
        c.set_property_keys(prop, linear_keys(lo, hi));
        run(&format!("keyed {prop:?} + pan"), c, None);
    }
    let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.1, 0.05));
    c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false));
    c.set_property_keys(Property::MaskX, vec![PropertyKey::new(0.0, 0.3), PropertyKey::new(1.0, 0.7)]);
    c.set_property_keys(
        Property::MaskWidth,
        vec![PropertyKey::new(0.0, 0.3), PropertyKey::new(1.0, 0.9)],
    );
    run("keyed mask + zoom", c, Some("keyframed mask"));
    let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.1, 0.05));
    c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.0, true));
    c.set_property_keys(
        Property::MaskFeather,
        vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(1.0, 0.4)],
    );
    c.set_property_keys(Property::Brightness, linear_keys(-0.2, 0.2));
    run("keyed mask feather and brightness + zoom", c, Some("keyframed mask"));
    let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.1, 0.05));
    c.set_property_keys(Property::Opacity, linear_keys(1.0, 0.5));
    run("keyed opacity + zoom (refused)", c, Some("opacity below 1"));
    let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.1, 0.05));
    c.set_property_keys(Property::CropZoom, linear_keys(1.0, 1.5));
    run("keyed crop + zoom (refused)", c, Some("crop"));
    checker_all("keyed colour and mask", fails);
}

// ---- checker/a, second round: what the fix lifted ----------------------------------------------------------------
//
// d4a8fdaa pins the frame pick of a placed clip to `fps ... start_time`, lifts the refusal of a grade ahead of a placement, and
// lifts 4:4:4 and 4:2:2 behind a probe of this FFmpeg's `scale`. Hunts for a frame that is drawn and is not the export's.

/// Every grade of `grades()` ahead of a moving zoom or position, and the grade beside what else a layer carries: a mask, a
/// key, a keyed colour, a fade, a dissolve, a tagged picture, other shapes and formats. A frame the plan draws is the export's to
/// the sample; one it refuses is listed.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_a_grade_ahead_of_a_placement_is_the_exports() {
    use kerf_core::{Property, PropertyKey};
    let m = media();
    let v = variants();
    let sh = shapes();
    let g = grades();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut fails = Vec::new();
    let mut drawn_total = 0;
    let mut run = |name: &str, tl: Timeline, assets: &[Asset], must_draw: bool| {
        let seen = checker_motion(&format!("checker/a/grade {name}"), &tl, assets, 30.0, 0..30, false);
        drawn_total += seen.drawn;
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
        if must_draw && seen.drawn == 0 {
            fails.push(format!("{name}: nothing was drawn: {:?}", seen.refused.first()));
        }
    };
    let over = |c: Clip| timeline(vec![vec![clip(&m.bars, 0.5, 1.5, 0.0)], vec![c]], None);
    let moving = |edit: &dyn Fn(&mut Clip)| {
        let mut c = zooming(&m.testsrc, 1.0, 1.5, (0.1, -0.05));
        edit(&mut c);
        c
    };
    let panning = |edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.testsrc, 0.5, 1.5, 0.0);
        c.keyframes = vec![zoom_key(0.0, 1.0, -0.2, 0.1), zoom_key(1.0, 1.0, 0.2, -0.1)];
        edit(&mut c);
        c
    };
    for (gname, grade) in &g {
        run(
            &format!("`{gname}` + zoom"),
            over(moving(&|c| c.grade = grade.clone())),
            &assets,
            true,
        );
    }
    for (gname, grade) in [&g[1], &g[3], &g[8]] {
        let with = |name: &str, c: Clip, run: &mut RunFn| {
            run(&format!("`{gname}` + {name}"), over(c), &assets, false);
        };
        let mut r = |n: &str, tl: Timeline, a: &[Asset], d: bool| run(n, tl, a, d);
        with("pan", panning(&|c| c.grade = grade.clone()), &mut r);
        with(
            "feathered mask",
            moving(&|c| {
                c.grade = grade.clone();
                c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.25, false));
            }),
            &mut r,
        );
        with(
            "chroma key",
            moving(&|c| {
                c.grade = grade.clone();
                c.effects = vec![key("green", 0.2, 0.2)];
            }),
            &mut r,
        );
        with(
            "keyed brightness",
            moving(&|c| {
                c.grade = grade.clone();
                c.set_property_keys(Property::Brightness, linear_keys(-0.2, 0.2));
            }),
            &mut r,
        );
        with(
            "static colour",
            moving(&|c| {
                c.grade = grade.clone();
                c.color.contrast = 1.2;
                c.color.saturation = 1.1;
            }),
            &mut r,
        );
        with(
            "fade-in",
            moving(&|c| {
                c.grade = grade.clone();
                c.fade_in = 0.4;
            }),
            &mut r,
        );
        with(
            "fade-out",
            moving(&|c| {
                c.grade = grade.clone();
                c.fade_out = 0.4;
            }),
            &mut r,
        );
        with(
            "effect (grayscale)",
            moving(&|c| {
                c.grade = grade.clone();
                c.effects = vec![kerf_core::VideoEffect::Grayscale];
            }),
            &mut r,
        );
        with(
            "keyed mask",
            moving(&|c| {
                c.grade = grade.clone();
                c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.0, false));
                c.set_property_keys(Property::MaskX, vec![PropertyKey::new(0.0, 0.3), PropertyKey::new(1.0, 0.7)]);
            }),
            &mut r,
        );
        // Dissolved in over another clip, and out of one.
        let mut out = clip(&m.bars, 0.5, 1.5, 0.0);
        out.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(1.0, 1.3, 0.0, 0.0)];
        let mut inc = moving(&|c| c.grade = grade.clone());
        inc.timeline_start = 1.0;
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        run(
            &format!("`{gname}` + dissolve in"),
            timeline(vec![vec![out, inc]], None),
            &assets,
            false,
        );
        // Another layer over it.
        let mut top = clip(&m.bars, 0.5, 1.5, 0.0);
        top.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, true));
        run(
            &format!("`{gname}` + a layer over it"),
            timeline(vec![vec![over_clip(m, &g, grade)], vec![top]], None),
            &assets,
            false,
        );
        // Pictures of other shapes, tags and formats.
        for (name, a, under) in [
            ("bt709 tagged", &v.testsrc709, &v.bars709),
            ("smpte170m tagged", &v.testsrc170m, &m.bars),
            ("10-bit", &v.tenbit, &m.bars),
            ("gray", &v.gray, &m.bars),
            ("pillarboxed 480x360", &sh.pillar, &m.bars),
            ("odd width 203x360", &sh.odd_width, &m.bars),
            ("10-bit tagged tv", &sh.tenbit_tv, &m.bars),
        ] {
            let mut c = zooming(a, 1.0, 1.5, (0.1, -0.05));
            c.grade = grade.clone();
            let tl = timeline(vec![vec![clip(under, 0.5, 1.5, 0.0)], vec![c]], None);
            run(&format!("`{gname}` + {name}"), tl, &[under.clone(), a.clone()], false);
        }
        // Cropped (odd width, odd offset), and a delivery frame of another shape.
        let mut c = moving(&|c| c.grade = grade.clone());
        c.transform.crop_right = 0.0013;
        run(&format!("`{gname}` + crop to 639 wide"), over(c), &assets, false);
        let mut c = moving(&|c| c.grade = grade.clone());
        c.transform.crop_left = 0.003;
        run(&format!("`{gname}` + crop from x 1"), over(c), &assets, false);
        let tl = timeline(
            vec![vec![moving(&|c| c.grade = grade.clone())]],
            Some(Delivery::new(640, 480, Fit::Contain)),
        );
        run(&format!("`{gname}` + letterboxed in 640x480"), tl, &assets[1..], false);
    }
    eprintln!("grade ahead of a placement: {drawn_total} frames drawn");
    checker_all("a grade ahead of a placement", fails);
}

/// What a case of the grade hunt is run with: a name, a cut, its assets and whether something must be drawn.
type RunFn<'a> = dyn FnMut(&str, Timeline, &[Asset], bool) + 'a;

/// (A helper for the case above: a plain grey-graded clip that another layer is laid over.)
fn over_clip(m: &Media, g: &[(&'static str, kerf_core::Grade)], grade: &kerf_core::Grade) -> Clip {
    let _ = g;
    let mut c = zooming(&m.testsrc, 1.0, 1.4, (0.0, 0.05));
    c.grade = grade.clone();
    c
}

/// 4:4:4 and 4:2:2 under a placement, once the probe says this FFmpeg converts them in a graph as the decode does: the sizes,
/// tags and containers the probe's 64x36 picture is not (odd widths, a pillarbox, tagged pictures, 4:4:4 from x264), and the
/// formats it must leave refused (10-bit, `yuvj`, 4:4:0, RGB, an alpha plane).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_4_4_4_and_4_2_2_under_a_placement_are_the_exports_or_refused() {
    let m = media();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-chroma");
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str, size: &str, vf: &str, pix: &str, codec: &[&str], flags: &[&str]| -> Option<Asset> {
        let p = dir.join(name);
        let (w, h) = size.split_once('x').unwrap();
        let source = format!("{}x{}", w.parse::<u32>().unwrap().next_multiple_of(2), h);
        let graph = format!("testsrc2=size={source}:rate=30:duration=4{vf},setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown");
        let mut args = vec![
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            graph.as_str(),
        ];
        args.extend_from_slice(codec);
        args.extend_from_slice(&["-pix_fmt", pix]);
        args.extend_from_slice(flags);
        args.push(p.to_str().unwrap());
        let ok = Command::new(kerf_core::ffmpeg_path())
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .ok()?
            .status
            .success();
        ok.then(|| probed(&p))
    };
    let ffv1 = ["-c:v", "ffv1"];
    let x264 = [
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-qp",
        "0",
        "-g",
        "10",
        "-profile:v",
        "high444",
    ];
    let odd = ",crop=w=203:h=360:x=0:y=0:exact=1";
    // (name, asset, drawn expected when the probe matches)
    let mut cases: Vec<(&str, Option<Asset>, bool)> = vec![
        (
            "yuv444p 640x360",
            make("a444.mkv", "640x360", "", "yuv444p", &ffv1, &[]),
            true,
        ),
        (
            "yuv422p 640x360",
            make("a422.mkv", "640x360", "", "yuv422p", &ffv1, &[]),
            true,
        ),
        (
            "yuv444p 203x360 odd width",
            make("b444.mkv", "203x360", odd, "yuv444p", &ffv1, &[]),
            true,
        ),
        (
            "yuv422p 203x360 odd width",
            make("b422.mkv", "203x360", odd, "yuv422p", &ffv1, &[]),
            true,
        ),
        (
            "yuv444p 480x360 pillarbox",
            make("c444.mkv", "480x360", "", "yuv444p", &ffv1, &[]),
            true,
        ),
        (
            "yuv422p 480x360 pillarbox",
            make("c422.mkv", "480x360", "", "yuv422p", &ffv1, &[]),
            true,
        ),
        (
            "yuv444p 640x356",
            make("d444.mkv", "640x356", "", "yuv444p", &ffv1, &[]),
            true,
        ),
        (
            "yuv422p 639x360",
            make(
                "e422.mkv",
                "640x360",
                ",crop=w=639:h=360:x=0:y=0:exact=1",
                "yuv422p",
                &ffv1,
                &[],
            ),
            true,
        ),
        (
            "yuv444p x264 high444 mp4",
            make("f444.mp4", "640x360", "", "yuv444p", &x264, &[]),
            true,
        ),
        (
            "yuv444p tagged bt709",
            make(
                "g444.mkv",
                "640x360",
                ",setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709",
                "yuv444p",
                &ffv1,
                &[],
            ),
            true,
        ),
        (
            "yuv422p tagged bt2020",
            make(
                "h422.mkv",
                "640x360",
                ",setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
                "yuv422p",
                &ffv1,
                &[],
            ),
            true,
        ),
        (
            "yuv444p tagged tv",
            make("i444.mkv", "640x360", "", "yuv444p", &ffv1, &["-color_range", "tv"]),
            true,
        ),
        (
            "yuv444p10le",
            make("j444.mkv", "640x360", "", "yuv444p10le", &ffv1, &[]),
            false,
        ),
        (
            "yuv422p10le",
            make("k422.mkv", "640x360", "", "yuv422p10le", &ffv1, &[]),
            false,
        ),
        (
            "yuvj444p (mjpeg)",
            make(
                "l444.mkv",
                "640x360",
                "",
                "yuvj444p",
                &["-c:v", "mjpeg", "-qscale:v", "1"],
                &[],
            ),
            false,
        ),
        (
            "yuvj422p (mjpeg)",
            make(
                "l422.mkv",
                "640x360",
                "",
                "yuvj422p",
                &["-c:v", "mjpeg", "-qscale:v", "1"],
                &[],
            ),
            false,
        ),
        ("yuv440p", make("m440.mkv", "640x360", "", "yuv440p", &ffv1, &[]), false),
        ("yuva444p", make("n444.mkv", "640x360", "", "yuva444p", &ffv1, &[]), false),
        ("gbrp", make("o.mkv", "640x360", "", "gbrp", &ffv1, &[]), false),
    ];
    let mut fails = Vec::new();
    for (name, asset, drawn_expected) in cases.drain(..) {
        let Some(asset) = asset else {
            eprintln!("checker/a/chroma {name}: skipped (this ffmpeg cannot write it)");
            continue;
        };
        let tl = timeline(
            vec![
                vec![clip(&m.bars, 0.5, 1.5, 0.0)],
                vec![zooming(&asset, 1.0, 1.6, (0.1, 0.0))],
            ],
            None,
        );
        let assets = [m.bars.clone(), asset.clone()];
        let seen = checker_motion(&format!("checker/a/chroma {name}"), &tl, &assets, 30.0, 0..30, false);
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
        if drawn_expected != (seen.drawn > 0) && compositor().caps().placed_chroma {
            let why = seen.refused.first().map(|r| r.1.as_str()).unwrap_or("");
            fails.push(format!(
                "{name}: expected {}, drawn {} refused {}: {why}",
                if drawn_expected { "drawn" } else { "refused" },
                seen.drawn,
                seen.refused.len()
            ));
        }
    }
    // Flagged full range (the plan may draw or refuse it, by the composite's range policy: a drawn frame must be the export's).
    for (name, asset) in [
        (
            "yuv444p flagged pc",
            make("p444.mkv", "640x360", "", "yuv444p", &ffv1, &["-color_range", "pc"]),
        ),
        (
            "yuv422p flagged pc",
            make("p422.mkv", "640x360", "", "yuv422p", &ffv1, &["-color_range", "pc"]),
        ),
    ] {
        let Some(asset) = asset else { continue };
        let tl = timeline(
            vec![
                vec![clip(&m.bars, 0.5, 1.5, 0.0)],
                vec![zooming(&asset, 1.0, 1.6, (0.1, 0.0))],
            ],
            None,
        );
        let assets = [m.bars.clone(), asset.clone()];
        let seen = checker_motion(&format!("checker/a/chroma {name}"), &tl, &assets, 30.0, 0..30, false);
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
    }
    checker_all("4:4:4 and 4:2:2 under a placement", fails);
}

/// An export in parts (the decode budget forced to a tenth of a megapixel, so the span is cut and each part opens the clips on
/// screen for it): the frames of the export are the plan's. (A ranged export is not here: the planner takes its range applied
/// by the caller, so the harness would be comparing different frames.) The budget is an environment variable, so the case runs
/// itself again as a child process with it set, and the other cases of the suite keep their single run.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_a_placed_clip_in_an_export_in_parts() {
    let parts_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-parts");
    if std::env::var_os("KERF_EXPORT_PARTS").is_none() {
        let _ = std::fs::remove_dir_all(&parts_dir);
        let out = Command::new(std::env::current_exe().expect("this test binary"))
            .args([
                "--ignored",
                "--exact",
                "--nocapture",
                "checker_a_a_placed_clip_in_an_export_in_parts",
            ])
            .env("KERF_EXPORT_PARTS", "0.1")
            .env("KERF_SEGMENT_DIR", &parts_dir)
            .stdin(Stdio::null())
            .output()
            .expect("run the case again with the parts budget");
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the export in parts differs from the plan");
        assert!(
            parts_dir.exists(),
            "the export was not cut into parts (no segment directory was made)"
        );
        return;
    }
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let mut fails = Vec::new();
    // In parts: several placed clips back to back, so the cuts land between them and inside them.
    for fps in [30.0f64, 24.0] {
        let mut clips = Vec::new();
        for (i, (from, to)) in [(0.0, 0.7), (0.7, 1.4), (1.4, 2.1)].into_iter().enumerate() {
            let mut c = clip(&m.testsrc, 0.3 * i as f64, 0.3 * i as f64 + 0.7, from);
            c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.7, 1.5, 0.1 * i as f64, 0.0)];
            let _ = to;
            clips.push(c);
        }
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 3.0, 0.0)], clips], None);
        let name = format!("parts @{fps}");
        let seen = checker_motion(&format!("checker/a/{name}"), &tl, &assets, fps, 0..(2.0 * fps) as u64, false);
        if let Some(o) = seen.off.first() {
            fails.push(format!(
                "{name}: frame {} plane {}: {} samples differ (max {}, first at {:?})",
                o.0, o.1, o.2, o.3, o.4
            ));
        }
        if !seen.refused.is_empty() {
            fails.push(format!("{name}: refused {:?}", seen.refused.first()));
        }
    }
    checker_all("an export in parts", fails);
}

// ---- a still image on a Ken Burns move ---------------------------------------------------------------

/// Photographs of the shapes a slideshow has: a 3:2 JPEG, a portrait one, an odd-sized one, a 16:9 PNG, and the formats the route is
/// not measured for.
struct Photos {
    landscape: Asset,
    portrait: Asset,
    odd: Asset,
    wide: Asset,
    jpeg_422: Asset,
    rgba: Asset,
}

fn photos() -> &'static Photos {
    static P: OnceLock<Photos> = OnceLock::new();
    P.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-photos");
        std::fs::create_dir_all(&dir).unwrap();
        let photo = |name: &str, size: &str, quality: &[&str]| {
            let p = dir.join(name);
            let src = format!("testsrc2=size={size}:rate=1:duration=1");
            let mut args = vec!["-f", "lavfi", "-i", src.as_str(), "-frames:v", "1"];
            args.extend(quality.iter().copied());
            args.push(p.to_str().unwrap());
            ffmpeg(&args);
            probed(&p)
        };
        let jpeg = ["-q:v", "3", "-pix_fmt", "yuvj420p"];
        Photos {
            landscape: photo("photo-3x2.jpg", "1920x1280", &jpeg),
            portrait: photo("photo-portrait.jpg", "1280x1920", &jpeg),
            odd: photo("photo-odd.jpg", "1501x1003", &jpeg),
            wide: photo("photo-16x9.png", "1920x1080", &["-pix_fmt", "rgb24"]),
            jpeg_422: photo("photo-422.jpg", "1920x1280", &["-q:v", "3", "-pix_fmt", "yuvj422p"]),
            rgba: photo("photo-rgba.png", "1920x1080", &["-pix_fmt", "rgba"]),
        }
    })
}

/// A photograph held for `secs` from `start`, zooming in from 1.2x at the top left to the frame: nothing hangs off it.
fn ken_burns(photo: &Asset, start: f64, secs: f64) -> Clip {
    let mut c = clip(photo, 0.0, secs, start);
    c.keyframes = vec![zoom_key(0.0, 1.2, 0.04, 0.03), zoom_key(secs, 1.0, 0.0, 0.0)];
    c
}

fn cover(w: u32, h: u32) -> Option<Delivery> {
    Some(Delivery::new(w, h, Fit::Cover))
}

/// Whether the plan of the export takes the moving still's route for a layer at `k`.
fn takes_the_route(tl: &Timeline, assets: &[Asset], fps: f64, k: u64) -> bool {
    let planner = Planner::new(tl, assets, &at_fps(fps), PlanRequest::motion(policy())).expect("planner");
    let plan = planner.at_frame(k).expect("plan");
    !plan.layers.is_empty() && plan.layers.iter().any(|l| plan.takes_still_route(l))
}

/// [`run`] for a case with a JPEG in the cut: drawn where this FFmpeg's composite range is fixed (6.1.1), refused whole where it is
/// negotiated (9.0.2 flags a cut that holds a full-range picture full range, and converts every frame of it, the ones before and after
/// the picture too).
fn run_jpeg(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) {
    if policy() == CompositeColorPolicy::FixedBt601 {
        run(case, tl, assets, fps, secs);
    } else {
        run_refusing(case, tl, assets, fps, secs, &full_range_cut_refused());
    }
}

/// A moving still is one picture, moved by one `perspective` per frame: the planes of every export frame are the compositor's,
/// every sample, for each kind of photograph the route is measured for.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_still_is_drawn_as_the_export_draws_it() {
    let p = photos();
    for (name, photo) in [
        ("jpeg 3:2", &p.landscape),
        ("jpeg portrait", &p.portrait),
        ("jpeg odd", &p.odd),
        ("png 16:9", &p.wide),
    ] {
        let tl = timeline(vec![vec![ken_burns(photo, 0.0, 1.0)]], cover(1920, 1080));
        assert!(
            takes_the_route(&tl, std::slice::from_ref(photo), 30.0, 3),
            "{name}: not the moving still's route"
        );
        let case = format!("ken-burns/{name}");
        if name.starts_with("png") {
            run(&case, &tl, std::slice::from_ref(photo), 30.0, 1.0);
        } else {
            run_jpeg(&case, &tl, std::slice::from_ref(photo), 30.0, 1.0);
        }
    }
    // The frame rates the pick and the pose are counted in, a clip that starts between two frames and one the speed retimes.
    let png = std::slice::from_ref(&p.wide);
    for fps in [24.0, 25.0, 60.0] {
        let tl = timeline(vec![vec![ken_burns(&p.wide, 0.37, 1.0)]], cover(960, 540));
        run_range(
            &format!("ken-burns/png at {fps} fps, late"),
            &tl,
            png,
            fps,
            (0.37 * fps) as u64..(1.37 * fps) as u64 - 1,
        );
    }
    let mut fast = ken_burns(&p.wide, 0.0, 1.0);
    fast.speed = 2.0;
    run(
        "ken-burns/png at speed 2",
        &timeline(vec![vec![fast]], cover(960, 540)),
        png,
        30.0,
        0.5,
    );
    let mut back = ken_burns(&p.wide, 0.0, 1.0);
    back.speed = -1.0;
    run(
        "ken-burns/png reversed",
        &timeline(vec![vec![back]], cover(960, 540)),
        png,
        30.0,
        1.0,
    );
}

/// The shapes of the move, as the still has them: a held zoom with a pan, a crop first, three keys eased, `Contain` zoomed past the
/// pillarbox, a vertical frame, and the layers around it (a video under a photograph, a photograph under a video).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_shapes_of_a_ken_burns_move_are_drawn_as_the_export_draws_them() {
    let p = photos();
    let m = media();
    let png = std::slice::from_ref(&p.wide);
    let mut held = clip(&p.wide, 0.0, 1.0, 0.0);
    held.keyframes = vec![zoom_key(0.0, 1.4, -0.1, 0.05), zoom_key(1.0, 1.4, 0.1, -0.05)];
    run(
        "ken-burns/held zoom, panning",
        &timeline(vec![vec![held]], cover(960, 540)),
        png,
        30.0,
        1.0,
    );
    let mut keyed = clip(&p.wide, 0.0, 1.0, 0.0);
    keyed.keyframes = vec![
        zoom_key(0.0, 1.5, 0.0, 0.0),
        zoom_key(0.4, 1.15, 0.05, 0.02),
        zoom_key(1.0, 1.6, -0.1, 0.0),
    ];
    keyed.keyframes[0].easing = kerf_core::Easing::EaseInOut;
    run(
        "ken-burns/three keys, eased",
        &timeline(vec![vec![keyed]], cover(960, 540)),
        png,
        30.0,
        1.0,
    );
    let mut cropped = ken_burns(&p.wide, 0.0, 1.0);
    cropped.transform.crop_left = 0.1;
    cropped.transform.crop_right = 0.05;
    cropped.transform.crop_top = 0.08;
    run(
        "ken-burns/cropped first",
        &timeline(vec![vec![cropped]], cover(960, 540)),
        png,
        30.0,
        1.0,
    );
    let mut pillar = clip(&p.landscape, 0.0, 1.0, 0.0);
    pillar.keyframes = vec![zoom_key(0.0, 1.9, 0.05, 0.0), zoom_key(1.0, 1.7, -0.05, 0.0)];
    let tl = timeline(vec![vec![pillar]], Some(Delivery::new(960, 540, Fit::Contain)));
    run_jpeg("ken-burns/contain", &tl, std::slice::from_ref(&p.landscape), 30.0, 1.0);
    let tl = timeline(vec![vec![ken_burns(&p.landscape, 0.0, 1.0)]], cover(540, 960));
    run_jpeg("ken-burns/vertical frame", &tl, std::slice::from_ref(&p.landscape), 30.0, 1.0);
    // Over a video, and under one.
    let tl = timeline(
        vec![vec![clip(&m.testsrc, 0.5, 1.5, 0.0)], vec![ken_burns(&p.wide, 0.0, 1.0)]],
        cover(640, 360),
    );
    run("ken-burns/over video", &tl, &[m.testsrc.clone(), p.wide.clone()], 30.0, 1.0);
    let mut pip = clip(&m.testsrc, 0.5, 1.5, 0.0);
    pip.transform.scale = 0.5;
    pip.transform.pos_x = 0.25;
    let tl = timeline(vec![vec![ken_burns(&p.wide, 0.0, 1.0)], vec![pip]], cover(640, 360));
    run(
        "ken-burns/under a picture in picture",
        &tl,
        &[m.testsrc.clone(), p.wide.clone()],
        30.0,
        1.0,
    );
}

/// The move inside a transition: the outgoing clip plays on past its end under the next with its move, and the incoming one is faded,
/// dissolved, slid or pushed as the graph does it after the `perspective`.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_still_inside_a_transition_is_drawn_as_the_export_draws_it() {
    let p = photos();
    let png = std::slice::from_ref(&p.wide);
    let one = |edit: &dyn Fn(&mut Clip)| {
        let mut c = ken_burns(&p.wide, 0.0, 1.0);
        edit(&mut c);
        timeline(vec![vec![c]], cover(960, 540))
    };
    run("ken-burns/fade-in", &one(&|c| c.fade_in = 0.5), png, 30.0, 1.0);
    run("ken-burns/fade-out", &one(&|c| c.fade_out = 0.5), png, 30.0, 1.0);
    run(
        "ken-burns/fades both",
        &one(&|c| {
            c.fade_in = 0.4;
            c.fade_out = 0.4;
        }),
        png,
        30.0,
        1.0,
    );
    for (name, kind) in [
        ("crossfade", TransitionKind::Crossfade),
        ("dip to black", TransitionKind::DipToBlack),
        ("slide", TransitionKind::SlideLeft),
        ("push", TransitionKind::PushLeft),
    ] {
        for (order, a, b) in [
            ("png into jpeg", &p.wide, &p.landscape),
            ("jpeg into png", &p.landscape, &p.wide),
            ("png into png", &p.wide, &p.wide),
        ] {
            let out = ken_burns(a, 0.0, 1.0);
            let mut inc = ken_burns(b, 1.0, 1.0);
            inc.transition_in = Some(Transition { kind, duration: 0.6 });
            let tl = timeline(vec![vec![out, inc]], cover(960, 540));
            let assets = [a.clone(), b.clone()];
            let case = format!("ken-burns/{name}, {order}");
            if order == "png into png" {
                run(&case, &tl, &assets, 30.0, 2.0);
            } else {
                run_jpeg(&case, &tl, &assets, 30.0, 2.0);
            }
        }
    }
    // The slideshow itself: JPEGs and a PNG of three shapes, the first fading in, crossfaded, the last fading out.
    let mut clips = Vec::new();
    for (i, photo) in [&p.landscape, &p.portrait, &p.wide, &p.landscape].into_iter().enumerate() {
        let mut c = ken_burns(photo, i as f64 * 0.8, 1.0);
        if i > 0 {
            c.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 0.2,
            });
        }
        clips.push(c);
    }
    clips[0].fade_in = 0.3;
    clips[3].fade_out = 0.3;
    let tl = timeline(vec![clips], cover(960, 540));
    run_jpeg(
        "ken-burns/slideshow",
        &tl,
        &[p.landscape.clone(), p.portrait.clone(), p.wide.clone()],
        30.0,
        3.4,
    );
}

/// What the route is not drawn for stays FFmpeg's, on every frame: the picture formats it was not measured for, a mask, a colour, a
/// translucent layer, an effect, a keyed crop.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_a_moving_still_is_not_drawn_for_is_refused_in_an_export_frame() {
    let p = photos();
    let still = |c: Clip| timeline(vec![vec![c]], cover(960, 540));
    for (name, photo) in [("4:2:2 jpeg", &p.jpeg_422), ("png with alpha", &p.rgba)] {
        refused(
            &format!("ken-burns/{name}"),
            &still(ken_burns(photo, 0.0, 1.0)),
            std::slice::from_ref(photo),
            &at_fps(30.0),
            upto(1.0, 30.0),
            "a still whose picture moves",
        );
    }
    let png = std::slice::from_ref(&p.wide);
    let case = |name: &str, edit: &dyn Fn(&mut Clip)| {
        let mut c = ken_burns(&p.wide, 0.0, 1.0);
        edit(&mut c);
        refused(
            &format!("ken-burns/{name}"),
            &still(c),
            png,
            &at_fps(30.0),
            upto(1.0, 30.0),
            "a still whose picture moves",
        );
    };
    case("colour", &|c| {
        c.color = Color {
            contrast: 1.2,
            ..Color::default()
        }
    });
    case("translucent", &|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.6));
    case("grayscale", &|c| c.effects = vec![kerf_core::VideoEffect::Grayscale]);
    // A mask, a blur, a rotation: the general chain draws those (the move is a moving zoom there, placed after them), so the route is
    // not taken at all.
    for (name, edit) in [
        (
            "mask",
            Box::new(|c: &mut Clip| c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false))) as Box<dyn Fn(&mut Clip)>,
        ),
        (
            "blur",
            Box::new(|c: &mut Clip| c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 3.0 }]),
        ),
        (
            "rotation",
            Box::new(|c: &mut Clip| c.keyframes.iter_mut().for_each(|k| k.rotation = 4.0)),
        ),
    ] {
        let mut c = ken_burns(&p.wide, 0.0, 1.0);
        edit(&mut c);
        let tl = still(c);
        assert!(!takes_the_route(&tl, png, 30.0, 3), "{name} takes the moving still's route");
        // ... and no case holds the footage placement to a still of those, so the plan refuses it and FFmpeg renders it.
        refused(
            &format!("ken-burns/{name}"),
            &tl,
            png,
            &at_fps(30.0),
            upto(1.0, 30.0),
            "a still whose picture moves",
        );
    }
    // A keyed crop is the moving still's too, which no pass draws.
    let mut cropping = ken_burns(&p.wide, 0.0, 1.0);
    cropping.set_property_keys(
        kerf_core::Property::CropZoom,
        vec![kerf_core::PropertyKey::new(0.0, 1.0), kerf_core::PropertyKey::new(1.0, 1.3)],
    );
    refused(
        "ken-burns/keyed crop",
        &still(cropping),
        png,
        &at_fps(30.0),
        upto(1.0, 30.0),
        "keyframed crop",
    );
}

/// A move that leaves the frame bare at its start is the general chain's: the picture is fitted to the frame (a `scale` and a `crop`) and then
/// placed by the placement, and FFmpeg makes the fit picture the placement is laid from, in the same pass that converts a JPEG's range or an
/// RGB picture, so an enlargement of a full-range JPEG and a PNG are drawn to the sample.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_move_that_pans_off_the_picture_is_placed_from_the_fit_ffmpeg_makes_in_an_export_frame() {
    let p = photos();
    let edge = |photo: &Asset, start: f64| {
        let mut c = clip(photo, 0.0, 1.0, start);
        c.keyframes = vec![zoom_key(0.0, 1.0, -0.03, 0.0), zoom_key(1.0, 1.18, 0.03, -0.02)];
        c
    };
    for (name, photo) in [
        ("jpeg 3:2", &p.landscape),
        ("jpeg portrait", &p.portrait),
        ("jpeg odd", &p.odd),
        ("png 16:9", &p.wide),
    ] {
        let tl = timeline(vec![vec![edge(photo, 0.0)]], cover(1920, 1080));
        assert!(
            !takes_the_route(&tl, std::slice::from_ref(photo), 30.0, 3),
            "{name}: the moving still's route"
        );
        let case = format!("pan-off/{name}");
        if name.starts_with("png") {
            run(&case, &tl, std::slice::from_ref(photo), 30.0, 1.0);
        } else {
            run_jpeg(&case, &tl, std::slice::from_ref(photo), 30.0, 1.0);
        }
    }
    // Fading in, dissolving into another photograph, and the slideshow's own mix of both routes.
    let mut fading = edge(&p.portrait, 0.0);
    fading.fade_in = 0.5;
    run_jpeg(
        "pan-off/jpeg portrait, fading in",
        &timeline(vec![vec![fading]], cover(960, 540)),
        std::slice::from_ref(&p.portrait),
        30.0,
        1.0,
    );
    let mut inc = edge(&p.portrait, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    run_jpeg(
        "pan-off/png into jpeg portrait, dissolving",
        &timeline(vec![vec![edge(&p.wide, 0.0), inc]], cover(960, 540)),
        &[p.wide.clone(), p.portrait.clone()],
        30.0,
        2.0,
    );
    // The photo slideshow archetype: the three shapes on a slow move, half of them panning off the frame's edge, half inside it,
    // crossfaded, the first fading in and the last out.
    let mut clips = Vec::new();
    for (i, photo) in [&p.landscape, &p.portrait, &p.wide, &p.landscape, &p.portrait, &p.wide]
        .into_iter()
        .enumerate()
    {
        let mut c = clip(photo, 0.0, 1.0, i as f64 * 0.8);
        let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
        c.keyframes = if i % 4 < 2 {
            vec![zoom_key(0.0, 1.0, -0.03 * dir, 0.0), zoom_key(1.0, 1.18, 0.03 * dir, -0.02)]
        } else {
            vec![zoom_key(0.0, 1.2, 0.04 * dir, 0.03), zoom_key(1.0, 1.0, 0.0, 0.0)]
        };
        if i > 0 {
            c.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 0.2,
            });
        }
        clips.push(c);
    }
    clips[0].fade_in = 0.3;
    clips[5].fade_out = 0.3;
    let tl = timeline(vec![clips], cover(960, 540));
    run_jpeg(
        "ken-burns/slideshow, both routes",
        &tl,
        &[p.landscape.clone(), p.portrait.clone(), p.wide.clone()],
        30.0,
        4.8,
    );
}

// ---- checker/t6: a still image on a Ken Burns move, in export frames, cases the implementer did not write -----------------
//
// The independent check of work package T6. Every drawn frame is compared plane by plane with the export's, to the sample. A frame
// is drawn and exact, or the plan refuses it (and FFmpeg draws it): a case fails on a drawn frame that differs, and on a refusal
// where the case says the frames must be drawn.

/// Whether a case's frames must be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Draw {
    /// Whatever the plan decides, as long as what is drawn is exact.
    Any,
    /// Drawn on every FFmpeg (a PNG alone, or among limited-range footage).
    Always,
    /// Drawn where the composite's range is fixed (6.1.1): a JPEG's full range is negotiated on 9.0.2 and the cut that holds one is refused.
    Jpeg,
}

#[path = "common/t6.rs"]
mod t6_files;

fn t6_pic(kind: &str, w: u32, h: u32) -> Asset {
    if kind == "jpeg" {
        t6_files::jpeg(w, h)
    } else {
        t6_files::png(w, h)
    }
}

fn t6_draw(kind: &str) -> Draw {
    if kind == "jpeg" {
        Draw::Jpeg
    } else {
        Draw::Always
    }
}

/// A photograph held for `secs` from `start`, on the keys `(time, scale, x, y)`.
fn t6_move(photo: &Asset, start: f64, secs: f64, keys: &[(f64, f64, f64, f64)]) -> Clip {
    let mut c = clip(photo, 0.0, secs, start);
    c.keyframes = keys.iter().map(|&(t, s, x, y)| zoom_key(t, s, x, y)).collect();
    c
}

/// The slideshow's own move over `secs`: in from 1.2x towards the top left, out to the frame.
fn t6_in(photo: &Asset, start: f64, secs: f64) -> Clip {
    t6_move(photo, start, secs, &[(0.0, 1.2, 0.04, 0.03), (secs, 1.0, 0.0, 0.0)])
}

/// The move that pans off the picture's edge (the placement's, with FFmpeg making the fit picture).
fn t6_off(photo: &Asset, start: f64, secs: f64) -> Clip {
    t6_move(photo, start, secs, &[(0.0, 1.0, -0.03, 0.0), (secs, 1.18, 0.03, -0.02)])
}

struct T6Export {
    name: String,
    tl: Timeline,
    assets: Vec<Asset>,
    opts: ExportOptions,
    frames: Range<u64>,
    draw: Draw,
}

fn t6_export(name: &str, tl: Timeline, assets: &[Asset], fps: f64, secs: f64, draw: Draw) -> T6Export {
    T6Export {
        name: name.to_string(),
        tl,
        assets: assets.to_vec(),
        opts: at_fps(fps),
        frames: upto(secs, fps),
        draw,
    }
}

impl T6Export {
    fn frames(mut self, frames: Range<u64>) -> Self {
        self.frames = frames;
        self
    }

    fn with_opts(mut self, opts: ExportOptions) -> Self {
        self.opts = opts;
        self
    }
}

fn t6_exports(what: &str, cases: Vec<T6Export>) {
    let mut fails = Vec::new();
    let (mut drawn, mut refused) = (0, 0);
    for c in cases {
        let seen = checker_motion_with(&c.name, &c.tl, &c.assets, &c.assets, &c.opts, c.frames.clone(), false);
        drawn += seen.drawn;
        refused += seen.refused.len();
        if let Some(o) = seen.off.first() {
            let frames: std::collections::BTreeSet<u64> = seen.off.iter().map(|o| o.0).collect();
            fails.push(format!(
                "{}: {} frame(s) differ ({:?}); first: frame {} plane {}: {} samples differ (max {}, first at {:?}); montage in target/parity/motion",
                c.name,
                frames.len(),
                frames.iter().take(10).collect::<Vec<_>>(),
                o.0,
                o.1,
                o.2,
                o.3,
                o.4
            ));
        }
        let must = match c.draw {
            Draw::Any => false,
            Draw::Always => true,
            Draw::Jpeg => policy() == CompositeColorPolicy::FixedBt601,
        };
        if must && (!seen.refused.is_empty() || seen.drawn == 0) {
            fails.push(format!(
                "{}: refused where it must be drawn ({} frames; first {:?})",
                c.name,
                seen.refused.len(),
                seen.refused.first()
            ));
        }
    }
    eprintln!(
        "T6 {what}: {drawn} frames drawn exact, {refused} refused, {} failing cases",
        fails.len()
    );
    checker_all(what, fails);
}

/// Photographs of every size in a slideshow of one clip each, on both moves, both kinds.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_of_photographs_of_every_size() {
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        for (w, h) in [
            (16, 16),
            (17, 31),
            (31, 17),
            (64, 36),
            (640, 360),
            (1501, 1003),
            (1281, 721),
            (1920, 1080),
            (1080, 1920),
            (1600, 400),
            (400, 1600),
            (4000, 3000),
            (8192, 4608),
            (8192, 2048),
            (2048, 8192),
        ] {
            let photo = t6_pic(kind, w, h);
            let assets = std::slice::from_ref(&photo);
            for (route, clip) in [
                ("inside", t6_in(&photo, 0.0, 1.5)),
                ("off the edge", t6_off(&photo, 0.0, 1.5)),
            ] {
                let tl = timeline(vec![vec![clip]], cover(320, 180));
                cases.push(t6_export(
                    &format!("checker/t6/size {kind} {w}x{h} {route}"),
                    tl,
                    assets,
                    10.0,
                    1.5,
                    Draw::Any,
                ));
            }
        }
    }
    t6_exports("photograph sizes in export frames", cases);
}

/// Every frame rate an export is cut at, with a clip that starts and keys between frames, a speed and a reverse.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_at_every_frame_rate() {
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1501, 1003);
        let assets = std::slice::from_ref(&photo);
        for fps in [
            1.0, 7.5, 12.5, 23.976, 24.0, 25.0, 29.97, 30.0, 48.0, 50.0, 59.94, 60.0, 120.0,
        ] {
            let secs = if fps > 60.0 { 1.0 } else { 2.0 };
            // Keys that fall between the frames of every rate, and a late start that does too.
            let c = t6_move(
                &photo,
                0.337,
                secs,
                &[
                    (0.011, 1.35, 0.06, 0.02),
                    (secs * 0.53, 1.12, -0.03, 0.0),
                    (secs - 0.013, 1.05, 0.01, -0.01),
                ],
            );
            let tl = timeline(vec![vec![c]], cover(320, 180));
            let end = ((0.337 + secs) * fps).floor() as u64;
            cases.push(
                t6_export(
                    &format!("checker/t6/{fps} fps {kind}, late"),
                    tl,
                    assets,
                    fps,
                    secs,
                    t6_draw(kind),
                )
                .frames(0..end),
            );
        }
        for (name, speed) in [
            ("speed 0.5", 0.5),
            ("speed 1.5", 1.5),
            ("speed 4", 4.0),
            ("reversed", -1.0),
            ("reversed x2", -2.0),
        ] {
            let mut c = t6_in(&photo, 0.0, 2.0);
            c.speed = speed;
            let secs = 2.0 / speed.abs();
            cases.push(t6_export(
                &format!("checker/t6/{name} {kind}"),
                timeline(vec![vec![c]], cover(320, 180)),
                assets,
                25.0,
                secs,
                t6_draw(kind),
            ));
        }
    }
    t6_exports("frame rates in export frames", cases);
}

/// A very long clip: the copy is made once and reused for every frame, and the table steps by less than a sample a frame.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_of_a_very_long_clip_reuses_the_copy() {
    let mut cases = Vec::new();
    for kind in ["png", "jpeg"] {
        let photo = t6_pic(kind, 1920, 1080);
        let assets = std::slice::from_ref(&photo);
        let secs = 240.0;
        let c = t6_move(&photo, 0.0, secs, &[(0.0, 1.0, 0.0, 0.0), (secs, 1.4, 0.05, 0.02)]);
        cases.push(t6_export(
            &format!("checker/t6/{secs} s clip {kind}"),
            timeline(vec![vec![c]], cover(160, 90)),
            assets,
            5.0,
            secs - 0.2,
            t6_draw(kind),
        ));
    }
    t6_exports("a very long clip", cases);
}

/// Zoom and pan at the numbers a branch switches at, in export frames.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_at_the_extremes_of_zoom_and_pan() {
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1920, 1280);
        let assets = std::slice::from_ref(&photo);
        let o = (0.0, 0.0);
        for (name, a, b, draw) in [
            ("zoom 1 to 1.0001", (1.0, o), (1.0001, o), true),
            ("zoom 1.0001 to 1", (1.0001, o), (1.0, o), true),
            ("zoom 1 to 4", (1.0, o), (4.0, o), true),
            ("zoom 4 to 1", (4.0, o), (1.0, o), true),
            ("zoom 6 to 6.5 panning", (6.0, (-0.2, 0.1)), (6.5, (0.2, -0.1)), false),
            ("zoom 1.2 pan to the edge", (1.2, (0.1, -0.1)), (1.2, (-0.1, 0.1)), true),
            (
                "zoom 1.2 pan 1 sample inside",
                (1.2, (0.1 - 0.5 / 1920.0, 0.0)),
                (1.2, (-0.1 + 0.5 / 1920.0, 0.0)),
                true,
            ),
            ("zoom 1.2 pan 1e-7 past", (1.2, (0.1 + 1e-7, 0.0)), (1.2, o), false),
            (
                "zoom 1.2 pan 1 sample past",
                (1.2, (0.1 + 1.0 / 1920.0, 0.0)),
                (1.2, o),
                false,
            ),
            ("zoom 1.2 pan 1 row past", (1.2, (0.0, 0.1 + 1.0 / 1080.0)), (1.2, o), false),
            ("pan 2e-9 at zoom 1", (1.0, o), (1.0, (2e-9, 0.0)), false),
            ("zoom 1.02", (1.02, (0.01, 0.0)), (1.0, o), false),
        ] {
            let c = t6_move(&photo, 0.0, 2.0, &[(0.0, a.0, a.1 .0, a.1 .1), (2.0, b.0, b.1 .0, b.1 .1)]);
            cases.push(t6_export(
                &format!("checker/t6/{name} {kind}"),
                timeline(vec![vec![c]], cover(480, 270)),
                assets,
                12.0,
                2.0,
                if draw { t6_draw(kind) } else { Draw::Any },
            ));
        }
    }
    t6_exports("zoom and pan extremes in export frames", cases);
}

/// Easings and key layouts in export frames (the curve is an expression of the frame's time in the graph).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_on_every_easing_and_key_layout() {
    use kerf_core::Easing as E;
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1920, 1280);
        let assets = std::slice::from_ref(&photo);
        let draw = t6_draw(kind);
        for easing in [
            E::Linear,
            E::Hold,
            E::EaseIn,
            E::EaseOut,
            E::EaseInOut,
            E::Bezier {
                x1: 0.1,
                y1: 0.9,
                x2: 0.9,
                y2: 0.1,
            },
            E::Bezier {
                x1: 1.0,
                y1: 0.0,
                x2: 0.0,
                y2: 1.0,
            },
        ] {
            let mut c = t6_move(&photo, 0.0, 2.0, &[(0.0, 1.45, 0.12, 0.1), (2.0, 1.05, -0.02, 0.0)]);
            c.keyframes[0].easing = easing;
            cases.push(t6_export(
                &format!("checker/t6/easing {easing:?} {kind}"),
                timeline(vec![vec![c]], cover(480, 270)),
                assets,
                25.0,
                2.0,
                draw,
            ));
        }
        let mut c = t6_move(&photo, 0.0, 2.0, &[(0.5, 1.4, 0.1, 0.0), (1.5, 1.1, -0.05, 0.02)]);
        c.keyframes[0].easing = E::EaseInOut;
        cases.push(t6_export(
            &format!("checker/t6/keys inside the clip {kind}"),
            timeline(vec![vec![c]], cover(480, 270)),
            assets,
            25.0,
            2.0,
            draw,
        ));
        let c = t6_move(&photo, 0.0, 2.0, &[(0.0, 1.0, 0.0, 0.0), (8.0, 1.6, 0.2, 0.0)]);
        cases.push(t6_export(
            &format!("checker/t6/keys past the end {kind}"),
            timeline(vec![vec![c]], cover(480, 270)),
            assets,
            25.0,
            2.0,
            draw,
        ));
        let keys: Vec<(f64, f64, f64, f64)> = (0..12)
            .map(|i| {
                let f = f64::from(i);
                (
                    f * 2.0 / 11.0,
                    1.3 + 0.25 * (f * 1.7).sin(),
                    0.12 * (f * 2.3).sin(),
                    0.08 * (f * 0.9).cos(),
                )
            })
            .collect();
        let mut c = t6_move(&photo, 0.0, 2.0, &keys);
        let easings = [E::Linear, E::EaseIn, E::EaseOut, E::EaseInOut, E::Hold, E::Linear];
        for (i, k) in c.keyframes.iter_mut().enumerate() {
            k.easing = easings[i % easings.len()];
        }
        cases.push(t6_export(
            &format!("checker/t6/twelve keys {kind}"),
            timeline(vec![vec![c]], cover(480, 270)),
            assets,
            25.0,
            2.0,
            draw,
        ));
    }
    t6_exports("easings and key layouts in export frames", cases);
}

/// Fits and static crops in export frames.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_in_every_fit_and_crop() {
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        for (shape, (w, h)) in [("3:2", (1920, 1280)), ("portrait", (1280, 1920)), ("square", (1200, 1200))] {
            let photo = t6_pic(kind, w, h);
            for (fit_name, fit) in [("cover", Fit::Cover), ("contain", Fit::Contain), ("blur", Fit::Blur)] {
                for (frame_name, (fw, fh)) in [("16:9", (320, 180)), ("9:16", (180, 320)), ("1:1", (240, 240))] {
                    let covers = if fit == Fit::Cover { 1.0 } else { 4.0 };
                    let c = t6_move(
                        &photo,
                        0.0,
                        1.5,
                        &[(0.0, covers * 1.25, 0.02, 0.01), (1.5, covers * 1.1, -0.02, -0.01)],
                    );
                    cases.push(t6_export(
                        &format!("checker/t6/{kind} {shape} in {frame_name} {fit_name}"),
                        timeline(vec![vec![c]], Some(Delivery::new(fw, fh, fit))),
                        std::slice::from_ref(&photo),
                        10.0,
                        1.5,
                        Draw::Any,
                    ));
                }
            }
        }
        let photo = t6_pic(kind, 1920, 1280);
        for (name, crop) in [
            ("left 10%", (0.1, 0.0, 0.0, 0.0)),
            ("all four", (0.11, 0.07, 0.13, 0.05)),
            ("tiny", (0.001, 0.001, 0.001, 0.001)),
            ("most of it", (0.45, 0.45, 0.4, 0.4)),
            ("odd fractions", (0.123456789, 0.0987654321, 0.0333333333, 0.0666666667)),
        ] {
            let mut c = t6_move(&photo, 0.0, 1.5, &[(0.0, 1.3, 0.02, 0.0), (1.5, 1.1, -0.02, 0.0)]);
            (
                c.transform.crop_left,
                c.transform.crop_right,
                c.transform.crop_top,
                c.transform.crop_bottom,
            ) = (crop.0, crop.1, crop.2, crop.3);
            cases.push(t6_export(
                &format!("checker/t6/{kind} crop {name}"),
                timeline(vec![vec![c]], cover(320, 180)),
                std::slice::from_ref(&photo),
                10.0,
                1.5,
                Draw::Any,
            ));
        }
    }
    t6_exports("fits and crops in export frames", cases);
}

/// A still among video and other stills: dissolves in both directions, three photographs of both kinds and all three shapes in
/// overlapping transitions, a still over video on another track, a title, and the same photograph on two tracks at once.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_among_video_and_other_stills() {
    let m = media();
    let (jpg, png, portrait) = (
        t6_pic("jpeg", 1920, 1280),
        t6_pic("png", 1920, 1080),
        t6_pic("jpeg", 1280, 1920),
    );
    let mut cases = Vec::new();
    let frame = || cover(640, 360);
    let into = |c: &mut Clip, kind: TransitionKind, d: f64| c.transition_in = Some(Transition { kind, duration: d });
    for (name, kind) in [
        ("crossfade", TransitionKind::Crossfade),
        ("dip to black", TransitionKind::DipToBlack),
        ("slide left", TransitionKind::SlideLeft),
        ("slide down", TransitionKind::SlideDown),
        ("push right", TransitionKind::PushRight),
    ] {
        for (still, photo, draw) in [("png", &png, Draw::Always), ("jpeg", &jpg, Draw::Jpeg)] {
            let mut inc = t6_in(photo, 1.0, 2.0);
            into(&mut inc, kind, 0.8);
            cases.push(t6_export(
                &format!("checker/t6/{name}: video into {still}"),
                timeline(vec![vec![clip(&m.testsrc, 0.0, 1.5, 0.0), inc]], frame()),
                &[m.testsrc.clone(), photo.clone()],
                25.0,
                3.0,
                draw,
            ));
            let mut inc = clip(&m.bars, 0.0, 1.5, 1.5);
            into(&mut inc, kind, 0.8);
            cases.push(t6_export(
                &format!("checker/t6/{name}: {still} into video"),
                timeline(vec![vec![t6_in(photo, 0.0, 2.0), inc]], frame()),
                &[m.bars.clone(), photo.clone()],
                25.0,
                3.0,
                draw,
            ));
        }
        for (name2, a, b) in [
            ("png into jpeg", &png, &jpg),
            ("jpeg into png", &jpg, &png),
            ("png into png", &png, &png),
        ] {
            let mut inc = t6_in(b, 1.0, 2.0);
            into(&mut inc, kind, 0.7);
            let draw = if name2 == "png into png" { Draw::Always } else { Draw::Jpeg };
            cases.push(t6_export(
                &format!("checker/t6/{name}: {name2}"),
                timeline(vec![vec![t6_in(a, 0.0, 1.5), inc]], frame()),
                &[a.clone(), b.clone()],
                25.0,
                3.0,
                draw,
            ));
        }
    }
    // Three photographs, overlapping dissolves (a clip in two transitions at once).
    let mut a = t6_in(&jpg, 0.0, 1.5);
    let mut b = t6_off(&png, 1.0, 1.5);
    let mut c = t6_move(&portrait, 1.9, 1.5, &[(0.0, 1.25, -0.04, 0.0), (1.5, 1.05, 0.01, 0.0)]);
    a.fade_in = 0.3;
    into(&mut b, TransitionKind::Crossfade, 0.5);
    into(&mut c, TransitionKind::Crossfade, 0.6);
    c.fade_out = 0.4;
    cases.push(t6_export(
        "checker/t6/three photographs, overlapping dissolves",
        timeline(vec![vec![a, b, c]], frame()),
        &[jpg.clone(), png.clone(), portrait.clone()],
        25.0,
        3.4,
        Draw::Jpeg,
    ));
    // A still over video, fading; the same photograph twice on two tracks; a title over it.
    let mut over = t6_in(&png, 0.5, 1.5);
    over.fade_in = 0.5;
    over.fade_out = 0.5;
    cases.push(t6_export(
        "checker/t6/png fading over video",
        timeline(vec![vec![clip(&m.testsrc, 0.0, 2.5, 0.0)], vec![over]], frame()),
        &[m.testsrc.clone(), png.clone()],
        25.0,
        2.5,
        Draw::Always,
    ));
    let (mut a, mut b) = (t6_in(&png, 0.0, 2.0), t6_off(&png, 0.5, 1.0));
    b.fade_in = 0.4;
    a.fade_out = 0.2;
    cases.push(t6_export(
        "checker/t6/the same png on two tracks",
        timeline(vec![vec![a], vec![b]], frame()),
        std::slice::from_ref(&png),
        25.0,
        2.0,
        Draw::Always,
    ));
    let mut tl = timeline(vec![vec![t6_in(&png, 0.0, 2.0)]], frame());
    tl.overlays = vec![kerf_core::TextOverlay::new("Summer 2026", 0.3, 1.8)];
    cases.push(t6_export(
        "checker/t6/title over png",
        tl,
        std::slice::from_ref(&png),
        25.0,
        2.0,
        Draw::Any,
    ));
    // The slideshow archetype, long: twelve photographs of both kinds and three shapes, crossfaded, the first fading in.
    let shapes = [&jpg, &png, &portrait];
    let mut clips = Vec::new();
    for i in 0..12 {
        let photo = shapes[i % 3];
        let mut c = if i % 4 < 2 {
            t6_off(photo, i as f64 * 1.0, 1.4)
        } else {
            t6_in(photo, i as f64 * 1.0, 1.4)
        };
        if i > 0 {
            into(&mut c, TransitionKind::Crossfade, 0.4);
        }
        clips.push(c);
    }
    clips[0].fade_in = 0.5;
    clips[11].fade_out = 0.5;
    cases.push(t6_export(
        "checker/t6/slideshow of twelve",
        timeline(vec![clips], frame()),
        &[jpg.clone(), png.clone(), portrait.clone()],
        25.0,
        12.4,
        Draw::Jpeg,
    ));
    t6_exports("stills among video and other stills in export frames", cases);
}

/// Live behaviours on a moving still in export frames.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_with_live_behaviours() {
    use kerf_core::Behavior as B;
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1920, 1280);
        let assets = std::slice::from_ref(&photo);
        for (name, behaviors) in [
            (
                "breathe",
                vec![B::Breathe {
                    amount: 0.06,
                    speed: 0.7,
                    phase: 0.1,
                }],
            ),
            (
                "shake",
                vec![B::Shake {
                    amount: 0.01,
                    speed: 3.0,
                    seed: 7,
                    phase: 0.0,
                }],
            ),
            (
                "drift",
                vec![B::Drift {
                    speed: 0.02,
                    angle: 30.0,
                    phase: 0.0,
                }],
            ),
        ] {
            let mut c = t6_move(&photo, 0.0, 2.0, &[(0.0, 1.3, 0.02, 0.0), (2.0, 1.2, -0.02, 0.0)]);
            c.behaviors = behaviors;
            cases.push(t6_export(
                &format!("checker/t6/{name} {kind}"),
                timeline(vec![vec![c]], cover(480, 270)),
                assets,
                25.0,
                2.0,
                if name == "drift" { Draw::Any } else { t6_draw(kind) },
            ));
        }
        let mut c = clip(&photo, 0.0, 2.0, 0.0);
        c.transform.scale = 1.3;
        c.behaviors = vec![B::Shake {
            amount: 0.02,
            speed: 2.0,
            seed: 3,
            phase: 0.0,
        }];
        cases.push(t6_export(
            &format!("checker/t6/shake alone {kind}"),
            timeline(vec![vec![c]], cover(480, 270)),
            assets,
            25.0,
            2.0,
            Draw::Any,
        ));
    }
    t6_exports("live behaviours in export frames", cases);
}

/// The delivery's scaler, named.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_with_every_scaler() {
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1501, 1003);
        for scaler in ["bicubic", "bilinear", "lanczos", "neighbor", "spline"] {
            for (route, c) in [
                ("inside", t6_in(&photo, 0.0, 1.5)),
                ("off the edge", t6_off(&photo, 0.0, 1.5)),
            ] {
                cases.push(
                    t6_export(
                        &format!("checker/t6/scaler {scaler} {kind} {route}"),
                        timeline(vec![vec![c]], cover(320, 180)),
                        std::slice::from_ref(&photo),
                        10.0,
                        1.5,
                        Draw::Any,
                    )
                    .with_opts(ExportOptions {
                        scaler: Some(scaler.to_string()),
                        fps: Some(10.0),
                        ..ExportOptions::default()
                    }),
                );
            }
        }
    }
    t6_exports("scalers in export frames", cases);
}

/// Export frames at a size other than the delivery's are the plan's to refuse (the copy is made for the delivery's frame), whatever the
/// route and the kind, and the delivery's own is not.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_frames_at_another_size_are_refused() {
    let mut fails = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1920, 1280);
        for (route, c) in [
            ("inside", t6_in(&photo, 0.0, 2.0)),
            ("off the edge", t6_off(&photo, 0.0, 2.0)),
        ] {
            let tl = timeline(vec![vec![c]], cover(640, 360));
            let planner = Planner::new(
                &tl,
                std::slice::from_ref(&photo),
                &at_fps(10.0),
                PlanRequest::motion(policy()),
            )
            .expect("planner");
            for k in [0u64, 7, 19] {
                let plan = planner.at_frame(k).expect("plan");
                let at = |size: (u32, u32)| -> Vec<String> {
                    plan.reasons(&compositor().caps(), size)
                        .iter()
                        .map(ToString::to_string)
                        .collect()
                };
                let delivery_ok = at((640, 360)).is_empty();
                if !delivery_ok && !(kind == "jpeg" && policy() != CompositeColorPolicy::FixedBt601) {
                    fails.push(format!(
                        "{kind} {route} frame {k}: refused at the delivery's frame: {:?}",
                        at((640, 360))
                    ));
                }
                for size in [
                    (320, 180),
                    (638, 358),
                    (642, 362),
                    (640, 358),
                    (1280, 720),
                    (2, 2),
                    (640, 360 + 2),
                ] {
                    if at(size).is_empty() {
                        fails.push(format!("{kind} {route} frame {k}: drawn at {size:?}"));
                    }
                }
            }
        }
    }
    checker_all("export frames at another size", fails);
}

/// The zoom at which a `Contain` frame's pillars are exactly covered, and either side of it, in export frames; and frames of every size.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_at_the_boundary_of_what_covers_and_in_odd_frames() {
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1600, 1200);
        let exact = 4.0 / 3.0;
        for (name, s) in [
            ("exactly", exact),
            ("1e-9 over", exact + 1e-9),
            ("1e-9 under", exact - 1e-9),
            ("1e-7 under", exact - 1e-7),
            ("1e-6 under", exact - 1e-6),
            ("1e-5 under", exact - 1e-5),
            ("1e-3 over", exact + 1e-3),
        ] {
            let c = t6_move(&photo, 0.0, 1.5, &[(0.0, s, 0.0, 0.0), (1.5, s * 1.2, 0.0, 0.0)]);
            cases.push(t6_export(
                &format!("checker/t6/contain {name} {kind}"),
                timeline(vec![vec![c]], Some(Delivery::new(320, 180, Fit::Contain))),
                std::slice::from_ref(&photo),
                10.0,
                1.5,
                Draw::Any,
            ));
        }
        let photo = t6_pic(kind, 1501, 1003);
        for (w, h) in [(2, 2), (4, 4), (6, 2), (2, 6), (16, 16), (30, 30), (642, 362), (1280, 720)] {
            cases.push(t6_export(
                &format!("checker/t6/frame {w}x{h} {kind}"),
                timeline(vec![vec![t6_in(&photo, 0.0, 1.0)]], cover(w, h)),
                std::slice::from_ref(&photo),
                10.0,
                1.0,
                Draw::Any,
            ));
        }
    }
    t6_exports("the boundary of what covers, and odd frames, in export frames", cases);
}

/// A slideshow whose copies do not all fit in the process's cache at once (a 4K canvas's copy is 14 MB, the cache keeps 96): photographs
/// come round again after others were made, and the copy of one that went is made again, exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); a few minutes"]
fn checker_t6_export_of_a_slideshow_that_outgrows_the_copy_cache() {
    let photos: Vec<Asset> = [(4000, 3000), (3000, 4000), (4096, 2304), (3500, 3500), (4200, 2800)]
        .into_iter()
        .map(|(w, h)| t6_pic("png", w, h))
        .collect();
    let mut clips = Vec::new();
    for i in 0..12 {
        let photo = &photos[i % photos.len()];
        let secs = 1.0;
        let zoom = 1.15 + 0.02 * (i / photos.len()) as f64 + 0.01 * i as f64;
        let mut c = t6_move(
            photo,
            i as f64 * 0.8,
            secs,
            &[(0.0, zoom, 0.02, 0.01), (secs, 1.05, -0.01, 0.0)],
        );
        if i > 0 {
            c.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 0.2,
            });
        }
        clips.push(c);
    }
    let end = 11.0 * 0.8 + 1.0;
    let tl = timeline(vec![clips], cover(3840, 2160));
    t6_exports(
        "a slideshow that outgrows the copy cache",
        vec![t6_export(
            "checker/t6/twelve 4K photographs at 3840x2160",
            tl,
            &photos,
            3.0,
            end,
            Draw::Always,
        )],
    );
}

/// Clips of a frame or two, of a length that is not a multiple of the frame time, and dissolves as short as a frame.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_of_clips_a_frame_or_two_long() {
    let mut cases = Vec::new();
    for kind in ["jpeg", "png"] {
        let photo = t6_pic(kind, 1501, 1003);
        let other = t6_pic("png", 1280, 720);
        for fps in [30.0, 23.976, 29.97, 25.0] {
            for secs in [0.034, 0.05, 0.0667, 0.1, 0.1334, 0.37, 1.0 / 3.0, 1.337] {
                let c = t6_move(&photo, 0.0, secs, &[(0.0, 1.3, 0.04, 0.03), (secs, 1.0, 0.0, 0.0)]);
                let n = (secs * fps).floor().max(1.0) as u64;
                cases.push(
                    t6_export(
                        &format!("checker/t6/{secs} s at {fps} fps {kind}"),
                        timeline(vec![vec![c]], cover(320, 180)),
                        std::slice::from_ref(&photo),
                        fps,
                        secs,
                        t6_draw(kind),
                    )
                    .frames(0..n.min(((secs * fps).ceil() as u64).max(1))),
                );
            }
        }
        // Four short clips back to back, each dissolving into the next over a frame or three.
        let mut clips = Vec::new();
        for (i, p) in [&photo, &other, &photo, &other].into_iter().enumerate() {
            let mut c = t6_move(p, i as f64 * 0.2, 0.3, &[(0.0, 1.3, 0.04, 0.03), (0.3, 1.0, 0.0, 0.0)]);
            if i > 0 {
                c.transition_in = Some(Transition {
                    kind: TransitionKind::Crossfade,
                    duration: 0.0334 * i as f64,
                });
            }
            clips.push(c);
        }
        cases.push(t6_export(
            &format!("checker/t6/four short clips, short dissolves {kind}"),
            timeline(vec![clips], cover(320, 180)),
            &[photo.clone(), other.clone()],
            30.0,
            0.9,
            t6_draw(kind),
        ));
    }
    t6_exports("clips a frame or two long", cases);
}

type LayerEdit = Box<dyn Fn(&mut Clip)>;

/// The refusals of a moving still in export frames, drawn anyway: the kinds the route is not measured for (told to the plan as a kind it
/// takes), a JPEG where the composite's range is negotiated (9.0.2: `FullRangeCut`), and the layers it refuses. One line a case. Judges
/// nothing. The plan-level ones need `KERF_CHECKER_ANYWAY` and the scratch patch that drops them.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); the plan-level refusals need KERF_CHECKER_ANYWAY and the scratch patch"]
fn checker_t6_export_refusals_drawn_anyway() {
    let patched = std::env::var_os("KERF_CHECKER_ANYWAY").is_some();
    let m = media();
    let (jpg, png, portrait) = (
        t6_pic("jpeg", 1920, 1280),
        t6_pic("png", 1920, 1080),
        t6_pic("jpeg", 1280, 1920),
    );
    let frame = || cover(640, 360);
    let into = |c: &mut Clip, d: f64| {
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: d,
        });
    };
    let show = |name: &str, tl: &Timeline, assets: &[Asset], told: &[Asset], fps: f64, secs: f64| {
        let seen = checker_motion_as(&format!("anyway/{name}"), tl, assets, told, fps, upto(secs, fps), true);
        let drawn_off: std::collections::BTreeSet<u64> = seen.off.iter().map(|o| o.0).collect();
        let verdict = if seen.drawn == 0 {
            "still refused".to_string()
        } else if seen.off.is_empty() {
            "EXACT (the refusal is wider than it has to be)".to_string()
        } else {
            let o = &seen.off[0];
            format!(
                "not exact: {} of {} drawn frames differ; first frame {} plane {}: {} samples, max {}, first {:?}",
                drawn_off.len(),
                seen.drawn,
                o.0,
                o.1,
                o.2,
                o.3,
                o.4
            )
        };
        eprintln!(
            "T6 ANYWAY export {name:<58} {verdict}{}",
            seen.refused
                .first()
                .map(|r| format!(" [{}]", r.1.chars().take(110).collect::<String>()))
                .unwrap_or_default()
        );
    };
    // A cut of JPEGs alone: a slideshow with its fades, a single photograph, a photograph over video, and the mix with PNGs.
    let mut clips = Vec::new();
    for i in 0..4 {
        let photo = if i % 2 == 0 { &jpg } else { &portrait };
        let mut c = if i % 2 == 0 {
            t6_in(photo, i as f64 * 0.8, 1.0)
        } else {
            t6_off(photo, i as f64 * 0.8, 1.0)
        };
        if i > 0 {
            into(&mut c, 0.3);
        }
        clips.push(c);
    }
    clips[0].fade_in = 0.3;
    clips[3].fade_out = 0.3;
    show(
        "jpeg slideshow (jpegs alone, fades)",
        &timeline(vec![clips.clone()], frame()),
        &[jpg.clone(), portrait.clone()],
        &[jpg.clone(), portrait],
        25.0,
        3.4,
    );
    show(
        "one jpeg",
        &timeline(vec![vec![t6_in(&jpg, 0.0, 1.5)]], frame()),
        std::slice::from_ref(&jpg),
        std::slice::from_ref(&jpg),
        25.0,
        1.5,
    );
    let mut faded = t6_in(&jpg, 0.0, 1.5);
    faded.fade_in = 0.5;
    show(
        "one jpeg fading in",
        &timeline(vec![vec![faded]], frame()),
        std::slice::from_ref(&jpg),
        std::slice::from_ref(&jpg),
        25.0,
        1.5,
    );
    let mut gap = t6_in(&jpg, 0.5, 1.0);
    gap.fade_out = 0.3;
    show(
        "one jpeg with a gap before it",
        &timeline(vec![vec![gap]], frame()),
        std::slice::from_ref(&jpg),
        std::slice::from_ref(&jpg),
        25.0,
        1.5,
    );
    show(
        "jpeg over video",
        &timeline(
            vec![vec![clip(&m.testsrc, 0.0, 1.5, 0.0)], vec![t6_in(&jpg, 0.0, 1.5)]],
            frame(),
        ),
        &[m.testsrc.clone(), jpg.clone()],
        &[m.testsrc.clone(), jpg.clone()],
        25.0,
        1.5,
    );
    let mut mixed = vec![t6_in(&jpg, 0.0, 1.5), t6_in(&png, 1.2, 1.5)];
    into(&mut mixed[1], 0.4);
    show(
        "jpeg into png",
        &timeline(vec![mixed], frame()),
        &[jpg.clone(), png.clone()],
        &[jpg.clone(), png.clone()],
        25.0,
        2.7,
    );
    let mut mixed = vec![t6_in(&png, 0.0, 1.5), t6_in(&jpg, 1.2, 1.5)];
    into(&mut mixed[1], 0.4);
    show(
        "png into jpeg",
        &timeline(vec![mixed], frame()),
        &[png.clone(), jpg.clone()],
        &[png.clone(), jpg],
        25.0,
        2.7,
    );
    // The kinds the route has no measured chain for (the file is what it says; the plan is told it is a JPEG or a PNG).
    let told = |a: &Asset, fmt: &str| {
        let mut t = a.clone();
        for s in t.streams.iter_mut() {
            s.pix_fmt = Some(fmt.to_string());
        }
        t
    };
    for (name, file, pix, as_fmt) in [
        ("4:2:2 jpeg", "m-422.jpg", "yuvj422p", "yuvj420p"),
        ("4:4:4 jpeg", "m-444.jpg", "yuvj444p", "yuvj420p"),
        ("rgba png (opaque)", "m-rgba.png", "rgba", "rgb24"),
        ("gray png", "m-gray.png", "gray", "rgb24"),
        ("16-bit png", "m-rgb48.png", "rgb48be", "rgb24"),
        ("palette png", "m-pal8.png", "pal8", "rgb24"),
        ("webp (yuv420p)", "m-420.webp", "yuv420p", "rgb24"),
    ] {
        let args: Vec<&str> = if file.ends_with(".jpg") {
            vec!["-q:v", "3", "-pix_fmt", pix]
        } else if file.ends_with(".webp") {
            vec!["-pix_fmt", pix, "-c:v", "libwebp", "-q:v", "90"]
        } else {
            vec!["-pix_fmt", pix]
        };
        let photo = t6_files::photo(file, &t6_files::content(1280, 853), &args);
        let lied = told(&photo, as_fmt);
        for (route, c) in [
            ("inside", t6_in(&photo, 0.0, 1.5)),
            ("off the edge", t6_off(&photo, 0.0, 1.5)),
        ] {
            show(
                &format!("{name}, {route}"),
                &timeline(vec![vec![c]], frame()),
                std::slice::from_ref(&photo),
                std::slice::from_ref(&lied),
                25.0,
                1.5,
            );
        }
    }
    if patched {
        // The layers the route refuses, on a PNG.
        let layers: Vec<(&str, LayerEdit)> = vec![
            (
                "colour",
                Box::new(|c: &mut Clip| {
                    c.color = Color {
                        contrast: 1.2,
                        ..Color::default()
                    }
                }),
            ),
            (
                "translucent",
                Box::new(|c: &mut Clip| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.6)),
            ),
            (
                "grayscale",
                Box::new(|c: &mut Clip| c.effects = vec![kerf_core::VideoEffect::Grayscale]),
            ),
            (
                "blur",
                Box::new(|c: &mut Clip| c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 3.0 }]),
            ),
            (
                "mask",
                Box::new(|c: &mut Clip| c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.2, false))),
            ),
            (
                "rotation",
                Box::new(|c: &mut Clip| c.keyframes.iter_mut().for_each(|k| k.rotation = 4.0)),
            ),
            (
                "keyed crop",
                Box::new(|c: &mut Clip| {
                    c.set_property_keys(
                        kerf_core::Property::CropZoom,
                        vec![kerf_core::PropertyKey::new(0.0, 1.0), kerf_core::PropertyKey::new(1.5, 1.3)],
                    );
                }),
            ),
        ];
        for (name, edit) in &layers {
            for (route, mut c) in [("inside", t6_in(&png, 0.0, 1.5)), ("off the edge", t6_off(&png, 0.0, 1.5))] {
                edit(&mut c);
                show(
                    &format!("png with {name}, {route}"),
                    &timeline(vec![vec![c]], frame()),
                    std::slice::from_ref(&png),
                    std::slice::from_ref(&png),
                    25.0,
                    1.5,
                );
            }
        }
        let mut inc = t6_in(&png, 1.2, 1.5);
        inc.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 0.6,
        });
        show(
            "png dip to white",
            &timeline(vec![vec![t6_in(&png, 0.0, 1.5), inc]], frame()),
            std::slice::from_ref(&png),
            std::slice::from_ref(&png),
            25.0,
            2.7,
        );
    }
}

/// A file replaced since it was probed, in export frames: the asset says what the file was, the file is a larger picture now. The graph's
/// `scale` takes the whole of it; a fit picture made for the compositor opens with a `crop` of the probed size, which cuts the top left.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_of_a_file_replaced_by_a_larger_picture_since_it_was_probed() {
    let mut cases = Vec::new();
    let dir = t6_files::dir();
    for kind in ["jpeg", "png"] {
        let (ext, args): (&str, &[&str]) = if kind == "jpeg" {
            ("jpg", &["-q:v", "3", "-pix_fmt", "yuvj420p"])
        } else {
            ("png", &["-pix_fmt", "rgb24"])
        };
        for (name, (w, h)) in [
            ("one row more", (1920, 1281)),
            ("twice the size", (3840, 2560)),
            ("same size", (1920, 1280)),
        ] {
            let file = format!("stale-export-{kind}-{}.{ext}", name.replace(' ', "-"));
            let probed_as = t6_files::photo(&file, &t6_files::t2(1920, 1280), args);
            let path = dir.join(&file);
            let grain = t6_files::grain(w, h);
            let mut make = vec!["-f", "lavfi", "-i", grain.as_str(), "-frames:v", "1"];
            make.extend_from_slice(args);
            make.push(path.to_str().unwrap());
            ffmpeg(&make);
            for (route, c) in [
                ("inside", t6_in(&probed_as, 0.0, 1.0)),
                ("off the edge", t6_off(&probed_as, 0.0, 1.0)),
            ] {
                cases.push(t6_export(
                    &format!("checker/t6/{kind} replaced: {name}, {route}"),
                    timeline(vec![vec![c]], cover(320, 180)),
                    std::slice::from_ref(&probed_as),
                    10.0,
                    1.0,
                    Draw::Any,
                ));
            }
        }
    }
    t6_exports("a file replaced since it was probed, in export frames", cases);
}

/// A moving still beside footage that names a matrix, in export frames: the export labels untagged HD (a 1920x1080 photograph is one)
/// BT.709 for the whole graph that holds such a clip, ahead of the picture's chain; the copy made for the compositor has to read under the same one.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_in_a_cut_that_names_a_matrix() {
    let c = checker_media();
    let h = hd_pair();
    let mut cases = Vec::new();
    for (kind, w, ph) in [
        ("png", 1920, 1080),
        ("jpeg", 1920, 1080),
        ("png", 1280, 720),
        ("jpeg", 1280, 720),
        ("png", 640, 360),
    ] {
        let photo = t6_pic(kind, w, ph);
        for (name, video, (fw, fh)) in [
            ("bt709 512x256", &c.testsrc709, (512, 256)),
            ("bt2020 512x256", &c.testsrc2020, (512, 256)),
            ("untagged 512x256", &c.testsrc, (512, 256)),
            ("bt709 HD", &h.phone, (1280, 720)),
            ("untagged HD", &h.untagged, (1280, 720)),
        ] {
            let assets = [photo.clone(), video.clone()];
            let mut inc = t6_in(&photo, 1.0, 1.5);
            inc.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 0.5,
            });
            cases.push(t6_export(
                &format!("checker/t6/{kind} {w}x{ph} dissolving in after {name} video"),
                timeline(vec![vec![clip(video, 0.0, 1.5, 0.0), inc]], cover(fw, fh)),
                &assets,
                8.0,
                2.5,
                Draw::Any,
            ));
            cases.push(t6_export(
                &format!("checker/t6/{kind} {w}x{ph} over {name} video"),
                timeline(
                    vec![vec![clip(video, 0.0, 1.5, 0.0)], vec![t6_in(&photo, 0.0, 1.5)]],
                    cover(fw, fh),
                ),
                &assets,
                8.0,
                1.5,
                Draw::Any,
            ));
            cases.push(t6_export(
                &format!("checker/t6/{kind} {w}x{ph} pan-off beside {name} video"),
                timeline(
                    vec![vec![clip(video, 0.0, 1.5, 0.0), t6_off(&photo, 1.0, 1.5)]],
                    cover(fw, fh),
                ),
                &assets,
                8.0,
                2.5,
                Draw::Any,
            ));
        }
    }
    t6_exports("a cut that names a matrix, in export frames", cases);
}

/// Files of every make in export frames: the EXIF orientations, the colour chunks, the layouts FFmpeg decodes to another pixel format.
#[test]
#[ignore = "needs ffmpeg, python3 with Pillow and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_of_files_of_every_make() {
    let mut cases = Vec::new();
    let mut variants = t6_files::jpeg_variants();
    variants.extend(t6_files::png_variants());
    variants.extend(t6_files::other_formats());
    for (name, photo) in variants {
        // (A photograph the probe saw upright and the decode turns is refused when the fit picture is made, in the render and not in the
        // plan: the harness takes a render error for a failure, so those are the Still suite's.)
        let turned = (name.starts_with("EXIF orientation") && !name.ends_with(['1', '2', '3', '4']))
            || (name.starts_with("eXIf orientation") && !name.ends_with('2'));
        for (route, c) in [
            ("inside", t6_in(&photo, 0.0, 1.0)),
            ("off the edge", t6_off(&photo, 0.0, 1.0)),
        ] {
            if turned && route == "off the edge" {
                continue;
            }
            cases.push(t6_export(
                &format!("checker/t6/file {name}: {route}"),
                timeline(vec![vec![c]], cover(320, 180)),
                std::slice::from_ref(&photo),
                10.0,
                1.0,
                Draw::Any,
            ));
        }
    }
    t6_exports("files of every make in export frames", cases);
}

/// The control of `checker_t6_export_in_a_cut_that_names_a_matrix`: a still that does not move (the path this package did not touch),
/// dissolving in after footage that names a matrix. Frames past the footage have no layer that names one, and FFmpeg 9 still converts the
/// picture's RGB with the matrix the cut's graph negotiates.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_t6_export_of_a_static_png_after_footage_that_names_a_matrix_is_the_control() {
    let c = checker_media();
    let mut cases = Vec::new();
    for (name, video) in [("bt709", &c.testsrc709), ("bt2020", &c.testsrc2020), ("untagged", &c.testsrc)] {
        for (w, h) in [(640, 360), (512, 256)] {
            let png = t6_pic("png", w, h);
            let mut still = clip(&png, 0.0, 1.5, 1.0);
            still.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 0.5,
            });
            cases.push(t6_export(
                &format!("checker/t6/control: a static png {w}x{h} dissolving in after {name} video"),
                timeline(vec![vec![clip(video, 0.0, 1.5, 0.0), still]], cover(512, 256)),
                &[png.clone(), video.clone()],
                8.0,
                2.5,
                Draw::Any,
            ));
        }
    }
    t6_exports("a static png after footage that names a matrix, in export frames", cases);
}

// ==== checker/e/m: lane A's placement beside lane E's turns, blends and probe (the merged tree) ==============================
//
// The independent check of the merge of the placement lane into the effects lane. Every case holds the frames the plan lets through to the
// export's planes to the sample (no bound) and the frames it refuses to a reason; the refused frames are drawn anyway where the compositor
// will (its own guards stand) to price the refusal (`WIDER` is a refusal that was not needed). Nothing above this line is edited.

/// The angle in degrees a clip stores for `quarters` quarter turns and `off` radians: its `to_radians()` is that angle to within an ulp.
fn m_degrees(quarters: i32, off: f64) -> f64 {
    (f64::from(quarters) * std::f64::consts::FRAC_PI_2 + off).to_degrees()
}

/// The filter's own answer for an angle `off` radians off `quarters` quarter turns, where only its `simple_rotate` fast path decides: it takes
/// `fabs(angle - target) < FLT_EPSILON` for 0, π/2, π and 3π/2 and for no other target.
fn m_fast_path(quarters: i32, off: f64) -> Option<bool> {
    (0..4).contains(&quarters).then(|| off.abs() < f64::from(f32::EPSILON))
}

/// The reasons `plan.reasons` gives for every frame of `frames` of the cut: each one's kind and its text.
fn m_reasons(
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    frames: Range<u64>,
) -> Vec<(u64, Vec<(&'static str, String)>)> {
    let planner = Planner::new(tl, assets, opts, PlanRequest::motion(policy())).expect("planner");
    frames
        .map(|k| {
            let plan = planner.at_frame(k).expect("plan");
            let size = (plan.canvas.width, plan.canvas.height);
            (
                k,
                plan.reasons(&compositor().caps(), size)
                    .iter()
                    .map(|u| (u.kind(), u.to_string()))
                    .collect(),
            )
        })
        .collect()
}

/// A turn `off` radians off a whole number of quarters, on a picture of even sides, one with an odd width moved off centre, and the same
/// two blended: the window of the plan's `quarter_turn` is the filter's own (`FLT_EPSILON`, 1.19e-7 radians), so the angles just inside it are
/// drawn on every frame to the sample (plain and blended, whichever of the fast path or the integer path the filter takes) and those just
/// outside are FFmpeg's. The plan's answer is held to the filter's rule independently for the angles its fast path alone decides (0, 90, 180
/// and 270 degrees and a hair), and to the export for every one it draws.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_turn_at_the_edges_of_the_filters_window_is_drawn_exactly_or_refused() {
    use kerf_core::BlendMode;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let offs = [0.0, 1.0e-7, -1.0e-7, 1.19e-7, -1.19e-7, 1.2e-7, -1.2e-7, 1.3e-7, -1.3e-7];
    let (mut inside, mut outside) = (0, 0);
    for q in [0, 1, 2, 3, -1, 4, 5, 40, 41] {
        let offs: &[f64] = if (0..4).contains(&q) {
            &offs
        } else {
            &[0.0, 1.0e-7, -1.19e-7, 1.2e-7]
        };
        for &off in offs {
            let degrees = m_degrees(q, off);
            let plan_says = kerf_core::quarter_turn(degrees.to_radians()).is_some();
            let draws = match m_fast_path(q, off) {
                Some(rule) => {
                    if rule != plan_says {
                        failures.push(format!(
                            "{q} quarters {off:+e} rad: the filter's fast path says {rule}, the plan {plan_says}"
                        ));
                    }
                    if rule {
                        inside += 1;
                    } else {
                        outside += 1;
                    }
                    Some(rule)
                }
                None => Some(plan_says),
            };
            for (what, crop, at, mode) in [
                ("plain", 0.0, (0.0, 0.0), None),
                ("odd and moved", 1.0 / 640.0, (0.2, 0.1), None),
                ("blended odd and moved", 1.0 / 640.0, (-0.2, 0.15), Some(BlendMode::Multiply)),
            ] {
                let mut c = clip(&m.testsrc, 0.2, 0.5, 0.0);
                c.transform.rotation = degrees;
                c.transform.crop_right = crop;
                c.transform.pos_x = at.0;
                c.transform.pos_y = at.1;
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                let tl = timeline(vec![vec![clip(&m.bars, 0.2, 0.5, 0.0)], vec![c]], None);
                m_case_at(
                    30.0,
                    &format!("checker/e/m/turn {q} quarters {off:+e} rad {what}"),
                    &tl,
                    &assets,
                    0..4,
                    draws,
                    &mut failures,
                    &mut wider,
                    &mut anyway,
                );
            }
        }
    }
    eprintln!("{inside} angles inside the fast path's window, {outside} outside it (each on three pictures)");
    assert!(inside == 20 && outside == 16, "{inside} inside, {outside} outside");
    r4_finish(
        "checker_e_m_a_turn_at_the_edges_of_the_filters_window",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// A turned picture of another shape, depth or label than the plain 8-bit 16:9 pictures, beside a bottom layer of its own colour tag: a
/// 480x360 pillarbox and a 203x360 / 640x203 odd-sided picture (the bars the pad leaves turn with it), 10-bit (with a range tag, without, and
/// full range), `yuvj420p`, gray, 4:4:4 and 4:2:2, BT.709, NTSC and PAL labels in either order, and a 720p BT.709 clip over an untagged one; at
/// the quarters and at angles just inside the filter's window (a fast-path hair, and the integer path's), plain and blended, centred and moved. A
/// layer the compositor's scaler resizes is lane B's and is left out (as `checker_e_r4_*` leaves it out); the rest is drawn exactly or refused,
/// on this build's colour policy (`BottomLayerTag` on 9.0.2, `FixedBt601` on 6.1.1).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_turned_picture_of_another_shape_depth_or_label_is_the_exports_or_refused() {
    use kerf_core::BlendMode;
    let m = media();
    let v = variants();
    let s = shapes();
    let h = hd_pair();
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    // (name, bottom, top, the plain turned layer is drawn on every frame)
    let pairs: Vec<(&str, &Asset, &Asset, Option<bool>)> = vec![
        ("pillar 480x360", &m.bars, &s.pillar, Some(true)),
        ("odd width 203x360", &m.bars, &s.odd_width, Some(true)),
        ("odd height 640x203", &m.bars, &s.odd_height, None),
        ("10-bit tv", &m.bars, &s.tenbit_tv, None),
        ("10-bit tv over 10-bit tv", &s.tenbit_tv, &s.tenbit_tv, None),
        ("10-bit", &m.bars, &v.tenbit, None),
        ("10-bit pc", &m.bars, &v.tenbit_pc, None),
        ("yuvj420p", &m.bars, &v.mjpeg, None),
        ("gray", &m.bars, &v.gray, None),
        ("4:4:4", &m.bars, &v.yuv444, None),
        ("709 over 709", &v.bars709, &v.testsrc709, Some(true)),
        ("709 over untagged", &m.bars, &v.testsrc709, None),
        ("untagged over 709", &v.bars709, &m.testsrc, None),
        ("170m over 470bg", &v.bars470bg, &v.testsrc170m, None),
        ("noise over 709", &v.bars709, &m.noise, None),
        ("720p 709 over untagged", &h.untagged, &h.phone, None),
        ("720p untagged over 709", &h.phone, &h.untagged, None),
    ];
    let angles = [90.0, m_degrees(1, 1.0e-7), m_degrees(2, -1.19e-7), m_degrees(4, 1.1e-7)];
    for (name, bottom, top, drawn) in pairs {
        for angle in angles {
            for (mode, at) in [
                (None, (0.0, 0.0)),
                (None, (0.2, 0.1)),
                (Some(BlendMode::Multiply), (-0.15, 0.2)),
            ] {
                let mut c = clip(top, 0.1, 0.4, 0.0);
                c.transform.rotation = angle;
                c.transform.pos_x = at.0;
                c.transform.pos_y = at.1;
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                let mut assets = vec![bottom.clone()];
                if top.id != bottom.id {
                    assets.push(top.clone());
                }
                let tl = timeline(vec![vec![clip(bottom, 0.1, 0.4, 0.0)], vec![c]], None);
                let quarter = kerf_core::quarter_turn(angle.to_radians()).is_some();
                let scaled = r3_resized(&tl, &assets, 30.0);
                if scaled && mode.is_none() {
                    continue;
                }
                let draws = if !quarter || (scaled && mode.is_some()) {
                    Some(false)
                } else if mode.is_none() {
                    drawn
                } else {
                    None
                };
                m_case_at(
                    30.0,
                    &format!("checker/e/m/shape {name} turned {angle:.9} {mode:?} at {at:?}"),
                    &tl,
                    &assets,
                    0..3,
                    draws,
                    &mut failures,
                    &mut wider,
                    &mut anyway,
                );
            }
        }
    }
    r4_finish(
        "checker_e_m_a_turned_picture_of_another_shape_depth_or_label",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// A blended clip over a placement. A pose that moves on the blended clip itself (a moving zoom or position, a live breath, drift or shake, a
/// slide's travel added to a keyed position) is `BlendMode: "a moving zoom or position"` on every frame of the clip, nothing of it is drawn,
/// and what the compositor would do with it anyway is its own guard (`look.placed`), which is counted. A position or zoom that holds one value
/// is no pose at all (the clip's graph is a static one) and a static position is the plain moved layer: drawn to the sample. And a placement
/// *under* the blend (the bottom layer or a layer between) is a canvas like another: drawn exactly or refused whatever it is.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_blend_over_a_held_or_moving_placement_is_exact_or_refused() {
    use kerf_core::{Behavior as B, BlendMode, Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone(), m.noise.clone()];
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let opts = at_fps(30.0);
    let keys = |a: f64, b: f64| vec![PropertyKey::new(0.0, a), PropertyKey::new(0.5, b)];
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    // (name, edit, the pose moves: every frame of the clip is refused for it, drawn: every frame is drawn, either: None)
    let cases: Vec<(&str, Edit<'_>, Option<bool>, bool)> = vec![
        (
            "moving zoom",
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 1.5, 0.0, 0.0)]),
            Some(false),
            true,
        ),
        (
            "zoom out",
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 0.6, 0.0, 0.0)]),
            Some(false),
            true,
        ),
        (
            "moving position",
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, -0.1, 0.0), zoom_key(0.5, 1.0, 0.1, 0.05)]),
            Some(false),
            true,
        ),
        (
            "moving x alone",
            Box::new(|c| c.set_property_keys(Property::PosX, keys(0.0, 0.2))),
            Some(false),
            true,
        ),
        (
            "moving y alone",
            Box::new(|c| c.set_property_keys(Property::PosY, keys(0.05, -0.15))),
            Some(false),
            true,
        ),
        (
            "a move of 1e-6",
            Box::new(|c| c.set_property_keys(Property::PosX, keys(0.0, 1.0e-6))),
            Some(false),
            true,
        ),
        (
            "a move of no size",
            Box::new(|c| c.set_property_keys(Property::PosX, keys(0.1, 0.1 + 1.0e-12))),
            None,
            false,
        ),
        (
            "a held zoom and position",
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.1, 0.05), zoom_key(0.5, 1.0, 0.1, 0.05)]),
            None,
            false,
        ),
        (
            "a held position (x key)",
            Box::new(|c| c.set_property_keys(Property::PosX, keys(0.15, 0.15))),
            None,
            false,
        ),
        (
            "a static position",
            Box::new(|c| {
                c.transform.pos_x = 0.13;
                c.transform.pos_y = -0.07;
            }),
            Some(true),
            false,
        ),
        (
            "a static position off the frame",
            Box::new(|c| {
                c.transform.pos_x = 0.6;
                c.transform.pos_y = 0.3;
            }),
            None,
            false,
        ),
        (
            "a breath",
            Box::new(|c| {
                c.behaviors = vec![B::Breathe {
                    amount: 0.15,
                    speed: 1.3,
                    phase: 0.2,
                }];
            }),
            Some(false),
            true,
        ),
        (
            "a drift",
            Box::new(|c| {
                c.behaviors = vec![B::Drift {
                    speed: 0.2,
                    angle: 30.0,
                    phase: 0.1,
                }];
            }),
            Some(false),
            true,
        ),
        (
            "a shake",
            Box::new(|c| {
                c.behaviors = vec![B::Shake {
                    amount: 0.03,
                    speed: 2.5,
                    seed: 7,
                    phase: 0.0,
                }];
            }),
            Some(false),
            true,
        ),
        (
            "a breath of no depth",
            Box::new(|c| {
                c.behaviors = vec![B::Breathe {
                    amount: 0.0,
                    speed: 1.0,
                    phase: 0.0,
                }];
            }),
            None,
            false,
        ),
        (
            "a move that ends before the clip does",
            Box::new(|c| c.set_property_keys(Property::PosX, vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(0.1, 0.2)])),
            Some(false),
            true,
        ),
    ];
    for (name, edit, draws, refused_by_blend) in &cases {
        for mode in [BlendMode::Multiply, BlendMode::Difference] {
            let mut c = clip(&m.testsrc, 0.5, 1.0, 0.0);
            c.blend_mode = mode;
            edit(&mut c);
            let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![c]], None);
            let case = format!("checker/e/m/blend {name} {mode:?}");
            m_case_at(
                30.0,
                &case,
                &tl,
                &assets,
                0..8,
                *draws,
                &mut failures,
                &mut wider,
                &mut anyway,
            );
            if *refused_by_blend {
                // The plan's refusal is not the only one: the compositor refuses a blend over a pose itself (`look.placed`), so the frame
                // asked for with the plan's reasons ignored is not drawn at all, or is drawn exactly.
                // (Without the scratch patch the compositor judges the plan itself and refuses before any guard of its own is reached.)
                if std::env::var_os("KERF_CHECKER_ANYWAY").is_some() {
                    let bypass = ce_scan(&format!("{case} (plan bypassed)"), &tl, &assets, &opts, 0..8, true);
                    eprintln!("BYPASS {}", bypass.line(&case));
                    if bypass.drawn > 0 && !bypass.exact() {
                        failures.push(format!(
                            "{case}: the compositor draws a blend over a pose when the plan is bypassed, and not exactly: {}",
                            bypass.line("bypassed")
                        ));
                    }
                }
                for (k, why) in m_reasons(&tl, &assets, &opts, 0..8) {
                    if !why
                        .iter()
                        .any(|(kind, w)| *kind == "blend_mode" && w.contains("a moving zoom or position"))
                    {
                        failures.push(format!("{case}: frame {k} is not refused by the blend for the pose: {why:?}"));
                    }
                }
            }
        }
    }
    // A blend over noise (every sample of the clip's edge differs) and with an odd width, still, so the held cases are not an accident of the pattern.
    for mode in [BlendMode::Multiply, BlendMode::Exclusion, BlendMode::HardLight] {
        let mut c = clip(&m.noise, 0.5, 1.0, 0.0);
        c.blend_mode = mode;
        c.transform.crop_right = 1.0 / 640.0;
        c.transform.pos_x = 0.07;
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![c]], None);
        m_case_at(
            30.0,
            &format!("checker/e/m/blend noise odd static {mode:?}"),
            &tl,
            &assets,
            0..8,
            Some(true),
            &mut failures,
            &mut wider,
            &mut anyway,
        );
    }
    // A placement under the blend: the bottom layer zooms out (the canvas shows around it) or moves, or a layer between does; the blended clip is
    // static, turned or not, plain or odd.
    for (below, edit) in [
        (
            "bottom zooms out",
            Box::new(|c: &mut Clip| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 0.6, 0.1, 0.0)]) as Edit<'_>,
        ),
        (
            "bottom zooms in",
            Box::new(|c: &mut Clip| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 1.4, 0.0, 0.0)]),
        ),
        (
            "bottom moves",
            Box::new(|c: &mut Clip| c.set_property_keys(Property::PosX, keys(-0.2, 0.2))),
        ),
    ] {
        for (clip_what, turn, crop) in [("static", 0.0, 0.0), ("turned 90", 90.0, 0.0), ("odd", 0.0, 1.0 / 640.0)] {
            for mode in [BlendMode::Multiply, BlendMode::Add] {
                let mut bottom = clip(&m.bars, 0.5, 1.0, 0.0);
                edit(&mut bottom);
                let mut top = clip(&m.testsrc, 0.5, 1.0, 0.0);
                top.blend_mode = mode;
                top.transform.rotation = turn;
                top.transform.crop_right = crop;
                let tl = timeline(vec![vec![bottom], vec![top]], None);
                m_case_at(
                    30.0,
                    &format!("checker/e/m/blend over {below}, {clip_what} {mode:?}"),
                    &tl,
                    &assets,
                    0..8,
                    None,
                    &mut failures,
                    &mut wider,
                    &mut anyway,
                );
            }
        }
    }
    // A slide into a blended clip whose position is held (its travel is the overlay's own, drawn) and into one that moves (a pose with the
    // travel added: refused).
    for (name, kind, moving) in [
        ("slide left, held", TransitionKind::SlideLeft, false),
        ("slide up, held", TransitionKind::SlideUp, false),
        ("push left, held", TransitionKind::PushLeft, false),
        ("slide left, moving", TransitionKind::SlideLeft, true),
        ("push right, moving", TransitionKind::PushRight, true),
    ] {
        let mut first = clip(&m.testsrc, 0.5, 1.0, 0.0);
        first.blend_mode = BlendMode::Screen;
        let mut second = clip(&m.noise, 0.5, 1.0, 0.5);
        second.blend_mode = BlendMode::Multiply;
        second.transition_in = Some(Transition { kind, duration: 0.3 });
        if moving {
            second.set_property_keys(Property::PosX, keys(-0.1, 0.1));
        }
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0)], vec![first, second]], None);
        let case = format!("checker/e/m/blend {name}");
        m_case_at(30.0, &case, &tl, &assets, 0..30, None, &mut failures, &mut wider, &mut anyway);
        if moving {
            let reasons = m_reasons(&tl, &assets, &opts, 15..30);
            if let Some((k, why)) = reasons.iter().find(|(_, why)| {
                !why.iter()
                    .any(|(kind, w)| *kind == "blend_mode" && w.contains("a moving zoom or position"))
            }) {
                failures.push(format!("{case}: frame {k} is not refused by the blend for the pose: {why:?}"));
            }
        }
    }
    for mode in [BlendMode::Multiply, BlendMode::Difference] {
        let mut mid = clip(&m.noise, 0.5, 1.0, 0.0);
        mid.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 0.7, 0.1, 0.1)];
        let mut top = clip(&m.testsrc, 0.5, 1.0, 0.0);
        top.blend_mode = mode;
        top.transform.pos_x = 0.1;
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![mid], vec![top]], None);
        m_case_at(
            30.0,
            &format!("checker/e/m/blend over a middle layer that zooms {mode:?}"),
            &tl,
            &assets,
            0..8,
            None,
            &mut failures,
            &mut wider,
            &mut anyway,
        );
    }
    r4_finish(
        "checker_e_m_a_blend_over_a_held_or_moving_placement",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// A quarter turn beside keyed motion, and the `geq` edge that two lanes shift ahead of different passes (a placement's scale; a turn). On a
/// picture whose every edge sample differs (noise, and the smooth gradient whose only strong edge is the layer's own):
/// * a quarter turn with a position or zoom that moves is refused, whichever key moves it (a placement is not drawn behind a rotation);
/// * a turn beside a position or zoom that holds one value is the static turn, drawn exactly or refused;
/// * a mask or a keyed opacity that holds 1 ahead of a moving zoom or position is drawn exactly on the build whose `geq` reads a plane's last
///   column and row one in (6.1) and on the one that copies them (9.0), and a keyed opacity on a turned layer, at the quarters and a hair
///   inside the window, likewise.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_quarter_turn_beside_keyed_motion_and_the_geq_edge_before_a_placement_or_a_turn() {
    use kerf_core::{Behavior as B, BlendMode, Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone(), m.noise.clone(), m.smooth.clone()];
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let opts = at_fps(30.0);
    let keys = |a: f64, b: f64| vec![PropertyKey::new(0.0, a), PropertyKey::new(0.5, b)];
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    let rotated_keys = |degrees: f64| -> Vec<kerf_core::Keyframe> {
        vec![
            kerf_core::Keyframe {
                rotation: degrees,
                ..zoom_key(0.0, 1.0, 0.0, 0.0)
            },
            kerf_core::Keyframe {
                rotation: degrees,
                ..zoom_key(0.5, 1.3, 0.0, 0.0)
            },
        ]
    };
    let one = m_degrees(1, 1.0e-7);
    // (name, edit, draws, the pose and the turn are both there: the plan names the rotation)
    let cases: Vec<(&str, Edit<'_>, Option<bool>, bool)> = vec![
        (
            "a quarter and a moving x",
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.set_property_keys(Property::PosX, keys(0.0, 0.2));
            }),
            Some(false),
            true,
        ),
        (
            "a quarter and a moving y",
            Box::new(|c| {
                c.transform.rotation = 270.0;
                c.set_property_keys(Property::PosY, keys(0.1, -0.1));
            }),
            Some(false),
            true,
        ),
        (
            "a quarter and a moving scale",
            Box::new(|c| {
                c.transform.rotation = 180.0;
                c.set_property_keys(Property::Scale, keys(1.0, 1.3));
            }),
            Some(false),
            true,
        ),
        (
            "a hair off a quarter and a moving x",
            Box::new(move |c| {
                c.transform.rotation = one;
                c.set_property_keys(Property::PosX, keys(0.0, 0.2));
            }),
            Some(false),
            true,
        ),
        (
            "keys that carry a quarter and move the zoom",
            Box::new(move |c| c.keyframes = rotated_keys(90.0)),
            Some(false),
            true,
        ),
        (
            "keys that carry a hair off a quarter and move the zoom",
            Box::new(move |c| c.keyframes = rotated_keys(one)),
            Some(false),
            true,
        ),
        (
            "a quarter and a drift",
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.behaviors = vec![B::Drift {
                    speed: 0.2,
                    angle: 30.0,
                    phase: 0.0,
                }];
            }),
            Some(false),
            true,
        ),
        (
            "a quarter and a spin",
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.behaviors = vec![B::Spin { speed: 0.3, phase: 0.0 }];
            }),
            Some(false),
            false,
        ),
        (
            "a quarter and a held x",
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.set_property_keys(Property::PosX, keys(0.15, 0.15));
            }),
            None,
            false,
        ),
        (
            "a quarter and a held scale",
            Box::new(|c| {
                c.transform.rotation = 180.0;
                c.set_property_keys(Property::Scale, keys(1.0, 1.0));
            }),
            None,
            false,
        ),
        (
            "a hair inside the window and a held y",
            Box::new(move |c| {
                c.transform.rotation = one;
                c.set_property_keys(Property::PosY, keys(-0.1, -0.1));
            }),
            None,
            false,
        ),
        (
            "a quarter and a keyed opacity that holds 1",
            Box::new(|c| {
                c.transform.rotation = 90.0;
                c.set_property_keys(Property::Opacity, keys(1.0, 1.0));
            }),
            None,
            false,
        ),
        (
            "a quarter and a keyed opacity that falls",
            Box::new(|c| {
                c.transform.rotation = 270.0;
                c.set_property_keys(Property::Opacity, keys(1.0, 0.25));
            }),
            None,
            false,
        ),
        (
            "a hair inside the window and a keyed opacity that falls",
            Box::new(move |c| {
                c.transform.rotation = one;
                c.set_property_keys(Property::Opacity, keys(1.0, 0.4));
            }),
            None,
            false,
        ),
        (
            "a half turn a hair under the window and a keyed opacity that falls",
            Box::new(|c| {
                c.transform.rotation = m_degrees(2, -1.19e-7);
                c.set_property_keys(Property::Opacity, keys(0.9, 0.4));
            }),
            Some(true),
            false,
        ),
        (
            "a moving x and a keyed opacity that holds 1",
            Box::new(|c| {
                c.set_property_keys(Property::PosX, keys(0.0, 0.2));
                c.set_property_keys(Property::Opacity, keys(1.0, 1.0));
            }),
            None,
            false,
        ),
        (
            "a moving zoom and a keyed opacity that holds 1",
            Box::new(|c| {
                c.set_property_keys(Property::Scale, keys(1.0, 1.25));
                c.set_property_keys(Property::Opacity, keys(1.0, 1.0));
            }),
            None,
            false,
        ),
        (
            "a moving x and a mask",
            Box::new(|c| {
                c.set_property_keys(Property::PosX, keys(-0.1, 0.1));
                c.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.0, false));
            }),
            None,
            false,
        ),
        (
            "a moving zoom and a feathered inverted mask",
            Box::new(|c| {
                c.set_property_keys(Property::Scale, keys(1.0, 1.25));
                c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.4, true));
            }),
            None,
            false,
        ),
        (
            "a moving x and the full-frame mask",
            Box::new(|c| {
                c.set_property_keys(Property::PosX, keys(0.0, 0.1));
                c.mask = Some(kerf_core::Mask {
                    shape: kerf_core::MaskShape::Rect,
                    x: 0.5,
                    y: 0.5,
                    width: 1.0,
                    height: 1.0,
                    feather: 0.0,
                    inverted: false,
                });
            }),
            None,
            false,
        ),
    ];
    for (asset_name, asset) in [("noise", &m.noise), ("smooth", &m.smooth)] {
        for (name, edit, draws, names_rotation) in &cases {
            for mode in [None, Some(BlendMode::Multiply)] {
                if asset_name == "smooth" && mode.is_some() {
                    continue;
                }
                let mut c = clip(asset, 0.5, 1.0, 0.0);
                edit(&mut c);
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![c]], None);
                let case = format!("checker/e/m/keyed {asset_name} {name} {mode:?}");
                let scaled = r3_resized(&tl, &assets, 30.0);
                let draws = if mode.is_some() && draws == &Some(true) {
                    None
                } else {
                    *draws
                };
                if scaled && mode.is_none() && draws != Some(false) {
                    continue;
                }
                m_case_at(30.0, &case, &tl, &assets, 0..8, draws, &mut failures, &mut wider, &mut anyway);
                if *names_rotation && mode.is_none() {
                    let reasons = m_reasons(&tl, &assets, &opts, 0..8);
                    if let Some((k, why)) = reasons
                        .iter()
                        .find(|(_, why)| !why.iter().any(|(_, w)| w.to_lowercase().contains("rotation")))
                    {
                        failures.push(format!("{case}: frame {k} is not refused for the rotation: {why:?}"));
                    }
                }
            }
        }
    }
    r4_finish(
        "checker_e_m_a_quarter_turn_beside_keyed_motion",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// The still plan of frame `k`'s instant (`RenderPlan::at`, the plan a scrubbed still is drawn from) against the export's frame `k`, to the
/// sample, for a cut whose pictures hold still around the instant (the dissolve-free cuts here), or the refusal. `Err` carries what is wrong.
fn m_still(case: &str, tl: &Timeline, assets: &[Asset], ks: &[u64], draws: Option<bool>) -> Result<(usize, usize), Vec<String>> {
    let opts = at_fps(30.0);
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let played = export(&slug, tl, assets, &opts);
    let (mut drawn, mut refused, mut failures) = (0, 0, Vec::new());
    for &k in ks {
        let t = k as f64 / 30.0 - 1e-4;
        let plan = kerf_core::RenderPlan::at(tl, assets, &opts, t, policy()).expect("still plan");
        let size = plan.size(u32::MAX);
        let why = plan.reasons(&compositor().caps(), size);
        if !why.is_empty() {
            refused += 1;
            eprintln!("{case} @ {k}: refused: {}", why[0]);
            if draws == Some(true) {
                failures.push(format!("{case} @ {k}: refused: {why:?}"));
            }
            continue;
        }
        if draws == Some(false) {
            failures.push(format!("{case} @ {k}: drawn, a refusal was expected"));
        }
        let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
        let got = match compositor().composite_yuv(&plan, &frames, size) {
            Ok(g) => g,
            Err(e) => {
                failures.push(format!("{case} @ {k}: the compositor's own guard: {e}"));
                continue;
            }
        };
        drawn += 1;
        let want = played.planes(k as usize);
        let got = planes_of(&got);
        let stats: Vec<PlaneStats> = (0..3).map(|p| PlaneStats::of(want[p], got[p])).collect();
        eprintln!(
            "{case} @ {k}: drawn, Y {} U {} V {} differ",
            stats[0].differing, stats[1].differing, stats[2].differing
        );
        if stats.iter().any(|s| s.differing > 0) {
            failures.push(format!(
                "{case} @ {k}: Y {} (max {}) U {} (max {}) V {} (max {}) differ",
                stats[0].differing, stats[0].max, stats[1].differing, stats[1].max, stats[2].differing, stats[2].max
            ));
        }
    }
    if failures.is_empty() {
        Ok((drawn, refused))
    } else {
        Err(failures)
    }
}

/// The still plan at the edges of the filter's window and over a pose: a layer turned `off` radians off a quarter is the export's frame to the
/// sample where the plan draws it (plain and blended) and refused outside the fast path's window; a blended clip over a keyed zoom or position
/// is refused in the still as in the export, and one whose position holds is drawn exactly; and a still of a keyed opacity that holds 1 on a
/// quarter turn of noise is the export's, whichever side of the `geq` edge this build is on.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_still_at_the_windows_edge_and_over_a_pose_is_the_exports_frame_to_the_sample() {
    use kerf_core::{BlendMode, Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.noise.clone()];
    let mut failures: Vec<String> = Vec::new();
    let mut results: Vec<Result<(usize, usize), Vec<String>>> = Vec::new();
    for q in [0, 1, 2, 3, -1] {
        for off in [0.0, 1.0e-7, -1.19e-7, 1.2e-7, -1.3e-7] {
            let degrees = m_degrees(q, off);
            let plan_says = kerf_core::quarter_turn(degrees.to_radians()).is_some();
            let want = m_fast_path(q, off).unwrap_or(plan_says);
            if want != plan_says {
                failures.push(format!(
                    "{q} quarters {off:+e}: the filter's fast path says {want}, the plan {plan_says}"
                ));
            }
            for (what, crop, mode) in [
                ("odd", 1.0 / 640.0, None),
                ("odd blended", 1.0 / 640.0, Some(BlendMode::Multiply)),
            ] {
                let mut c = clip(&m.noise, 0.2, 1.0, 0.0);
                c.transform.rotation = degrees;
                c.transform.crop_right = crop;
                c.transform.pos_x = 0.05;
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
                results.push(m_still(
                    &format!("checker/e/m/still turn {q} quarters {off:+e} {what}"),
                    &tl,
                    &assets,
                    &[2, 9],
                    Some(want),
                ));
            }
        }
    }
    let key = |a: f64, b: f64| vec![PropertyKey::new(0.0, a), PropertyKey::new(1.0, b)];
    for mode in [BlendMode::Multiply, BlendMode::Add, BlendMode::Difference] {
        for (name, moving, draws) in [
            ("moving x", true, Some(false)),
            ("held x", false, Some(true)),
            ("moving zoom", true, Some(false)),
        ] {
            let mut c = clip(&m.noise, 0.2, 1.0, 0.0);
            c.blend_mode = mode;
            if name == "moving zoom" {
                c.set_property_keys(Property::Scale, key(1.0, 1.4));
            } else {
                c.set_property_keys(Property::PosX, key(0.1, if moving { 0.3 } else { 0.1 }));
            }
            let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![c]], None);
            results.push(m_still(
                &format!("checker/e/m/still blend {name} {mode:?}"),
                &tl,
                &assets,
                &[2, 9, 20],
                draws,
            ));
        }
    }
    let mut held = clip(&m.noise, 0.2, 1.0, 0.0);
    held.transform.rotation = 270.0;
    held.set_property_keys(Property::Opacity, key(1.0, 1.0));
    let tl = timeline(vec![vec![clip(&m.bars, 0.2, 1.0, 0.0)], vec![held]], None);
    results.push(m_still(
        "checker/e/m/still keyed opacity 1 and a quarter",
        &tl,
        &assets,
        &[2, 9, 20],
        None,
    ));
    let (mut drawn, mut refused) = (0, 0);
    for r in results {
        match r {
            Ok((d, r)) => {
                drawn += d;
                refused += r;
            }
            Err(f) => failures.extend(f),
        }
    }
    eprintln!("{drawn} still frames drawn and held to the export, {refused} refused");
    assert!(drawn >= 60, "{drawn} frames drawn");
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

/// A keyed colour (the `eq` of each frame) beside what the two lanes added around it: a quarter turn and a hair inside the filter's window,
/// a blend mode, a static position, and a moving position or zoom behind it. Drawn exactly or refused, frame by frame.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_keyed_colour_beside_a_turn_a_blend_and_a_placement() {
    use kerf_core::{BlendMode, Property, PropertyKey};
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone(), m.noise.clone()];
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let keys = |a: f64, b: f64| vec![PropertyKey::new(0.0, a), PropertyKey::new(0.5, b)];
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    let hair = m_degrees(1, 1.1e-7);
    let cases: Vec<(&str, Edit<'_>)> = vec![
        ("nothing else", Box::new(|_| {})),
        ("a quarter", Box::new(|c| c.transform.rotation = 90.0)),
        (
            "a half turn a hair under the window",
            Box::new(|c| c.transform.rotation = m_degrees(2, -1.19e-7)),
        ),
        ("a hair inside the window", Box::new(move |c| c.transform.rotation = hair)),
        ("a static position", Box::new(|c| c.transform.pos_x = 0.12)),
        (
            "a moving x",
            Box::new(move |c| c.set_property_keys(Property::PosX, keys(-0.1, 0.1))),
        ),
        (
            "a moving zoom",
            Box::new(move |c| c.set_property_keys(Property::Scale, keys(1.0, 1.3))),
        ),
    ];
    for (name, edit) in &cases {
        for (props, what) in [
            (&[Property::Brightness][..], "brightness"),
            (&[Property::Saturation, Property::Gamma][..], "saturation and gamma"),
        ] {
            for mode in [None, Some(BlendMode::Screen)] {
                let mut c = colour_keyed(&m.noise, props, &linear_keys);
                c.source_in = 0.5;
                c.source_out = 1.0;
                edit(&mut c);
                if let Some(mode) = mode {
                    c.blend_mode = mode;
                }
                let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![c]], None);
                let scaled = r3_resized(&tl, &assets, 30.0);
                if scaled && mode.is_none() {
                    continue;
                }
                m_case_at(
                    30.0,
                    &format!("checker/e/m/colour {what} with {name} {mode:?}"),
                    &tl,
                    &assets,
                    0..8,
                    None,
                    &mut failures,
                    &mut wider,
                    &mut anyway,
                );
            }
        }
    }
    r4_finish("checker_e_m_a_keyed_colour_beside", failures, &wider, &anyway, true);
}

/// [`r4_case`] at the frame rate `fps`: the verdict against what the plan said, and the refused frames drawn anyway for what the refusal costs.
#[allow(clippy::too_many_arguments)]
fn m_case_at(
    fps: f64,
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    frames: Range<u64>,
    draws: Option<bool>,
    failures: &mut Vec<String>,
    wider: &mut Vec<String>,
    drawn_anyway: &mut Vec<String>,
) {
    let opts = at_fps(fps);
    let scan = ce_scan(case, tl, assets, &opts, frames.clone(), false);
    eprintln!("{}", scan.line(case));
    CE_DRAWN.with(|d| d.set(d.get() + scan.drawn));
    if let Some(why) = scan.verdict(draws) {
        failures.push(format!("{case}: {why}"));
    }
    // (The compositor judges the plan itself, so a frame the plan refuses is only drawn by a compositor built without that judgement: the
    // scratch patch `KERF_CHECKER_ANYWAY` stands for. Without it the draw-anyway is a second export that can only repeat the refusal.)
    if scan.drawn < scan.frames && std::env::var_os("KERF_CHECKER_ANYWAY").is_some() {
        let name = format!("{case} (drawn anyway)");
        let anyway = ce_scan(&name, tl, assets, &opts, frames, true);
        eprintln!("anyway: {}", anyway.line(&name));
        drawn_anyway.push(format!(
            "{case}: anyway drawn {} of {}, guards {}, Y {} UV {} differ",
            anyway.drawn,
            anyway.frames,
            anyway.guards.values().sum::<usize>(),
            anyway.y.differing,
            anyway.uv.differing
        ));
        if anyway.drawn > scan.drawn && anyway.exact() {
            wider.push(format!("{case}: refused and drawn exactly anyway ({} frames)", anyway.drawn));
        }
    }
}

/// Turned pictures in BT.601, BT.709 and BT.2020 (the checker's 512x256 pictures at 32 frames a second, a power-of-two canvas), each over a
/// bottom layer of each tag, at the quarters and a hair inside the window, plain and blended (a BT.2020 composite is a blend extra the probe
/// measures): drawn exactly or refused, on this build's colour policy.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_turned_picture_of_each_matrix_over_a_bottom_layer_of_each_is_exact_or_refused() {
    use kerf_core::BlendMode;
    let c = checker_media();
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let bottoms = [("untagged", &c.bars), ("709", &c.testsrc709), ("2020", &c.testsrc2020)];
    let tops = [
        ("untagged", &c.testsrc),
        ("709", &c.testsrc709),
        ("2020", &c.testsrc2020),
        ("noise", &c.noise),
    ];
    for (bname, bottom) in bottoms {
        for (tname, top) in tops {
            for angle in [90.0, m_degrees(3, -1.19e-7)] {
                for mode in [None, Some(BlendMode::Multiply)] {
                    let mut t = clip(top, 0.25, 0.5, 0.0);
                    t.transform.rotation = angle;
                    t.transform.pos_x = 0.1;
                    if let Some(mode) = mode {
                        t.blend_mode = mode;
                    }
                    let assets = [bottom.clone(), top.clone()];
                    let tl = timeline(vec![vec![clip(bottom, 0.25, 0.5, 0.0)], vec![t]], None);
                    m_case_at(
                        32.0,
                        &format!("checker/e/m/matrix {tname} turned {angle:.9} over {bname} {mode:?}"),
                        &tl,
                        &assets,
                        0..4,
                        None,
                        &mut failures,
                        &mut wider,
                        &mut anyway,
                    );
                }
            }
        }
    }
    r4_finish(
        "checker_e_m_a_turned_picture_of_each_matrix_over_a_bottom_layer_of_each",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// A keyed opacity (a `geq` alpha that reads a plane's last column and row one in on 6.1) on a turned picture that does not fill the frame or
/// has odd sides: the pad's bars are part of the plane the `geq` runs on, so its last column and row, shifted ahead of the turn, are the bars'
/// (or the odd picture's own), not the picture's. A pillarbox and two odd-sided pictures, the quarters and a hair inside the window, holding 1 and
/// falling; then a mask beside a moving zoom on the same pictures.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_m_a_keyed_opacity_on_a_turned_letterboxed_or_odd_picture_shifts_the_plane_it_runs_on() {
    use kerf_core::{Property, PropertyKey};
    let m = media();
    let s = shapes();
    let (mut failures, mut wider, mut anyway) = (Vec::new(), Vec::new(), Vec::new());
    let keys = |a: f64, b: f64| vec![PropertyKey::new(0.0, a), PropertyKey::new(0.5, b)];
    for (name, asset) in [
        ("pillar", &s.pillar),
        ("odd width", &s.odd_width),
        ("odd height", &s.odd_height),
        ("noise", &m.noise),
    ] {
        let assets = [m.bars.clone(), asset.clone()];
        for angle in [90.0, 180.0, 270.0, m_degrees(1, 1.1e-7), m_degrees(2, -1.1e-7)] {
            for (what, from, to) in [("falls", 1.0, 0.3), ("holds 1", 1.0, 1.0)] {
                let mut c = clip(asset, 0.5, 1.0, 0.0);
                c.transform.rotation = angle;
                c.set_property_keys(Property::Opacity, keys(from, to));
                let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![c]], None);
                m_case_at(
                    30.0,
                    &format!("checker/e/m/geq {name} turned {angle:.9} opacity {what}"),
                    &tl,
                    &assets,
                    0..4,
                    None,
                    &mut failures,
                    &mut wider,
                    &mut anyway,
                );
            }
        }
        let mut z = clip(asset, 0.5, 1.0, 0.0);
        z.set_property_keys(Property::Scale, keys(1.0, 1.3));
        z.mask = Some(mask_of(kerf_core::MaskShape::Rect, 0.0, false));
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![z]], None);
        m_case_at(
            30.0,
            &format!("checker/e/m/geq {name} mask and a moving zoom"),
            &tl,
            &assets,
            0..8,
            None,
            &mut failures,
            &mut wider,
            &mut anyway,
        );
    }
    r4_finish(
        "checker_e_m_a_keyed_opacity_on_a_turned_letterboxed_or_odd_picture",
        failures,
        &wider,
        &anyway,
        true,
    );
}

/// The price of the refusal of a blend over a pose (`BlendMode: "a moving zoom or position"`): the frames the plan refuses are drawn anyway and
/// compared with the export. A refusal is *necessary* when the drawn frame is not the export's; one that would have been exact is wider than it
/// has to be. The compositor judges the plan itself (and guards a blend over a placed layer itself), so the draw-anyway needs the scratch patch
/// (`KERF_CHECKER_ANYWAY`: the `blend_mode` and `zoom_behind` reasons are dropped and the `look.placed` guard of the blend is skipped); without it the
/// case holds the refusal (every frame of every case is refused for the pose, none drawn) and prints no price. With the patch it prints
/// `PRICE exact` / `PRICE inexact` for every case and fails on none: the verdict is the printed list.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); the price needs KERF_CHECKER_ANYWAY and the scratch patch"]
fn checker_e_m_a_blend_over_a_pose_what_the_refusal_costs() {
    use kerf_core::{Property, PropertyKey, Transition};
    let m = media();
    let v = variants();
    let s = shapes();
    let patched = std::env::var_os("KERF_CHECKER_ANYWAY").is_some();
    let opts = at_fps(30.0);
    let keys = |a: f64, b: f64| vec![PropertyKey::new(0.0, a), PropertyKey::new(0.2, b)];
    type Edit<'a> = Box<dyn Fn(&mut Clip) + 'a>;
    let poses: Vec<(&str, Edit<'_>)> = vec![
        (
            "zoom in",
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.2, 1.5, 0.0, 0.0)]),
        ),
        (
            "zoom out and over",
            Box::new(|c| c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.2, 0.55, 0.12, -0.08)]),
        ),
        (
            "position",
            Box::new(move |c| c.set_property_keys(Property::PosX, keys(-0.15, 0.2))),
        ),
    ];
    let mut priced: Vec<(String, usize, usize, bool)> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut price = |case: &str, tl: &Timeline, assets: &[Asset], frames: Range<u64>| {
        let refused = m_reasons(tl, assets, &opts, frames.clone());
        // (A cut of several clips is judged where a blended clip that moves is on screen only: the dissolves below are not asserted.)
        for (k, why) in refused.iter().filter(|_| !case.contains("a dissolve")) {
            if !why
                .iter()
                .any(|(kind, w)| *kind == "blend_mode" && w.contains("a moving zoom or position"))
            {
                failures.push(format!("{case}: frame {k} is not refused by the blend for the pose: {why:?}"));
            }
        }
        if !patched {
            return;
        }
        let drawn = ce_scan(case, tl, assets, &opts, frames, true);
        eprintln!("PRICE {}", drawn.line(case));
        priced.push((case.to_string(), drawn.drawn, drawn.frames, drawn.exact()));
    };
    // Every mode over a pose, on per-sample noise (an edge sample of the clip is never the same as its neighbour's).
    for (k, mode) in BLEND_MODES.iter().enumerate() {
        for (pname, pose) in &poses {
            let mut c = clip(&m.noise, 0.5, 1.0, 0.0);
            c.blend_mode = *mode;
            pose(&mut c);
            let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![c]], None);
            price(
                &format!("checker/e/m/price {mode:?} noise {pname} {k}"),
                &tl,
                &[m.bars.clone(), m.noise.clone()],
                0..6,
            );
        }
    }
    // Other pictures under Multiply and Difference: shapes, depth, labels, a bottom layer of its own tag.
    let pairs: Vec<(&str, &Asset, &Asset)> = vec![
        ("testsrc", &m.bars, &m.testsrc),
        ("edges", &m.bars, &m.edges),
        ("smooth", &m.testsrc, &m.smooth),
        ("pillar", &m.bars, &s.pillar),
        ("odd width", &m.bars, &s.odd_width),
        ("10-bit tv", &m.bars, &s.tenbit_tv),
        ("709 over 709", &v.bars709, &v.testsrc709),
        ("untagged over 709", &v.bars709, &m.testsrc),
        ("709 over untagged", &m.bars, &v.testsrc709),
        ("gray", &m.bars, &v.gray),
    ];
    for (name, bottom, top) in pairs {
        for mode in [kerf_core::BlendMode::Multiply, kerf_core::BlendMode::Difference] {
            for (pname, pose) in &poses[..2] {
                let mut c = clip(top, 0.5, 1.0, 0.0);
                c.blend_mode = mode;
                pose(&mut c);
                let tl = timeline(vec![vec![clip(bottom, 0.5, 1.0, 0.0)], vec![c]], None);
                price(
                    &format!("checker/e/m/price {name} {mode:?} {pname}"),
                    &tl,
                    &[bottom.clone(), top.clone()],
                    0..6,
                );
            }
        }
    }
    // An odd width, fades, a keyed opacity, a mask and a keyed colour on a moving blended clip; a placed bottom layer under a placed blended clip.
    let extras: Vec<(&str, Edit<'_>)> = vec![
        (
            "odd width",
            Box::new(|c| {
                c.transform.crop_right = 1.0 / 640.0;
                c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.2, 1.3, 0.05, 0.0)];
            }),
        ),
        (
            "fades",
            Box::new(|c| {
                c.fade_in = 0.1;
                c.fade_out = 0.1;
                c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.2, 1.3, 0.0, 0.0)];
            }),
        ),
        (
            "fades and a zoom out",
            Box::new(|c| {
                c.fade_in = 0.1;
                c.fade_out = 0.1;
                c.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.2, 0.6, 0.1, -0.05)];
            }),
        ),
        (
            "a fade-in and a move",
            Box::new(move |c| {
                c.fade_in = 0.15;
                c.set_property_keys(Property::PosX, keys(-0.1, 0.1));
            }),
        ),
        (
            "keyed opacity holds 1",
            Box::new(move |c| {
                c.set_property_keys(Property::PosX, keys(0.0, 0.2));
                c.set_property_keys(Property::Opacity, keys(1.0, 1.0));
            }),
        ),
        (
            "mask",
            Box::new(move |c| {
                c.set_property_keys(Property::Scale, keys(1.0, 1.3));
                c.mask = Some(mask_of(kerf_core::MaskShape::Ellipse, 0.3, false));
            }),
        ),
        (
            "keyed colour",
            Box::new(move |c| {
                c.set_property_keys(Property::PosY, keys(0.0, 0.1));
                c.set_property_keys(Property::Brightness, keys(-0.2, 0.2));
            }),
        ),
        (
            "a static turn",
            Box::new(move |c| {
                c.transform.rotation = 90.0;
                c.set_property_keys(Property::PosX, keys(0.0, 0.1));
            }),
        ),
    ];
    for (name, edit) in &extras {
        let mut c = clip(&m.noise, 0.5, 1.0, 0.0);
        c.blend_mode = kerf_core::BlendMode::Screen;
        edit(&mut c);
        let tl = timeline(vec![vec![clip(&m.bars, 0.5, 1.0, 0.0)], vec![c]], None);
        price(
            &format!("checker/e/m/price extra {name}"),
            &tl,
            &[m.bars.clone(), m.noise.clone()],
            0..6,
        );
    }
    let mut bottom = clip(&m.bars, 0.5, 1.0, 0.0);
    bottom.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.2, 0.6, 0.1, 0.0)];
    let mut top = clip(&m.noise, 0.5, 1.0, 0.0);
    top.blend_mode = kerf_core::BlendMode::Multiply;
    top.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.2, 1.4, 0.0, 0.0)];
    let tl = timeline(vec![vec![bottom], vec![top]], None);
    price(
        "checker/e/m/price a placed bottom layer under a placed blended clip",
        &tl,
        &[m.bars.clone(), m.noise.clone()],
        0..6,
    );
    // A dissolve between two blended clips: the pose moves on both, on one or on neither (the control, which is drawn today), and the clip
    // that does not move is blended or not.
    for (name, move_a, move_b, blend_a, blend_b) in [
        ("both blended, both move", true, true, true, true),
        ("both blended, the incoming moves", false, true, true, true),
        ("both blended, the outgoing moves", true, false, true, true),
        ("both blended, neither moves (the control)", false, false, true, true),
        ("a plain outgoing and a blended incoming that moves", true, true, false, true),
        ("a blended outgoing that moves and a plain incoming", true, true, true, false),
    ] {
        let mut a = clip(&m.testsrc, 0.5, 1.0, 0.0);
        if blend_a {
            a.blend_mode = kerf_core::BlendMode::Screen;
        }
        if move_a {
            a.keyframes = vec![zoom_key(0.0, 1.0, 0.0, 0.0), zoom_key(0.5, 1.4, 0.0, 0.0)];
        }
        let mut b = clip(&m.noise, 0.5, 1.0, 0.5);
        if blend_b {
            b.blend_mode = kerf_core::BlendMode::Multiply;
        }
        if move_b {
            b.keyframes = vec![zoom_key(0.0, 1.0, 0.1, 0.0), zoom_key(0.5, 0.7, 0.0, 0.1)];
        }
        b.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.3,
        });
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0)], vec![a, b]], None);
        price(
            &format!("checker/e/m/price a dissolve, {name}"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone(), m.noise.clone()],
            8..26,
        );
    }
    // The other transitions into a blended clip that moves: dips (the clip's own alpha fades), slides and pushes (the travel is added to the pose).
    for kind in [
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::PushUp,
        TransitionKind::SlideDown,
    ] {
        let mut a = clip(&m.testsrc, 0.5, 1.0, 0.0);
        a.blend_mode = kerf_core::BlendMode::Screen;
        let mut b = clip(&m.noise, 0.5, 1.0, 0.5);
        b.blend_mode = kerf_core::BlendMode::Multiply;
        b.keyframes = vec![zoom_key(0.0, 1.0, 0.1, 0.0), zoom_key(0.5, 0.7, 0.0, 0.1)];
        b.transition_in = Some(Transition { kind, duration: 0.3 });
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0)], vec![a, b]], None);
        price(
            &format!("checker/e/m/price a dissolve through {kind:?}, the incoming moves"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone(), m.noise.clone()],
            8..26,
        );
    }
    if patched {
        let exact = priced.iter().filter(|p| p.3 && p.1 == p.2).count();
        let inexact: Vec<&(String, usize, usize, bool)> = priced.iter().filter(|p| p.1 > 0 && !p.3).collect();
        let undrawn: Vec<&(String, usize, usize, bool)> = priced.iter().filter(|p| p.1 < p.2).collect();
        eprintln!(
            "PRICE summary: {} cases, {exact} drawn exactly on every frame (the refusal was wider than it had to be), {} drawn and not exact (necessary), {} not drawn by the compositor itself",
            priced.len(),
            inexact.len(),
            undrawn.len()
        );
        for p in &inexact {
            eprintln!("PRICE necessary: {}", p.0);
        }
        for p in &undrawn {
            eprintln!("PRICE not drawn: {}", p.0);
        }
    }
    assert!(failures.is_empty(), "{} cases: {failures:#?}", failures.len());
}

// ==== T6, second round: the matrix a moving still decides, the passes a moving still has no pose for, the colour that is keyed ========

/// A limited-range 4:2:0 picture that is a still (a JPEG 2000 file: the kind a WebP is for the compositor): `yuv420p`, no range flag.
fn t6_limited_420(w: u32, h: u32) -> Asset {
    let photo = t6_files::photo(
        &format!("limited-{w}x{h}.jp2"),
        &t6_files::grain(w, h),
        &["-c:v", "jpeg2000", "-pix_fmt", "yuv420p"],
    );
    let stream = photo
        .streams
        .iter()
        .find(|s| s.kind == kerf_core::StreamKind::Video)
        .expect("a picture");
    assert!(stream.image, "{:?} is not probed as a still image", photo.path);
    assert_eq!(stream.pix_fmt.as_deref(), Some("yuv420p"));
    photo
}

/// The export's moving still on its own route is a plain chain (`perspective` draws it into the frame and it ends in the delivery's
/// format, not on the alpha plane `overlay` takes natively), so it never decides the colour matrix of the cut: the first clip does, the footage
/// below it. A planner that took the still as placed (an alpha-plane chain, as a moving zoom's is) let the still's matrix decide, and the
/// footage was converted from its own tag into the still's in the frames before the still came in, where FFmpeg 9 leaves it alone.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn t6_a_moving_still_on_the_route_does_not_decide_the_matrix_of_the_footage_below_it() {
    let c = checker_media();
    let h = hd_pair();
    let mut cases = Vec::new();
    let mut decided = Vec::new();
    for (w, ph) in [(1200, 800), (640, 360)] {
        let photo = t6_limited_420(w, ph);
        for (name, video, (fw, fh), tag) in [
            ("bt709 512x256", &c.testsrc709, (512, 256), Some(kerf_core::YuvMatrix::Bt709)),
            (
                "bt2020 512x256",
                &c.testsrc2020,
                (512, 256),
                Some(kerf_core::YuvMatrix::Bt2020),
            ),
            ("untagged 512x256", &c.testsrc, (512, 256), None),
            ("bt709 HD", &h.phone, (1280, 720), Some(kerf_core::YuvMatrix::Bt709)),
        ] {
            let assets = [photo.clone(), video.clone()];
            // The still comes in after the footage has been on its own for a second, and goes on over it.
            let over = timeline(
                vec![vec![clip(video, 0.0, 2.0, 0.0)], vec![t6_in(&photo, 1.0, 1.0)]],
                cover(fw, fh),
            );
            let mut after = t6_in(&photo, 1.0, 1.5);
            after.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 0.5,
            });
            let dissolving = timeline(vec![vec![clip(video, 0.0, 1.0, 0.0), after]], cover(fw, fh));
            for (how, tl) in [("over", over), ("dissolving in after", dissolving)] {
                assert!(
                    takes_the_route(&tl, &assets, 8.0, 12),
                    "{name} {how}: the still does not take the route"
                );
                // What the plan says decides the cut's matrix: the footage's own tag, in every frame (negotiating policies only: where
                // the composite is fixed there is nothing to decide). A still that dissolves in is on the alpha plane, and decides itself.
                if policy() == CompositeColorPolicy::BottomLayerTag && how == "over" {
                    let planner = Planner::new(&tl, &assets, &at_fps(8.0), PlanRequest::motion(policy())).expect("planner");
                    for k in [0, 4, 9, 12] {
                        let plan = planner.at_frame(k).expect("plan");
                        decided.push((format!("{w}x{ph} {name} {how} frame {k}"), plan.canvas.cut_matrix, tag));
                    }
                }
                cases.push(t6_export(
                    &format!("t6/limited 4:2:0 still {w}x{ph} {how} {name} footage"),
                    tl,
                    &assets,
                    8.0,
                    2.0,
                    Draw::Any,
                ));
            }
        }
    }
    for (what, got, want) in &decided {
        // (The still's own tag, an untagged picture's 601, is not the footage's: a mixed cut says the footage's, and a cut of one matrix none.)
        assert_eq!(got, want, "{what}: the cut's matrix is not the footage's");
    }
    t6_exports("a limited-range moving still among footage that names a matrix", cases);
}

/// Every frame of `tl` is refused for `reason` (the plan of each is asked, with the compositor's own caps).
fn t6_refused_in_every_frame(case: &str, tl: &Timeline, assets: &[Asset], frames: Range<u64>, reason: &str) {
    let planner = Planner::new(tl, assets, &at_fps(30.0), PlanRequest::motion(policy())).expect("planner");
    let caps = compositor().caps();
    let mut missed = Vec::new();
    for k in frames.clone() {
        let plan = planner.at_frame(k).expect("plan");
        let why: Vec<String> = plan
            .reasons(&caps, plan.size(u32::MAX))
            .iter()
            .map(ToString::to_string)
            .collect();
        if !why.iter().any(|w| w.contains(reason)) {
            missed.push((k, why));
        }
    }
    assert!(
        missed.is_empty(),
        "{case}: {} of {} frames were not refused for `{reason}`; first: {:?}",
        missed.len(),
        frames.end - frames.start,
        missed.first()
    );
}

/// A moving still is not blended and its colour is not keyed: the blend chain and the `eq` per frame are not what the route's or the fit's
/// cases hold to FFmpeg's. A colour keyed through neutral at some frame is no colour there at all, but the engine's `eq` is in the chain of
/// every frame of the clip, so every frame is FFmpeg's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn t6_a_blended_or_keyed_colour_moving_still_is_refused_in_every_export_frame() {
    let p = photos();
    let png = std::slice::from_ref(&p.wide);
    let alone = |c: Clip| timeline(vec![vec![c]], cover(960, 540));
    let planner_at = |tl: &Timeline, k: u64| {
        Planner::new(tl, png, &at_fps(30.0), PlanRequest::motion(policy()))
            .expect("planner")
            .at_frame(k)
            .expect("plan")
    };
    for (route, mk) in [
        ("on the route", ken_burns as fn(&Asset, f64, f64) -> Clip),
        ("placed from the fit picture", t6_off as fn(&Asset, f64, f64) -> Clip),
    ] {
        // Brightness from -0.3 to 0.3 over the clip's second: neutral at 0.5 s, frame 15.
        let mut keyed = mk(&p.wide, 0.0, 1.0);
        keyed.set_property_keys(
            kerf_core::Property::Brightness,
            vec![kerf_core::PropertyKey::new(0.0, -0.3), kerf_core::PropertyKey::new(1.0, 0.3)],
        );
        let tl = alone(keyed);
        let plan = planner_at(&tl, 15);
        assert!(plan.layers[0].color.is_identity(), "{route}: frame 15 is not the neutral one");
        assert!(plan.layers[0].animated.is_some_and(|a| a.color), "{route}");
        assert_eq!(plan.takes_still_route(&plan.layers[0]), route == "on the route");
        t6_refused_in_every_frame(
            &format!("t6/keyed colour {route}"),
            &tl,
            png,
            upto(1.0, 30.0),
            "a still whose picture moves",
        );
        // A blend mode over the footage below it: FFmpeg's, as it is for a moving zoom.
        let mut blended = mk(&p.wide, 0.0, 1.0);
        blended.blend_mode = kerf_core::BlendMode::Multiply;
        let m = checker_media();
        let tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)], vec![blended]], cover(960, 540));
        let assets = [p.wide.clone(), m.testsrc.clone()];
        t6_refused_in_every_frame(
            &format!("t6/blended {route}"),
            &tl,
            &assets,
            upto(1.0, 30.0),
            "a moving zoom or position",
        );
    }
}

// ---- floating animated SVG titles in an export ---------------------------------------------

// The export reads a numbered sequence of PNGs resvg drew (`svg_layer::svg_sequences`): frame `j` of a title is the document at its
// clock `doc_t0 + j / fps` on exactly the output frames the overlay's `between(t,start,end)` holds on, whatever the frame rate, and the
// compositor draws the same PNG's planes converted by FFmpeg with `overlay`'s own arithmetic. Every frame of every case below is compared
// with the export's plane for plane: 0 differing samples.

#[path = "common/svg_titles.rs"]
mod svg_titles;
use svg_titles::{title, MOVING_ART, TITLE_ART};

fn titled(mut tl: Timeline, titles: Vec<kerf_core::SvgOverlay>) -> Timeline {
    tl.svg_overlays = titles;
    tl
}

fn pixels(x: i32, y: i32, w: u32, h: u32) -> [f64; 4] {
    svg_titles::pixels((640, 360), x, y, w, h)
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_floating_title_is_drawn_picture_by_picture_in_an_export() {
    let m = media();
    let assets = [m.testsrc.clone()];
    // A moving title placed at odd pixels, starting and ending between frames (its first picture is the first frame at or after the start,
    // its last the last at or before the end, and the document's clock runs from its start), at four frame rates.
    for fps in [30.0, 25.0, 24.0, 30000.0 / 1001.0] {
        let tl = titled(
            timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None),
            vec![title(MOVING_ART, 0.213, 1.517, pixels(101, 31, 421, 231), 1.0)],
        );
        run(&format!("svg/export/moving@{fps:.3}"), &tl, &assets, fps, 2.0);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_apps_svg_presets_are_drawn_as_the_export_draws_them() {
    use kerf_core::SvgPresetKind::{FadeUp, LowerThird, Typewriter, WordPop};
    let m = media();
    let assets = [m.testsrc.clone()];
    let full = [0.0, 0.0, 1.0, 1.0];
    for (name, kind, text, sub) in [
        ("fade-up", FadeUp, "Step one", Some("Import your footage")),
        ("lower-third", LowerThird, "Ada Lovelace", Some("Engineer")),
        ("typewriter", Typewriter, "Cut what does not help", None),
        ("word-pop", WordPop, "Export", None),
    ] {
        let tl = titled(
            timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None),
            vec![svg_titles::preset(kind, text, sub, (0.2, 1.8), full, (640, 360))],
        );
        run(&format!("svg/export/{name}"), &tl, &assets, 30.0, 2.0);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn titles_of_any_size_offset_and_opacity_are_drawn_as_the_export_draws_them() {
    let m = media();
    let assets = [m.testsrc.clone()];
    // Odd and negative offsets, odd sizes, a title past the edge of the frame, a translucent one, several at once.
    let placements = [
        ("odd-everything", pixels(65, 47, 237, 105), 1.0),
        ("off-top-left", pixels(-37, -23, 300, 170), 1.0),
        ("off-bottom-right", pixels(500, 290, 300, 170), 0.6),
        ("tiny", pixels(301, 177, 7, 5), 1.0),
        ("translucent", pixels(64, 46, 236, 104), 0.35),
    ];
    for (name, at, opacity) in placements {
        let tl = titled(
            timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)]], None),
            vec![title(MOVING_ART, 0.1, 0.9, at, opacity)],
        );
        run(&format!("svg/export/at/{name}"), &tl, &assets, 30.0, 1.0);
    }
    // A title that runs past the end of the cut (its last picture is the cut's last frame), one that begins part-way through its
    // animation (`offset`: what a range export cuts off its front) and one whose window closes between two frames.
    let mut running = title(MOVING_ART, 0.0, 5.0, pixels(65, 47, 237, 105), 1.0);
    running.offset = 0.35;
    let past = titled(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)]], None),
        vec![title(MOVING_ART, 0.0, 5.0, pixels(65, 47, 237, 105), 1.0), running],
    );
    run("svg/export/past-the-end+offset", &past, &assets, 30.0, 1.0);
    let between = titled(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)]], None),
        vec![title(MOVING_ART, 1.0 / 60.0, 0.5 + 1.0 / 90.0, pixels(31, 17, 411, 301), 1.0)],
    );
    run("svg/export/between-frames", &between, &assets, 30.0, 1.0);
    let mut several = titled(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)]], None),
        vec![
            title(TITLE_ART, 0.0, 1.0, pixels(40, 30, 331, 201), 1.0),
            title(MOVING_ART, 0.2, 0.8, pixels(100, 61, 421, 231), 0.7),
            title(TITLE_ART, 0.5, 1.0, pixels(121, 81, 301, 151), 0.5),
        ],
    );
    run("svg/export/several", &several, &assets, 30.0, 1.0);
    // ... and a caption over them: the export lays the clips, the titles in list order, then the text.
    let mut caption = kerf_core::TextOverlay::new("Caption over the titles", 0.1, 0.9);
    (caption.pos_y, caption.size, caption.bg) = (0.8, 0.06, Some("black@0.5".into()));
    several.overlays = vec![caption];
    run("svg/export/several+text", &several, &assets, 30.0, 1.0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_title_is_laid_on_the_composite_fades_dissolves_and_dips_leave() {
    let m = media();
    // A clip fading in and out under a title, and a dissolve, a dip and a slide between two clips with the title across the cut.
    let mut fading = clip(&m.testsrc, 0.0, 1.5, 0.0);
    fading.fade_in = 0.4;
    fading.fade_out = 0.4;
    run(
        "svg/export/over-fades",
        &titled(
            timeline(vec![vec![fading]], None),
            vec![title(TITLE_ART, 0.0, 1.5, pixels(65, 47, 237, 105), 1.0)],
        ),
        std::slice::from_ref(&m.testsrc),
        30.0,
        1.5,
    );
    for (name, kind, scale) in [
        ("dissolve", Some(TransitionKind::Crossfade), 1.0),
        ("dip", Some(TransitionKind::DipToBlack), 1.0),
        ("slide", Some(TransitionKind::SlideLeft), 0.6),
    ] {
        let (tl, assets) = pair(m, kind, 0.6, scale);
        let tl = titled(tl, vec![title(MOVING_ART, 0.2, 1.9, pixels(40, 30, 331, 201), 0.9)]);
        run(&format!("svg/export/over-{name}"), &tl, &assets, 30.0, 2.0);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_title_clip_is_refused_in_every_frame_and_the_frames_around_it_are_drawn() {
    use kerf_core::SvgPresetKind::FadeUp;
    let m = media();
    let card = Clip::title(
        kerf_core::TitleClip {
            name: "Intro".into(),
            svg: kerf_core::render_preset(FadeUp, &kerf_core::SvgPresetParams::new("Intro"), 0.5, (640, 360)),
            template: None,
        },
        0.5,
        0.5,
    );
    let mut tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 1.5, 0.0)]], None);
    tl.tracks.push(Track {
        clips: vec![card],
        ..Track::new(StreamKind::Video, "V2")
    });
    run_refusing(
        "svg/export/title-clip",
        &tl,
        std::slice::from_ref(&m.testsrc),
        30.0,
        1.5,
        &refusing("Svg", |u| matches!(u, Unsupported::Svg)),
    );
}

// ---- checker/svg: floating titles in an export, checked independently ------------------------------------------------------
//
// Cases the implementer did not write: window ends on and a hair off a frame's time at awkward rates, a negative start, a window between
// two frames, an offset; every kind of colour the composite can be; mixed matrices; the transitions and layers a title lies over; a held
// picture; and the refusals drawn anyway. A frame the plan draws is held to the export plane for plane (0 differing samples); a frame it
// refuses is drawn anyway and counted, and a refusal that would have been exact in every frame is reported.

mod checker_svg {
    use super::*;

    pub struct Extra {
        pub t709: Asset,
        pub t2020: Asset,
        pub t170m: Asset,
        pub fullrange: Asset,
        pub pc: Asset,
        pub hd: Asset,
    }

    pub fn extra() -> &'static Extra {
        static E: OnceLock<Extra> = OnceLock::new();
        E.get_or_init(|| {
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-svg-media");
            std::fs::create_dir_all(&dir).unwrap();
            let make = |name: &str, lavfi: &str, codec: &[&str], pix: &str, extra: &[&str]| -> Asset {
                let p = dir.join(name);
                let part = dir.join(format!(
                    "{name}.{}.part.{}",
                    std::process::id(),
                    name.rsplit('.').next().unwrap()
                ));
                let mut args = vec!["-f", "lavfi", "-i", lavfi, "-c:v"];
                args.extend_from_slice(codec);
                args.extend(["-pix_fmt", pix]);
                args.extend_from_slice(extra);
                args.push(part.to_str().unwrap());
                ffmpeg(&args);
                std::fs::rename(&part, &p).unwrap();
                probed(&p)
            };
            let h264 = ["libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
            let tag = |cs: &str, p: &str, trc: &str| format!("setparams=colorspace={cs}:color_primaries={p}:color_trc={trc}");
            let src = |size: &str, secs: u32, set: &str| format!("testsrc2=size={size}:rate=30:duration={secs},{set}");
            let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
            Extra {
                t709: make(
                    "t709.mp4",
                    &src("640x360", 4, &tag("bt709", "bt709", "bt709")),
                    &h264,
                    "yuv420p",
                    &[],
                ),
                t2020: make(
                    "t2020.mp4",
                    &src("640x360", 4, &tag("bt2020nc", "bt2020", "bt709")),
                    &h264,
                    "yuv420p",
                    &[],
                ),
                t170m: make(
                    "t170m.mp4",
                    &src("640x360", 4, &tag("smpte170m", "smpte170m", "smpte170m")),
                    &h264,
                    "yuv420p",
                    &[],
                ),
                fullrange: make(
                    "fullrange.mkv",
                    &src("640x360", 4, untagged),
                    &["mjpeg", "-qscale:v", "1"],
                    "yuvj420p",
                    &[],
                ),
                pc: make(
                    "pc.mkv",
                    &src("640x360", 4, untagged),
                    &["ffv1"],
                    "yuv420p",
                    &["-color_range", "pc"],
                ),
                hd: make(
                    "hd.mp4",
                    &src("1280x720", 2, untagged),
                    &["libx264", "-preset", "ultrafast", "-crf", "10", "-g", "10"],
                    "yuv420p",
                    &[],
                ),
            }
        })
    }

    #[derive(Default, Debug)]
    pub struct Stats {
        pub frames: usize,
        pub drawn: usize,
        pub refused: usize,
        pub refused_exact: usize,
        pub kinds: std::collections::BTreeSet<String>,
        pub failures: Vec<(u64, usize, i32)>,
    }

    /// Plan, export and compare every frame of `frames`: a frame the plan draws must equal the export's planes (a failure otherwise); one it
    /// refuses is drawn anyway (a render-time refusal counts as refused and not exact) and counted.
    pub fn judged(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Range<u64>) -> Stats {
        judged_in(case, tl, assets, fps, frames, true)
    }

    /// [`judged`] that reports the frames the plan draws and the export does not, instead of failing on them (`strict` false).
    pub fn judged_in(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Range<u64>, strict: bool) -> Stats {
        let opts = at_fps(fps);
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
        let size = (played.w as u32, played.h as u32);
        let mut motion = MotionFrames::new(frame_source());
        let mut stats = Stats::default();
        for k in frames {
            stats.frames += 1;
            let plan = planner.at_frame(k).expect("plan");
            let why = plan.reasons(&compositor().caps(), size);
            let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
            // (The render refuses what the plan refuses: a refused frame is drawn anyway only under a scratch patch that lifts the refusal.)
            let drawn = compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size);
            let want = played.planes(k as usize);
            let exact = match &drawn {
                Ok(d) => {
                    let got = planes_of(d);
                    let (y, u, v) = (
                        PlaneStats::of(want[0], got[0]),
                        PlaneStats::of(want[1], got[1]),
                        PlaneStats::of(want[2], got[2]),
                    );
                    let n = y.differing + u.differing + v.differing;
                    (n == 0, n, y.max.max(u.max).max(v.max))
                }
                Err(_) => (false, usize::MAX, 255),
            };
            if why.is_empty() {
                stats.drawn += 1;
                if !exact.0 {
                    stats.failures.push((k, exact.1, exact.2));
                    if stats.failures.len() <= 3 {
                        montage(case, k, &played, &plan, &picture, motion.counts());
                    }
                }
            } else {
                stats.refused += 1;
                for w in &why {
                    stats.kinds.insert(w.kind().to_string());
                }
                if exact.0 {
                    stats.refused_exact += 1;
                }
            }
        }
        eprintln!(
            "CHECKER-SVG {case} @{fps}: {} frames, drawn {} (differing in {}), refused {} {:?} of which exact anyway {}",
            stats.frames,
            stats.drawn,
            stats.failures.len(),
            stats.refused,
            stats.kinds,
            stats.refused_exact
        );
        assert!(
            !strict || std::env::var_os("KERF_PARITY_EXPLORE").is_some() || stats.failures.is_empty(),
            "{case}: frames the plan draws that differ from the export (frame, differing samples, max): {:?}",
            &stats.failures[..stats.failures.len().min(12)]
        );
        stats
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_window_ends_at_awkward_rates() {
    let m = media();
    let assets = [m.testsrc.clone()];
    for fps in [
        23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0, 12.5, 15.0, 7.0, 10.0, 48.0, 100.0, 119.88,
    ] {
        let rate = kerf_core::Rational::from_fps(fps).expect("a rate");
        let ft = |k: u64| rate.frame_time(k);
        let secs = 18.5 / fps;
        let typed = |t: f64| (t * 1000.0).round() / 1000.0;
        let mut offset = title(MOVING_ART, ft(2), ft(14), pixels(451, 141, 181, 121), 1.0);
        offset.offset = 0.4;
        let titles = vec![
            // both ends on a frame's time, and a hair inside and outside each
            title(MOVING_ART, ft(3), ft(11), pixels(11, 7, 201, 121), 1.0),
            title(MOVING_ART, ft(3) + 1e-9, ft(11) - 1e-9, pixels(231, 7, 201, 121), 1.0),
            title(MOVING_ART, ft(3) - 1e-9, ft(11) + 1e-9, pixels(451, 7, 181, 121), 1.0),
            // times typed to the millisecond
            title(MOVING_ART, typed(ft(5)), typed(ft(13)), pixels(11, 141, 201, 121), 0.8),
            // one frame; none (between two frames); a negative start; to the cut's end
            title(MOVING_ART, ft(7), ft(7) + 0.5 / fps, pixels(231, 141, 201, 121), 1.0),
            title(
                MOVING_ART,
                ft(7) + 0.2 / fps,
                ft(7) + 0.8 / fps,
                pixels(11, 271, 201, 81),
                1.0,
            ),
            title(MOVING_ART, -0.5, ft(9), pixels(231, 271, 101, 81), 1.0),
            title(TITLE_ART, ft(2), secs, pixels(341, 271, 101, 81), 1.0),
            offset,
        ];
        let tl = titled(timeline(vec![vec![clip(&m.testsrc, 0.0, secs, 0.0)]], None), titles);
        let stats = checker_svg::judged(&format!("checker/svg/export/ends@{fps}"), &tl, &assets, fps, 0..18);
        assert_eq!(stats.refused, 0, "{stats:?}");
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_over_every_composite_and_mixed_matrices() {
    use checker_svg::*;
    let m = media();
    let x = extra();
    let art = |a: &str| title(a, 0.0, 1.0, pixels(65, 47, 237, 105), 1.0);
    let both = |tl: Timeline| {
        titled(
            tl,
            vec![art(TITLE_ART), title(MOVING_ART, 0.0, 1.0, pixels(331, 201, 211, 101), 0.7)],
        )
    };
    let single = |a: &Asset| both(timeline(vec![vec![clip(a, 0.0, 1.0, 0.0)]], None));
    let mut summary = Vec::new();
    let one: Vec<(&str, Timeline, Vec<Asset>)> = vec![
        ("untagged", single(&m.testsrc), vec![m.testsrc.clone()]),
        ("bt709", single(&x.t709), vec![x.t709.clone()]),
        ("bt2020", single(&x.t2020), vec![x.t2020.clone()]),
        ("smpte170m", single(&x.t170m), vec![x.t170m.clone()]),
        ("yuvj420p", single(&x.fullrange), vec![x.fullrange.clone()]),
        ("yuv420p flagged pc", single(&x.pc), vec![x.pc.clone()]),
        ("hd untagged", single(&x.hd), vec![x.hd.clone()]),
        (
            "still png",
            both(timeline(vec![vec![clip(&m.image, 0.0, 1.0, 0.0)]], None)),
            vec![m.image.clone()],
        ),
        (
            "untagged then bt709",
            both(timeline(
                vec![vec![clip(&m.testsrc, 0.0, 0.5, 0.0), clip(&x.t709, 0.0, 0.5, 0.5)]],
                None,
            )),
            vec![m.testsrc.clone(), x.t709.clone()],
        ),
        (
            "bt709 then untagged",
            both(timeline(
                vec![vec![clip(&x.t709, 0.0, 0.5, 0.0), clip(&m.testsrc, 0.0, 0.5, 0.5)]],
                None,
            )),
            vec![m.testsrc.clone(), x.t709.clone()],
        ),
        (
            "bt709 then bt2020",
            both(timeline(
                vec![vec![clip(&x.t709, 0.0, 0.5, 0.0), clip(&x.t2020, 0.0, 0.5, 0.5)]],
                None,
            )),
            vec![x.t709.clone(), x.t2020.clone()],
        ),
        (
            "hd untagged over bt709 (the BT.709 label)",
            both(timeline(
                vec![vec![clip(&x.t709, 0.0, 1.0, 0.0)], vec![clip(&x.hd, 0.0, 1.0, 0.0)]],
                None,
            )),
            vec![x.t709.clone(), x.hd.clone()],
        ),
        (
            "bt709 over untagged",
            both(timeline(
                vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)], vec![clip(&x.t709, 0.0, 1.0, 0.0)]],
                None,
            )),
            vec![m.testsrc.clone(), x.t709.clone()],
        ),
    ];
    for (name, tl, assets) in one {
        let stats = checker_svg::judged(&format!("checker/svg/export/composite/{name}"), &tl, &assets, 30.0, 0..30);
        summary.push(format!(
            "{name}: {} drawn, {} refused {:?}, exact anyway {}",
            stats.drawn, stats.refused, stats.kinds, stats.refused_exact
        ));
    }
    eprintln!("CHECKER-SVG composite summary:\n{}", summary.join("\n"));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_over_transitions_layers_and_captions() {
    use checker_svg::*;
    let m = media();
    let x = extra();
    let ti = title(MOVING_ART, 0.0, 2.0, pixels(65, 47, 237, 105), 0.9);
    let to = |tl: Timeline| {
        titled(
            tl,
            vec![ti.clone(), title(TITLE_ART, 0.3, 1.9, pixels(331, 201, 211, 101), 1.0)],
        )
    };
    // Every transition the model has, the title across the cut.
    for (name, kind, scale) in [
        ("crossfade", TransitionKind::Crossfade, 1.0),
        ("dip-black", TransitionKind::DipToBlack, 1.0),
        ("dip-white", TransitionKind::DipToWhite, 1.0),
        ("slide-left", TransitionKind::SlideLeft, 0.6),
        ("slide-right", TransitionKind::SlideRight, 0.6),
        ("slide-up", TransitionKind::SlideUp, 0.6),
        ("slide-down", TransitionKind::SlideDown, 0.6),
    ] {
        let (tl, assets) = pair(m, Some(kind), 0.6, scale);
        checker_svg::judged(&format!("checker/svg/export/over/{name}"), &to(tl), &assets, 30.0, 0..60);
    }
    // A fading clip, a translucent layer, a blended one, a small one in a corner, and a caption over the lot.
    let mut fading = clip(&m.testsrc, 0.0, 2.0, 0.0);
    fading.fade_in = 0.4;
    fading.fade_out = 0.4;
    let mut ghost = clip(&m.bars, 0.0, 2.0, 0.0);
    ghost.transform.opacity = 0.5;
    ghost.transform.scale = 0.6;
    let mut blended = clip(&m.bars, 0.0, 2.0, 0.0);
    blended.blend_mode = kerf_core::BlendMode::Multiply;
    let mut pip = clip(&x.t709, 0.0, 2.0, 0.0);
    pip.transform.scale = 0.4;
    pip.transform.pos_x = 0.25;
    pip.transform.pos_y = -0.2;
    let cases: Vec<(&str, Timeline, Vec<Asset>)> = vec![
        ("fades", timeline(vec![vec![fading]], None), vec![m.testsrc.clone()]),
        (
            "translucent-layer",
            timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)], vec![ghost]], None),
            vec![m.testsrc.clone(), m.bars.clone()],
        ),
        (
            "blended-layer",
            timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)], vec![blended]], None),
            vec![m.testsrc.clone(), m.bars.clone()],
        ),
        (
            "pip-bt709-over-untagged",
            timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)], vec![pip]], None),
            vec![m.testsrc.clone(), x.t709.clone()],
        ),
    ];
    for (name, tl, assets) in cases {
        checker_svg::judged(&format!("checker/svg/export/over/{name}"), &to(tl), &assets, 30.0, 0..60);
    }
    let mut captioned = to(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None));
    let mut caption = kerf_core::TextOverlay::new("Caption over the titles", 0.1, 1.9);
    (caption.pos_y, caption.size, caption.bg) = (0.8, 0.06, Some("black@0.5".into()));
    captioned.overlays = vec![caption];
    checker_svg::judged(
        "checker/svg/export/over/caption",
        &captioned,
        std::slice::from_ref(&m.testsrc),
        30.0,
        0..60,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_held_pictures_and_documents_that_step() {
    let m = media();
    let assets = [m.testsrc.clone()];
    let held = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90"><rect x="9" y="7" width="140" height="70" rx="6" fill="#33aaff" fill-opacity="0.6"/></svg>"##;
    let words = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90"><!-- animate --><rect x="9" y="7" width="140" height="70" fill="#ffaa33" fill-opacity="0.45"/></svg>"##;
    let step = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90"><rect x="9" y="7" width="140" height="70" fill="#cc2244"><set attributeName="fill" to="#22cc44" begin="0.5s"/><set attributeName="fill-opacity" to="0.3" begin="0.8s"/></rect></svg>"##;
    for fps in [29.97, 30.0, 25.0, 23.976, 60.0] {
        let tl = titled(
            timeline(vec![vec![clip(&m.testsrc, 0.0, 1.3, 0.0)]], None),
            vec![
                title(held, 0.1, 1.2, pixels(11, 7, 301, 151), 1.0),
                title(words, 0.0, 1.3, pixels(331, 7, 201, 111), 1.0),
                title(step, 0.05, 1.25, pixels(61, 201, 351, 151), 1.0),
            ],
        );
        let stats = checker_svg::judged(
            &format!("checker/svg/export/held@{fps}"),
            &tl,
            &assets,
            fps,
            0..(1.3 * fps).floor() as u64,
        );
        assert_eq!(stats.refused, 0, "{stats:?}");
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_a_long_cut_many_titles_at_59_94() {
    use kerf_core::SvgPresetKind::{FadeUp, LowerThird, Typewriter, WordPop};
    let m = media();
    let assets = [m.testsrc.clone()];
    let full = [0.0, 0.0, 1.0, 1.0];
    let tl = titled(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None),
        vec![
            title(MOVING_ART, 0.0, 2.0, full, 1.0),
            title(TITLE_ART, 0.25, 1.75, pixels(33, 27, 301, 171), 0.5),
            svg_titles::preset(Typewriter, "Cut what does not help", None, (0.1, 1.9), full, (640, 360)),
            svg_titles::preset(
                LowerThird,
                "Ada Lovelace",
                Some("Engineer"),
                (0.4, 1.6),
                pixels(1, 1, 639, 359),
                (640, 360),
            ),
            svg_titles::preset(WordPop, "Export", None, (0.7, 1.1), pixels(121, 81, 301, 151), (640, 360)),
            svg_titles::preset(
                FadeUp,
                "Step one",
                Some("Import"),
                (1.0, 2.0),
                pixels(-31, -11, 701, 391),
                (640, 360),
            ),
        ],
    );
    let stats = checker_svg::judged("checker/svg/export/many@59.94", &tl, &assets, 59.94, 0..119);
    assert_eq!(stats.refused, 0, "{stats:?}");
}

// ---- checker/svg: rendering and warming at once -----------------------------------------------------------------------------

/// Three threads warm the pictures of the frames ahead (`Compositor::warm_svg`, what the app's prefetch thread does) in different orders
/// while the frames are drawn: every drawn frame is the export's, and in the child run below every picture is converted once.
#[allow(clippy::print_stdout)]
fn checker_svg_concurrent_body() {
    let m = media();
    let assets = [m.testsrc.clone()];
    let opts = at_fps(30.0);
    let tl = titled(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 1.5, 0.0)]], None),
        vec![
            title(MOVING_ART, 0.0, 1.5, pixels(21, 11, 401, 231), 0.8),
            title(TITLE_ART, 0.2, 1.4, pixels(151, 71, 453, 255), 1.0),
        ],
    );
    let played = export(&format!("checker_svg_concurrent_{}", std::process::id()), &tl, &assets, &opts);
    let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let size = (played.w as u32, played.h as u32);
    let plans: Vec<_> = (0..45u64).map(|k| planner.at_frame(k).expect("plan")).collect();
    let _ = compositor();
    if let Some(log) = std::env::var_os("CHECKER_SVG_LOG") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(log).expect("the spy's log");
        writeln!(f, "MARK").unwrap();
    }
    let keys: std::collections::BTreeSet<String> = plans
        .iter()
        .flat_map(|p| p.svg.iter())
        .filter_map(|s| {
            let t = s.title.as_ref()?;
            let text = t.text().ok()?;
            Some(t.key(&text, s.rect(size)?))
        })
        .collect();
    println!("CHECKER_SVG_DISTINCT {}", keys.len());
    let bad = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for t in 0..3usize {
            let plans = &plans;
            scope.spawn(move || {
                let order: Vec<usize> = match t {
                    0 => (0..45).collect(),
                    1 => (0..45).rev().collect(),
                    _ => (0..45).map(|i| (i * 7) % 45).collect(),
                };
                for k in order {
                    let _ = compositor().warm_svg(&plans[k], size);
                }
            });
        }
        let mut motion = MotionFrames::new(frame_source());
        for (k, plan) in plans.iter().enumerate() {
            let picture = motion.frames(plan).expect("frames");
            let drawn = compositor()
                .composite_yuv_counted(plan, &picture, motion.counts(), size)
                .unwrap_or_else(|e| panic!("frame {k}: {e}"));
            let want = played.planes(k);
            let got = planes_of(&drawn);
            if (0..3).any(|i| PlaneStats::of(want[i], got[i]).differing != 0) {
                bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });
    assert_eq!(
        bad.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "frames that differ from the export while pictures are warmed beside them"
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_rendering_while_warming_is_exact() {
    checker_svg_concurrent_body();
}

/// The same, in a process of its own run under a spy that logs each FFmpeg run: after the probe, one conversion per distinct picture.
#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_each_picture_is_converted_once_when_render_and_warm_race() {
    use std::os::unix::fs::PermissionsExt;
    if std::env::var_os("CHECKER_SVG_CHILD").is_some() {
        checker_svg_concurrent_body();
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-svg-race");
    std::fs::create_dir_all(&dir).unwrap();
    let spy = dir.join(format!("spy-{}.sh", std::process::id()));
    std::fs::write(
        &spy,
        "#!/bin/sh\nclass=other\nfor a in \"$@\"; do case \"$a\" in *image2pipe*) class=convert;; esac; done\nprintf '%s\\n' \"$class\" >> \"$CHECKER_SVG_LOG\"\nexec \"$CHECKER_SVG_REAL\" \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&spy, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = dir.join("log.txt");
    std::fs::write(&log, "").unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "checker_svg_export_each_picture_is_converted_once_when_render_and_warm_race",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("CHECKER_SVG_CHILD", "1")
        .env("KERF_FFMPEG", &spy)
        .env("CHECKER_SVG_REAL", kerf_core::ffmpeg_path())
        .env("CHECKER_SVG_LOG", &log)
        .stdin(Stdio::null())
        .output()
        .expect("run the child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the child failed:\n{stdout}\n{stderr}");
    let distinct: usize = stdout
        .lines()
        .find_map(|l| l.split_once("CHECKER_SVG_DISTINCT ").map(|(_, rest)| rest))
        .expect("the child counted the pictures")
        .trim()
        .parse()
        .unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    let after = text.split("MARK").nth(1).expect("the mark");
    let converted = after.lines().filter(|l| *l == "convert").count();
    eprintln!("{distinct} distinct pictures, {converted} conversions after the probe");
    assert_eq!(converted, distinct, "a picture was converted more than once, or not at all");
}

/// A title clip on a track over a clip, plain (no fade, no transition, the whole frame): the plan refuses every frame it is on
/// (`Unsupported::Svg`) because the clip's chain is not an overlay. With a scratch patch to `planner.rs` that names it as a floating
/// title the compositor draws it anyway; this case prints how many of its frames are then the export's. (Unpatched: all refused.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_a_plain_title_clip_drawn_as_a_floating_title_anyway() {
    use kerf_core::SvgPresetKind::{FadeUp, LowerThird};
    let m = media();
    let assets = [m.testsrc.clone()];
    let card = |kind, text: &str, start, dur| {
        Clip::title(
            kerf_core::TitleClip {
                name: "Card".into(),
                svg: kerf_core::render_preset(kind, &kerf_core::SvgPresetParams::new(text), dur, (640, 360)),
                template: None,
            },
            start,
            dur,
        )
    };
    let static_art = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 640 360"><rect x="41" y="37" width="300" height="140" rx="9" fill="#3355ff" fill-opacity="0.55"/><circle cx="400" cy="200" r="60" fill="#ffd000" fill-opacity="0.8"/></svg>"##.to_string();
    let static_card = Clip::title(
        kerf_core::TitleClip {
            name: "Static".into(),
            svg: static_art,
            template: None,
        },
        0.3,
        0.9,
    );
    for (name, clip) in [
        ("static", static_card),
        ("fade-up", card(FadeUp, "Intro", 0.3, 0.9)),
        ("lower-third", card(LowerThird, "Ada", 0.3, 0.9)),
    ] {
        let mut tl = timeline(vec![vec![clip_of(&m.testsrc, 1.5)]], None);
        tl.tracks.push(Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V2")
        });
        let stats = checker_svg::judged_in(
            &format!("checker/svg/export/title-clip/{name}"),
            &tl,
            &assets,
            30.0,
            0..45,
            false,
        );
        eprintln!("CHECKER-SVG title clip {name}: {stats:?}");
    }
}

fn clip_of(a: &Asset, secs: f64) -> Clip {
    clip(a, 0.0, secs, 0.0)
}

/// A picture whose conversion child stalls costs the frame at most the child's 20 s, is then refused (not tried again) for 20 s without
/// another child, does not hold up another picture, and is made again once the 20 s are over.
#[cfg(unix)]
#[allow(clippy::print_stdout)]
fn checker_svg_stall_body() {
    let m = media();
    let assets = [m.testsrc.clone()];
    let opts = at_fps(30.0);
    let tl = titled(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 1.5, 0.0)]], None),
        vec![title(MOVING_ART, 0.0, 1.5, pixels(21, 11, 401, 231), 0.8)],
    );
    let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let size = (640u32, 360u32);
    let (a, b) = (planner.at_frame(5).expect("plan"), planner.at_frame(20).expect("plan"));
    let _ = compositor();
    let mut motion = MotionFrames::new(frame_source());
    let draw = |plan: &kerf_core::RenderPlan, motion: &mut MotionFrames| {
        let picture = motion.frames(plan).expect("frames");
        compositor().composite_yuv_counted(plan, &picture, motion.counts(), size)
    };
    let stall = std::env::var_os("CHECKER_SVG_STALLFILE").expect("the stall file");
    std::fs::write(&stall, "").unwrap();
    let t0 = std::time::Instant::now();
    let first = draw(&a, &mut motion);
    let first_took = t0.elapsed();
    let failed_at = std::time::Instant::now();
    println!(
        "CHECKER_SVG_STALL first: {:?} after {:.1} s",
        first.as_ref().map(|_| "drawn").map_err(ToString::to_string),
        first_took.as_secs_f64()
    );
    assert!(first.is_err(), "a stalled child must not be drawn");
    assert!(first_took.as_secs_f64() < 30.0, "{first_took:?}");
    let t1 = std::time::Instant::now();
    let again = draw(&a, &mut motion).expect_err("remembered");
    println!("CHECKER_SVG_STALL again: {again} after {:.2} s", t1.elapsed().as_secs_f64());
    assert!(t1.elapsed().as_secs_f64() < 2.0, "the failure is remembered, no second child");
    // Another picture is its own (the stall file is still there: it stalls too, but it is not refused for A's sake) -- remove the stall.
    std::fs::remove_file(&stall).unwrap();
    let t2 = std::time::Instant::now();
    draw(&b, &mut motion).expect("another picture is made while one is remembered as failed");
    println!("CHECKER_SVG_STALL other: drawn after {:.2} s", t2.elapsed().as_secs_f64());
    assert!(t2.elapsed().as_secs_f64() < 5.0);
    assert!(draw(&a, &mut motion).is_err(), "still remembered with the child fixed");
    let wait = std::time::Duration::from_millis(20_400).saturating_sub(failed_at.elapsed());
    std::thread::sleep(wait);
    let t3 = std::time::Instant::now();
    draw(&a, &mut motion).expect("made again after 20 s");
    println!(
        "CHECKER_SVG_STALL recovered: drawn after {:.2} s, {:.1} s after the failure",
        t3.elapsed().as_secs_f64(),
        failed_at.elapsed().as_secs_f64()
    );
}

#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); takes about 45 s"]
fn checker_svg_export_a_stalled_conversion_is_bounded_remembered_and_forgotten_after_twenty_seconds() {
    use std::os::unix::fs::PermissionsExt;
    if std::env::var_os("CHECKER_SVG_CHILD").is_some() {
        checker_svg_stall_body();
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-svg-stall");
    std::fs::create_dir_all(&dir).unwrap();
    let spy = dir.join(format!("spy-{}.sh", std::process::id()));
    std::fs::write(
        &spy,
        "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in *format=yuva420p*) [ -e \"$CHECKER_SVG_STALLFILE\" ] && exec sleep 3601.37;; esac; done\nexec \"$CHECKER_SVG_REAL\" \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&spy, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "checker_svg_export_a_stalled_conversion_is_bounded_remembered_and_forgotten_after_twenty_seconds",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("CHECKER_SVG_CHILD", "1")
        .env("KERF_FFMPEG", &spy)
        .env("CHECKER_SVG_REAL", kerf_core::ffmpeg_path())
        .env("CHECKER_SVG_STALLFILE", dir.join(format!("stall-{}", std::process::id())))
        .stdin(Stdio::null())
        .output()
        .expect("run the child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stdout.lines().filter(|l| l.contains("CHECKER_SVG_STALL")) {
        eprintln!("{line}");
    }
    assert!(out.status.success(), "the child failed:\n{stdout}\n{stderr}");
}

/// The same in an export: the frames of the gap are refused (`SvgOverlay(NoLayer)`); the frames over the clip are drawn and exact.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_svg_export_a_title_over_a_gap_in_the_cut() {
    let m = media();
    let assets = [m.testsrc.clone()];
    let tl = titled(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 0.5, 0.5)]], None),
        vec![
            title(MOVING_ART, 0.0, 1.0, pixels(65, 47, 237, 105), 1.0),
            title(TITLE_ART, 0.1, 0.9, pixels(331, 201, 211, 101), 0.6),
        ],
    );
    let stats = checker_svg::judged("checker/svg/export/gap", &tl, &assets, 30.0, 0..30);
    eprintln!("CHECKER-SVG gap: {stats:?}");
    assert!(
        (12..=15).contains(&stats.refused),
        "the frames of the gap (the clip's overlay opens a frame early): {stats:?}"
    );
    assert!(stats.kinds.contains("svg_overlay"), "{stats:?}");
}

// ---- checker/mx: the cascade of a dissolving clip's fit `scale`, and the fades of a converted clip, over several frames ---------------------------

/// Phone-shaped footage the implementer's `matrix_media` does not hold: BT.2020, and hard black-and-white edges (a filter overshoots a
/// byte there) in each of the three matrices, 540x960 lossless H.264.
struct MxMedia {
    p2020: Asset,
    e709: Asset,
    e601: Asset,
    e2020: Asset,
}

fn mx_media() -> &'static MxMedia {
    static M: OnceLock<MxMedia> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("motion-media-mx");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, lavfi: &str| -> Asset {
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
            probed(&p)
        };
        let bt709 = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
        let smpte170m = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";
        let bt2020 = "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10";
        let edges = "nullsrc=size=540x960:rate=30:duration=4,format=yuv420p,geq=lum='if(gt(mod(floor(X/3)+floor(Y/5)+floor(N/3),2),0),255,0)':cb='if(gt(mod(floor(X/7)+floor(N/5),2),0),240,16)':cr='if(gt(mod(floor(Y/9),2),0),240,16)'";
        MxMedia {
            p2020: make("p2020.mp4", &format!("testsrc2=size=540x960:rate=30:duration=4,{bt2020}")),
            e709: make("e709.mp4", &format!("{edges},{bt709}")),
            e601: make("e601.mp4", &format!("{edges},{smpte170m}")),
            e2020: make("e2020.mp4", &format!("{edges},{bt2020}")),
        }
    })
}

/// Three phone clips of a second each, the first plain, the others entering through a crossfade of `dissolve` seconds (the first of
/// those decides the cut's matrix, the third is converted through the cascade), on a canvas of `delivery`.
fn mx_vlog(a: &Asset, b: &Asset, c: &Asset, dissolve: f64, delivery: Option<Delivery>) -> (Timeline, Vec<Asset>) {
    let crossfade = |mut c: Clip| {
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: dissolve,
        });
        c
    };
    let track = vec![
        clip(a, 0.5, 1.5, 0.0),
        crossfade(clip(b, 0.5, 1.5, 1.0)),
        crossfade(clip(c, 0.5, 1.5, 2.0)),
    ];
    let mut assets: Vec<Asset> = vec![];
    for x in [a, b, c] {
        if !assets.iter().any(|y| y.id == x.id) {
            assets.push(x.clone());
        }
    }
    (timeline(vec![track], delivery), assets)
}

/// Dissolving phone clips of every ordered trio of matrices, on busy and on hard-edged pictures, at ratios from 6:1 down to a mild shrink,
/// dissolves from a few frames to a second, at 24, 25, 30 and 60 frames per second: every frame of every plane held to the export.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_dissolving_phone_clips_of_every_trio_are_the_exports() {
    let x = matrix_media();
    let y = mx_media();
    let busy = [&x.portrait709, &x.portrait601, &y.p2020];
    let edged = [&y.e709, &y.e601, &y.e2020];
    let trios: [(usize, usize, usize); 6] = [(0, 1, 2), (0, 2, 1), (1, 0, 2), (1, 2, 0), (2, 0, 1), (2, 1, 0)];
    let canvases = [
        Some(Delivery::new(960, 540, Fit::Contain)),
        Some(Delivery::new(290, 160, Fit::Contain)),
        Some(Delivery::new(640, 360, Fit::Contain)),
        Some(Delivery::new(1280, 720, Fit::Contain)),
        Some(Delivery::new(480, 270, Fit::Contain)),
        Some(Delivery::new(320, 180, Fit::Contain)),
        Some(Delivery::new(580, 320, Fit::Contain)),
        Some(Delivery::new(1920, 1080, Fit::Contain)),
    ];
    let fps_list = [24.0, 25.0, 30.0, 60.0];
    let dissolves = [0.1, 0.25, 0.5, 1.0];
    let mut n = 0usize;
    let (mut ran, mut skipped) = (0usize, 0usize);
    for (t, &(a, b, c)) in trios.iter().enumerate() {
        for (src_name, set) in [("busy", &busy), ("edged", &edged)] {
            let canvas = canvases[(n + t) % canvases.len()];
            let fps = fps_list[n % fps_list.len()];
            let dissolve = dissolves[(n / 2) % dissolves.len()];
            n += 1;
            let (tl, assets) = mx_vlog(set[a], set[b], set[c], dissolve, canvas);
            let (w, h) = canvas.map_or((0, 0), |d| (d.width, d.height));
            let case = format!("checker/mx/vlog {src_name} trio {a}{b}{c} {w}x{h} {fps} fps, dissolve {dissolve}");
            // (A canvas whose fit leaves a picture of odd size is the plan's to refuse, as it always was: skipped, and counted.)
            if let Some(why) = mx_refusal_in(&tl, &assets, fps, 3.0) {
                eprintln!("{case}: not held, the plan refuses it: {why}");
                skipped += 1;
                continue;
            }
            run(&case, &tl, &assets, fps, 3.0);
            ran += 1;
        }
    }
    eprintln!(
        "checker/mx vlogs: {ran} held to the export, {skipped} refused by the plan for a canvas that leaves an odd picture"
    );
    assert!(ran >= 6, "only {ran} vlogs ran");
}

/// The first reason the plan refuses a frame of the cut, if it does.
fn mx_refusal_in(tl: &Timeline, assets: &[Asset], fps: f64, secs: f64) -> Option<String> {
    let opts = at_fps(fps);
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(policy())).expect("planner");
    for k in upto(secs, fps) {
        let plan = planner.at_frame(k).expect("plan");
        let why = plan.reasons(&compositor().caps(), (plan.canvas.width, plan.canvas.height));
        if !why.is_empty() {
            return Some(format!("frame {k}: {why:?}"));
        }
    }
    None
}

/// A fade to black on a plain converted clip (the planes are faded, then converted): fades of one frame to the whole clip, in and out, at
/// four frame rates, letterboxed and not, on busy and on hard-edged pictures, and a fade on the bottom clip (not converted).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_fades_on_a_plain_converted_clip_are_the_exports_every_frame() {
    let x = matrix_media();
    let y = mx_media();
    let landscape = Some(Delivery::new(960, 540, Fit::Contain));
    let mut n = 0usize;
    for fps in [24.0, 25.0, 30.0, 60.0] {
        let frame = 1.0 / fps;
        for (fade_in, fade_out) in [
            (frame, 0.0),
            (2.0 * frame, 0.0),
            (0.0, frame),
            (0.0, 3.0 * frame),
            (0.1, 0.1),
            (0.4, 0.0),
            (0.0, 0.4),
            (0.5, 0.5),
            (1.0, 0.0),
            (0.0, 1.0),
            (0.7, 0.7),
        ] {
            // Rotating: the clip converted is BT.601 under a BT.709 bottom, BT.709 under BT.2020, BT.2020 under BT.601, in turn.
            let (bottom, top) = match n % 3 {
                0 => (&x.portrait709, if n.is_multiple_of(2) { &x.portrait601 } else { &y.e601 }),
                1 => (&y.p2020, if n.is_multiple_of(2) { &x.portrait709 } else { &y.e709 }),
                _ => (&x.portrait601, if n.is_multiple_of(2) { &y.p2020 } else { &y.e2020 }),
            };
            let canvas = if n % 4 < 2 { landscape } else { None };
            n += 1;
            let mut faded = clip(top, 0.5, 1.5, 1.0);
            faded.fade_in = fade_in;
            faded.fade_out = fade_out;
            let tl = timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0), faded]], canvas);
            run(
                &format!("checker/mx/fade in {fade_in:.3} out {fade_out:.3} @{fps} n{n}"),
                &tl,
                &[bottom.clone(), top.clone()],
                fps,
                2.0,
            );
        }
    }
    // The fade on the bottom clip, which is not converted, over a clip that is.
    let mut faded = clip(&x.portrait709, 0.5, 1.5, 0.0);
    faded.fade_in = 0.3;
    faded.fade_out = 0.3;
    let tl = timeline(vec![vec![faded, clip(&y.e601, 0.5, 1.5, 1.0)]], landscape);
    run(
        "checker/mx/fade on the bottom, a converted clip after",
        &tl,
        &[x.portrait709.clone(), y.e601.clone()],
        30.0,
        2.0,
    );
}

/// The transitions of a converted incoming clip over several frames and rates: a dip to black (a fade on a plain chain), slides and
/// pushes (a plain chain placed by `overlay`), at short and long durations.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_transitions_into_a_converted_clip_are_the_exports() {
    let x = matrix_media();
    let y = mx_media();
    let landscape = Some(Delivery::new(960, 540, Fit::Contain));
    let mut n = 0usize;
    for kind in [
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::SlideDown,
        TransitionKind::PushUp,
        TransitionKind::PushRight,
        TransitionKind::Crossfade,
    ] {
        for (duration, fps) in [(0.1, 30.0), (0.5, 25.0), (0.9, 60.0)] {
            let (first, second) = match n % 3 {
                0 => (&x.portrait709, &y.e601),
                1 => (&y.e2020, &x.portrait709),
                _ => (&x.portrait601, &y.p2020),
            };
            n += 1;
            let mut incoming = clip(second, 0.5, 1.5, 1.0);
            incoming.transition_in = Some(Transition { kind, duration });
            let tl = timeline(vec![vec![clip(first, 0.5, 1.5, 0.0), incoming]], landscape);
            run(
                &format!("checker/mx/{kind:?} {duration} @{fps} n{n}"),
                &tl,
                &[first.clone(), second.clone()],
                fps,
                2.0,
            );
        }
    }
}

/// The refusals of the cascade in an export, held: the frames the plan refuses are FFmpeg's, the rest are the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_export_refusals_are_held() {
    if policy() == CompositeColorPolicy::FixedBt601 {
        eprintln!("a fixed-matrix FFmpeg converts nothing: no refusal to hold");
        return;
    }
    let x = matrix_media();
    let landscape = Some(Delivery::new(960, 540, Fit::Contain));
    let crossfade = |mut c: Clip| {
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.4,
        });
        c
    };
    let (p709, p601) = (&x.portrait709, &x.portrait601);
    let assets = [p709.clone(), p601.clone()];
    let wide = [x.wide709.clone(), x.wide601.clone()];
    let mut cases: Vec<(&str, Timeline, Vec<Asset>)> = Vec::new();
    // A dissolving clip's own fade-out (the alpha plane), the third clip being the converted one.
    let mut converted = crossfade(clip(p601, 0.5, 1.5, 2.0));
    converted.fade_out = 0.5;
    cases.push((
        "a dissolving converted clip's own fade-out",
        timeline(
            vec![vec![
                clip(p709, 0.5, 1.5, 0.0),
                crossfade(clip(p709, 0.5, 1.5, 1.0)),
                converted,
            ]],
            landscape,
        ),
        assets.to_vec(),
    ));
    let mut converted = crossfade(clip(p601, 0.5, 1.5, 2.0));
    converted.fade_in = 0.3;
    cases.push((
        "a dissolving converted clip's own fade-in",
        timeline(
            vec![vec![
                clip(p709, 0.5, 1.5, 0.0),
                crossfade(clip(p709, 0.5, 1.5, 1.0)),
                converted,
            ]],
            landscape,
        ),
        assets.to_vec(),
    ));
    // A blurred fill behind dissolving phone clips (the ledger's T5b).
    cases.push((
        "dissolving phone clips over a blurred fill",
        timeline(
            vec![vec![
                clip(p709, 0.5, 1.5, 0.0),
                crossfade(clip(p601, 0.5, 1.5, 1.0)),
                crossfade(clip(p709, 0.5, 1.5, 2.0)),
            ]],
            Some(Delivery::new(960, 540, Fit::Blur)),
        ),
        assets.to_vec(),
    ));
    // A picture that grows: a 960x540 clip into 1280x720; dissolving.
    cases.push((
        "dissolving wide clips enlarged",
        timeline(
            vec![vec![
                clip(&x.wide709, 0.5, 1.5, 0.0),
                crossfade(clip(&x.wide601, 0.5, 1.5, 1.0)),
                crossfade(clip(&x.wide709, 0.5, 1.5, 2.0)),
            ]],
            Some(Delivery::new(1280, 720, Fit::Contain)),
        ),
        wide.to_vec(),
    ));
    // A Cover crop: the phone clips covering a landscape frame, shrunk.
    cases.push((
        "dissolving phone clips under Cover",
        timeline(
            vec![vec![
                clip(p709, 0.5, 1.5, 0.0),
                crossfade(clip(p601, 0.5, 1.5, 1.0)),
                crossfade(clip(p709, 0.5, 1.5, 2.0)),
            ]],
            Some(Delivery::new(480, 270, Fit::Cover)),
        ),
        assets.to_vec(),
    ));
    for (name, tl, assets) in cases {
        run_refusing(
            &format!("checker/mx/refused {name}"),
            &tl,
            &assets,
            30.0,
            3.0,
            &matrix_conversion_refused(),
        );
    }
}

/// A plain converted clip over a blurred fill that fades to black (the ledger's T5b, whose first clip fades in for half a second ahead of a
/// dissolving clip of the other matrix): the filled frame is faded, then converted. Plain clips only (every frame drawn), and with the
/// dissolves of the vlog (the frames the plan refuses for the blurred fill behind a dissolving layer are FFmpeg's, the rest are the export's).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_a_plain_converted_clip_over_a_blurred_fill_fades_as_the_export_does() {
    let x = matrix_media();
    let y = mx_media();
    let (p709, p601) = (&x.portrait709, &x.portrait601);
    let blur = |w, h| Some(Delivery::new(w, h, Fit::Blur));
    for (fps, w, h) in [(60.0, 960u32, 540u32), (30.0, 960, 540), (25.0, 640, 360), (60.0, 1920, 1080)] {
        // Two plain clips: the second is converted into the first's matrix, fading in and out over the fill.
        for (bottom, top) in [(p709, p601), (p601, p709), (&y.e709, &y.e601), (&y.p2020, p709)] {
            let mut faded = clip(top, 0.5, 1.5, 1.0);
            faded.fade_in = 0.4;
            faded.fade_out = 0.4;
            let tl = timeline(vec![vec![clip(bottom, 0.5, 1.5, 0.0), faded]], blur(w, h));
            let case = format!("checker/mx/blur fill, plain converted clip fading {w}x{h} @{fps}");
            if let Some(why) = mx_refusal_in(&tl, &[bottom.clone(), top.clone()], fps, 2.0) {
                eprintln!("{case}: not held, the plan refuses it: {why}");
                continue;
            }
            run(&case, &tl, &[bottom.clone(), top.clone()], fps, 2.0);
        }
    }
    // The vlog's own shape: a first clip that fades in, converted into the matrix of the first dissolving clip.
    let crossfade = |mut c: Clip| {
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        c
    };
    let mut first = clip(p709, 0.5, 1.5, 0.0);
    first.fade_in = 0.5;
    let tl = timeline(
        vec![vec![
            first,
            crossfade(clip(p601, 0.5, 1.5, 1.0)),
            crossfade(clip(p709, 0.5, 1.5, 2.0)),
        ]],
        blur(960, 540),
    );
    let case = "checker/mx/blur fill, the vlog's first clip fades in, dissolves after";
    let assets = [p709.clone(), p601.clone()];
    if policy() == CompositeColorPolicy::FixedBt601 {
        run(case, &tl, &assets, 60.0, 3.0);
    } else {
        run_refusing(case, &tl, &assets, 60.0, 3.0, &matrix_conversion_refused());
    }
}
