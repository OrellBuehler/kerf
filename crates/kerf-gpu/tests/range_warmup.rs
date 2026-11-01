//! The file's range flag under a placement is asked ahead of the render, never by it: a layer whose asset did not record its
//! range, in a cut where the composite's range is negotiated, is refused by the compositing (`composite`, which is handed
//! the frames and decodes nothing) until `prepare` has asked the file (a cold cache is "the file would not say"), and
//! drawn or refused as the file says once it has. (A host's decode reads the file's traits too, which leaves the answer
//! in the cache: `FrameSource::check_cursor` asks for it by name.)
//!
//! Needs `ffmpeg`, `ffprobe` and an adapter; `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test range_warmup -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use kerf_core::{Clip, CompositeColorPolicy, ExportOptions, Keyframe, Project, RenderPlan, StreamKind, Timeline, Track};
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
    RenderPlan::at(
        &timeline,
        &[asset],
        &ExportOptions::default(),
        0.5,
        CompositeColorPolicy::BottomLayerTag,
    )
    .expect("a plan")
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
    let compositor = Compositor::new(gpu).expect("a compositor").with_placement(Placement::Match);
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
