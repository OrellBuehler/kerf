//! `prepare` measures the translucent round trip for a plan that holds a layer tagged SMPTE 240M where FFmpeg negotiates the composite's
//! matrix (a stack of that tag alone is drawn on the round trip's verdict, `GpuCaps::smpte240m`, even when nothing in it is translucent), and
//! not where the composite is BT.601 whatever the tag (an opaque layer needs no matrix there). One test, because each answer is cached once per
//! process: a fresh process is what "unmeasured" is.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test smpte240m_prepare -- --ignored --nocapture
//! ```

use std::process::Command;

use kerf_core::{Clip, CompositeColorPolicy, ExportOptions, Project, RenderPlan, StreamKind, Timeline, Track};
use kerf_gpu::roundtrip_probe::{self, Translucency};

#[test]
#[ignore = "needs ffmpeg"]
fn a_plan_of_smpte240m_measures_the_round_trip_only_where_the_composite_follows_the_tag() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("smpte240m-prepare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("archive.mp4");
    let made = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=64x64:rate=30:duration=1,setparams=colorspace=smpte240m:range=tv")
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .expect("run ffmpeg");
    assert!(made.success());
    let asset = Project::probe_asset(&path).expect("probe");
    assert_eq!(asset.streams[0].color_space.as_deref(), Some("smpte240m"));
    let timeline = Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(asset.id, 0.0, 1.0, 0.0)],
            ..Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let plan = |policy| {
        RenderPlan::at(
            &timeline,
            std::slice::from_ref(&asset),
            &ExportOptions::default(),
            0.5,
            policy,
        )
        .unwrap()
    };
    assert_eq!(
        roundtrip_probe::measured(),
        Translucency::Unknown,
        "a probe ran before the test asked"
    );
    // Under a fixed policy the composite is BT.601 whatever the tag, and an opaque layer is converted by nothing.
    kerf_gpu::prepare(&plan(CompositeColorPolicy::FixedBt601));
    assert_eq!(
        roundtrip_probe::measured(),
        Translucency::Unknown,
        "an opaque 240M layer under a fixed policy measured the round trip"
    );
    // Where the composite follows the bottom layer's tag it is 240M's, and the stack is drawn on the round trip's verdict.
    let negotiated = plan(CompositeColorPolicy::BottomLayerTag);
    assert_eq!(negotiated.canvas.matrix, kerf_core::YuvMatrix::Smpte240m);
    kerf_gpu::prepare(&negotiated);
    assert_ne!(
        roundtrip_probe::measured(),
        Translucency::Unknown,
        "a stack of 240M under a negotiating policy left the round trip unmeasured"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
