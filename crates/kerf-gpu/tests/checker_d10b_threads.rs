//! The independent checker's cases for the threads of a 10-bit delivery (`checker_d10b_threads`): FFmpeg 6.1 restarts the 4:2:0 dither at each slice of
//! the scaler's threads, so the plan must know the count the export really runs its graph on -- the count `cpu::limit_args` and the lease write into the
//! argv it spawns, a wide graph's cap (`cap_wide_graph`), and FFmpeg's own default for the cores it counts (`-cpucount` stands in for a bigger machine).
//!
//! Needs `ffmpeg` and an adapter (lavapipe is enough); `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_d10b_threads -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

mod common;
#[path = "common/d10b.rs"]
mod d10b;

use std::path::PathBuf;

use d10b::{clip, media, options, run, timeline, Run, Stats, LAYOUTS};
use kerf_core::{Clip, ExportOptions, Fit, PlanRequest, Planner, TenBitLayout};

/// The argv the engine spawns for the export of `tl` (the thread caps, the progress flags and all), captured by an `ffmpeg` that records the argv of
/// every run with a graph and stops each: `render_with` is the engine's own path (lease, budget, `spawn_caps`). A cut rendered in parts
/// (`KERF_EXPORT_PARTS` unset) spawns several; the one with the widest graph is returned, with how many there were.
fn spawned_argv_in(
    tl: &kerf_core::Timeline,
    assets: &[kerf_core::Asset],
    opts: &ExportOptions,
    parts_off: bool,
) -> (Vec<String>, usize) {
    let dir = common::media_dir("d10b-spawn");
    let wrapper = dir.join("ffmpeg-wrapper.sh");
    let log = dir.join(format!("argv-{}", std::process::id()));
    for entry in std::fs::read_dir(&dir).expect("dir").flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(&format!("argv-{}", std::process::id()))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    common::write_script(
        &wrapper,
        "#!/bin/sh\nfound=\nfor a in \"$@\"; do case \"$a\" in -filter_complex|-filter_complex_script|-/filter_complex) found=1;; esac; done\n\
         if [ -n \"$found\" ]; then\n  n=0\n  while [ -e \"$D10B_LOG.$n\" ]; do n=$((n+1)); done\n  out=\"$D10B_LOG.$n\"\n  : > \"$out\"\n  prev=\n\
         for a in \"$@\"; do\n    printf '%s\\0' \"$a\" >> \"$out\"\n\
         case \"$prev\" in -filter_complex_script|-/filter_complex) cp \"$a\" \"$out.graph\";; esac\n    prev=\"$a\"\n  done\n  exit 1\nfi\n\
         exec \"$D10B_REAL\" \"$@\"\n",
    );
    let real = kerf_core::ffmpeg_path();
    std::env::set_var("D10B_REAL", &real);
    std::env::set_var("D10B_LOG", &log);
    std::env::set_var("KERF_FFMPEG", &wrapper);
    let out = dir.join("out.mkv");
    let saved = std::env::var_os("KERF_EXPORT_PARTS");
    if parts_off {
        std::env::set_var("KERF_EXPORT_PARTS", "off");
    }
    let _ = kerf_core::render_with(tl, assets, &out, opts);
    match saved {
        Some(v) => std::env::set_var("KERF_EXPORT_PARTS", v),
        None => std::env::remove_var("KERF_EXPORT_PARTS"),
    }
    std::env::set_var("KERF_FFMPEG", &real);
    let mut runs: Vec<Vec<String>> = Vec::new();
    for n in 0.. {
        let path = format!("{}.{n}", log.display());
        let Ok(bytes) = std::fs::read(&path) else { break };
        let mut args: Vec<String> = bytes
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if std::fs::metadata(format!("{path}.graph")).is_ok() {
            for i in 1..args.len() {
                if matches!(args[i - 1].as_str(), "-filter_complex_script" | "-/filter_complex") {
                    args[i] = format!("{path}.graph");
                }
            }
        }
        runs.push(args);
    }
    assert!(!runs.is_empty(), "the export spawned no ffmpeg with a graph");
    let count = runs.len();
    let widest = runs.into_iter().max_by_key(|a| graph_of(a).len()).expect("a run");
    (widest, count)
}

fn spawned_argv(tl: &kerf_core::Timeline, assets: &[kerf_core::Asset], opts: &ExportOptions) -> Vec<String> {
    spawned_argv_in(tl, assets, opts, true).0
}

/// The swscale contexts the engine counts in a graph (`cap_wide_graph`): the pieces named like the filters that name or force a pixel format.
fn contexts(graph: &str) -> usize {
    const NAMES: [&str; 11] = [
        "scale",
        "format",
        "overlay",
        "blend",
        "lutyuv",
        "colorchannelmixer",
        "alphamerge",
        "alphaextract",
        "geq",
        "fade",
        "pad",
    ];
    graph
        .split([';', ','])
        .filter(|piece| {
            let mut rest = piece.trim_start();
            while let Some(after) = rest.strip_prefix('[') {
                rest = after.split_once(']').map_or("", |(_, tail)| tail).trim_start();
            }
            NAMES.contains(&rest.split(['=', '[', ' ']).next().unwrap_or(""))
        })
        .count()
}

/// `-filter_complex_threads` of the argv, if it names one ahead of the first input.
fn argv_threads(args: &[String]) -> Option<u32> {
    let first_input = args.iter().position(|a| a == "-i").unwrap_or(args.len());
    let at = args[..first_input].iter().position(|a| a == "-filter_complex_threads")?;
    args.get(at + 1)?.parse().ok()
}

fn graph_of(args: &[String]) -> String {
    let at = args
        .iter()
        .position(|a| matches!(a.as_str(), "-filter_complex" | "-filter_complex_script" | "-/filter_complex"))
        .expect("a graph");
    if args[at] == "-filter_complex" {
        args[at + 1].clone()
    } else {
        std::fs::read_to_string(&args[at + 1]).expect("the graph's script")
    }
}

/// A cut of `n` two-frame clips of one noise picture, shrunk into a 64x36 Cover frame: a plain 10-bit chain per clip, every one of them dithered.
fn many_clips(n: usize) -> (kerf_core::Timeline, Vec<kerf_core::Asset>) {
    let a = media("wide-noise", "noise", (160, 90), "30", 3.0, "yuv420p", None);
    let clips: Vec<Clip> = (0..n)
        .map(|i| {
            let mut c = clip(&a, (i % 20) as f64 * 0.1, 2.0 / 30.0, i as f64 * 2.0 / 30.0);
            if i % 7 == 0 {
                c.fade_in = 1.0 / 30.0;
            }
            c
        })
        .collect();
    (timeline(vec![clips], vec![], (64, 36), Fit::Cover, 30.0), vec![a])
}

/// The plan draws the graph at the thread count the export really runs on, or refuses it: a wide graph (many clips) has its filter threads lowered by
/// the engine at spawn (`cap_wide_graph`), which is a count the plan has to know. On FFmpeg 6.1 the 4:2:0 dither depends on it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_the_plan_knows_the_threads_the_export_spawns_with() {
    d10b::measured();
    let kinds = kerf_gpu::delivery10::measured_kinds();
    eprintln!("measured kinds {kinds:?}");
    let mut total = Stats::default();
    for percent in [75u8, 100] {
        kerf_core::set_cpu_percent(percent);
        for n in [6usize, 80, 140, 200, 260, 400] {
            let (tl, assets) = many_clips(n);
            for layout in LAYOUTS {
                let opts = options(layout, 30.0);
                let args = spawned_argv(&tl, &assets, &opts);
                let graph = graph_of(&args);
                let spawned = argv_threads(&args);
                let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(d10b::policy())).expect("planner");
                let first = planner.at_frame(0).expect("plan");
                let caps = d10b::compositor(None).caps();
                let range = first.graph_thread_range(&caps);
                let rows = first.ten_bit_dither_rows(&caps, first.canvas.height);
                eprintln!(
                    "{percent}% {n:>4} clips {}: {} swscale contexts, spawned with -filter_complex_threads {spawned:?}, the plan judges the graph on {range:?} threads (dither rows {rows:?})",
                    layout.pix_fmt(),
                    contexts(&graph)
                );
                let frames = (n as u64) * 2;
                let s = run(&Run {
                    case: &format!("{n} clips at {percent}% as spawned"),
                    tl: &tl,
                    assets: &assets,
                    layout,
                    opts,
                    threads: None,
                    argv: Some(args),
                    extra: Vec::new(),
                    frames: vec![0..4, frames - 4..frames],
                    allow_refused: true,
                    exported: None,
                });
                d10b::report(&format!("{n} clips at {percent}% as spawned"), layout, spawned, &s);
                total.add(s);
            }
        }
    }
    kerf_core::set_cpu_percent(kerf_core::DEFAULT_CPU_PERCENT);
    assert!(
        total.failures.is_empty(),
        "{} differing samples:\n{}",
        total.differing,
        total.failures.join("\n")
    );
    assert_eq!(total.differing, 0);
}

/// Without the setting: a long clip on V1 with a few hundred plain clips above it, a cut the engine's planner cannot cut into parts (a part would decode
/// the long clip's first half to drop it): one run. Its graph is as wide as the cut is.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_a_long_clip_under_many_short_ones_is_one_wide_run() {
    d10b::measured();
    let long = media("long-testsrc", "testsrc2", (96, 54), "30", 44.0, "yuv420p", None);
    let short = media("wide-noise", "noise", (160, 90), "30", 3.0, "yuv420p", None);
    let base = clip(&long, 0.0, 40.0, 0.0);
    let n = 220usize;
    let tops: Vec<Clip> = (0..n)
        .map(|i| clip(&short, (i % 20) as f64 * 0.1, 2.0 / 30.0, 21.0 + i as f64 * 2.0 / 30.0))
        .collect();
    let tl = timeline(vec![vec![base], tops], vec![], (64, 36), Fit::Cover, 30.0);
    let assets = vec![long, short];
    let mut total = Stats::default();
    for layout in LAYOUTS {
        let opts = options(layout, 30.0);
        let (args, runs) = spawned_argv_in(&tl, &assets, &opts, false);
        let graph = graph_of(&args);
        let spawned = argv_threads(&args);
        eprintln!(
            "{}: {runs} run(s) spawned; the widest holds {} swscale contexts, -filter_complex_threads {spawned:?}",
            layout.pix_fmt(),
            contexts(&graph),
        );
        assert_eq!(runs, 1, "the cut was rendered in parts after all");
        let first = 21 * 30;
        let last = first + 2 * n as u64;
        let s = run(&Run {
            case: "a long clip under 220 short ones",
            tl: &tl,
            assets: &assets,
            layout,
            opts,
            threads: None,
            argv: Some(args),
            extra: Vec::new(),
            frames: vec![first..first + 6, last - 6..last],
            allow_refused: true,
            exported: None,
        });
        d10b::report("a long clip under 220 short ones", layout, spawned, &s);
        total.add(s);
    }
    assert!(
        total.failures.is_empty(),
        "{} differing samples:\n{}",
        total.differing,
        total.failures.join("\n")
    );
}

/// The product's own export of the wide cut with the lossless VP9 encoder (the engine's only lossless 10-bit one), rendered in parts where the engine
/// renders it so: held to the raw frames of the same argv. A first look at whether `real_export` is the graph's planes.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_a_lossless_vp9_export_is_the_graphs_planes() {
    d10b::measured();
    let (tl, assets) = many_clips(6);
    for layout in LAYOUTS {
        let real = d10b::real_export(&tl, &assets, layout, 30.0, &[]);
        let opts = options(layout, 30.0);
        let args = kerf_core::export_args(&tl, &assets, "OUT", &opts).expect("argv");
        let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(d10b::policy())).expect("planner");
        let first = planner.at_frame(0).expect("plan");
        let bytes = d10b::frame_samples(layout, (first.canvas.width as usize, first.canvas.height as usize)) * 2;
        let raw = d10b::run_frames(&d10b::raw_run(&args, Some(3), &[], 11), bytes, &|_| true);
        eprintln!("{layout:?}: real {} frames, raw {} frames", real.len(), raw.len());
        let apart = (0..12u64).filter(|k| real.get(k) != raw.get(k)).count();
        assert_eq!(apart, 0, "{layout:?}: frames of the vp9 file that are not the graph's");
    }
}

/// Every even height from 8 to 130 and slice thread counts from 2 to 16 (the engine writes any of them: the budget's, FFmpeg's default for the cores it
/// counts, a wide graph's cap): the compositor told the count against the export told the same. 4:2:2 has no dependence; it is run at two counts.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); minutes"]
fn checker_d10b_every_height_and_thread_count() {
    d10b::measured();
    let src = media("sweep-noise", "noise", (160, 90), "30", 2.0, "yuv420p", None);
    let assets = vec![src.clone()];
    let mut total = Stats::default();
    for h in (8u32..=130).step_by(2) {
        let w = ((h * 16 / 9) & !1).max(8);
        let mut c1 = clip(&src, 0.1, 4.0 / 30.0, 0.0);
        c1.fade_in = 0.1;
        let c2 = clip(&src, 0.4, 3.0 / 30.0, 0.2);
        let tl = timeline(vec![vec![c1, c2]], vec![], (w, h), Fit::Cover, 30.0);
        for layout in LAYOUTS {
            let counts: &[u32] = if layout == TenBitLayout::P420 {
                &[2, 3, 4, 5, 6, 7, 8, 12, 16]
            } else {
                &[1, 5]
            };
            for &n in counts {
                let s = run(&Run {
                    case: &format!("height {h} on {n} threads"),
                    tl: &tl,
                    assets: &assets,
                    layout,
                    opts: options(layout, 30.0),
                    threads: Some(n),
                    argv: None,
                    extra: Vec::new(),
                    frames: vec![0..9],
                    allow_refused: true,
                    exported: None,
                });
                if s.differing != 0 || s.drawn == 0 {
                    d10b::report(&format!("height {h}"), layout, Some(n), &s);
                }
                total.add(s);
            }
        }
    }
    eprintln!(
        "heights x threads: drawn {} refused {} ({:?}) compared {} differing {}",
        total.drawn, total.refused, total.kinds, total.compared, total.differing
    );
    assert!(
        total.failures.is_empty(),
        "{} differing samples:\n{}",
        total.differing,
        total.failures.join("\n")
    );
}

/// FFmpeg with no thread flag runs its graph on one more thread than the CPUs it counts (at most 16, one on one CPU): `-cpucount` stands in for a
/// machine with more or fewer cores than this one, and the plan's `graph_thread_range_for` is that arithmetic. The planes at `-cpucount c` are those at
/// the thread count the plan names.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_d10b_the_default_threads_are_one_more_than_the_cpus_ffmpeg_counts() {
    d10b::measured();
    let src = media("sweep-noise", "noise", (160, 90), "30", 2.0, "yuv420p", None);
    let assets = vec![src.clone()];
    let mut c1 = clip(&src, 0.1, 5.0 / 30.0, 0.0);
    c1.fade_in = 0.1;
    // A height the thread counts 2 to 16 cut differently (54 rows: 3 threads cut at 18 and flip; 2 at 27 -> 28 and do not...).
    for h in [54u32, 90, 100] {
        let tl = timeline(vec![vec![c1.clone()]], vec![], ((h * 16 / 9) & !1, h), Fit::Cover, 30.0);
        let opts = options(TenBitLayout::P420, 30.0);
        let args = kerf_core::export_args(&tl, &assets, "OUT", &opts).expect("argv");
        let planner = Planner::new(&tl, &assets, &opts, PlanRequest::motion(d10b::policy())).expect("planner");
        let first = planner.at_frame(0).expect("plan");
        let bytes = d10b::frame_samples(TenBitLayout::P420, (first.canvas.width as usize, h as usize)) * 2;
        let at =
            |extra: &[&str], threads: Option<u32>| d10b::run_frames(&d10b::raw_run(&args, threads, extra, 4), bytes, &|_| true);
        let mut distinct = std::collections::BTreeSet::new();
        for count in [1usize, 2, 3, 4, 5, 6, 7, 8, 12, 15, 16, 17, 31, 64] {
            let expected = if count > 1 { (count + 1).min(16) } else { 1 };
            let cpucount = count.to_string();
            let by_count = at(&["-cpucount", &cpucount], None);
            let by_threads = at(&[], Some(expected as u32));
            assert_eq!(
                by_count, by_threads,
                "height {h}: -cpucount {count} is not {expected} filter threads"
            );
            distinct.insert(
                by_threads
                    .values()
                    .flatten()
                    .copied()
                    .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(b))),
            );
            let rows = kerf_core::ten_bit_dither_rows(expected as u32, h);
            eprintln!("height {h}: -cpucount {count:>2} = {expected:>2} filter threads (the plan's dither rows {rows})");
        }
        if kerf_gpu::delivery10::measured_kinds().contains(kerf_core::TenBitKinds::PER_SLICE) {
            assert!(
                distinct.len() > 1,
                "height {h}: no thread count made a difference, nothing was tested"
            );
        } else {
            eprintln!(
                "height {h}: this FFmpeg's planes do not depend on the threads ({} distinct)",
                distinct.len()
            );
        }
    }
}

#[allow(dead_code)]
fn unused(_: PathBuf) {}
