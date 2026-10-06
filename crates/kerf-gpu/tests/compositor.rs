//! The compositor on synthetic frames — no FFmpeg, only an adapter.
//!
//! `#[ignore]`d because a machine without a GPU or a software Vulkan driver has
//! no adapter; the parity harness is the real acceptance test, this is the quick
//! one to run while changing a shader.

use chrono::Utc;
use kerf_core::{Asset, Clip, ExportOptions, RenderPlan, StreamInfo, StreamKind, Timeline, Track, Transform, VideoEffect};
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions, YuvFrame};
use uuid::Uuid;

fn compositor() -> Compositor {
    Compositor::new(Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
}

fn asset(w: u32, h: u32) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: "/not/decoded.mp4".into(),
        name: "synthetic".into(),
        duration: 10.0,
        streams: vec![StreamInfo {
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
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
    }
}

fn timeline_of(clips: Vec<Clip>) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
    }
}

/// A frame of one constant colour.
fn flat(w: u32, h: u32, y: u8, u: u8, v: u8) -> YuvFrame {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    YuvFrame {
        width: w,
        height: h,
        y: vec![y; (w * h) as usize],
        u: vec![u; (cw * ch) as usize],
        v: vec![v; (cw * ch) as usize],
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn an_empty_frame_is_opaque_black() {
    let a = asset(64, 36);
    let tl = timeline_of(vec![Clip::new(a.id, 0.0, 1.0, 5.0)]);
    let plan = RenderPlan::at(&tl, &[a], &ExportOptions::default(), 1.0).unwrap();
    assert!(plan.layers.is_empty());
    let frame = compositor().composite(&plan, &[], (64, 36)).unwrap();
    assert_eq!((frame.width, frame.height), (64, 36));
    assert!(frame.data.as_chunks::<4>().0.iter().all(|p| *p == [0, 0, 0, 255]));
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_flat_layer_converts_with_the_plans_matrix() {
    let a = asset(64, 36);
    let tl = timeline_of(vec![Clip::new(a.id, 0.0, 1.0, 0.0)]);
    let plan = RenderPlan::at(&tl, &[a], &ExportOptions::default(), 0.5).unwrap();
    // Y 100, U 160, V 90 is (37, 116, 162) in BT.601 limited range (FFmpeg's
    // swscale lands one to two levels lower; the parity harness measures that).
    let frame = compositor()
        .composite(&plan, &[flat(64, 36, 100, 160, 90)], (64, 36))
        .unwrap();
    for p in frame.data.as_chunks::<4>().0 {
        assert!((i32::from(p[0]) - 37).abs() <= 1, "{p:?}");
        assert!((i32::from(p[1]) - 116).abs() <= 1, "{p:?}");
        assert!((i32::from(p[2]) - 162).abs() <= 1, "{p:?}");
        assert_eq!(p[3], 255);
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_half_size_layer_leaves_black_around_it() {
    let a = asset(64, 36);
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    c.transform = Transform {
        scale: 0.5,
        ..Transform::default()
    };
    let tl = timeline_of(vec![c]);
    let plan = RenderPlan::at(&tl, &[a], &ExportOptions::default(), 0.5).unwrap();
    let frame = compositor()
        .composite(&plan, &[flat(64, 36, 180, 128, 128)], (64, 36))
        .unwrap();
    let px = |x: usize, y: usize| &frame.data[(y * 64 + x) * 4..][..4];
    // The 32x18 picture is centred: x in 16..48, y in 9..27.
    assert_eq!(px(2, 2), [0, 0, 0, 255]);
    assert_eq!(px(61, 33), [0, 0, 0, 255]);
    let mid = px(32, 18);
    assert!(mid[0] > 180 && mid[0] == mid[1] && mid[1] == mid[2], "{mid:?}");
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_plan_the_gpu_cannot_draw_is_refused_not_drawn_wrong() {
    let a = asset(64, 36);
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    c.effects.push(VideoEffect::Blur { sigma: 3.0 });
    let tl = timeline_of(vec![c]);
    let plan = RenderPlan::at(&tl, &[a], &ExportOptions::default(), 0.5).unwrap();
    assert!(!plan.gpu_supported());
    let err = compositor()
        .composite(&plan, &[flat(64, 36, 100, 128, 128)], (64, 36))
        .unwrap_err();
    assert!(
        matches!(err, GpuError::Unsupported(ref why) if why.contains("video effects")),
        "{err}"
    );
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_canvas_with_an_odd_side_is_refused() {
    let a = asset(64, 36);
    let tl = timeline_of(vec![Clip::new(a.id, 0.0, 1.0, 0.0)]);
    let plan = RenderPlan::at(&tl, &[a], &ExportOptions::default(), 0.5).unwrap();
    let err = compositor()
        .composite(&plan, &[flat(64, 36, 100, 128, 128)], (63, 36))
        .unwrap_err();
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
}
