//! A 10-bit delivery (`yuv420p10le`, `yuv422p10le`: ProRes 422's) against the compositor: the planes the export hands its encoder, plane for plane,
//! sample for sample, **no tolerance** -- and every refusal the plan makes of what the 10-bit chain was not measured for.
//!
//! The export's frames are the graph's terminal planes (the engine's own argv, `kerf_core::export_args`, its encoder swapped for a raw pipe: x264
//! at 10 bits is lossy at `-crf 0`), at an explicit number of filter threads, which the compositor is told (`with_graph_threads`): on FFmpeg 6.1 the
//! dither of a 4:2:0 picture restarts at each slice of the scaler's threads. `kerf_gpu::delivery10` has the chain, the model of it and the probe
//! that measures this FFmpeg's against the model; `checker_delivery10_model.rs` holds the model to the export over many sizes and thread counts,
//! this file holds the shader passes to the export.
//!
//! Needs `ffmpeg` and an adapter (lavapipe is enough); `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test delivery10_parity -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

mod common;
#[path = "common/ten_bit.rs"]
mod ten_bit;

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::model::{Color, Transition, TransitionKind, VideoEffect};
use kerf_core::{
    Asset, Clip, CompositeColorPolicy, Container, Delivery, ExportOptions, Fit, PlanRequest, Planner, Project, RateControl,
    StreamKind, TenBitKinds, TenBitLayout, TextOverlay, Timeline, Track, Unsupported,
};
use kerf_gpu::delivery10::{self, Planes10};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

/// The measurements a 10-bit frame is judged by, made once: this FFmpeg's scaler, the layout of its `drawtext` and its 10-bit export.
fn measured() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::text::model();
        let verdict = delivery10::ten_bit();
        eprintln!(
            "this FFmpeg's 10-bit export: {verdict:?}, {:?} {:?}",
            delivery10::measured_kinds(),
            delivery10::details()
        );
    });
}

/// A compositor that draws as the graph with `threads` slice threads does.
fn compositor(threads: u32) -> &'static Compositor {
    static COMPOSITORS: Mutex<Option<HashMap<u32, &'static Compositor>>> = Mutex::new(None);
    measured();
    let mut held = COMPOSITORS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let held = held.get_or_insert_with(HashMap::new);
    held.entry(threads).or_insert_with(|| {
        Box::leak(Box::new(
            Compositor::new(gpu()).expect("a compositor").with_graph_threads(threads),
        ))
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
        "the composite colour probe could not read this FFmpeg ({})",
        kerf_core::ffmpeg_path()
    );
    measured
}

// ---- media -----------------------------------------------------------------------------

struct Media {
    /// 640x360 test pattern with a running frame counter, 30 fps.
    testsrc: Asset,
    /// Colour bars, 30 fps.
    bars: Asset,
    /// Full-range noise in all three planes: every Y, U, V combination, below black and above white included.
    noise: Asset,
    /// 1280x720 at 25 fps: a clip in a 1080p, 30 fps frame (the coverage ledger's teaser).
    hd: Asset,
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = common::media_dir("delivery10-media");
        let video = |name: &str, lavfi: &str| -> Asset {
            let p: PathBuf = dir.join(name);
            if !p.exists() {
                common::ffmpeg(&[
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
            }
            Project::probe_asset(&p).unwrap_or_else(|e| panic!("probe {}: {e}", p.display()))
        };
        let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        Media {
            testsrc: video("testsrc.mp4", &format!("testsrc2=size=640x360:rate=30:duration=3,{untagged}")),
            bars: video("bars.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=3,{untagged}")),
            noise: video(
                "noise.mp4",
                "nullsrc=size=640x360:rate=30:duration=3,format=yuv420p,geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29+N*5,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3+N*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17+N*7,256)'",
            ),
            hd: video("hd.mp4", &format!("testsrc2=size=1280x720:rate=25:duration=3,{untagged}")),
        }
    })
}

fn timeline(tracks: Vec<Vec<Clip>>, overlays: Vec<TextOverlay>, canvas: (u32, u32), fit: Fit, fps: f64) -> Timeline {
    Timeline {
        tracks: tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| Track {
                clips,
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            })
            .collect(),
        overlays,
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(canvas.0, canvas.1, fit).with_fps(Some(fps)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

fn clip(a: &Asset, source_in: f64, dur: f64, start: f64) -> Clip {
    Clip::new(a.id, source_in, source_in + dur, start)
}

fn options(layout: TenBitLayout, fps: f64) -> ExportOptions {
    ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        fps: Some(fps),
        pix_fmt: Some(layout.pix_fmt().into()),
        ..ExportOptions::default()
    }
}

// ---- the comparison ----------------------------------------------------------------------

/// Every sample compared and every one that differed, over a case.
#[derive(Default)]
struct Tally {
    compared: usize,
    frames: usize,
}

/// The export's frames `frames` of `tl` in `layout` on a graph of `threads` slice threads, against the compositor's: 0 differing samples in
/// Y, U and V of every frame, or the case fails. Returns the samples compared.
fn compare(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    layout: TenBitLayout,
    threads: u32,
    fps: f64,
    frames: Range<u64>,
) -> Tally {
    compare_ranges(case, tl, assets, layout, threads, fps, std::slice::from_ref(&frames))
}

/// [`compare`] over several runs of frames of one export (a long cut's interesting stretches), the export read as it is written.
fn compare_ranges(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    layout: TenBitLayout,
    threads: u32,
    fps: f64,
    ranges: &[Range<u64>],
) -> Tally {
    let opts = options(layout, fps);
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let first = planner.at_frame(0).expect("plan");
    let (w, h) = (first.canvas.width as usize, first.canvas.height as usize);
    let (cw, ch) = (w / 2, if layout == TenBitLayout::P422 { h } else { h / 2 });
    let frame_samples = w * h + 2 * cw * ch;
    let upto = ranges.iter().map(|r| r.end).max().unwrap_or(1) - 1;
    let wanted = |k: u64| ranges.iter().any(|r| r.contains(&k));
    let exported = ten_bit::export_frames(tl, assets, &opts, Some(threads), upto, frame_samples * 2, &wanted);
    let comp = compositor(threads);
    let caps = comp.caps();
    let mut tally = Tally::default();
    for frames in ranges {
        let mut motion = MotionFrames::new(frame_source());
        for k in frames.clone() {
            let plan = planner.at_frame(k).expect("plan");
            let size = (plan.canvas.width, plan.canvas.height);
            let why = plan.reasons(&caps, size);
            assert!(
                why.is_empty(),
                "{case}: {} frame {k} on {threads} threads: the plan says the GPU cannot draw this: {why:?}",
                layout.pix_fmt()
            );
            let bytes = exported
                .get(&k)
                .unwrap_or_else(|| panic!("{case}: the export has no frame {k}"));
            let want = ten_bit::samples(bytes);
            let plan = plan.for_decode(&caps, size);
            let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
            let drawn: Planes10 = comp
                .composite_yuv10_counted(&plan, &picture, motion.counts(), size)
                .unwrap_or_else(|e| panic!("{case}: {} frame {k}: {e}", layout.pix_fmt()));
            let mut at = 0;
            for (name, plane) in [("Y", &drawn.y), ("U", &drawn.u), ("V", &drawn.v)] {
                let reference = &want[at..at + plane.data.len()];
                let apart = plane.data.iter().zip(reference).filter(|(a, b)| a != b).count();
                if apart != 0 {
                    let first = plane.data.iter().zip(reference).position(|(a, b)| a != b).unwrap_or(0);
                    panic!(
                        "{case}: {} frame {k} on {threads} threads: {name} differs from the export in {apart} of {} samples (the first at ({}, {}): the compositor's {}, FFmpeg's {})",
                        layout.pix_fmt(),
                        plane.data.len(),
                        first % plane.w,
                        first / plane.w,
                        plane.data[first],
                        reference[first]
                    );
                }
                at += plane.data.len();
                tally.compared += plane.data.len();
            }
            tally.frames += 1;
        }
    }
    eprintln!(
        "{case:<48} {} on {threads} thread(s): frames {} | {} samples, 0 differ",
        layout.pix_fmt(),
        tally.frames,
        tally.compared
    );
    tally
}

fn both(case: &str, tl: &Timeline, assets: &[Asset], threads: &[u32], fps: f64, frames: Range<u64>) -> usize {
    let mut compared = 0;
    for layout in [TenBitLayout::P420, TenBitLayout::P422] {
        for &n in threads {
            compared += compare(case, tl, assets, layout, n, fps, frames.clone()).compared;
        }
    }
    compared
}

/// This FFmpeg's 10-bit export is the model, or the case that follows is not about it.
fn require_ten_bit() {
    measured();
    let kinds = delivery10::measured_kinds();
    assert!(
        kinds.contains(TenBitKinds::P420) && kinds.contains(TenBitKinds::P422),
        "this FFmpeg's 10-bit export is not the model ({:?}: {:?})",
        delivery10::measured(),
        delivery10::details()
    );
}

// ---- the probe -----------------------------------------------------------------------------

/// The probe itself, on the FFmpeg in use: it must be able to tell, and say the same twice.
#[test]
#[ignore = "needs ffmpeg"]
fn this_ffmpegs_ten_bit_export_is_the_model_or_the_probe_says_where_not() {
    let first = delivery10::ten_bit();
    assert!(!matches!(first, delivery10::TenBit::Unknown), "the probe could not run");
    assert_eq!(first, delivery10::ten_bit());
    eprintln!(
        "the 10-bit export: {first:?}, {:?}, {:?}",
        delivery10::measured_kinds(),
        delivery10::details()
    );
    require_ten_bit();
}

// ---- the cases -----------------------------------------------------------------------------

/// A clip in a bigger frame with fades in and out and a title, and the same clip as it is: each layout, on one and several slice threads.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_ten_bit_delivery_is_drawn_as_the_export_hands_it_to_the_encoder() {
    require_ten_bit();
    let m = media();
    let mut compared = 0;
    // The teaser's shape: a clip enlarged 1.5x into a Cover frame, fading in and out, with a title.
    let mut c = clip(&m.testsrc, 0.0, 0.5, 0.0);
    c.fade_in = 0.2;
    c.fade_out = 0.2;
    let title = TextOverlay::new("Out now", 0.1, 0.4);
    let tl = timeline(vec![vec![c]], vec![title], (960, 540), Fit::Cover, 30.0);
    compared += both(
        "enlarged, faded, titled",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[3],
        30.0,
        0..15,
    );
    // A cut between two clips (the second fades out), the frame the size of the picture: the format conversion and the fades alone.
    let a = clip(&m.testsrc, 0.5, 0.4, 0.0);
    let mut b = clip(&m.bars, 0.0, 0.4, 0.4);
    b.fade_out = 0.3;
    let tl = timeline(vec![vec![a, b]], vec![], (640, 360), Fit::Cover, 30.0);
    compared += both(
        "a cut, the size of the picture",
        &tl,
        &[m.testsrc.clone(), m.bars.clone()],
        &[1, 4],
        30.0,
        0..24,
    );
    // Noise shrunk to half, the fade-in running: full-range samples, clipped.
    let mut n = clip(&m.noise, 0.0, 0.4, 0.0);
    n.fade_in = 0.3;
    let tl = timeline(vec![vec![n]], vec![], (320, 180), Fit::Cover, 30.0);
    compared += both(
        "noise shrunk by two",
        &tl,
        std::slice::from_ref(&m.noise),
        &[2, 5],
        30.0,
        0..12,
    );
    // Fades that overlap on a short clip (the fade-out runs on the fade-in's last frames), a clip at double speed and one played backwards.
    let mut o = clip(&m.testsrc, 0.0, 0.4, 0.0);
    o.fade_in = 0.3;
    o.fade_out = 0.3;
    let mut fast = clip(&m.testsrc, 0.5, 0.5, 0.4);
    fast.speed = 2.0;
    let mut back = clip(&m.bars, 0.0, 0.4, 0.65);
    back.speed = -1.0;
    let tl = timeline(vec![vec![o, fast, back]], vec![], (640, 360), Fit::Cover, 30.0);
    compared += both(
        "overlapping fades, 2x, reversed",
        &tl,
        &[m.testsrc.clone(), m.bars.clone()],
        &[3],
        30.0,
        0..30,
    );
    // Two tracks: the clip on top covers the one below.
    let under = clip(&m.bars, 0.0, 0.4, 0.0);
    let mut over = clip(&m.testsrc, 0.5, 0.4, 0.1);
    over.fade_in = 0.1;
    let tl = timeline(vec![vec![under], vec![over]], vec![], (640, 360), Fit::Cover, 30.0);
    compared += both("two tracks", &tl, &[m.bars.clone(), m.testsrc.clone()], &[3], 30.0, 0..12);
    // A dip to black joins two clips: each side is a `fade` to black of its own, the outgoing one playing on past its end.
    let a = clip(&m.testsrc, 0.0, 0.4, 0.0);
    let mut b = clip(&m.bars, 0.0, 0.4, 0.4);
    b.transition_in = Some(Transition {
        kind: TransitionKind::DipToBlack,
        duration: 0.2,
    });
    let tl = timeline(vec![vec![a, b]], vec![], (640, 360), Fit::Cover, 30.0);
    compared += both("a dip to black", &tl, &[m.testsrc.clone(), m.bars.clone()], &[3], 30.0, 0..24);
    // A Cover crop of two rows in a frame two rows short: 4:2:2 crops on its own chroma grid, at an odd row (1), where 4:2:0's is rounded to even.
    let mut c = clip(&m.testsrc, 0.0, 0.4, 0.0);
    c.fade_in = 0.2;
    let tl = timeline(vec![vec![c]], vec![], (640, 358), Fit::Cover, 30.0);
    compared += both(
        "a Cover crop at an odd row",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[3],
        30.0,
        0..12,
    );
    // A 25 fps 720p clip in a 30 fps 1080p frame (the ledger's teaser): the pick of the source frame, the enlargement, the fades.
    let mut h = clip(&m.hd, 0.5, 0.4, 0.0);
    h.fade_in = 0.15;
    h.fade_out = 0.15;
    let tl = timeline(
        vec![vec![h]],
        vec![TextOverlay::new("Out now", 0.1, 0.3)],
        (1920, 1080),
        Fit::Cover,
        30.0,
    );
    compared += both(
        "720p at 25 in 1080p at 30",
        &tl,
        std::slice::from_ref(&m.hd),
        &[3],
        30.0,
        0..12,
    );
    eprintln!("a_ten_bit_delivery_is_drawn_as_the_export_hands_it_to_the_encoder: {compared} samples compared, 0 differ");
}

/// A frame with no clip (a gap) is the black base, taken to 10 bits.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_black_base_of_a_gap_is_a_ten_bit_black() {
    require_ten_bit();
    let m = media();
    let a = clip(&m.testsrc, 0.0, 0.2, 0.0);
    let b = clip(&m.testsrc, 0.5, 0.2, 0.4);
    let tl = timeline(vec![vec![a, b]], vec![], (640, 360), Fit::Cover, 30.0);
    both(
        "a gap between two clips",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[3],
        30.0,
        0..18,
    );
}

// ---- what is refused -----------------------------------------------------------------------

/// The reasons `plan.reasons` gives, for the compositor's own caps, at frame `k` of a cut delivered in `layout`.
fn why(tl: &Timeline, assets: &[Asset], layout: TenBitLayout, caps: &kerf_core::GpuCaps, k: u64) -> Vec<Unsupported> {
    let opts = options(layout, 30.0);
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(policy())).expect("planner");
    let plan = planner.at_frame(k).expect("plan");
    plan.reasons(caps, (plan.canvas.width, plan.canvas.height))
}

/// An edit of a clip.
type Edit = Box<dyn Fn(&mut Clip)>;

fn refused_for_the_chain(why: &[Unsupported]) -> bool {
    why.iter().any(|u| matches!(u, Unsupported::DeliveryWith(..)))
}

/// What the 10-bit chain was not measured for is refused by the plan -- and by the render, which asks the plan -- in either layout; and with no
/// layout in the caps the whole delivery is, as it always was.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_the_ten_bit_chain_was_not_measured_for_is_refused() {
    require_ten_bit();
    let m = media();
    let comp = compositor(3);
    let caps = comp.caps();
    let plain = |edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.testsrc, 0.0, 0.5, 0.0);
        c.fade_in = 0.1;
        edit(&mut c);
        timeline(vec![vec![c]], vec![], (960, 540), Fit::Cover, 30.0)
    };
    let testsrc = std::slice::from_ref(&m.testsrc);
    for layout in [TenBitLayout::P420, TenBitLayout::P422] {
        // The plain clip is drawn.
        let reasons = why(&plain(&|_| {}), testsrc, layout, &caps, 5);
        assert!(reasons.is_empty(), "{layout:?}: {reasons:?}");
        // ... and each of these runs in the middle of the chain, on the pictures the chain makes in 10 bits.
        let cases: [(&str, Edit); 6] = [
            (
                "a colour correction",
                Box::new(|c: &mut Clip| {
                    c.color = Color {
                        brightness: 0.1,
                        ..Color::default()
                    };
                }),
            ),
            ("a zoom", Box::new(|c: &mut Clip| c.transform.scale = 0.5)),
            ("an opacity", Box::new(|c: &mut Clip| c.transform.opacity = 0.5)),
            (
                "a blur",
                Box::new(|c: &mut Clip| c.effects.push(VideoEffect::Blur { sigma: 3.0 })),
            ),
            (
                "a dissolve",
                Box::new(|c: &mut Clip| {
                    c.transition_in = Some(Transition {
                        kind: TransitionKind::Crossfade,
                        duration: 0.2,
                    });
                }),
            ),
            (
                "a dip to white",
                Box::new(|c: &mut Clip| {
                    c.transition_in = Some(Transition {
                        kind: TransitionKind::DipToWhite,
                        duration: 0.2,
                    });
                }),
            ),
        ];
        for (name, edit) in cases {
            let mut tl = plain(&*edit);
            if name == "a dissolve" || name == "a dip to white" {
                // (A transition joins two clips.)
                let mut second = clip(&m.testsrc, 0.5, 0.5, 0.5);
                edit(&mut second);
                tl.tracks[0].clips.push(second);
                tl.tracks[0].clips[0].transition_in = None;
            }
            let reasons = why(&tl, testsrc, layout, &caps, 14);
            assert!(
                !reasons.is_empty() && !matches!(reasons.as_slice(), [Unsupported::Delivery(_)]),
                "{layout:?} {name}: {reasons:?}"
            );
            assert!(
                refused_for_the_chain(&reasons) || reasons.iter().any(|u| !matches!(u, Unsupported::DeliveryFrame(_))),
                "{layout:?} {name}: {reasons:?}"
            );
        }
    }
    // The render asks the plan, so it refuses what the plan refuses -- and hands out a 10-bit delivery's planes only: not its RGB, and nothing
    // for a plan that is not one.
    let frame = Arc::new(kerf_gpu::YuvFrame {
        width: 640,
        height: 360,
        y: vec![90; 640 * 360],
        u: vec![100; 320 * 180],
        v: vec![150; 320 * 180],
    });
    let plan_of = |tl: &Timeline, layout: Option<TenBitLayout>| {
        let opts = layout.map_or_else(
            || ExportOptions {
                fps: Some(30.0),
                ..ExportOptions::default()
            },
            |l| options(l, 30.0),
        );
        Planner::new(tl, testsrc, &opts, PlanRequest::motion(policy()))
            .expect("planner")
            .at_frame(5)
            .expect("plan")
    };
    let at_size = |tl: &Timeline| {
        let c = tl.format.expect("a frame");
        (c.width, c.height)
    };
    let graded = plain(&|c| c.color.brightness = 0.1);
    let plan = plan_of(&graded, Some(TenBitLayout::P420));
    let frames = vec![Some(frame.clone()); plan.layers.len()];
    let err = comp
        .composite_yuv10_shared(&plan, &frames, at_size(&graded))
        .expect_err("the plan refuses a colour correction");
    assert!(matches!(err, kerf_gpu::GpuError::Unsupported(_)), "{err}");
    let ok = plain(&|_| {});
    let eight = plan_of(&ok, None);
    let frames = vec![Some(frame.clone()); eight.layers.len()];
    let err = comp
        .composite_yuv10_shared(&eight, &frames, at_size(&ok))
        .expect_err("an 8-bit delivery has no 10-bit planes");
    assert!(matches!(err, kerf_gpu::GpuError::Unsupported(_)), "{err}");
    let ten = plan_of(&ok, Some(TenBitLayout::P420));
    let frames = vec![Some(frame); ten.layers.len()];
    // (A compositor whose display conversion is known to match, so that the refusal is the delivery's and not the display's.)
    let shown = Compositor::new(gpu())
        .expect("a compositor")
        .with_scaler_kernels(kerf_gpu::scaler::Kernels::Match(kerf_gpu::scaler::Model {
            kernel: kerf_gpu::scaler::Kernel::X86,
            default: kerf_gpu::sws::Scaler::Bicubic,
            named: kerf_core::ScalerKinds::NONE,
        }))
        .with_ten_bit(TenBitKinds::P420.union(TenBitKinds::P422))
        .with_display(kerf_gpu::display_probe::Display::Match);
    let err = shown
        .composite_shared(&ten, &frames, at_size(&ok))
        .expect_err("a 10-bit delivery's RGB is not the export's");
    assert!(
        matches!(&err, kerf_gpu::GpuError::Unsupported(w) if w.contains("10-bit")),
        "{err}"
    );
    // Text over a frame with no clip, and a delivery that names a scaler: the frame, not a layer.
    let gap = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 0.2, 0.0)]],
        vec![TextOverlay::new("Out now", 0.3, 0.5)],
        (960, 540),
        Fit::Cover,
        30.0,
    );
    let reasons = why(&gap, testsrc, TenBitLayout::P420, &caps, 12);
    assert!(
        reasons
            .iter()
            .any(|u| matches!(u, Unsupported::DeliveryFrame(w) if w.contains("no clip"))),
        "{reasons:?}"
    );
    let tl = plain(&|_| {});
    let named = ExportOptions {
        scaler: Some("lanczos".into()),
        ..options(TenBitLayout::P420, 30.0)
    };
    let planner = Planner::new(&tl, testsrc, &named, PlanRequest::motion(policy())).expect("planner");
    let plan = planner.at_frame(5).expect("plan");
    let reasons = plan.reasons(&caps, (plan.canvas.width, plan.canvas.height));
    assert!(
        reasons
            .iter()
            .any(|u| matches!(u, Unsupported::DeliveryFrame(w) if w.contains("scaler"))),
        "{reasons:?}"
    );
    // With no 10-bit layout in the caps (a build the probe did not match), the delivery is refused whole: the plan, then the render.
    let mut off = caps;
    off.ten_bit = TenBitKinds::NONE;
    let reasons = why(&plain(&|_| {}), testsrc, TenBitLayout::P420, &off, 5);
    assert!(
        reasons
            .iter()
            .any(|u| matches!(u, Unsupported::Delivery(pf) if pf == "yuv420p10le")),
        "{reasons:?}"
    );
    // Other 10-bit layouts and depths stay FFmpeg's whatever the caps say.
    for pf in ["yuv444p10le", "yuv420p12le", "p010le"] {
        let opts = ExportOptions {
            pix_fmt: Some(pf.into()),
            ..options(TenBitLayout::P420, 30.0)
        };
        let planner = Planner::new(&tl, testsrc, &opts, PlanRequest::motion(policy())).expect("planner");
        let plan = planner.at_frame(5).expect("plan");
        let reasons = plan.reasons(&caps, (plan.canvas.width, plan.canvas.height));
        assert!(
            reasons.iter().any(|u| matches!(u, Unsupported::Delivery(p) if p == pf)),
            "{pf}: {reasons:?}"
        );
    }
}

// ---- the passes against the model (no FFmpeg) ------------------------------------------------

struct Lcg(u64);

impl Lcg {
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (self.0 >> 33) as u8
            })
            .collect()
    }
}

fn by_hand_asset(w: u32, h: u32) -> Asset {
    Asset {
        id: uuid::Uuid::new_v4(),
        path: "/media/hand.mp4".into(),
        name: "hand".into(),
        duration: 30.0,
        streams: vec![kerf_core::StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
            fps: Some(30.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".into()),
            color_space: None,
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: chrono::Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

/// The shader passes are the CPU model, sample for sample: no FFmpeg, only the adapter. Noise pictures resized, cropped and faded (every factor a
/// 0.1 s fade-in and fade-out run through), both layouts, the C scaler and x86's, both ways of indexing the 4:2:0 dither, several slice thread
/// counts, and the frames where nothing covers the canvas.
/// A source size and the canvas it is cut into.
type Pair = ((u32, u32), (u32, u32));

fn passes_against_the_model(sizes: &[Pair], threads: &[(u32, bool)], frames: &[u64]) {
    use kerf_gpu::scaler::{Kernel, Kernels, Model};
    use kerf_gpu::sws::Scaler;
    let (mut compared, mut refused) = (0usize, 0usize);
    let mut rng = Lcg(0x1234_5678_9abc_def1);
    for kernel in [Kernel::X86, Kernel::C] {
        let pinned = Kernels::Match(Model {
            kernel,
            default: Scaler::Bicubic,
            named: kerf_core::ScalerKinds::NONE,
        });
        for &(source, canvas) in sizes {
            let asset = by_hand_asset(source.0, source.1);
            let mut first = Clip::new(asset.id, 0.0, 0.4, 0.0);
            first.fade_in = 0.1;
            first.fade_out = 0.1;
            // A second clip after a gap: the frames between are the black base.
            let second = Clip::new(asset.id, 0.0, 0.2, 0.6);
            let tl = timeline(vec![vec![first, second]], vec![], canvas, Fit::Cover, 30.0);
            let (sw, sh) = (source.0 as usize, source.1 as usize);
            let frame = Arc::new(kerf_gpu::YuvFrame {
                width: source.0,
                height: source.1,
                y: rng.bytes(sw * sh),
                u: rng.bytes(sw.div_ceil(2) * sh.div_ceil(2)),
                v: rng.bytes(sw.div_ceil(2) * sh.div_ceil(2)),
            });
            for layout in [TenBitLayout::P420, TenBitLayout::P422] {
                let opts = options(layout, 30.0);
                let planner = Planner::new(
                    &tl,
                    std::slice::from_ref(&asset),
                    &opts,
                    PlanRequest::motion(CompositeColorPolicy::FixedBt601),
                )
                .expect("planner");
                for &(threads, per_slice) in threads {
                    let kinds = if per_slice {
                        TenBitKinds::ALL
                    } else {
                        TenBitKinds::P420.union(TenBitKinds::P422)
                    };
                    let comp = Compositor::new(gpu())
                        .expect("a compositor")
                        .with_scaler_kernels(pinned)
                        .with_ten_bit(kinds)
                        .with_graph_threads(threads);
                    let caps = comp.caps();
                    for &k in frames {
                        let plan = planner.at_frame(k).expect("plan");
                        let size = (plan.canvas.width, plan.canvas.height);
                        let why = plan.reasons(&caps, size);
                        // (A picture the fit leaves with an odd side is refused whole, in both layouts: not a frame to compare.)
                        if why
                            .iter()
                            .any(|u| matches!(u, Unsupported::DeliveryWith(_, w) if w.contains("odd size")))
                        {
                            assert_eq!(why.len(), 1, "{layout:?} {source:?}->{canvas:?} frame {k}: {why:?}");
                            refused += 1;
                            continue;
                        }
                        assert!(why.is_empty(), "{layout:?} {source:?}->{canvas:?} frame {k}: {why:?}");
                        let covered = plan.layers.len() == 1 && !(12..18).contains(&k);
                        let frames = vec![covered.then(|| frame.clone()); plan.layers.len()];
                        let drawn = comp
                            .composite_yuv10_shared(&plan, &frames, size)
                            .unwrap_or_else(|e| panic!("{e}"));
                        // The model.
                        let slice_rows = if per_slice && layout == TenBitLayout::P420 {
                            kerf_core::ten_bit_dither_rows(threads, size.1)
                        } else {
                            0
                        };
                        let planes8 = if covered {
                            let layer = &plan.layers[0];
                            let geom = plan.layer_geometry(layer, size).expect("geometry");
                            let chain = delivery10::LayerChain {
                                layout,
                                stage: geom
                                    .stages
                                    .first()
                                    .copied()
                                    .unwrap_or_else(|| delivery10::whole_stage(source.0, source.1)),
                                fades: plan
                                    .fade_ops(layer)
                                    .into_iter()
                                    .map(|op| op.factor)
                                    .filter(|f| *f < kerf_core::FADE_FULL)
                                    .collect(),
                                slice_rows,
                                scaler: Scaler::Bicubic,
                                kernel,
                            };
                            delivery10::layer_planes([&frame.y, &frame.u, &frame.v], (sw, sh), &chain).expect("tables")
                        } else {
                            delivery10::base_planes((size.0 as usize, size.1 as usize), layout, slice_rows, kernel)
                                .expect("tables")
                        };
                        let want = delivery10::to_ten_bit(
                            [&planes8[0], &planes8[1], &planes8[2]],
                            (size.0 as usize, size.1 as usize),
                            layout,
                            kernel,
                        )
                        .expect("tables");
                        for (name, a, b) in [("Y", &drawn.y, &want.y), ("U", &drawn.u, &want.u), ("V", &drawn.v, &want.v)] {
                            let apart = a.data.iter().zip(&b.data).filter(|(x, y)| x != y).count();
                            assert_eq!(
                                apart, 0,
                                "{layout:?} {kernel:?} {source:?}->{canvas:?} {threads} threads frame {k}: {name} differs from the model"
                            );
                            compared += a.data.len();
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "the ten-bit passes against the model: {compared} samples compared, 0 differ ({refused} frames of odd pictures refused)"
    );
}

/// The fast set: three pictures (an enlargement, a shrink with a Cover crop, a crop at an odd row), both scalers, both ways of indexing the dither.
#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn the_ten_bit_passes_are_the_model() {
    passes_against_the_model(
        &[((64, 36), (96, 54)), ((128, 96), (192, 108)), ((640, 358), (640, 354))],
        &[(1, false), (4, true)],
        &[1, 6, 12, 14, 21],
    );
}

/// The sweep: nine pictures, four thread settings, twelve frames (every part of a fade, the gap and the second clip).
#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn checker_delivery10_passes_are_the_model_over_many_pictures_and_thread_counts() {
    passes_against_the_model(
        &[
            ((64, 36), (64, 36)),
            ((64, 36), (96, 54)),
            ((96, 54), (64, 36)),
            ((160, 90), (64, 36)),
            ((128, 96), (192, 108)),
            ((180, 104), (90, 52)),
            ((200, 150), (120, 68)),
            ((322, 182), (240, 136)),
            ((640, 358), (640, 354)),
        ],
        &[(1, false), (2, true), (4, true), (3, false)],
        &[0, 1, 2, 3, 6, 9, 11, 14, 16, 19, 20, 22],
    );
}

// ---- the ledger's teaser, at its delivery size ---------------------------------------------------

/// T23a and T23d of the coverage ledger, as the export delivers them: eight 2 s cuts of a 720p clip at 25 fps in a 1080p frame at 30 fps, Cover,
/// the first fading in over 0.4 s and the last out over 0.6 s, a title from 4 s to 8 s; the stretches around every kind of frame (the fade-in, a
/// cut, the title's first frames, a cut under the title, the fade-out and the end). The ledger judges these at 540 px; this holds them at 1920x1080.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); minutes"]
fn checker_delivery10_the_ledgers_teaser_at_its_delivery_size() {
    require_ten_bit();
    let m = media();
    let dir = common::media_dir("delivery10-media");
    let path = dir.join("teaser-base.mp4");
    if !path.exists() {
        common::ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=25:duration=14,setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "25",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ]);
    }
    let base = Project::probe_asset(&path).expect("probe the teaser's base");
    let _ = m;
    let mut at = 0.0;
    let mut clips: Vec<Clip> = (0..8u32)
        .map(|i| {
            let c = clip(&base, f64::from(i % 6) * 2.0, 2.0, at);
            at += 2.0;
            c
        })
        .collect();
    clips[0].fade_in = 0.4;
    clips[7].fade_out = 0.6;
    let tl = timeline(
        vec![clips],
        vec![TextOverlay::new("Out now", 4.0, 8.0)],
        (1920, 1080),
        Fit::Cover,
        30.0,
    );
    let ranges = [0..14, 58..64, 118..124, 238..244, 464..480];
    let mut compared = 0;
    for layout in [TenBitLayout::P420, TenBitLayout::P422] {
        compared += compare_ranges(
            "the teaser at 1920x1080",
            &tl,
            std::slice::from_ref(&base),
            layout,
            3,
            30.0,
            &ranges,
        )
        .compared;
    }
    eprintln!("checker_delivery10_the_ledgers_teaser_at_its_delivery_size: {compared} samples compared, 0 differ");
}
