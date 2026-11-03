//! The caps wiring of the merged filters and placement lanes, and what a play session that has not measured anything refuses.
//!
//! * `checker_b_m_caps_api_*` (no FFmpeg, no adapter): every pin of `Compositor::caps_for`, `caps_with`, `caps_pinned` and
//!   `session_caps_for` reaches the cap it is for, and none reaches another (a swapped or dropped pin is a different cap, not a compile error).
//! * `checker_b_m_a_cold_session_*` (`#[ignore]`d, FFmpeg): in a process that has measured nothing, the plan of every kind of clip that needs
//!   a probe (a mask, a blend, a moving zoom, a keyed opacity, a resized picture, a layer below full opacity, a blur) is refused up front by the session's caps, drawing it anyway is
//!   `Unsupported` and runs no probe; after `prepare_timeline` the same plans are judged clean. One test, because the answers are cached once per
//!   process: a fresh process is what "cold" is.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_b_m_cold -- --ignored --nocapture
//! ```

use std::path::Path;
use std::process::{Command, Stdio};

use kerf_core::{
    Asset, BlendKinds, BlendMode, Clip, CompositeColorPolicy, Delivery, ExportOptions, Fit, GeqEdge, Keyframe, Mask, MaskShape,
    OriginalMedia, PlanRequest, Planner, PlayCut, Project, StreamKind, Timeline, Track, Unsupported,
};
use kerf_gpu::convert::Kernels as Convert;
use kerf_gpu::grade::Kernels as Grade;
use kerf_gpu::scaler::{Kernels as Scaler, Model};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

fn all_blends() -> [BlendKinds; 2] {
    [BlendKinds::ALL; 2]
}

#[test]
fn checker_b_m_caps_api_scaler_pin_reaches_the_scaler_caps_only() {
    for (scaler, exact) in [
        (Scaler::Match(Model::ASSUMED), true),
        (Scaler::Differs, false),
        (Scaler::Unknown, false),
    ] {
        let caps = Compositor::caps_for(Grade::Match, scaler, Convert::Match);
        assert_eq!(caps.exact_scaler, exact, "{scaler:?}");
        assert_eq!(caps.scaler_approximate, !exact, "{scaler:?}");
        // (the scaler says nothing of the conversions)
        assert!(
            caps.matrix_convert && caps.dip_white && caps.blur_fill,
            "{scaler:?}: {caps:?}"
        );
        assert_eq!(caps.grade, Compositor::CAPS.grade, "{scaler:?}");
        let session = Compositor::session_caps_for(Grade::Match, scaler, Convert::Match);
        assert!(
            session.file_range && !caps.file_range,
            "{scaler:?}: the session's caps ask the files, the frame's do not"
        );
        assert_eq!(session.exact_scaler, exact, "{scaler:?}");
        assert_eq!(session.matrix_convert, caps.matrix_convert, "{scaler:?}");
    }
}

#[test]
fn checker_b_m_caps_api_convert_and_grade_pins_reach_the_conversion_caps_only() {
    let scaler = Scaler::Match(Model::ASSUMED);
    for convert in [Convert::Match, Convert::Differs("rehearsed"), Convert::Unknown] {
        let caps = Compositor::caps_for(Grade::Match, scaler, convert);
        assert_eq!(caps.matrix_convert, convert == Convert::Match, "{convert:?}");
        // (a conversion between matrices says nothing of the scaler, the grade or the dip)
        assert!(
            caps.exact_scaler && !caps.scaler_approximate && caps.dip_white,
            "{convert:?}: {caps:?}"
        );
        assert_eq!(caps.grade, Compositor::CAPS.grade, "{convert:?}");
    }
    for grade in [Grade::Match, Grade::Differs("rehearsed"), Grade::Unknown] {
        let caps = Compositor::caps_for(grade, scaler, Convert::Match);
        let matched = grade == Grade::Match;
        assert_eq!(caps.dip_white, matched, "{grade:?}");
        assert_eq!(
            caps.matrix_convert, matched,
            "{grade:?}: the conversion's way in is the grade's kernel too"
        );
        assert_eq!(caps.grade == Compositor::CAPS.grade, matched, "{grade:?}");
        assert!(caps.exact_scaler, "{grade:?}: the grade says nothing of the scaler");
    }
}

#[test]
fn checker_b_m_caps_api_pinned_pins_land_where_they_belong() {
    let text = None;
    // All six pins, each one in turn the odd one out: a swap of two is a different set of caps.
    let base = Compositor::caps_pinned(
        Grade::Match,
        Scaler::Match(Model::ASSUMED),
        Convert::Match,
        text,
        all_blends(),
        kerf_core::BlendExtras::ALL,
    );
    assert!(base.exact_scaler && base.matrix_convert && base.dip_white && !base.text);
    assert_eq!(base.blend, BlendKinds::ALL);
    assert_eq!(base.blend_extras, kerf_core::BlendExtras::ALL);

    let scaler_off = Compositor::caps_pinned(
        Grade::Match,
        Scaler::Differs,
        Convert::Match,
        text,
        all_blends(),
        kerf_core::BlendExtras::ALL,
    );
    assert!(!scaler_off.exact_scaler && scaler_off.matrix_convert && scaler_off.dip_white);
    let convert_off = Compositor::caps_pinned(
        Grade::Match,
        Scaler::Match(Model::ASSUMED),
        Convert::Unknown,
        text,
        all_blends(),
        kerf_core::BlendExtras::ALL,
    );
    assert!(convert_off.exact_scaler && !convert_off.matrix_convert && convert_off.dip_white);
    let grade_off = Compositor::caps_pinned(
        Grade::Unknown,
        Scaler::Match(Model::ASSUMED),
        Convert::Match,
        text,
        all_blends(),
        kerf_core::BlendExtras::ALL,
    );
    assert!(grade_off.exact_scaler && !grade_off.matrix_convert && !grade_off.dip_white);

    // The blend modes are those both matrices draw; the extras are passed through.
    let one_matrix = [BlendKinds::ALL, BlendKinds::NONE];
    let blend_off = Compositor::caps_pinned(
        Grade::Match,
        Scaler::Match(Model::ASSUMED),
        Convert::Match,
        text,
        one_matrix,
        kerf_core::BlendExtras::NONE,
    );
    assert_eq!(
        blend_off.blend,
        BlendKinds::NONE,
        "a mode only one matrix draws is not in the caps"
    );
    assert_eq!(blend_off.blend_extras, kerf_core::BlendExtras::NONE);
    assert!(blend_off.exact_scaler && blend_off.matrix_convert);

    // `caps_with` is `caps_pinned` with this process's scaler and conversion (nothing measured here: unknown, so neither is in the caps).
    let with = Compositor::caps_with(Grade::Match, text, all_blends(), kerf_core::BlendExtras::ALL);
    assert!(
        !with.exact_scaler && with.scaler_approximate,
        "the scaler is not measured in this process"
    );
    assert!(
        !with.matrix_convert,
        "the conversion between matrices is not measured in this process"
    );
    assert!(with.dip_white, "the dip follows the grade pin");
    assert_eq!(with.blend, BlendKinds::ALL);
    assert_eq!(with.blend_extras, kerf_core::BlendExtras::ALL, "the extras are the caller's");
    let with = Compositor::caps_with(Grade::Match, text, one_matrix, kerf_core::BlendExtras::NONE);
    assert_eq!(
        with.blend,
        BlendKinds::NONE,
        "a mode only one matrix draws is not in the caps"
    );
    assert_eq!(with.blend_extras, kerf_core::BlendExtras::NONE);
    // `caps_for` takes this process's blend measurement, which is nothing here.
    let for_caps = Compositor::caps_for(Grade::Match, Scaler::Match(Model::ASSUMED), Convert::Match);
    assert_eq!(
        for_caps.blend,
        BlendKinds::NONE,
        "no blend mode is in the caps of a process that measured none"
    );
    assert_eq!(for_caps.blend_extras, kerf_core::BlendExtras::NONE);
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn checker_b_m_caps_api_translucency_and_blur_pins_reach_their_caps_only() {
    use kerf_core::EffectKinds;
    use kerf_gpu::blur_probe::Blur;
    use kerf_gpu::roundtrip_probe::Translucency;
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let comp = || Compositor::new(std::sync::Arc::clone(&gpu)).expect("a compositor");
    let rest = |c: &kerf_core::GpuCaps| (c.exact_scaler, c.dip_white, c.grade, c.effects.without(EffectKinds::BLUR));
    let base = comp().with_translucency(Translucency::Match).with_blur(Blur::Match).caps();
    assert!(
        !base.translucent_approximate && base.effects.contains(EffectKinds::BLUR),
        "{base:?}"
    );
    for t in [
        Translucency::Match,
        Translucency::Differs("rehearsed".into()),
        Translucency::Unknown,
    ] {
        let caps = comp().with_translucency(t.clone()).with_blur(Blur::Match).caps();
        assert_eq!(caps.translucent_approximate, t != Translucency::Match, "{t:?}");
        assert!(
            caps.effects.contains(EffectKinds::BLUR),
            "{t:?}: the round trip says nothing of the blur"
        );
        assert_eq!(rest(&caps), rest(&base), "{t:?}");
    }
    for b in [Blur::Match, Blur::Differs("rehearsed".into()), Blur::Unknown] {
        let caps = comp().with_translucency(Translucency::Match).with_blur(b.clone()).caps();
        assert_eq!(caps.effects.contains(EffectKinds::BLUR), b == Blur::Match, "{b:?}");
        assert!(
            !caps.translucent_approximate,
            "{b:?}: the blur says nothing of the round trip"
        );
        assert_eq!(rest(&caps), rest(&base), "{b:?}");
    }
}

// ---- a cold session -----------------------------------------------------------------------------------------------

fn ffmpeg(args: &[&str]) {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn media(dir: &Path, w: u32, h: u32) -> Asset {
    let path = dir.join(format!("noise-{w}x{h}.mkv"));
    if !path.exists() {
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=1.2,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
        );
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ]);
    }
    // (An asset that recorded no range, as one stored before the flag was: FFmpeg 9 writes `tv` into a file whose encoder was never told, and
    // the session's range gate is about the asset that says nothing.)
    let mut asset = Project::probe_asset(&path).expect("probe");
    for stream in &mut asset.streams {
        stream.color_range = None;
    }
    asset
}

fn key(time: f64, scale: f64, opacity: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity,
        easing: Default::default(),
    }
}

/// Seven segments on the second track over a base, each of a kind that needs a probe, and the frame in the middle of each.
fn session() -> (Timeline, Vec<Asset>, Vec<(&'static str, u64)>) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-b-m-cold");
    std::fs::create_dir_all(&dir).unwrap();
    let (base, same, big) = (media(&dir, 160, 90), media(&dir, 160, 90), media(&dir, 320, 180));
    let mut masked = Clip::new(same.id, 0.0, 0.2, 0.0);
    masked.mask = Some(Mask {
        shape: MaskShape::Ellipse,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather: 0.2,
        inverted: false,
    });
    let mut blended = Clip::new(same.id, 0.2, 0.4, 0.2);
    blended.blend_mode = BlendMode::Multiply;
    let mut zooming = Clip::new(same.id, 0.4, 0.6, 0.4);
    zooming.keyframes = vec![key(0.0, 1.0, 1.0), key(0.2, 1.6, 1.0)];
    let mut fading = Clip::new(same.id, 0.6, 0.8, 0.6);
    fading.keyframes = vec![key(0.0, 1.0, 1.0), key(0.2, 1.0, 0.4)];
    let shrunk = Clip::new(big.id, 0.8, 1.0, 0.8);
    let mut translucent = Clip::new(same.id, 0.0, 0.1, 1.0);
    translucent.transform.opacity = 0.6;
    let mut blurred = Clip::new(same.id, 0.1, 0.2, 1.1);
    blurred.effects = vec![kerf_core::VideoEffect::Blur { sigma: 2.0 }];
    let timeline = Timeline {
        tracks: vec![
            Track {
                clips: vec![Clip::new(base.id, 0.0, 1.0, 0.0)],
                ..Track::new(StreamKind::Video, "V1".to_string())
            },
            Track {
                clips: vec![masked, blended, zooming, fading, shrunk, translucent, blurred],
                ..Track::new(StreamKind::Video, "V2".to_string())
            },
        ],
        format: Some(Delivery::new(160, 90, Fit::Contain)),
        ..Timeline::new()
    };
    let frames = vec![
        ("mask", 3),
        ("blend", 9),
        ("moving zoom", 15),
        ("keyed opacity", 21),
        ("resized picture", 27),
        ("translucent layer", 31),
        ("blur", 34),
    ];
    (timeline, vec![base, same, big], frames)
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_a_cold_session_is_refused_by_the_caps_and_primed_by_prepare_timeline() {
    assert_eq!(
        kerf_core::geq_edge_measured(),
        GeqEdge::Unknown,
        "a probe ran before the test asked"
    );
    assert_eq!(kerf_gpu::scaler::measured(), Scaler::Unknown);
    assert_eq!(kerf_gpu::blend::measured(), [BlendKinds::NONE; 2]);
    assert_eq!(
        kerf_gpu::placement_probe::measured(),
        kerf_gpu::placement_probe::Placement::Unknown
    );
    assert_eq!(kerf_gpu::conversion::measured(), kerf_gpu::conversion::Conversion::Unknown);
    assert_eq!(
        kerf_gpu::roundtrip_probe::measured(),
        kerf_gpu::roundtrip_probe::Translucency::Unknown
    );
    assert_eq!(kerf_gpu::blur_probe::measured(), kerf_gpu::blur_probe::Blur::Unknown);

    let (timeline, assets, frames) = session();
    // (The composite's colour policy is not one of the gates: the planner takes it as an input.)
    let policy = match kerf_core::composite_color_policy() {
        CompositeColorPolicy::Unknown => panic!("the composite colour probe could not read this FFmpeg"),
        p => p,
    };
    let planner = Planner::new(&timeline, &assets, &ExportOptions::default(), PlanRequest::motion(policy)).expect("planner");
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let comp = Compositor::new(std::sync::Arc::clone(&gpu)).expect("a compositor");
    let size = (160, 90);
    // The caps a host builds without a compositor (the render bar's: `caps_for`, `caps_with`, `caps_pinned`, `session_caps_for`) are
    // made of what this process has measured: with the `geq` edge unknown a mask and a keyed opacity are not in them, as they are not in
    // `Compositor::caps`; a compositor pinned to an edge says what the pin says, measured or not.
    let (grade, scaler, convert) = (Grade::Match, Scaler::Match(Model::ASSUMED), Convert::Match);
    let static_caps = || {
        [
            ("caps_for", Compositor::caps_for(grade, scaler, convert)),
            ("session_caps_for", Compositor::session_caps_for(grade, scaler, convert)),
            (
                "caps_with",
                Compositor::caps_with(grade, None, all_blends(), kerf_core::BlendExtras::ALL),
            ),
            (
                "caps_pinned",
                Compositor::caps_pinned(grade, scaler, convert, None, all_blends(), kerf_core::BlendExtras::ALL),
            ),
        ]
    };
    for (name, caps) in static_caps() {
        assert!(
            !caps.mask && !caps.keyed_opacity,
            "{name}: a mask or a keyed opacity is in the caps of a process that has not measured the geq edge"
        );
    }
    let pinned_caps = |edge| {
        Compositor::new(std::sync::Arc::clone(&gpu))
            .expect("a compositor")
            .with_geq_edge(edge)
            .caps()
    };
    assert!(
        !pinned_caps(GeqEdge::Unknown).mask && !pinned_caps(GeqEdge::Unknown).keyed_opacity,
        "pinned unknown"
    );
    for edge in [GeqEdge::Exact, GeqEdge::Penultimate] {
        let caps = pinned_caps(edge);
        assert!(caps.mask && caps.keyed_opacity, "pinned {edge:?}: {caps:?}");
    }

    let kind_of = |what: &str, u: &Unsupported| -> bool {
        let text = u.to_string();
        match what {
            "mask" => matches!(u, Unsupported::Mask(_)),
            "blend" => matches!(u, Unsupported::BlendMode(..)),
            "keyed opacity" => matches!(u, Unsupported::KeyedOpacity(_)),
            "resized picture" => matches!(u, Unsupported::ScalerApproximate(_)),
            "translucent layer" => matches!(u, Unsupported::TranslucentApproximate(_)),
            "blur" => matches!(u, Unsupported::Effects(_, w) if w.contains("blur")),
            "moving zoom" => text.contains("moving zoom") || text.contains("placement"),
            _ => false,
        }
    };
    // Cold: each plan is refused by the session's caps, for the probe it needs, and the render (handed the plan anyway) refuses it too
    // without measuring.
    let caps = comp.session_caps();
    assert!(
        !caps.mask
            && !caps.keyed_opacity
            && !caps.placement
            && !caps.exact_scaler
            && caps.blend == BlendKinds::NONE
            && caps.translucent_approximate
            && !caps.effects.contains(kerf_core::EffectKinds::BLUR),
        "{caps:?}"
    );
    for (what, k) in &frames {
        let plan = planner.at_frame(*k).expect("plan");
        let why = plan.reasons(&caps, size);
        assert!(
            why.iter().any(|u| kind_of(what, u)),
            "cold, the {what} frame {k} was not refused for it: {why:?}"
        );
        // (The shared variant is the one a host drives and never spawns a process; `composite_yuv`, the one-shot, asks for the placement.)
        let nothing: Vec<Option<std::sync::Arc<kerf_gpu::YuvFrame>>> = vec![None; plan.layers.len()];
        let drawn = comp.composite_yuv_shared(&plan, &nothing, size);
        let err = drawn.as_ref().expect_err(&format!(
            "a cold render drew the {what} frame {k}, which needs a probe ({why:?})"
        ));
        assert!(matches!(err, kerf_gpu::GpuError::Unsupported(_)), "{what}: {err}");
    }
    assert_eq!(kerf_core::geq_edge_measured(), GeqEdge::Unknown, "a render ran the geq probe");
    assert_eq!(kerf_gpu::scaler::measured(), Scaler::Unknown, "a render ran the scaler probe");
    assert_eq!(
        kerf_gpu::blend::measured(),
        [BlendKinds::NONE; 2],
        "a render ran the blend probe"
    );
    assert_eq!(
        kerf_gpu::placement_probe::measured(),
        kerf_gpu::placement_probe::Placement::Unknown,
        "a render ran the placement probe"
    );
    assert_eq!(
        kerf_gpu::roundtrip_probe::measured(),
        kerf_gpu::roundtrip_probe::Translucency::Unknown,
        "a render ran the translucency probe"
    );
    assert_eq!(
        kerf_gpu::blur_probe::measured(),
        kerf_gpu::blur_probe::Blur::Unknown,
        "a render ran the blur probe"
    );

    // Primed by the timeline alone: the whole cut's probes, once, ahead of any frame.
    kerf_gpu::prepare_timeline(&timeline);
    assert_ne!(
        kerf_core::geq_edge_measured(),
        GeqEdge::Unknown,
        "prepare_timeline left the geq edge unmeasured"
    );
    for (name, caps) in static_caps() {
        assert!(
            caps.mask && caps.keyed_opacity,
            "{name}: the edge is measured and a mask or a keyed opacity is still out of the caps: {caps:?}"
        );
    }
    let still_unknown = pinned_caps(GeqEdge::Unknown);
    assert!(
        !still_unknown.mask && !still_unknown.keyed_opacity,
        "a pin to an unknown edge is overruled by the measurement"
    );
    let caps = comp.session_caps();
    assert!(
        caps.mask && caps.keyed_opacity,
        "the geq edge was not measured by prepare_timeline: {caps:?}"
    );
    assert!(
        caps.placement && caps.behaviors,
        "the placement was not measured by prepare_timeline: {caps:?}"
    );
    assert!(
        caps.placed_chroma,
        "the planar conversion under a placement was not measured by prepare_timeline: {caps:?}"
    );
    assert!(
        caps.blend != BlendKinds::NONE,
        "the blend chain was not measured by prepare_timeline: {caps:?}"
    );
    assert!(caps.exact_scaler, "the scaler was not measured by prepare_timeline: {caps:?}");
    assert!(
        !caps.translucent_approximate,
        "the translucent round trip was not measured by prepare_timeline: {caps:?}"
    );
    assert!(
        caps.effects.contains(kerf_core::EffectKinds::BLUR),
        "the blur was not measured by prepare_timeline: {caps:?}"
    );
    // The files' ranges are asked for by the session's own step (`prepare_placed_files`, which `begin_play` runs after the timeline's): where
    // the composite's range is negotiated (9.0.2) a placed clip whose asset did not record its range is refused by the session's caps until
    // its file has said; where it is not (6.1.1) there is nothing to ask.
    let cut = PlayCut::new(&timeline, &assets, 0.0, policy, &OriginalMedia).expect("a cut");
    let negotiated = policy == CompositeColorPolicy::BottomLayerTag;
    for (what, k) in &frames {
        let plan = cut.at_frame(*k).expect("plan");
        let why = plan.reasons(&caps, size);
        let ranged = |u: &Unsupported| matches!(u, Unsupported::PlacedFormat(_, d) if d.contains("has not said"));
        if *what == "moving zoom" && negotiated {
            assert!(
                why.iter().any(ranged),
                "the asset records no range and nobody asked its file, yet the session's caps took the placed frame: {why:?}"
            );
        } else {
            assert!(why.is_empty(), "primed, the {what} frame {k} is still refused: {why:?}");
        }
    }
    kerf_gpu::prepare_placed_files(&cut, &timeline);
    for (what, k) in &frames {
        let plan = cut.at_frame(*k).expect("plan");
        let why = plan.reasons(&caps, size);
        assert!(
            why.is_empty(),
            "primed and the files asked, the {what} frame {k} is still refused: {why:?}"
        );
    }
}
