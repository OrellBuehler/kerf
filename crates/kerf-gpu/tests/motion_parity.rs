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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    fade_alpha, fade_chroma, fade_luma, Asset, Clip, Color, CompositeColorPolicy, Container, Delivery, ExportOptions, FadeEdge,
    FadeStep, FadeTint, Fit, PlanMode, PlanRequest, Planner, Project, RateControl, StreamKind, Timeline, Track, Transition,
    TransitionKind, FADE_FULL,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames, YuvFrame};

// ---- the machinery -------------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| Compositor::new(gpu()).expect("a compositor"))
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
    /// cases with a rotated layer.
    smooth: Asset,
    black: Asset,
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
        markers: Vec::new(),
        format,
        master: Default::default(),
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

/// Two levels: a rotated layer, inside its edge. `rotate` is a fixed-point bilinear sample and the
/// shader a float one (the scaled picture before it is one level off on its own), and the same
/// two levels show whether the layer fades, dissolves or is keyed.
const ROTATED: Limits = Limits { y: 2, uv: 2 };

/// Two levels: a layer the compositor scales *and* grades. The scaler's one level is scaled by the
/// `eq` contrast (1.1) and rounded again.
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
fn montage(case: &str, k: u64, played: &Played, plan: &kerf_core::RenderPlan, frames: &[Option<Arc<YuvFrame>>]) {
    let (w, h) = (played.w, played.h);
    let reference = played.rgb(k as usize);
    let gpu = compositor()
        .composite_shared(plan, frames, (w as u32, h as u32))
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
    /// A rotated layer's edge is a fixed-point stair-step in FFmpeg and a float sample here, and
    /// its chroma there has a fringe the compositor does not copy (`tests/parity.rs`,
    /// `a_rotated_neutral_layer_has_no_colour_fringe`): the samples within [`BAND`] of the edge of a
    /// rotated layer's picture (its footprint, from the plan's geometry: a content-based band
    /// vanishes where a fade makes the edge faint) are left out. Everything inside is compared.
    rotated: bool,
    /// The frames the plan refuses for this reason (FFmpeg draws them) are not compared, and at
    /// least one must be: the case is about a refusal. Every other frame is compared with the
    /// export, so a refusal that is wider than it has to be shows too. A frame the plan refuses
    /// for another reason is a failure.
    refusing: Option<&'a str>,
}

/// Samples this close to the edge of a rotated layer are the edge band.
const BAND: f64 = 3.0;

/// The pixels within [`BAND`] of the edge of a rotated layer's picture, from the plan's geometry
/// (`rot_sample`'s convention: the layer box's centre, the picture rotated about it).
fn rotated_band(plan: &kerf_core::RenderPlan, (w, h): (u32, u32)) -> Vec<bool> {
    let (w, h) = (w as usize, h as usize);
    let mut band = vec![false; w * h];
    for layer in &plan.layers {
        let Ok(g) = plan.layer_geometry(layer, (w as u32, h as u32)) else {
            continue;
        };
        let Some(r) = g.rotation else { continue };
        let (lw, lh) = g.layer;
        let (pw, ph) = (f64::from(g.picture.0), f64::from(g.picture.1));
        let (c, s) = (r.angle.cos(), r.angle.sin());
        for y in 0..h {
            for x in 0..w {
                let dx = x as f64 - f64::from(g.origin.0) - (f64::from(lw) - 1.0) / 2.0;
                let dy = y as f64 - f64::from(g.origin.1) - (f64::from(lh) - 1.0) / 2.0;
                let (sx, sy) = (c * dx + s * dy, -s * dx + c * dy);
                let (ex, ey) = (pw / 2.0 - sx.abs(), ph / 2.0 - sy.abs());
                let near = if ex >= 0.0 && ey >= 0.0 {
                    ex.min(ey)
                } else {
                    (-ex).max(0.0).hypot((-ey).max(0.0))
                };
                if near <= BAND {
                    band[y * w + x] = true;
                }
            }
        }
    }
    band
}

/// [`PlaneStats::of`] over the samples `keep` allows.
fn stats_outside(a: &[u8], b: &[u8], excluded: &[bool]) -> PlaneStats {
    let mut s = PlaneStats::default();
    for ((x, y), out) in a.iter().zip(b).zip(excluded) {
        if *out {
            continue;
        }
        let d = (i32::from(*x) - i32::from(*y)).abs();
        s.n += 1;
        s.max = s.max.max(d);
        s.differing += usize::from(d != 0);
        s.sum += d as u64;
        s.sq += (d * d) as u64;
    }
    s
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
        rotated,
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
    let mut excluded_share = 0.0f64;
    let mut refused_frames = 0usize;
    for k in frames.clone() {
        let plan = planner.at_frame(k).expect("plan");
        assert_eq!((plan.canvas.width, plan.canvas.height), (played.w as u32, played.h as u32));
        let why = plan.reasons(&compositor().caps(), size);
        if refusing.is_some_and(|r| why.iter().any(|w| w.to_string().contains(r))) {
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
            .composite_yuv_shared(&plan, &picture, size)
            .unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let want = played.planes(k as usize);
        let got = planes_of(&drawn);
        let (y, u, v) = if rotated {
            let band = rotated_band(&plan, size);
            // A chroma sample is out when any pixel of its block is, and so is its neighbour's.
            let (cw, ch) = (played.w / 2, played.h / 2);
            let block = |r: usize, c: usize| {
                [(0, 0), (1, 0), (0, 1), (1, 1)]
                    .iter()
                    .any(|(dx, dy)| band[(2 * r + dy) * played.w + 2 * c + dx])
            };
            let wide: Vec<bool> = (0..cw * ch)
                .map(|i| {
                    let (r, c) = (i / cw, i % cw);
                    (r.saturating_sub(1)..=(r + 1).min(ch - 1))
                        .any(|rr| (c.saturating_sub(1)..=(c + 1).min(cw - 1)).any(|cc| block(rr, cc)))
                })
                .collect();
            excluded_share = excluded_share.max(band.iter().filter(|b| **b).count() as f64 / band.len() as f64);
            (
                stats_outside(want[0], got[0], &band),
                stats_outside(want[1], got[1], &wide),
                stats_outside(want[2], got[2], &wide),
            )
        } else {
            (
                PlaneStats::of(want[0], got[0]),
                PlaneStats::of(want[1], got[1]),
                PlaneStats::of(want[2], got[2]),
            )
        };
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
            montage(case, k, &played, &plan, &picture);
        }
    }
    let uv = {
        let mut s = us;
        s.add(vs);
        s
    };
    let banded = if rotated {
        format!(" | edge band <= {:.1}% left out", excluded_share * 100.0)
    } else {
        String::new()
    };
    if let Some(reason) = refusing {
        assert!(refused_frames > 0, "{case}: no frame was refused for `{reason}`");
    }
    let banded = match refusing {
        Some(reason) => format!("{banded} | {refused_frames} frames refused for `{reason}`, FFmpeg draws them"),
        None => banded,
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
        rotated: false,
        refusing: None,
    });
}

/// [`run`] for a case about a refusal: the frames the plan refuses for `reason` are FFmpeg's, the
/// others are compared with the export as usual.
fn run_refusing(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, limits: Limits, reason: &str) {
    check_motion(Check {
        case,
        tl,
        assets,
        opts: at_fps(fps),
        frames: upto(secs, fps),
        limits,
        look: &[],
        rotated: false,
        refusing: Some(reason),
    });
}

/// [`run`] for a case with a rotated layer: the samples within [`BAND`] of its edge are left out.
fn run_rotated(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, limits: Limits) {
    check_motion(Check {
        case,
        tl,
        assets,
        opts: at_fps(fps),
        frames: upto(secs, fps),
        limits,
        look: &look_at(secs, fps),
        rotated: true,
        refusing: None,
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
    run(
        "keyed/position",
        &tl,
        &[m.black.clone(), m.testsrc.clone()],
        30.0,
        1.0,
        SCALED,
    );
    // The easing of a segment is the polyline both renderers read (`Clip::transform_at`): eased,
    // held and a bezier move the picture to the same pixels.
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
            SCALED,
        );
    }
    let mut turning = clip(&m.smooth, 0.5, 1.5, 0.0);
    turning.keyframes = vec![key(0.0, 0.0, 0.0), key(1.0, 0.0, 30.0)];
    let tl = timeline(vec![vec![under], vec![turning]], None);
    run_rotated(
        "keyed/rotation",
        &tl,
        &[m.black.clone(), m.smooth.clone()],
        30.0,
        1.0,
        ROTATED,
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
    run_rotated("fade/rotated", &tl, &assets, 30.0, 1.0, ROTATED);
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
    run_rotated("dissolve/rotated-incoming", &tl, &assets, 30.0, 2.0, ROTATED);
    // A rotated clip with no fade at all, as an export draws it.
    let mut still_turned = clip(&m.smooth, 0.5, 1.5, 0.0);
    still_turned.transform.rotation = 20.0;
    still_turned.transform.scale = 0.6;
    let tl = timeline(vec![vec![under], vec![still_turned]], None);
    run_rotated("plain/rotated", &tl, &assets, 30.0, 1.0, ROTATED);
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
    run_refusing("fade/translucent", &tl, &assets, 30.0, 1.0, TRANSLUCENT, "below full opacity");
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
        "below full opacity",
    );
}

// ---- other pictures: pixel formats, tags, full range -----------------------------------------

/// The test pattern in the pictures a dip to white or a fade treats differently: what the round
/// trip of a dip was not measured on, what FFmpeg 9 fades before converting its range, a BT.709 tag.
struct Variants {
    tenbit: Asset,
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
        let x264 = ["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
        let still = ["-frames:v", "1"];
        Variants {
            tenbit: make("tenbit.mp4", ts, "yuv420p10le", &x264),
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

const NOT_MEASURED: &str = "not 8-bit limited-range";

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
        run_refusing(&format!("dip/white {name}"), &tl, &assets, 30.0, 2.0, EXACT, "scaler resizes");
        let (tl, assets) = after_black(&m.testsrc, black, 0.8, scale, format);
        run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0, SCALED);
    }
    // Refused: a letterboxed layer.
    let tall = Some(Delivery::new(640, 360, Fit::Contain));
    let (tl, assets) = after_black(&m.portrait, white, 0.8, 1.0, tall);
    run_refusing("dip/white letterboxed", &tl, &assets, 30.0, 2.0, EXACT, "letterboxed");
    let (tl, assets) = after_black(&m.portrait, black, 0.8, 1.0, tall);
    run("dip/black letterboxed", &tl, &assets, 30.0, 2.0, SCALED);
    // ... a layer of odd size (0.4985 of 640x360 is 319x179), which a dip to black draws.
    let (tl, assets) = after_black(&m.testsrc, white, 0.8, 0.4985, None);
    run_refusing("dip/white odd layer", &tl, &assets, 30.0, 2.0, EXACT, "odd size 319x179");
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
        let (tl, assets) = after_black(a, white, 0.8, 1.0, None);
        run_refusing(&format!("dip/white {name}"), &tl, &assets, 30.0, 2.0, EXACT, NOT_MEASURED);
        let (tl, assets) = after_black(a, black, 0.8, 1.0, None);
        run(&format!("dip/black {name}"), &tl, &assets, 30.0, 2.0, EXACT);
    }
}

/// Under FFmpeg 6 a full-range (`yuvj420p`) picture is converted to limited range at the decode, as
/// the compositor does, and everything below is drawn to the level. FFmpeg 9 keeps its raw
/// values through the graph (the export's planes hold them: 17 levels of Y and 16 of UV off a
/// compositor that converts first, in every frame, and the stream is tagged full range as a
/// whole) and `fade` darkens them toward the limited-range black 16 before the range is converted
/// (frame 0 of a fade-in reads Y 30 where black is 16), so the plan refuses the picture in an
/// export frame and inside a fade, a dip or a dissolve on that FFmpeg.
fn run_full_range(case: &str, tl: &Timeline, assets: &[Asset], fps: f64, secs: f64, limits: Limits) {
    match policy() {
        CompositeColorPolicy::FixedBt601 => run(case, tl, assets, fps, secs, limits),
        _ => run_refusing(case, tl, assets, fps, secs, limits, "full-range picture"),
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
            CompositeColorPolicy::FixedBt601 => NOT_MEASURED,
            _ => "full-range picture",
        };
        run_refusing(
            &format!("full-range/dip-white {name}"),
            &tl,
            &[a.clone(), a.clone()],
            30.0,
            2.0,
            EXACT,
            reason,
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
        "full-range picture",
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
