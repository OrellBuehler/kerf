//! The Motion plan against the **evaluated** export graph, on a grid of frames.
//!
//! Parsing `enable=`, `fade=` or `-ss` back out of the graph and comparing it with the
//! plan would be tautological — the builders format from `ClipTiming`, so it can only
//! restate what was just printed. What is not is *evaluation*: `keyframe_expr` writes a
//! piecewise curve as nested `if(lt(..))` text, the plan samples the same curve with
//! `interpolate`, and nothing but this test checks that the two are one function. Every
//! output frame of five frame rates, over a clip with animated zoom / position / rotation
//! / opacity, two with a slide or push, and an animated title, the graph's expressions are
//! evaluated at `ffmpeg_frame_time(k)` by the ~50-line evaluator below and compared
//! with what the plan says: the sampled `transform`, the overlay's `x` / `y` against the
//! layer's `origin` (offset and all), the title's position, opacity and `between`.
//!
//! **What this does and does not say.** It checks the *grammar*: that the text the builders
//! write, evaluated at the output frame's time, is the curve the plan samples. It does not say
//! the graph reads each expression at that time, or that what follows a filter keeps up with
//! it; that the *pictures* agree is `rendered.rs`'s and `keyed_zoom.rs`'s (every output frame of
//! a keyed zoom against `transform_at`) and the parity harness's to say. (The zoom
//! (`scale eval=frame`) used to be read at the *source* frame's time and not always shown at
//! all, and a Motion plan still refuses a moving zoom, `Unsupported::KeyedZoom`: the export
//! has since been fixed (`video_clip_chain` puts the zoom after `fps` and last), and lifting
//! that refusal is for the compositor that would draw it.)

use std::f64::consts::PI;

use super::*;
use crate::clip_timing::{ffmpeg_frame_time, Rational};
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track};
use crate::model::{Easing, Fit, Keyframe, TextKeyframe, Transition, TransitionKind};
use crate::planner::{PlanRequest, Planner};

/// FFmpeg's expression grammar as `keyframe_expr`, `motion_expr` and the overlay /
/// `drawtext` positions use it: `+ - * /`, unary minus, `if lt between hypot`, variables.
struct Eval<'a> {
    s: &'a [u8],
    i: usize,
    vars: &'a [(&'a str, f64)],
    depth: usize,
}

/// How many expressions libavutil lets nest (`stack_index` in `av_expr_parse`: the whole text,
/// each bracket and each function argument is one); the next is refused with `EMFILE`, which
/// the filter reports as `Invalid argument`.
const NESTING_LIMIT: usize = 100;

fn eval(src: &str, vars: &[(&str, f64)]) -> f64 {
    let mut e = Eval {
        s: src.as_bytes(),
        i: 0,
        vars,
        depth: 0,
    };
    let v = e.nested();
    assert_eq!(e.i, src.len(), "unparsed tail of `{src}`");
    v
}

impl Eval<'_> {
    fn peek(&self) -> u8 {
        self.s.get(self.i).copied().unwrap_or(0)
    }
    fn eat(&mut self, c: u8) {
        assert_eq!(
            self.peek(),
            c,
            "expected `{}` at {} of `{}`",
            c as char,
            self.i,
            String::from_utf8_lossy(self.s)
        );
        self.i += 1;
    }
    fn nested(&mut self) -> f64 {
        self.depth += 1;
        assert!(
            self.depth <= NESTING_LIMIT,
            "libavutil refuses an expression nested {} deep",
            self.depth
        );
        let v = self.sum();
        self.depth -= 1;
        v
    }
    fn sum(&mut self) -> f64 {
        let mut v = self.term();
        loop {
            match self.peek() {
                b'+' => {
                    self.i += 1;
                    v += self.term();
                }
                b'-' => {
                    self.i += 1;
                    v -= self.term();
                }
                _ => return v,
            }
        }
    }
    fn term(&mut self) -> f64 {
        let mut v = self.unary();
        loop {
            match self.peek() {
                b'*' => {
                    self.i += 1;
                    v *= self.unary();
                }
                b'/' => {
                    self.i += 1;
                    v /= self.unary();
                }
                _ => return v,
            }
        }
    }
    fn unary(&mut self) -> f64 {
        if self.peek() == b'-' {
            self.i += 1;
            -self.unary()
        } else {
            self.atom()
        }
    }
    fn atom(&mut self) -> f64 {
        let c = self.peek();
        if c == b'(' {
            self.i += 1;
            let v = self.nested();
            self.eat(b')');
            return v;
        }
        let start = self.i;
        if c.is_ascii_digit() || c == b'.' {
            while self.peek().is_ascii_digit() || self.peek() == b'.' {
                self.i += 1;
            }
            return std::str::from_utf8(&self.s[start..self.i]).unwrap().parse().unwrap();
        }
        while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i]).unwrap();
        if self.peek() != b'(' {
            return self
                .vars
                .iter()
                .find(|v| v.0 == name)
                .unwrap_or_else(|| panic!("unknown `{name}`"))
                .1;
        }
        self.i += 1;
        let mut args = vec![self.nested()];
        while self.peek() == b',' {
            self.i += 1;
            args.push(self.nested());
        }
        self.eat(b')');
        let flag = |b: bool| f64::from(u8::from(b));
        match (name, args.as_slice()) {
            ("if", [c, a, b]) => {
                if *c != 0.0 {
                    *a
                } else {
                    *b
                }
            }
            ("lt", [a, b]) => flag(a < b),
            ("between", [x, lo, hi]) => flag(x >= lo && x <= hi),
            ("hypot", [a, b]) => a.hypot(*b),
            _ => panic!("function `{name}/{}`", args.len()),
        }
    }
}

/// `s` split at `sep` where it is not inside single quotes.
fn split_outside_quotes(s: &str, sep: char) -> Vec<&str> {
    let (mut parts, mut start, mut quoted) = (Vec::new(), 0, false);
    for (i, c) in s.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            c if c == sep && !quoted => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// The `key=value` options of a filter's argument text, quotes stripped.
fn parse_options(body: &str) -> Vec<(String, String)> {
    split_outside_quotes(body, ':')
        .into_iter()
        .filter_map(|o| o.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_matches('\'').to_string()))
        .collect()
}

/// The options of the first filter called `name` in `chain` (a pad label after the last
/// filter is not part of its options).
fn options(chain: &str, name: &str) -> Option<Vec<(String, String)>> {
    all_options(chain, name).into_iter().next()
}

/// The options of every filter called `name` in `chain`, in order: a chain has more than one
/// `scale` (the fit, and a zoom), and the first is not the one that zooms.
fn all_options(chain: &str, name: &str) -> Vec<Vec<(String, String)>> {
    let chain = chain.rsplit_once('[').map_or(chain, |(f, _)| f);
    split_outside_quotes(chain, ',')
        .into_iter()
        .filter_map(|f| f.strip_prefix(&format!("{name}=")))
        .map(parse_options)
        .collect()
}

fn opt<'a>(opts: &'a [(String, String)], key: &str) -> Option<&'a str> {
    opts.iter().find(|o| o.0 == key).map(|o| o.1.as_str())
}

fn keyed(time: f64, scale: f64, (pos_x, pos_y): (f64, f64), rotation: f64, opacity: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x,
        pos_y,
        rotation,
        opacity,
        easing: Default::default(),
    }
}

fn transition(kind: TransitionKind, duration: f64) -> Option<Transition> {
    Some(Transition { kind, duration })
}

/// The cuts under test, each on its own 9:16 Contain frame over 16:9 footage.
fn cuts(asset: &crate::model::Asset) -> Vec<(&'static str, Timeline)> {
    let a = asset.id;
    // Animated zoom, position, rotation and opacity on off-grid keyframes, and a title.
    let mut moving = make_clip(a, 2.0, 7.0, 1.013);
    moving.keyframes = vec![
        keyed(0.0, 1.0, (0.0, 0.0), 0.0, 1.0),
        keyed(0.37, 1.4, (0.1, -0.1), 15.0, 0.8),
        keyed(1.13, 0.8, (-0.2, 0.05), -10.0, 0.3),
        keyed(2.5, 1.2, (0.3, 0.0), 30.0, 1.0),
        keyed(4.9, 1.0, (0.0, 0.2), 0.0, 0.5),
    ];
    let mut animated = timeline_of(vec![video_track(vec![moving.clone()])]);
    let mut title = TextOverlay::new("Fish", 1.5, 5.25);
    title.keyframes = vec![
        TextKeyframe {
            time: 0.0,
            pos_x: 0.2,
            pos_y: 0.8,
            opacity: 0.0,
        },
        TextKeyframe {
            time: 0.7,
            pos_x: 0.5,
            pos_y: 0.7,
            opacity: 1.0,
        },
        TextKeyframe {
            time: 3.75,
            pos_x: 0.9,
            pos_y: 0.2,
            opacity: 0.25,
        },
    ];
    animated.overlays.push(title);
    // A slide onto a clip with a static offset and zoom, a push onto an animated one, and a
    // slide onto an untouched one (whose padded full frame is what travels).
    let pair = |incoming: Clip| timeline_of(vec![video_track(vec![make_clip(a, 0.0, 2.0, 1.0), incoming])]);
    let mut slid = make_clip(a, 10.0, 14.0, 3.0);
    slid.transform.scale = 0.8;
    slid.transform.pos_x = 0.05;
    slid.transition_in = transition(TransitionKind::SlideLeft, 0.7);
    let mut pushed = moving;
    pushed.timeline_start = 3.0;
    pushed.transition_in = transition(TransitionKind::PushUp, 0.9);
    let mut plain = make_clip(a, 10.0, 14.0, 3.0);
    plain.transition_in = transition(TransitionKind::SlideRight, 0.5);
    // The same motion along every kind of easing: the graph writes each curve as the polyline
    // `transform_at` samples, a hold as a step.
    let mut eased = pushed.clone();
    eased.timeline_start = 0.4;
    eased.transition_in = None;
    let easings = [
        Easing::EaseInOut,
        Easing::Hold,
        Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 0.1,
        },
        Easing::EaseOut,
    ];
    for (k, e) in eased.keyframes.iter_mut().zip(easings) {
        k.easing = e;
    }
    // A dozen and a half keys, eased into each other: ~200 points a channel, so the expressions
    // are the balanced tree (a chain that long is past libavutil's nesting limit, which
    // the evaluator enforces).
    let mut many = eased.clone();
    many.keyframes = (0..18)
        .map(|i| {
            let f = f64::from(i);
            Keyframe {
                easing: [
                    Easing::EaseInOut,
                    Easing::Hold,
                    Easing::EaseOut,
                    Easing::Linear,
                    Easing::EaseIn,
                ][i as usize % 5],
                ..keyed(
                    f * 0.27 + 0.013 * f64::from(i % 3),
                    0.8 + 0.1 * f64::from(i % 5),
                    (f64::from((i * 7) % 10 - 5) / 20.0, f64::from((i * 3) % 7 - 3) / 20.0),
                    f64::from((i * 37) % 60 - 30),
                    1.0 - 0.1 * f64::from(i % 4),
                )
            }
        })
        .collect();
    let mut cuts = vec![
        ("animated clip and title", animated),
        ("slide onto a static offset", pair(slid)),
        ("push onto an animated clip", pair(pushed)),
        ("slide onto an identity clip", pair(plain)),
        ("eased keys", timeline_of(vec![video_track(vec![eased])])),
        ("many eased keys", timeline_of(vec![video_track(vec![many])])),
    ];
    for (_, tl) in &mut cuts {
        tl.format = Some(crate::model::Delivery::new(360, 640, Fit::Contain));
    }
    cuts
}

/// How many (layer, frame) pairs of the sweep carry a zoom expression to evaluate.
const ZOOMS_AT_LEAST: usize = 1_000;

#[test]
fn the_motion_plan_samples_the_curves_the_graphs_expressions_write_at_every_output_frame_time() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = [asset];
    let (mut layers_checked, mut titles_checked, mut moved, mut zooms_checked) = (0, 0, 0, 0);
    for (name, tl) in cuts(&assets[0]) {
        for (fps, (num, den)) in [
            (24.0, (24, 1)),
            (25.0, (25, 1)),
            (29.97, (2997, 100)),
            (30.0, (30, 1)),
            (60.0, (60, 1)),
        ] {
            let opts = ExportOptions {
                fps: Some(fps),
                ..ExportOptions::default()
            };
            let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(CompositeColorPolicy::FixedBt601)).unwrap();
            assert_eq!(planner.canvas().fps, Rational::new(num, den).unwrap());
            let args = build_export_args(&tl, &assets, "x.mp4", &opts).unwrap();
            let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
            let chains: Vec<&str> = graph.split(';').collect();
            let chain_of = |flat: usize| *chains.iter().find(|c| c.ends_with(&format!("[v{flat}]"))).unwrap();
            let overlay_of = |flat: usize| *chains.iter().find(|c| c.contains(&format!("[v{flat}]overlay="))).unwrap();
            let texts: Vec<&str> = chains.iter().copied().filter(|c| c.contains("drawtext=")).collect();
            let end = tl.duration() + 1.0;
            for k in 0..(end * fps) as u64 {
                let plan = planner.at_frame(k).unwrap();
                let t = ffmpeg_frame_time(k, num, den);
                let (w, h) = (f64::from(plan.canvas.width), f64::from(plan.canvas.height));
                for layer in &plan.layers {
                    let flat = tl.tracks[0].clips.iter().position(|c| c.id == layer.clip_id).unwrap();
                    let at = format!("{name}, {fps} fps, frame {k} (t = {t:?}), clip {flat}");
                    let geom = plan.layer_geometry(layer, (plan.canvas.width, plan.canvas.height)).unwrap();
                    // The overlay's position, truncated to the 4:2:0 grid the way `overlay` does.
                    let body = overlay_of(flat).split_once("]overlay=").unwrap().1.split('[').next().unwrap();
                    let opts = parse_options(body);
                    if let Some(x) = opt(&opts, "x") {
                        let vars = [
                            ("t", t),
                            ("W", w),
                            ("H", h),
                            ("w", f64::from(geom.layer.0)),
                            ("h", f64::from(geom.layer.1)),
                        ];
                        let y = opt(&opts, "y").unwrap();
                        let place = |expr: &str| (eval(expr, &vars) as i64 & !1) as i32;
                        assert_eq!(geom.origin, (place(x), place(y)), "{at}: {body}");
                        moved += 1;
                    } else {
                        // No `x` / `y`: an untouched, untravelled clip at the origin.
                        assert_eq!(geom.origin, (0, 0), "{at}");
                    }
                    let chain = chain_of(flat);
                    let tf = layer.transform;
                    // (`options` would give the first `scale`, the fit, which never has `eval`.)
                    if let Some(z) = all_options(chain, "scale")
                        .into_iter()
                        .find(|o| opt(o, "eval") == Some("frame"))
                    {
                        // The expression's grammar at the output frame's time, which is the
                        // time the graph reads it at (the zoom follows `fps`).
                        let zoom = eval(opt(&z, "w").unwrap(), &[("t", t), ("iw", 1.0)]);
                        assert!((zoom - tf.scale).abs() < 1e-9, "{at}: zoom {zoom} vs {}", tf.scale);
                        zooms_checked += 1;
                    }
                    if let Some(r) = options(chain, "rotate") {
                        let rad = eval(opt(&r, "a").unwrap(), &[("t", t), ("PI", PI)]);
                        assert!((rad - tf.rotation.to_radians()).abs() < 1e-9, "{at}: rotate {rad}");
                    }
                    if let Some(g) = options(chain, "geq") {
                        let a = opt(&g, "a").unwrap().strip_suffix("*alpha(X,Y)").unwrap();
                        assert!((eval(a, &[("T", t)]) - tf.opacity).abs() < 1e-9, "{at}: opacity");
                    }
                    layers_checked += 1;
                }
                for (title, text) in tl.overlays.iter().zip(&texts) {
                    let opts = options(text.split_once(']').unwrap().1, "drawtext").unwrap();
                    let live = eval(opt(&opts, "enable").unwrap(), &[("t", t)]) != 0.0;
                    let planned = plan.overlays.iter().find(|o| o.id == title.id);
                    assert_eq!(
                        live,
                        planned.is_some(),
                        "{name}, {fps} fps, frame {k}: title live at t = {t:?}"
                    );
                    let Some(p) = planned else { continue };
                    let vars = [("t", t), ("w", 1000.0), ("text_w", 0.0), ("h", 1000.0), ("text_h", 0.0)];
                    let (x, y) = (
                        eval(opt(&opts, "x").unwrap(), &vars) / 1000.0,
                        eval(opt(&opts, "y").unwrap(), &vars) / 1000.0,
                    );
                    let alpha = eval(opt(&opts, "alpha").unwrap(), &vars);
                    assert!(
                        [(x, p.pos.0), (y, p.pos.1), (alpha, p.alpha)]
                            .iter()
                            .all(|(a, b)| (a - b).abs() < 1e-9),
                        "{name}, {fps} fps, frame {k}: title at ({x}, {y}) alpha {alpha}, plan {:?} {}",
                        p.pos,
                        p.alpha
                    );
                    titles_checked += 1;
                }
            }
        }
    }
    // The sweep must have looked at something: animated layers, travelling ones, titles.
    assert!(
        layers_checked > 3_000 && moved > 2_000 && titles_checked > 500,
        "{layers_checked} {moved} {titles_checked}"
    );
    // ... and the zoom's own `scale eval=frame` has to have been found among the clip's
    // scales and evaluated (the check used to take the first `scale`, and never ran).
    assert!(zooms_checked > ZOOMS_AT_LEAST, "{zooms_checked} zoom expressions evaluated");
}

#[test]
fn the_evaluator_reads_what_the_graph_writes() {
    let v = [("t", 2.5), ("T", 2.5)];
    assert_eq!(eval("1+2*3-4/2", &v), 5.0);
    assert_eq!(eval("(t-5)--0.1", &v), -2.4);
    assert_eq!(eval("if(lt(t,3),(0+(-0.5)*(t-1)/(2)),9)", &v), -0.375);
    assert_eq!(eval("if(lt(t,2),1,if(lt(t,3),7,8))", &v), 7.0);
    assert_eq!((eval("between(t,2.5,3)", &v), eval("between(t,0,2.4999)", &v)), (1.0, 0.0));
    let e = crate::engine::cli::keyframe_expr(&[(0.0, 0.0), (1.0, 10.0), (3.0, -2.0)], "t", 1.5);
    for (t, want) in [
        (0.0, 0.0),
        (1.5, 0.0),
        (2.0, 5.0),
        (2.5, 10.0),
        (3.5, 4.0),
        (4.5, -2.0),
        (9.0, -2.0),
    ] {
        assert!((eval(&e, &[("t", t)]) - want).abs() < 1e-9, "{t}: {e}");
    }
}

#[test]
#[should_panic(expected = "libavutil refuses")]
fn the_evaluator_refuses_what_libavutil_refuses() {
    let chain = (0..NESTING_LIMIT).fold("1".to_string(), |e, i| format!("if(lt(t,{i}),{i},{e})"));
    eval(&chain, &[("t", 0.0)]);
}

#[test]
fn a_long_keyframe_expression_is_the_polyline_it_was_written_from_at_every_time() {
    let easings = [
        Easing::EaseInOut,
        Easing::Hold,
        Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 0.1,
        },
        Easing::Linear,
        Easing::EaseOut,
        Easing::EaseIn,
    ];
    // Off-grid times, a key that goes nowhere (equal times) and values that go both ways.
    let keys: Vec<(f64, f64, Easing)> = (0..16)
        .map(|i| {
            let time = f64::from(i) * 0.37 + 0.011 * f64::from(i % 4);
            (time, f64::from((i * 5) % 9) - 4.0, easings[i as usize % easings.len()])
        })
        .chain([
            (5.9, 3.0, Easing::Hold),
            (5.9, -2.0, Easing::Linear),
            (7.0, 1.5, Easing::Linear),
        ])
        .collect();
    let pts = crate::model::eased_points(&keys);
    assert!(
        pts.len() > crate::engine::cli::KEYFRAME_TREE_POINTS * 4,
        "{} points",
        pts.len()
    );
    let expr = crate::engine::cli::keyframe_expr(&pts, "t", 0.0);
    // The time of every point, a hair either side of it, between each two, and past both ends.
    let mut times = vec![-1.0, 0.0, 99.0];
    for w in pts.windows(2) {
        let (a, b) = (w[0].0, w[1].0);
        times.extend([a - 1e-9, a, a + 1e-9, (a + b) / 2.0, b - 1e-9, b]);
    }
    for t in times {
        let got = eval(&expr, &[("t", t)]);
        let want = crate::model::interpolate(&pts, t).unwrap();
        assert!(
            (got - want).abs() < 1e-9,
            "t = {t}: the expression says {got}, the polyline {want}"
        );
    }
}
