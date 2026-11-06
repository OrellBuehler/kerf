//! `checker/p/gblur-rule`: the rule that says where FFmpeg's `gblur` is not the recursion the compositor draws
//! (`kerf_core::gblur::plane_hazard`, `picture_hazard`, `cpu_graph_thread_range`), held to a model written here from the control flow of
//! `vf_gblur.c` and `x86/vf_gblur.asm` (the same in FFmpeg 6.1 and 9.0), not from the rule's closed form. No FFmpeg: the FFmpeg half is
//! `kerf-gpu`'s `tests/checker_p_blur.rs` and `tests/blur_slices.rs`.
//!
//! The kernel, as read:
//!
//! * `gaussianiir2d` cuts a plane into `min(height, threads)` row slices for the horizontal pass (`height * job / jobs` to
//!   `height * (job + 1) / jobs`), `min(width, threads)` column slices for the vertical one and `min(width * height, threads)` slices of
//!   rows for `filter_postscale`.
//! * `horiz_slice` (AVX2: 8 floats, AVX-512: 16): a slice of fewer rows than a vector takes the masked path, whose loop over the
//!   `width - 1` samples after the first is a do-while (one pass at least: wrong for a plane of width 1). A slice of a vector of rows or
//!   more takes the blocked path, whose loop over the same samples runs `lanes` columns at a time, also a do-while: a row of `lanes`
//!   samples or fewer is read and written past its end.
//! * `verti_slice` loops over the rows below the first as a do-while: wrong for a plane of height 1.
//! * `postscale_slice` scales one whole vector at least, so a slice of no rows (more slices than rows) scales the vector at the start of
//!   the buffer a second time.
#![allow(clippy::print_stderr)]

use kerf_core::gblur::{picture_hazard, plane_hazard, KernelHazard};
use std::sync::Mutex;

/// What the kernel gets wrong on a `w` x `h` plane on `t` slice threads with vectors of `lanes` floats, by simulating its slices and loops.
#[derive(Default, Debug, PartialEq, Eq)]
struct Faults {
    one_sample: bool,
    empty_slice: bool,
    narrow_block: bool,
}

fn simulate(w: u32, h: u32, t: u32, lanes: u32) -> Faults {
    let mut faults = Faults::default();
    let (w, h, t, lanes) = (i64::from(w), i64::from(h), i64::from(t.max(1)), i64::from(lanes));
    if w <= 1 || h <= 1 {
        faults.one_sample = true;
        return faults;
    }
    // The horizontal pass.
    let jobs = h.min(t);
    for job in 0..jobs {
        let rows = h * (job + 1) / jobs - h * job / jobs;
        if rows >= lanes {
            // `.loop_x`: do { lanes columns; x -= lanes } while x >= lanes, from x = width - 1; then the same leftwards.
            let (mut x, mut done) = (w - 1, 0);
            loop {
                done += lanes;
                x -= lanes;
                if x < lanes {
                    break;
                }
            }
            if done > w - 1 {
                faults.narrow_block = true;
            }
        }
    }
    // The postscale pass: a slice of no rows still scales a vector.
    let jobs = (w * h).min(t);
    for job in 0..jobs {
        if h * (job + 1) / jobs - h * job / jobs == 0 {
            faults.empty_slice = true;
        }
    }
    faults
}

fn model(w: u32, h: u32, t: u32, lanes: u32) -> Option<KernelHazard> {
    let f = simulate(w, h, t, lanes);
    if f.one_sample {
        Some(KernelHazard::OneSample)
    } else if f.empty_slice {
        Some(KernelHazard::EmptySlices)
    } else if f.narrow_block {
        Some(KernelHazard::NarrowBlocks)
    } else {
        None
    }
}

#[test]
fn checker_p_gblur_rule_a_plane_is_a_hazard_exactly_where_the_kernels_loops_are_wrong() {
    let mut checked = 0u64;
    let mut by_kind = [0u64; 4];
    for lanes in [8u32, 16] {
        for w in 0..=70u32 {
            for h in 0..=300u32 {
                for t in 1..=70u32 {
                    let want = model(w, h, t, lanes);
                    let got = plane_hazard((w, h), t, lanes);
                    assert_eq!(got, want, "{w}x{h} on {t} threads, {lanes} lanes");
                    checked += 1;
                    by_kind[match got {
                        None => 0,
                        Some(KernelHazard::OneSample) => 1,
                        Some(KernelHazard::EmptySlices) => 2,
                        Some(KernelHazard::NarrowBlocks) => 3,
                    }] += 1;
                }
            }
        }
    }
    eprintln!("CHECKER-P rule: {checked} planes, clear / one sample / empty slices / narrow blocks: {by_kind:?}");
    assert!(
        by_kind.iter().all(|n| *n > 1000),
        "the grid should meet every kind: {by_kind:?}"
    );
    // Zero threads are one (the plan clamps a pin).
    for (w, h) in [(12, 16), (40, 3), (2, 2), (17, 16)] {
        for lanes in [8, 16] {
            assert_eq!(
                plane_hazard((w, h), 0, lanes),
                plane_hazard((w, h), 1, lanes),
                "{w}x{h} {lanes}"
            );
        }
    }
}

/// The boundaries by hand (what a flipped comparison would move): the threads against the rows, the slice's rows against the vector,
/// the row's length against the vector.
#[test]
fn checker_p_gblur_rule_the_boundaries() {
    use KernelHazard::*;
    for lanes in [8u32, 16] {
        // Rows against threads: `threads > height` is the hazard, `threads == height` is not.
        assert_eq!(plane_hazard((100, lanes + 4), lanes + 4, lanes), None);
        assert_eq!(plane_hazard((100, lanes + 4), lanes + 5, lanes), Some(EmptySlices));
        assert_eq!(plane_hazard((100, 2), 2, lanes), None);
        assert_eq!(plane_hazard((100, 2), 3, lanes), Some(EmptySlices));
        // Width against the vector: `width <= lanes` under a vector of rows is the hazard, `width == lanes + 1` is not.
        assert_eq!(plane_hazard((lanes, lanes), 1, lanes), Some(NarrowBlocks));
        assert_eq!(plane_hazard((lanes + 1, lanes), 1, lanes), None);
        assert_eq!(plane_hazard((lanes - 1, lanes), 1, lanes), Some(NarrowBlocks));
        // Slice rows against the vector: `rows >= lanes` is the hazard, `rows == lanes - 1` is not.
        assert_eq!(plane_hazard((lanes, lanes - 1), 1, lanes), None);
        // The largest slice decides: `2 * lanes - 1` rows in two slices are `lanes - 1` and `lanes`; `2 * lanes - 2` are `lanes - 1` twice.
        assert_eq!(plane_hazard((4, 2 * lanes - 1), 2, lanes), Some(NarrowBlocks));
        assert_eq!(plane_hazard((4, 2 * lanes - 2), 2, lanes), None);
        // One sample across, either way, at any thread count.
        for t in [1, 2, 3, 8, 64] {
            assert_eq!(plane_hazard((1, 50), t, lanes), Some(OneSample));
            assert_eq!(plane_hazard((50, 1), t, lanes), Some(OneSample));
            assert_eq!(plane_hazard((1, 1), t, lanes), Some(OneSample));
        }
    }
    // The vector widths differ: 8 columns over 8 rows is the 8-lane kernel's and not the 16-lane one's; 12 over 16 the other way round.
    assert_eq!(plane_hazard((8, 8), 1, 8), Some(NarrowBlocks));
    assert_eq!(plane_hazard((8, 8), 1, 16), None);
    assert_eq!(plane_hazard((12, 16), 1, 8), None);
    assert_eq!(plane_hazard((12, 16), 1, 16), Some(NarrowBlocks));
}

/// A picture is judged by its luma and its half-size chroma (rounded up) at every thread count of the range and every vector width it
/// could have, `0` being "not known which": both.
#[test]
fn checker_p_gblur_rule_a_picture_is_its_luma_and_rounded_up_chroma_at_every_count_and_width() {
    let widths = |lanes: u32| -> Vec<u32> {
        match lanes {
            0 => vec![8, 16],
            l => vec![l],
        }
    };
    let ranges: [(u32, u32); 7] = [(1, 1), (2, 2), (5, 5), (1, 16), (5, 16), (16, 16), (63, 63)];
    let mut met_clear = 0;
    let mut met_hazard = 0;
    for w in (0..=90u32).chain([127, 128, 129, 255, 640, 641, 1920]) {
        for h in (0..=90u32).chain([127, 128, 129, 255, 360, 361, 1080]) {
            for (lo, hi) in ranges {
                for lanes in [0u32, 8, 16] {
                    let mut want = None;
                    'search: for t in lo..=hi {
                        for (pw, ph) in [(w, h), (w.div_ceil(2), h.div_ceil(2))] {
                            for &l in &widths(lanes) {
                                if let Some(kind) = model(pw, ph, t, l) {
                                    want = Some((kind, pw, ph, t));
                                    break 'search;
                                }
                            }
                        }
                    }
                    let got = picture_hazard((w, h), lo..=hi, lanes);
                    assert_eq!(
                        got.map(|g| (g.hazard, g.dims.0, g.dims.1, g.threads)),
                        want,
                        "{w}x{h} over {lo}..={hi}, {lanes} lanes"
                    );
                    if got.is_some() {
                        met_hazard += 1;
                    } else {
                        met_clear += 1;
                    }
                }
            }
        }
    }
    eprintln!("CHECKER-P rule: pictures clear {met_clear}, hazardous {met_hazard}");
    assert!(met_clear > 1000 && met_hazard > 1000);
    // A full-size picture is nobody's hazard at any count FFmpeg's graph can run on (a 16-thread ceiling, or a budget up to 64 cores:
    // its chroma has 64 rows or more); a small one is at the counts above its chroma rows.
    for (w, h) in [(1920, 1080), (1280, 720), (640, 360), (320, 180), (256, 128)] {
        assert_eq!(picture_hazard((w, h), 1..=64, 0), None, "{w}x{h}");
    }
    assert_eq!(picture_hazard((160, 90), 1..=45, 0), None);
    assert_eq!(picture_hazard((160, 90), 1..=46, 0).map(|p| p.threads), Some(46));
}

static BUDGET: Mutex<()> = Mutex::new(());

/// The slice threads the plan judges a graph over are the ones the engine asks FFmpeg for: at every budget, the range is exactly the
/// `-filter_complex_threads` that `limit_ffmpeg_args` writes where it writes one; else, where FFmpeg's own core count is reproduced
/// (`kerf_core::ffmpeg_cpu_count`, Linux), exactly what FFmpeg's default comes to for it (`avpriv_slicethread_create`: one more than the
/// cores it counts, at most 16, one on a single core); else it is `1..=16`, which holds FFmpeg's default for every core count, the
/// one of a failed call included. The exact case replaced a range of 5 to 16 on
/// four cores, which an uncapped export judged though FFmpeg runs it on 5.
#[test]
fn checker_p_gblur_rule_the_thread_range_is_what_the_engine_asks_ffmpeg_for() {
    let _budget = BUDGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    let cores = kerf_core::cpu_cores();
    let ffmpeg_default = |ncpu: usize| if ncpu > 1 { (ncpu + 1).min(16) } else { 1 };
    let (mut capped, mut uncapped) = (0, 0);
    for percent in 10..=100u8 {
        kerf_core::set_cpu_percent(percent);
        let mut args: Vec<String> = ["-i", "in.mkv", "out.mkv"].map(String::from).to_vec();
        kerf_core::limit_ffmpeg_args(&mut args, 1);
        let asked = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|at| args.get(at + 1))
                .and_then(|n| n.parse::<usize>().ok())
        };
        let (lo, hi) = kerf_core::cpu_graph_thread_range();
        match (asked("-filter_complex_threads"), asked("-filter_threads")) {
            (Some(n), Some(m)) => {
                capped += 1;
                assert_eq!(n, m, "{percent}%");
                assert_eq!(
                    (lo, hi),
                    (n, n),
                    "{percent}% of {cores} cores asks FFmpeg for {n} slice threads: {args:?}"
                );
            }
            (None, None) => {
                uncapped += 1;
                assert!(lo <= hi, "{percent}%: {lo}..={hi}");
                if let Some(count) = kerf_core::ffmpeg_cpu_count() {
                    let exact = ffmpeg_default(count);
                    assert_eq!(
                        (lo, hi),
                        (exact, exact),
                        "{percent}%: FFmpeg counts {count} cores, {cores} for us"
                    );
                    continue;
                }
                for ncpu in cores..=(cores + 512) {
                    let n = ffmpeg_default(ncpu);
                    assert!(
                        (lo..=hi).contains(&n),
                        "{percent}% of {cores} cores, FFmpeg on {ncpu}: {n} slice threads, outside the judged {lo}..={hi}"
                    );
                }
                // (A count that is not reproduced may be FFmpeg's one of a failed call, so the range is every default there is.)
                assert_eq!((lo, hi), (1, kerf_core::MAX_DEFAULT_GRAPH_THREADS), "{percent}%");
            }
            other => panic!("{percent}%: one of the two thread flags only: {other:?} {args:?}"),
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!("CHECKER-P rule: on {cores} cores, {capped} budgets cap the graph, {uncapped} leave it to FFmpeg");
    assert!(capped > 0 || cores == 1, "no budget capped the graph");
    assert!(uncapped > 0, "100% caps nothing");
}
