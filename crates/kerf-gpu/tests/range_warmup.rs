//! The file's range flag under a placement is asked ahead of the render, never by it: a layer whose asset did not record its
//! range, in a cut where the composite's range is negotiated, is refused by the compositing (`composite`, which is handed
//! the frames and decodes nothing) until `prepare` has asked the file (a cold cache is "the file would not say"), and
//! drawn or refused as the file says once it has. (A host's decode reads the file's traits too, which leaves the answer
//! in the cache: `FrameSource::check_cursor` asks for it by name.)
//!
//! The decisions made for a whole span rather than a frame see that refusal first: a host's `session_caps`
//! (`Compositor::session_caps_for`, `file_range`) make the plan read the same cache, so `PlayCut::ahead` and `resume` (a play session, a primer) and the render
//! bar refuse a file that is full range or has not said, instead of a session starting on the GPU and handing over a frame
//! into it. The asking is `prepare_ahead` (the session) and `prepare_ranges` (the bar's hook).
//!
//! Needs `ffmpeg`, `ffprobe` and an adapter (for the render test; the span test needs no adapter); `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test range_warmup -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use kerf_core::render_bar::{render_bar, BarRequest, BarState};
use kerf_core::{
    Clip, CompositeColorPolicy, ExportOptions, Keyframe, OriginalMedia, PlayCut, Project, RenderPlan, StreamKind, Timeline, Track,
};
use kerf_gpu::grade::Kernels;
use kerf_gpu::placement_probe::Placement;
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions, YuvFrame};

fn make(dir: &Path, name: &str, flags: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=64x36:rate=30:duration=1", "-c:v", "ffv1"];
    args.extend_from_slice(&["-pix_fmt", "yuv420p"]);
    args.extend_from_slice(flags);
    args.push(path.to_str().expect("a utf-8 path"));
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

/// A plan of a moving zoom over `path`, whose asset did not record its range, under a composite that negotiates the range.
fn plan_over(path: &Path) -> RenderPlan {
    let (timeline, asset) = timeline_over(path);
    RenderPlan::at(
        &timeline,
        &[asset],
        &ExportOptions::default(),
        0.5,
        CompositeColorPolicy::BottomLayerTag,
    )
    .expect("a plan")
}

/// The cut of that timeline, as a play session plans it.
fn cut_over(path: &Path) -> PlayCut {
    let (timeline, asset) = timeline_over(path);
    PlayCut::new(&timeline, &[asset], 0.0, CompositeColorPolicy::BottomLayerTag, &OriginalMedia).expect("a cut")
}

fn timeline_over(path: &Path) -> (Timeline, kerf_core::Asset) {
    let mut asset = Project::probe_asset(path).expect("probe the asset");
    for stream in &mut asset.streams {
        stream.color_range = None;
    }
    let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
    let key = |time: f64, scale: f64| Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    clip.keyframes = vec![key(0.0, 1.0), key(1.0, 1.6)];
    let timeline = Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    (timeline, asset)
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough)"]
fn the_render_never_asks_the_file_for_its_range_and_prepare_does() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("range-warmup");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let limited = make(&dir, "limited.mkv", &["-color_range", "tv"]);
    let full = make(&dir, "full.mkv", &["-color_range", "pc"]);

    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    // (Pinned, so that the placement probe is not what this test is about.)
    let compositor = Compositor::new(gpu)
        .expect("a compositor")
        .with_placement(Placement::Match)
        .with_display(kerf_gpu::display_probe::Display::Match);
    let flat = |w: u32, h: u32| YuvFrame {
        width: w,
        height: h,
        y: vec![90; (w * h) as usize],
        u: vec![120; (w.div_ceil(2) * h.div_ceil(2)) as usize],
        v: vec![130; (w.div_ceil(2) * h.div_ceil(2)) as usize],
    };
    let render = |plan: &RenderPlan| {
        let (w, h) = (plan.layers[0].stream.width, plan.layers[0].stream.height);
        compositor.composite(plan, &[Some(flat(w, h))], plan.size(u32::MAX))
    };
    let why = |result: Result<_, GpuError>| match result {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    };

    for (file, name, drawn, says) in [
        (&limited, "limited", true, ""),
        (&full, "full range", false, "full-range picture its asset did not record"),
    ] {
        let plan = plan_over(file);
        let file = Path::new(&plan.layers[0].path);
        assert_eq!(
            kerf_core::source_is_full_range_known(file),
            None,
            "{name}: nobody has asked yet"
        );
        // Cold: refused as "the file would not say", and the render has not asked it (the cache is still empty).
        let cold = why(render(&plan));
        assert!(
            cold.contains("was not asked ahead of the render"),
            "{name}: a cold range is refused by name: {cold:?}"
        );
        assert_eq!(
            kerf_core::source_is_full_range_known(file),
            None,
            "{name}: the render probed the file"
        );
        // Prepared: the answer is the file's.
        kerf_gpu::prepare(&plan);
        assert_eq!(kerf_core::source_is_full_range_known(file), Some(!drawn), "{name}");
        let warm = why(render(&plan));
        if drawn {
            assert_eq!(warm, "", "{name}: drawn once the file has said limited range");
        } else {
            assert!(warm.contains(says), "{name}: refused as the file says: {warm:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A play session's scan ahead and its taking a play back (`PlayCut::ahead`, `resume`) and the render bar judge the span by the
/// file's range, as the render will: a cold file is a refusal ("has not said"), a full-range one is a refusal, a limited one is
/// drawn, and the asking is the host's (`prepare_ahead`, `prepare_ranges`), never the plan's.
#[test]
#[ignore = "needs ffmpeg and ffprobe"]
fn the_session_and_the_render_bar_refuse_a_files_range_before_the_frame_and_not_after() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("range-warmup-session");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    assert_eq!(
        kerf_gpu::placement_probe::placement(),
        Placement::Match,
        "the control: this FFmpeg places as the model does, so the placement is in the caps"
    );
    let caps = Compositor::session_caps_for(Kernels::Match, kerf_gpu::scaler::measured(), kerf_gpu::convert::measured());
    assert!(caps.placement && caps.file_range, "{caps:?}");
    let pure = Compositor::caps_for(Kernels::Match, kerf_gpu::scaler::measured(), kerf_gpu::convert::measured());
    assert!(
        pure.placement && !pure.file_range,
        "the compositor's own caps, and the build's, ask nobody: {pure:?}"
    );
    const { assert!(!Compositor::CAPS.file_range) };

    for (flags, name, refused) in [
        (["-color_range", "tv"], "limited", false),
        (["-color_range", "pc"], "full-range", true),
    ] {
        // The session: `ahead` over its horizon, `resume` from the start.
        let file = make(&dir, &format!("{name}-session.mkv"), &flags);
        let cut = cut_over(&file);
        let plan = cut.plan(0.0).expect("the first frame");
        let (path, size) = (PathBuf::from(&plan.layers[0].path), plan.size(u32::MAX));
        assert_eq!(kerf_core::source_is_full_range_known(&path), None, "{name}: nobody has asked");
        let cold = cut
            .ahead(0.0, 0.5, size, &caps)
            .unwrap_or_else(|| panic!("{name}: a cold file was a drawn span"));
        assert_eq!(cold.why.kind(), "placed_format", "{name}: {cold:?}");
        assert!(cold.why.to_string().contains("has not said"), "{name}: {cold:?}");
        assert_eq!(
            cut.resume(0.0, 0.5, 0.3, size, &caps),
            None,
            "{name}: nothing to take back to"
        );
        assert!(
            cut.ahead(0.0, 0.5, size, &pure).is_none(),
            "{name}: a pure plan does not mind"
        );
        kerf_gpu::prepare_ahead(&cut, 0.0, 0.5);
        assert_eq!(kerf_core::source_is_full_range_known(&path), Some(refused), "{name}");
        let ahead = cut.ahead(0.0, 0.5, size, &caps);
        let resume = cut.resume(0.0, 0.5, 0.3, size, &caps);
        if refused {
            let ahead = ahead.unwrap_or_else(|| panic!("{name}: a full-range file was a drawn span"));
            assert!(ahead.why.to_string().contains("full-range"), "{name}: {ahead:?}");
            assert_eq!(resume, None, "{name}");
        } else {
            assert_eq!(ahead, None, "{name}: the file said limited range");
            assert_eq!(resume, Some(0.0), "{name}");
        }

        // The render bar, on files nobody has asked: its hook asks them.
        let file = make(&dir, &format!("{name}-bar.mkv"), &flags);
        let (timeline, asset) = timeline_over(&file);
        let judge = |ask: Option<&dyn Fn(&RenderPlan)>| {
            render_bar(
                &timeline,
                std::slice::from_ref(&asset),
                &BarRequest {
                    caps: Some(caps),
                    width: 640,
                    color: CompositeColorPolicy::BottomLayerTag,
                    media: &OriginalMedia,
                    waits: Vec::new(),
                    ask,
                },
            )
        };
        let states =
            |bar: &kerf_core::render_bar::RenderBar| bar.spans.iter().map(|s| (s.state, s.kind.clone())).collect::<Vec<_>>();
        let cold = judge(None);
        assert_eq!(
            states(&cold),
            vec![(BarState::Stream, "placed_format".to_string())],
            "{name}: not asked"
        );
        let asked = judge(Some(&kerf_gpu::prepare_ranges));
        let want = if refused {
            (BarState::Stream, "placed_format".to_string())
        } else {
            (BarState::Gpu, "gpu".to_string())
        };
        assert_eq!(
            states(&asked),
            vec![want.clone()],
            "{name}: asked by the bar's hook ({:?})",
            asked.spans
        );
        // ... and the answer is the file's, kept: the next bar needs no hook.
        assert_eq!(states(&judge(None)), vec![want], "{name}: remembered");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
