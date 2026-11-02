//! A render never runs a probe: a masked plan before `prepare` is refused up front (no `mask` in the caps), drawing it anyway is
//! `Unsupported`, and the `geq` probe has not run (it is a child process; a frame never waits on one). After `prepare` the mask is
//! in the caps and is drawn. One test, because the answer is cached once per process: a fresh process is what "unmeasured" is.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test geq_render -- --ignored --nocapture
//! ```

use std::path::Path;
use std::process::{Command, Stdio};

use kerf_core::{
    Asset, Clip, ExportOptions, GeqEdge, Mask, MaskShape, Project, RenderPlan, StreamKind, Timeline, Track, Unsupported,
};
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions};

fn make_clip(path: &Path) -> Asset {
    let tmp = path.with_extension("tmp.mkv");
    let lavfi = "nullsrc=size=64x36:rate=30:duration=0.6,format=yuv420p,\
                 geq=lum='mod(random(1)*255,256)':cb='mod(random(2)*255,256)':cr='mod(random(3)*255,256)'";
    let status = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", lavfi])
        .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success());
    std::fs::rename(&tmp, path).unwrap();
    Project::probe_asset(path).expect("probe")
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_render_with_an_unmeasured_geq_never_runs_the_probe_and_prepare_opens_the_mask() {
    // (This process has measured nothing: no earlier test in this binary, and `make_clip` below runs `ffmpeg`, not the probe.)
    assert_eq!(
        kerf_core::geq_edge_measured(),
        GeqEdge::Unknown,
        "a probe ran before the test asked"
    );
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let comp = Compositor::new(gpu).expect("a compositor");
    assert_eq!(
        kerf_core::geq_edge_measured(),
        GeqEdge::Unknown,
        "building a compositor ran the probe"
    );
    assert!(
        !comp.caps().mask && !comp.caps().keyed_opacity,
        "a mask is in the caps before the geq was measured"
    );

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("geq-render");
    std::fs::create_dir_all(&dir).unwrap();
    let asset = make_clip(&dir.join("a.mkv"));
    let mut clip = Clip::new(asset.id, 0.0, 0.5, 0.0);
    clip.mask = Some(Mask {
        shape: MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather: 0.0,
        inverted: false,
    });
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1")
        }],
        ..Timeline::new()
    };
    let assets = [asset];
    let policy = kerf_core::composite_color_policy();
    let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.2, policy).expect("plan");
    let size = plan.size(u32::MAX);
    let refusals = plan.reasons(&comp.caps(), size);
    assert!(
        refusals.iter().any(|u| matches!(u, Unsupported::Mask(_))),
        "the plan draws a mask the compositor cannot yet: {refusals:?}"
    );
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    // The render itself, handed the plan anyway: refused, and it does not measure.
    let err = comp
        .composite_yuv(&plan, &frames, size)
        .expect_err("the render drew a masked frame before the geq was measured");
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
    assert_eq!(kerf_core::geq_edge_measured(), GeqEdge::Unknown, "the render ran the probe");

    // `prepare` measures, off the render.
    kerf_gpu::prepare(&plan);
    assert_ne!(
        kerf_core::geq_edge_measured(),
        GeqEdge::Unknown,
        "prepare did not measure the geq"
    );
    assert!(comp.caps().mask && comp.caps().keyed_opacity);
    assert!(
        plan.reasons(&comp.caps(), size).is_empty(),
        "{:?}",
        plan.reasons(&comp.caps(), size)
    );
    comp.composite_yuv(&plan, &frames, size)
        .expect("the mask is drawn after prepare");
}
