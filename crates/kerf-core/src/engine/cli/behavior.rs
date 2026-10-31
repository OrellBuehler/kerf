//! A clip's live behaviours (`model::behavior`) as the text of FFmpeg expressions — pure.
//!
//! Every place the graph turns one of a clip's transform numbers into an expression of time goes
//! through [`driven_expr`], so none can forget the behaviours: the number's curve (or its static
//! value) with each live behaviour that moves it written after it, `(curve)+(term)` for a position
//! or a rotation and `(curve)*(1+term)` for the zoom. A number no behaviour moves comes back as
//! [`keyframe_expr`] of its curve, byte for byte what it always was, so a clip without behaviours
//! renders the graph it always did.
//!
//! **The text is the model's arithmetic, operation for operation** ([`Behavior::term`]): the same
//! constants in the same order, so `eval(text) == term` to the last bit apart from the libm
//! `sin` / `cos` of the machine that runs the filter. Nothing transcendental is computed to write
//! the text (`cos(angle*PI/180)` is left to the filter, not printed as a number), so the export's and
//! the stream's expressions read the same on every machine. (The scrubbed still is not text of that
//! kind: it evaluates the pose at a moment and prints the numbers, which are this machine's libm's.)
//!
//! Time is the clip's clock: `(t-<timeline_start>)`, as the keys read it (`t` for `overlay` /
//! `rotate` / `scale`, the frame counter's time for a `perspective`), plus the behaviour's `phase`.

use super::*;
use crate::model::{Behavior, Wave};

/// `lt` (the clip-local clock) plus `phase` seconds.
fn clock(lt: &str, phase: f64) -> String {
    if phase == 0.0 {
        lt.to_string()
    } else if phase > 0.0 {
        format!("({lt}+{})", fnum(phase))
    } else {
        format!("({lt}-{})", fnum(-phase))
    }
}

/// The term `b` (sanitized, live) adds to `prop`, as an expression of the clock `lt` — the text of
/// [`Behavior::term`]. Empty for a number it does not move.
fn term_expr(b: &Behavior, prop: Property, lt: &str) -> Option<String> {
    if !b.drives(prop) {
        return None;
    }
    let u = clock(lt, b.phase());
    Some(match *b {
        Behavior::Sway { amount, speed, .. } | Behavior::Breathe { amount, speed, .. } => {
            format!("{}*sin(2*PI*{}*{u})", fnum(amount), fnum(speed))
        }
        Behavior::Spin { speed, .. } => format!("{}*{u}", fnum(speed * 360.0)),
        Behavior::Drift { speed, angle, .. } => {
            let trig = if prop == Property::PosX { "cos" } else { "sin" };
            format!("{}*{trig}({}*PI/180)*{u}", fnum(speed), fnum(angle))
        }
        Behavior::Shake { amount, .. } => {
            let sine = |w: Wave| format!("{}*sin(2*PI*{}*{u}+{})", fnum(w.weight), fnum(w.freq), fnum(w.phase));
            let [a, b, c] = b.waves(prop)?;
            format!("{}*({}+{}+{})", fnum(amount), sine(a), sine(b), sine(c))
        }
    })
}

/// `base` (the text of `prop`'s curve or static value) with the clip's live behaviours that move
/// `prop` written onto it, over the time variable `tvar`. `base` itself when none does.
pub(super) fn layered(clip: &Clip, prop: Property, base: String, tvar: &str) -> String {
    let live: Vec<Behavior> = clip.live_behaviors().into_iter().filter(|b| b.drives(prop)).collect();
    if live.is_empty() {
        return base;
    }
    let lt = format!("({tvar}-{})", fnum(clip.timeline_start));
    let mut e = format!("({base})");
    for term in live.iter().filter_map(|b| term_expr(b, prop, &lt)) {
        if prop == Property::Scale {
            e.push_str(&format!("*(1+{term})"));
        } else {
            e.push_str(&format!("+({term})"));
        }
    }
    e
}

/// The expression of one of a clip's transform numbers over the time variable `tvar`: its keyed
/// curve, else its static value, with the behaviours that move it ([`layered`]). The one place
/// the graph's per-frame transform numbers come from.
pub(super) fn driven_expr(clip: &Clip, prop: Property, tvar: &str) -> String {
    layered(
        clip,
        prop,
        keyframe_expr(&curve_or_static(clip, prop), tvar, clip.timeline_start),
        tvar,
    )
}

#[cfg(test)]
mod tests {
    use super::super::sweep::eval;
    use super::*;
    use crate::model::{Clip, PropertyKey, Timeline};
    use std::f64::consts::PI;
    use uuid::Uuid;

    fn clip() -> Clip {
        let mut c = Clip::new(Uuid::new_v4(), 0.0, 20.0, 3.5);
        c.set_property_keys(Property::Scale, vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(8.0, 1.5)]);
        c.transform.pos_x = 0.05;
        c.transform.rotation = 4.0;
        c
    }

    fn every_kind() -> Vec<Behavior> {
        vec![
            Behavior::Sway {
                amount: 7.5,
                speed: 0.35,
                phase: 0.0,
            },
            Behavior::Sway {
                amount: 2.0,
                speed: 1.7,
                phase: -0.4,
            },
            Behavior::Breathe {
                amount: 0.12,
                speed: 0.8,
                phase: 1.25,
            },
            Behavior::Spin {
                speed: -0.07,
                phase: 0.0,
            },
            Behavior::Drift {
                speed: 0.015,
                angle: -33.0,
                phase: 2.0,
            },
            Behavior::Shake {
                amount: 0.013,
                speed: 3.1,
                seed: 4_000_000_001,
                phase: 0.5,
            },
        ]
    }

    #[test]
    fn a_number_no_behaviour_moves_is_the_expression_it_always_was() {
        let mut c = clip();
        for p in Property::TRANSFORM {
            assert_eq!(
                driven_expr(&c, p, "t"),
                keyframe_expr(&curve_or_static(&c, p), "t", c.timeline_start)
            );
        }
        c.behaviors = vec![Behavior::Sway {
            amount: 3.0,
            speed: 1.0,
            phase: 0.0,
        }];
        assert_eq!(
            driven_expr(&c, Property::PosX, "t"),
            keyframe_expr(&curve_or_static(&c, Property::PosX), "t", c.timeline_start),
            "a sway leaves the position's text alone"
        );
        assert_eq!(
            driven_expr(&c, Property::Scale, "t"),
            keyframe_expr(&curve_or_static(&c, Property::Scale), "t", c.timeline_start)
        );
        assert_eq!(driven_expr(&c, Property::Rotation, "t"), "(4)+(3*sin(2*PI*1*(t-3.5)))");
    }

    #[test]
    fn the_text_is_the_models_arithmetic_at_every_time() {
        let mut c = clip();
        c.behaviors = every_kind();
        for tvar in ["t", "T"] {
            for prop in Property::TRANSFORM {
                let text = driven_expr(&c, prop, tvar);
                for i in 0..=400 {
                    let t = c.timeline_start - 0.5 + f64::from(i) * 0.061;
                    let got = eval(&text, &[(tvar, t), ("PI", PI)]);
                    let want = c.property_at(prop, t - c.timeline_start);
                    // (Rust and the evaluator are one libm here; FFmpeg's, the machine's.)
                    assert!(
                        (got - want).abs() <= 1e-12 * want.abs().max(1.0),
                        "{prop:?} at {t}: {got} vs {want}\n{text}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_hostile_behaviour_is_written_held_to_its_ranges() {
        let mut c = clip();
        c.behaviors = vec![
            Behavior::Sway {
                amount: f64::NAN,
                speed: 1.0,
                phase: 0.0,
            },
            Behavior::Breathe {
                amount: 1e300,
                speed: f64::INFINITY,
                phase: 0.0,
            },
            Behavior::Drift {
                speed: 1e300,
                angle: f64::NEG_INFINITY,
                phase: f64::NAN,
            },
            Behavior::Spin {
                speed: 99.0,
                phase: -1e300,
            },
        ];
        for prop in Property::TRANSFORM {
            let text = driven_expr(&c, prop, "t");
            assert!(
                !text.contains("NaN") && !text.contains("inf") && !text.contains("e300"),
                "{text}"
            );
            let v = eval(&text, &[("t", 9.0), ("PI", PI)]);
            assert!(v.is_finite(), "{prop:?}: {v}");
        }
    }

    #[test]
    fn the_terms_nest_shallowly_and_the_sum_is_flat() {
        let mut c = clip();
        c.behaviors = every_kind();
        c.behaviors.extend(every_kind().into_iter().take(2));
        for prop in Property::TRANSFORM {
            // `eval` panics past libavutil's 100 levels.
            let _ = eval(&driven_expr(&c, prop, "t"), &[("t", 4.0), ("PI", PI)]);
        }
        assert!(driven_expr(&c, Property::PosX, "t").len() < 2_000);
        // Beside a long eased curve (a balanced tree, ~145 points a number) the behaviours add a level for
        // the brackets round the curve and a few for their own terms, not a level a key.
        let mut keyed = clip();
        keyed.behaviors = every_kind();
        let keys: Vec<PropertyKey> = (0..13)
            .map(|i| PropertyKey {
                time: f64::from(i) * 0.2,
                value: [0.7, 1.2, 0.9][i as usize % 3],
                easing: crate::model::Easing::EaseInOut,
            })
            .collect();
        for prop in [Property::Scale, Property::PosX, Property::PosY, Property::Rotation] {
            keyed.set_property_keys(prop, keys.clone());
            for tvar in ["t", "((in+4)*1001/30000)"] {
                let text = driven_expr(&keyed, prop, tvar);
                let vars = [("t", 1.7), ("in", 90.0), ("PI", PI)];
                let got = eval(&text, &vars);
                let at = if tvar == "t" { 1.7 } else { 94.0 * 1001.0 / 30000.0 };
                assert!(
                    (got - keyed.property_at(prop, at - keyed.timeline_start)).abs() < 1e-9,
                    "{prop:?}"
                );
            }
        }
    }

    fn graph(args: &[String]) -> String {
        args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone()
    }

    fn source() -> crate::model::Asset {
        use crate::engine::test_support::{test_asset, video_stream};
        test_asset(vec![video_stream(1920, 1080, 30.0)])
    }

    fn cut(asset: &crate::model::Asset, mut edit: impl FnMut(&mut Clip)) -> Timeline {
        use crate::engine::test_support::{make_clip, single};
        let mut clip = make_clip(asset.id, 0.0, 6.0, 1.0);
        edit(&mut clip);
        single(vec![clip])
    }

    #[test]
    fn behaviours_that_move_nothing_leave_every_argv_byte_identical() {
        let idle = vec![
            Behavior::Sway {
                amount: 0.0,
                speed: 1.0,
                phase: 0.0,
            },
            Behavior::Breathe {
                amount: 0.2,
                speed: 0.0,
                phase: 1.0,
            },
            Behavior::Spin { speed: 0.0, phase: 0.0 },
            Behavior::Drift {
                speed: 0.0,
                angle: 40.0,
                phase: 0.0,
            },
            Behavior::Shake {
                amount: 0.0,
                speed: 3.0,
                seed: 1,
                phase: 0.0,
            },
        ];
        let opts = ExportOptions::default();
        let asset = source();
        let assets = [asset.clone()];
        for pose in [|_: &mut Clip| {}, |c: &mut Clip| c.transform.scale = 0.8] {
            let plain = cut(&asset, pose);
            let quiet = cut(&asset, |c| {
                pose(c);
                c.behaviors = idle.clone();
            });
            let export = |t: &Timeline| build_export_args(t, &assets, "out.mp4", &opts).unwrap();
            assert_eq!(export(&plain), export(&quiet));
            let still = |t: &Timeline| build_still_args(t, &assets, &opts, 2.0, 640, None, &StillOutput::RgbPipe).unwrap();
            assert_eq!(still(&plain), still(&quiet));
        }
    }

    /// An expression with a call and a comma outside quotes is the end of the filter: `sin(..)` has none,
    /// but the curve it is added to is a chain of `if(lt(..),..)`, and everything is quoted.
    #[test]
    fn every_expression_with_a_behaviour_in_it_is_quoted_in_the_export() {
        for behaviors in [
            vec![Behavior::Sway {
                amount: 6.0,
                speed: 0.5,
                phase: 0.0,
            }],
            vec![Behavior::Breathe {
                amount: 0.2,
                speed: 1.0,
                phase: 0.5,
            }],
            vec![Behavior::Spin { speed: 0.1, phase: 0.0 }],
            vec![Behavior::Drift {
                speed: 0.05,
                angle: 30.0,
                phase: 0.0,
            }],
            vec![Behavior::Shake {
                amount: 0.01,
                speed: 3.0,
                seed: 4,
                phase: 0.0,
            }],
            every_kind(),
        ] {
            for keyed in [false, true] {
                let asset = source();
                let assets = [asset.clone()];
                let tl = cut(&asset, |c| {
                    c.behaviors = behaviors.clone();
                    if keyed {
                        for p in Property::TRANSFORM {
                            c.set_property_keys(
                                p,
                                vec![PropertyKey::new(0.0, p.neutral()), PropertyKey::new(3.0, p.neutral())],
                            );
                        }
                    }
                });
                let args = build_export_args(&tl, &assets, "out.mp4", &ExportOptions::default()).unwrap();
                let text = graph(&args);
                assert!(
                    text.contains("sin(") || text.contains("cos(") || text.contains("*(t-"),
                    "{text}"
                );
                let mut quoted = false;
                for (i, ch) in text.char_indices() {
                    if ch == '\'' {
                        quoted = !quoted;
                    }
                    if !quoted && (text[i..].starts_with("sin(") || text[i..].starts_with("cos(") || text[i..].starts_with("(t-"))
                    {
                        panic!("an expression is outside its quotes at {i} of {text}");
                    }
                }
                assert!(!quoted);
            }
        }
    }

    /// The window of the picture a moving still keeps, `(x, y, w, h)`, read from the `crop` it is cut to.
    fn window(m: &StillMotion) -> (u32, u32, u32, u32) {
        let once = m.once("");
        let numbers: Vec<u32> = once
            .rsplit("crop=")
            .next()
            .unwrap()
            .split(':')
            .map(|n| n.parse().unwrap())
            .collect();
        (numbers[2], numbers[3], numbers[0], numbers[1])
    }

    fn holds(outer: (u32, u32, u32, u32), inner: (u32, u32, u32, u32)) -> bool {
        outer.0 <= inner.0
            && outer.1 <= inner.1
            && outer.0 + outer.2 >= inner.0 + inner.2
            && outer.1 + outer.3 >= inner.1 + inner.3
    }

    /// A shake's peaks are between a clip's evenly spaced samples, and the export evaluates the pose at
    /// the delivery's frames (`slot + k` of its grid, less the clip's start), not `length / count` from
    /// the clip's first instant: a clip that does not start on a frame read a fast shake a fraction of a
    /// frame off, so the window kept for the moving still could be a few pixels short of a frame it draws.
    #[test]
    fn a_moving_still_is_judged_at_the_frames_the_export_draws() {
        use crate::engine::test_support::img_asset;
        let asset = img_asset(Uuid::new_v4());
        let fmt = ExportFormat {
            fit: Fit::Cover,
            ..ExportFormat::default()
        };
        let frame = (fmt.width, fmt.height);
        let fx = ClipFx::default();
        let mut worse = 0;
        for start in [0.0, 0.017, 0.5, 1.0 / 3.0 + 0.011, 2.9, 7.123] {
            let mut clip = Clip::new(asset.id, 0.0, 4.0, start);
            clip.transform.scale = 1.4;
            clip.behaviors = vec![Behavior::Shake {
                amount: 0.06,
                speed: 9.0,
                seed: 3,
                phase: 0.0,
            }];
            let plan = still_motion_for(&clip, std::slice::from_ref(&asset), &fmt, &fx).expect("the move is drawn by the route");

            // The pose at every frame the export draws the clip on: the grid's frames from the one `fps`
            // pins the first picture to, `ceil(length * fps)` of them.
            let first = (start * fmt.fps).round() as i64;
            let drawn: Vec<f64> = (0..(4.0_f64 * fmt.fps).ceil() as i64)
                .map(|k| (first + k) as f64 / fmt.fps - start)
                .collect();
            let exact = StillMotion::new(&clip, still_motion::displayed(Some(&asset)), frame, frame, fmt.fit, &drawn).unwrap();
            assert!(
                holds(window(&plan), window(&exact)),
                "starting at {start}: the window {:?} is short of {:?}",
                window(&plan),
                window(&exact)
            );

            // The even samples this replaced (a clip starting on a frame is read the same either way).
            let count = (4.0_f64 * fmt.fps).ceil() as usize;
            let even: Vec<f64> = (0..=count).map(|k| 4.0 * k as f64 / count as f64).collect();
            let old = StillMotion::new(&clip, still_motion::displayed(Some(&asset)), frame, frame, fmt.fit, &even).unwrap();
            if !holds(window(&old), window(&exact)) {
                worse += 1;
            }
        }
        assert!(
            worse >= 2,
            "the even samples fall short of a frame the export draws for only {worse} starts"
        );
    }

    /// A clip without behaviours keeps the samples it always had (no digest moved): its extremes are its curve's points.
    #[test]
    fn a_keyed_moving_still_keeps_its_even_samples() {
        use crate::engine::test_support::img_asset;
        let asset = img_asset(Uuid::new_v4());
        let fmt = ExportFormat {
            fit: Fit::Cover,
            ..ExportFormat::default()
        };
        let frame = (fmt.width, fmt.height);
        let mut clip = Clip::new(asset.id, 0.0, 4.0, 0.017);
        clip.set_property_keys(Property::Scale, vec![PropertyKey::new(0.0, 1.1), PropertyKey::new(4.0, 1.4)]);
        clip.set_property_keys(
            Property::PosX,
            vec![PropertyKey::new(0.0, -0.04), PropertyKey::new(4.0, 0.04)],
        );
        let plan = still_motion_for(&clip, std::slice::from_ref(&asset), &fmt, &ClipFx::default()).unwrap();
        let count = 120;
        let mut even: Vec<f64> = (0..=count).map(|k| 4.0 * f64::from(k) / f64::from(count)).collect();
        for p in Property::TRANSFORM {
            even.extend(
                clip.property_curve(p)
                    .iter()
                    .map(|pt| pt.0)
                    .filter(|t| (0.0..=4.0).contains(t)),
            );
        }
        let want = StillMotion::new(&clip, still_motion::displayed(Some(&asset)), frame, frame, fmt.fit, &even).unwrap();
        assert_eq!(plan, want);
    }

    /// The reason for closed forms: a two-minute shake is a few hundred characters of graph, where the same
    /// motion sampled at 30 fps is 3600 keys a number (a command line is capped at 32 K characters on Windows).
    #[test]
    fn a_two_minute_shake_costs_a_short_expression() {
        use crate::engine::test_support::{make_clip, single, test_asset, video_stream};
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let mut clip = make_clip(asset.id, 0.0, 120.0, 0.0);
        let plain = single(vec![clip.clone()]);
        clip.behaviors = every_kind();
        let args = build_export_args(
            &single(vec![clip]),
            std::slice::from_ref(&asset),
            "out.mp4",
            &ExportOptions::default(),
        )
        .unwrap();
        let before = build_export_args(&plain, std::slice::from_ref(&asset), "out.mp4", &ExportOptions::default()).unwrap();
        let (with, without) = (graph(&args).len(), graph(&before).len());
        assert!(with - without < 6_000, "{} characters of behaviours", with - without);
        assert!(args.iter().map(String::len).sum::<usize>() < 12_000);
    }
}
