//! `gblur` on small planes and across slice threads: the rule that says where FFmpeg's own kernel is not the recursion the model reproduces
//! (`kerf_core::gblur::plane_hazard`), held to FFmpeg plane by plane.
//!
//! FFmpeg's AVX2 and AVX-512 `gblur` (`x86/vf_gblur.asm`, the same in 6.1 and 9.0) is not a filter on every plane shape. Read off the
//! kernel and `vf_gblur.c`'s slicing, and held here against FFmpeg 6.1.1 and 9.0.2 on both kernels:
//!
//! * a plane one sample wide or tall: the horizontal kernel's loop over `width - 1` samples and the vertical one's loop over the rows
//!   below the first run once too many (a plane of width 1 corrupts the heap);
//! * more slice threads than rows: `filter_postscale` cuts the buffer into `min(width * height, threads)` slices of `height * job / jobs`
//!   rows, a slice of no rows still scales one whole vector from its start, and the first slice is one of them, so the first `lanes`
//!   samples are scaled twice (racily: the empty slice and the real one write the same samples at once);
//! * a slice of `lanes` rows or more over a plane `lanes` samples wide or fewer: the blocked horizontal kernel runs its block of columns
//!   once before it asks whether the row is longer than that.
//!
//! `lanes` is the kernel's vector width, 8 floats for AVX2 and 16 for AVX-512, found by experiment (the blur probe does the same).
//! **Soundness is the claim**: every plane the rule clears is exactly the model, on every shape, thread count and kernel tried, in a
//! single filter (`-filter_threads`), in a complex graph (`-filter_complex_threads`, what an export runs) and in a real export at the
//! budgets the app can set. A plane the rule refuses is, but for the postscale race, bad too (it is the set, not a margin).
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test blur_slices -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kerf_core::gblur::plane_hazard;
use kerf_gpu::effects::{blur_plane, gaussian};

const SIGMA: f64 = 0.5;

/// The budget is the process's: the two tests that move it do not overlap.
static BUDGET: Mutex<()> = Mutex::new(());

fn lcg(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as u8
        })
        .collect()
}

/// How long a child of the sweep may run before it is killed: nothing here waits on a process forever.
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

fn run(args: &[String], input: Vec<u8>) -> Option<Vec<u8>> {
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(args);
    kerf_core::run_piped_until(&mut cmd, input, Instant::now() + CHILD_TIMEOUT)
}

/// How the blur is asked for: a simple graph (`-vf`, `-filter_threads`) or a complex one (`-filter_complex`, `-filter_complex_threads`:
/// the export's, which writes the same cap for both).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Graph {
    Simple,
    Complex,
}

type Planes = [Vec<u8>; 3];

/// The three planes of a `w` x `h` `yuv420p` picture of noise, and the blur FFmpeg makes of them with `threads` slice threads (`None`:
/// FFmpeg's default) and `cpu` flags ahead of everything else.
fn blurred(w: usize, h: usize, threads: Option<usize>, graph: Graph, cpu: &[&str]) -> Option<(Planes, Planes)> {
    blurred_at(SIGMA, w, h, threads, graph, cpu)
}

/// [`blurred`] at `sigma`.
fn blurred_at(sigma: f64, w: usize, h: usize, threads: Option<usize>, graph: Graph, cpu: &[&str]) -> Option<(Planes, Planes)> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = [
        lcg(1 + (w * 131 + h) as u64, w * h),
        lcg(2 + (w * 137 + h) as u64, cw * ch),
        lcg(3 + (w * 139 + h) as u64, cw * ch),
    ];
    let raw: Vec<u8> = planes.iter().flatten().copied().collect();
    let mut args: Vec<String> = cpu.iter().map(|s| (*s).to_string()).collect();
    args.extend(["-hide_banner", "-loglevel", "error", "-nostdin"].map(String::from));
    if let Some(t) = threads {
        let t = t.to_string();
        args.extend(["-filter_threads".into(), t.clone(), "-filter_complex_threads".into(), t]);
    }
    args.extend(
        [
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "pipe:0",
        ]
        .map(String::from),
    );
    let filter = kerf_core::blur_filter(sigma);
    match graph {
        Graph::Simple => args.extend(["-vf".into(), filter]),
        Graph::Complex => args.extend([
            "-filter_complex".into(),
            format!("[0:v]{filter}[v]"),
            "-map".into(),
            "[v]".into(),
        ]),
    }
    args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"].map(String::from));
    let out = run(&args, raw)?;
    if out.len() != w * h + 2 * cw * ch {
        return None;
    }
    let (y, rest) = out.split_at(w * h);
    let (u, v) = rest.split_at(cw * ch);
    Some((planes, [y.to_vec(), u.to_vec(), v.to_vec()]))
}

fn plane_dims(w: usize, h: usize) -> [(usize, usize); 3] {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    [(w, h), (cw, ch), (cw, ch)]
}

/// Which planes of the picture are exactly the model's.
fn exact_planes(planes: &Planes, got: &Planes, w: usize, h: usize) -> [bool; 3] {
    let g = gaussian(SIGMA).expect("sigma");
    let mut exact = [false; 3];
    for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
        exact[k] = got[k] == blur_plane(&planes[k], pw, ph, &g);
    }
    exact
}

/// The kernel's vector width as an experiment finds it: 16 when a 12x16 luma plane (one thread) is not the model's, 8 when its 6x8 chroma
/// plane is, `None` when neither is (no AVX2 or AVX-512 kernel on this machine).
fn kernel_lanes(cpu: &[&str]) -> Option<u32> {
    let (planes, got) = blurred(12, 16, Some(1), Graph::Simple, cpu)?;
    let exact = exact_planes(&planes, &got, 12, 16);
    match (exact[0], exact[1] && exact[2]) {
        (false, true) => Some(16),
        (true, false) => Some(8),
        _ => None,
    }
}

/// A kernel this machine can be made to run: its name, the `-cpuflags` that select it, its vector width.
type Kernel = (&'static str, Vec<&'static str>, u32);

/// Every kernel this machine can be made to run: its own, and AVX2's where it has AVX-512 too (`-cpuflags -avx512`).
fn kernels() -> Vec<Kernel> {
    let mut out: Vec<Kernel> = Vec::new();
    let own: Vec<&str> = Vec::new();
    if let Some(lanes) = kernel_lanes(&own) {
        out.push(("its own", own, lanes));
    }
    let avx2: Vec<&str> = vec!["-cpuflags", "-avx512"];
    if let Some(8) = kernel_lanes(&avx2) {
        if out.iter().all(|(_, _, l)| *l != 8) {
            out.push(("AVX2 (-cpuflags -avx512)", avx2, 8));
        }
    }
    out
}

/// What a sweep found: planes the rule clears that differ from the model, planes it refuses that are exact, and how many of the latter by
/// hazard.
struct Swept {
    unsound: Vec<String>,
    loose: Vec<String>,
    loose_by_kind: BTreeMap<String, usize>,
}

fn sweep(shapes: &[(usize, usize, usize)], graph: Graph, cpu: &[&str], lanes: u32) -> Swept {
    type Found = ((usize, usize, usize), Option<[bool; 3]>);
    let results: Mutex<Vec<Found>> = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&(w, h, t)) = shapes.get(i) else { break };
                let r = blurred(w, h, Some(t), graph, cpu).map(|(planes, got)| exact_planes(&planes, &got, w, h));
                results.lock().unwrap().push(((w, h, t), r));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort();
    let mut swept = Swept {
        unsound: Vec::new(),
        loose: Vec::new(),
        loose_by_kind: BTreeMap::new(),
    };
    for ((w, h, t), exact) in results {
        for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
            let hazard = plane_hazard((pw as u32, ph as u32), t as u32, lanes);
            let name = ["Y", "U", "V"][k];
            let Some(exact) = exact.map(|e| e[k]) else {
                // FFmpeg would not run it (a plane one sample wide can crash it): nothing the rule clears may be one.
                if hazard.is_none() {
                    swept
                        .unsound
                        .push(format!("{graph:?} {w}x{h} T{t} plane {name} {pw}x{ph}: FFmpeg failed"));
                }
                continue;
            };
            match (hazard, exact) {
                (None, false) => swept.unsound.push(format!(
                    "{graph:?} {w}x{h} T{t} plane {name} {pw}x{ph}: cleared and not the model's"
                )),
                (Some(kind), true) => {
                    *swept.loose_by_kind.entry(format!("{kind:?}")).or_insert(0) += 1;
                    swept.loose.push(format!(
                        "{graph:?} {w}x{h} T{t} plane {name} {pw}x{ph}: {kind:?} refused, exact"
                    ));
                }
                _ => {}
            }
        }
    }
    swept
}

/// The shapes of the simple-graph sweep: 12 widths x 17 heights x 11 thread counts = 2,244 pictures per kernel, laid on the edges of the
/// rule (a width of 1, of 8 and 9 (AVX2's vector), of 15, 16 and 17 (AVX-512's); a height of 1, of 4 and 5 against the small thread
/// counts, of 8 and 9, 15, 16 and 17 against the vectors and 16 and 17 threads, of 33 and 64 against 32 and 64 threads, and 129 and 241
/// against 16 threads' slices of 9 and 16 rows).
fn shapes() -> Vec<(usize, usize, usize)> {
    let ws = [1usize, 2, 3, 4, 8, 9, 15, 16, 17, 18, 33, 40];
    let hs = [1usize, 2, 3, 4, 5, 8, 9, 15, 16, 17, 18, 24, 33, 48, 64, 129, 241];
    let mut out = Vec::new();
    for &t in &THREADS {
        for &h in &hs {
            for &w in &ws {
                out.push((w, h, t));
            }
        }
    }
    out
}

/// The slice-thread counts of the sweeps.
const THREADS: [usize; 11] = [1, 2, 3, 4, 5, 8, 9, 16, 17, 32, 64];

/// The rule against FFmpeg, shape by shape, thread count by thread count, on each kernel the machine has, in a simple graph and a complex
/// one: no plane the rule clears may differ from the model, and (the rule being the kernel's, not a margin) none the rule refuses may be
/// exact but for the postscale race.
#[test]
#[ignore = "needs ffmpeg"]
fn every_plane_the_rule_clears_is_the_model_at_every_thread_count_on_every_kernel() {
    let kernels = kernels();
    assert!(
        !kernels.is_empty(),
        "this machine's gblur has no AVX2 or AVX-512 kernel: the blur probe says Differs and nothing is drawn"
    );
    let all = shapes();
    // The complex graph (what an export runs) over a slice of the same shapes: 6 widths x 10 heights x 11 thread counts = 660 per kernel.
    let complex: Vec<(usize, usize, usize)> = all
        .iter()
        .copied()
        .filter(|&(w, h, _)| [1, 2, 9, 16, 17, 40].contains(&w) && [1, 2, 3, 5, 9, 16, 17, 33, 64, 241].contains(&h))
        .collect();
    let mut failures = Vec::new();
    for (name, cpu, lanes) in &kernels {
        for (graph, set) in [(Graph::Simple, &all), (Graph::Complex, &complex)] {
            let swept = sweep(set, graph, cpu, *lanes);
            eprintln!(
                "BLUR-SLICES {name} ({lanes} lanes) {graph:?}: {} shapes, {} planes cleared that differ, {} refused that are exact {:?}",
                set.len(),
                swept.unsound.len(),
                swept.loose.len(),
                swept.loose_by_kind
            );
            failures.extend(swept.unsound);
            for l in swept.loose.iter().filter(|l| !l.contains("EmptySlices")) {
                failures.push(format!("the rule is wider than the kernel: {l}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first {:#?}",
        failures.len(),
        &failures[..failures.len().min(12)]
    );
}

/// A blur of sigma 0 is the picture, and the rule lifts the one hazard that cannot change it (`gblur::plane_hazard_for`): with more
/// slice threads than rows the empty postscale slice scales a vector by 1 and clips it again. Every plane the lifted rule clears is the
/// input to the sample, on each kernel, in a simple and a complex graph, two runs each (the slice is racy); the refused rest is only
/// reported (a plane one sample across corrupts the heap, a vector of rows over a narrow plane reads past the row: those are heap
/// luck, and stay refused).
#[test]
#[ignore = "needs ffmpeg"]
fn a_blur_of_sigma_0_is_the_picture_on_every_plane_the_lifted_rule_clears() {
    const RUNS: usize = 2;
    let kernels = kernels();
    assert!(!kernels.is_empty(), "no AVX2 or AVX-512 kernel on this machine");
    let shapes: Vec<(usize, usize, usize)> = shapes()
        .into_iter()
        .filter(|&(w, h, t)| [2, 3, 9, 16, 17, 40].contains(&w) && h >= 2 && (t == 1 || t > h.div_ceil(2)))
        .collect();
    let mut failures = Vec::new();
    for (name, cpu, lanes) in &kernels {
        for graph in [Graph::Simple, Graph::Complex] {
            let set: Vec<_> = shapes
                .iter()
                .copied()
                .filter(|&(w, h, _)| {
                    graph == Graph::Simple || ([2, 9, 16, 17, 40].contains(&w) && [2, 3, 5, 9, 16, 33].contains(&h))
                })
                .collect();
            let found: Mutex<Vec<String>> = Mutex::new(Vec::new());
            let (cleared, lifted, refused_exact, refused) = (
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            );
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..3 {
                    s.spawn(|| loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&(w, h, t)) = set.get(i) else { break };
                        for run in 0..RUNS {
                            let Some((planes, got)) = blurred_at(0.0, w, h, Some(t), graph, cpu) else {
                                found.lock().unwrap().push(format!("{graph:?} {w}x{h} T{t}: FFmpeg failed"));
                                continue;
                            };
                            for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
                                let dims = (pw as u32, ph as u32);
                                let exact = got[k] == planes[k];
                                match kerf_core::gblur::plane_hazard_for(dims, t as u32, *lanes, true) {
                                    None => {
                                        cleared.fetch_add(1, Ordering::SeqCst);
                                        if plane_hazard(dims, t as u32, *lanes).is_some() {
                                            lifted.fetch_add(1, Ordering::SeqCst);
                                        }
                                        if !exact {
                                            found.lock().unwrap().push(format!(
                                                "{graph:?} {w}x{h} T{t} run {run} plane {} {pw}x{ph}: cleared and not the picture",
                                                ["Y", "U", "V"][k]
                                            ));
                                        }
                                    }
                                    Some(_) => {
                                        refused.fetch_add(1, Ordering::SeqCst);
                                        refused_exact.fetch_add(usize::from(exact), Ordering::SeqCst);
                                    }
                                }
                            }
                        }
                    });
                }
            });
            eprintln!(
                "BLUR-SLICES sigma 0 {name} ({lanes} lanes) {graph:?}: {} shapes x {RUNS} runs, {} planes cleared ({} only by the lift), {} refused ({} of them exact)",
                set.len(),
                cleared.load(Ordering::SeqCst),
                lifted.load(Ordering::SeqCst),
                refused.load(Ordering::SeqCst),
                refused_exact.load(Ordering::SeqCst)
            );
            assert!(lifted.load(Ordering::SeqCst) > 0, "the sweep never reached the lifted hazard");
            failures.extend(found.into_inner().unwrap());
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first {:#?}",
        failures.len(),
        &failures[..failures.len().min(12)]
    );
}

/// The slice threads FFmpeg gives a graph with no flag at all are inside the range the plan judges over (`cpu_graph_thread_range`): the
/// largest luma height that is bad for lack of rows, plus one, is the thread count (a 40-wide plane has no other hazard).
#[test]
#[ignore = "needs ffmpeg"]
fn the_default_slice_threads_of_a_graph_are_inside_the_range_the_plan_judges() {
    // (FFmpeg's own default is what a graph runs on when the budget is the whole machine; below it the engine writes the budget's
    // threads, which the export test holds.)
    let _budget = BUDGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    kerf_core::set_cpu_percent(100);
    let (lo, hi) = kerf_core::cpu_graph_thread_range();
    kerf_core::set_cpu_percent(was);
    for graph in [Graph::Simple, Graph::Complex] {
        let mut worst_bad = 0;
        for h in 2..=24usize {
            // (The race can hide a bad plane in one run of three.)
            let bad = (0..3).any(|_| {
                let (planes, got) = blurred(40, h, None, graph, &[]).expect("ffmpeg");
                !exact_planes(&planes, &got, 40, h)[0]
            });
            if bad {
                worst_bad = h;
            }
        }
        let threads = worst_bad + 1;
        eprintln!("BLUR-SLICES default threads {graph:?}: {threads} (the plan judges {lo}..={hi})");
        assert!(
            lo <= threads && threads <= hi,
            "{graph:?}: FFmpeg's graph ran on {threads} threads, the plan judges {lo}..={hi}"
        );
    }
}

fn noise_clip(dir: &Path, w: usize, h: usize) -> kerf_core::Asset {
    let path = dir.join(format!("blur-slices-{w}x{h}.mkv"));
    if !path.exists() {
        let src = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=0.2,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
        );
        let args: Vec<String> = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain([path.to_string_lossy().into_owned()])
        .collect();
        run(&args, Vec::new()).expect("FFmpeg makes the clip");
    }
    kerf_core::Project::probe_asset(&path).expect("probe")
}

/// A real export, the engine's own argv at the budget the app set: a plane the rule clears at every thread count of the machine's range
/// at that budget is the model's in the exported frame, and the plan (judging the same range) refuses the picture whenever a plane of it
/// is not.
#[test]
#[ignore = "needs ffmpeg"]
fn an_exports_own_argv_at_every_budget_is_the_model_on_every_plane_the_plan_clears() {
    let _budget = BUDGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    use kerf_core::{
        Clip, Container, EffectKinds, ExportOptions, GpuCaps, RateControl, RenderPlan, StreamKind, Timeline, Track, Unsupported,
        VideoEffect,
    };
    let kernels = kernels();
    let (_, cpu, lanes) = kernels.first().expect("a kernel").clone();
    assert!(cpu.is_empty(), "the export is run on this machine's own kernel");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("blur-slices");
    std::fs::create_dir_all(&dir).unwrap();
    let was = kerf_core::cpu_percent();
    let g = gaussian(SIGMA).unwrap();
    let mut failures = Vec::new();
    let (mut cleared, mut refused) = (0, 0);
    let sizes = [
        (64usize, 4usize),
        (64, 6),
        (64, 8),
        (64, 10),
        (64, 16),
        (64, 20),
        (64, 24),
        (64, 34),
        (16, 16),
        (16, 40),
        (12, 34),
        (8, 16),
    ];
    for percent in [25u8, 50, 75, 100] {
        kerf_core::set_cpu_percent(percent);
        let (lo, hi) = kerf_core::cpu_graph_thread_range();
        for (w, h) in sizes {
            let asset = noise_clip(&dir, w, h);
            let mut clip = Clip::new(asset.id, 0.0, 0.2, 0.0);
            clip.effects = vec![VideoEffect::Blur { sigma: SIGMA }];
            let tl = Timeline {
                tracks: vec![Track {
                    clips: vec![clip],
                    ..Track::new(StreamKind::Video, "V1")
                }],
                ..Timeline::new()
            };
            let assets = [asset.clone()];
            let opts = ExportOptions {
                container: Container::Mkv,
                video_codec: Some("libx264".into()),
                rate_control: RateControl::Lossless,
                fps: Some(30.0),
                ..ExportOptions::default()
            };
            let file = dir.join(format!("export-{percent}-{w}x{h}.mkv"));
            kerf_core::render_with(&tl, &assets, &file, &opts).expect("export");
            let decode = |path: &Path| {
                let out = run(
                    &[
                        "-hide_banner",
                        "-loglevel",
                        "error",
                        "-nostdin",
                        "-i",
                        path.to_str().unwrap(),
                        "-frames:v",
                        "1",
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "yuv420p",
                        "pipe:1",
                    ]
                    .map(String::from),
                    Vec::new(),
                )
                .expect("decode");
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                assert_eq!(out.len(), w * h + 2 * cw * ch);
                let (y, rest) = out.split_at(w * h);
                let (u, v) = rest.split_at(cw * ch);
                [y.to_vec(), u.to_vec(), v.to_vec()]
            };
            let source = decode(Path::new(&asset.path));
            let exported = decode(&file);
            let _ = std::fs::remove_file(&file);
            let mut caps = GpuCaps::A0;
            caps.effects = EffectKinds::BLUR;
            caps.blur_lanes = lanes as u8;
            let plan = RenderPlan::at(&tl, &assets, &opts, 0.1, kerf_core::composite_color_policy()).expect("plan");
            let plan_refuses = plan
                .reasons(&caps, plan.size(u32::MAX))
                .iter()
                .any(|u| matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane")));
            let mut any_bad = false;
            for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
                let exact = exported[k] == blur_plane(&source[k], pw, ph, &g);
                any_bad |= !exact;
                let clear = (lo..=hi).all(|t| plane_hazard((pw as u32, ph as u32), t as u32, lanes).is_none());
                if clear && !exact {
                    failures.push(format!(
                        "budget {percent}% ({lo}..={hi} threads) {w}x{h} plane {} {pw}x{ph}: cleared and not the model's",
                        ["Y", "U", "V"][k]
                    ));
                }
            }
            if any_bad && !plan_refuses {
                failures.push(format!(
                    "budget {percent}% ({lo}..={hi} threads) {w}x{h}: the export is not the model's and the plan draws it"
                ));
            }
            if plan_refuses {
                refused += 1;
            } else {
                cleared += 1;
            }
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!("BLUR-SLICES exports: {cleared} pictures cleared by the plan, {refused} refused");
    assert!(cleared > 0 && refused > 0, "the sweep should meet both");
    assert!(failures.is_empty(), "{failures:#?}");
}
