//! The runtime parity self-test: three small frames drawn by the compositor and by this
//! machine's own FFmpeg, and compared the way the parity harness compares them.
//!
//! Parity is proven in CI on two FFmpeg builds and one software adapter. A user has a third
//! FFmpeg (a distro's, a different major version, a build with another `swscale`) and a real
//! GPU, so the one thing that can be done about "the GPU draws what FFmpeg draws" on *their*
//! machine is to check it there, once, before the GPU path draws anything the user looks at. The
//! frames are chosen to cover what drifts between builds, not what is checked best:
//!
//! * `fit`: one untagged layer enlarged 4:3 into the canvas (swscale's bicubic tables, the final
//!   conversion to RGB);
//! * `pip`: a gradient with a shrunk, cropped, translucent, colour-graded picture-in-picture over
//!   it (the scaler down, `eq`, the opacity RGB round trip, `overlay`);
//! * `bt709`: a BT.709-tagged stack (the composite colour policy the plan was given, which is how
//!   FFmpeg 6 and 9 disagree).
//!
//! No rotation: its float sampling can differ by a level between GPUs for reasons that are not
//! drift, and a false alarm turns the feature off for someone it works for. The limits are the
//! harness's own `STRICT` ones ([`PSNR_FLAT_MIN`] and friends, held equal to `tests/parity.rs`' by
//! a test there).
//!
//! The result is a verdict for the caller to act on: [`SelfTestError::Mismatch`] (the GPU draws
//! something else than this FFmpeg: switch the GPU path off) or [`SelfTestError::Inconclusive`] (it
//! could not be checked: an FFmpeg that would not make the fixtures, a colour policy that could not
//! be measured, a deadline; the GPU path stays off). What the caller does about either, and whether
//! it asks again, is its business.
//!
//! Nothing here touches a project, a window or the preview's device state: it makes its own
//! compositor and frame source on the `Gpu` it is given, runs FFmpeg the way the interactive paths
//! do (a thread-capped child, no machine-budget slot), and takes about a second.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kerf_core::{
    composite_color_policy, export_still, Asset, Clip, Color, CompositeColorPolicy, Delivery, ExportOptions, Fit, ImageFormat,
    Project, RenderPlan, StreamKind, Timeline, Track, Transform, YuvMatrix,
};

use crate::compositor::Compositor;
use crate::frame_source::{FrameSource, FrameSourceConfig, Hint};
use crate::gpu::{Gpu, GpuError};
use crate::source::{run_child, RunError};

/// Pixels this close to a strong reference gradient are the edge band (`tests/parity.rs`' `BAND`).
pub const BAND: usize = 2;
/// A neighbour-to-neighbour step that makes an edge (`EDGE_STEP`).
pub const EDGE_STEP: i32 = 24;
/// Required PSNR over the flat region, dB (`PSNR_FLAT_MIN`).
pub const PSNR_FLAT_MIN: f64 = 40.0;
/// Allowed max per-channel error over the flat region (`MAX_FLAT`).
pub const MAX_FLAT: i32 = 8;
/// Whole-image PSNR floor, edge band included (`PSNR_ALL_MIN`).
pub const PSNR_ALL_MIN: f64 = 40.0;
/// The least share of the frame the strict check must judge (`FLAT_SHARE_MIN`).
pub const FLAT_SHARE_MIN: f64 = 0.5;

/// The whole self-test is given this long; past it nothing more is started and the answer is
/// "could not check". It is looked at between steps (before the clips and before each case), not
/// inside one: a case begun just before it still runs its reference (`export_still`, which has a 120 s
/// timeout of its own) and a conversion (30 s), so the worst case is about four minutes.
pub const DEADLINE: Duration = Duration::from_secs(90);
/// One fixture clip, one container probe, one conversion.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// The canvas the cases are drawn on, and the clips' size.
const CANVAS: (u32, u32) = (640, 360);
const CLIP: (u32, u32) = (480, 270);
/// Seconds of footage in a fixture, and the instant every case is drawn at (a frame inside it).
const CLIP_SECS: f64 = 0.4;
const AT: f64 = 0.2;

const UNTAGGED: &str = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
const BT709: &str = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";

/// What one frame measured.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseReport {
    pub name: &'static str,
    pub psnr_flat: f64,
    pub max_flat: i32,
    pub psnr_all: f64,
    /// The share of the frame outside the edge band (what the strict check judged).
    pub flat_share: f64,
    /// FFmpeg's still, and the compositor's render (decode included).
    pub ffmpeg: Duration,
    pub gpu: Duration,
}

impl CaseReport {
    /// Whether the frame is within the harness's limits.
    pub fn passed(&self) -> bool {
        self.psnr_flat >= PSNR_FLAT_MIN
            && self.max_flat <= MAX_FLAT
            && self.psnr_all >= PSNR_ALL_MIN
            && self.flat_share >= FLAT_SHARE_MIN
    }

    fn line(&self) -> String {
        format!(
            "{} {}: flat {:.1} dB max {}, all {:.1} dB, {:.0}% judged (ffmpeg {:.0} ms, gpu {:.0} ms)",
            if self.passed() { "ok" } else { "MISMATCH" },
            self.name,
            self.psnr_flat,
            self.max_flat,
            self.psnr_all,
            self.flat_share * 100.0,
            self.ffmpeg.as_secs_f64() * 1e3,
            self.gpu.as_secs_f64() * 1e3,
        )
    }
}

/// Every frame, the policy they were drawn under and how long it all took.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfTestReport {
    pub cases: Vec<CaseReport>,
    pub policy: CompositeColorPolicy,
    pub elapsed: Duration,
}

impl SelfTestReport {
    pub fn passed(&self) -> bool {
        !self.cases.is_empty() && self.cases.iter().all(CaseReport::passed)
    }

    /// One line per frame, for the log and for a failure's reason.
    pub fn summary(&self) -> String {
        let lines: Vec<String> = self.cases.iter().map(CaseReport::line).collect();
        format!(
            "{} ({:?}, {:.0} ms)",
            lines.join("; "),
            self.policy,
            self.elapsed.as_secs_f64() * 1e3
        )
    }
}

/// Why the GPU path is not to be used on this machine.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SelfTestError {
    /// The GPU drew a frame that is not FFmpeg's, outside the harness limits.
    #[error("the GPU does not draw what this FFmpeg draws: {}", .0.summary())]
    Mismatch(SelfTestReport),
    /// The check could not be made.
    #[error("the GPU could not be checked against this FFmpeg: {0}")]
    Inconclusive(String),
}

impl SelfTestError {
    fn inconclusive(why: impl Into<String>) -> Self {
        Self::Inconclusive(why.into())
    }
}

/// Run the self-test on `gpu` under this FFmpeg's measured colour policy. Blocks for about a second
/// (the FFmpeg children, the renders); a blocking thread's, never the UI's, and holds no project lock.
pub fn parity_self_test(gpu: &Arc<Gpu>) -> Result<SelfTestReport, SelfTestError> {
    let policy = composite_color_policy();
    if policy == CompositeColorPolicy::Unknown {
        return Err(SelfTestError::inconclusive(
            "this FFmpeg's composite colour policy could not be measured",
        ));
    }
    run(gpu, policy, policy, Instant::now() + DEADLINE)
}

/// [`parity_self_test`] with the policy the GPU's plans are made under given apart from the one
/// FFmpeg really has (`ffmpeg_policy`, which only decides whether a BT.709 stack is checkable):
/// the way to rehearse the drift this test is for.
fn run(
    gpu: &Arc<Gpu>,
    ffmpeg_policy: CompositeColorPolicy,
    plan_policy: CompositeColorPolicy,
    deadline: Instant,
) -> Result<SelfTestReport, SelfTestError> {
    let started = Instant::now();
    let scratch = Scratch::new().map_err(|e| SelfTestError::inconclusive(format!("no scratch directory: {e}")))?;
    let media = Media::make(scratch.path(), deadline)?;
    let cases = media.cases();

    let compositor = Compositor::new(Arc::clone(gpu)).map_err(gpu_error)?;
    let source = FrameSource::new(FrameSourceConfig::default());
    let mut reports = Vec::with_capacity(cases.len());
    for case in &cases {
        past(deadline, case.name)?;
        let plan = RenderPlan::at(&case.timeline, &case.assets, &ExportOptions::default(), AT, plan_policy)
            .map_err(|e| SelfTestError::inconclusive(format!("{}: no plan: {e}", case.name)))?;
        let size = plan.size(u32::MAX);
        let why = plan.reasons(&compositor.caps(), size);
        if !why.is_empty() {
            return Err(SelfTestError::inconclusive(format!(
                "{}: the plan refuses its own check frame: {}",
                case.name,
                why.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
            )));
        }
        if case.name == "bt709" && plan.layers.iter().any(|l| l.stream.matrix() != Some(YuvMatrix::Bt709)) {
            return Err(SelfTestError::inconclusive(
                "the probe did not see the BT.709 tag on the clip it made",
            ));
        }

        let t0 = Instant::now();
        let reference = reference_rgb(&case.timeline, &case.assets, size, scratch.path(), case.name)?;
        let ffmpeg = t0.elapsed();

        let t1 = Instant::now();
        let (frame, _) = compositor
            .render_plan_with(&plan, size, &source, Hint::Scrub)
            .map_err(gpu_error)?;
        let gpu_time = t1.elapsed();
        let gpu_rgb: Vec<u8> = frame
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        if gpu_rgb.len() != reference.len() {
            return Err(SelfTestError::inconclusive(format!(
                "{}: the GPU drew {} px and FFmpeg {}",
                case.name,
                gpu_rgb.len() / 3,
                reference.len() / 3
            )));
        }

        let m = compare(&reference, &gpu_rgb, size.0 as usize, size.1 as usize);
        reports.push(CaseReport {
            name: case.name,
            psnr_flat: m.psnr_flat,
            max_flat: m.max_flat,
            psnr_all: m.psnr_all,
            flat_share: 1.0 - m.edge_fraction,
            ffmpeg,
            gpu: gpu_time,
        });
    }
    source.release_all();
    let report = SelfTestReport {
        cases: reports,
        policy: ffmpeg_policy,
        elapsed: started.elapsed(),
    };
    if report.passed() {
        Ok(report)
    } else {
        Err(SelfTestError::Mismatch(report))
    }
}

fn run_failure(e: RunError) -> String {
    match e {
        RunError::Overflow => "it wrote more than expected".into(),
        RunError::Failed(why) => why,
    }
}

fn gpu_error(e: GpuError) -> SelfTestError {
    SelfTestError::inconclusive(format!("the GPU could not draw the check frame: {e}"))
}

fn past(deadline: Instant, before: &str) -> Result<(), SelfTestError> {
    if Instant::now() >= deadline {
        return Err(SelfTestError::inconclusive(format!(
            "ran out of time ({} s) before {before}",
            DEADLINE.as_secs()
        )));
    }
    Ok(())
}

// ---- the media and the frames ----------------------------------------------------------------

/// One frame to draw both ways.
struct Case {
    name: &'static str,
    timeline: Timeline,
    assets: Vec<Asset>,
}

/// The three fixture clips, probed the way an import probes them.
struct Media {
    testsrc: Asset,
    gradient: Asset,
    testsrc709: Asset,
}

impl Media {
    fn make(dir: &Path, deadline: Instant) -> Result<Self, SelfTestError> {
        let testsrc = format!("testsrc2=size={}x{}:rate=30:duration={CLIP_SECS},{UNTAGGED}", CLIP.0, CLIP.1);
        let gradient = format!(
            "gradients=size={0}x{1}:rate=30:duration={CLIP_SECS}:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1={0}:y1={1}:speed=0.00001,format=yuv420p,{UNTAGGED}",
            CLIP.0, CLIP.1
        );
        let testsrc709 = format!("testsrc2=size={}x{}:rate=30:duration={CLIP_SECS},{BT709}", CLIP.0, CLIP.1);
        past(deadline, "making the check clips")?;
        let made: Vec<Result<Asset, SelfTestError>> = std::thread::scope(|scope| {
            let handles: Vec<_> = [
                ("testsrc.mkv", &testsrc),
                ("gradient.mkv", &gradient),
                ("testsrc709.mkv", &testsrc709),
            ]
            .into_iter()
            .map(|(name, lavfi)| scope.spawn(move || make_clip(dir, name, lavfi)))
            .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(SelfTestError::inconclusive("a check clip's thread panicked")))
                })
                .collect()
        });
        let mut made = made.into_iter();
        let mut next = || made.next().expect("three clips");
        Ok(Self {
            testsrc: next()?,
            gradient: next()?,
            testsrc709: next()?,
        })
    }

    fn cases(&self) -> Vec<Case> {
        let clip = |a: &Asset| Clip::new(a.id, 0.0, CLIP_SECS, 0.0);
        let mut pip = clip(&self.testsrc);
        pip.transform = Transform {
            scale: 0.5,
            pos_x: 0.2,
            pos_y: -0.1,
            opacity: 0.8,
            crop_left: 0.1,
            ..Transform::default()
        };
        pip.color = Color {
            contrast: 1.15,
            saturation: 1.2,
            gamma: 1.1,
            ..Color::default()
        };
        let mut small = clip(&self.testsrc709);
        small.transform = Transform {
            scale: 0.5,
            pos_x: -0.2,
            pos_y: 0.15,
            ..Transform::default()
        };
        vec![
            Case {
                name: "fit",
                timeline: timeline(vec![vec![clip(&self.testsrc)]]),
                assets: vec![self.testsrc.clone()],
            },
            Case {
                name: "pip",
                timeline: timeline(vec![vec![clip(&self.gradient)], vec![pip]]),
                assets: vec![self.gradient.clone(), self.testsrc.clone()],
            },
            Case {
                name: "bt709",
                timeline: timeline(vec![vec![clip(&self.testsrc709)], vec![small]]),
                assets: vec![self.testsrc709.clone()],
            },
        ]
    }
}

fn timeline(tracks: Vec<Vec<Clip>>) -> Timeline {
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
        format: Some(Delivery::new(CANVAS.0, CANVAS.1, Fit::Contain)),
        master: Default::default(),
    }
}

/// A lossless clip of `lavfi` (FFV1 in Matroska: ffmpeg's own encoder, an indexed container, every
/// frame a keyframe), probed as an import would.
fn make_clip(dir: &Path, name: &str, lavfi: &str) -> Result<Asset, SelfTestError> {
    let path = dir.join(name);
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-f", "lavfi", "-i"]
        .into_iter()
        .map(String::from)
        .collect();
    args.push(lavfi.into());
    args.extend(["-c:v", "ffv1", "-pix_fmt", "yuv420p"].map(String::from));
    args.push(path.to_string_lossy().into_owned());
    kerf_core::limit_frame_args(&mut args, 3);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(&args);
    run_child(cmd, 1 << 16, STEP_TIMEOUT)
        .map_err(|e| SelfTestError::inconclusive(format!("could not make {name}: {}", run_failure(e))))?;
    Project::probe_asset(&path).map_err(|e| SelfTestError::inconclusive(format!("could not probe {name}: {e}")))
}

/// FFmpeg's own still of `timeline` at [`AT`], as packed RGB at `size`.
fn reference_rgb(
    timeline: &Timeline,
    assets: &[Asset],
    size: (u32, u32),
    dir: &Path,
    name: &str,
) -> Result<Vec<u8>, SelfTestError> {
    let png = dir.join(format!("{name}.png"));
    export_still(timeline, assets, &ExportOptions::default(), AT, &png, ImageFormat::Png, 0)
        .map_err(|e| SelfTestError::inconclusive(format!("{name}: FFmpeg's still failed: {e}")))?;
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-i"]
        .into_iter()
        .map(String::from)
        .collect();
    args.push(png.to_string_lossy().into_owned());
    args.extend(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"].map(String::from));
    kerf_core::limit_frame_args(&mut args, 1);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(&args);
    let rgb = run_child(cmd, size.0 as usize * size.1 as usize * 3 + 1, STEP_TIMEOUT)
        .map_err(|e| SelfTestError::inconclusive(format!("{name}: could not read FFmpeg's still back: {}", run_failure(e))))?;
    if rgb.len() != size.0 as usize * size.1 as usize * 3 {
        return Err(SelfTestError::inconclusive(format!(
            "{name}: FFmpeg's still is {} px, not {}x{}",
            rgb.len() / 3,
            size.0,
            size.1
        )));
    }
    Ok(rgb)
}

/// A scratch directory under the temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kerf-gpu-selftest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- the comparison (tests/parity.rs' `compare`, with the strict limits) ---------------------

/// What [`compare`] measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub psnr_flat: f64,
    pub max_flat: i32,
    pub mean_flat: f64,
    pub psnr_all: f64,
    pub max_all: i32,
    /// The share of pixels in the edge band.
    pub edge_fraction: f64,
}

fn psnr(sq_err: f64, n: usize) -> f64 {
    if sq_err == 0.0 || n == 0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / (sq_err / n as f64)).log10()
}

/// The pixels within [`BAND`] of a strong gradient in `reference` (packed RGB).
fn edge_band(reference: &[u8], w: usize, h: usize) -> Vec<bool> {
    let px = |x: usize, y: usize, c: usize| i32::from(reference[(y * w + x) * 3 + c]);
    let mut seed = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut step = 0;
            for c in 0..3 {
                if x + 1 < w {
                    step = step.max((px(x + 1, y, c) - px(x, y, c)).abs());
                }
                if y + 1 < h {
                    step = step.max((px(x, y + 1, c) - px(x, y, c)).abs());
                }
            }
            if step > EDGE_STEP {
                // A step is between two pixels; both are on the edge.
                seed[y * w + x] = true;
                if x + 1 < w {
                    seed[y * w + x + 1] |= (0..3).any(|c| (px(x + 1, y, c) - px(x, y, c)).abs() > EDGE_STEP);
                }
                if y + 1 < h {
                    seed[(y + 1) * w + x] |= (0..3).any(|c| (px(x, y + 1, c) - px(x, y, c)).abs() > EDGE_STEP);
                }
            }
        }
    }
    let mut wide = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if seed[y * w + x] {
                for xx in x.saturating_sub(BAND)..=(x + BAND).min(w - 1) {
                    wide[y * w + xx] = true;
                }
            }
        }
    }
    let mut band = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if wide[y * w + x] {
                for yy in y.saturating_sub(BAND)..=(y + BAND).min(h - 1) {
                    band[yy * w + x] = true;
                }
            }
        }
    }
    band
}

/// Hold `gpu_rgb` to `reference` (both packed RGB, `w` x `h`): PSNR and the worst error over the
/// pixels outside the edge band, and the whole image's PSNR.
pub fn compare(reference: &[u8], gpu_rgb: &[u8], w: usize, h: usize) -> Metrics {
    assert!(
        reference.len() == w * h * 3 && gpu_rgb.len() == w * h * 3,
        "two {w}x{h} RGB pictures"
    );
    let band = edge_band(reference, w, h);
    let (mut se_flat, mut n_flat, mut sum_flat, mut max_flat) = (0.0f64, 0usize, 0.0f64, 0i32);
    let (mut se_all, mut max_all) = (0.0f64, 0i32);
    for (i, &in_band) in band.iter().enumerate() {
        for c in 0..3 {
            let d = i32::from(reference[i * 3 + c]) - i32::from(gpu_rgb[i * 3 + c]);
            let sq = f64::from(d * d);
            se_all += sq;
            max_all = max_all.max(d.abs());
            if !in_band {
                se_flat += sq;
                sum_flat += f64::from(d.abs());
                n_flat += 1;
                max_flat = max_flat.max(d.abs());
            }
        }
    }
    Metrics {
        psnr_flat: psnr(se_flat, n_flat),
        max_flat,
        mean_flat: if n_flat == 0 { 0.0 } else { sum_flat / n_flat as f64 },
        psnr_all: psnr(se_all, w * h * 3),
        max_all,
        edge_fraction: band.iter().filter(|b| **b).count() as f64 / (w * h) as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuOptions;

    /// A picture with a smooth ramp and one hard vertical edge.
    fn picture(w: usize, h: usize) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let v = (40 + (x * 3 + y) % 60) as u8 + if x >= w / 2 { 100 } else { 0 };
                [v, v / 2 + 20, 255 - v]
            })
            .collect()
    }

    fn judged(reference: &[u8], other: &[u8], w: usize, h: usize) -> CaseReport {
        let m = compare(reference, other, w, h);
        CaseReport {
            name: "t",
            psnr_flat: m.psnr_flat,
            max_flat: m.max_flat,
            psnr_all: m.psnr_all,
            flat_share: 1.0 - m.edge_fraction,
            ffmpeg: Duration::ZERO,
            gpu: Duration::ZERO,
        }
    }

    #[test]
    fn the_same_picture_passes_and_a_level_of_noise_does_too() {
        let (w, h) = (64, 48);
        let a = picture(w, h);
        assert!(judged(&a, &a, w, h).passed());
        let noisy: Vec<u8> = a.iter().enumerate().map(|(i, v)| v.saturating_add((i % 3) as u8)).collect();
        let r = judged(&a, &noisy, w, h);
        assert!(r.passed() && r.max_flat <= 2, "{r:?}");
    }

    #[test]
    fn a_picture_that_is_off_by_a_tint_or_a_row_fails() {
        let (w, h) = (64, 48);
        let a = picture(w, h);
        let tinted: Vec<u8> = a.chunks(3).flat_map(|p| [p[0].saturating_add(20), p[1], p[2]]).collect();
        let r = judged(&a, &tinted, w, h);
        assert!(!r.passed() && r.max_flat > MAX_FLAT, "{r:?}");

        // The whole picture a row down: the flat check mostly misses it, the whole-image floor does not.
        let mut shifted = vec![0u8; a.len()];
        shifted[w * 3..].copy_from_slice(&a[..a.len() - w * 3]);
        shifted[..w * 3].copy_from_slice(&a[..w * 3]);
        let r = judged(&a, &shifted, w, h);
        assert!(!r.passed(), "{r:?}");
    }

    #[test]
    fn a_hard_edge_a_pixel_off_is_excused_by_the_band_but_a_flat_area_is_not() {
        let (w, h) = (64, 48);
        let edge = |at: usize| -> Vec<u8> {
            (0..w * h)
                .flat_map(|i| if i % w < at { [30, 30, 30] } else { [220, 220, 220] })
                .collect()
        };
        // The band keeps the flat check off the edge itself (the whole-image floor still sees it).
        let r = judged(&edge(32), &edge(33), w, h);
        assert!(r.max_flat == 0 && r.psnr_flat >= PSNR_FLAT_MIN, "{r:?}");
        assert!(r.psnr_all < PSNR_ALL_MIN && !r.passed(), "{r:?}");
        // The same error in the flat part is judged.
        let mut off = edge(32);
        for p in off.chunks_mut(3).take(w * 10) {
            p[0] = p[0].saturating_add(12);
        }
        let r = judged(&edge(32), &off, w, h);
        assert!(r.max_flat > MAX_FLAT && !r.passed(), "{r:?}");
    }

    #[test]
    fn a_picture_that_is_all_edge_has_nothing_the_strict_check_can_judge() {
        let (w, h) = (32, 32);
        let checker: Vec<u8> = (0..w * h)
            .flat_map(|i| if (i % w + i / w) % 2 == 0 { [235; 3] } else { [16; 3] })
            .collect();
        let r = judged(&checker, &checker, w, h);
        assert!(r.flat_share < FLAT_SHARE_MIN && !r.passed(), "{r:?}");
    }

    #[test]
    fn a_report_is_passed_only_when_it_has_frames_and_every_one_passed() {
        let ok = CaseReport {
            name: "ok",
            psnr_flat: 50.0,
            max_flat: 2,
            psnr_all: 45.0,
            flat_share: 0.9,
            ffmpeg: Duration::ZERO,
            gpu: Duration::ZERO,
        };
        let bad = CaseReport {
            name: "bad",
            max_flat: 30,
            ..ok
        };
        let report = |cases: Vec<CaseReport>| SelfTestReport {
            cases,
            policy: CompositeColorPolicy::FixedBt601,
            elapsed: Duration::ZERO,
        };
        assert!(report(vec![ok.clone()]).passed());
        assert!(!report(vec![]).passed());
        let mixed = report(vec![ok, bad]);
        assert!(!mixed.passed());
        assert!(mixed.summary().contains("MISMATCH bad"), "{}", mixed.summary());
        assert!(SelfTestError::Mismatch(mixed).to_string().contains("MISMATCH bad"));
    }

    fn adapter() -> Arc<Gpu> {
        Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)")
    }

    /// The tests that run the check share a process, and so the scratch directories' name: one at a
    /// time keeps the leftovers check to its own.
    fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    #[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_and_this_adapter_pass_their_own_self_test() {
        let _one = one_at_a_time();
        let gpu = adapter();
        let report = parity_self_test(&gpu).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.cases.len(), 3);
        eprintln!("self-test: {}", report.summary());
        assert!(report.elapsed < Duration::from_secs(30), "{:?}", report.elapsed);
        // The scratch directory is gone.
        let leftovers = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with(&format!("kerf-gpu-selftest-{}-", std::process::id())))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    #[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
    #[allow(clippy::print_stderr)]
    fn a_gpu_drawn_under_the_wrong_colour_policy_fails_it() {
        let _one = one_at_a_time();
        let gpu = adapter();
        let real = composite_color_policy();
        let wrong = match real {
            CompositeColorPolicy::FixedBt601 => CompositeColorPolicy::BottomLayerTag,
            CompositeColorPolicy::BottomLayerTag => CompositeColorPolicy::FixedBt601,
            CompositeColorPolicy::Unknown => panic!("the colour probe could not read this FFmpeg"),
        };
        match run(&gpu, real, wrong, Instant::now() + DEADLINE) {
            Err(SelfTestError::Mismatch(report)) => {
                eprintln!("drift: {}", report.summary());
                let failed: Vec<_> = report.cases.iter().filter(|c| !c.passed()).map(|c| c.name).collect();
                assert_eq!(failed, ["bt709"], "{}", report.summary());
            }
            other => panic!("expected a mismatch on the BT.709 frame, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
    fn a_deadline_that_has_passed_is_could_not_check_and_leaves_nothing_behind() {
        let _one = one_at_a_time();
        let gpu = adapter();
        let policy = composite_color_policy();
        let err = run(&gpu, policy, policy, Instant::now()).unwrap_err();
        assert!(
            matches!(&err, SelfTestError::Inconclusive(why) if why.contains("ran out of time")),
            "{err}"
        );
    }
}
