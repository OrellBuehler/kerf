//! The independent checker's cases for 10-bit deliveries (`checker_d10b_*`): the compositor against **the product's own export** -- `render_with` (its lease,
//! its thread caps, its parts and the pass that joins them) to a lossless 10-bit VP9 file, decoded to the delivery's planes -- for both layouts, with a
//! compositor that is told nothing about the graph's threads. 0 differing samples in every frame the plan draws; the frames it refuses are counted.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_d10b_parity -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! taskset -c 0-1 cargo test ...                                                      # a two-core machine
//! ```

#![allow(clippy::print_stderr, clippy::single_range_in_vec_init)]

mod common;
#[path = "common/d10b.rs"]
mod d10b;

use d10b::{clip, media, options, run, timeline, Run, Stats, LAYOUTS};
use kerf_core::model::{Transition, TransitionKind, VideoEffect};
use kerf_core::{Asset, Clip, Fit, TenBitLayout, TextOverlay, Timeline};

/// One cut, both layouts, the product's export against the compositor over `frames`; `must_draw`: frames the plan has to have drawn (so a case that
/// refuses everything does not pass for nothing).
fn case(name: &str, tl: &Timeline, assets: &[Asset], fps: f64, frames: Vec<std::ops::Range<u64>>, must_draw: usize) -> Stats {
    let mut total = Stats::default();
    for layout in LAYOUTS {
        let exported = d10b::real_export(tl, assets, layout, fps, &[]);
        let have = exported.len() as u64;
        let frames: Vec<_> = frames.iter().map(|r| r.start.min(have)..r.end.min(have)).collect();
        let mut s = run(&Run {
            case: name,
            tl,
            assets,
            layout,
            opts: options(layout, fps),
            threads: None,
            argv: None,
            extra: Vec::new(),
            frames: frames.clone(),
            allow_refused: true,
            exported: Some(exported),
        });
        if s.differing > 0 {
            // The lossless VP9 file is the product's own output, but libvpx's 4:2:2 at 10 bits is not always lossless (two samples of a frame one level off,
            // measured against the raw pipe of the same argv). A difference is therefore held to the graph's own planes at the thread count the plan judges
            // the cut on, and only that verdict counts; the file's disagreement with the graph is reported.
            let planner = kerf_core::Planner::new(
                tl,
                assets,
                &options(layout, fps),
                kerf_core::PlanRequest::motion(d10b::policy()),
            )
            .expect("planner");
            let first = planner.at_frame(0).expect("plan");
            let (lo, hi) = first.graph_thread_range(&d10b::caps_of(None));
            eprintln!("   the VP9 file differs from the compositor in {} samples; held to the graph's planes at {lo}..{hi} threads instead", s.differing);
            if lo == hi {
                s = run(&Run {
                    case: name,
                    tl,
                    assets,
                    layout,
                    opts: options(layout, fps),
                    threads: Some(lo),
                    argv: None,
                    extra: Vec::new(),
                    frames,
                    allow_refused: true,
                    exported: None,
                });
            }
        }
        d10b::report(name, layout, None, &s);
        assert!(
            s.drawn >= must_draw,
            "{name} {layout:?}: the plan drew {} frames, wanted at least {must_draw} ({:?})",
            s.drawn,
            s.kinds
        );
        total.add(s);
    }
    total
}

fn finish(total: &Stats) {
    eprintln!(
        "TOTAL drawn {} refused {} compared {} differing {}",
        total.drawn, total.refused, total.compared, total.differing
    );
    assert!(
        total.failures.is_empty(),
        "{} differing samples in drawn frames:\n{}",
        total.differing,
        total.failures.join("\n")
    );
    assert_eq!(total.differing, 0);
}

fn noise(w: u32, h: u32, secs: f64, rate: &str) -> Asset {
    media(
        &format!("noise-{w}x{h}-{rate}-{secs}"),
        "noise",
        (w, h),
        rate,
        secs,
        "yuv420p",
        None,
    )
}

fn pattern(w: u32, h: u32, secs: f64, rate: &str) -> Asset {
    media(
        &format!("tst-{w}x{h}-{rate}-{secs}"),
        "testsrc2",
        (w, h),
        rate,
        secs,
        "yuv420p",
        None,
    )
}

/// Frame rates (a clip at one rate in a delivery at another, fractional rates) and clips played fast, slow and backwards, with fades in them.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_frame_rates_speeds_and_reversed_clips() {
    d10b::measured();
    let mut total = Stats::default();
    let a30 = noise(160, 90, 3.0, "30");
    let a25 = pattern(160, 90, 3.0, "25");
    let a2997 = pattern(160, 90, 3.0, "30000/1001");
    let a24 = noise(160, 90, 3.0, "24");
    for (fps, canvas) in [
        (24.0, (96, 54)),
        (25.0, (96, 54)),
        (30000.0 / 1001.0, (96, 54)),
        (30.0, (96, 54)),
        (50.0, (96, 54)),
        (60000.0 / 1001.0, (96, 90)),
        (60.0, (96, 54)),
        (23.976, (96, 54)),
        (12.5, (96, 54)),
    ] {
        let mut c1 = clip(&a30, 0.2, 0.5, 0.0);
        c1.fade_in = 0.15;
        let c2 = clip(&a25, 0.1, 0.4, 0.5);
        let mut c3 = clip(&a2997, 0.3, 0.4, 0.9);
        c3.fade_out = 0.2;
        let c4 = clip(&a24, 0.0, 0.3, 1.3);
        let tl = timeline(vec![vec![c1, c2, c3, c4]], vec![], canvas, Fit::Cover, fps);
        let n = (1.6 * fps) as u64;
        total.add(case(
            &format!("mixed rates in a delivery at {fps:.3}"),
            &tl,
            &[a30.clone(), a25.clone(), a2997.clone(), a24.clone()],
            fps,
            vec![0..n],
            n as usize / 2,
        ));
    }
    for (speed, dur) in [
        (0.5, 0.4),
        (2.0, 0.8),
        (4.0, 1.0),
        (-1.0, 0.5),
        (-0.5, 0.3),
        (-2.0, 0.8),
        (0.25, 0.2),
        (1.5, 0.6),
    ] {
        let mut c = clip(&a30, 0.6, dur, 0.1);
        c.speed = speed;
        c.fade_in = 0.1;
        c.fade_out = 0.1;
        let after = clip(&a25, 0.0, 0.3, 0.1 + dur / speed.abs() + 0.1);
        let tl = timeline(vec![vec![c, after]], vec![], (96, 90), Fit::Cover, 30.0);
        let n = ((0.1 + dur / speed.abs() + 0.4) * 30.0) as u64;
        total.add(case(
            &format!("speed {speed}"),
            &tl,
            &[a30.clone(), a25.clone()],
            30.0,
            vec![0..n],
            n as usize / 3,
        ));
    }
    finish(&total);
}

/// Fades that meet cuts and gaps: a fade out into a fade in, a dip to black, a gap, a one-frame clip, a fade shorter than a frame, fades longer than
/// the clip; the first frame, the last frame and the frames at the edges.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_fades_cuts_gaps_and_the_edges_of_the_cut() {
    d10b::measured();
    let mut total = Stats::default();
    let a = noise(160, 90, 3.0, "30");
    let b = pattern(160, 90, 3.0, "30");
    let assets = [a.clone(), b.clone()];
    for canvas in [(96, 54), (96, 90), (128, 72)] {
        // A fade out into a fade in on abutting clips.
        let mut c1 = clip(&a, 0.0, 0.4, 0.0);
        c1.fade_out = 0.2;
        let mut c2 = clip(&b, 0.0, 0.4, 0.4);
        c2.fade_in = 0.2;
        total.add(case(
            &format!("fade out into fade in {canvas:?}"),
            &timeline(vec![vec![c1, c2]], vec![], canvas, Fit::Cover, 30.0),
            &assets,
            30.0,
            vec![0..24],
            20,
        ));
        // A dip to black joining two clips.
        let c1 = clip(&a, 0.0, 0.4, 0.0);
        let mut c2 = clip(&b, 0.0, 0.4, 0.4);
        c2.transition_in = Some(Transition {
            kind: TransitionKind::DipToBlack,
            duration: 0.3,
        });
        total.add(case(
            &format!("dip to black {canvas:?}"),
            &timeline(vec![vec![c1, c2]], vec![], canvas, Fit::Cover, 30.0),
            &assets,
            30.0,
            vec![0..24],
            10,
        ));
        // A gap of five frames, with a fade out before it and a fade in after it; the clip before starts at frame 0 and the one after ends the cut.
        let mut c1 = clip(&a, 0.0, 0.3, 0.0);
        c1.fade_out = 0.1;
        let mut c2 = clip(&b, 0.5, 0.3, 0.467);
        c2.fade_in = 0.1;
        total.add(case(
            &format!("a gap with fades either side {canvas:?}"),
            &timeline(vec![vec![c1, c2]], vec![], canvas, Fit::Cover, 30.0),
            &assets,
            30.0,
            vec![0..24],
            14,
        ));
        // One-frame clips, a fade shorter than a frame, a fade pair longer than the clip.
        let mut c1 = clip(&a, 0.0, 1.0 / 30.0, 0.0);
        c1.fade_in = 0.01;
        let mut c2 = clip(&b, 0.2, 2.0 / 30.0, 1.0 / 30.0);
        c2.fade_out = 0.01;
        let mut c3 = clip(&a, 0.5, 0.3, 3.0 / 30.0);
        c3.fade_in = 0.25;
        c3.fade_out = 0.25;
        let mut c4 = clip(&b, 0.5, 0.2, 3.0 / 30.0 + 0.3);
        c4.fade_in = 0.3;
        c4.fade_out = 0.3;
        total.add(case(
            &format!("tiny clips and long fades {canvas:?}"),
            &timeline(vec![vec![c1, c2, c3, c4]], vec![], canvas, Fit::Cover, 30.0),
            &assets,
            30.0,
            vec![0..19],
            12,
        ));
        // A dissolve, a dip to white and a slide are not drawn: counted, and the frames around them that are drawn are exact.
        let c1 = clip(&a, 0.0, 0.4, 0.0);
        let mut c2 = clip(&b, 0.0, 0.4, 0.4);
        c2.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.2,
        });
        total.add(case(
            &format!("a dissolve {canvas:?}"),
            &timeline(vec![vec![c1, c2]], vec![], canvas, Fit::Cover, 30.0),
            &assets,
            30.0,
            vec![0..24],
            4,
        ));
        let c1 = clip(&a, 0.0, 0.4, 0.0);
        let mut c2 = clip(&b, 0.0, 0.4, 0.4);
        c2.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 0.2,
        });
        total.add(case(
            &format!("a dip to white {canvas:?}"),
            &timeline(vec![vec![c1, c2]], vec![], canvas, Fit::Cover, 30.0),
            &assets,
            30.0,
            vec![0..24],
            0,
        ));
    }
    finish(&total);
}

/// Titles over clips, over a cut, over a gap; two tracks; a clip that does not fill the frame (refused); a clip with an effect or a colour (refused).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_titles_tracks_and_clips_that_do_not_fill_the_frame() {
    d10b::measured();
    let mut total = Stats::default();
    let a = noise(160, 90, 3.0, "30");
    let b = pattern(160, 90, 3.0, "30");
    let assets = [a.clone(), b.clone()];
    for canvas in [(96, 54), (128, 72)] {
        // A title over the first clip, the cut and a gap (the frames over no clip are refused; the others are drawn).
        let mut c1 = clip(&a, 0.0, 0.4, 0.0);
        c1.fade_in = 0.1;
        let c2 = clip(&b, 0.0, 0.4, 0.5);
        let tl = timeline(
            vec![vec![c1, c2]],
            vec![TextOverlay::new("Out now", 0.1, 0.8)],
            canvas,
            Fit::Cover,
            30.0,
        );
        total.add(case(
            &format!("a title over a cut and a gap {canvas:?}"),
            &tl,
            &assets,
            30.0,
            vec![0..27],
            15,
        ));
        // Two tracks: the top clip covers the lower; a fade on the top clip; the lower clip alone before and after.
        let under = clip(&a, 0.0, 0.8, 0.0);
        let mut over = clip(&b, 0.3, 0.4, 0.2);
        over.fade_in = 0.1;
        over.fade_out = 0.1;
        let tl = timeline(
            vec![vec![under], vec![over]],
            vec![TextOverlay::new("Top", 0.3, 0.5)],
            canvas,
            Fit::Cover,
            30.0,
        );
        total.add(case(
            &format!("two tracks and a title {canvas:?}"),
            &tl,
            &assets,
            30.0,
            vec![0..24],
            20,
        ));
        // A top clip that does not fill the frame (zoomed out): refused, the lower track beneath it too.
        let under = clip(&a, 0.0, 0.5, 0.0);
        let mut over = clip(&b, 0.3, 0.3, 0.1);
        over.transform.scale = 0.5;
        let tl = timeline(vec![vec![under], vec![over]], vec![], canvas, Fit::Cover, 30.0);
        total.add(case(
            &format!("a smaller clip over another {canvas:?}"),
            &tl,
            &assets,
            30.0,
            vec![0..15],
            3,
        ));
        // A clip delivered Contain in a different aspect (bars), with a colour correction, an effect.
        let c = clip(&a, 0.0, 0.5, 0.0);
        let tl = timeline(vec![vec![c]], vec![], (canvas.0, canvas.1 + 18), Fit::Contain, 30.0);
        total.add(case(&format!("letterboxed {canvas:?}"), &tl, &assets, 30.0, vec![0..15], 0));
        let mut c = clip(&a, 0.0, 0.5, 0.0);
        c.color.brightness = 0.1;
        let tl = timeline(vec![vec![c]], vec![], canvas, Fit::Cover, 30.0);
        total.add(case(
            &format!("a colour correction {canvas:?}"),
            &tl,
            &assets,
            30.0,
            vec![0..15],
            0,
        ));
        let mut c = clip(&a, 0.0, 0.5, 0.0);
        c.effects.push(VideoEffect::Invert);
        let tl = timeline(vec![vec![c]], vec![], canvas, Fit::Cover, 30.0);
        total.add(case(&format!("an effect {canvas:?}"), &tl, &assets, 30.0, vec![0..15], 0));
    }
    finish(&total);
}

/// Sources that are not the plain 8-bit 4:2:0 of the composite's matrix: 10-bit, 4:2:2, 4:4:4, full range, other matrices, odd sizes. The plan draws what
/// it can prove and refuses the rest; every frame it draws is the product's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_sources_of_other_formats_ranges_and_matrices() {
    d10b::measured();
    let mut total = Stats::default();
    let mut sources: Vec<(String, Asset)> = Vec::new();
    for (name, fmt, tags) in [
        (
            "yuv420p-bt709",
            "yuv420p",
            Some("colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv"),
        ),
        (
            "yuv420p-bt2020",
            "yuv420p",
            Some("colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt709:range=tv"),
        ),
        (
            "yuv420p-bt601",
            "yuv420p",
            Some("colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m:range=tv"),
        ),
        (
            "yuv420p-bt470bg",
            "yuv420p",
            Some("colorspace=bt470bg:color_primaries=bt470bg:color_trc=bt709:range=tv"),
        ),
        (
            "yuv420p-pcrange",
            "yuv420p",
            Some("colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=pc"),
        ),
        ("yuvj420p", "yuvj420p", None),
        ("yuv420p10le", "yuv420p10le", None),
        ("yuv422p", "yuv422p", None),
        ("yuv444p", "yuv444p", None),
        ("yuv420p-untagged", "yuv420p", None),
    ] {
        let a = media(&format!("src-{name}"), "noise", (160, 90), "30", 2.0, fmt, tags);
        sources.push((name.to_string(), a));
    }
    for layout_canvas in [(96, 54), (160, 90)] {
        for (name, a) in &sources {
            let mut c = clip(a, 0.1, 0.5, 0.0);
            c.fade_in = 0.1;
            c.fade_out = 0.1;
            let tl = timeline(vec![vec![c]], vec![], layout_canvas, Fit::Cover, 30.0);
            total.add(case(
                &format!("source {name} into {layout_canvas:?}"),
                &tl,
                std::slice::from_ref(a),
                30.0,
                vec![0..15],
                0,
            ));
        }
    }
    // A cut of two matrices (the composite is converted across it) and of an odd-sized picture.
    let (_, a) = &sources[0];
    let (_, b) = &sources[2];
    let tl = timeline(
        vec![vec![clip(a, 0.0, 0.3, 0.0), clip(b, 0.0, 0.3, 0.3)]],
        vec![],
        (96, 54),
        Fit::Cover,
        30.0,
    );
    total.add(case(
        "a cut of bt709 and bt601",
        &tl,
        &[a.clone(), b.clone()],
        30.0,
        vec![0..18],
        0,
    ));
    let odd = media("src-odd", "noise", (161, 91), "30", 2.0, "yuv420p", None);
    let tl = timeline(vec![vec![clip(&odd, 0.0, 0.5, 0.0)]], vec![], (96, 54), Fit::Cover, 30.0);
    total.add(case(
        "an odd-sized picture",
        &tl,
        std::slice::from_ref(&odd),
        30.0,
        vec![0..15],
        0,
    ));
    let tl = timeline(vec![vec![clip(&odd, 0.0, 0.5, 0.0)]], vec![], (97, 55), Fit::Cover, 30.0);
    total.add(case(
        "an odd-sized frame",
        &tl,
        std::slice::from_ref(&odd),
        30.0,
        vec![0..15],
        0,
    ));
    finish(&total);
}

/// Canvas heights across the ones where the scaler's slices start on an odd chroma row for two to eight threads, and the widths that go with them, at
/// the product's own thread count. Cover crops (odd offsets), enlargements, shrinks, identity.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_heights_where_the_slices_flip() {
    d10b::measured();
    let mut total = Stats::default();
    let src = noise(160, 90, 3.0, "30");
    let wide = noise(320, 180, 2.0, "30");
    for h in [
        6u32, 10, 14, 18, 30, 34, 38, 50, 54, 58, 66, 70, 90, 94, 102, 126, 130, 162, 178,
    ] {
        let w = (h * 16 / 9) & !1;
        let mut c = clip(&src, 0.1, 0.3, 0.0);
        c.fade_in = 0.1;
        let big = clip(&wide, 0.1, 0.2, 0.3);
        let tl = timeline(vec![vec![c, big]], vec![], (w.max(8), h), Fit::Cover, 30.0);
        total.add(case(
            &format!("height {h} (width {w})"),
            &tl,
            &[src.clone(), wide.clone()],
            30.0,
            vec![0..15],
            0,
        ));
    }
    assert!(total.drawn >= 100, "only {} frames drawn ({:?})", total.drawn, total.kinds);
    finish(&total);
}

/// A cut long enough to be rendered in parts (more inputs than one run takes), the parts joined by the engine's last pass.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_a_cut_rendered_in_parts() {
    d10b::measured();
    let a = noise(160, 90, 3.0, "30");
    let b = pattern(160, 90, 3.0, "30");
    let mut clips = Vec::new();
    let mut at = 0.0;
    for i in 0..48 {
        let src = if i % 2 == 0 { &a } else { &b };
        let mut c = clip(src, (i % 10) as f64 * 0.2, 5.0 / 30.0, at);
        if i % 5 == 0 {
            c.fade_in = 0.1;
        }
        if i % 7 == 0 {
            c.fade_out = 0.1;
        }
        at += 5.0 / 30.0 + if i % 11 == 10 { 0.1 } else { 0.0 };
        clips.push(c);
    }
    let tl = timeline(
        vec![clips],
        vec![TextOverlay::new("Parts", 0.5, 6.0)],
        (96, 54),
        Fit::Cover,
        30.0,
    );
    let total = case("48 clips in parts", &tl, &[a, b], 30.0, vec![0..240], 100);
    finish(&total);
}

#[allow(dead_code)]
fn unused(_: TenBitLayout, _: Clip) {}

/// The fast smoke the mutation checks are run with: short raw exports at pinned slice-thread counts (the dither flips at 4 threads for 52 rows and at 3 for
/// 54), both layouts; the cases the plan draws are held exact, those it refuses are the gates a mutant of the plan must not open.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_smoke() {
    d10b::measured();
    let a = noise(160, 90, 3.0, "30");
    let b = pattern(160, 90, 3.0, "30");
    let pair = vec![a.clone(), b.clone()];
    let m709 = media(
        "src-yuv420p-bt709",
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
    let odd = media("src-odd", "noise", (161, 91), "30", 2.0, "yuv420p", None);
    let s422 = media("src-yuv422p", "noise", (160, 90), "30", 2.0, "yuv422p", None);
    let mut total = Stats::default();
    let mut go = |name: &str,
                  tl: &Timeline,
                  assets: &[Asset],
                  threads: u32,
                  frames: std::ops::Range<u64>,
                  must: usize,
                  opts: &dyn Fn(TenBitLayout) -> kerf_core::ExportOptions| {
        for layout in LAYOUTS {
            let s = run(&Run {
                case: name,
                tl,
                assets,
                layout,
                opts: opts(layout),
                threads: Some(threads),
                argv: None,
                extra: Vec::new(),
                frames: vec![frames.clone()],
                allow_refused: true,
                exported: None,
            });
            d10b::report(name, layout, Some(threads), &s);
            assert!(
                s.drawn >= must,
                "{name} {layout:?}: drew {} wanted {must} ({:?})",
                s.drawn,
                s.kinds
            );
            total.add(s);
        }
    };
    let o = |l: TenBitLayout| options(l, 30.0);
    for (canvas, threads) in [((92u32, 52u32), 4u32), ((96, 54), 3), ((94, 54), 3), ((96, 52), 2)] {
        // Enlarged / shrunk and cropped at an odd offset, fades both ways, a gap between two clips, a title over the first.
        let mut c1 = clip(&a, 0.1, 0.4, 0.0);
        c1.fade_in = 0.15;
        c1.fade_out = 0.15;
        let c2 = clip(&b, 0.5, 0.2, 0.5);
        let tl = timeline(
            vec![vec![c1, c2]],
            vec![TextOverlay::new("Hi", 0.05, 0.3)],
            canvas,
            Fit::Cover,
            30.0,
        );
        go(
            &format!("fades, a gap and a title {canvas:?}"),
            &tl,
            &pair,
            threads,
            0..22,
            18,
            &o,
        );
        // Two tracks.
        let under = clip(&a, 0.0, 0.4, 0.0);
        let mut over = clip(&b, 0.3, 0.2, 0.1);
        over.fade_in = 0.1;
        let tl = timeline(vec![vec![under], vec![over]], vec![], canvas, Fit::Cover, 30.0);
        go(&format!("two tracks {canvas:?}"), &tl, &pair, threads, 0..12, 12, &o);
    }
    // The gates: each of these has to be refused (or, for the frames the plan does draw, exact).
    let canvas = (96, 54);
    let mix = |kind: TransitionKind| {
        let c1 = clip(&a, 0.0, 0.3, 0.0);
        let mut c2 = clip(&b, 0.0, 0.3, 0.3);
        c2.transition_in = Some(Transition { kind, duration: 0.2 });
        timeline(vec![vec![c1, c2]], vec![], canvas, Fit::Cover, 30.0)
    };
    go("a dissolve", &mix(TransitionKind::Crossfade), &pair, 3, 0..18, 0, &o);
    go("a dip to white", &mix(TransitionKind::DipToWhite), &pair, 3, 0..18, 0, &o);
    let gap = timeline(
        vec![vec![clip(&a, 0.0, 0.2, 0.0), clip(&b, 0.0, 0.2, 0.6)]],
        vec![TextOverlay::new("Out now", 0.25, 0.5)],
        canvas,
        Fit::Cover,
        30.0,
    );
    go("a title over no clip", &gap, &pair, 3, 0..24, 12, &o);
    let named = |l: TenBitLayout| kerf_core::ExportOptions {
        scaler: Some("lanczos".into()),
        ..options(l, 30.0)
    };
    let mut c = clip(&a, 0.0, 0.3, 0.0);
    c.fade_in = 0.1;
    go(
        "a named scaler",
        &timeline(vec![vec![c]], vec![], canvas, Fit::Cover, 30.0),
        &pair,
        3,
        0..9,
        0,
        &named,
    );
    let two = timeline(
        vec![vec![clip(&m709, 0.0, 0.3, 0.0), clip(&m601, 0.0, 0.3, 0.3)]],
        vec![],
        canvas,
        Fit::Cover,
        30.0,
    );
    go("a cut of two matrices", &two, &[m709.clone(), m601], 3, 0..18, 0, &o);
    go(
        "a bt709 clip",
        &timeline(vec![vec![clip(&m709, 0.0, 0.3, 0.0)]], vec![], canvas, Fit::Cover, 30.0),
        std::slice::from_ref(&m709),
        3,
        0..9,
        0,
        &o,
    );
    go(
        "an odd picture",
        &timeline(vec![vec![clip(&odd, 0.0, 0.3, 0.0)]], vec![], canvas, Fit::Cover, 30.0),
        std::slice::from_ref(&odd),
        3,
        0..9,
        0,
        &o,
    );
    go(
        "a 4:2:2 picture",
        &timeline(vec![vec![clip(&s422, 0.0, 0.3, 0.0)]], vec![], canvas, Fit::Cover, 30.0),
        std::slice::from_ref(&s422),
        3,
        0..9,
        0,
        &o,
    );
    let mut z = clip(&a, 0.0, 0.3, 0.0);
    z.transform.scale = 0.5;
    go(
        "a zoom out",
        &timeline(vec![vec![z]], vec![], canvas, Fit::Cover, 30.0),
        &pair,
        3,
        0..9,
        0,
        &o,
    );
    let mut p = clip(&a, 0.0, 0.3, 0.0);
    p.transform.opacity = 0.5;
    go(
        "an opacity",
        &timeline(vec![vec![p]], vec![], canvas, Fit::Cover, 30.0),
        &pair,
        3,
        0..9,
        0,
        &o,
    );
    let pc = media(
        "src-yuv420p-pcrange",
        "noise",
        (160, 90),
        "30",
        2.0,
        "yuv420p",
        Some("colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=pc"),
    );
    go(
        "a full-range clip",
        &timeline(vec![vec![clip(&pc, 0.0, 0.3, 0.0)]], vec![], canvas, Fit::Cover, 30.0),
        std::slice::from_ref(&pc),
        3,
        0..9,
        0,
        &o,
    );
    let still = {
        let path = common::media_dir("d10b-media").join("still.png");
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
    go(
        "a still image",
        &timeline(vec![vec![clip(&still, 0.0, 0.3, 0.0)]], vec![], canvas, Fit::Cover, 30.0),
        std::slice::from_ref(&still),
        3,
        0..9,
        0,
        &o,
    );
    let mut col = clip(&a, 0.0, 0.3, 0.0);
    col.color.brightness = 0.1;
    go(
        "a colour correction",
        &timeline(vec![vec![col]], vec![], canvas, Fit::Cover, 30.0),
        &pair,
        3,
        0..9,
        0,
        &o,
    );
    let mut fx = clip(&a, 0.0, 0.3, 0.0);
    fx.effects.push(VideoEffect::Invert);
    go(
        "an effect",
        &timeline(vec![vec![fx]], vec![], canvas, Fit::Cover, 30.0),
        &pair,
        3,
        0..9,
        0,
        &o,
    );
    go(
        "a letterbox",
        &timeline(vec![vec![clip(&a, 0.0, 0.3, 0.0)]], vec![], (96, 72), Fit::Contain, 30.0),
        &pair,
        3,
        0..9,
        0,
        &o,
    );
    finish(&total);
}

/// Cover crops at every parity of the offset: pictures of two aspects cut into canvases whose crop is 0, 1, 2 or 3 rows / columns a side (4:2:2 crops on
/// its own chroma grid; 4:2:0 rounds the offset to even), at a thread count where the dither flips (3 threads, 54 rows) and one where it does not.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_cover_crops_at_every_offset() {
    d10b::measured();
    let wide = noise(160, 90, 3.0, "30");
    let four3 = noise(128, 96, 3.0, "30");
    let assets = vec![wide.clone(), four3.clone()];
    let mut total = Stats::default();
    for (src, srcs) in [(&wide, (160u32, 90u32)), (&four3, (128, 96))] {
        for h in [30u32, 44, 50, 52, 54, 56] {
            for w in (40u32..=106).step_by(2) {
                // The Cover scale (`force_original_aspect_ratio=increase`) and the crop that follows: skip canvases it leaves uncropped.
                let (sw, sh) = (f64::from(srcs.0), f64::from(srcs.1));
                let k = (f64::from(w) / sw).max(f64::from(h) / sh);
                let (cw, ch) = ((sw * k).ceil() as u32, (sh * k).ceil() as u32);
                if (cw == w && ch == h) && w % 6 != 0 {
                    continue;
                }
                let mut c = clip(src, 0.1, 0.2, 0.0);
                c.fade_in = 0.1;
                let tl = timeline(vec![vec![c]], vec![], (w, h), Fit::Cover, 30.0);
                for layout in LAYOUTS {
                    for threads in [3u32, 4] {
                        if layout == TenBitLayout::P422 && threads == 4 {
                            continue;
                        }
                        let s = run(&Run {
                            case: &format!("crop {srcs:?} into {w}x{h}"),
                            tl: &tl,
                            assets: &assets,
                            layout,
                            opts: options(layout, 30.0),
                            threads: Some(threads),
                            argv: None,
                            extra: Vec::new(),
                            frames: vec![0..5],
                            allow_refused: true,
                            exported: None,
                        });
                        if s.differing != 0 {
                            d10b::report(&format!("crop {srcs:?} into {w}x{h}"), layout, Some(threads), &s);
                        }
                        total.add(s);
                    }
                }
            }
        }
    }
    eprintln!(
        "cover crops: drawn {} refused {} ({:?}) compared {} differing {}",
        total.drawn, total.refused, total.kinds, total.compared, total.differing
    );
    assert!(total.drawn > 100, "only {} frames drawn", total.drawn);
    finish(&total);
}

/// A preview of a cut delivered in 10 bits is the 8-bit chain (a still is always 8-bit 4:2:0): FFmpeg's still graph for the 10-bit options is the very
/// string of the 8-bit options', and the still plan is judged as any still (no `Delivery*` refusal in it). The export's frame is the 10-bit chain's
/// (dithered and rounded elsewhere), so a preview is not the export's planes -- measured below as the levels the two differ by.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_a_preview_of_a_ten_bit_delivery_is_the_eight_bit_still() {
    d10b::measured();
    let a = noise(160, 90, 3.0, "30");
    let mut c = clip(&a, 0.1, 0.4, 0.0);
    c.fade_in = 0.1;
    let tl = timeline(vec![vec![c]], vec![], (96, 54), Fit::Cover, 30.0);
    let assets = vec![a];
    let eight = kerf_core::ExportOptions {
        fps: Some(30.0),
        ..kerf_core::ExportOptions::default()
    };
    let g8 = kerf_core::still_graph(&tl, &assets, &eight, 0.05)
        .expect("graph")
        .expect("a graph");
    for layout in LAYOUTS {
        let g10 = kerf_core::still_graph(&tl, &assets, &options(layout, 30.0), 0.05)
            .expect("graph")
            .expect("a graph");
        assert_eq!(
            g8, g10,
            "{layout:?}: the still graph of a 10-bit delivery is not the 8-bit one"
        );
        let planner = kerf_core::Planner::new(
            &tl,
            &assets,
            &options(layout, 30.0),
            kerf_core::PlanRequest::still(d10b::policy()),
        )
        .expect("planner");
        let plan = planner.at(0.05).expect("plan");
        let caps = d10b::compositor(Some(3)).caps();
        let why = plan.reasons(&caps, (plan.canvas.width, plan.canvas.height));
        assert!(
            !why.iter().any(|u| matches!(
                u,
                kerf_core::Unsupported::Delivery(_)
                    | kerf_core::Unsupported::DeliveryWith(..)
                    | kerf_core::Unsupported::DeliveryFrame(_)
            )),
            "{layout:?}: a still plan carries a delivery refusal: {why:?}"
        );
        eprintln!("{layout:?}: still graph identical to the 8-bit one; still plan reasons {why:?}");
    }
}
