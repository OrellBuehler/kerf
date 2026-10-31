//! Live behaviours (B8b) through the real `ffmpeg` (`#[ignore]`d).
//!
//! A behaviour is a formula of the clip's clock added to its pose, written into the graph as an
//! expression (`behavior.rs`) and into the model as `Clip::transform_at`; the string-level tests hold the
//! one to the other with an evaluator, but only FFmpeg can say that it accepts the text (`sin` and `cos`
//! beside `if(lt(..))` trees inside a `rotate`, an `overlay` and a `perspective`), reads `t` where the
//! chain meant it to, and puts the picture where the model says. These use `keyed_zoom.rs`'s harness: a
//! red field with a blue box over a green frame, measured on **every output frame** against
//! `Clip::transform_at` and every eighth against the scrubbed still, for each kind alone and all five
//! together, on top of keys, at speeds other than 1 and reversed, on a still image, under both fits, in a
//! range export, at every frame rate, and in the playback stream. A split clip has to draw what the whole
//! clip does (the phase moves with the cut).
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored behavior_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build; `KERF_ZOOM_KEEP` / `KERF_ZOOM_VERBOSE` as for the zoom).

use super::build_export_args;
use super::keyed_zoom::*;
use crate::clip_timing::ffmpeg_frame_time;
use crate::model::{Behavior, Clip, Easing, Fit, Keyframe, Property};

fn sway() -> Behavior {
    Behavior::Sway {
        amount: 14.0,
        speed: 0.6,
        phase: 0.0,
    }
}

fn breathe() -> Behavior {
    Behavior::Breathe {
        amount: 0.25,
        speed: 0.8,
        phase: 0.3,
    }
}

fn spin() -> Behavior {
    Behavior::Spin { speed: 0.15, phase: 0.0 }
}

fn drift() -> Behavior {
    Behavior::Drift {
        speed: 0.06,
        angle: 20.0,
        phase: -0.2,
    }
}

fn shake() -> Behavior {
    Behavior::Shake {
        amount: 0.03,
        speed: 4.0,
        seed: 21,
        phase: 0.0,
    }
}

fn all() -> Vec<Behavior> {
    vec![sway(), breathe(), spin(), drift(), shake()]
}

/// A case whose clip has `behaviors` (and no keys of its own, unless `keys` says), played at `speed`.
fn at_speed(name: &str, keys: Vec<Keyframe>, behaviors: Vec<Behavior>, speed: f64) -> Case {
    Case::new(name, keys).decorated(move |c| {
        c.behaviors = behaviors.clone();
        c.speed = speed;
    })
}

fn with(name: &str, keys: Vec<Keyframe>, behaviors: Vec<Behavior>) -> Case {
    at_speed(name, keys, behaviors, 1.0)
}

fn key(time: f64, scale: f64, x: f64, rotation: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x: x,
        pos_y: x / 2.0,
        rotation,
        opacity: 1.0,
        easing: Easing::Linear,
    }
}

/// The five kinds, each alone: a sway and a spin are a keyed rotation, a breath a moving zoom, a drift
/// and a shake a moving position — three different chains.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn every_behavior_alone_is_where_transform_at_puts_it() {
    run_all(
        "behavior-alone",
        vec![
            with("sway", vec![], vec![sway()]).rotated(),
            with("spin", vec![], vec![spin()]).rotated(),
            with("breathe", vec![], vec![breathe()]),
            with("drift", vec![], vec![drift()]),
            with("shake", vec![], vec![shake()]),
            // Behaviours that move nothing leave the clip's chain as it is: a clip at a held 0.8 zoom.
            Case::new("neutral", vec![]).decorated(|c| {
                c.transform.scale = 0.8;
                c.behaviors = vec![
                    Behavior::Sway {
                        amount: 0.0,
                        speed: 1.0,
                        phase: 0.0,
                    },
                    Behavior::Shake {
                        amount: 0.1,
                        speed: 0.0,
                        seed: 3,
                        phase: 0.0,
                    },
                ];
            }),
        ],
    );
}

/// All five at once, and each on top of keys on the same numbers and others: the term is added to the
/// curve, so a keyed zoom with a breath is the product, a keyed turn with a sway the sum.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn behaviors_ride_on_top_of_keys() {
    let moving = vec![
        key(0.0, 0.8, -0.15, 0.0),
        key(1.2, 1.2, 0.15, 25.0),
        key(2.4, 0.9, 0.0, -10.0),
    ];
    run_all(
        "behavior-on-keys",
        vec![
            with("all five", vec![], all()).rotated(),
            with("all five on keys", moving, all()).rotated(),
            with(
                "sway on a keyed turn",
                vec![key(0.0, 1.0, 0.0, -20.0), key(2.4, 1.0, 0.0, 30.0)],
                vec![sway()],
            )
            .rotated(),
            with(
                "breathe on a keyed zoom",
                vec![key(0.0, 0.5, 0.0, 0.0), key(2.4, 1.3, 0.0, 0.0)],
                vec![breathe()],
            ),
            with(
                "shake on a keyed pan",
                vec![key(0.0, 1.0, -0.2, 0.0), key(2.4, 1.0, 0.2, 0.0)],
                vec![shake()],
            ),
            with(
                "drift on keys holding still",
                vec![key(0.0, 0.7, 0.1, 0.0), key(2.4, 0.7, 0.1, 0.0)],
                vec![drift(), sway()],
            )
            .rotated(),
        ],
    );
}

/// The clock is the clip's own timeline clock, so speed and reverse change nothing about a behaviour: a
/// clip played at 1.5x, at half speed and backwards sways and shakes at the same Hz.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn speed_and_reverse_do_not_change_the_clock() {
    let lively = vec![sway(), shake(), breathe()];
    run_all(
        "behavior-speed",
        vec![
            at_speed("1.5x", vec![], lively.clone(), 1.5).rotated(),
            at_speed("half speed", vec![], lively.clone(), 0.5).rotated(),
            at_speed("reversed", vec![], lively, -1.0).rotated(),
        ],
    );
}

/// A still image takes the moving-still route when its picture moves (drift, shake, breath) and the
/// general chain when it only turns; under `Cover` a drift pans across a cover that hangs over the frame.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn stills_and_both_fits_follow_the_model() {
    run_all(
        "behavior-fits",
        vec![
            with("still, drift + breathe", vec![], vec![drift(), breathe()]).from(Source::Still),
            with("still, shake", vec![], vec![shake()]).from(Source::Still),
            with("still, sway", vec![], vec![sway()]).from(Source::Still).rotated(),
            with("cover, drift + shake", vec![], vec![drift(), shake()]).fit(Fit::Cover),
            with("cover, breathe + sway", vec![], vec![breathe(), sway()])
                .fit(Fit::Cover)
                .rotated(),
            with("cover, all five", vec![], all()).fit(Fit::Cover).rotated(),
            zoomed_still(),
        ],
    );
}

/// A photo zoomed in under `Cover`, drifting, shaking and breathing without ever uncovering the frame: the
/// kind of still the moving-still route draws in one resample of a copy scaled once.
fn zoomed_still() -> Case {
    Case::new("still, zoomed under cover, drift + shake + breathe", vec![])
        .decorated(|c| {
            c.transform.scale = 2.2;
            c.behaviors = vec![drift(), shake(), breathe()];
        })
        .from(Source::Still)
        .fit(Fit::Cover)
}

/// The still above is taken by `still_motion` (the copy scaled once), and its pictures are the model's.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_still_that_moves_by_behaviors_is_drawn_by_the_moving_still_route() {
    let dir = scratch("behavior-still-motion");
    let case = zoomed_still();
    let (timeline, assets) = case.timeline(&dir);
    let opts = options(case.rate, case.fit);
    let args = build_export_args(&timeline, &assets, "unused.mkv", &opts).unwrap();
    let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
    assert!(graph.contains(":flags=lanczos,crop="), "scaled once: {graph}");
    assert!(graph.contains("perspective="), "{graph}");
    run_in(&dir, vec![case]);
    cleanup(&dir);
}

/// A range export is `Timeline::slice`: the clip's head is cut off and its behaviours' phase moves with it,
/// so the motion in the file is the motion of the same moments of the whole cut.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_range_export_carries_the_motion_on() {
    run_all(
        "behavior-range",
        vec![
            with("range", vec![], all()).ranged(1.2, 3.0).rotated(),
            with(
                "range on keys",
                vec![key(0.0, 0.8, -0.1, 0.0), key(2.4, 1.1, 0.1, 15.0)],
                all(),
            )
            .ranged(1.7, 2.9)
            .rotated(),
        ],
    );
}

/// The clock is read at the output frame's time at any rate (the placement counts frames of a pinned `fps`).
#[test]
#[ignore = "needs the ffmpeg binary"]
fn every_frame_rate_reads_the_same_clock() {
    let cases = RATES
        .iter()
        .map(|rate| {
            let mut case = with(&format!("{} fps", rate.0), vec![], all()).rotated();
            case.rate = *rate;
            case.source = Source::Video(rate.0);
            case
        })
        .collect();
    run_all("behavior-rates", cases);
}

/// Playback is the same graph: stream a clip with every behaviour and hold each decoded frame to
/// `transform_at` at the time it carries.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_playback_stream_moves_too() {
    let dir = scratch("behavior-stream");
    playback_matches_transform_at(&dir, &with("playback", vec![], all()).rotated());
    playback_matches_transform_at(
        &dir,
        &with(
            "playback on keys",
            vec![key(0.0, 0.8, -0.1, 0.0), key(2.4, 1.1, 0.1, 15.0)],
            all(),
        )
        .rotated(),
    );
    cleanup(&dir);
}

/// Thirteen eased keys on every number, plus every behaviour: the curve is a balanced tree, and the
/// behaviours' terms are written beside it without taking it past libavutil's 100 levels of nesting.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_clip_with_many_eased_keys_and_every_behavior_exports_and_plays_back() {
    let dir = scratch("behavior-many-eased");
    let case = || with("many eased keys + behaviours", many_eased_keys(), all()).rotated();
    run_in(&dir, vec![case()]);
    playback_matches_transform_at(&dir, &case());
    cleanup(&dir);
}

/// Bake a clip with every behaviour into keys at the export's rate and the picture is the live one's, frame for
/// frame (the keys sit on the frame grid, so every frame reads a value the live formula gave).
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_baked_clip_draws_what_the_live_one_does() {
    let dir = scratch("behavior-bake");
    let case = with("bake", vec![key(0.0, 0.9, -0.1, 0.0), key(2.4, 1.1, 0.1, 15.0)], all()).rotated();
    let (live, assets) = case.timeline(&dir);
    let opts = options(case.rate, case.fit);
    let mut baked = live.clone();
    let clip = baked.tracks[1].clips.last_mut().unwrap();
    let report = clip.bake_behaviors(30.0, 1000).unwrap();
    assert!(clip.behaviors.is_empty());
    assert_eq!(
        report.properties,
        vec![Property::Scale, Property::PosX, Property::PosY, Property::Rotation]
    );
    assert_ne!(
        build_export_args(&live, &assets, "unused.mkv", &opts).unwrap(),
        build_export_args(&baked, &assets, "unused.mkv", &opts).unwrap(),
        "the keys are another graph"
    );
    let (a, b) = (
        export_frames(&live, &assets, &opts, &dir, "bake-live"),
        export_frames(&baked, &assets, &opts, &dir, "bake-baked"),
    );
    assert_eq!(a.len(), b.len());
    let start = live.tracks[1].clips[0].timeline_start;
    let (mut worst, mut compared) = (0.0f64, 0);
    for (k, (x, y)) in a.iter().zip(&b).enumerate() {
        let t = ffmpeg_frame_time(k as u64, 30, 1);
        if t < start + 0.05 || t > start + 2.4 - 0.05 {
            continue;
        }
        let off = x.iter().zip(y).map(|(p, q)| f64::from(p.abs_diff(*q))).sum::<f64>() / x.len() as f64;
        worst = worst.max(off);
        compared += 1;
        assert!(off < 1.0, "frame {k} (t = {t:.3}) is {off:.2} levels from the live clip's");
    }
    assert!(compared > 60, "{compared}");
    report_lines(&[format!(
        "bake: {compared} frames, worst {worst:.3} levels, {} keys a number",
        report.keys
    )]);
    cleanup(&dir);
}

/// Cut a swaying, shaking, breathing clip in two: the halves draw what the whole clip draws, frame for frame
/// (the right half's phase moved by the cut). The footage is a still picture, so the picture is the same
/// whichever source frame is picked and what differs is the pose alone.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_split_clip_draws_what_the_whole_clip_does() {
    let dir = scratch("behavior-split");
    let case = with("split", vec![key(0.0, 0.9, -0.1, 0.0), key(2.4, 1.1, 0.1, 15.0)], all()).rotated();
    let (whole, assets) = case.timeline(&dir);
    let opts = options(case.rate, case.fit);
    let clip = whole.tracks[1].clips.last().unwrap().clone();
    let full = export_frames(&whole, &assets, &opts, &dir, "split-whole");
    for at in [
        clip.timeline_start + 0.9,
        clip.timeline_start + 1.2333333333333334,
        clip.timeline_start + 2.1,
    ] {
        let mut cut = whole.clone();
        let (left, right) = cut.split_clip(clip.id, at).unwrap();
        assert_eq!((left.behaviors.len(), right.behaviors.len()), (5, 5));
        let halves = export_frames(&cut, &assets, &opts, &dir, "split-halves");
        assert_eq!(halves.len(), full.len());
        let (mut worst, mut compared) = (0.0f64, 0);
        for (k, (a, b)) in full.iter().zip(&halves).enumerate() {
            let t = ffmpeg_frame_time(k as u64, 30, 1);
            if (t - at).abs() < 3.0 / 30.0 || t < clip.timeline_start + 0.1 || t > clip.timeline_end() - 0.1 {
                continue;
            }
            let off = a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len() as f64;
            worst = worst.max(off);
            compared += 1;
            assert!(
                off < 1.0,
                "split at {at:.3}: frame {k} (t = {t:.3}) is {off:.2} levels from the whole clip's"
            );
        }
        assert!(compared > 40, "{compared}");
        report_lines(&[format!("split at {at:.3}: {compared} frames, worst {worst:.3} levels")]);
    }
    // The right half really is another clip whose behaviours carry the cut.
    let mut cut = whole;
    let (left, right) = cut.split_clip(clip.id, clip.timeline_start + 0.9).unwrap();
    assert!((right.behaviors[0].phase() - (left.behaviors[0].phase() + 0.9)).abs() < 1e-9);
    assert!(right.property_at(Property::Rotation, 0.0).is_finite());
    let _: &Clip = &right;
    cleanup(&dir);
}
