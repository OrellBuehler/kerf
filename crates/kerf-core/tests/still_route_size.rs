//! The size a moving still's export frame is drawn at (no FFmpeg: the assets are built by hand). The export makes the still's scaled
//! copy for its delivery frame, so the plan of an export frame is drawn at that frame and refused at any other; the export at another
//! size is another plan (`ExportOptions::resolution`), whose copy, window and fit order are judged against that frame. A still plan is
//! worked out for whatever size it renders at.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test still_route_size
//! ```

use kerf_core::{
    Asset, BlendExtras, BlendKinds, Clip, CompositeColorPolicy, Delivery, EffectKinds, ExportOptions, Fit, FitOrder, GpuCaps,
    GradeKinds, Keyframe, PlanMode, PlanRequest, Planner, RenderPlan, StillKinds, StreamKind, Timeline, Track, Transform,
};
use uuid::Uuid;

fn image(pix_fmt: &str, w: u32, h: u32) -> Asset {
    let stream = serde_json::json!({
        "index": 0,
        "kind": "video",
        "codec": "png",
        "width": w,
        "height": h,
        "image": true,
        "pix_fmt": pix_fmt,
    });
    serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(),
        "path": "/nonexistent/photo.png",
        "name": "photo.png",
        "duration": 5.0,
        "streams": [stream],
        "imported_at": "2024-01-01T00:00:00Z",
    }))
    .expect("an image asset")
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

/// Every pass the compositor has, on, and every chain of every kind measured: the most a plan can ask of the caps.
fn everything() -> GpuCaps {
    let mut c = GpuCaps::A0;
    c.motion = true;
    c.fades = true;
    c.transitions = true;
    c.dip_white = true;
    c.keyed_opacity = true;
    c.keyed_zoom = true;
    c.placement = true;
    c.placed_chroma = true;
    c.behaviors = true;
    c.keyed_color = true;
    c.mask = true;
    c.text = true;
    c.reframe = true;
    c.hdr = true;
    c.effects = EffectKinds::ALL;
    c.still_motion = StillKinds::ALL;
    c.grade = GradeKinds::ALL;
    c.exact_scaler = true;
    c.scalers = kerf_core::ScalerKinds::ALL;
    c.blur_fill = true;
    c.matrix_convert = true;
    c.blend = BlendKinds::ALL;
    c.blend_extras = BlendExtras::ALL;
    c
}

fn timeline(clip: Clip, canvas: (u32, u32)) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        format: Delivery::new(canvas.0, canvas.1, Fit::Cover).with_fps(Some(10.0)).ok(),
        ..Timeline::new()
    }
}

/// A slow zoom out from 1.3x to the frame, panning a little: inside the picture the whole way, so the export takes the route.
fn inside(asset: &Asset) -> Clip {
    let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
    clip.keyframes = vec![key(0.0, 1.3, 0.03, 0.02), key(1.0, 1.05, -0.01, 0.0)];
    clip
}

fn plan_of(asset: &Asset, clip: Clip, canvas: (u32, u32), resolution: Option<(u32, u32)>, mode: PlanMode, k: u64) -> RenderPlan {
    let tl = timeline(clip, canvas);
    let opts = ExportOptions {
        fps: Some(10.0),
        resolution,
        ..ExportOptions::default()
    };
    let policy = CompositeColorPolicy::FixedBt601;
    let request = if mode == PlanMode::Still {
        PlanRequest::still(policy)
    } else {
        PlanRequest::motion(policy)
    };
    Planner::new(&tl, std::slice::from_ref(asset), &opts, request)
        .expect("a planner")
        .at_frame(k)
        .expect("a plan")
}

/// What the plan refuses of the moving still at `size`, as text.
fn still_motion(plan: &RenderPlan, size: (u32, u32)) -> Vec<String> {
    plan.reasons(&everything(), size)
        .iter()
        .filter(|u| u.kind() == "still_motion")
        .map(ToString::to_string)
        .collect()
}

const DELIVERY: (u32, u32) = (320, 180);
const OTHER_SIZES: [(u32, u32); 7] = [(160, 90), (322, 180), (320, 182), (318, 178), (640, 360), (2, 2), (540, 302)];

#[test]
fn an_export_frame_of_a_moving_still_is_drawn_at_the_delivery_frame_and_at_no_other() {
    for pix in ["rgb24", "yuvj420p"] {
        let photo = image(pix, 1200, 800);
        for k in [0u64, 4, 9] {
            let plan = plan_of(&photo, inside(&photo), DELIVERY, None, PlanMode::Motion, k);
            let layer = &plan.layers[0];
            assert!(
                plan.takes_still_route(layer),
                "{pix} frame {k}: the export does not take the route"
            );
            let export = layer
                .still
                .as_ref()
                .and_then(|s| s.export.as_ref())
                .expect("the clip's own copy");
            assert_eq!(
                export.frame, DELIVERY,
                "{pix} frame {k}: the copy is not made for the delivery's frame"
            );
            assert_eq!(still_motion(&plan, DELIVERY), Vec::<String>::new(), "{pix} frame {k}");
            for size in OTHER_SIZES {
                let why = still_motion(&plan, size);
                assert!(
                    why.iter().any(|w| w.contains("at a render of")),
                    "{pix} frame {k}: drawn, or refused for another reason, at {size:?}: {why:?}"
                );
            }
        }
    }
}

#[test]
fn working_a_plan_out_for_a_render_leaves_an_export_frames_copy_alone() {
    let photo = image("rgb24", 1200, 800);
    let plan = plan_of(&photo, inside(&photo), DELIVERY, None, PlanMode::Motion, 4);
    let worked = plan.clone().for_render((160, 90));
    assert_eq!(
        plan.layers[0].still, worked.layers[0].still,
        "an export frame's copy was remade for the render"
    );
    assert!(
        !still_motion(&worked, (160, 90)).is_empty(),
        "an export frame is drawn at 160x90 once it is worked out for it"
    );
    assert!(still_motion(&worked, DELIVERY).is_empty());
}

#[test]
fn an_export_at_another_resolution_is_a_plan_of_its_own_with_its_own_copy() {
    for pix in ["rgb24", "yuvj420p"] {
        let photo = image(pix, 1200, 800);
        for k in [0u64, 4, 9] {
            let full = plan_of(&photo, inside(&photo), DELIVERY, None, PlanMode::Motion, k);
            let half = plan_of(&photo, inside(&photo), DELIVERY, Some((160, 90)), PlanMode::Motion, k);
            assert_eq!((half.canvas.width, half.canvas.height), (160, 90));
            let copy = |p: &RenderPlan| p.layers[0].still.as_ref().and_then(|s| s.export.clone()).expect("a copy");
            let (full_copy, half_copy) = (copy(&full), copy(&half));
            assert_eq!(full_copy.frame, DELIVERY);
            assert_eq!(half_copy.frame, (160, 90));
            assert_ne!(full_copy.q, half_copy.q, "{pix} frame {k}: the same copy for two frames");
            assert!(
                still_motion(&half, (160, 90)).is_empty(),
                "{pix} frame {k}: the half-size export is refused at its own frame"
            );
            assert!(
                !still_motion(&half, DELIVERY).is_empty(),
                "{pix} frame {k}: the half-size export is drawn at the full frame"
            );
            assert!(
                !still_motion(&full, (160, 90)).is_empty(),
                "{pix} frame {k}: the full export is drawn at the half frame"
            );
        }
    }
}

#[test]
fn a_still_plan_is_worked_out_for_the_size_it_renders_at() {
    for pix in ["rgb24", "yuvj420p"] {
        let photo = image(pix, 1200, 800);
        let plan = plan_of(&photo, inside(&photo), DELIVERY, None, PlanMode::Still, 4);
        assert!(
            plan.takes_still_route(&plan.layers[0]),
            "{pix}: a still plan does not take the route"
        );
        for size in [(160, 90), DELIVERY, (540, 302), (64, 36)] {
            let worked = plan.clone().for_render(size);
            assert_eq!(
                still_motion(&worked, size),
                Vec::<String>::new(),
                "{pix}: a still worked out for {size:?} is refused at it"
            );
            let copy = worked.layers[0]
                .still
                .as_ref()
                .and_then(|s| s.export.clone())
                .expect("a copy");
            assert_eq!(copy.frame, size, "{pix}: the copy is not made for {size:?}");
        }
        let worked = plan.clone().for_render((160, 90));
        assert!(
            !still_motion(&worked, DELIVERY).is_empty(),
            "{pix}: a still worked out for 160x90 is drawn at the delivery's frame"
        );
    }
}

/// The ledger plans every cut at 540 px wide: `still_size` of a 16:9 delivery is 540x302, which is not 16:9, so the same cut exported at
/// that resolution is judged against another frame: a 960x540 picture on a smaller zoom hangs its cover two pixels over the frame
/// (`cover_overflows`), where it does not at the delivery's own, and the fit order it takes is no longer the plain fit. The export at a
/// width is not the delivery's plan drawn at it, which is why no plan stands for one.
#[test]
fn the_fit_order_of_an_export_is_judged_against_its_own_frame() {
    let picture = image("yuvj420p", 960, 540);
    let pip = |asset: &Asset| {
        let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
        clip.transform = Transform {
            scale: 0.25,
            pos_x: 0.35,
            pos_y: 0.3,
            ..Transform::default()
        };
        clip
    };
    let delivery = plan_of(&picture, pip(&picture), (1920, 1080), None, PlanMode::Motion, 0);
    assert_eq!(delivery.size(540), (540, 302));
    assert_eq!(
        delivery.layers[0].fit_order,
        FitOrder::Fitted,
        "the delivery's frame hangs nothing over"
    );
    let at_width = plan_of(&picture, pip(&picture), (1920, 1080), Some((540, 302)), PlanMode::Motion, 0);
    assert_eq!((at_width.canvas.width, at_width.canvas.height), (540, 302));
    assert_eq!(
        at_width.layers[0].fit_order,
        FitOrder::Uncropped,
        "the export at 540x302 was judged as the export at 1920x1080 is"
    );
}
