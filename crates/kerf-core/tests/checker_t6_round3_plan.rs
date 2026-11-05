//! T6 fix round 3, the plan checked on its own (no FFmpeg: the assets are built by hand): what text the fit picture has, which chain the plan
//! says it is, whether any caps set can ever draw a text with a `crop` besides the guard, the scaler as the engine spells it, the blend
//! rule of a moving still, and the layer a refusal names. The text is classified here by hand (a splitter of the top-level filters), not by
//! the plan's `FitShape`, so the two are held against each other.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_t6_round3_plan -- --nocapture
//! ```

#![allow(
    clippy::print_stderr,
    clippy::semicolon_if_nothing_returned,
    clippy::type_complexity,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;

use kerf_core::{
    Asset, BlendExtras, BlendKinds, BlendMode, Clip, CompositeColorPolicy, Delivery, EffectKinds, ExportOptions, FadeEdge, Fit,
    FitShape, GpuCaps, GradeKinds, Keyframe, LayerRef, PlanMode, PlanRequest, Planner, Property, PropertyKey, Rational,
    RenderPlan, StillChain, StillKinds, StreamKind, Timeline, Track, Unsupported, YuvMatrix,
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
    c.blur_fill = true;
    c.matrix_convert = true;
    c.blend = BlendKinds::ALL;
    c.blend_extras = BlendExtras::ALL;
    c
}

// ---- the text of the copy, classified by hand ---------------------------------------------------------------------------

/// The top-level filters of a chain (a comma inside single quotes is the filter's own).
fn split_filters(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for ch in s.chars() {
        match ch {
            '\'' => {
                quoted = !quoted;
                cur.push(ch);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn name_of(part: &str) -> &str {
    part.split('=').next().unwrap_or("")
}

#[derive(Debug, Clone, PartialEq)]
enum Class {
    Engine {
        bicubic: bool,
    },
    EngineCrop {
        bicubic: bool,
    },
    /// A guard `crop` and `scale`s, and no other `crop`.
    Staged {
        bicubic: bool,
    },
    /// A guard `crop`, `scale`s and some other `crop` (before a scale: the clip's static crop; after one: the Cover overhang).
    Cropped {
        bicubic: bool,
        before: bool,
        after: bool,
    },
    Unknown(String),
}

/// `scale=W:H` or `scale=W:H:flags=bicubic`: `Some(bicubic)`.
fn staged_scale(p: &str) -> Option<bool> {
    let a: Vec<&str> = p.strip_prefix("scale=")?.split(':').collect();
    match a.as_slice() {
        [w, h] if digits(w) && digits(h) => Some(false),
        [w, h, "flags=bicubic"] if digits(w) && digits(h) => Some(true),
        _ => None,
    }
}

/// `scale=W:H:force_original_aspect_ratio=increase[:flags=bicubic]`: `(W:H, bicubic)`.
fn engine_scale(p: &str) -> Option<(String, bool)> {
    let a: Vec<&str> = p.strip_prefix("scale=")?.split(':').collect();
    match a.as_slice() {
        [w, h, "force_original_aspect_ratio=increase"] if digits(w) && digits(h) => Some((format!("{w}:{h}"), false)),
        [w, h, "force_original_aspect_ratio=increase", "flags=bicubic"] if digits(w) && digits(h) => {
            Some((format!("{w}:{h}"), true))
        }
        _ => None,
    }
}

fn plain_crop(p: &str) -> Option<Vec<String>> {
    let a: Vec<String> = p.strip_prefix("crop=")?.split(':').map(String::from).collect();
    a.iter().all(|x| digits(x)).then_some(a)
}

/// The class of a copy's text (the filters ahead of the closing conversion).
fn class_of(filters: &str) -> Class {
    let parts = split_filters(filters);
    let unknown = || Class::Unknown(filters.to_string());
    match parts.as_slice() {
        [g, rest @ ..] if g.starts_with("crop=w='if(eq(iw,") && g.ends_with(":exact=1") => {
            let (mut scales, mut flagged, mut crops) = (0, 0, 0);
            let (mut before, mut after) = (false, false);
            let mut seen_scale = false;
            for p in rest {
                match name_of(p) {
                    "scale" => match staged_scale(p) {
                        Some(b) => {
                            scales += 1;
                            flagged += usize::from(b);
                            seen_scale = true;
                        }
                        None => return unknown(),
                    },
                    "crop" => {
                        if plain_crop(p).is_none_or(|a| a.len() != 4) {
                            return unknown();
                        }
                        crops += 1;
                        if seen_scale {
                            after = true;
                        } else {
                            before = true;
                        }
                    }
                    _ => return unknown(),
                }
            }
            if scales == 0 || (flagged != 0 && flagged != scales) {
                return unknown();
            }
            let bicubic = flagged != 0;
            if crops == 0 {
                Class::Staged { bicubic }
            } else {
                Class::Cropped { bicubic, before, after }
            }
        }
        [s, c] => match (engine_scale(s), plain_crop(c)) {
            (Some((size, bicubic)), Some(crop)) if crop.join(":") == size => Class::Engine { bicubic },
            _ => unknown(),
        },
        [k, s, c] if k.starts_with("crop=w=iw*") => {
            let a: Vec<&str> = k.strip_prefix("crop=").unwrap().split(':').collect();
            let well_formed = a.len() == 4
                && ["w=iw*", "h=ih*", "x=iw*", "y=ih*"]
                    .iter()
                    .zip(&a)
                    .all(|(prefix, x)| x.strip_prefix(prefix).is_some_and(|n| n.parse::<f64>().is_ok()));
            match (well_formed, engine_scale(s), plain_crop(c)) {
                (true, Some((size, bicubic)), Some(crop)) if crop.join(":") == size => Class::EngineCrop { bicubic },
                _ => unknown(),
            }
        }
        _ => unknown(),
    }
}

fn shape_of(class: &Class) -> Option<FitShape> {
    match class {
        Class::Engine { .. } => Some(FitShape::Engine),
        Class::EngineCrop { .. } => Some(FitShape::EngineCrop),
        Class::Staged { .. } => Some(FitShape::Staged),
        Class::Cropped { .. } => Some(FitShape::Cropped),
        Class::Unknown(_) => None,
    }
}

fn bicubic_of(class: &Class) -> bool {
    match class {
        Class::Engine { bicubic }
        | Class::EngineCrop { bicubic }
        | Class::Staged { bicubic }
        | Class::Cropped { bicubic, .. } => *bicubic,
        Class::Unknown(_) => false,
    }
}

#[test]
fn the_splitter_reads_the_texts_it_is_held_to() {
    let guard = "crop=w='if(eq(iw,640)*eq(ih,426),640,0)':h=426:x=0:y=0:exact=1";
    assert_eq!(class_of(&format!("{guard},scale=270:180")), Class::Staged { bicubic: false });
    assert_eq!(
        class_of(&format!("{guard},scale=270:180:flags=bicubic")),
        Class::Staged { bicubic: true }
    );
    assert_eq!(
        class_of(&format!("{guard},scale=320:213,crop=320:180:0:16,scale=256:144")),
        Class::Cropped {
            bicubic: false,
            before: false,
            after: true
        }
    );
    assert_eq!(
        class_of(&format!("{guard},crop=512:426:64:0,scale=270:180")),
        Class::Cropped {
            bicubic: false,
            before: true,
            after: false
        }
    );
    assert_eq!(
        class_of("scale=320:180:force_original_aspect_ratio=increase,crop=320:180"),
        Class::Engine { bicubic: false }
    );
    assert_eq!(
        class_of(
            "crop=w=iw*0.9:h=ih*0.95:x=iw*0.1:y=ih*0.05,scale=320:180:force_original_aspect_ratio=increase:flags=bicubic,crop=320:180"
        ),
        Class::EngineCrop { bicubic: true }
    );
    // A text of no shape, or one that is two things, is not a class.
    for bad in [
        "scale=320:180,crop=320:180".to_string(),
        format!("{guard},scale=270:180,scale=100:100:flags=bicubic"),
        format!("{guard},pad=320:180"),
        format!("{guard},crop=1:2:3"),
        "scale=320:180:force_original_aspect_ratio=increase,crop=322:180".to_string(),
    ] {
        assert!(matches!(class_of(&bad), Class::Unknown(_)), "{bad}");
    }
}

// ---- the sweep ----------------------------------------------------------------------------------------------------------

type Edit = Box<dyn Fn(&mut Clip)>;

fn moves() -> Vec<(&'static str, Edit)> {
    let keys = |ks: Vec<Keyframe>| -> Edit { Box::new(move |c: &mut Clip| c.keyframes = ks.clone()) };
    vec![
        ("off a", keys(vec![key(0.0, 1.0, -0.04, 0.0), key(1.0, 1.2, 0.04, -0.02)])),
        ("off b", keys(vec![key(0.0, 1.0, -0.03, 0.0), key(1.0, 1.18, 0.03, -0.02)])),
        ("zoom out", keys(vec![key(0.0, 0.9, 0.0, 0.0), key(1.0, 0.7, 0.04, 0.0)])),
        (
            "held 0.8 pan",
            keys(vec![key(0.0, 0.8, -0.04, 0.0), key(1.0, 0.8, 0.04, -0.02)]),
        ),
        (
            "held 0.5 pan",
            keys(vec![key(0.0, 0.5, -0.05, 0.0), key(1.0, 0.5, 0.05, 0.03)]),
        ),
        (
            "held 1.0 pan",
            keys(vec![key(0.0, 1.0, -0.05, 0.0), key(1.0, 1.0, 0.05, 0.0)]),
        ),
        (
            "held 1.25 pan",
            keys(vec![key(0.0, 1.25, -0.1, 0.0), key(1.0, 1.25, 0.1, 0.0)]),
        ),
        ("inside", keys(vec![key(0.0, 1.3, 0.03, 0.02), key(1.0, 1.05, -0.01, 0.0)])),
        ("zoom only", keys(vec![key(0.0, 1.0, 0.0, 0.0), key(1.0, 1.3, 0.0, 0.0)])),
        (
            "static 0.8, pan keyed",
            Box::new(|c: &mut Clip| {
                c.transform.scale = 0.8;
                c.set_property_keys(
                    Property::PosX,
                    vec![PropertyKey::new(0.0, -0.05), PropertyKey::new(1.0, 0.05)],
                );
            }),
        ),
        (
            "static 1.4, pan keyed",
            Box::new(|c: &mut Clip| {
                c.transform.scale = 1.4;
                c.set_property_keys(
                    Property::PosY,
                    vec![PropertyKey::new(0.0, -0.05), PropertyKey::new(1.0, 0.05)],
                );
            }),
        ),
        (
            "pan keyed only",
            Box::new(|c: &mut Clip| {
                c.set_property_keys(
                    Property::PosX,
                    vec![PropertyKey::new(0.0, -0.05), PropertyKey::new(1.0, 0.05)],
                );
            }),
        ),
    ]
}

const CROPS: [(f64, f64, f64, f64); 6] = [
    (0.0, 0.0, 0.0, 0.0),
    (0.1, 0.05, 0.0, 0.0),
    (0.0, 0.0, 0.2, 0.13),
    (0.07, 0.11, 0.03, 0.09),
    (0.3, 0.0, 0.0, 0.3),
    (0.013, 0.007, 0.011, 0.017),
];

fn timeline(clip: Clip, canvas: (u32, u32), fit: Fit) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        format: Delivery::new(canvas.0, canvas.1, fit).with_fps(Some(10.0)).ok(),
        ..Timeline::new()
    }
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

#[derive(Default)]
struct Tally {
    layers: usize,
    by_class: BTreeMap<String, usize>,
    cropped_before: usize,
    cropped_after: usize,
    cropped_both: usize,
    drawn_by_everything: BTreeMap<String, usize>,
}

fn label(class: &Class) -> String {
    match class {
        Class::Engine { bicubic } => format!("Engine{}", if *bicubic { "+bicubic" } else { "" }),
        Class::EngineCrop { bicubic } => format!("EngineCrop{}", if *bicubic { "+bicubic" } else { "" }),
        Class::Staged { bicubic } => format!("Staged{}", if *bicubic { "+bicubic" } else { "" }),
        Class::Cropped { bicubic, .. } => format!("Cropped{}", if *bicubic { "+bicubic" } else { "" }),
        Class::Unknown(t) => format!("UNKNOWN {t}"),
    }
}

/// Judge the fit picture of every layer of `plan` at `size`; returns the failures.
fn judge(plan: &RenderPlan, size: (u32, u32), scaler: Option<&str>, case: &str, tally: &mut Tally, fails: &mut Vec<String>) {
    let caps = everything();
    let why = plan.reasons(&caps, size);
    for (n, layer) in plan.layers.iter().enumerate() {
        let Some(fit) = plan.fit_copy(layer, size) else {
            continue;
        };
        tally.layers += 1;
        let class = class_of(&fit.filters);
        *tally.by_class.entry(label(&class)).or_default() += 1;
        let Some(shape) = shape_of(&class) else {
            fails.push(format!("{case}: a text of no shape: {}", fit.filters));
            continue;
        };
        if fit.shape != shape {
            fails.push(format!(
                "{case}: the plan says {:?}, the text is {class:?}: {}",
                fit.shape, fit.filters
            ));
        }
        let chain = plan.still_chain(layer, size);
        let want = StillChain::edge(shape, scaler == Some("bicubic"));
        if chain != Some(want) {
            fails.push(format!(
                "{case}: the chain is {chain:?}, the text and the delivery say {want:?}"
            ));
        }
        if bicubic_of(&class) != (scaler == Some("bicubic")) {
            fails.push(format!(
                "{case}: the text names the bicubic scaler: {}, the delivery's scaler is {scaler:?}: {}",
                bicubic_of(&class),
                fit.filters
            ));
        }
        let Some(kind) = plan.still_kind(layer) else {
            fails.push(format!("{case}: a fit made for a layer of no measured kind"));
            continue;
        };
        let refused_for_chain = why.iter().any(|u| match u {
            Unsupported::StillMotion(l, msg) => l.index == n && msg.contains("has not measured FFmpeg making"),
            _ => false,
        });
        let held = StillKinds::ALL.contains_chain(kind, want);
        if let Class::Cropped { before, after, .. } = &class {
            tally.cropped_before += usize::from(*before && !*after);
            tally.cropped_after += usize::from(*after && !*before);
            tally.cropped_both += usize::from(*before && *after);
            if held || StillKinds::ALL.chain(want) != StillKinds::NONE || kind.chain(want) != StillKinds::NONE {
                fails.push(format!("{case}: the caps hold a Cropped chain: {}", fit.filters));
            }
            if want.measurable() {
                fails.push(format!("{case}: a Cropped chain is measurable"));
            }
            if !refused_for_chain {
                fails.push(format!(
                    "{case}: layer {n} with a crop besides the guard is not refused though every chain is measured: {} / {why:?}",
                    fit.filters
                ));
            }
        } else {
            if !held {
                fails.push(format!("{case}: a chain of ALL is not held: {want:?}"));
            }
            if refused_for_chain {
                fails.push(format!(
                    "{case}: layer {n} refused for its chain though every chain is measured: {}",
                    fit.filters
                ));
            }
            if why.is_empty() {
                *tally.drawn_by_everything.entry(label(&class)).or_default() += 1;
            }
        }
        // A frame the most generous caps draw has no Cropped text in any layer.
        if why.is_empty() && matches!(class, Class::Cropped { .. }) {
            fails.push(format!("{case}: a frame with a Cropped layer is drawn: {}", fit.filters));
        }
    }
}

#[test]
fn no_text_with_a_crop_besides_the_guard_is_a_chain_and_none_is_drawn_whatever_the_caps() {
    let mut rng = Lcg(0x7_3a11_0c4d);
    let canvases: [(u32, u32); 8] = [
        (320, 180),
        (426, 240),
        (322, 182),
        (180, 320),
        (256, 256),
        (100, 100),
        (640, 360),
        (64, 36),
    ];
    let all_moves = moves();
    let mut tally = Tally::default();
    let mut fails: Vec<String> = Vec::new();
    let (mut plans, mut cases) = (0usize, 0usize);
    for n in 0..9000usize {
        let canvas = *rng.pick(&canvases);
        let k = [1u32, 2, 3, 4, 7][(rng.next() % 5) as usize];
        // Pictures of the frame's own shape, one pixel off it either way, and ones of other shapes.
        let picture = match rng.next() % 10 {
            0 => (canvas.0 * k, canvas.1 * k),
            1 => (canvas.0 * k + 1, canvas.1 * k),
            2 => (canvas.0 * k, canvas.1 * k + 1),
            3 => (canvas.0 * k - 1, canvas.1 * k),
            4 => (canvas.0 * k + 2, canvas.1 * k + 2),
            5 => (canvas.0 * k, canvas.1 * k - 1),
            _ => *rng.pick(&[
                (1200, 800),
                (801, 601),
                (160, 90),
                (90, 160),
                (641, 479),
                (1920, 1080),
                (3000, 400),
                (400, 3000),
                (64, 64),
                (640, 360),
                (641, 360),
                (640, 361),
                (321, 181),
                (322, 182),
                (323, 181),
                (2, 2),
                (1, 1000),
            ]),
        };
        let pix = *rng.pick(&["rgb24", "yuvj420p"]);
        let asset = image(pix, picture.0, picture.1);
        let fit = *rng.pick(&[Fit::Cover, Fit::Cover, Fit::Contain, Fit::Contain, Fit::Blur]);
        let (mname, edit) = rng.pick(&all_moves);
        let crop = *rng.pick(&CROPS);
        let scaler = *rng.pick(&[None, None, Some("bicubic")]);
        let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
        edit(&mut clip);
        clip.transform.crop_left = crop.0;
        clip.transform.crop_top = crop.1;
        clip.transform.crop_right = crop.2;
        clip.transform.crop_bottom = crop.3;
        let tl = timeline(clip, canvas, fit);
        let opts = ExportOptions {
            fps: Some(10.0),
            scaler: scaler.map(String::from),
            ..ExportOptions::default()
        };
        for mode in [PlanMode::Motion, PlanMode::Still] {
            let request = if mode == PlanMode::Still {
                PlanRequest::still(CompositeColorPolicy::FixedBt601)
            } else {
                PlanRequest::motion(CompositeColorPolicy::FixedBt601)
            };
            let Ok(planner) = Planner::new(&tl, std::slice::from_ref(&asset), &opts, request) else {
                continue;
            };
            plans += 1;
            for frame in [0u64, 4, 9] {
                let plan = match mode {
                    PlanMode::Motion => planner.at_frame(frame),
                    PlanMode::Still => planner.at(frame as f64 / 10.0),
                };
                let Ok(plan) = plan else { continue };
                let delivery = (plan.canvas.width, plan.canvas.height);
                let sizes: Vec<(u32, u32)> = if mode == PlanMode::Still {
                    vec![delivery, plan.size(64)]
                } else {
                    vec![delivery]
                };
                for size in sizes {
                    let plan = plan.clone().for_render(size);
                    cases += 1;
                    let case = format!(
                        "#{n} {pix} {}x{} / {mname} / {fit:?} {}x{} / crop {crop:?} / {scaler:?} / {mode:?} f{frame} at {size:?}",
                        picture.0, picture.1, canvas.0, canvas.1
                    );
                    judge(&plan, size, scaler, &case, &mut tally, &mut fails);
                }
            }
        }
    }
    eprintln!("R3 PLAN {plans} plans, {cases} frames judged, {} fit pictures", tally.layers);
    for (class, n) in &tally.by_class {
        eprintln!("R3 PLAN class {class:<24} {n}");
    }
    for (class, n) in &tally.drawn_by_everything {
        eprintln!("R3 PLAN drawn by the most generous caps {class:<24} {n}");
    }
    eprintln!(
        "R3 PLAN Cropped: crop before the scale only {}, after it only {}, both {}",
        tally.cropped_before, tally.cropped_after, tally.cropped_both
    );
    for f in fails.iter().take(40) {
        eprintln!("R3 PLAN FAIL {f}");
    }
    assert!(fails.is_empty(), "{} failures; first: {}", fails.len(), fails[0]);
    // The sweep has reached every shape (a sweep that never makes a Cropped text proves nothing about refusing it).
    for want in ["Engine", "EngineCrop", "Staged", "Staged+bicubic", "Engine+bicubic"] {
        assert!(
            tally.by_class.get(want).copied().unwrap_or(0) >= 20,
            "too few {want}: {:?}",
            tally.by_class
        );
    }
    for want in ["Cropped", "Cropped+bicubic"] {
        assert!(
            tally.by_class.get(want).copied().unwrap_or(0) >= 100,
            "too few {want}: {:?}",
            tally.by_class
        );
    }
    assert!(
        tally.cropped_before >= 20 && tally.cropped_after >= 20,
        "a Cropped text of each cause (the clip's crop, the Cover overhang): {} / {}",
        tally.cropped_before,
        tally.cropped_after
    );
    assert!(
        tally.drawn_by_everything.get("Staged").copied().unwrap_or(0) >= 20,
        "Staged texts are drawn by the most generous caps: {:?}",
        tally.drawn_by_everything
    );
}

/// The reaches the task names, one by one: each case builds the shape and says which text it must have.
#[test]
fn the_shapes_a_staged_text_takes_by_construction() {
    let canvas = (320u32, 180u32);
    let cases: Vec<(&str, (u32, u32), Fit, Edit, Option<(f64, f64, f64, f64)>, &str)> = vec![
        (
            "contain, wide picture",
            (1200, 800),
            Fit::Contain,
            moves().remove(0).1,
            None,
            "Staged",
        ),
        (
            "contain, tall picture",
            (400, 3000),
            Fit::Contain,
            moves().remove(0).1,
            None,
            "Staged",
        ),
        (
            "contain, zoom out",
            (1200, 800),
            Fit::Contain,
            moves().remove(2).1,
            None,
            "Staged",
        ),
        (
            "contain behind a crop",
            (1200, 800),
            Fit::Contain,
            moves().remove(0).1,
            Some(CROPS[1]),
            "Cropped",
        ),
        (
            "contain behind a tiny crop",
            (1200, 800),
            Fit::Contain,
            moves().remove(0).1,
            Some(CROPS[5]),
            "Cropped",
        ),
        (
            "held zoom, picture of the frame's shape",
            (960, 540),
            Fit::Cover,
            moves().remove(3).1,
            None,
            "Staged",
        ),
        (
            "held zoom, picture of the frame's shape, 1x",
            (320, 180),
            Fit::Cover,
            moves().remove(3).1,
            None,
            "Staged",
        ),
        (
            "held zoom, wider picture (overhang)",
            (1200, 800),
            Fit::Cover,
            moves().remove(3).1,
            None,
            "Cropped",
        ),
        (
            "held zoom, taller picture (overhang)",
            (320, 400),
            Fit::Cover,
            moves().remove(3).1,
            None,
            "Cropped",
        ),
        (
            "held zoom, 961 wide (the cover rounds to the frame)",
            (961, 540),
            Fit::Cover,
            moves().remove(3).1,
            None,
            "Staged",
        ),
        (
            "held zoom, 963 wide (the cover is a pixel over)",
            (963, 540),
            Fit::Cover,
            moves().remove(3).1,
            None,
            "Cropped",
        ),
        (
            "held zoom behind a crop",
            (960, 540),
            Fit::Cover,
            moves().remove(3).1,
            Some(CROPS[2]),
            "Cropped",
        ),
        ("cover engine", (1200, 800), Fit::Cover, moves().remove(0).1, None, "Engine"),
        (
            "cover engine behind a crop",
            (1200, 800),
            Fit::Cover,
            moves().remove(0).1,
            Some(CROPS[1]),
            "EngineCrop",
        ),
    ];
    let mut fails = Vec::new();
    for (name, picture, fit, edit, crop, want) in cases {
        for scaler in [None, Some("bicubic")] {
            let asset = image("rgb24", picture.0, picture.1);
            let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
            edit(&mut clip);
            if let Some(c) = crop {
                clip.transform.crop_left = c.0;
                clip.transform.crop_top = c.1;
                clip.transform.crop_right = c.2;
                clip.transform.crop_bottom = c.3;
            }
            let tl = timeline(clip, canvas, fit);
            let opts = ExportOptions {
                fps: Some(10.0),
                scaler: scaler.map(String::from),
                ..ExportOptions::default()
            };
            let planner = Planner::new(
                &tl,
                std::slice::from_ref(&asset),
                &opts,
                PlanRequest::motion(CompositeColorPolicy::FixedBt601),
            )
            .unwrap();
            let plan = planner.at_frame(4).unwrap();
            let layer = &plan.layers[0];
            let Some(fit_copy) = plan.fit_copy(layer, canvas) else {
                fails.push(format!("{name}: no fit picture"));
                continue;
            };
            let class = class_of(&fit_copy.filters);
            let got = label(&class);
            let got = got.trim_end_matches("+bicubic");
            eprintln!("R3 SHAPE {name:<46} {scaler:?}: {got}  {}", fit_copy.filters);
            if got != want {
                fails.push(format!("{name} {scaler:?}: {got}, wanted {want}: {}", fit_copy.filters));
            }
            let refused = plan
                .reasons(&everything(), canvas)
                .iter()
                .any(|u| matches!(u, Unsupported::StillMotion(..)));
            if refused != (want == "Cropped") {
                fails.push(format!("{name} {scaler:?}: refused {refused}, the text is {got}"));
            }
        }
    }
    assert!(fails.is_empty(), "{fails:#?}");
}

#[test]
fn a_chain_that_cannot_be_measured_is_in_no_set_and_the_eight_that_can_are_in_all_of_them() {
    assert_eq!(StillChain::COUNT, 8);
    assert_eq!(StillChain::ALL.len(), 8);
    let kinds = [StillKinds::YUVJ420P, StillKinds::RGB24, StillKinds::YUV420P];
    let mut seen = 0u32;
    for kind in kinds {
        for (i, chain) in StillChain::ALL.into_iter().enumerate() {
            assert!(chain.measurable());
            assert_eq!(chain.index(), i);
            let one = kind.chain(chain);
            assert_ne!(one, StillKinds::NONE);
            assert!(StillKinds::ALL.contains_chain(kind, chain));
            assert!(!StillKinds::NONE.contains_chain(kind, chain));
            assert!(kind.all_chains().contains(one));
            assert!(!one.minus_chain(kind, chain).contains(one));
            // each (kind, chain) is a bit of its own
            seen += 1;
        }
        for shape in [FitShape::Cropped] {
            for bicubic in [false, true] {
                let c = StillChain::edge(shape, bicubic);
                assert!(!c.measurable() && !c.is_base() && c.index() == StillChain::COUNT);
                assert_eq!(kind.chain(c), StillKinds::NONE);
                assert!(!StillKinds::ALL.contains_chain(kind, c));
                assert!(!kind.all_chains().contains_chain(kind, c));
                assert_eq!(StillKinds::ALL.minus_chain(kind, c), StillKinds::ALL);
            }
        }
    }
    assert_eq!(seen, 24);
    let mut union = StillKinds::NONE;
    for kind in kinds {
        for chain in StillChain::ALL {
            let before = union;
            union = union.union(kind.chain(chain));
            assert_ne!(before, union, "two chains share a bit");
        }
    }
    assert_eq!(union, StillKinds::ALL);
    // A kind constant holds its two base chains and no other.
    for kind in kinds {
        for chain in StillChain::ALL {
            assert_eq!(kind.contains(kind.chain(chain)), chain.is_base(), "{kind:?} {chain:?}");
        }
    }
}

// ---- the scaler as the engine spells it ---------------------------------------------------------------------------------------

#[test]
fn the_scaler_is_named_as_the_engine_spells_it_in_the_chain_the_flags_the_text_and_the_refusal() {
    let spellings: [Option<&str>; 17] = [
        None,
        Some("bicubic"),
        Some("Bicubic"),
        Some("BICUBIC"),
        Some("biCubic"),
        Some("bicubic "),
        Some(" bicubic"),
        Some("bicubic\n"),
        Some("bicubic:flags=x"),
        Some(""),
        Some("lanczos"),
        Some("Lanczos"),
        Some("bilinear"),
        Some("neighbor"),
        Some("spline"),
        Some("bicubıc"),
        Some("bicubic\0"),
    ];
    let asset = image("rgb24", 1200, 800);
    let mut fails = Vec::new();
    for scaler in spellings {
        // inside the frame (the route), off its edge (the engine's fit), a Contain frame (the stages)
        for (what, fit, mv) in [
            ("route", Fit::Cover, 7usize),
            ("engine", Fit::Cover, 0),
            ("staged", Fit::Contain, 0),
        ] {
            let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
            moves()[mv].1(&mut clip);
            let tl = timeline(clip, (320, 180), fit);
            let opts = ExportOptions {
                fps: Some(10.0),
                scaler: scaler.map(String::from),
                ..ExportOptions::default()
            };
            let planner = Planner::new(
                &tl,
                std::slice::from_ref(&asset),
                &opts,
                PlanRequest::motion(CompositeColorPolicy::FixedBt601),
            )
            .unwrap();
            let plan = planner.at_frame(4).unwrap();
            let layer = &plan.layers[0];
            let named = scaler == Some("bicubic");
            let case = format!("{what} with the scaler {scaler:?}");
            // The engine's own list is exact: the spelling that writes `:flags=bicubic` into a still's text is "bicubic" and no other.
            let chain = plan.still_chain(layer, (320, 180));
            match chain {
                Some(c) if c.bicubic == named => {}
                other => fails.push(format!(
                    "{case}: the chain is {other:?}, the engine names the bicubic scaler: {named}"
                )),
            }
            if let Some(still) = &layer.still {
                if (still.flags == ":flags=bicubic") != named {
                    fails.push(format!("{case}: the route's flags are {:?}", still.flags));
                }
                if scaler.is_some_and(|s| !s.is_empty() && s != "bicubic") && !still.flags.is_empty() && scaler != Some("lanczos")
                {
                    // a name in the engine's list writes it; others write none
                }
            } else if let Some(fit_copy) = plan.fit_copy(layer, (320, 180)) {
                if fit_copy.filters.contains(":flags=bicubic") != named {
                    fails.push(format!("{case}: the text is {}", fit_copy.filters));
                }
                if fit_copy.filters.contains("flags=") && !named {
                    fails.push(format!(
                        "{case}: a scaler flag in the text of a delivery that names none: {}",
                        fit_copy.filters
                    ));
                }
            } else {
                fails.push(format!("{case}: neither route nor fit picture"));
            }
            // The compositor implements bicubic (swscale's default): the spellings that name another scaler are refused, the one
            // spelling of a name outside the engine's list (which writes no flags) is the default scaler's.
            let refused = plan
                .size_free_reasons(&everything())
                .iter()
                .any(|u| matches!(u, Unsupported::Scaler(_)));
            let default_or_bicubic = scaler.is_none_or(|s| s.eq_ignore_ascii_case("bicubic"));
            if refused == default_or_bicubic {
                fails.push(format!("{case}: Scaler refusal {refused}"));
            }
        }
    }
    assert!(fails.is_empty(), "{fails:#?}");
}

// ---- the blend rule keys on the route ----------------------------------------------------------------------------------------

#[test]
fn a_blend_on_every_kind_of_moving_still_is_refused_and_on_a_still_that_does_not_move_it_is_not() {
    let asset = image("rgb24", 1200, 800);
    let caps = everything();
    let mut fails = Vec::new();
    let arrangements: Vec<(&str, Edit, bool)> = vec![
        ("inside the frame (the route)", Box::new(|c: &mut Clip| moves()[7].1(c)), true),
        (
            "off the edge (the placement)",
            Box::new(|c: &mut Clip| moves()[0].1(c)),
            false,
        ),
        ("zoom only (the route)", Box::new(|c: &mut Clip| moves()[8].1(c)), true),
        (
            "a keyed crop and nothing else (the route, no placement)",
            Box::new(|c: &mut Clip| {
                c.set_property_keys(
                    Property::CropZoom,
                    vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(1.0, 1.5)],
                );
            }),
            true,
        ),
        (
            "a keyed crop pan and zoom (the route, no placement)",
            Box::new(|c: &mut Clip| {
                c.set_property_keys(
                    Property::CropZoom,
                    vec![PropertyKey::new(0.0, 1.3), PropertyKey::new(1.0, 1.6)],
                );
                c.set_property_keys(Property::CropX, vec![PropertyKey::new(0.0, -0.2), PropertyKey::new(1.0, 0.2)]);
            }),
            true,
        ),
    ];
    let mut crop_only_refused = 0;
    for (name, edit, route) in &arrangements {
        for mode in BlendMode::ALL.into_iter().filter(|m| !m.is_normal()) {
            for plan_mode in [PlanMode::Motion, PlanMode::Still] {
                let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
                edit(&mut clip);
                clip.blend_mode = mode;
                let tl = timeline(clip, (320, 180), Fit::Cover);
                let opts = ExportOptions {
                    fps: Some(10.0),
                    ..ExportOptions::default()
                };
                let request = if plan_mode == PlanMode::Still {
                    PlanRequest::still(CompositeColorPolicy::FixedBt601)
                } else {
                    PlanRequest::motion(CompositeColorPolicy::FixedBt601)
                };
                let planner = Planner::new(&tl, std::slice::from_ref(&asset), &opts, request).unwrap();
                let plan = match plan_mode {
                    PlanMode::Motion => planner.at_frame(4).unwrap(),
                    PlanMode::Still => planner.at(0.4).unwrap(),
                };
                let layer = &plan.layers[0];
                let case = format!("{name} / {mode:?} / {plan_mode:?}");
                if plan.takes_still_route(layer) != *route {
                    fails.push(format!(
                        "{case}: takes_still_route {} where the arrangement means {route}",
                        plan.takes_still_route(layer)
                    ));
                    continue;
                }
                let blend_refusal = plan
                    .size_free_reasons(&caps)
                    .into_iter()
                    .any(|u| matches!(&u, Unsupported::BlendMode(l, m, why) if l.index == 0 && *m == mode && why.contains("moving zoom or position")));
                if !blend_refusal {
                    fails.push(format!("{case}: not refused for the move"));
                }
                crop_only_refused += usize::from(name.contains("crop") && blend_refusal);
            }
        }
    }
    // A blend on a still that does not move is not refused for a move it does not make.
    for mode in BlendMode::ALL.into_iter().filter(|m| !m.is_normal()) {
        let clip = {
            let mut c = Clip::new(asset.id, 0.0, 1.0, 0.0);
            c.blend_mode = mode;
            c
        };
        let tl = timeline(clip, (320, 180), Fit::Cover);
        let opts = ExportOptions {
            fps: Some(10.0),
            ..ExportOptions::default()
        };
        let planner = Planner::new(
            &tl,
            std::slice::from_ref(&asset),
            &opts,
            PlanRequest::motion(CompositeColorPolicy::FixedBt601),
        )
        .unwrap();
        let plan = planner.at_frame(4).unwrap();
        let moved = plan
            .size_free_reasons(&everything())
            .into_iter()
            .any(|u| matches!(&u, Unsupported::BlendMode(_, _, why) if why.contains("moving zoom or position")));
        if moved {
            fails.push(format!("{mode:?} on a still that does not move: refused for a move"));
        }
    }
    assert!(
        crop_only_refused >= 2 * 2 * 8,
        "the crop-only arrangements were not judged: {crop_only_refused}"
    );
    assert!(fails.is_empty(), "{fails:#?}");
}

// ---- which layer a refusal names ----------------------------------------------------------------------------------------------

#[test]
fn every_refusal_that_is_about_a_layer_names_it_and_every_refusal_of_the_frame_names_none() {
    let l = |index: usize| LayerRef {
        index,
        name: "x".to_string(),
    };
    let r = Rational::from_fps(30.0).unwrap();
    let layered: Vec<Unsupported> = vec![
        Unsupported::NoPicture(l(7)),
        Unsupported::BlurFillWith(l(7), "x"),
        Unsupported::Hdr(l(7)),
        Unsupported::AlphaPicture(l(7), String::new()),
        Unsupported::NotKnownOpaque(l(7), String::new()),
        Unsupported::TranslucentMatrix(l(7), String::new()),
        Unsupported::GradedFullRange(l(7), String::new()),
        Unsupported::GradeSteep(l(7), 1),
        Unsupported::Grade(l(7), "x"),
        Unsupported::GradeWith(l(7), "x"),
        Unsupported::Effects(l(7), "x"),
        Unsupported::Mask(l(7)),
        Unsupported::MaskWith(l(7), "x"),
        Unsupported::Matte(l(7), "x"),
        Unsupported::Reframe(l(7)),
        Unsupported::Fade(l(7), FadeEdge::In, false),
        Unsupported::FadeOnTranslucent(l(7)),
        Unsupported::Travel(l(7)),
        Unsupported::DipToWhite(l(7), kerf_core::DipWhy::Format),
        Unsupported::FullRangeNegotiated(l(7)),
        Unsupported::FullRangeResized(l(7), "x"),
        Unsupported::SpeedRamp(l(7)),
        Unsupported::Stabilized(l(7)),
        Unsupported::BlendMode(l(7), BlendMode::Multiply, "x"),
        Unsupported::Tail(l(7)),
        Unsupported::KeyedOpacity(l(7)),
        Unsupported::KeyedColor(l(7)),
        Unsupported::KeyedMask(l(7)),
        Unsupported::KeyedCrop(l(7)),
        Unsupported::StillMotion(l(7), String::new()),
        Unsupported::KeyedZoom(l(7)),
        Unsupported::KeyedPlacement(l(7)),
        Unsupported::Rotation(l(7), "x"),
        Unsupported::PlacedFormat(l(7), String::new()),
        Unsupported::Behavior(l(7)),
        Unsupported::ZoomBehind(l(7), "x"),
        Unsupported::FoldedFit(l(7), "x"),
        Unsupported::MixedMatricesWith(l(7), "x"),
        Unsupported::ScalerApproximate(l(7)),
        Unsupported::MatrixUnknown(7),
        Unsupported::GradedInFullRangeStack(7),
        Unsupported::Shrink(7, (2, 2), (1, 1)),
        Unsupported::FormatResize(7, (2, 2), (1, 1), String::new()),
        Unsupported::Geometry(7, String::new()),
    ];
    for u in &layered {
        assert_eq!(u.layer(), Some(7), "{u:?}");
    }
    let frame_wide: Vec<Unsupported> = vec![
        Unsupported::Motion,
        Unsupported::Delivery("yuv444p".into()),
        Unsupported::Gif,
        Unsupported::BlurFill,
        Unsupported::PickRate { canvas: r, clips: r },
        Unsupported::FullRangeCut,
        Unsupported::Text,
        Unsupported::TextWithoutFont,
        Unsupported::TextSequence(String::new()),
        Unsupported::Svg,
        Unsupported::Scaler("lanczos".into()),
        Unsupported::MixedMatrices,
        Unsupported::UnmeasuredPolicy(YuvMatrix::Bt709),
        Unsupported::RgbInStack(YuvMatrix::Bt709),
    ];
    for u in &frame_wide {
        assert_eq!(u.layer(), None, "{u:?}");
    }
    // The same through plans: a refusal the planner raises for the layer at index 1 of a stack says 1, and a stack's refusal that is about
    // one layer of it (a graded layer over a full-range picture; a layer whose matrix is unknown) names that layer.
    let full = image("yuvj420p", 640, 360);
    let plain = image("rgb24", 640, 360);
    let mut graded = Clip::new(plain.id, 0.0, 1.0, 0.0);
    graded.color.brightness = 0.2;
    let tl = Timeline {
        tracks: vec![
            Track {
                clips: vec![Clip::new(full.id, 0.0, 1.0, 0.0)],
                ..Track::new(StreamKind::Video, "V1".to_string())
            },
            Track {
                clips: vec![graded],
                ..Track::new(StreamKind::Video, "V2".to_string())
            },
        ],
        format: Delivery::new(320, 180, Fit::Cover).with_fps(Some(10.0)).ok(),
        ..Timeline::new()
    };
    let opts = ExportOptions {
        fps: Some(10.0),
        ..ExportOptions::default()
    };
    let assets = [full, plain];
    for policy in [CompositeColorPolicy::BottomLayerTag, CompositeColorPolicy::Unknown] {
        let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(policy)).unwrap();
        let plan = planner.at_frame(2).unwrap();
        for u in plan.size_free_reasons(&everything()) {
            let text = format!("{u:?}");
            // The index the payload carries (a `LayerRef { index: N` or a first field `(N,`), compared with what `layer()` says.
            let carried = text
                .split("index: ")
                .nth(1)
                .or_else(|| text.split_once('(').map(|(_, rest)| rest))
                .and_then(|rest| {
                    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    n.parse::<usize>().ok()
                });
            assert_eq!(u.layer(), carried, "{policy:?}: {text}");
            eprintln!("R3 LAYER {policy:?}: {text} -> {:?}", u.layer());
        }
    }
}
