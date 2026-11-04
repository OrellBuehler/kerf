//! What `kerf_gpu::prepare` measures and makes for a plan with moving stills: only the kinds and chains the plan needs, and nothing for a
//! layer the plan refuses whatever the probe says. The probes' answers are kept for the life of the process, so this is one test in a binary
//! of its own, run from a cold process.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test stills_prepare -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::{Command, Stdio};

use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, MotionFrames, Opened};

use kerf_core::{
    Clip, CompositeColorPolicy, Delivery, ExportOptions, Fit, FitShape, Keyframe, PlanRequest, Planner, Project, RenderPlan,
    StillChain, StillKinds, StreamKind, Timeline, Track,
};

fn photo(dir: &Path, name: &str, flags: &[&str]) -> kerf_core::Asset {
    let path = dir.join(name);
    let mut args = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x426:rate=1:duration=1",
        "-frames:v",
        "1",
    ];
    args.extend_from_slice(flags);
    args.push(path.to_str().expect("a utf-8 path"));
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Project::probe_asset(&path).expect("probe the photograph")
}

fn key(time: f64, scale: f64, x: f64, y: f64) -> Keyframe {
    Keyframe {
        easing: Default::default(),
        time,
        scale,
        pos_x: x,
        pos_y: y,
        rotation: 0.0,
        opacity: 1.0,
    }
}

/// The export frame (at the delivery's frame) of a still on a move: inside the frame, or panned off its edge, behind a static crop or not.
fn frame_of(asset: &kerf_core::Asset, inside: bool, cropped: bool, policy: CompositeColorPolicy) -> RenderPlan {
    let mut c = Clip::new(asset.id, 0.0, 2.0, 0.0);
    c.keyframes = if inside {
        vec![key(0.0, 1.2, 0.04, 0.03), key(2.0, 1.0, 0.0, 0.0)]
    } else {
        vec![key(0.0, 1.0, -0.03, 0.0), key(2.0, 1.18, 0.03, -0.02)]
    };
    if cropped {
        c.transform.crop_left = 0.1;
        c.transform.crop_top = 0.05;
    }
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![c],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(320, 180, Fit::Cover).with_fps(Some(30.0)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    Planner::new(&tl, std::slice::from_ref(asset), &opts, PlanRequest::motion(policy))
        .expect("a planner")
        .at_frame(10)
        .expect("a plan")
}

fn frame_with_scaler(asset: &kerf_core::Asset, scaler: &str, policy: CompositeColorPolicy) -> RenderPlan {
    let mut c = Clip::new(asset.id, 0.0, 2.0, 0.0);
    c.keyframes = vec![key(0.0, 1.2, 0.04, 0.03), key(2.0, 1.0, 0.0, 0.0)];
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![c],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(320, 180, Fit::Cover).with_fps(Some(30.0)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let opts = ExportOptions {
        fps: Some(30.0),
        scaler: Some(scaler.to_string()),
        ..ExportOptions::default()
    };
    Planner::new(&tl, std::slice::from_ref(asset), &opts, PlanRequest::motion(policy))
        .expect("a planner")
        .at_frame(10)
        .expect("a plan")
}

#[test]
#[ignore = "needs ffmpeg"]
fn prepare_measures_and_makes_only_what_a_plan_needs() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("stills-prepare");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let policy = kerf_core::composite_color_policy();
    assert_ne!(
        policy,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    let png = photo(&dir, "p.png", &["-pix_fmt", "rgb24"]);
    let jpeg = photo(&dir, "p.jpg", &["-q:v", "3", "-pix_fmt", "yuvj420p"]);
    let rgb = StillKinds::RGB24;
    let jpg = StillKinds::YUVJ420P;

    assert_eq!(
        kerf_gpu::still_probe::measured(),
        StillKinds::NONE,
        "a cold process measured nothing"
    );
    let held = || kerf_gpu::copies::held().0;
    let before = held();

    // A PNG slideshow's plain move: the PNG's base chains, and nothing of a JPEG's, nor of any other chain.
    let plain = frame_of(&png, true, false, policy);
    kerf_gpu::prepare(&plain);
    let measured = kerf_gpu::still_probe::measured();
    eprintln!("after a plain PNG move: {measured:?}, {} copies held", held());
    assert_eq!(measured, rgb, "the PNG's base chains, and only those");
    assert_eq!(held(), before + 1, "the copy the move is drawn from was made ahead");

    // A fit picture behind a static crop: its chain is measured when a plan needs it, beside the base it already has.
    let crop = frame_of(&png, false, true, policy);
    assert_eq!(
        crop.still_chain(&crop.layers[0], (320, 180)),
        Some(StillChain::edge(FitShape::EngineCrop, false))
    );
    kerf_gpu::prepare(&crop);
    let measured = kerf_gpu::still_probe::measured();
    assert_eq!(
        measured,
        rgb.union(rgb.chain(StillChain::edge(FitShape::EngineCrop, false))),
        "the crop's chain was measured, and not the staged fit's or the bicubic ones"
    );
    assert_eq!(held(), before + 2);

    // A frame FFmpeg draws whatever its stills are (a scaler the compositor does not implement): nothing is measured for it and no copy is
    // made, though its move inside the frame would be a copy of its own. (The route's text carries Lanczos by default, so a delivery that
    // names that one makes the very copy already held and proves nothing; `bilinear` makes another.)
    let bilinear = frame_with_scaler(&png, "bilinear", policy);
    let (before_measured, before_held) = (kerf_gpu::still_probe::measured(), held());
    kerf_gpu::prepare(&bilinear);
    assert_eq!(kerf_gpu::still_probe::measured(), before_measured);
    assert_eq!(held(), before_held, "a copy was made for a frame FFmpeg draws");

    // The pictures the feeders make ahead of a clip (`CursorFeed::open_planned`) follow the plan as `prepare` does: made for a frame the
    // compositor draws, not for one FFmpeg draws whole (`open` makes them for any layer, which is what the plan is asked first for).
    let caps = Compositor::caps_for(
        kerf_gpu::grade::measured(),
        kerf_gpu::scaler::measured(),
        kerf_gpu::convert::measured(),
    );
    let feed = MotionFrames::new(FrameSource::new(FrameSourceConfig::default())).feed();
    let size = bilinear.size(u32::MAX);
    assert!(
        !bilinear.reasons(&caps, size).is_empty(),
        "the control: FFmpeg draws this frame"
    );
    let before = held();
    let refused = feed
        .open_planned(&bilinear, &caps, size, &bilinear.layers[0], 0.0)
        .expect("asked");
    assert_eq!(refused, Opened::Skipped);
    assert_eq!(held(), before, "a picture was made for a frame FFmpeg draws");
    let other = photo(&dir, "q.png", &["-pix_fmt", "rgb24"]);
    let drawn = frame_of(&other, true, false, policy);
    assert!(drawn.reasons(&caps, size).is_empty(), "{:?}", drawn.reasons(&caps, size));
    let made = feed.open_planned(&drawn, &caps, size, &drawn.layers[0], 0.0).expect("asked");
    assert_eq!(made, Opened::Waiting { evicted: None });
    assert_eq!(
        held(),
        before + 1,
        "the picture of a frame the compositor draws was not made ahead"
    );
    // ... and `open` itself, asked of the refused frame's layer, makes it: the plan is what is asked first.
    feed.open(&bilinear.layers[0], 0.0).expect("opened");
    assert_eq!(held(), before + 2);

    // A JPEG: measured and its copy made where the plan can draw it (6.1.1, the composite's range fixed); refused whatever the probe says
    // where the range is negotiated (9.0.2), so nothing is measured for it and no copy is made.
    let jpeg_plan = frame_of(&jpeg, true, false, policy);
    let copies = held();
    kerf_gpu::prepare(&jpeg_plan);
    let measured = kerf_gpu::still_probe::measured();
    eprintln!("after a JPEG move: {measured:?}, {} copies held", held());
    if policy == CompositeColorPolicy::FixedBt601 {
        assert!(measured.contains(jpg), "a JPEG the plan can draw was not measured");
        assert_eq!(held(), copies + 1);
    } else {
        assert!(!measured.contains(jpg), "{measured:?}");
        assert_eq!(
            measured,
            rgb.union(rgb.chain(StillChain::edge(FitShape::EngineCrop, false))),
            "a JPEG the plan refuses was measured for"
        );
        assert_eq!(held(), copies, "a copy was made for a layer the plan refuses");
    }
}
