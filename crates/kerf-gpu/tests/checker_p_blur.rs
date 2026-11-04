//! `checker/p/blur`: an independent check of the rule that says where FFmpeg's `gblur` is the recursion the compositor draws
//! (`kerf_core::gblur::plane_hazard`, `picture_hazard`, `RenderPlan::graph_thread_range`), written from `vf_gblur.c` and
//! `x86/vf_gblur.asm` and not from the implementer's sweep.
//!
//! * `checker_p_blur_sweep_*` (FFmpeg only): a model of the filter written here (`mul_add` for every step, the coefficients from
//!   `set_params` ported again), every plane of a few thousand pictures, thread counts 1 to 64, a simple and a complex graph, the
//!   boundary heights of each rule (a plane's rows against the threads, a slice's rows against 8 and 16), every sigma from 0.05 to 1024,
//!   on the machine's own kernel and on AVX2 (`-cpuflags -avx512`). **Soundness:** no plane the rule clears may differ.
//! * `checker_p_blur_sigma_zero_*`: `gblur` with sigma 0 is the identity on every plane but a one-sample-wide luma one (which crashes
//!   the filter), so a refusal on any other plane is wider than the kernel.
//! * `checker_p_blur_probe_*`: the blur probe's answer holds on this FFmpeg, and the explicit thread flags of its runs win over the
//!   budget's. (Run it under a wrapper that adds `-cpuflags ...` as `KERF_FFMPEG` to hold the probe to another kernel.)
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p_blur -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![allow(clippy::print_stderr)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kerf_core::gblur::{picture_hazard, plane_hazard, KernelHazard};

// ---- FFmpeg ---------------------------------------------------------------------------------

/// One FFmpeg run over `input`, killed after a minute: its stdout, `None` when it did not exit cleanly (a plane one sample wide can
/// crash `gblur`).
fn run(args: &[String], input: Vec<u8>) -> Option<Vec<u8>> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let _ = writer.join();
    let out = reader.join().ok()?;
    status.filter(std::process::ExitStatus::success).map(|_| out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Graph {
    Simple,
    Complex,
}

type Planes = [Vec<u8>; 3];

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5851_f42d_4c95_7f2d;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

/// The planes of a `w` x `h` `yuv420p` picture of noise, and the same through `gblur=sigma=<sigma>` on `threads` slice threads
/// (`None`: FFmpeg's own number), with `cpu` ahead of everything: `None` if FFmpeg would not run it.
fn blurred(
    w: usize,
    h: usize,
    sigma: &str,
    threads: Option<usize>,
    graph: Graph,
    cpu: &[&str],
    seed: u64,
) -> Option<(Planes, Planes)> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = [
        noise(seed * 3 + (w * 131 + h) as u64, w * h),
        noise(seed * 5 + (w * 137 + h) as u64, cw * ch),
        noise(seed * 7 + (w * 139 + h) as u64, cw * ch),
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
    let filter = format!("gblur=sigma={sigma}");
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

// ---- the model, from vf_gblur.c ------------------------------------------------------------------------

/// `set_params` for one step: `(nu, boundaryscale, postscale)` as `float`s, a value that is not a normal number replaced as the filter does.
fn set_params(sigma: f32) -> (f32, f32, f32) {
    let lambda = f64::from(sigma * sigma) / 2.0;
    let dnu = (1.0 + 2.0 * lambda - (1.0 + 4.0 * lambda).sqrt()) / (2.0 * lambda);
    let mut post = (dnu / lambda).powf(1.0) as f32;
    let mut bscale = (1.0 / (1.0 - dnu)) as f32;
    let mut nu = dnu as f32;
    if !post.is_normal() {
        post = 1.0;
    }
    if !bscale.is_normal() {
        bscale = 1.0;
    }
    if !nu.is_normal() {
        nu = 0.0;
    }
    (nu, bscale, post)
}

/// `gblur` on one 8-bit plane as the AVX2 / AVX-512 kernels run it (`vfmadd231ps` for every step of the recursion).
fn model(src: &[u8], w: usize, h: usize, sigma: f32) -> Vec<u8> {
    let (nu, bscale, post) = set_params(sigma);
    let post = post * post;
    let mut b: Vec<f32> = src.iter().map(|&v| f32::from(v)).collect();
    for row in b.chunks_exact_mut(w) {
        row[0] *= bscale;
        for x in 1..w {
            row[x] = row[x - 1].mul_add(nu, row[x]);
        }
        row[w - 1] *= bscale;
        for x in (1..w).rev() {
            row[x - 1] = row[x].mul_add(nu, row[x - 1]);
        }
    }
    for x in 0..w {
        b[x] *= bscale;
        for y in 1..h {
            b[y * w + x] = b[(y - 1) * w + x].mul_add(nu, b[y * w + x]);
        }
        b[(h - 1) * w + x] *= bscale;
        for y in (1..h).rev() {
            b[(y - 1) * w + x] = b[y * w + x].mul_add(nu, b[(y - 1) * w + x]);
        }
    }
    b.iter()
        .map(|&v| (v * post).clamp(0.0, 255.0).round_ties_even() as u8)
        .collect()
}

/// How a plane differs from what was wanted: samples, the largest difference, the first `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gap {
    n: usize,
    max: i32,
    first: (usize, usize),
}

fn gap(got: &[u8], want: &[u8], w: usize) -> Option<Gap> {
    let mut g: Option<Gap> = None;
    for (i, (a, b)) in got.iter().zip(want).enumerate() {
        let d = (i32::from(*a) - i32::from(*b)).abs();
        if d != 0 {
            let g = g.get_or_insert(Gap {
                n: 0,
                max: 0,
                first: (i % w, i / w),
            });
            g.n += 1;
            g.max = g.max.max(d);
        }
    }
    g
}

// ---- the kernels this machine can be made to run --------------------------------------------------

/// A kernel: its name, the `-cpuflags` that select it and its vector width.
type Kernel = (&'static str, Vec<&'static str>, u32);

/// The width of the kernel a `cpu` selection runs, found by experiment: a 24x32 picture at one thread has a 12x16 chroma plane, a
/// hazard for the 16-lane kernel and none for the 8-lane one; 64x64 tells a kernel that is not the model's at all. `None` for such a
/// kernel (an SSE4-only or a C one: the blur probe says `Differs` and nothing is drawn).
fn lanes_of(cpu: &[&str]) -> Option<u32> {
    let exact = |w: usize, h: usize| -> Option<[bool; 3]> {
        let (planes, got) = blurred(w, h, "3", Some(1), Graph::Simple, cpu, 1)?;
        let mut e = [false; 3];
        for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
            e[k] = got[k] == model(&planes[k], pw, ph, 3.0);
        }
        Some(e)
    };
    if exact(64, 64)? != [true; 3] {
        return None;
    }
    let e = exact(24, 32)?;
    match (e[0], e[1] && e[2]) {
        (true, false) => Some(16),
        (true, true) => Some(8),
        _ => None,
    }
}

fn kernels() -> Vec<Kernel> {
    let mut out: Vec<Kernel> = Vec::new();
    if let Some(l) = lanes_of(&[]) {
        out.push(("own", vec![], l));
        if l == 16 && lanes_of(&["-cpuflags", "-avx512"]) == Some(8) {
            out.push(("avx2 (-cpuflags -avx512)", vec!["-cpuflags", "-avx512"], 8));
        }
    }
    out
}

// ---- the shapes ------------------------------------------------------------------------------------

/// Plane heights at which a rule flips for `t` threads: the threads themselves (a plane with fewer rows has an empty postscale slice),
/// and a slice of 8 and of 16 rows (`ceil(h / min(h, t)) >= lanes`).
fn flip_planes(t: i64) -> BTreeSet<i64> {
    let mut p = BTreeSet::new();
    for d in -1..=1 {
        p.insert(t + d);
    }
    for l in [8, 16] {
        for d in -2..=2 {
            p.insert((l - 1) * t + d);
            p.insert(l * t + d);
        }
    }
    p
}

/// Picture heights whose luma or chroma plane is at a flip for `t` threads.
fn heights(t: usize) -> Vec<usize> {
    let mut hs = BTreeSet::new();
    for p in flip_planes(t as i64) {
        if p >= 2 {
            for h in [p - 1, p, p + 1, 2 * p - 1, 2 * p] {
                if (2..=2200).contains(&h) {
                    hs.insert(h as usize);
                }
            }
        }
    }
    hs.into_iter().collect()
}

const WIDTHS: [usize; 31] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 15, 16, 17, 18, 19, 24, 30, 31, 32, 33, 34, 35, 40, 47, 48, 49, 64, 65, 100, 129,
];
const THREADS: [usize; 31] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 24, 31, 32, 33, 40, 48, 63, 64, 100, 128, 256,
];

/// A reproducible pick of `n` of the `(width, height, threads)` triples the rules flip at.
fn pick(n: usize, seed: u64) -> Vec<(usize, usize, usize)> {
    let mut all = Vec::new();
    for &t in &THREADS {
        for &h in &heights(t) {
            for &w in &WIDTHS {
                all.push((w, h, t));
            }
        }
    }
    let mut s = seed | 1;
    let mut out = BTreeSet::new();
    while out.len() < n.min(all.len()) {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.insert(all[(s % all.len() as u64) as usize]);
    }
    out.into_iter().collect()
}

fn samples() -> usize {
    std::env::var("KERF_CHECKER_P_SAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3500)
}

const SIGMAS: [&str; 5] = ["0.05", "0.5", "3", "40", "1024"];

/// What a sweep found.
#[derive(Default)]
struct Swept {
    unsound: Vec<String>,
    loose: Vec<String>,
    by_kind: BTreeMap<String, (usize, usize)>,
    cleared: usize,
    runs: usize,
}

/// Run each sample on `workers` threads: the cleared planes (`plane_hazard` at `lanes`) against the model.
fn sweep(cases: &[(usize, usize, usize, usize)], graph_of: impl Fn(usize) -> Graph + Sync, cpu: &[&str], lanes: u32) -> Swept {
    type Found = ((usize, usize, usize, usize), Option<Vec<Option<Gap>>>);
    let results: Mutex<Vec<Found>> = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&(w, h, t, si)) = cases.get(i) else { break };
                let sigma = SIGMAS[si % SIGMAS.len()];
                let r = blurred(w, h, sigma, Some(t), graph_of(i), cpu, 1 + i as u64).map(|(planes, got)| {
                    plane_dims(w, h)
                        .into_iter()
                        .enumerate()
                        .map(|(k, (pw, ph))| gap(&got[k], &model(&planes[k], pw, ph, sigma.parse().unwrap()), pw))
                        .collect()
                });
                results.lock().unwrap().push(((w, h, t, si), r));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|r| r.0);
    let mut swept = Swept::default();
    for ((w, h, t, si), got) in results {
        let sigma = SIGMAS[si % SIGMAS.len()];
        swept.runs += 1;
        for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
            let hazard = plane_hazard((pw as u32, ph as u32), t as u32, lanes);
            let name = ["Y", "U", "V"][k];
            let found = got.as_ref().map(|g| g[k]);
            match (hazard, found) {
                (None, None) => swept.unsound.push(format!(
                    "{w}x{h} T{t} sigma {sigma} plane {name} {pw}x{ph}: cleared, and FFmpeg did not run"
                )),
                (None, Some(Some(g))) => swept.unsound.push(format!(
                    "{w}x{h} T{t} sigma {sigma} plane {name} {pw}x{ph}: cleared and differs: {} samples, max {}, first {:?}",
                    g.n, g.max, g.first
                )),
                (None, Some(None)) => swept.cleared += 1,
                (Some(kind), Some(None)) => {
                    swept.by_kind.entry(format!("{kind:?}")).or_default().0 += 1;
                    swept.loose.push(format!(
                        "{w}x{h} T{t} sigma {sigma} plane {name} {pw}x{ph}: {kind:?} refused and exact"
                    ));
                }
                (Some(kind), _) => swept.by_kind.entry(format!("{kind:?}")).or_default().1 += 1,
            }
        }
    }
    swept
}

fn cases_of(n: usize, seed: u64) -> Vec<(usize, usize, usize, usize)> {
    pick(n, seed)
        .into_iter()
        .enumerate()
        .map(|(i, (w, h, t))| (w, h, t, i % SIGMAS.len()))
        .collect()
}

fn report(what: &str, name: &str, lanes: u32, swept: &Swept) {
    eprintln!(
        "CHECKER-P {what} {name} ({lanes} lanes): {} runs, {} planes cleared and exact, {} cleared and differing, refused (exact, differing) by kind {:?}",
        swept.runs,
        swept.cleared,
        swept.unsound.len(),
        swept.by_kind
    );
    for l in swept.loose.iter().filter(|l| !l.contains("EmptySlices")).take(12) {
        eprintln!("CHECKER-P   loose: {l}");
    }
}

/// **Soundness**, simple graph: no plane the rule clears differs from the model, at any thread count, sigma or kernel.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_blur_sweep_no_cleared_plane_differs_in_a_simple_graph() {
    let kernels = kernels();
    assert!(!kernels.is_empty(), "this FFmpeg's gblur is not the model's on a plain shape");
    let mut failures = Vec::new();
    for (name, cpu, lanes) in &kernels {
        let swept = sweep(&cases_of(samples(), 0x1357), |_| Graph::Simple, cpu, *lanes);
        report("simple", name, *lanes, &swept);
        failures.extend(swept.unsound.into_iter().map(|f| format!("{name}: {f}")));
    }
    assert!(
        failures.is_empty(),
        "{} unsound, first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(15)]
    );
}

/// **Soundness**, complex graph (what an export runs): the same, with `-filter_complex` and `-filter_complex_threads`.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_blur_sweep_no_cleared_plane_differs_in_a_complex_graph() {
    let kernels = kernels();
    assert!(!kernels.is_empty());
    let mut failures = Vec::new();
    for (name, cpu, lanes) in &kernels {
        let swept = sweep(&cases_of(samples() / 2, 0x2468), |_| Graph::Complex, cpu, *lanes);
        report("complex", name, *lanes, &swept);
        failures.extend(swept.unsound.into_iter().map(|f| format!("{name}: {f}")));
    }
    assert!(
        failures.is_empty(),
        "{} unsound, first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(15)]
    );
}

/// The picture-level judgement is the plane-level one over luma and rounded-up half-size chroma: for every picture and thread count the
/// picture is cleared exactly when every plane is, on both widths and with the width not known.
#[test]
fn checker_p_blur_picture_hazard_is_the_planes_hazards() {
    for w in (1..=70u32).step_by(3).chain([127, 128, 129, 640, 641]) {
        for h in (1..=70u32).step_by(3).chain([127, 128, 129, 360, 361]) {
            for t in [1u32, 2, 3, 5, 8, 15, 16, 17, 64] {
                for lanes in [0u32, 8, 16] {
                    let planes = [(w, h), (w.div_ceil(2), h.div_ceil(2))];
                    let widths: &[u32] = match lanes {
                        0 => &[8, 16],
                        8 => &[8],
                        _ => &[16],
                    };
                    let any = planes
                        .iter()
                        .any(|&p| widths.iter().any(|&l| plane_hazard(p, t, l).is_some()));
                    assert_eq!(
                        picture_hazard((w, h), t..=t, lanes).is_some(),
                        any,
                        "{w}x{h} T{t} lanes {lanes}"
                    );
                }
            }
        }
    }
}

// ---- sigma 0 ---------------------------------------------------------------------------------------

/// A picture's run at sigma 0: its `(width, height, threads)` and, per plane, how it differs from its input (`None`: FFmpeg did not run).
type Identity = ((usize, usize, usize), Option<Vec<Option<Gap>>>);

/// `gblur` with sigma 0 has `nu` 0 and both scales 1 (`set_params`: `dnu` is NaN, so `postscale` and `boundaryscale` read as not normal and
/// become 1, `nu` 0). Where the kernel runs the recursion it is the identity; on a plane the rule refuses it is
///
/// * `EmptySlices`: the identity, and provably: the empty slice scales a vector a second time by `postscale * postscaleV` = 1 and clamps
///   to the range again, both idempotent;
/// * `NarrowBlocks`: the identity here and not provably: the blocked loop reads and writes past the row's end, and the leftward pass
///   carries what it read there into the row (`p[x - 1] = fma(p[x], 0, p[x - 1])` is `p[x - 1]` unless `p[x]` is not finite), so it holds
///   for what is past the row (the next row's samples, or the buffer's unwritten tail) being finite;
/// * `OneSample`: a luma plane one sample wide crashes or corrupts FFmpeg's heap; a plane one sample tall reads the identity here.
///
/// This holds the soundness (a plane the rule clears is the identity) and prints, by kind, how many refused planes are the identity
/// anyway: the refusals that are wider than the kernel for sigma 0 (the finding to read off the run; nothing here fails on it).
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_blur_sigma_zero_is_the_identity_where_the_rule_clears_it_and_mostly_where_it_does_not() {
    let mut refused: BTreeMap<String, (usize, usize, Vec<String>)> = BTreeMap::new();
    let mut unsound = Vec::new();
    for (name, cpu, lanes) in kernels() {
        let cases: Vec<(usize, usize, usize)> = pick(1500, 0x777)
            .into_iter()
            .chain([
                (12, 16, 1),
                (24, 32, 1),
                (40, 3, 16),
                (64, 4, 8),
                (2, 2, 3),
                (3, 7, 8),
                (40, 1, 4),
                (2, 40, 2),
                (16, 16, 1),
                (8, 8, 1),
                (1, 40, 1),
                (40, 1, 1),
                (2, 5, 1),
                (3, 3, 4),
            ])
            .collect();
        let next = AtomicUsize::new(0);
        let found: Mutex<Vec<Identity>> = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..3 {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(&(w, h, t)) = cases.get(i) else { break };
                    let r = blurred(w, h, "0", Some(t), Graph::Complex, &cpu, 9 + i as u64).map(|(planes, got)| {
                        plane_dims(w, h)
                            .into_iter()
                            .enumerate()
                            .map(|(k, (pw, _))| gap(&got[k], &planes[k], pw))
                            .collect()
                    });
                    found.lock().unwrap().push(((w, h, t), r));
                });
            }
        });
        let mut found = found.into_inner().unwrap();
        found.sort_by_key(|f| f.0);
        let mut identity = 0usize;
        for ((w, h, t), r) in found {
            for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
                let hazard = plane_hazard((pw as u32, ph as u32), t as u32, lanes);
                let ident = r.as_ref().map(|g| g[k].is_none());
                match (hazard, ident) {
                    (None, Some(true)) => identity += 1,
                    (None, other) => unsound.push(format!(
                        "{name} {w}x{h} T{t} plane {k}: cleared, sigma 0 is not the identity: {other:?}"
                    )),
                    (Some(kind), Some(is_identity)) => {
                        let what = if kind == KernelHazard::OneSample {
                            format!("OneSample {}", if pw == 1 { "wide" } else { "tall" })
                        } else {
                            format!("{kind:?}")
                        };
                        let entry = refused.entry(what).or_default();
                        if is_identity {
                            entry.0 += 1;
                            if entry.2.len() < 3 {
                                entry
                                    .2
                                    .push(format!("{name} {w}x{h} T{t} plane {} {pw}x{ph}", ["Y", "U", "V"][k]));
                            }
                        } else {
                            entry.1 += 1;
                        }
                    }
                    (Some(_), None) => {}
                }
            }
        }
        eprintln!("CHECKER-P sigma0 {name}: {identity} cleared planes are the identity");
    }
    for (kind, (ident, not, e_g)) in &refused {
        eprintln!("CHECKER-P sigma0: {kind}: refused planes that are the identity {ident}, that are not {not}, e.g. {e_g:?}");
    }
    assert!(unsound.is_empty(), "{unsound:#?}");
    // (EmptySlices is the identity wherever FFmpeg ran: the argument above.)
    if let Some((_, not, _)) = refused.get("EmptySlices") {
        assert_eq!(*not, 0, "sigma 0 scaled twice by 1 is not the identity?");
    }
}

/// Every picture from 1x1 to 34x34 at nine slice-thread counts (and so every plane of 1 to 17 across, chroma rounding included), on each
/// kernel: no plane the rule clears may differ from the model; the refused ones are counted by kind (the exact ones are luck, not proof:
/// `NarrowBlocks` reads past the row, and the race of `EmptySlices` hides in a run of one).
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_blur_every_tiny_picture_the_rule_clears_is_the_model() {
    let ts = [1usize, 2, 3, 5, 8, 9, 16, 17, 33];
    let mut cases = Vec::new();
    for w in 1..=34usize {
        for h in 1..=34usize {
            for &t in &ts {
                cases.push((w, h, t, (w + h + t) % SIGMAS.len()));
            }
        }
    }
    let mut failures = Vec::new();
    for (name, cpu, lanes) in kernels() {
        let swept = sweep(&cases, |_| Graph::Simple, &cpu, lanes);
        report("tiny", name, lanes, &swept);
        failures.extend(swept.unsound.into_iter().map(|f| format!("{name}: {f}")));
    }
    assert!(
        failures.is_empty(),
        "{} unsound, first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(15)]
    );
}

// ---- thread flags and the probe -----------------------------------------------------------------------

/// A later `-filter_threads` wins over an earlier one (the probe's runs carry the budget's flags first and their own after):
/// 40x6 has chroma of 3 rows, which five threads leave an empty postscale slice on and one does not.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_blur_the_last_thread_flags_win() {
    let one = blurred(40, 6, "3", Some(1), Graph::Simple, &[], 1).expect("ffmpeg");
    let five = blurred(40, 6, "3", Some(5), Graph::Simple, &[], 1).expect("ffmpeg");
    assert_ne!(
        one.1, five.1,
        "one and five threads give the same picture: the shape no longer tells them apart"
    );
    let (cw, ch) = (20usize, 3usize);
    let raw: Vec<u8> = one.0.iter().flatten().copied().collect();
    for (first, last, want) in [(1, 5, &five.1), (5, 1, &one.1)] {
        let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-filter_threads"]
            .map(String::from)
            .to_vec();
        args.extend([
            first.to_string(),
            "-filter_complex_threads".into(),
            first.to_string(),
            "-threads".into(),
            "3".into(),
            "-filter_threads".into(),
            last.to_string(),
            "-filter_complex_threads".into(),
            last.to_string(),
        ]);
        args.extend(
            [
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                "40x6",
                "-i",
                "pipe:0",
                "-vf",
                "gblur=sigma=3",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ]
            .map(String::from),
        );
        let out = run(&args, raw.clone()).expect("ffmpeg");
        let flat: Vec<u8> = want.iter().flatten().copied().collect();
        assert_eq!(out.len(), 40 * 6 + 2 * cw * ch);
        assert_eq!(out, flat, "-filter_threads {first} then {last}: the last did not win");
    }
}

/// The blur probe on this FFmpeg: `Match` with the kernel's width or not at all, and then every plane its width clears is the model's on
/// a sweep of its own. Run under `KERF_FFMPEG=<wrapper adding -cpuflags ...>` for another kernel; the printed line is the finding.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_blur_probe_a_match_is_a_kernel_the_rule_is_sound_for() {
    let verdict = kerf_gpu::blur_probe::blur();
    let lanes = kerf_gpu::blur_probe::measured_lanes();
    eprintln!(
        "CHECKER-P probe: {verdict:?}, {lanes} lanes (KERF_FFMPEG={:?})",
        std::env::var("KERF_FFMPEG").ok()
    );
    if verdict != kerf_gpu::blur_probe::Blur::Match {
        let own = lanes_of(&[]);
        eprintln!("CHECKER-P probe: not a match; the model on a plain shape says lanes {own:?}");
        return;
    }
    let l = u32::from(lanes);
    let cases = cases_of(samples(), 0x4242);
    let widths: Vec<u32> = if l == 0 { vec![8, 16] } else { vec![l] };
    let mut failures = Vec::new();
    for w in widths {
        let swept = sweep(&cases, |i| if i % 3 == 0 { Graph::Complex } else { Graph::Simple }, &[], w);
        report("probe-width", "own", w, &swept);
        if l != 0 || swept.unsound.is_empty() {
            failures.extend(swept.unsound);
        }
    }
    assert!(
        failures.is_empty(),
        "the probe said Match ({lanes} lanes) and the rule clears planes that differ: {} first {:#?}",
        failures.len(),
        &failures[..failures.len().min(10)]
    );
}

// ---- the boundaries, against FFmpeg -----------------------------------------------------------------

/// Whether any of three runs of `w` x `h` on `t` threads differs from the model on a plane (the race of an empty slice can hide a bad
/// plane in a run of one): per plane, `Y`, `U`, `V`.
fn differs_in_three(w: usize, h: usize, t: usize, cpu: &[&str]) -> Option<[bool; 3]> {
    let mut bad = [false; 3];
    for seed in 0..3u64 {
        let (planes, got) = blurred(w, h, "3", Some(t), Graph::Simple, cpu, 1 + seed)?;
        for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
            bad[k] |= got[k] != model(&planes[k], pw, ph, 3.0);
        }
    }
    Some(bad)
}

/// The edge of each hazard is where the rule puts it, on both sides, against FFmpeg on each kernel: a plane the rule refuses differs from
/// the model (at sigma 3, where what the kernel does wrong moves the picture by tens of levels) and one it clears does not. So a comparison
/// moved by one in the rule is a refusal of an exact plane or a clearing of a bad one, at the plane it moved to.
///
/// * rows against threads: `rows - 1`, `rows` (exact) and `rows + 1` threads (bad), for plane heights 2 to 40;
/// * width against the vector: `lanes - 1`, `lanes` (bad) and `lanes + 1` (exact), under one thread that cuts a slice of `lanes` rows or
///   more;
/// * a slice's rows against the vector: `lanes - 1` (exact) and `lanes`, `lanes + 1` (bad) rows, at one thread, and `2 * lanes - 1` and
///   `2 * lanes` rows in two slices (`lanes - 1` and `lanes`).
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_blur_the_edge_of_each_hazard_is_where_the_rule_puts_it() {
    let mut failures = Vec::new();
    let mut edges = 0;
    for (name, cpu, lanes) in kernels() {
        let mut cases: Vec<(usize, usize, usize)> = Vec::new();
        // Rows against threads (a picture twice as tall has chroma of the plane's height: the luma plane is the test, the chroma planes
        // are judged by their own dimensions all the same).
        for h in 2..=40usize {
            for t in [h - 1, h, h + 1] {
                for w in [40usize, 65] {
                    cases.push((w, h, t.max(1)));
                }
            }
        }
        let l = lanes as usize;
        for w in [l - 1, l, l + 1] {
            for h in [l - 1, l, l + 1, 2 * l - 1, 2 * l] {
                for t in [1usize, 2] {
                    cases.push((w.max(2), h, t));
                }
            }
        }
        cases.sort_unstable();
        cases.dedup();
        for (w, h, t) in cases {
            let Some(bad) = differs_in_three(w, h, t, &cpu) else {
                continue;
            };
            for (k, (pw, ph)) in plane_dims(w, h).into_iter().enumerate() {
                let Some(hazard) = plane_hazard((pw as u32, ph as u32), t as u32, lanes) else {
                    if bad[k] {
                        failures.push(format!(
                            "{name} {w}x{h} T{t} plane {} {pw}x{ph}: cleared and bad",
                            ["Y", "U", "V"][k]
                        ));
                    }
                    continue;
                };
                if hazard == KernelHazard::OneSample {
                    continue;
                }
                edges += 1;
                if !bad[k] {
                    failures.push(format!(
                        "{name} {w}x{h} T{t} plane {} {pw}x{ph}: {hazard:?} refused, and three runs are the model's",
                        ["Y", "U", "V"][k]
                    ));
                }
            }
        }
    }
    eprintln!(
        "CHECKER-P edges: {edges} refused planes at the edges, each bad in FFmpeg; {} problems",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} problems, first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(15)]
    );
}
