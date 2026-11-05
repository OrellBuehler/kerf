//! `checker/p2/sigma0`: the lifted sigma-0 hazard, tried to be broken against the real `gblur` on the build `KERF_FFMPEG` names (6.1.1 by
//! default).
//!
//! The plan clears one hazard for a blur of sigma exactly 0 (`gblur::plane_hazard_for(.., identity)`): more slice threads than rows
//! (`EmptySlices`), on the argument that the empty slice scales a vector by `1 * 1` and clips it to the depth's range again, which is
//! the sample it was, in any interleaving. What this holds the argument to:
//!
//! * `checker_p2_sigma0_every_plane_the_lifted_rule_clears_is_the_picture`: every plane the lifted rule clears is the input, to the
//!   sample, on each kernel the machine has, in a simple and a complex graph, over a grid of widths, heights and thread counts, three runs
//!   of each (the slice is racy), for the 8-bit 4:2:0 the engine draws **and** the formats an `gblur` accepts that it does not (10, 12
//!   and 16 bit, 4:4:4, 4:2:2, gray, an alpha plane, float): the argument is not about 8 bits, and a format the plan lets through
//!   some day must not be the one it breaks on. Also, at the kernel's level, for the sigmas whose derived coefficients are the identity without being 0 (which the plan does not lift).
//! * `checker_p2_sigma0_the_race_between_the_empty_slice_and_the_real_one_leaves_the_picture`: a few shapes run a hundred times each.
//! * `checker_p2_sigma0_what_the_plan_refuses_at_sigma_0_is_reported`: the planes the lifted rule still refuses (a plane one sample
//!   across, a vector of rows over a narrow plane) are run and counted; they are expected exact in a normal run (the heap tail they read
//!   is zero) and **not** exact when that tail is not finite, which is why they stay refused: the second half of the test runs the same
//!   pictures under `MALLOC_PERTURB_` and reports what changes.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p2_sigma0 -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kerf_core::gblur::{plane_hazard, plane_hazard_for, KernelHazard};
use kerf_gpu::effects::{blur_plane, gaussian};

const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

fn run(args: &[String], input: Vec<u8>, env: &[(&str, &str)]) -> Option<Vec<u8>> {
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    kerf_core::run_piped_until(&mut cmd, input, Instant::now() + CHILD_TIMEOUT)
}

fn lcg(seed: u64, n: usize) -> Vec<u32> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 24) as u32
        })
        .collect()
}

/// A pixel format `gblur` takes: how many planes, how deep, how they are sub-sampled.
#[derive(Clone, Copy)]
struct Format {
    name: &'static str,
    /// Bytes a sample takes in the raw file.
    bytes: usize,
    /// Bits of a sample (`0`: a 32-bit float).
    depth: u32,
    /// Chroma sub-sampling `(horizontal, vertical)` shifts; `None`: no chroma plane (gray) or full size.
    shift: (u32, u32),
    /// Planes: luma, chroma planes, alpha.
    chroma: usize,
    alpha: bool,
}

const fn fmt(name: &'static str, bytes: usize, depth: u32, shift: (u32, u32), chroma: usize, alpha: bool) -> Format {
    Format {
        name,
        bytes,
        depth,
        shift,
        chroma,
        alpha,
    }
}

const FORMATS: [Format; 12] = [
    fmt("yuv420p", 1, 8, (1, 1), 2, false),
    fmt("yuvj420p", 1, 8, (1, 1), 2, false),
    fmt("yuv420p10le", 2, 10, (1, 1), 2, false),
    fmt("yuv420p12le", 2, 12, (1, 1), 2, false),
    fmt("yuv420p16le", 2, 16, (1, 1), 2, false),
    fmt("yuv444p", 1, 8, (0, 0), 2, false),
    fmt("yuv422p", 1, 8, (1, 0), 2, false),
    fmt("yuva420p", 1, 8, (1, 1), 2, true),
    fmt("gray", 1, 8, (0, 0), 0, false),
    fmt("gray10le", 2, 10, (0, 0), 0, false),
    fmt("gbrp", 1, 8, (0, 0), 2, false),
    fmt("grayf32le", 4, 0, (0, 0), 0, false),
];

impl Format {
    /// The dimensions of each plane of a `w` x `h` picture, in the order the raw file has them.
    fn planes(&self, w: usize, h: usize) -> Vec<(usize, usize)> {
        let (cw, ch) = (
            (w + (1 << self.shift.0) - 1) >> self.shift.0,
            (h + (1 << self.shift.1) - 1) >> self.shift.1,
        );
        let mut out = vec![(w, h)];
        out.extend(std::iter::repeat_n((cw, ch), self.chroma));
        if self.alpha {
            out.push((w, h));
        }
        out
    }

    /// A picture of noise: every sample in the depth's range (a float one in `[0, 1000)`), as the raw file.
    fn noise(&self, w: usize, h: usize, seed: u64) -> Vec<u8> {
        let mut raw = Vec::new();
        for (k, (pw, ph)) in self.planes(w, h).into_iter().enumerate() {
            for v in lcg(seed + 31 * k as u64 + (w * 131 + h) as u64, pw * ph) {
                match self.depth {
                    0 => raw.extend_from_slice(&((v % 1_000_000) as f32 / 1000.0).to_le_bytes()),
                    8 => raw.push((v & 0xff) as u8),
                    d => raw.extend_from_slice(&((v & ((1 << d) - 1)) as u16).to_le_bytes()),
                }
            }
        }
        raw
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Graph {
    Simple,
    Complex,
}

/// `format`'s `w` x `h` noise through `gblur` at `sigma` on `threads` slice threads: `(input, output)` as raw bytes; `None` when FFmpeg
/// would not run it.
fn blurred(
    format: Format,
    sigma: f64,
    (w, h, threads): (usize, usize, usize),
    graph: Graph,
    cpu: &[&str],
    env: &[(&str, &str)],
) -> Option<(Vec<u8>, Vec<u8>)> {
    let raw = format.noise(w, h, 5);
    let mut args: Vec<String> = cpu.iter().map(|s| (*s).to_string()).collect();
    args.extend(["-hide_banner", "-loglevel", "error", "-nostdin"].map(String::from));
    let t = threads.to_string();
    args.extend(["-filter_threads".into(), t.clone(), "-filter_complex_threads".into(), t]);
    args.extend(
        [
            "-f",
            "rawvideo",
            "-pix_fmt",
            format.name,
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
    args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", format.name, "pipe:1"].map(String::from));
    let out = run(&args, raw.clone(), env)?;
    (out.len() == raw.len()).then_some((raw, out))
}

/// The kernel's vector width as an experiment finds it (a 12x16 luma plane at one thread, as `blur_slices.rs` does).
fn kernel_lanes(cpu: &[&str]) -> Option<u32> {
    let g = gaussian(3.0).expect("sigma 3");
    let (raw, out) = blurred(FORMATS[0], 3.0, (12, 16, 1), Graph::Simple, cpu, &[])?;
    let model = |(pw, ph): (usize, usize), at: usize| {
        let plane = &raw[at..at + pw * ph];
        out[at..at + pw * ph] == blur_plane(plane, pw, ph, &g)[..]
    };
    let luma = model((12, 16), 0);
    let chroma = model((6, 8), 192) && model((6, 8), 240);
    match (luma, chroma) {
        (false, true) => Some(16),
        (true, false) => Some(8),
        _ => None,
    }
}

type Kernel = (&'static str, Vec<&'static str>, u32);

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

/// Where a plane's samples are in the raw file.
fn offsets(format: Format, w: usize, h: usize) -> Vec<(usize, usize, usize, usize)> {
    let mut at = 0;
    format
        .planes(w, h)
        .into_iter()
        .map(|(pw, ph)| {
            let start = at;
            at += pw * ph * format.bytes;
            (pw, ph, start, at)
        })
        .collect()
}

/// Sigmas whose derived coefficients are the identity (`nu` 0, both scales 1): 0, and any small enough for `1 + 4 lambda` to round to 1. The
/// plan lifts the empty slice only for sigma 0 (the NaN path of `set_params`); the tiny ones are held here at the kernel's own level
/// (`plane_hazard_for(.., true)` against the filter), as evidence for the argument and not as a lift.
const IDENTITY_SIGMAS: [f64; 3] = [0.0, 1e-9, 1e-12];

#[derive(Default)]
struct Tally {
    cleared: usize,
    lifted: usize,
    refused: usize,
    refused_exact: usize,
    failures: Vec<String>,
    refused_by_kind: BTreeMap<String, (usize, usize)>,
}

fn sweep(
    format: Format,
    sigmas: &[f64],
    shapes: &[(usize, usize, usize)],
    graph: Graph,
    (cpu, lanes): (&[&str], u32),
    runs: usize,
) -> Tally {
    let tally = Mutex::new(Tally::default());
    let next = AtomicUsize::new(0);
    let jobs: Vec<(f64, (usize, usize, usize))> = sigmas
        .iter()
        .flat_map(|&s| shapes.iter().map(move |&shape| (s, shape)))
        .collect();
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&(sigma, (w, h, t))) = jobs.get(i) else { break };
                for run in 0..runs {
                    let found = blurred(format, sigma, (w, h, t), graph, cpu, &[]);
                    let mut tally = tally.lock().unwrap();
                    let Some((raw, out)) = found else {
                        tally
                            .failures
                            .push(format!("{} {graph:?} {w}x{h} T{t} sigma {sigma}: FFmpeg failed", format.name));
                        continue;
                    };
                    for (k, (pw, ph, from, to)) in offsets(format, w, h).into_iter().enumerate() {
                        let exact = raw[from..to] == out[from..to];
                        let dims = (pw as u32, ph as u32);
                        match plane_hazard_for(dims, t as u32, lanes, true) {
                            None => {
                                tally.cleared += 1;
                                tally.lifted += usize::from(plane_hazard(dims, t as u32, lanes).is_some());
                                if !exact {
                                    let differing = raw[from..to].iter().zip(&out[from..to]).filter(|(a, b)| a != b).count();
                                    tally.failures.push(format!(
                                        "{} {graph:?} {w}x{h} T{t} sigma {sigma} run {run} plane {k} {pw}x{ph}: cleared and not the picture ({differing} bytes differ)",
                                        format.name
                                    ));
                                }
                            }
                            Some(kind) => {
                                tally.refused += 1;
                                tally.refused_exact += usize::from(exact);
                                let e = tally.refused_by_kind.entry(format!("{kind:?}")).or_default();
                                e.0 += 1;
                                e.1 += usize::from(exact);
                            }
                        }
                    }
                }
            });
        }
    });
    tally.into_inner().unwrap()
}

fn grid(widths: &[usize], heights: &[usize], threads: &[usize], only_beyond_rows: bool) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for &t in threads {
        for &h in heights {
            for &w in widths {
                if !only_beyond_rows || t > h.div_ceil(2) || t == 1 {
                    out.push((w, h, t));
                }
            }
        }
    }
    out
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_p2_sigma0_every_plane_the_lifted_rule_clears_is_the_picture() {
    let kernels = kernels();
    assert!(!kernels.is_empty(), "no AVX2 or AVX-512 kernel on this machine");
    let mut failures = Vec::new();
    let mut ran = 0usize;
    // The 8-bit 4:2:0 the engine draws: a full grid, every threads-beyond-rows pair, both graphs, three runs of each.
    let widths = [2usize, 3, 4, 8, 9, 15, 16, 17, 33, 40];
    let heights = [2usize, 3, 4, 5, 7, 8, 9, 12, 16, 17, 33];
    let threads = [1usize, 2, 3, 5, 8, 9, 16, 17, 32, 64];
    let main = grid(&widths, &heights, &threads, true);
    // The other formats: a smaller grid, the simple graph, two runs.
    let others = grid(&[2, 9, 16, 40], &[3, 8, 17], &[5, 17, 64], true);
    for (name, cpu, lanes) in &kernels {
        for graph in [Graph::Simple, Graph::Complex] {
            let shapes: Vec<_> = if graph == Graph::Simple {
                main.clone()
            } else {
                main.iter()
                    .copied()
                    .filter(|&(w, h, _)| [2, 9, 16, 40].contains(&w) && [3, 5, 8, 17].contains(&h))
                    .collect()
            };
            let t = sweep(FORMATS[0], &IDENTITY_SIGMAS[..1], &shapes, graph, (cpu, *lanes), 2);
            eprintln!(
                "CHECKER-P2 sigma0 yuv420p {name} {graph:?}: {} shapes x 2 runs, {} planes cleared ({} only by the lift), {} refused ({} exact) {:?}",
                shapes.len(),
                t.cleared,
                t.lifted,
                t.refused,
                t.refused_exact,
                t.refused_by_kind
            );
            assert!(t.lifted > 100, "the grid never reached the lifted hazard");
            ran += t.cleared;
            failures.extend(t.failures);
        }
        // The sigmas that derive the identity's coefficients without being 0 (kernel level; the plan does not lift them).
        let shapes = grid(&[2, 9, 16, 40], &[3, 8, 17], &[5, 17, 64], true);
        let t = sweep(FORMATS[0], &IDENTITY_SIGMAS[1..], &shapes, Graph::Simple, (cpu, *lanes), 2);
        eprintln!(
            "CHECKER-P2 sigma0 tiny sigmas {name}: {} planes cleared ({} only by the lift), {} refused ({} exact)",
            t.cleared, t.lifted, t.refused, t.refused_exact
        );
        ran += t.cleared;
        failures.extend(t.failures);
        for format in &FORMATS[1..] {
            let t = sweep(*format, &IDENTITY_SIGMAS[..1], &others, Graph::Simple, (cpu, *lanes), 2);
            eprintln!(
                "CHECKER-P2 sigma0 {} {name}: {} planes cleared ({} only by the lift), {} refused ({} exact)",
                format.name, t.cleared, t.lifted, t.refused, t.refused_exact
            );
            ran += t.cleared;
            failures.extend(t.failures);
        }
    }
    eprintln!("CHECKER-P2 sigma0: {ran} cleared planes held to the picture");
    failures.sort();
    failures.dedup();
    assert!(
        failures.is_empty(),
        "{} failures, first:\n{}",
        failures.len(),
        failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_p2_sigma0_the_race_between_the_empty_slice_and_the_real_one_leaves_the_picture() {
    // Tiny pictures under many more threads than rows: the most empty slices a plane can have, so the most writes to the same vector.
    let kernels = kernels();
    assert!(!kernels.is_empty());
    let shapes = [
        (40usize, 2usize, 64usize),
        (40, 3, 32),
        (16, 4, 17),
        (9, 6, 16),
        (17, 8, 64),
        (2, 2, 9),
        (33, 5, 64),
    ];
    let mut failures = Vec::new();
    for (name, cpu, lanes) in &kernels {
        for graph in [Graph::Simple, Graph::Complex] {
            let t = sweep(FORMATS[0], &IDENTITY_SIGMAS[..1], &shapes, graph, (cpu, *lanes), 40);
            eprintln!(
                "CHECKER-P2 sigma0 race {name} {graph:?}: {} planes cleared ({} only by the lift) x 40 runs, {} failures",
                t.cleared,
                t.lifted,
                t.failures.len()
            );
            assert!(t.lifted > 0);
            failures.extend(t.failures);
        }
    }
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} failures, first:\n{}",
        failures.len(),
        failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// An allocator shim for the tests below: what `posix_memalign` / `aligned_alloc` / `memalign` (what `av_malloc` calls) hand out is filled
/// with the byte `NANHEAP_BYTE` (default `0xff`, which makes a float NaN) before FFmpeg sees it. A heap is not promised to be zero or
/// finite, and `gblur`'s buffer (`av_malloc_array`, never written past the plane) is exactly the memory the blocked kernel reads past a
/// row's end. `None` where there is no C compiler or no `LD_PRELOAD`.
fn heap_shim() -> Option<std::path::PathBuf> {
    static SHIM: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    SHIM.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-p2-sigma0");
        std::fs::create_dir_all(&dir).ok()?;
        let (src, lib) = (
            dir.join(format!("nanheap-{}.c", std::process::id())),
            dir.join(format!("libnanheap-{}.so", std::process::id())),
        );
        std::fs::write(
            &src,
            r#"#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>
static unsigned char fill(void) { const char *v = getenv("NANHEAP_BYTE"); return v ? (unsigned char)strtol(v, 0, 0) : 0xff; }
int posix_memalign(void **out, size_t align, size_t size) {
    static int (*real)(void **, size_t, size_t);
    if (!real) real = dlsym(RTLD_NEXT, "posix_memalign");
    int rc = real(out, align, size);
    if (rc == 0 && *out && size) memset(*out, fill(), size);
    return rc;
}
void *aligned_alloc(size_t align, size_t size) {
    static void *(*real)(size_t, size_t);
    if (!real) real = dlsym(RTLD_NEXT, "aligned_alloc");
    void *p = real(align, size);
    if (p && size) memset(p, fill(), size);
    return p;
}
void *memalign(size_t align, size_t size) {
    static void *(*real)(size_t, size_t);
    if (!real) real = dlsym(RTLD_NEXT, "memalign");
    void *p = real(align, size);
    if (p && size) memset(p, fill(), size);
    return p;
}
"#,
        )
        .ok()?;
        let mut cc = std::process::Command::new("cc");
        cc.args(["-shared", "-fPIC", "-O1", "-o"]).arg(&lib).arg(&src).arg("-ldl");
        let out = child::output_within(cc, Duration::from_secs(60)).ok()?;
        out.status.success().then_some(lib)
    })
    .clone()
}

#[path = "common/child.rs"]
mod child;

/// The lifted planes do not depend on the heap: with every allocation FFmpeg makes filled with NaN (`0xff`), a denormal-looking `0x80` and a
/// large finite `0x7f`, a plane the lifted rule clears is still the picture. (The empty slice, the one hazard the lift drops, reads and writes
/// whole vectors of the buffer from a row's start; this holds the argument that none of that reaches a sample of the plane to a heap that
/// would show it.)
#[test]
#[ignore = "needs ffmpeg and a C compiler"]
fn checker_p2_sigma0_the_lifted_planes_do_not_depend_on_what_the_heap_holds() {
    let Some(shim) = heap_shim() else {
        eprintln!("CHECKER-P2 sigma0: no C compiler or no LD_PRELOAD here: not run");
        return;
    };
    let shim = shim.to_string_lossy().into_owned();
    let kernels = kernels();
    assert!(!kernels.is_empty());
    let shapes = grid(&[2, 3, 9, 16, 17, 40], &[2, 3, 5, 8, 17], &[5, 17, 64], true);
    let mut failures = Vec::new();
    let mut cleared = 0usize;
    for byte in ["0xff", "0x80", "0x7f"] {
        for (name, cpu, lanes) in &kernels {
            for &(w, h, t) in &shapes {
                let env = [("LD_PRELOAD", shim.as_str()), ("NANHEAP_BYTE", byte)];
                let Some((raw, out)) = blurred(FORMATS[0], 0.0, (w, h, t), Graph::Simple, cpu, &env) else {
                    failures.push(format!("{name} {w}x{h} T{t} heap {byte}: FFmpeg failed"));
                    continue;
                };
                for (k, (pw, ph, from, to)) in offsets(FORMATS[0], w, h).into_iter().enumerate() {
                    if plane_hazard_for((pw as u32, ph as u32), t as u32, *lanes, true).is_none() {
                        cleared += 1;
                        if raw[from..to] != out[from..to] {
                            failures.push(format!(
                                "{name} {w}x{h} T{t} heap {byte} plane {k} {pw}x{ph}: cleared and not the picture"
                            ));
                        }
                    }
                }
            }
        }
    }
    eprintln!("CHECKER-P2 sigma0 poisoned heaps: {cleared} cleared planes held to the picture");
    assert!(cleared > 500);
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} failures, first:\n{}",
        failures.len(),
        failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// What the lifted rule still refuses is refused for a reason, shown by drawing it anyway: a vector of rows over a narrow plane
/// (`NarrowBlocks`) is the picture on a zero heap and **not** on one that holds NaN past the row (`0 * NaN`), and a plane one sample across
/// (`OneSample`) is not the picture on that heap and takes FFmpeg down (SIGSEGV, SIGABRT) on some shapes even on the ordinary one. Reports the
/// counts; asserts that each refusal has a frame that is not exact, per kernel.
#[test]
#[ignore = "needs ffmpeg and a C compiler"]
fn checker_p2_sigma0_what_the_plan_refuses_at_sigma_0_is_not_the_picture_on_a_heap_that_is_not_zero() {
    let Some(shim) = heap_shim() else {
        eprintln!("CHECKER-P2 sigma0: no C compiler or no LD_PRELOAD here: not run");
        return;
    };
    let shim = shim.to_string_lossy().into_owned();
    let kernels = kernels();
    assert!(!kernels.is_empty());
    // (Luma whose last block of rows ends at the plane's bottom reads the buffer's never-written tail past the row; chroma and a luma
    // block with rows below it read pixels of the picture or of the plane before, which are finite.)
    let narrow: [(usize, usize, usize); 8] = [
        (12, 16, 1),
        (16, 64, 2),
        (12, 32, 1),
        (14, 80, 4),
        (8, 8, 1),
        (6, 16, 2),
        (8, 24, 1),
        (8, 32, 2),
    ];
    let thin: [(usize, usize, usize); 6] = [(1, 8, 1), (1, 16, 2), (1, 2, 2), (2, 40, 1), (3, 17, 2), (1, 40, 1)];
    for (name, cpu, lanes) in &kernels {
        for (kind, shapes, want) in [
            ("NarrowBlocks", &narrow[..], KernelHazard::NarrowBlocks),
            ("OneSample", &thin[..], KernelHazard::OneSample),
        ] {
            let (mut ran, mut zero_exact, mut zero_failed, mut nan_not_exact, mut nan_failed) = (0, 0, 0, 0, 0);
            for &(w, h, t) in shapes {
                // (Only the planes the lifted rule refuses for this reason.)
                let refused: Vec<usize> = offsets(FORMATS[0], w, h)
                    .into_iter()
                    .enumerate()
                    .filter(|(_, (pw, ph, ..))| plane_hazard_for((*pw as u32, *ph as u32), t as u32, *lanes, true) == Some(want))
                    .map(|(k, _)| k)
                    .collect();
                if refused.is_empty() {
                    continue;
                }
                ran += 1;
                match blurred(FORMATS[0], 0.0, (w, h, t), Graph::Simple, cpu, &[]) {
                    Some((raw, out)) => {
                        let o = offsets(FORMATS[0], w, h);
                        zero_exact += usize::from(refused.iter().all(|&k| raw[o[k].2..o[k].3] == out[o[k].2..o[k].3]));
                    }
                    None => zero_failed += 1,
                }
                let env = [("LD_PRELOAD", shim.as_str()), ("NANHEAP_BYTE", "0xff")];
                match blurred(FORMATS[0], 0.0, (w, h, t), Graph::Simple, cpu, &env) {
                    Some((raw, out)) => {
                        let o = offsets(FORMATS[0], w, h);
                        nan_not_exact += usize::from(refused.iter().any(|&k| raw[o[k].2..o[k].3] != out[o[k].2..o[k].3]));
                    }
                    None => nan_failed += 1,
                }
            }
            eprintln!(
                "CHECKER-P2 sigma0 refused {kind} on {name}: {ran} pictures; zero heap: {zero_exact} exact, {zero_failed} took FFmpeg down; NaN heap: {nan_not_exact} not the picture, {nan_failed} took FFmpeg down"
            );
            assert!(ran > 0, "{kind}: no shape the rule refuses for it");
            assert!(
                nan_not_exact + nan_failed > 0,
                "{name}: no {kind} picture was wrong on a NaN heap: the refusal would be wider than it has to be"
            );
        }
    }
}

/// What `Gaussian::is_identity` says (`set_params` replayed in `f64`) is what the filter does, over a scan of small sigmas: the cancellation
/// in `1 + 2 lambda - sqrt(1 + 4 lambda)` makes `nu` 0 for some sigmas and not for the ones next to them (1e-4 and 1e-5 are the identity,
/// 1e-6 is a gain of 2e17), so the classification is as good as the replay of the build's own double arithmetic. The plan lifts only sigma
/// exactly 0 and does not rely on it for the others; this holds the replay itself to the build, which the draw of every small sigma
/// rests on. A sigma called the identity that the filter does not leave alone is **unsound**; one the filter leaves alone that is not called
/// the identity is only a classification wider than it has to be, and is counted.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_p2_sigma0_the_plans_identity_is_the_filters_over_a_scan_of_small_sigmas() {
    let mut sigmas: Vec<f64> = Vec::new();
    let mut e = -14.0f64;
    while e <= -2.0 {
        sigmas.push(10f64.powf(e));
        e += 0.0625;
    }
    // The cancellation zone, finely, and a few the f32 option rounds to the same value.
    let mut s = 0.9e-8;
    while s <= 1.5e-8 {
        sigmas.push(s);
        s += 0.01e-8;
    }
    sigmas.extend([
        0.0,
        5e-324,
        1e-30,
        1e-20,
        f64::from(f32::MIN_POSITIVE),
        1.0e-4 + 1e-12,
        1.0e-5 * 1.000_000_1,
    ]);
    let next = AtomicUsize::new(0);
    let found: Mutex<Vec<(f64, bool, Option<bool>)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&sigma) = sigmas.get(i) else { break };
                let plan_says = gaussian_identity(sigma);
                let ran = blurred(FORMATS[0], sigma, (40, 40, 1), Graph::Simple, &[], &[]).map(|(raw, out)| raw == out);
                found.lock().unwrap().push((sigma, plan_says, ran));
            });
        }
    });
    let mut found = found.into_inner().unwrap();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut unsound, mut wider, mut agree, mut unrun) = (Vec::new(), 0usize, 0usize, 0usize);
    for &(sigma, plan_says, ran) in &found {
        match (plan_says, ran) {
            (_, None) => unrun += 1,
            (true, Some(false)) => unsound.push(sigma),
            (false, Some(true)) => wider += 1,
            _ => agree += 1,
        }
    }
    eprintln!(
        "CHECKER-P2 sigma0 scan: {} sigmas, {agree} agree, {wider} left alone by the filter and not called the identity, {} called the identity and not left alone, {unrun} did not run",
        found.len(),
        unsound.len()
    );
    assert!(unrun == 0, "FFmpeg would not run {unrun} of the sigmas");
    assert!(
        unsound.is_empty(),
        "called the identity and not left alone by the filter: {unsound:?}"
    );
}

fn gaussian_identity(sigma: f64) -> bool {
    kerf_core::gblur::gaussian(sigma).is_some_and(|g| g.is_identity())
}
