//! `checker_d10b_anyway`: the refusals of the 10-bit deliveries drawn anyway. For every frame the plan refuses (`DeliveryWith` / `DeliveryFrame` /
//! `Delivery`), the best the compositor could hand out without the 10-bit chain is the 8-bit plan's composite taken up to the delivery by the last
//! `scale` (`delivery10::to_ten_bit`); it is held to the export's planes. A refusal is **needed** if some frame it refuses is not that (the sample
//! counts are in the report); a refusal for which every frame would have been exact is **wider than it has to be**.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_d10b_anyway -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr, clippy::single_range_in_vec_init)]

mod common;
#[path = "common/d10b.rs"]
mod d10b;

use std::ops::Range;

use d10b::{clip, media, options, timeline, LAYOUTS};
use kerf_core::model::{BlendMode, Transition, TransitionKind, VideoEffect};
use kerf_core::{Asset, Clip, ExportOptions, Fit, PlanRequest, Planner, TenBitLayout, TextOverlay, Timeline};
use kerf_gpu::delivery10;
use kerf_gpu::MotionFrames;

#[derive(Default, Debug)]
struct Anyway {
    refused: usize,
    undrawable: usize,
    exact: usize,
    inexact: usize,
    differing: usize,
    max: u32,
    why: std::collections::BTreeMap<String, usize>,
}

impl Anyway {
    fn needed(&self) -> Option<bool> {
        (self.exact + self.inexact > 0).then_some(self.inexact > 0)
    }
}

fn anyway(
    name: &str,
    tl: &Timeline,
    assets: &[Asset],
    layout: TenBitLayout,
    opts10: &ExportOptions,
    frames: Range<u64>,
) -> Anyway {
    d10b::measured();
    let comp = d10b::compositor(Some(3));
    let caps = comp.caps();
    let kernel = kerf_gpu::scaler::measured().model().kernel;
    let mut opts8 = opts10.clone();
    opts8.pix_fmt = None;
    let p10 = Planner::new(tl, assets, opts10, PlanRequest::motion(d10b::policy()).alone()).expect("planner");
    let p8 = Planner::new(tl, assets, &opts8, PlanRequest::motion(d10b::policy()).alone()).expect("planner");
    let first = p10.at_frame(0).expect("plan");
    let (w, h) = (first.canvas.width as usize, first.canvas.height as usize);
    let args = kerf_core::export_args(tl, assets, "OUT", opts10).expect("argv");
    let bytes = d10b::frame_samples(layout, (w, h)) * 2;
    let exported = d10b::run_frames(&d10b::raw_run(&args, Some(3), &[], frames.end - 1), bytes, &|k| {
        frames.contains(&k)
    });
    let mut out = Anyway::default();
    let mut motion = MotionFrames::new(d10b::frame_source());
    for k in frames {
        let plan10 = p10.at_frame(k).expect("plan");
        let size = (plan10.canvas.width, plan10.canvas.height);
        let why = plan10.reasons(&caps, size);
        if why.is_empty() {
            continue;
        }
        out.refused += 1;
        for u in &why {
            *out.why.entry(u.kind().to_string()).or_default() += 1;
        }
        let Some(want) = exported.get(&k) else { continue };
        let want = d10b::samples(want);
        let plan8 = p8.at_frame(k).expect("plan");
        if !plan8.reasons(&caps, size).is_empty() {
            out.undrawable += 1;
            continue;
        }
        let plan8 = plan8.for_decode(&caps, size);
        let Ok(pictures) = motion.frames(&plan8) else {
            out.undrawable += 1;
            continue;
        };
        let Ok(c8) = comp.composite_yuv_counted(&plan8, &pictures, motion.counts(), size) else {
            out.undrawable += 1;
            continue;
        };
        let ten = delivery10::to_ten_bit([&c8.y, &c8.u, &c8.v], (w, h), layout, kernel).expect("tables");
        let mut at = 0;
        let mut apart = 0;
        for plane in [&ten.y, &ten.u, &ten.v] {
            let reference = &want[at..at + plane.data.len()];
            apart += plane.data.iter().zip(reference).filter(|(a, b)| a != b).count();
            out.max = out.max.max(
                plane
                    .data
                    .iter()
                    .zip(reference)
                    .map(|(a, b)| u32::from(a.abs_diff(*b)))
                    .max()
                    .unwrap_or(0),
            );
            at += plane.data.len();
        }
        if apart == 0 {
            out.exact += 1;
        } else {
            out.inexact += 1;
            out.differing += apart;
        }
    }
    let verdict = match out.needed() {
        None => "nothing drawable anyway".to_string(),
        Some(true) => "NEEDED (some frame drawn anyway is not the export's)".to_string(),
        Some(false) => "WIDER THAN NEEDED (every frame drawn anyway is the export's)".to_string(),
    };
    eprintln!(
        "{name:<46} {} | refused {:>3} undrawable {:>3} exact {:>3} inexact {:>3} ({} samples, max {}) | {verdict} | {:?}",
        layout.pix_fmt(),
        out.refused,
        out.undrawable,
        out.exact,
        out.inexact,
        out.differing,
        out.max,
        out.why
    );
    out
}

fn both(
    name: &str,
    tl: &Timeline,
    assets: &[Asset],
    opts: impl Fn(TenBitLayout) -> ExportOptions,
    frames: Range<u64>,
) -> Vec<(TenBitLayout, Anyway)> {
    LAYOUTS
        .iter()
        .map(|&l| (l, anyway(name, tl, assets, l, &opts(l), frames.clone())))
        .collect()
}

fn noise() -> Asset {
    media("noise-160x90-30-3", "noise", (160, 90), "30", 3.0, "yuv420p", None)
}

fn pattern() -> Asset {
    media("tst-160x90-30-3", "testsrc2", (160, 90), "30", 3.0, "yuv420p", None)
}

/// Every refusal of the layer list and the frame list, drawn anyway. Prints one line per case and layout; fails only if a case could not be set up.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_the_refusals_drawn_anyway() {
    let a = noise();
    let b = pattern();
    let pair = [a.clone(), b.clone()];
    let o = |l: TenBitLayout| options(l, 30.0);
    let mut results: Vec<(String, Vec<(TenBitLayout, Anyway)>)> = Vec::new();
    let mut put = |name: &str, r: Vec<(TenBitLayout, Anyway)>| results.push((name.to_string(), r));
    for canvas in [(96, 54), (128, 72)] {
        let t = |tracks: Vec<Vec<Clip>>, ov: Vec<TextOverlay>| timeline(tracks, ov, canvas, Fit::Cover, 30.0);
        // dissolve / dip to white / slide
        let mk = |kind: TransitionKind| {
            let c1 = clip(&a, 0.0, 0.4, 0.0);
            let mut c2 = clip(&b, 0.0, 0.4, 0.4);
            c2.transition_in = Some(Transition { kind, duration: 0.2 });
            t(vec![vec![c1, c2]], vec![])
        };
        put(
            &format!("a dissolve {canvas:?}"),
            both("dissolve", &mk(TransitionKind::Crossfade), &pair, o, 0..24),
        );
        put(
            &format!("a dip to white {canvas:?}"),
            both("dip to white", &mk(TransitionKind::DipToWhite), &pair, o, 0..24),
        );
        // colour, effect, opacity, zoom, blend
        let one = |edit: &dyn Fn(&mut Clip)| {
            let mut c = clip(&a, 0.0, 0.5, 0.0);
            c.fade_in = 0.1;
            edit(&mut c);
            t(vec![vec![c]], vec![])
        };
        put(
            &format!("a colour correction {canvas:?}"),
            both("colour", &one(&|c| c.color.brightness = 0.1), &pair, o, 0..15),
        );
        put(
            &format!("an effect {canvas:?}"),
            both("invert", &one(&|c| c.effects.push(VideoEffect::Invert)), &pair, o, 0..15),
        );
        put(
            &format!("an opacity {canvas:?}"),
            both("opacity", &one(&|c| c.transform.opacity = 0.6), &pair, o, 0..15),
        );
        put(
            &format!("a zoom out {canvas:?}"),
            both("zoom 0.5", &one(&|c| c.transform.scale = 0.5), &pair, o, 0..15),
        );
        put(
            &format!("a zoom in {canvas:?}"),
            both("zoom 1.5", &one(&|c| c.transform.scale = 1.5), &pair, o, 0..15),
        );
        put(
            &format!("a blend mode {canvas:?}"),
            both(
                "multiply",
                &t(
                    vec![
                        vec![clip(&b, 0.0, 0.5, 0.0)],
                        {
                            let mut c = clip(&a, 0.0, 0.4, 0.05);
                            c.blend_mode = BlendMode::Multiply;
                            vec![c]
                        }
                        .into_iter()
                        .collect(),
                    ],
                    vec![],
                ),
                &pair,
                o,
                0..15,
            ),
        );
        // letterbox
        let lb = timeline(
            vec![vec![clip(&a, 0.0, 0.5, 0.0)]],
            vec![],
            (canvas.0, canvas.1 + 18),
            Fit::Contain,
            30.0,
        );
        put(&format!("a letterbox {canvas:?}"), both("letterbox", &lb, &pair, o, 0..15));
        // text over no clip, text over a clip
        let gap = t(
            vec![vec![clip(&a, 0.0, 0.2, 0.0), clip(&b, 0.0, 0.2, 0.6)]],
            vec![TextOverlay::new("Out now", 0.25, 0.5)],
        );
        put(
            &format!("text over no clip {canvas:?}"),
            both("text over no clip", &gap, &pair, o, 0..24),
        );
        // other colour corrections (the 4:2:0 delivery's eq runs on the 8-bit picture)
        #[allow(clippy::type_complexity)]
        let edits: Vec<(&str, Box<dyn Fn(&mut Clip)>)> = vec![
            ("contrast", Box::new(|c: &mut Clip| c.color.contrast = 1.3)),
            ("saturation", Box::new(|c: &mut Clip| c.color.saturation = 1.4)),
            ("gamma", Box::new(|c: &mut Clip| c.color.gamma = 1.2)),
            ("temperature", Box::new(|c: &mut Clip| c.color.temperature = 0.3)),
            ("a rotation", Box::new(|c: &mut Clip| c.transform.rotation = 10.0)),
            (
                "a position",
                Box::new(|c: &mut Clip| {
                    c.transform.scale = 0.8;
                    c.transform.pos_x = 0.1;
                }),
            ),
            ("a crop", Box::new(|c: &mut Clip| c.transform.crop_left = 0.2)),
        ];
        for (name, edit) in &edits {
            put(&format!("{name} {canvas:?}"), both(name, &one(&**edit), &pair, o, 0..15));
        }
        // a named scaler
        for name in ["lanczos", "bilinear", "spline", "neighbor", "bicubic"] {
            let plain = t(
                vec![vec![{
                    let mut c = clip(&a, 0.0, 0.4, 0.0);
                    c.fade_in = 0.1;
                    c
                }]],
                vec![],
            );
            put(
                &format!("the scaler {name} {canvas:?}"),
                both(
                    &format!("scaler {name}"),
                    &plain,
                    &pair,
                    |l| ExportOptions {
                        scaler: Some(name.to_string()),
                        ..options(l, 30.0)
                    },
                    0..12,
                ),
            );
        }
    }
    // sources of other formats
    for (name, fmt, tags) in [
        ("yuv422p", "yuv422p", None),
        ("yuv444p", "yuv444p", None),
        ("yuv420p10le", "yuv420p10le", None),
        ("yuvj420p", "yuvj420p", None),
        (
            "bt709",
            "yuv420p",
            Some("colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv"),
        ),
        (
            "pc range",
            "yuv420p",
            Some("colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=pc"),
        ),
    ] {
        let s = media(&format!("src-{name}"), "noise", (160, 90), "30", 2.0, fmt, tags);
        let mut c = clip(&s, 0.1, 0.5, 0.0);
        c.fade_in = 0.1;
        let tl = timeline(vec![vec![c]], vec![], (96, 54), Fit::Cover, 30.0);
        put(
            &format!("source {name}"),
            both(&format!("source {name}"), &tl, std::slice::from_ref(&s), o, 0..15),
        );
    }
    // a still image, and a blurred fill
    let png = {
        let dir = common::media_dir("d10b-media");
        let path = dir.join("still.png");
        if !path.exists() {
            common::ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90",
                "-frames:v",
                "1",
                path.to_str().unwrap(),
            ]);
        }
        kerf_core::Project::probe_asset(&path).expect("probe the still")
    };
    let mut c = clip(&png, 0.0, 0.5, 0.0);
    c.fade_in = 0.1;
    let tl = timeline(vec![vec![c]], vec![], (96, 54), Fit::Cover, 30.0);
    put(
        "a still image",
        both("still image", &tl, std::slice::from_ref(&png), o, 0..15),
    );
    let tall = media("src-tall", "noise", (90, 160), "30", 2.0, "yuv420p", None);
    let tl = timeline(vec![vec![clip(&tall, 0.0, 0.5, 0.0)]], vec![], (96, 54), Fit::Blur, 30.0);
    put(
        "a blurred fill",
        both("blur fill", &tl, std::slice::from_ref(&tall), o, 0..15),
    );
    // a cut of two matrices, an odd picture, an odd frame
    let m709 = media(
        "src-bt709",
        "noise",
        (160, 90),
        "30",
        2.0,
        "yuv420p",
        Some("colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv"),
    );
    let m601 = media(
        "src-yuv420p-bt601",
        "noise",
        (160, 90),
        "30",
        2.0,
        "yuv420p",
        Some("colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m:range=tv"),
    );
    let tl = timeline(
        vec![vec![clip(&m709, 0.0, 0.3, 0.0), clip(&m601, 0.0, 0.3, 0.3)]],
        vec![],
        (96, 54),
        Fit::Cover,
        30.0,
    );
    put("a cut of two matrices", both("two matrices", &tl, &[m709, m601], o, 0..18));
    let odd = media("src-odd", "noise", (161, 91), "30", 2.0, "yuv420p", None);
    let tl = timeline(vec![vec![clip(&odd, 0.0, 0.5, 0.0)]], vec![], (96, 54), Fit::Cover, 30.0);
    put(
        "an odd picture",
        both("odd picture", &tl, std::slice::from_ref(&odd), o, 0..15),
    );
    let tl = timeline(vec![vec![clip(&odd, 0.0, 0.5, 0.0)]], vec![], (96, 54), Fit::Contain, 30.0);
    put(
        "an odd picture, Contain",
        both("odd picture contain", &tl, std::slice::from_ref(&odd), o, 0..15),
    );

    eprintln!("\nNECESSITY SUMMARY");
    let mut wide = Vec::new();
    for (name, rows) in &results {
        for (layout, r) in rows {
            let verdict = match r.needed() {
                None => "undrawable anyway",
                Some(true) => "needed",
                Some(false) => "WIDER THAN NEEDED",
            };
            if verdict == "WIDER THAN NEEDED" {
                wide.push(format!("{name} {}", layout.pix_fmt()));
            }
            eprintln!("  {name:<34} {:<12} {verdict}", layout.pix_fmt());
        }
    }
    eprintln!("over-wide refusals: {wide:?}");
}
