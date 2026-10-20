//! The compositor on synthetic frames — no FFmpeg, only an adapter.
//!
//! `#[ignore]`d because a machine without a GPU or a software Vulkan driver has
//! no adapter; the parity harness is the real acceptance test, this is the quick
//! one to run while changing a shader.

use chrono::Utc;
use kerf_core::{
    Asset, Clip, CompositeColorPolicy, Delivery, ExportOptions, Fit, PlanMode, PlanRequest, Planner, RenderPlan, StreamInfo,
    StreamKind, Timeline, Track, Transform, VideoEffect,
};
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions, YuvFrame};
use uuid::Uuid;

/// The synthetic frames here are untagged: both policies agree on them.
const POLICY: CompositeColorPolicy = CompositeColorPolicy::FixedBt601;

fn gpu() -> std::sync::Arc<Gpu> {
    Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)")
}

fn compositor() -> Compositor {
    Compositor::new(gpu()).expect("a compositor")
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
            pix_fmt: Some("yuv420p".into()),
            color_space: None,
            color_range: None,
            lead: 0.0,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
    }
}

fn timeline_of(clips: Vec<Clip>) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    }
}

/// A frame of one constant colour.
fn flat(w: u32, h: u32, y: u8, u: u8, v: u8) -> Option<YuvFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    Some(YuvFrame {
        width: w,
        height: h,
        y: vec![y; (w * h) as usize],
        u: vec![u; (cw * ch) as usize],
        v: vec![v; (cw * ch) as usize],
    })
}

fn plan_for(a: &Asset, c: Clip) -> RenderPlan {
    let tl = timeline_of(vec![c]);
    RenderPlan::at(&tl, std::slice::from_ref(a), &ExportOptions::default(), 0.5, POLICY).unwrap()
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn an_empty_frame_is_opaque_black() {
    let a = asset(64, 36);
    let tl = timeline_of(vec![Clip::new(a.id, 0.0, 1.0, 5.0)]);
    let plan = RenderPlan::at(&tl, &[a], &ExportOptions::default(), 1.0, POLICY).unwrap();
    assert!(plan.layers.is_empty());
    let frame = compositor().composite(&plan, &[], (64, 36)).unwrap();
    assert_eq!((frame.width, frame.height), (64, 36));
    assert!(frame.data.as_chunks::<4>().0.iter().all(|p| *p == [0, 0, 0, 255]));
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_flat_layer_converts_with_the_plans_matrix() {
    let a = asset(64, 36);
    let plan = plan_for(&a, Clip::new(a.id, 0.0, 1.0, 0.0));
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
    let plan = plan_for(&a, c);
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
fn a_letterboxed_picture_covers_the_layers_below_with_its_bars() {
    // Two tracks in a portrait frame: a bright base, and on top a landscape
    // clip that Contain letterboxes. FFmpeg's `pad` hands `overlay` the whole
    // frame, bars included, so the bars hide the base instead of showing it.
    let a = asset(64, 36);
    let mut tl = timeline_of(vec![Clip::new(a.id, 0.0, 1.0, 0.0)]);
    tl.format = Some(Delivery::new(36, 64, Fit::Contain));
    tl.tracks.push(Track {
        clips: vec![Clip::new(a.id, 0.0, 1.0, 0.0)],
        ..Track::new(StreamKind::Video, "V2")
    });
    let plan = RenderPlan::at(&tl, std::slice::from_ref(&a), &ExportOptions::default(), 0.5, POLICY).unwrap();
    assert_eq!(plan.layers.len(), 2);
    let frames = [flat(64, 36, 235, 128, 128), flat(64, 36, 100, 128, 128)];
    let frame = compositor().composite(&plan, &frames, (36, 64)).unwrap();
    let px = |x: usize, y: usize| &frame.data[(y * 36 + x) * 4..][..4];
    // Top bar and bottom bar: black, not the white base showing through.
    assert_eq!(px(18, 2), [0, 0, 0, 255]);
    assert_eq!(px(18, 61), [0, 0, 0, 255]);
    // The picture itself is the top clip's.
    let mid = px(18, 32);
    assert!(mid[0] < 120 && mid[0] > 80, "{mid:?}");
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_layer_with_no_frame_draws_nothing() {
    let a = asset(64, 36);
    let plan = plan_for(&a, Clip::new(a.id, 0.0, 1.0, 0.0));
    let frame = compositor().composite(&plan, &[None], (64, 36)).unwrap();
    assert!(frame.data.as_chunks::<4>().0.iter().all(|p| *p == [0, 0, 0, 255]));
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_plan_the_gpu_cannot_draw_is_refused_not_drawn_wrong() {
    let a = asset(64, 36);
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    // Every kind of effect is drawn now; a sigma `gblur` refuses is FFmpeg's to fail.
    c.effects.push(VideoEffect::Blur { sigma: 5000.0 });
    let plan = plan_for(&a, c);
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
    let plan = plan_for(&a, Clip::new(a.id, 0.0, 1.0, 0.0));
    let err = compositor()
        .composite(&plan, &[flat(64, 36, 100, 128, 128)], (63, 36))
        .unwrap_err();
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_frame_that_is_not_the_layers_stream_is_refused() {
    // The decode of a transposed (EXIF-oriented) picture is 36x64 where the
    // plan was made for 64x36: the compositor must not stretch it into place.
    let a = asset(64, 36);
    let plan = plan_for(&a, Clip::new(a.id, 0.0, 1.0, 0.0));
    let err = compositor()
        .composite(&plan, &[flat(36, 64, 100, 128, 128)], (64, 36))
        .unwrap_err();
    assert!(
        matches!(err, GpuError::Unsupported(ref why) if why.contains("stream")),
        "{err}"
    );
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn planes_that_do_not_add_up_are_refused_not_uploaded() {
    let a = asset(64, 36);
    let plan = plan_for(&a, Clip::new(a.id, 0.0, 1.0, 0.0));
    let mut short = flat(64, 36, 100, 128, 128).unwrap();
    short.u.truncate(10);
    let err = compositor().composite(&plan, &[Some(short)], (64, 36)).unwrap_err();
    assert!(matches!(err, GpuError::Decode(_)), "{err}");
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_zoom_past_the_texture_limit_is_refused_not_a_wgpu_panic() {
    let a = asset(640, 360);
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    c.transform.scale = 1000.0;
    let plan = plan_for(&a, c);
    let err = compositor()
        .composite(&plan, &[flat(640, 360, 100, 128, 128)], (640, 360))
        .unwrap_err();
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_lost_device_is_an_error_not_a_panic_and_stays_one() {
    let gpu = gpu();
    let comp = Compositor::new(gpu.clone()).unwrap();
    let a = asset(64, 36);
    let plan = plan_for(&a, Clip::new(a.id, 0.0, 1.0, 0.0));
    assert!(comp.composite(&plan, &[flat(64, 36, 100, 128, 128)], (64, 36)).is_ok());

    gpu.destroy();
    assert!(gpu.lost().is_some(), "the loss is recorded");
    for _ in 0..2 {
        let err = comp.composite(&plan, &[flat(64, 36, 100, 128, 128)], (64, 36)).unwrap_err();
        assert!(matches!(err, GpuError::DeviceLost(_)), "{err}");
    }
    // The owner's answer is a new device, and a new compositor on it.
    let fresh = Compositor::new(Gpu::new(GpuOptions::for_tests()).unwrap()).unwrap();
    assert!(fresh.composite(&plan, &[flat(64, 36, 100, 128, 128)], (64, 36)).is_ok());
    // And a compositor cannot be built on the lost one.
    assert!(matches!(Compositor::new(gpu), Err(GpuError::DeviceLost(_))));
}

// ---- masks and effects, against their own references (no FFmpeg) -------------------------------------

/// A deterministic noisy frame: every sample of Y, U and V is different from its neighbours.
fn noise(w: u32, h: u32, seed: u32) -> YuvFrame {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 24) as u8
    };
    YuvFrame {
        width: w,
        height: h,
        y: (0..w * h).map(|_| 16 + next() % 220).collect(),
        u: (0..cw * ch).map(|_| 16 + next() % 224).collect(),
        v: (0..cw * ch).map(|_| 16 + next() % 224).collect(),
    }
}

fn drawn_planes(a: &Asset, c: Clip, frame: &YuvFrame, size: (u32, u32)) -> YuvFrame {
    let plan = plan_for(a, c);
    assert_eq!(plan.reasons(&Compositor::CAPS, size), vec![], "the plan draws it");
    compositor().composite_yuv(&plan, &[Some(frame.clone())], size).unwrap()
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_blur_is_the_references_recursion_plane_by_plane() {
    use kerf_gpu::effects;
    for (w, h) in [(64u32, 36u32), (98, 62)] {
        let a = asset(w, h);
        let frame = noise(w, h, 7);
        for sigma in [0.5, 2.0, 6.0, 25.0] {
            let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
            c.effects = vec![VideoEffect::Blur { sigma }];
            let got = drawn_planes(&a, c, &frame, (w, h));
            let g = effects::gaussian(sigma).unwrap();
            let (cw, ch) = (w as usize / 2, h as usize / 2);
            let want_y = effects::blur_plane(&frame.y, w as usize, h as usize, &g);
            let want_u = effects::blur_plane(&frame.u, cw, ch, &g);
            let want_v = effects::blur_plane(&frame.v, cw, ch, &g);
            for (plane, a, b) in [("Y", &got.y, &want_y), ("U", &got.u, &want_u), ("V", &got.v, &want_v)] {
                let differing = a.iter().zip(b).filter(|(x, y)| x != y).count();
                let max = a
                    .iter()
                    .zip(b)
                    .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
                    .max()
                    .unwrap();
                // The shader's order is the reference's; a fused multiply-add on the device may flip a
                // sample that sits on a half.
                assert!(
                    max <= 1 && differing * 500 <= a.len(),
                    "{w}x{h} sigma {sigma} {plane}: max {max}, {differing} differ"
                );
            }
        }
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_sharpen_and_the_tables_are_the_references_byte_for_byte() {
    use kerf_gpu::effects;
    let (w, h) = (64u32, 36u32);
    let a = asset(w, h);
    let frame = noise(w, h, 11);
    for amount in [1.0, 2.5, 0.7, -1.0, -2.0, 5.0] {
        let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
        c.effects = vec![VideoEffect::Sharpen { amount }];
        let got = drawn_planes(&a, c, &frame, (w, h));
        let want = effects::sharpen_plane(&frame.y, w as usize, h as usize, effects::unsharp_amount(amount).unwrap());
        assert_eq!(got.y, want, "sharpen {amount}");
        assert_eq!((&got.u, &got.v), (&frame.u, &frame.v), "the chroma is left alone");
    }
    type Table = (fn(u8) -> u8, fn(u8) -> u8);
    let cases: [(Vec<VideoEffect>, Table); 4] = [
        (vec![VideoEffect::Invert], (|v| 255 - v, |v| 255 - v)),
        (vec![VideoEffect::Grayscale], (|v| v, |_| 128)),
        (vec![VideoEffect::Grayscale, VideoEffect::Invert], (|v| 255 - v, |_| 127)),
        (vec![VideoEffect::Invert, VideoEffect::Grayscale], (|v| 255 - v, |_| 128)),
    ];
    for (effects, f) in cases {
        let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
        c.effects = effects.clone();
        let got = drawn_planes(&a, c, &frame, (w, h));
        assert_eq!(got.y, frame.y.iter().map(|v| f.0(*v)).collect::<Vec<_>>(), "{effects:?} Y");
        assert_eq!(got.u, frame.u.iter().map(|v| f.1(*v)).collect::<Vec<_>>(), "{effects:?} U");
        assert_eq!(got.v, frame.v.iter().map(|v| f.1(*v)).collect::<Vec<_>>(), "{effects:?} V");
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_vignette_is_the_references_frame_after_frame() {
    use kerf_gpu::effects;
    for (w, h) in [(64u32, 36u32), (98, 62)] {
        let a = asset(w, h);
        let frame = noise(w, h, 5);
        let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
        c.effects = vec![VideoEffect::Vignette];
        let want = |n: u64| effects::vignette_planes(&frame.y, &frame.u, &frame.v, w, h, n);
        // A still graph runs its one frame through the filter: the first of the run.
        let got = drawn_planes(&a, c.clone(), &frame, (w, h));
        assert_eq!([got.y, got.u, got.v], want(0), "{w}x{h} still");
        // An export frame is dithered where the clip's run is: the count the cursor gives, past 2^32 samples too.
        let tl = timeline_of(vec![c]);
        let planner = Planner::new(
            &tl,
            std::slice::from_ref(&a),
            &ExportOptions::default(),
            PlanRequest::motion(POLICY),
        )
        .unwrap();
        let plan = planner.at_frame(3).unwrap();
        assert_eq!(plan.mode, PlanMode::Motion);
        let shared = [Some(std::sync::Arc::new(frame.clone()))];
        for n in [0u64, 1, 2, 29, 1_000_003] {
            let got = compositor()
                .composite_yuv_counted(&plan, &shared, &[Some(n)], (w, h))
                .unwrap();
            assert_eq!([got.y, got.u, got.v], want(n), "{w}x{h} frame {n}");
        }
        // Without its count an export frame's vignette is not drawn.
        for err in [
            compositor().composite_yuv_shared(&plan, &shared, (w, h)).unwrap_err(),
            compositor()
                .composite_yuv_counted(&plan, &shared, &[None], (w, h))
                .unwrap_err(),
        ] {
            assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
        }
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_chroma_keys_alpha_is_the_references_at_any_thread_count() {
    use kerf_gpu::effects::{self, ChromaKeyOptions};
    let uv = effects::chroma_key_uv([0, 128, 0]);
    for (w, h) in [(64u32, 36u32), (97, 55), (2, 2), (640, 360)] {
        // Chroma about the key, so the alpha is graded and every entry of the window counts.
        let mut frame = noise(w, h, 3);
        let near = |c: u8, n: u8| (i32::from(c) + i32::from(n % 61) - 30) as u8;
        frame.u = frame.u.iter().map(|n| near(uv[0], *n)).collect();
        frame.v = frame.v.iter().rev().map(|n| near(uv[1], *n)).collect();
        for threads in [1u32, 3, 5, 16] {
            let c = compositor().with_graph_threads(threads);
            for (similarity, blend) in [(0.02f32, 0.2f32), (0.15, 0.1), (0.05, 0.0), (0.01, 1.0)] {
                let key = ChromaKeyOptions { uv, similarity, blend };
                let got = c.chroma_key_alpha(&frame, &key).unwrap();
                let want = effects::chroma_key_plane(&frame.u, &frame.v, w, h, &key, threads);
                assert!(got == want, "{w}x{h} {threads} threads {key:?}");
            }
        }
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn the_key_runs_on_the_slices_of_the_graph_the_frame_stands_in_for() {
    let a = asset(64, 36);
    let export = u32::try_from(kerf_core::cpu_graph_threads()).unwrap();
    let c = compositor();
    // A paused frame of originals is the budget's graph; one of proxies or images is `single_frame_args`' one thread.
    let mut still = plan_for(&a, Clip::new(a.id, 0.0, 1.0, 0.0));
    assert_eq!(c.graph_threads_for(&still), export);
    still.layers[0].source.proxy = true;
    assert_eq!(c.graph_threads_for(&still), 1);
    still.layers[0].source.proxy = false;
    still.layers[0].is_image = true;
    assert_eq!(c.graph_threads_for(&still), 1);
    // The playback stream and an export run the budget's whatever they read.
    let tl = timeline_of(vec![Clip::new(a.id, 0.0, 1.0, 0.0)]);
    let planner = Planner::new(
        &tl,
        std::slice::from_ref(&a),
        &ExportOptions::default(),
        PlanRequest::motion(POLICY),
    )
    .unwrap();
    let mut motion = planner.at_frame(3).unwrap();
    motion.layers[0].source.proxy = true;
    assert_eq!(c.graph_threads_for(&motion), export);
    // Pinned, pinned.
    let pinned = compositor().with_graph_threads(7);
    assert_eq!((pinned.graph_threads_for(&still), pinned.graph_threads_for(&motion)), (7, 7));
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_keyed_layer_is_blended_with_the_keys_alpha_at_an_odd_chroma_size() {
    use kerf_gpu::effects::{self, ChromaKeyOptions};
    let (w, h) = (98u32, 62u32);
    let a = asset(w, h);
    let uv = effects::chroma_key_uv([0, 128, 0]);
    let mut frame = noise(w, h, 9);
    let near = |c: u8, n: u8| (i32::from(c) + i32::from(n % 61) - 30) as u8;
    frame.u = frame.u.iter().map(|n| near(uv[0], *n)).collect();
    frame.v = frame.v.iter().rev().map(|n| near(uv[1], *n)).collect();
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    c.effects = vec![VideoEffect::ChromaKey {
        color: "green".into(),
        similarity: 0.02,
        blend: 0.2,
    }];
    let threads = compositor().graph_threads_for(&plan_for(&a, c.clone()));
    let key = ChromaKeyOptions {
        uv,
        similarity: 0.02,
        blend: 0.2,
    };
    let alpha = effects::chroma_key_plane(&frame.u, &frame.v, w, h, &key, threads);
    // (A graded alpha, so the blend is held at many levels of it.)
    let levels: std::collections::BTreeSet<u8> = alpha.iter().copied().collect();
    assert!(levels.len() > 20, "{levels:?}");
    let got = drawn_planes(&a, c, &frame, (w, h));
    // `overlay` over the black base (Y 16, U and V 128): luma with each sample's alpha, chroma with its block's average.
    let blend = |d: u32, s: u8, al: u32| ((d * (255 - al) + u32::from(s) * al + 128) * 257) >> 16;
    for (i, (y, al)) in got.y.iter().zip(&alpha).enumerate() {
        assert_eq!(u32::from(*y), blend(16, frame.y[i], u32::from(*al)), "luma sample {i}");
    }
    let (cw, ch) = (w as usize / 2, h as usize / 2);
    for cy in 0..ch {
        for cx in 0..cw {
            let at = |dx: usize, dy: usize| u32::from(alpha[(2 * cy + dy) * w as usize + 2 * cx + dx]);
            // (`overlay` averages what its last column and row have, as for a mask.)
            let (right, below) = (cx + 1 < cw, cy + 1 < ch);
            let al = if right && below {
                (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) >> 2
            } else {
                let ah = if right { (at(0, 0) + at(1, 0)) >> 1 } else { at(0, 0) };
                let av = if below { (at(0, 0) + at(0, 1)) >> 1 } else { at(0, 0) };
                (av + ah) >> 1
            };
            let i = cy * cw + cx;
            assert_eq!(u32::from(got.u[i]), blend(128, frame.u[i], al), "U ({cx}, {cy})");
            assert_eq!(u32::from(got.v[i]), blend(128, frame.v[i], al), "V ({cx}, {cy})");
        }
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_mask_is_the_alpha_plane_blended_over_black() {
    use kerf_core::{Mask, MaskShape};
    let (w, h) = (64u32, 36u32);
    let a = asset(w, h);
    let mask = Mask {
        shape: MaskShape::Rect,
        x: 0.5,
        y: 0.5,
        width: 0.5,
        height: 0.5,
        feather: 0.4,
        inverted: false,
    };
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    c.mask = Some(mask);
    let frame = flat(w, h, 200, 128, 128).unwrap();
    let got = drawn_planes(&a, c, &frame, (w, h));
    let alpha = kerf_core::mask_alpha_plane(&mask, w, h);
    // Luma is `overlay`'s FAST_DIV255 of the layer over the black base (Y 16) with the sample's own alpha.
    for (i, (y, al)) in got.y.iter().zip(&alpha).enumerate() {
        let want = ((16 * (255 - u32::from(*al)) + 200 * u32::from(*al) + 128) * 257) >> 16;
        assert_eq!(u32::from(*y), want, "luma sample {i} (alpha {al})");
    }
    // Chroma is blended with the average of its block's alphas (the last column and row average what they have).
    let (cw, ch) = (w as usize / 2, h as usize / 2);
    for cy in 0..ch {
        for cx in 0..cw {
            let at = |dx: usize, dy: usize| u32::from(alpha[(2 * cy + dy) * w as usize + 2 * cx + dx]);
            let (right, below) = (cx + 1 < cw, cy + 1 < ch);
            let al = if right && below {
                (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) >> 2
            } else {
                let ah = if right { (at(0, 0) + at(1, 0)) >> 1 } else { at(0, 0) };
                let av = if below { (at(0, 0) + at(0, 1)) >> 1 } else { at(0, 0) };
                (av + ah) >> 1
            };
            // (U and V of the layer and the base are both 128: the blend keeps 128.)
            assert_eq!(got.u[cy * cw + cx], 128, "chroma ({cx}, {cy}) alpha {al}");
        }
    }
    assert!(got.y.contains(&16) && got.y.contains(&200) && got.y.iter().any(|v| *v > 16 && *v < 200));
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_mask_hands_the_planes_on_as_the_geq_that_clamps_its_read_does() {
    use kerf_core::{geq_penultimate_plane, GeqEdge, Mask, MaskShape};
    // A mask that keeps everything (alpha 255 on every sample): what is left is `geq`'s own copy of the
    // three planes. On a build that clamps its read to `w - 2` / `h - 2` (FFmpeg 6.1) its last column and
    // row are the ones before; on one that does not (9.0) they are the picture's own.
    for (w, h) in [(64u32, 36u32), (98, 62)] {
        let a = asset(w, h);
        let frame = noise(w, h, 11);
        let mask = Mask {
            shape: MaskShape::Rect,
            x: 0.5,
            y: 0.5,
            width: 1.0,
            height: 1.0,
            feather: 0.0,
            inverted: false,
        };
        assert!(kerf_core::mask_alpha_plane(&mask, w, h).iter().all(|a| *a == 255));
        let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
        c.mask = Some(mask);
        let plan = plan_for(&a, c);
        let draw = |edge: GeqEdge| {
            compositor()
                .with_geq_edge(edge)
                .composite_yuv(&plan, &[Some(frame.clone())], (w, h))
        };
        let exact = draw(GeqEdge::Exact).unwrap();
        assert_eq!((&exact.y, &exact.u, &exact.v), (&frame.y, &frame.u, &frame.v), "{w}x{h}");
        let got = draw(GeqEdge::Penultimate).unwrap();
        let (cw, ch) = (w as usize / 2, h as usize / 2);
        let (w, h) = (w as usize, h as usize);
        let want_y = geq_penultimate_plane(&frame.y, w, h);
        assert_eq!(got.y, want_y, "luma {w}x{h}");
        for (name, got, src) in [("U", &got.u, &frame.u), ("V", &got.v, &frame.v)] {
            let want = geq_penultimate_plane(src, cw, ch);
            assert_eq!(*got, want, "{name} {w}x{h}");
        }
        assert_ne!(want_y, frame.y, "the clamp changes this picture");
        // Neither answer known: refused, not drawn on a guess.
        let err = draw(GeqEdge::Unknown).unwrap_err();
        assert!(matches!(err, GpuError::Unsupported(_)), "{err:?}");
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn a_mask_blends_chroma_with_the_block_average_of_the_alpha() {
    use kerf_core::{Mask, MaskShape};
    // A layer of chroma 200 over a base of 128 shows the alpha the chroma samples were blended with.
    let (w, h) = (64u32, 36u32);
    let a = asset(w, h);
    let mask = Mask {
        shape: MaskShape::Ellipse,
        x: 0.5,
        y: 0.5,
        width: 0.7,
        height: 0.7,
        feather: 0.5,
        inverted: false,
    };
    let mut c = Clip::new(a.id, 0.0, 1.0, 0.0);
    c.mask = Some(mask);
    let frame = flat(w, h, 128, 200, 60).unwrap();
    let got = drawn_planes(&a, c, &frame, (w, h));
    let alpha = kerf_core::mask_alpha_plane(&mask, w, h);
    let (cw, ch) = (w as usize / 2, h as usize / 2);
    for cy in 0..ch {
        for cx in 0..cw {
            let at = |dx: usize, dy: usize| u32::from(alpha[(2 * cy + dy) * w as usize + 2 * cx + dx]);
            let (right, below) = (cx + 1 < cw, cy + 1 < ch);
            let al = if right && below {
                (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) >> 2
            } else {
                let ah = if right { (at(0, 0) + at(1, 0)) >> 1 } else { at(0, 0) };
                let av = if below { (at(0, 0) + at(0, 1)) >> 1 } else { at(0, 0) };
                (av + ah) >> 1
            };
            let blend = |d: u32, s: u32| ((d * (255 - al) + s * al + 128) * 257) >> 16;
            assert_eq!(u32::from(got.u[cy * cw + cx]), blend(128, 200), "U ({cx}, {cy}) alpha {al}");
            assert_eq!(u32::from(got.v[cy * cw + cx]), blend(128, 60), "V ({cx}, {cy}) alpha {al}");
        }
    }
}
