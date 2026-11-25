//! A moving still is never drawn as footage: for a still image whose zoom, position or behaviours move its picture, in every
//! arrangement of the things the plan has refusals for (colour, grade, effects, mask, rotation, opacity, a dip to white, a
//! blend, a keyed crop, every pixel format and range, every fit, both modes of plan, both composite policies, at the delivery's
//! frame and at others), judged against a caps set in which **every pass is on** (so that nothing but the still's own
//! reasons can refuse it), a layer the plan lets through is on the moving still's route or has the fit picture FFmpeg makes
//! for it, and never both. No FFmpeg is run: the assets are built by hand.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_a_t6_leaks
//! ```

#![allow(clippy::print_stderr, clippy::semicolon_if_nothing_returned)]

use kerf_core::{
    Asset, BlendExtras, BlendKinds, Clip, Color, CompositeColorPolicy, Delivery, EffectKinds, ExportOptions, Fit, GpuCaps,
    GradeKinds, Keyframe, PlanMode, PlanRequest, Planner, RenderPlan, StillKinds, StreamKind, Timeline, Track, Transition,
    TransitionKind,
};
use uuid::Uuid;

fn image(pix_fmt: &str, w: u32, h: u32, range: Option<&str>) -> Asset {
    let mut stream = serde_json::json!({
        "index": 0,
        "kind": "video",
        "codec": "png",
        "width": w,
        "height": h,
        "image": true,
        "pix_fmt": pix_fmt,
    });
    if let Some(r) = range {
        stream["color_range"] = serde_json::json!(r);
    }
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

/// Every pass the compositor has, on, and every kind of picture measured: the most that can be drawn.
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
    c.blur_fill = true;
    c.matrix_convert = true;
    c.blend = BlendKinds::ALL;
    c.blend_extras = BlendExtras::ALL;
    c
}

type Edit = Box<dyn Fn(&mut Clip)>;

fn edits() -> Vec<(&'static str, Edit)> {
    vec![
        ("plain", Box::new(|_| {})),
        (
            "colour",
            Box::new(|c| {
                c.color = Color {
                    contrast: 1.3,
                    ..Color::default()
                }
            }),
        ),
        (
            "grade",
            Box::new(|c| {
                c.grade.wheels = Some(kerf_core::Wheels {
                    midtones: kerf_core::Wheel {
                        x: 0.2,
                        y: -0.1,
                        lightness: 0.1,
                    },
                    ..Default::default()
                })
            }),
        ),
        ("grayscale", Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Grayscale])),
        ("invert", Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Invert])),
        (
            "blur",
            Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 2.0 }]),
        ),
        ("vignette", Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Vignette])),
        (
            "mask",
            Box::new(|c| {
                c.mask = Some(kerf_core::Mask {
                    shape: kerf_core::MaskShape::Ellipse,
                    x: 0.5,
                    y: 0.5,
                    width: 0.6,
                    height: 0.6,
                    feather: 0.1,
                    inverted: false,
                })
            }),
        ),
        (
            "rotation",
            Box::new(|c| c.keyframes.iter_mut().for_each(|k| k.rotation = 7.0)),
        ),
        (
            "keyed rotation",
            Box::new(|c| c.keyframes.last_mut().unwrap().rotation = 15.0),
        ),
        ("opacity", Box::new(|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.6))),
        ("keyed opacity", Box::new(|c| c.keyframes.first_mut().unwrap().opacity = 0.3)),
        ("fade in", Box::new(|c| c.fade_in = 0.4)),
        ("fade out", Box::new(|c| c.fade_out = 0.4)),
        ("blend", Box::new(|c| c.blend_mode = kerf_core::BlendMode::Multiply)),
        (
            "dip to white in",
            Box::new(|c| {
                c.transition_in = Some(Transition {
                    kind: TransitionKind::DipToWhite,
                    duration: 0.4,
                })
            }),
        ),
        (
            "crossfade in",
            Box::new(|c| {
                c.transition_in = Some(Transition {
                    kind: TransitionKind::Crossfade,
                    duration: 0.4,
                })
            }),
        ),
        (
            "slide in",
            Box::new(|c| {
                c.transition_in = Some(Transition {
                    kind: TransitionKind::SlideLeft,
                    duration: 0.4,
                })
            }),
        ),
        (
            "keyed crop",
            Box::new(|c| {
                c.set_property_keys(
                    kerf_core::Property::CropZoom,
                    vec![kerf_core::PropertyKey::new(0.0, 1.0), kerf_core::PropertyKey::new(1.0, 1.25)],
                )
            }),
        ),
        (
            "static crop",
            Box::new(|c| {
                c.transform.crop_left = 0.1;
                c.transform.crop_top = 0.05;
            }),
        ),
        (
            "breathe",
            Box::new(|c| {
                c.keyframes.clear();
                c.set_behaviors(vec![kerf_core::Behavior::Breathe {
                    amount: 0.1,
                    speed: 1.0,
                    phase: 0.0,
                }])
                .unwrap()
            }),
        ),
        (
            "shake",
            Box::new(|c| {
                c.keyframes.clear();
                c.set_behaviors(vec![kerf_core::Behavior::Shake {
                    amount: 0.02,
                    speed: 2.0,
                    seed: 3,
                    phase: 0.0,
                }])
                .unwrap()
            }),
        ),
        (
            "sway",
            Box::new(|c| {
                c.set_behaviors(vec![kerf_core::Behavior::Sway {
                    amount: 3.0,
                    speed: 1.0,
                    phase: 0.0,
                }])
                .unwrap()
            }),
        ),
        ("speed 2", Box::new(|c| c.speed = 2.0)),
    ]
}

#[test]
fn a_moving_still_the_plan_lets_through_is_on_the_route_or_has_the_fit_ffmpeg_makes() {
    let moves: [(&str, Vec<Keyframe>); 4] = [
        ("inside", vec![key(0.0, 1.3, 0.03, 0.02), key(1.0, 1.05, -0.01, 0.0)]),
        ("off the edge", vec![key(0.0, 1.0, -0.04, 0.0), key(1.0, 1.2, 0.04, -0.02)]),
        ("zoom out", vec![key(0.0, 0.9, 0.0, 0.0), key(1.0, 0.6, 0.05, 0.0)]),
        ("pan", vec![key(0.0, 1.25, -0.1, 0.0), key(1.0, 1.25, 0.1, 0.0)]),
    ];
    let pictures = [
        ("rgb24", 1200, 800, None),
        ("rgb24", 321, 181, None),
        ("rgba", 1200, 800, None),
        ("yuvj420p", 1200, 800, None),
        ("yuvj420p", 1501, 1003, None),
        ("yuvj422p", 1200, 800, None),
        ("yuvj444p", 1200, 800, None),
        ("yuv420p", 1200, 800, None),
        ("yuv420p", 1200, 800, Some("pc")),
        ("yuvj420p", 1200, 800, Some("tv")),
        ("gray", 1200, 800, None),
        ("rgb48be", 1200, 800, None),
        ("pal8", 1200, 800, None),
        ("rgb24", 8000, 4500, None),
        ("rgb24", 2, 2, None),
    ];
    let canvases = [(320, 180), (180, 320), (100, 100)];
    let policies = [CompositeColorPolicy::FixedBt601, CompositeColorPolicy::BottomLayerTag];
    let caps = everything();
    let (mut judged, mut routed, mut fitted, mut refused) = (0usize, 0usize, 0usize, 0usize);
    let mut leaks: Vec<String> = Vec::new();
    for (pix, w, h, range) in pictures {
        let asset = image(pix, w, h, range);
        for (mname, keys) in &moves {
            for (ename, edit) in edits() {
                for fit in [Fit::Cover, Fit::Contain, Fit::Blur] {
                    for (cw, ch) in canvases {
                        let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
                        clip.keyframes = keys.clone();
                        edit(&mut clip);
                        let tl = Timeline {
                            tracks: vec![Track {
                                clips: vec![clip],
                                ..Track::new(StreamKind::Video, "V1".to_string())
                            }],
                            format: Delivery::new(cw, ch, fit).with_fps(Some(10.0)).ok(),
                            ..Timeline::new()
                        };
                        for policy in policies {
                            for mode in [PlanMode::Still, PlanMode::Motion] {
                                let opts = ExportOptions {
                                    fps: Some(10.0),
                                    ..ExportOptions::default()
                                };
                                let request = if mode == PlanMode::Still {
                                    PlanRequest::still(policy)
                                } else {
                                    PlanRequest::motion(policy)
                                };
                                let Ok(planner) = Planner::new(&tl, std::slice::from_ref(&asset), &opts, request) else {
                                    continue;
                                };
                                for k in [0u64, 4, 9] {
                                    let plan: RenderPlan = match mode {
                                        PlanMode::Still => match planner.at(k as f64 / 10.0) {
                                            Ok(p) => p,
                                            Err(_) => continue,
                                        },
                                        PlanMode::Motion => match planner.at_frame(k) {
                                            Ok(p) => p,
                                            Err(_) => continue,
                                        },
                                    };
                                    let delivery = (plan.canvas.width, plan.canvas.height);
                                    // A still plan is judged at the delivery's frame and at a smaller render, worked out for it.
                                    let sizes: Vec<(u32, u32)> = if mode == PlanMode::Still {
                                        vec![delivery, plan.size(64)]
                                    } else {
                                        vec![delivery]
                                    };
                                    for size in sizes {
                                        let plan = plan.clone().for_render(size);
                                        let moving = plan
                                            .layers
                                            .iter()
                                            .filter(|l| l.is_image && l.animated.is_some_and(|a| a.places || a.still_motion))
                                            .count();
                                        if moving == 0 {
                                            continue;
                                        }
                                        judged += 1;
                                        let why = plan.reasons(&caps, size);
                                        let case = format!(
                                            "{pix} {w}x{h} {range:?} / {mname} / {ename} / {fit:?} {cw}x{ch} / {policy:?} / {mode:?} k={k} at {size:?}"
                                        );
                                        if !why.is_empty() {
                                            refused += 1;
                                            continue;
                                        }
                                        for layer in plan.layers.iter().filter(|l| l.is_image) {
                                            let route = plan.takes_still_route(layer);
                                            let fit_made = plan.fits_by_ffmpeg(layer);
                                            if route && fit_made {
                                                leaks.push(format!("{case}: both the route and a fit picture"));
                                            }
                                            let placed = layer.animated.is_some_and(|a| a.places);
                                            if placed && !route && !fit_made {
                                                leaks.push(format!(
                                                    "{case}: a moving still the plan lets through that is neither the route nor a fit FFmpeg makes"
                                                ));
                                            }
                                            if route {
                                                routed += 1;
                                            }
                                            if fit_made && layer.fit.as_ref().is_some_and(|f| f.frame == size) {
                                                fitted += 1;
                                            }
                                            if fit_made && layer.fit.as_ref().is_none_or(|f| f.frame != size) {
                                                leaks.push(format!(
                                                    "{case}: a fit picture that is not worked out for this size is judged"
                                                ));
                                            }
                                            // Whatever FFmpeg makes of the picture is of a kind that was measured.
                                            if (route || fit_made)
                                                && !matches!(
                                                    layer.stream.pix_fmt.as_deref().map(str::to_ascii_lowercase).as_deref(),
                                                    Some("rgb24" | "yuvj420p")
                                                )
                                            {
                                                leaks.push(format!("{case}: drawn for a {:?} picture", layer.stream.pix_fmt));
                                            }
                                            if (route || fit_made) && layer.stream.raw_full_range() {
                                                leaks.push(format!(
                                                    "{case}: drawn for a picture flagged full range in another format"
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("A6 LEAKS: {judged} plans of moving stills judged, {refused} refused, {routed} on the route, {fitted} with a fit picture FFmpeg makes, {} leaks", leaks.len());
    for l in leaks.iter().take(40) {
        eprintln!("A6 LEAK {l}");
    }
    assert!(leaks.is_empty(), "{} leaks, first: {}", leaks.len(), leaks[0]);
    assert!(routed > 0 && fitted > 0, "the sweep drew nothing on one of the routes");
}
