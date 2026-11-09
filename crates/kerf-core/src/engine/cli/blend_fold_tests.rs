//! A run of blended clips folded into one fork and join of the composite ([`blend::fold_runs`]), as strings: which
//! clips fold, that a shorter run keeps the nested graph byte for byte, what the folded graph is made of, and what the
//! planner counts (the renders are `blend_fold_rendered.rs`, `#[ignore]`d).

use super::blend::FOLD_MIN;
use super::segments::*;
use super::segments_tests::graph_of;
use super::*;
use crate::engine::test_support::{image_stream, make_clip, test_asset, timeline_of, video_stream, video_track};
use crate::model::{BlendMode, Transition, TransitionKind};

fn footage() -> Asset {
    let mut asset = test_asset(vec![video_stream(640, 360, 30.0)]);
    asset.path = "/media/src.mp4".into();
    asset
}

fn opts(fps: f64) -> ExportOptions {
    ExportOptions {
        fps: Some(fps),
        resolution: Some((640, 360)),
        ..ExportOptions::default()
    }
}

/// A blended clip of `asset` from `start` for `len` seconds of the same source frames.
fn blended(asset: &Asset, start: f64, len: f64) -> Clip {
    let mut clip = make_clip(asset.id, 0.5, 0.5 + len, start);
    clip.blend_mode = BlendMode::Multiply;
    clip
}

/// `n` blended clips of `each` frames one after the other, at `fps`.
fn run(asset: &Asset, n: usize, each: f64, fps: f64) -> Vec<Clip> {
    (0..n).map(|i| blended(asset, i as f64 * each / fps, each / fps)).collect()
}

/// A plain clip under `above`, as the blended ones have something to blend with.
fn over_a_base(base: &Asset, above: Vec<Clip>) -> Timeline {
    let length = above.iter().map(Clip::timeline_end).fold(0.0, f64::max);
    timeline_of(vec![
        video_track(vec![make_clip(base.id, 0.0, length, 0.0)]),
        video_track(above),
    ])
}

fn graph(timeline: &Timeline, assets: &[Asset], fps: f64) -> String {
    graph_of(&build_export_args(timeline, assets, "out.mkv", &opts(fps)).unwrap())
}

fn unfolded(timeline: &Timeline, assets: &[Asset], fps: f64) -> String {
    with_fold_min(usize::MAX, || graph(timeline, assets, fps))
}

fn folds_of(timeline: &Timeline, assets: &[Asset], fps: f64) -> Vec<Vec<usize>> {
    let cut = prepared_cut(timeline, &opts(fps));
    let fx = transition_fx(&cut, assets);
    blend_folds(&cut, assets, &fx, fps, &|_| true)
}

#[test]
fn a_run_of_six_is_the_nested_graph_and_seven_are_folded() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    for n in 1..=6 {
        let tl = over_a_base(&base, run(&src, n, 2.0, 30.0));
        let g = graph(&tl, &assets, 30.0);
        assert_eq!(g, unfolded(&tl, &assets, 30.0), "{n} clips keep the nested graph");
        assert!(!g.contains("bldf"), "{n}: {g}");
        assert_eq!(g.matches("format=yuv420p,split[bld").count(), n, "a fork for each clip: {g}");
    }
    let tl = over_a_base(&base, run(&src, 7, 2.0, 30.0));
    let g = graph(&tl, &assets, 30.0);
    assert_ne!(g, unfolded(&tl, &assets, 30.0));
    assert_eq!(g.matches("format=yuv420p,split").count(), 1, "one fork: {g}");
    // The fork's copies: the accumulation's, then each clip's window (the clips are flat 1..=7 after the base).
    assert!(
        g.contains("[vov0]format=yuv420p,split=8[bldf1][bld1w][bld2w][bld3w][bld4w][bld5w][bld6w][bld7w];"),
        "{g}"
    );
    assert!(g.contains("[bldf1_6][bld7y]overlay=eof_action=pass[outv]"), "{g}");
}

#[test]
fn every_clip_of_a_fold_has_the_chains_the_nested_graph_gives_it() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    let tl = over_a_base(&base, run(&src, 9, 2.0, 30.0));
    let (folded, nested) = (graph(&tl, &assets, 30.0), unfolded(&tl, &assets, 30.0));
    let (folded, nested): (Vec<&str>, Vec<&str>) = (folded.split(';').collect(), nested.split(';').collect());
    // What the nested graph has and the fold does not: each clip's fork and the overlay that joins it.
    let gone: Vec<&&str> = nested.iter().filter(|s| !folded.contains(s)).collect();
    assert_eq!(gone.len(), 18, "{gone:?}");
    assert!(gone
        .iter()
        .all(|s| s.contains("format=yuv420p,split[bld") || s.contains("overlay=eof_action=pass[vov") || s.ends_with("[outv]")));
    // What the fold adds: its fork and a join for each clip.
    let added: Vec<&&str> = folded.iter().filter(|s| !nested.contains(s)).collect();
    assert_eq!(added.len(), 1 + 9, "{added:?}");
    assert!(added[0].starts_with("[vov0]format=yuv420p,split=10[bldf1]"));
    // Every pad of the fold is written once and read once.
    let joined = folded.join(";");
    for pad in joined
        .split('[')
        .skip(1)
        .filter_map(|rest| rest.split_once(']').map(|(name, _)| name))
    {
        if pad.starts_with("bld") {
            assert_eq!(joined.matches(&format!("[{pad}]")).count(), 2, "{pad}");
        }
    }
}

#[test]
fn clips_that_share_a_frame_are_not_folded() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    let fps = 30.0;
    // Butted on the frame: the end of one is the first frame of the next.
    let tl = over_a_base(&base, run(&src, 8, 2.0, fps));
    assert_eq!(folds_of(&tl, &assets, fps), vec![(1..=8).collect::<Vec<_>>()]);
    // The fourth runs one frame long into the fifth.
    let mut long = run(&src, 8, 2.0, fps);
    long[3] = blended(&src, 6.0 / fps, 3.0 / fps);
    let tl = over_a_base(&base, long);
    assert!(folds_of(&tl, &assets, fps).is_empty());
    // In the middle of 14, two runs of seven remain.
    let mut long = run(&src, 14, 2.0, fps);
    long[6] = blended(&src, 12.0 / fps, 3.0 / fps);
    let tl = over_a_base(&base, long);
    assert_eq!(
        folds_of(&tl, &assets, fps),
        vec![(1..=7).collect::<Vec<_>>(), (8..=14).collect::<Vec<_>>()]
    );
    // A cut at 0.3 of a frame: the earlier clip is drawn on that frame and the next starts on it.
    let cuts: Vec<f64> = (0..9)
        .map(|i| if i == 0 { 0.0 } else { (5.0 * i as f64 + 0.3) / fps })
        .collect();
    let unaligned: Vec<Clip> = cuts.windows(2).map(|w| blended(&src, w[0], w[1] - w[0])).collect();
    assert!(folds_of(&over_a_base(&base, unaligned), &assets, fps).is_empty());
    // At 0.7 of a frame the next clip's nearest slot is the one after.
    let cuts: Vec<f64> = (0..9)
        .map(|i| if i == 0 { 0.0 } else { (5.0 * i as f64 + 0.7) / fps })
        .collect();
    let rounded: Vec<Clip> = cuts.windows(2).map(|w| blended(&src, w[0], w[1] - w[0])).collect();
    assert_eq!(folds_of(&over_a_base(&base, rounded), &assets, fps).len(), 1);
    // Exactly half a frame is either slot.
    let cuts: Vec<f64> = (0..9)
        .map(|i| if i == 0 { 0.0 } else { (5.0 * i as f64 + 0.5) / fps })
        .collect();
    let half: Vec<Clip> = cuts.windows(2).map(|w| blended(&src, w[0], w[1] - w[0])).collect();
    assert!(folds_of(&over_a_base(&base, half), &assets, fps).is_empty());
}

#[test]
fn ntsc_rates_fold_clips_cut_on_their_frames() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    for (fps, num, den) in [(30000.0 / 1001.0, 30000.0, 1001.0), (24000.0 / 1001.0, 24000.0, 1001.0)] {
        let frame = den / num;
        let clips: Vec<Clip> = (0..9)
            .map(|i| blended(&src, f64::from(i) * 2.0 * frame, 2.0 * frame))
            .collect();
        let tl = over_a_base(&base, clips);
        assert_eq!(folds_of(&tl, &assets, fps).len(), 1, "{fps}");
        let g = graph(&tl, &assets, fps);
        assert!(g.contains("split=10[bldf1]"), "{fps}: {g}");
    }
}

#[test]
fn a_speed_change_moves_a_clips_end_and_the_slots_follow() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    let fps = 30.0;
    // Eight clips of two frames on the timeline, alternately a 2x clip taking four source frames.
    let mut at = 0.0;
    let clips: Vec<Clip> = (0..8)
        .map(|i| {
            let mut clip = blended(&src, at, if i % 2 == 0 { 2.0 / fps } else { 4.0 / fps });
            if i % 2 == 1 {
                clip.speed = 2.0;
            }
            at = clip.timeline_end();
            clip
        })
        .collect();
    assert!((clips[1].timeline_end() - clips[1].timeline_start - 2.0 / fps).abs() < 1e-9);
    assert_eq!(folds_of(&over_a_base(&base, clips), &assets, fps).len(), 1);
    // A slowed clip that runs a frame past the next one's start does not.
    let mut at = 0.0;
    let clips: Vec<Clip> = (0..8)
        .map(|i| {
            let mut clip = blended(&src, at, 2.0 / fps);
            if i == 3 {
                clip.speed = 0.5;
                clip.source_out = clip.source_in + 1.5 / fps;
            }
            at += 2.0 / fps;
            clip
        })
        .collect();
    assert!(clips[3].timeline_end() > clips[4].timeline_start);
    assert!(folds_of(&over_a_base(&base, clips), &assets, fps).is_empty());
}

#[test]
fn a_transition_keeps_its_two_clips_apart() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    let fps = 30.0;
    // The outgoing clip plays on under the incoming one for the length of the dissolve.
    let mut crossed = run(&src, 8, 6.0, fps);
    crossed[4].transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.1,
    });
    assert!(folds_of(&over_a_base(&base, crossed), &assets, fps).is_empty());
    // Two runs of seven either side of it still fold.
    let mut crossed = run(&src, 14, 6.0, fps);
    crossed[7].transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.1,
    });
    assert_eq!(folds_of(&over_a_base(&base, crossed), &assets, fps).len(), 2);
}

#[test]
fn only_blended_footage_on_one_track_folds() {
    let (base, src) = (footage(), footage());
    let mut logo = test_asset(vec![image_stream(64, 64)]);
    logo.path = "/media/logo.png".into();
    let assets = [base.clone(), src.clone(), logo.clone()];
    let fps = 30.0;
    // A plain clip in the middle of eight ends the run on either side.
    let mut clips = run(&src, 8, 2.0, fps);
    clips[3].blend_mode = BlendMode::Normal;
    assert!(folds_of(&over_a_base(&base, clips), &assets, fps).is_empty());
    // So does a blended still: its frames are made up.
    let mut clips = run(&src, 8, 2.0, fps);
    clips[3] = {
        let mut still = make_clip(logo.id, 0.0, 2.0 / fps, 6.0 / fps);
        still.blend_mode = BlendMode::Multiply;
        still
    };
    assert!(folds_of(&over_a_base(&base, clips), &assets, fps).is_empty());
    // Seven and seven on two tracks fold apiece; four and four do not make seven.
    let tl = timeline_of(vec![
        video_track(vec![make_clip(base.id, 0.0, 1.0, 0.0)]),
        video_track(run(&src, 7, 2.0, fps)),
        video_track(run(&src, 7, 2.0, fps)),
    ]);
    assert_eq!(folds_of(&tl, &assets, fps).len(), 2);
    let tl = timeline_of(vec![
        video_track(vec![make_clip(base.id, 0.0, 1.0, 0.0)]),
        video_track(run(&src, 4, 2.0, fps)),
        video_track(run(&src, 4, 2.0, fps)),
    ]);
    assert!(folds_of(&tl, &assets, fps).is_empty());
}

#[test]
fn a_rate_the_canvas_and_the_clips_read_differently_is_not_folded() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    // `29.970029` is 92997/3103 on the canvas and 29970029/1000000 on a clip's `fps`: two grids.
    let fps = 29.970029;
    assert_ne!(Rational::from_fps(fps), Rational::from_fps_filter(fps));
    let tl = over_a_base(&base, run(&src, 8, 2.0, fps));
    assert!(folds_of(&tl, &assets, fps).is_empty());
    assert_eq!(graph(&tl, &assets, fps), unfolded(&tl, &assets, fps));
}

#[test]
fn the_planner_counts_the_forks_the_graph_nests() {
    let (base, src) = (footage(), footage());
    let assets = [base.clone(), src.clone()];
    let fps = 30.0;
    let plan = |tl: &Timeline| {
        let cut = prepared_cut(tl, &opts(fps));
        let fmt = export_format(&cut, &assets, &opts(fps));
        let rate = Rational::from_fps(fmt.fps).unwrap();
        let frames = canvas_frames(cut.duration(), rate);
        let fx = transition_fx(&cut, &assets);
        picture_clips(&cut, &assets, &fx, &fmt, rate, frames)
    };
    // Twelve on the frame fold into one run, which costs three forks; the plain clip under them has none.
    let butted = plan(&over_a_base(&base, run(&src, 12, 2.0, fps)));
    assert_eq!(butted.len(), 13);
    assert_eq!(butted[0].fork, Fork::None);
    assert!(butted[1..].iter().all(|c| c.fork == Fork::Run(0)));
    assert_eq!(open_cost(butted.iter()).2, RUN_FORKS);
    // A part holding some of the run folds what it holds, and counts what they would nested, up to that.
    assert_eq!(open_cost(butted[1..3].iter()).2, 2);
    assert_eq!(open_cost(butted[1..8].iter()).2, RUN_FORKS);
    let tl = over_a_base(&base, run(&src, 12, 2.0, fps));
    assert!(plan_export(&tl, &assets, &opts(fps), SEGMENT_LIMITS).is_none());
    // Twelve whose cuts share a frame cannot, and each is a fork.
    let cuts: Vec<f64> = (0..13)
        .map(|i| if i == 0 { 0.0 } else { (5.0 * f64::from(i) + 0.3) / fps })
        .collect();
    let unaligned: Vec<Clip> = cuts.windows(2).map(|w| blended(&src, w[0], w[1] - w[0])).collect();
    let tl = over_a_base(&base, unaligned);
    let counted = plan(&tl);
    assert_eq!(counted.iter().filter(|c| c.fork == Fork::Own).count(), 12);
    // ... so the export is cut into parts, none with more than six of them open.
    let in_parts = plan_export(&tl, &assets, &opts(fps), SEGMENT_LIMITS).expect("twelve nested blends are cut");
    assert!(in_parts.parts.len() >= 2, "{:?}", in_parts.parts);
    for part in &in_parts.parts {
        let (_, _, forks) = open_cost(in_parts.clips.iter().filter(|c| c.first < part.end() && c.last >= part.first));
        assert!(forks <= SEGMENT_LIMITS.blends, "{part:?}: {forks} forks open");
    }
    // The six that fit are not.
    let six = over_a_base(&base, {
        let cuts: Vec<f64> = (0..7)
            .map(|i| if i == 0 { 0.0 } else { (5.0 * f64::from(i) + 0.3) / fps })
            .collect();
        cuts.windows(2).map(|w| blended(&src, w[0], w[1] - w[0])).collect()
    });
    assert!(plan_export(&six, &assets, &opts(fps), SEGMENT_LIMITS).is_none());
    // A limit of the megapixels the user set does not lift the one of blends.
    assert!(plan_export(&tl, &assets, &opts(fps), limits().unwrap()).is_some());
    // A run and the clips nested beside it: three and five are eight, so the export is cut, and no part holds more than six.
    let alone = unaligned_clips(&src, 5, fps);
    let tl = timeline_of(vec![
        video_track(vec![make_clip(base.id, 0.0, 2.0, 0.0)]),
        video_track(run(&src, 8, 2.0, fps)),
        video_track(alone),
    ]);
    let mixed = plan(&tl);
    assert_eq!(open_cost(mixed.iter()).2, RUN_FORKS + 5);
    let in_parts = plan_export(&tl, &assets, &opts(fps), SEGMENT_LIMITS).expect("eight forks are cut");
    for part in &in_parts.parts {
        let (_, _, forks) = open_cost(in_parts.clips.iter().filter(|c| c.first < part.end() && c.last >= part.first));
        assert!(forks <= 6, "{part:?}: {forks}");
    }
}

/// `n` blended clips of five frames whose cuts share a frame (at 0.3 of one).
fn unaligned_clips(src: &Asset, n: usize, fps: f64) -> Vec<Clip> {
    let cuts: Vec<f64> = (0..=n)
        .map(|i| if i == 0 { 0.0 } else { (5.0 * i as f64 + 0.3) / fps })
        .collect();
    cuts.windows(2).map(|w| blended(src, w[0], w[1] - w[0])).collect()
}

#[test]
fn the_blend_limit_cuts_the_plan_where_the_limits_say() {
    fn blend_clip(first: u64, last: u64) -> PlanClip {
        PlanClip {
            flat: 0,
            first,
            last,
            input: 0,
            mpx: 1.0,
            source_per_frame: 0.04,
            restart: false,
            fork: Fork::Own,
        }
    }
    // Twelve clips of five frames, each sharing the frame the next starts on, over one input.
    let clips: Vec<PlanClip> = (0..12).map(|i| blend_clip(i * 5, i * 5 + 5)).collect();
    assert_eq!(plan_parts(&clips, 61, SEGMENT_LIMITS).map(|p| p.len()), Some(3));
    let parts = plan_parts(&clips, 61, SEGMENT_LIMITS).unwrap();
    for part in &parts {
        let (_, _, forks) = open_cost(clips.iter().filter(|c| c.first < part.end() && c.last >= part.first));
        assert!(forks <= 6, "{part:?}: {forks}");
    }
    // At the limit the export runs whole; twelve folded into a run are one fork.
    assert_eq!(plan_parts(&clips[..6], 31, SEGMENT_LIMITS), None);
    let mut folded = clips.clone();
    folded.iter_mut().for_each(|c| c.fork = Fork::Run(0));
    assert_eq!(plan_parts(&folded, 61, SEGMENT_LIMITS), None);
    // A limit of one blend cuts between every clip that can be.
    let tight = SegmentLimits {
        blends: 1,
        ..SEGMENT_LIMITS
    };
    assert!(plan_parts(&clips, 61, tight).unwrap().len() >= 6);
}

/// A seeded sweep over layouts of blended and plain clips on a few tracks: cuts on the frame and between frames, gaps,
/// overlaps, speed changes, dissolves, stills, and the rates the exporter meets. Whatever the layout, a graph with no run
/// to fold is the nested graph byte for byte, one with runs differs from it by the forks and joins of the folded clips
/// alone, and the planner counts the clips that are not folded.
#[test]
fn a_seeded_sweep_of_layouts_folds_only_what_it_may_and_changes_nothing_else() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n.max(1)
    };
    let (base, src) = (footage(), footage());
    let mut logo = test_asset(vec![image_stream(64, 64)]);
    logo.path = "/media/logo.png".into();
    let assets = [base.clone(), src.clone(), logo.clone()];
    let rates = [24.0, 25.0, 30000.0 / 1001.0, 30.0, 60.0];
    let (mut folded_cases, mut plain_cases, mut folded_clips) = (0, 0, 0);
    for case in 0..400 {
        let fps = rates[next(rates.len() as u64) as usize];
        let mut tracks = vec![video_track(vec![make_clip(base.id, 0.0, 40.0, 0.0)])];
        for _ in 0..1 + next(3) {
            let (mut at, n) = (0.0, next(18));
            let aligned = next(3) != 0;
            let mut clips = Vec::new();
            for _ in 0..n {
                let frames = 1.0 + next(6) as f64 + if aligned { 0.0 } else { next(100) as f64 / 100.0 };
                let gap = if next(8) == 0 { next(4) as f64 } else { 0.0 };
                let overlap = if next(10) == 0 { next(3) as f64 } else { 0.0 };
                let start = ((at / (1.0 / fps) + gap - overlap).max(0.0)) / fps;
                let mut clip = if next(12) == 0 {
                    make_clip(logo.id, 0.0, frames / fps, start)
                } else {
                    make_clip(src.id, 0.5, 0.5 + frames / fps, start)
                };
                if next(6) != 0 {
                    clip.blend_mode = BlendMode::ALL[1 + next(BlendMode::ALL.len() as u64 - 1) as usize];
                }
                match next(14) {
                    0 => clip.speed = 2.0,
                    1 => {
                        clip.transition_in = Some(Transition {
                            kind: TransitionKind::Crossfade,
                            duration: 1.0 / fps,
                        });
                    }
                    _ => {}
                }
                at = clip.timeline_end();
                clips.push(clip);
            }
            tracks.push(video_track(clips));
        }
        let tl = timeline_of(tracks);
        let (folded, nested) = (graph(&tl, &assets, fps), unfolded(&tl, &assets, fps));
        let runs = folds_of(&tl, &assets, fps);
        let members: Vec<usize> = runs.iter().flatten().copied().collect();
        let flats: Vec<(usize, usize, &Clip)> = {
            let mut flat = 0;
            let mut out = Vec::new();
            for (ti, track) in tl.tracks.iter().enumerate() {
                for clip in &track.clips {
                    out.push((flat, ti, clip));
                    flat += 1;
                }
            }
            out
        };
        for run in &runs {
            assert!(run.len() >= FOLD_MIN, "case {case}: {run:?}");
            let on: std::collections::HashSet<usize> = run.iter().map(|f| flats[*f].1).collect();
            assert_eq!(on.len(), 1, "case {case}: one track: {run:?}");
            for f in run {
                let clip = flats[*f].2;
                assert!(
                    clip.blend_mode.ffmpeg().is_some() && clip.asset_id == src.id,
                    "case {case}: {f}"
                );
            }
        }
        if runs.is_empty() {
            plain_cases += 1;
            assert_eq!(folded, nested, "case {case}: nothing to fold");
        } else {
            folded_cases += 1;
            folded_clips += members.len();
            let (f, n): (Vec<&str>, Vec<&str>) = (folded.split(';').collect(), nested.split(';').collect());
            let gone = n.iter().filter(|s| !f.contains(s)).count();
            let added = f.iter().filter(|s| !n.contains(s)).count();
            assert_eq!(gone, 2 * members.len(), "case {case}: a fork and a join a clip");
            assert_eq!(
                added,
                runs.len() + members.len(),
                "case {case}: a fork a run and a join a clip"
            );
            assert_eq!(
                f.iter().filter(|s| s.contains("format=yuv420p,split=")).count(),
                runs.len(),
                "case {case}"
            );
        }
        // The planner counts the forks that are left to nest.
        let cut = prepared_cut(&tl, &opts(fps));
        let fmt = export_format(&cut, &assets, &opts(fps));
        let rate = Rational::from_fps(fmt.fps).unwrap();
        let frames = canvas_frames(cut.duration(), rate);
        let fx = transition_fx(&cut, &assets);
        for c in picture_clips(&cut, &assets, &fx, &fmt, rate, frames) {
            let want = match (
                flats[c.flat].2.blend_mode.ffmpeg(),
                runs.iter().position(|r| r.contains(&c.flat)),
            ) {
                (None, _) => Fork::None,
                (Some(_), Some(run)) => Fork::Run(run),
                (Some(_), None) => Fork::Own,
            };
            assert_eq!(c.fork, want, "case {case}: clip {}", c.flat);
        }
    }
    assert!(
        folded_cases > 20 && plain_cases > 100,
        "the sweep should do both: {folded_cases} folded, {plain_cases} plain"
    );
    assert!(folded_clips > 200, "{folded_clips}");
}
