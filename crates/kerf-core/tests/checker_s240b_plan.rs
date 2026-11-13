//! `checker/s240b`, the plan's reading of SMPTE 240M: for every stack of up to three layers over the tags a file can carry, every pixel format that
//! decides a refusal, both opacities, each composite policy and the caps with and without `GpuCaps::smpte240m`, the plan's reasons keep to a few
//! rules written here from the spec and not from the code: no 240M picture is drawn without the cap or beside another matrix under a
//! negotiating policy, nothing is drawn under an unmeasured policy, a fixed policy asks nothing of a matrix for an opaque layer, the tags with no
//! table stay refused, and a stack with no 240M picture in it is judged as it was whatever the cap says. No FFmpeg, no GPU.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_s240b_plan
//! ```

use chrono::Utc;
use kerf_core::{
    Asset, Clip, CompositeColorPolicy, ExportOptions, GpuCaps, PlanRequest, Planner, RenderPlan, StreamInfo, StreamKind,
    Timeline, Track, Unsupported, YuvMatrix,
};
use uuid::Uuid;

fn video(w: u32, h: u32, pix_fmt: &str, tag: Option<&str>) -> StreamInfo {
    StreamInfo {
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
        pix_fmt: Some(pix_fmt.into()),
        color_space: tag.map(Into::into),
        color_range: None,
        lead: 0.0,
        time_base: None,
    }
}

fn asset(stream: StreamInfo) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: "/media/a.mp4".into(),
        name: "a".into(),
        duration: 20.0,
        streams: vec![stream],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

fn timeline(tracks: Vec<Vec<Clip>>) -> Timeline {
    let mut tl = Timeline::new();
    tl.tracks = tracks
        .into_iter()
        .map(|clips| {
            let mut t = Track::new(StreamKind::Video, "V");
            t.clips = clips;
            t
        })
        .collect();
    tl
}

/// The matrix class a tag is to swscale (the checker's reading of `ff_yuv2rgb_coeffs`): untagged, `reserved` and the SMPTE 170M / BT.470 BG tags
/// are BT.601, `fcc`, `bt2020c` and `ycgco` have a table or none and the compositor draws none of them.
fn class(tag: Option<&str>) -> &'static str {
    match tag {
        None | Some("smpte170m" | "bt470bg" | "reserved") => "601",
        Some("bt709") => "709",
        Some("bt2020nc") => "2020",
        Some("smpte240m") => "240",
        Some(_) => "other",
    }
}

/// A stack bottom first, each layer `(pixel format, tag, opacity)`.
fn plan_of(policy: CompositeColorPolicy, layers: &[(&str, Option<&str>, f64)]) -> RenderPlan {
    let assets: Vec<Asset> = layers.iter().map(|(fmt, tag, _)| asset(video(640, 360, fmt, *tag))).collect();
    let tracks = assets
        .iter()
        .zip(layers)
        .map(|(a, (_, _, op))| {
            let mut c = Clip::new(a.id, 0.0, 10.0, 0.0);
            c.transform.opacity = *op;
            vec![c]
        })
        .collect();
    RenderPlan::at(&timeline(tracks), &assets, &ExportOptions::default(), 1.0, policy).expect("plan")
}

fn caps(smpte240m: bool) -> GpuCaps {
    let mut c = GpuCaps::A0;
    c.smpte240m = smpte240m;
    c.matrix_convert = true;
    c.matrix_convert_scaled = true;
    c.translucent_odd = true;
    c.exact_scaler = true;
    c.motion = true;
    c.fades = true;
    c.transitions = true;
    c.keyed_opacity = true;
    c
}

fn reasons(plan: &RenderPlan, caps: &GpuCaps) -> Vec<Unsupported> {
    plan.reasons(caps, plan.size(u32::MAX))
}

fn about_matrix(u: &Unsupported) -> bool {
    matches!(
        u,
        Unsupported::MatrixUnknown(_)
            | Unsupported::MixedMatrices
            | Unsupported::MixedMatricesWith(..)
            | Unsupported::RgbInStack(_)
            | Unsupported::UnmeasuredPolicy(_)
            | Unsupported::TranslucentMatrix(..)
    )
}

const TAGS: [Option<&str>; 11] = [
    None,
    Some("smpte170m"),
    Some("bt470bg"),
    Some("reserved"),
    Some("bt709"),
    Some("bt2020nc"),
    Some("smpte240m"),
    Some("fcc"),
    Some("bt2020c"),
    Some("ycgco"),
    Some("unspecified"),
];

/// Every stack of one to three layers of the tags above (and each opacity of the upper ones), judged by the rules.
#[test]
fn the_rules_hold_for_every_stack_of_tags() {
    let mut checked = 0usize;
    let mut stacks: Vec<Vec<(Option<&str>, f64)>> = Vec::new();
    for &a in &TAGS {
        stacks.push(vec![(a, 1.0)]);
        stacks.push(vec![(a, 0.5)]);
        for &b in &TAGS {
            for op in [1.0, 0.5] {
                stacks.push(vec![(a, 1.0), (b, op)]);
            }
        }
    }
    for &a in &[None, Some("bt709"), Some("smpte240m"), Some("fcc")] {
        for &b in &[None, Some("bt709"), Some("smpte240m")] {
            for &c in &[None, Some("bt709"), Some("smpte240m"), Some("bt2020c")] {
                for (ob, oc) in [(1.0, 1.0), (0.5, 1.0), (1.0, 0.5), (0.5, 0.5)] {
                    stacks.push(vec![(a, 1.0), (b, ob), (c, oc)]);
                }
            }
        }
    }
    for stack in &stacks {
        let layers: Vec<(&str, Option<&str>, f64)> = stack.iter().map(|(t, o)| ("yuv420p", *t, *o)).collect();
        let classes: Vec<&str> = stack.iter().map(|(t, _)| class(*t)).collect();
        let has_240 = classes.contains(&"240");
        let all_same = classes.windows(2).all(|w| w[0] == w[1]);
        let other_translucent = stack.iter().any(|(t, o)| *o < 1.0 && class(*t) == "other");
        let any_other = classes.contains(&"other");
        let name = format!("{stack:?}");
        for policy in [
            CompositeColorPolicy::FixedBt601,
            CompositeColorPolicy::BottomLayerTag,
            CompositeColorPolicy::Unknown,
        ] {
            let plan = plan_of(policy, &layers);
            let off = reasons(&plan, &caps(false));
            let on = reasons(&plan, &caps(true));
            let ctx = format!("{policy:?} {name}");
            // A stack with no 240M picture is judged as it was, whatever the cap says.
            if !has_240 {
                assert_eq!(off, on, "{ctx}: the cap moved a stack without a 240M picture");
            }
            match policy {
                CompositeColorPolicy::FixedBt601 => {
                    assert_eq!(plan.canvas.matrix, YuvMatrix::Bt601, "{ctx}");
                    // No layer is ever converted under a fixed policy, so no mixed-matrix or unknown-matrix reason, and a matrix matters only to a
                    // translucent layer, which needs a table for its own.
                    assert!(
                        !on.iter().any(|u| matches!(
                            u,
                            Unsupported::MatrixUnknown(_)
                                | Unsupported::MixedMatrices
                                | Unsupported::MixedMatricesWith(..)
                                | Unsupported::UnmeasuredPolicy(_)
                        )),
                        "{ctx}: {on:?}"
                    );
                    let translucent_240 = stack.iter().any(|(t, o)| *o < 1.0 && class(*t) == "240");
                    if translucent_240 && !other_translucent {
                        assert!(
                            off.iter().any(|u| matches!(u, Unsupported::TranslucentMatrix(..))),
                            "{ctx}: the cap off must refuse a translucent 240M layer: {off:?}"
                        );
                        assert!(
                            !on.iter().any(|u| matches!(u, Unsupported::TranslucentMatrix(..))),
                            "{ctx}: the cap on must draw it: {on:?}"
                        );
                    }
                    if other_translucent {
                        assert!(
                            on.iter().any(|u| matches!(u, Unsupported::TranslucentMatrix(..))),
                            "{ctx}: a translucent layer of a tag with no table is drawn: {on:?}"
                        );
                    }
                    // An opaque stack asks nothing of the matrix at all.
                    if stack.iter().all(|(_, o)| *o >= 1.0) {
                        assert!(!on.iter().any(about_matrix), "{ctx}: {on:?}");
                        assert!(!off.iter().any(about_matrix), "{ctx}: {off:?}");
                    }
                }
                CompositeColorPolicy::BottomLayerTag => {
                    let bottom = class(stack[0].0);
                    let want = match bottom {
                        "240" => YuvMatrix::Smpte240m,
                        "709" => YuvMatrix::Bt709,
                        "2020" => YuvMatrix::Bt2020,
                        _ => YuvMatrix::Bt601,
                    };
                    if !any_other && all_same {
                        assert_eq!(plan.canvas.matrix, want, "{ctx}");
                    }
                    if has_240 {
                        assert!(!off.is_empty(), "{ctx}: a 240M picture is drawn with the cap off: {off:?}");
                        assert!(
                            off.iter()
                                .any(|u| matches!(u, Unsupported::MatrixUnknown(_) | Unsupported::MixedMatricesWith(..))),
                            "{ctx}: {off:?}"
                        );
                        if !all_same || any_other {
                            assert!(!on.is_empty(), "{ctx}: a 240M picture beside another matrix is drawn: {on:?}");
                        }
                        if all_same && !any_other {
                            assert!(
                                on.is_empty(),
                                "{ctx}: a stack of 240M alone is refused with the cap on: {on:?}"
                            );
                            assert_eq!(plan.canvas.matrix, YuvMatrix::Smpte240m, "{ctx}");
                        }
                    }
                    if any_other {
                        assert!(on.iter().any(|u| matches!(u, Unsupported::MatrixUnknown(_))), "{ctx}: {on:?}");
                    }
                }
                CompositeColorPolicy::Unknown => {
                    if has_240 {
                        assert!(
                            !on.is_empty(),
                            "{ctx}: a 240M picture is drawn under an unmeasured policy: {on:?}"
                        );
                        assert!(!off.is_empty(), "{ctx}");
                    }
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 1300, "only {checked} judgements were made");
}

/// Pictures that are 240M by tag and not 8-bit 4:2:0 or not YCbCr, and the stack's matrix: an RGB picture beside a 240M composite is refused
/// (it is taken into YCbCr with the composite's matrix), and a 240M tag on one never makes the picture a 240M one.
#[test]
fn a_240m_tag_on_a_picture_that_is_not_ycbcr_is_not_a_240m_picture() {
    for fmt in ["rgb24", "bgr0", "gbrp"] {
        let plan = plan_of(
            CompositeColorPolicy::BottomLayerTag,
            &[("yuv420p", Some("smpte240m"), 1.0), (fmt, Some("smpte240m"), 1.0)],
        );
        let why = reasons(&plan, &caps(true));
        assert!(
            why.iter().any(|u| matches!(u, Unsupported::RgbInStack(YuvMatrix::Smpte240m))),
            "{fmt}: {why:?}"
        );
        let alone = plan_of(CompositeColorPolicy::BottomLayerTag, &[(fmt, Some("smpte240m"), 1.0)]);
        assert_eq!(
            alone.canvas.matrix,
            YuvMatrix::Bt601,
            "{fmt}: an RGB picture is BT.601 whatever it is tagged"
        );
        let why = reasons(&alone, &caps(true));
        assert!(
            !why.iter().any(|u| matches!(u, Unsupported::MatrixUnknown(_))),
            "{fmt}: {why:?}"
        );
    }
    // A picture with an alpha channel, gray, 4:2:2 / 4:4:4 / 10-bit / full range sources tagged 240M, alone and translucent: whatever the plan
    // says of them with the cap on, it says at least what it says of the same picture tagged BT.709 (no 240M picture is drawn that a BT.709 one
    // of the same format would not be).
    for fmt in [
        "yuva420p",
        "gray",
        "yuv422p",
        "yuv444p",
        "yuv420p10le",
        "yuv422p10le",
        "yuvj420p",
        "yuvj444p",
        "yuv440p",
        "nv12",
        "p010le",
    ] {
        for op in [1.0, 0.5] {
            for policy in [CompositeColorPolicy::FixedBt601, CompositeColorPolicy::BottomLayerTag] {
                let c = caps(true);
                let a = reasons(&plan_of(policy, &[(fmt, Some("smpte240m"), op)]), &c);
                let b = reasons(&plan_of(policy, &[(fmt, Some("bt709"), op)]), &c);
                let strip = |v: &[Unsupported]| -> Vec<String> {
                    v.iter()
                        .filter(|u| !about_matrix(u))
                        .map(|u| u.to_string().replace("smpte240m", "bt709").replace("SMPTE 240M", "BT.709"))
                        .collect()
                };
                for r in strip(&b) {
                    assert!(
                        strip(&a).contains(&r),
                        "{fmt} {op} {policy:?}: a BT.709 picture is refused for `{r}`, the 240M one is not: {a:?}"
                    );
                }
            }
        }
    }
}

/// A cut as an export holds it (`PlanMode::Motion`): for every order of clips of 240M and other matrices, plain or dissolving, the frames
/// the plan draws under a negotiating policy are those of one matrix, and a frame with a 240M picture in it is never drawn beside a
/// picture of another matrix or in a composite that is not 240M's.
#[test]
fn a_cut_of_240m_and_others_draws_one_matrix_at_a_time() {
    use kerf_core::{Transition, TransitionKind};
    let tags: [Option<&str>; 4] = [None, Some("bt709"), Some("smpte240m"), Some("bt2020nc")];
    let mut frames_checked = 0usize;
    let mut drawn_240 = 0usize;
    let mut orders: Vec<Vec<Option<&str>>> = Vec::new();
    for &a in &tags {
        for &b in &tags {
            orders.push(vec![a, b]);
            for &c in &tags {
                orders.push(vec![a, b, c]);
            }
        }
    }
    for tags_in_order in &orders {
        for dissolve in [false, true] {
            for translucent in [None, Some(0), Some(1)] {
                let assets: Vec<Asset> = tags_in_order.iter().map(|t| asset(video(640, 360, "yuv420p", *t))).collect();
                let clips: Vec<Clip> = assets
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let mut c = Clip::new(a.id, 0.0, 1.0, i as f64);
                        if dissolve && i > 0 {
                            c.transition_in = Some(Transition {
                                kind: TransitionKind::Crossfade,
                                duration: 0.4,
                            });
                        }
                        if translucent == Some(i) {
                            c.transform.opacity = 0.6;
                        }
                        c
                    })
                    .collect();
                let tl = timeline(vec![clips]);
                let opts = ExportOptions {
                    fps: Some(30.0),
                    ..ExportOptions::default()
                };
                let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(CompositeColorPolicy::BottomLayerTag))
                    .expect("planner");
                let n = tags_in_order.len() as u64 * 30;
                for k in (0..n).step_by(3) {
                    let plan = planner.at_frame(k).expect("plan");
                    let size = plan.size(u32::MAX);
                    let on = plan.reasons(&caps(true), size);
                    let off = plan.reasons(&caps(false), size);
                    let on_screen: Vec<&str> = plan.layers.iter().map(|l| class(l.stream.color_space.as_deref())).collect();
                    let has_240 = on_screen.contains(&"240");
                    let ctx = format!("{tags_in_order:?} dissolve {dissolve} translucent {translucent:?} frame {k}");
                    if has_240 {
                        assert!(!off.is_empty(), "{ctx}: a 240M layer is drawn with the cap off");
                        if on.is_empty() {
                            drawn_240 += 1;
                            assert_eq!(
                                plan.canvas.matrix,
                                YuvMatrix::Smpte240m,
                                "{ctx}: drawn in a composite that is not 240M's"
                            );
                            assert!(
                                on_screen.iter().all(|c| *c == "240"),
                                "{ctx}: drawn beside another matrix: {on_screen:?}"
                            );
                        }
                    } else if plan.canvas.matrix == YuvMatrix::Smpte240m && !on_screen.is_empty() {
                        assert!(
                            !on.is_empty(),
                            "{ctx}: a layer of another matrix is drawn in a 240M composite: {on_screen:?}"
                        );
                    }
                    frames_checked += 1;
                }
            }
        }
    }
    assert!(
        frames_checked > 2000 && drawn_240 > 100,
        "{frames_checked} frames, {drawn_240} of them 240M and drawn"
    );
}

/// The 10-bit delivery of a 240M picture is refused with the cap on, whichever way the stack goes: the 8-bit control is not.
#[test]
fn a_10_bit_delivery_of_a_240m_layer_is_refused() {
    use kerf_core::{Delivery, Fit};
    for (tag, refused) in [(Some("smpte240m"), true), (Some("bt709"), false), (None, false)] {
        for pix in ["yuv420p10le", "yuv422p10le"] {
            let a = asset(video(1280, 720, "yuv420p", tag));
            let mut tl = timeline(vec![vec![Clip::new(a.id, 0.0, 4.0, 0.0)]]);
            tl.format = Some(Delivery::new(1920, 1080, Fit::Cover));
            let opts = ExportOptions {
                fps: Some(30.0),
                pix_fmt: Some(pix.into()),
                ..ExportOptions::default()
            };
            let planner = Planner::new(
                &tl,
                std::slice::from_ref(&a),
                &opts,
                PlanRequest::motion(CompositeColorPolicy::BottomLayerTag),
            )
            .expect("planner");
            let plan = planner.at_frame(60).expect("plan");
            let mut c = caps(true);
            c.ten_bit = kerf_core::TenBitKinds::P420.union(kerf_core::TenBitKinds::P422);
            let why = plan.reasons(&c, (plan.canvas.width, plan.canvas.height));
            let ten = why.iter().any(|u| matches!(u, Unsupported::DeliveryWith(..)));
            assert_eq!(ten, refused, "{tag:?} {pix}: {why:?}");
            if refused {
                assert!(!why.is_empty());
            }
        }
    }
}

/// The luma weights a matrix carries are the ones swscale's coefficient table is derived from (`yuv2rgb.c`'s own comment: `crv = 255 / 224 * 65536 *
/// (1 - Kr) / 0.5`, `cbu` the same with Kb, `cgu = ... * (Kb / Kg) * (1 - Kb) / 0.5`, `cgv = ... * (Kr / Kg) * (1 - Kr) / 0.5`): the published
/// integers of `ff_yuv2rgb_coeffs` for BT.601, BT.709, BT.2020 and SMPTE 240M, to the unit. A weight that is a digit off moves a coefficient by
/// hundreds.
#[test]
fn the_luma_weights_of_every_matrix_derive_swscales_published_table() {
    let published: [(YuvMatrix, [i64; 4]); 4] = [
        (YuvMatrix::Bt601, [104_597, 132_201, 25_675, 53_279]),
        (YuvMatrix::Bt709, [117_489, 138_438, 13_975, 34_925]),
        (YuvMatrix::Bt2020, [110_013, 140_363, 12_277, 42_626]),
        (YuvMatrix::Smpte240m, [117_579, 136_230, 16_907, 35_559]),
    ];
    for (matrix, table) in published {
        let (kr, kb) = matrix.weights();
        let kg = 1.0 - kr - kb;
        let k = 255.0 / 224.0 * 65536.0 / 0.5;
        let derived = [
            k * (1.0 - kr),
            k * (1.0 - kb),
            k * (kb / kg) * (1.0 - kb),
            k * (kr / kg) * (1.0 - kr),
        ];
        for (n, (d, t)) in derived.iter().zip(table).enumerate() {
            assert!(
                (d.round() as i64 - t).abs() <= 1,
                "{matrix:?} coefficient {n}: the weights ({kr}, {kb}) derive {d:.2}, swscale's table has {t}"
            );
        }
    }
    assert_eq!(YuvMatrix::Smpte240m.weights(), (0.212, 0.087));
}
