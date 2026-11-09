//! `checker/mx/cascade`: the cascade of a `scale` that resizes a picture as it takes it from one YCbCr matrix to another
//! (`kerf_gpu::convert::scaled_to_rgb24`, `Compositor::scale_convert_yuv`, `scaled_convert.wgsl`) against FFmpeg 9's own `scale`, plane for
//! plane, **0 differing samples**, on pictures the implementer's noise does not make: hard edges (a vertical filter overshoots a byte there,
//! which the C output functions of the last two rows index the tables with), single-sample stripes, saturated chroma, flat pictures of every
//! luma, ramps, a bright bottom edge; at the sizes where the plan's guards switch (equal area and one sample off it, one axis unscaled,
//! tiny pictures, 63:1, thin and tall pictures), every ordered pair of matrices, `yuv420p` and `yuva420p` destinations, 1 to 16 filter
//! threads, and through the export's own chains (a decider, a pad, `fps`, `format=yuva420p`).
//!
//! The refusals are held necessary the same way: a picture the guards refuse, put through the cascade anyway, is not FFmpeg's.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_mx_cascade -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
//!
//! On a build that does not cascade (FFmpeg 6: the policy is `FixedBt601`) every test returns at once.

#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::layer_geometry::{Rect, ScaleStage};
use kerf_core::{CompositeColorPolicy, YuvMatrix};
use kerf_gpu::convert::{rgb_to_yuv420_24, scaled_to_rgb24, Packed, Pic, Variant};
use kerf_gpu::{Compositor, Gpu, GpuOptions, YuvFrame};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| Compositor::new(gpu()).expect("a compositor"))
}

/// Whether this FFmpeg negotiates the matrix along the chain (a build that does not never asks `scale` to change one).
fn cascades() -> bool {
    let policy = kerf_core::composite_color_policy();
    if policy != CompositeColorPolicy::BottomLayerTag {
        eprintln!("{}: policy {policy:?}, nothing is converted", kerf_core::ffmpeg_path());
        return false;
    }
    assert_eq!(
        kerf_gpu::convert::scaled(),
        kerf_gpu::convert::Scaled::Match,
        "{}: the cascade probe",
        kerf_core::ffmpeg_path()
    );
    true
}

// ---- pictures --------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 20) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len() as u32) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Uniform noise in all three planes.
    Noise,
    /// Blocks of random size and a value from the extremes (0, 16, 235, 255) in luma and chroma: overshoot on both sides.
    Edges,
    /// Alternating 0 / 255 columns of this period (in luma samples), chroma saturated in alternation.
    StripesV(u32),
    /// ... rows.
    StripesH(u32),
    /// Smooth ramps in all three planes.
    Ramp,
    /// Every 2x2 chroma block a corner of the chroma square, luma noise: clipped RGB everywhere.
    Saturated,
    /// A picture of one value.
    Flat(u8, u8, u8),
    /// Noise with the last rows (and the first) black and white in turn, chroma at the extremes: the C rows' overshoot.
    EdgeRows,
}

fn picture(kind: Kind, w: u32, h: u32, seed: u64) -> YuvFrame {
    let (cw, ch) = (w / 2, h / 2);
    let mut r = Rng::new(seed ^ 0xA5A5);
    let (mut y, mut u, mut v) = (
        vec![0u8; (w * h) as usize],
        vec![0u8; (cw * ch) as usize],
        vec![0u8; (cw * ch) as usize],
    );
    match kind {
        Kind::Noise => {
            y.iter_mut().for_each(|p| *p = r.below(256) as u8);
            u.iter_mut().for_each(|p| *p = r.below(256) as u8);
            v.iter_mut().for_each(|p| *p = r.below(256) as u8);
        }
        Kind::Edges => {
            let extremes = [0u8, 16, 235, 255, 128];
            let (bw, bh) = (1 + r.below(7), 1 + r.below(7));
            for j in 0..h {
                for i in 0..w {
                    let mut cell = Rng::new(seed ^ u64::from(i / bw) << 20 ^ u64::from(j / bh) << 8 ^ 0x77);
                    y[(j * w + i) as usize] = cell.pick(&extremes);
                }
            }
            let (cbw, cbh) = (1 + r.below(4), 1 + r.below(4));
            for j in 0..ch {
                for i in 0..cw {
                    let mut cell = Rng::new(seed ^ u64::from(i / cbw) << 21 ^ u64::from(j / cbh) << 9 ^ 0x99);
                    u[(j * cw + i) as usize] = cell.pick(&[0u8, 16, 128, 240, 255]);
                    v[(j * cw + i) as usize] = cell.pick(&[0u8, 16, 128, 240, 255]);
                }
            }
        }
        Kind::StripesV(p) => {
            for j in 0..h {
                for i in 0..w {
                    y[(j * w + i) as usize] = if (i / p) % 2 == 0 { 0 } else { 255 };
                }
            }
            for j in 0..ch {
                for i in 0..cw {
                    u[(j * cw + i) as usize] = if (i / p.div_ceil(2)) % 2 == 0 { 0 } else { 255 };
                    v[(j * cw + i) as usize] = if (i / p.div_ceil(2)) % 2 == 0 { 255 } else { 0 };
                }
            }
        }
        Kind::StripesH(p) => {
            for j in 0..h {
                for i in 0..w {
                    y[(j * w + i) as usize] = if (j / p) % 2 == 0 { 255 } else { 0 };
                }
            }
            for j in 0..ch {
                for i in 0..cw {
                    u[(j * cw + i) as usize] = if (j / p.div_ceil(2)) % 2 == 0 { 255 } else { 0 };
                    v[(j * cw + i) as usize] = if (j / p.div_ceil(2)) % 2 == 0 { 0 } else { 255 };
                }
            }
        }
        Kind::Ramp => {
            for j in 0..h {
                for i in 0..w {
                    y[(j * w + i) as usize] = ((i * 255) / w.max(1) + (j * 31) / h.max(1)).min(255) as u8;
                }
            }
            for j in 0..ch {
                for i in 0..cw {
                    u[(j * cw + i) as usize] = ((i * 255) / cw.max(1)) as u8;
                    v[(j * cw + i) as usize] = (255 - (j * 255) / ch.max(1)) as u8;
                }
            }
        }
        Kind::Saturated => {
            y.iter_mut().for_each(|p| *p = r.below(256) as u8);
            u.iter_mut().for_each(|p| *p = if r.below(2) == 0 { 0 } else { 255 });
            v.iter_mut().for_each(|p| *p = if r.below(2) == 0 { 0 } else { 255 });
        }
        Kind::Flat(a, b, c) => {
            y.fill(a);
            u.fill(b);
            v.fill(c);
        }
        Kind::EdgeRows => {
            y.iter_mut().for_each(|p| *p = r.below(256) as u8);
            u.iter_mut().for_each(|p| *p = r.pick(&[0u8, 128, 255]));
            v.iter_mut().for_each(|p| *p = r.pick(&[0u8, 128, 255]));
            for j in 0..h {
                let edge = j < 3 || j + 6 >= h;
                if edge {
                    for i in 0..w {
                        y[(j * w + i) as usize] = if (i + j) % 2 == 0 { 0 } else { 255 };
                    }
                }
            }
        }
    }
    YuvFrame {
        width: w,
        height: h,
        y,
        u,
        v,
    }
}

fn planes(f: &YuvFrame) -> [&[u8]; 3] {
    [&f.y, &f.u, &f.v]
}

// ---- FFmpeg ----------------------------------------------------------------------------------------

/// The tags FFmpeg reads as each matrix (`PlanStream::matrix`: `smpte170m`, `bt470bg` and no tag are BT.601).
fn tags(m: YuvMatrix) -> &'static [&'static str] {
    match m {
        YuvMatrix::Bt601 => &["smpte170m", "bt470bg"],
        YuvMatrix::Bt709 => &["bt709"],
        YuvMatrix::Bt2020 => &["bt2020nc"],
    }
}

const MATRICES: [YuvMatrix; 3] = [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020];

fn pairs() -> Vec<(YuvMatrix, YuvMatrix)> {
    MATRICES
        .iter()
        .flat_map(|f| MATRICES.iter().map(move |t| (*f, *t)))
        .filter(|(f, t)| f != t)
        .collect()
}

/// FFmpeg's bare `scale` of a `yuv420p` frame to `size`, naming both matrices, as raw `pix_fmt` bytes.
fn ffmpeg_scale(frame: &YuvFrame, size: (u32, u32), from: &str, to: &str, pix_fmt: &str, threads: u32, extra: &str) -> Vec<u8> {
    let mut input = frame.y.clone();
    input.extend(&frame.u);
    input.extend(&frame.v);
    let vf = format!(
        "setparams=colorspace={from}:range=tv,scale={}:{}:out_color_matrix={to}{extra},format={pix_fmt}",
        size.0, size.1
    );
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-filter_threads"])
        .arg(threads.to_string())
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s"])
        .arg(format!("{}x{}", frame.width, frame.height))
        .args([
            "-i",
            "-",
            "-vf",
            &vf,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    writer.join().expect("writer");
    assert!(out.status.success(), "{vf}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// How the planes `got` differ from `want`: per plane the count, the largest difference and the first (x, y).
fn diff_plane(got: &[u8], want: &[u8], width: usize) -> Option<(usize, i32, (usize, usize))> {
    assert_eq!(got.len(), want.len());
    let mut n = 0;
    let mut max = 0;
    let mut first = None;
    for (i, (a, b)) in got.iter().zip(want).enumerate() {
        if a != b {
            n += 1;
            max = max.max((i32::from(*a) - i32::from(*b)).abs());
            first.get_or_insert((i % width, i / width));
        }
    }
    (n > 0).then(|| (n, max, first.unwrap()))
}

fn describe(label: &str, got: [&[u8]; 3], want: [&[u8]; 3], size: (u32, u32)) -> Option<String> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut bad = Vec::new();
    for (name, g, wnt, width) in [
        ("Y", got[0], want[0], w),
        ("U", got[1], want[1], w / 2),
        ("V", got[2], want[2], w / 2),
    ] {
        if let Some((n, max, at)) = diff_plane(g, wnt, width) {
            bad.push(format!("{name}: {n} samples differ (max {max}, first at {at:?})"));
        }
    }
    let _ = h;
    (!bad.is_empty()).then(|| format!("{label}: {}", bad.join("; ")))
}

// ---- a case ----------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Case {
    src: (u32, u32),
    dst: (u32, u32),
    kind: Kind,
    seed: u64,
    from: YuvMatrix,
    to: YuvMatrix,
    from_tag: &'static str,
    to_tag: &'static str,
    pix_fmt: &'static str,
    threads: u32,
}

impl Case {
    fn new(src: (u32, u32), dst: (u32, u32), kind: Kind, pair: (YuvMatrix, YuvMatrix), n: usize) -> Self {
        let (from, to) = pair;
        Self {
            src,
            dst,
            kind,
            seed: 7 + n as u64,
            from,
            to,
            from_tag: tags(from)[n % tags(from).len()],
            to_tag: tags(to)[(n / 2) % tags(to).len()],
            pix_fmt: ["yuv420p", "yuva420p"][n % 2],
            threads: [1, 4, 16, 2, 3, 7][n % 6],
        }
    }
    fn name(&self) -> String {
        format!(
            "{}x{} -> {}x{} {:?} {:?}->{:?} ({}->{}) {} threads {}",
            self.src.0,
            self.src.1,
            self.dst.0,
            self.dst.1,
            self.kind,
            self.from,
            self.to,
            self.from_tag,
            self.to_tag,
            self.pix_fmt,
            self.threads
        )
    }
    fn frame(&self) -> YuvFrame {
        picture(self.kind, self.src.0, self.src.1, self.seed)
    }
    fn ffmpeg(&self, frame: &YuvFrame) -> Vec<u8> {
        ffmpeg_scale(frame, self.dst, self.from_tag, self.to_tag, self.pix_fmt, self.threads, "")
    }
}

/// FFmpeg's answers to `cases`, three children at a time.
fn ffmpeg_answers(cases: &[Case]) -> Vec<Vec<u8>> {
    let next = AtomicUsize::new(0);
    let out: Mutex<BTreeMap<usize, Vec<u8>>> = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(case) = cases.get(i) else { break };
                let frame = case.frame();
                let got = case.ffmpeg(&frame);
                out.lock().unwrap().insert(i, got);
            });
        }
    });
    out.into_inner().unwrap().into_values().collect()
}

/// The GPU's cascade against FFmpeg's, for every case: the failures, each described.
fn compare(cases: &[Case]) -> Vec<String> {
    let answers = ffmpeg_answers(cases);
    let mut failures = Vec::new();
    for (case, ffmpeg) in cases.iter().zip(&answers) {
        let frame = case.frame();
        let n = (case.dst.0 * case.dst.1) as usize;
        let gpu = match compositor().scale_convert_yuv(&frame, case.dst, case.from, case.to) {
            Ok(g) => g,
            Err(e) => {
                failures.push(format!("{}: the GPU would not draw it: {e}", case.name()));
                continue;
            }
        };
        let want_len = if case.pix_fmt == "yuva420p" { n * 5 / 2 } else { n * 3 / 2 };
        assert_eq!(ffmpeg.len(), want_len, "{}", case.name());
        let (fy, rest) = ffmpeg.split_at(n);
        let (fu, rest) = rest.split_at(n / 4);
        let (fv, fa) = rest.split_at(n / 4);
        if let Some(d) = describe(&case.name(), planes(&gpu), [fy, fu, fv], case.dst) {
            failures.push(d);
        }
        if case.pix_fmt == "yuva420p" && fa.iter().any(|a| *a != 255) {
            failures.push(format!("{}: FFmpeg's alpha plane is not 255 throughout", case.name()));
        }
    }
    failures
}

fn report(what: &str, cases: usize, failures: &[String]) {
    eprintln!(
        "{what}: {cases} cases, {} failing ({})",
        failures.len(),
        kerf_core::ffmpeg_path()
    );
    for f in failures.iter().take(30) {
        eprintln!("FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{what}: {} of {cases} cases differ from FFmpeg's, the first: {}",
        failures.len(),
        failures[0]
    );
}

fn kinds() -> Vec<Kind> {
    vec![
        Kind::Noise,
        Kind::Edges,
        Kind::StripesV(1),
        Kind::StripesV(3),
        Kind::StripesH(1),
        Kind::StripesH(2),
        Kind::Ramp,
        Kind::Saturated,
        Kind::Flat(16, 128, 128),
        Kind::Flat(235, 128, 128),
        Kind::Flat(0, 0, 255),
        Kind::Flat(255, 255, 0),
        Kind::EdgeRows,
    ]
}

fn drawn_by_the_plan(src: (u32, u32), dst: (u32, u32)) -> Option<&'static str> {
    ScaleStage {
        src: Rect::whole(src.0, src.1),
        scaled: dst,
        keep: Rect::whole(dst.0, dst.1),
    }
    .cascade_refusal()
}

// ---- sufficiency: what the plan draws is FFmpeg's ---------------------------------------------------

/// Sizes at the switches of the plan's guards and of swscale's tables, every one of which the plan draws.
type Size = (u32, u32);
type Resize = (Size, Size);

const BOUNDARY: &[Resize] = &[
    // one sample under the equal-area line, and a shrink by the smallest even steps
    ((66, 48), (64, 46)),
    ((64, 50), (64, 48)),
    ((66, 48), (64, 48)),
    ((98, 32), (94, 32)),
    ((98, 32), (96, 34)),
    ((96, 32), (64, 46)),
    ((96, 32), (62, 48)),
    ((100, 30), (64, 46)),
    ((64, 48), (96, 30)),
    ((128, 96), (64, 190)),
    ((200, 50), (50, 190)),
    ((50, 200), (190, 50)),
    // one axis unscaled
    ((64, 100), (64, 60)),
    ((100, 64), (60, 64)),
    ((64, 100), (64, 98)),
    ((100, 64), (98, 64)),
    // tiny
    ((8, 8), (6, 6)),
    ((10, 10), (6, 6)),
    ((12, 8), (8, 6)),
    ((16, 16), (6, 6)),
    ((6, 12), (6, 10)),
    ((8, 16), (4, 6)),
    ((32, 32), (30, 6)),
    ((20, 20), (18, 6)),
    // dh is 6, 8 and 10: the last two rows are a third to a fifth of the picture
    ((64, 48), (64, 6)),
    ((90, 48), (80, 8)),
    ((90, 48), (80, 10)),
    // 2:1, 3:1, non-integer ratios
    ((128, 96), (64, 48)),
    ((192, 144), (64, 48)),
    ((100, 70), (66, 46)),
    ((150, 100), (100, 66)),
    ((130, 90), (86, 60)),
    ((210, 130), (140, 86)),
    // thin and tall
    ((2000, 20), (100, 18)),
    ((20, 2000), (18, 100)),
    ((2000, 36), (500, 34)),
    ((16, 640), (8, 300)),
    ((640, 16), (300, 8)),
    // steep (63:1 on one axis)
    ((1008, 32), (16, 30)),
    ((32, 1008), (30, 16)),
    ((1260, 126), (20, 20)),
    ((500, 100), (10, 80)),
    ((100, 500), (80, 10)),
    // large
    ((1920, 1080), (640, 360)),
    ((1920, 1080), (538, 302)),
    ((1080, 1920), (170, 302)),
    ((1280, 720), (1278, 718)),
    ((1920, 1080), (1918, 1078)),
    ((1280, 720), (1280, 716)),
    ((1280, 720), (1000, 720)),
];

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_the_sizes_at_the_guards_are_ffmpegs_in_every_picture_and_pair() {
    if !cascades() {
        return;
    }
    let mut cases = Vec::new();
    let mut n = 0;
    for &(src, dst) in BOUNDARY {
        if let Some(why) = drawn_by_the_plan(src, dst) {
            eprintln!("{src:?} -> {dst:?}: refused by the plan ({why}), not held here");
            continue;
        }
        let big = src.0 * src.1 > 400_000;
        for (k, kind) in kinds().into_iter().enumerate() {
            // (A large picture with a few pictures and pairs: lavapipe draws them slowly.)
            if big && k % 4 != 0 {
                continue;
            }
            let pair = pairs()[(n + k) % 6];
            cases.push(Case::new(src, dst, kind, pair, n));
            n += 1;
        }
    }
    report("boundary sizes", cases.len(), &compare(&cases));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_every_ordered_pair_of_matrices_both_destinations_and_every_thread_count() {
    if !cascades() {
        return;
    }
    let mut cases = Vec::new();
    let mut n = 0;
    for (src, dst) in [
        ((96, 64), (50, 34)),
        ((200, 120), (80, 60)),
        ((64, 48), (62, 30)),
        ((90, 90), (30, 90 - 2)),
    ] {
        for pair in pairs() {
            for pix_fmt in ["yuv420p", "yuva420p"] {
                for threads in [1, 2, 3, 4, 7, 16] {
                    for kind in [Kind::Noise, Kind::Edges, Kind::EdgeRows] {
                        let mut c = Case::new(src, dst, kind, pair, n);
                        c.pix_fmt = pix_fmt;
                        c.threads = threads;
                        c.from_tag = tags(pair.0)[n % tags(pair.0).len()];
                        c.to_tag = tags(pair.1)[n % tags(pair.1).len()];
                        cases.push(c);
                        n += 1;
                    }
                }
            }
        }
    }
    report("pairs x destinations x threads", cases.len(), &compare(&cases));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_a_random_sweep_of_every_size_the_plan_draws() {
    if !cascades() {
        return;
    }
    let mut rng = Rng::new(0x5EED);
    let mut cases = Vec::new();
    let mut guard_disagreements = Vec::new();
    let all_kinds = kinds();
    let all_pairs = pairs();
    while cases.len() < 1400 {
        let src = (2 * (3 + rng.below(110)), 2 * (3 + rng.below(110)));
        let dst = (2 * (3 + rng.below(110)), 2 * (3 + rng.below(110)));
        if drawn_by_the_plan(src, dst).is_some() {
            continue;
        }
        // (The plan's own cap on one stage: past 63:1 on an axis the frame is FFmpeg's.)
        let past = |a: u32, b: u32| u64::from(a) > 63 * u64::from(b);
        if past(src.0, dst.0) || past(src.1, dst.1) {
            continue;
        }
        let n = cases.len();
        let kind = all_kinds[rng.below(all_kinds.len() as u32) as usize];
        let pair = all_pairs[rng.below(6) as usize];
        let case = Case::new(src, dst, kind, pair, n);
        // A size the plan draws that the passes would not: the plan and the compositor disagree.
        let frame = picture(Kind::Noise, src.0, src.1, 1);
        if let Err(e) = compositor().scale_convert_yuv(&frame, dst, pair.0, pair.1) {
            guard_disagreements.push(format!("{src:?} -> {dst:?}: {e}"));
            continue;
        }
        cases.push(case);
    }
    eprintln!("sizes the plan draws and the passes refuse: {}", guard_disagreements.len());
    let mut failures = compare(&cases);
    failures.extend(
        guard_disagreements
            .into_iter()
            .map(|d| format!("plan and passes disagree: {d}")),
    );
    report("random sizes", cases.len(), &failures);
}

/// The plan's guard against the model's, exhaustively and without FFmpeg: no even size the plan draws is one the passes cannot.
#[test]
#[ignore = "slow in a debug build: every even axis pair up to 400 samples"]
fn checker_mx_the_plans_guard_never_draws_a_size_the_tables_cannot_follow() {
    use kerf_gpu::sws::{self, Scaler};
    let table = |src: u32, dst: u32, one: i32, align: usize| sws::table_aligned(Scaler::Bicubic, src, dst, one, align);
    let (ah, av) = (sws::SIMD_ALIGN_HORIZONTAL, sws::SIMD_ALIGN_VERTICAL);
    let past = |a: u32, b: u32| u64::from(a) > 63 * u64::from(b);
    // The model follows an axis pair when its tables exist; the vertical luma table must have more than two taps.
    let horizontal_ok =
        |s: u32, d: u32| table(s, d, sws::ONE_HORIZONTAL, ah).is_some() && table(s / 2, d / 2, sws::ONE_HORIZONTAL, ah).is_some();
    let vertical_ok = |s: u32, d: u32| {
        table(s, d, sws::ONE_VERTICAL, av).is_some_and(|t| t.size > 2) && table(s / 2, d, sws::ONE_VERTICAL, av).is_some()
    };
    let sizes: Vec<u32> = (3..=200).map(|n| n * 2).collect();
    let mut bad_h = Vec::new();
    let mut bad_v = Vec::new();
    for &s in &sizes {
        for &d in &sizes {
            if past(s, d) {
                continue;
            }
            if !horizontal_ok(s, d) {
                bad_h.push((s, d));
            }
            if d > 4 && s != d && !vertical_ok(s, d) {
                bad_v.push((s, d));
            }
        }
    }
    eprintln!(
        "axis pairs the tables cannot follow (<= 63:1): horizontal {}, vertical {}",
        bad_h.len(),
        bad_v.len()
    );
    // For each such pair, the plan must refuse every picture that uses it (it can only through the area, which couples the axes).
    for &(s, d) in bad_h.iter().take(50) {
        let drawn = sizes
            .iter()
            .flat_map(|&sh| sizes.iter().map(move |&dh| (sh, dh)))
            .filter(|&(sh, dh)| !past(sh, dh))
            .any(|(sh, dh)| drawn_by_the_plan((s, sh), (d, dh)).is_none());
        assert!(
            !drawn,
            "horizontal {s} -> {d}: the tables cannot follow it and the plan draws a picture that uses it"
        );
    }
    for &(s, d) in bad_v.iter().take(50) {
        let drawn = sizes
            .iter()
            .flat_map(|&sw| sizes.iter().map(move |&dw| (sw, dw)))
            .filter(|&(sw, dw)| !past(sw, dw))
            .any(|(sw, dw)| drawn_by_the_plan((sw, s), (dw, d)).is_none());
        assert!(
            !drawn,
            "vertical {s} -> {d}: the tables cannot follow it and the plan draws a picture that uses it"
        );
    }
}

// ---- the C rows ------------------------------------------------------------------------------------

/// The pictures that decide the last two rows: black and white edges (the vertical sum overshoots a byte and the C function indexes its
/// tables with the unclipped luma), at each height around the switch.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_the_c_rows_at_the_foot_of_every_height_are_ffmpegs() {
    if !cascades() {
        return;
    }
    let mut cases = Vec::new();
    let mut n = 0;
    for dh in [6u32, 8, 10, 12, 14, 16, 18, 22, 30, 46] {
        for sh in [dh + 2, dh + 4, dh * 2, dh * 2 + 2, dh * 3, dh * 5, dh * 9] {
            if sh > 600 {
                continue;
            }
            for (sw, dw) in [(48u32, 32u32), (66, 64), (100, 24)] {
                if drawn_by_the_plan((sw, sh), (dw, dh)).is_some() {
                    continue;
                }
                for kind in [Kind::EdgeRows, Kind::StripesH(1), Kind::Edges, Kind::Saturated] {
                    let mut c = Case::new((sw, sh), (dw, dh), kind, pairs()[n % 6], n);
                    c.threads = [1, 16, 4][n % 3];
                    cases.push(c);
                    n += 1;
                }
            }
        }
    }
    report("rows at the foot", cases.len(), &compare(&cases));
}

/// Which plane's vertical sum a tie is searched in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sum {
    Luma,
    U,
    V,
}

/// An input whose sum in the C rows (the last two) is `2^19 - 1` modulo `2^19` at some pixel: one less than that and the sum `>> 19` is a
/// level lower, so a rounding constant that is off by one in the C function (`(ya + 1) >> 19`, for luma or for either chroma plane) moves
/// that pixel. Found by search over the model's own tables, for a height whose last rows' weights are odd (a 2:1 shrink has all-even ones,
/// and so an even sum, always).
fn a_picture_with_a_tie_in_the_c_rows(sw: u32, sh: u32, dw: u32, dh: u32, which: Sum) -> Option<(YuvFrame, usize, usize, u64)> {
    use kerf_gpu::sws::{self, Scaler};
    let (scw, sch, dcw) = (sw / 2, sh / 2, dw / 2);
    let (ah, av) = (sws::SIMD_ALIGN_HORIZONTAL, sws::SIMD_ALIGN_VERTICAL);
    let (hs, hd, vs) = if which == Sum::Luma { (sw, dw, sh) } else { (scw, dcw, sch) };
    let hl = sws::table_aligned(Scaler::Bicubic, hs, hd, sws::ONE_HORIZONTAL, ah).unwrap();
    let vl = sws::table_aligned(Scaler::Bicubic, vs, dh, sws::ONE_VERTICAL, av).unwrap();
    let mut rng = Rng::new(0xC0DE);
    let mut base = picture(Kind::Noise, sw, sh, 21);
    // Mid values in the planes that are not searched, so a level of the searched one is a level of every channel and nothing is clipped.
    base.y.fill(128);
    base.u.fill(128);
    base.v.fill(128);
    let (pw, ph) = if which == Sum::Luma { (sw, sh) } else { (scw, sch) };
    for tries in 1..=600_000u64 {
        let plane = match which {
            Sum::Luma => &mut base.y,
            Sum::U => &mut base.u,
            Sum::V => &mut base.v,
        };
        // (Only the rows the last two output rows read.)
        for out_row in [dh - 2, dh - 1] {
            for j in 0..vl.size {
                let r = (vl.pos[out_row as usize] as usize + j).min(ph as usize - 1);
                for i in 0..pw {
                    plane[(r as u32 * pw + i) as usize] = 40 + rng.below(160) as u8;
                }
            }
        }
        for out_row in [dh - 2, dh - 1] {
            let o = out_row as usize;
            for x in 0..hd as usize {
                let mut ya: i64 = 1 << 18;
                for j in 0..vl.size {
                    let r = (vl.pos[o] as usize + j).min(ph as usize - 1);
                    let acc: i64 = (0..hl.size)
                        .map(|t| {
                            let col = (hl.pos[x] as usize + t).min(pw as usize - 1);
                            i64::from(hl.coeff[x * hl.size + t]) * i64::from(plane[r * pw as usize + col])
                        })
                        .sum();
                    ya += i64::from(vl.coeff[o * vl.size + j]) * (acc >> 7).min(32767);
                }
                if (ya & 0x7_FFFF) == 0x7_FFFF {
                    let at = if which == Sum::Luma { x } else { 2 * x };
                    return Some((base, at, o, tries));
                }
            }
        }
    }
    None
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_a_sum_one_short_of_a_level_in_the_c_rows_is_ffmpegs() {
    if !cascades() {
        return;
    }
    let mut failures = Vec::new();
    let mut held = 0;
    for which in [Sum::Luma, Sum::U, Sum::V] {
        for (sw, sh, dw, dh) in [
            (64u32, 64u32, 32u32, 48u32),
            (64, 100, 32, 60),
            (48, 64, 40, 48),
            (64, 96, 48, 64),
        ] {
            let Some((frame, x, row, tries)) = a_picture_with_a_tie_in_the_c_rows(sw, sh, dw, dh, which) else {
                failures.push(format!(
                    "{which:?} {sw}x{sh} -> {dw}x{dh}: no sum of 2^19 - 1 in the last rows within the search"
                ));
                continue;
            };
            eprintln!("{which:?} {sw}x{sh} -> {dw}x{dh}: a sum of 2^19 - 1 at ({x}, {row}) after {tries} pictures");
            for (n, pair) in pairs().into_iter().enumerate() {
                for threads in [1, 16] {
                    let mut case = Case::new((sw, sh), (dw, dh), Kind::Noise, pair, n);
                    case.threads = threads;
                    case.pix_fmt = "yuv420p";
                    case.from_tag = tags(pair.0)[0];
                    case.to_tag = tags(pair.1)[0];
                    let ffmpeg = case.ffmpeg(&frame);
                    let size = (dw * dh) as usize;
                    let (fy, rest) = ffmpeg.split_at(size);
                    let (fu, fv) = rest.split_at(size / 4);
                    let fv = &fv[..size / 4];
                    let gpu = compositor()
                        .scale_convert_yuv(&frame, case.dst, case.from, case.to)
                        .expect("the passes");
                    let name = format!("{which:?} {}", case.name());
                    match describe(&name, planes(&gpu), [fy, fu, fv], case.dst) {
                        Some(d) => failures.push(format!("GPU {d}")),
                        None => held += 1,
                    }
                    // ... and the scalar model the probe and the tests follow, against FFmpeg on the same picture.
                    let pic = Pic {
                        planes: planes(&frame),
                        size: (sw as usize, sh as usize),
                    };
                    let model =
                        kerf_gpu::convert::scale_and_change_matrix(pic, (dw as usize, dh as usize), pair.0, pair.1, Packed::Mmx)
                            .expect("a size the model follows");
                    match describe(&name, [&model[0], &model[1], &model[2]], [fy, fu, fv], case.dst) {
                        Some(d) => failures.push(format!("model {d}")),
                        None => held += 1,
                    }
                }
            }
        }
    }
    eprintln!("{held} comparisons of pictures with a tie in the C rows (GPU and model) are FFmpeg's");
    report("a sum one short of a level", held + failures.len(), &failures);
}

// ---- through the export's own chains ---------------------------------------------------------------

fn scratch() -> PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-mx-chain");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `frame` as a one-frame lossless file tagged `tag` (`None`: untagged), limited range.
fn make_file(frame: &YuvFrame, tag: Option<&str>, name: &str) -> PathBuf {
    let path = scratch().join(name);
    let mut input = frame.y.clone();
    input.extend(&frame.u);
    input.extend(&frame.v);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
    ])
    .arg(format!("{}x{}", frame.width, frame.height))
    .args([
        "-framerate",
        "1",
        "-i",
        "-",
        "-frames:v",
        "1",
        "-c:v",
        "ffv1",
        "-pix_fmt",
        "yuv420p",
        "-color_range",
        "tv",
    ]);
    // (The tag is set on the frames with `setparams`: `-colorspace` on the output would have FFmpeg convert the picture into that matrix.)
    cmd.args([
        "-vf",
        &match tag {
            Some(tag) => format!("setparams=colorspace={tag}:range=tv"),
            None => "setparams=range=tv".to_string(),
        },
    ]);
    cmd.arg("-y")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    path
}

/// The export's chains for a decider and a resized clip, over a canvas: `plane` is a dissolving clip's `fps`, `format=yuva420p`.
fn chain_graph(canvas: (u32, u32), picture_size: (u32, u32), at: (u32, u32), plane: bool, sar_first: bool) -> String {
    let (cw, ch) = canvas;
    let tail = if plane { ",fps=1,format=yuva420p" } else { "" };
    let fit = format!(
        "scale={}:{},{}pad={cw}:{ch}:{}:{},setsar=1",
        picture_size.0,
        picture_size.1,
        if sar_first { "setsar=1," } else { "" },
        at.0,
        at.1
    );
    format!(
        "color=c=black:s={}x{ch}:r=1:d=1,format=yuv420p[vbase];[0:v]{fit}{tail}[v0];[vbase][v0]overlay=x=0:y=0[o0];[1:v]{fit}{tail}[v1];[o0][v1]overlay=x={cw}:y=0",
        2 * cw
    )
}

fn run_chain(deciders: &PathBuf, resized: &PathBuf, graph: &str, canvas: (u32, u32), threads: u32) -> Vec<u8> {
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-filter_threads"])
        .arg(threads.to_string())
        .arg("-i")
        .arg(deciders)
        .arg("-i")
        .arg(resized)
        .args([
            "-filter_complex",
            &format!("{graph}[o]"),
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().expect("ffmpeg");
    assert!(out.status.success(), "{graph}: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len(), (2 * canvas.0 * canvas.1 * 3 / 2) as usize);
    out.stdout
}

/// The right-hand tile's picture (at `at`, `size`) of the two-tile frame.
fn tile_window(frame: &[u8], canvas: (u32, u32), at: (u32, u32), size: (u32, u32)) -> [Vec<u8>; 3] {
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let (fw, fh) = (2 * cw, ch);
    let (luma, rest) = frame.split_at(fw * fh);
    let (u, v) = rest.split_at(fw * fh / 4);
    let cut = |plane: &[u8], pw: usize, x0: usize, y0: usize, w: usize, h: usize| -> Vec<u8> {
        (y0..y0 + h)
            .flat_map(|r| plane[r * pw + x0..r * pw + x0 + w].iter().copied())
            .collect()
    };
    let (x0, y0, w, h) = (cw + at.0 as usize, at.1 as usize, size.0 as usize, size.1 as usize);
    [
        cut(luma, fw, x0, y0, w, h),
        cut(u, fw / 2, x0 / 2, y0 / 2, w / 2, h / 2),
        cut(v, fw / 2, x0 / 2, y0 / 2, w / 2, h / 2),
    ]
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_a_dissolving_and_a_still_chain_with_a_pad_are_ffmpegs() {
    if !cascades() {
        return;
    }
    // (file size, canvas, the fit's picture size, its offset in the canvas)
    let shapes: &[(Size, Size, Size, Size)] = &[
        ((160, 160), (64, 48), (48, 48), (8, 0)),
        ((96, 192), (64, 48), (24, 48), (20, 0)),
        ((200, 120), (64, 48), (64, 38), (0, 4)),
        ((320, 180), (100, 60), (100, 56), (0, 2)),
        ((100, 300), (80, 60), (20, 60), (30, 0)),
        ((130, 70), (66, 40), (66, 36), (0, 2)),
        ((64, 64), (62, 40), (40, 40), (10, 0)),
        ((120, 120), (50, 50), (50, 50), (0, 0)),
        ((100, 40), (50, 40), (50, 20), (0, 10)),
    ];
    let mut failures = Vec::new();
    let mut n = 0usize;
    for &(file, canvas, picture_size, at) in shapes {
        // Even offsets only (a pad at an odd offset is another matter, the plan's).
        for pair in pairs() {
            for plane in [true, false] {
                for kind in [Kind::Noise, Kind::Edges] {
                    n += 1;
                    let decider = picture(Kind::Noise, canvas.0, canvas.1, 3);
                    let frame = picture(kind, file.0, file.1, 40 + n as u64);
                    let decider_tag = tags(pair.1)[n % tags(pair.1).len()];
                    let resized_tag = tags(pair.0)[n % tags(pair.0).len()];
                    let dpath = make_file(&decider, Some(decider_tag), &format!("d-{n}.mkv"));
                    let rpath = make_file(&frame, Some(resized_tag), &format!("r-{n}.mkv"));
                    let graph = chain_graph(canvas, picture_size, at, plane, n.is_multiple_of(3));
                    let threads = [1, 4, 16][n % 3];
                    let got = run_chain(&dpath, &rpath, &graph, canvas, threads);
                    let want = window_of_gpu(&frame, picture_size, pair);
                    let name = format!(
                        "{file:?} into {canvas:?} as {picture_size:?} at {at:?} {:?}->{:?} {} {kind:?} threads {threads}",
                        pair.0,
                        pair.1,
                        if plane { "dissolving" } else { "still" }
                    );
                    let tile = tile_window(&got, canvas, at, picture_size);
                    match want {
                        Err(e) => failures.push(format!("{name}: the GPU would not draw it: {e}")),
                        Ok(want) => {
                            if let Some(d) = describe(
                                &name,
                                [&tile[0], &tile[1], &tile[2]],
                                [&want.y, &want.u, &want.v],
                                picture_size,
                            ) {
                                failures.push(d);
                            }
                        }
                    }
                    let _ = std::fs::remove_file(&dpath);
                    let _ = std::fs::remove_file(&rpath);
                }
            }
        }
    }
    report("export chains", n, &failures);
}

fn window_of_gpu(frame: &YuvFrame, size: (u32, u32), pair: (YuvMatrix, YuvMatrix)) -> Result<YuvFrame, String> {
    compositor()
        .scale_convert_yuv(frame, size, pair.0, pair.1)
        .map_err(|e| e.to_string())
}

// ---- necessity: what the guards refuse is not FFmpeg's ---------------------------------------------

/// What the cascade would draw if the guards let it (`scaled_to_rgb24` at the destination's size, then `ff_rgb24toyv12`), or `None` where
/// the arithmetic itself cannot follow (an odd side).
fn cascade_anyway(frame: &YuvFrame, dst: (u32, u32), from: YuvMatrix, to: YuvMatrix) -> Option<[Vec<u8>; 3]> {
    let pic = Pic {
        planes: planes(frame),
        size: (frame.width as usize, frame.height as usize),
    };
    let rgb = scaled_to_rgb24(pic, (dst.0 as usize, dst.1 as usize), from, Packed::Mmx)?;
    Some(rgb_to_yuv420_24(&rgb, dst.0 as usize, dst.1 as usize, to, Variant::C))
}

fn ffmpeg_planes(case: &Case, frame: &YuvFrame) -> [Vec<u8>; 3] {
    let got = case.ffmpeg(frame);
    let n = (case.dst.0 * case.dst.1) as usize;
    [
        got[..n].to_vec(),
        got[n..n + n / 4].to_vec(),
        got[n + n / 4..n + n / 2].to_vec(),
    ]
}

/// The refusals of `ScaleStage::cascade_refusal`: for each kind, a set of pictures drawn anyway by the cascade, of which FFmpeg's differ
/// (at least once, and the count of those that are exact is reported: a refusal that is only sometimes needed is still needed).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_the_refusals_are_necessary() {
    if !cascades() {
        return;
    }
    let groups: &[(&str, &[Resize])] = &[
        (
            "does not shrink: equal area",
            &[
                ((96, 32), (64, 48)),
                ((64, 48), (96, 32)),
                ((100, 60), (60, 100)),
                ((128, 72), (96, 96)),
                ((200, 50), (100, 100)),
            ],
        ),
        (
            "does not shrink: grows",
            &[
                ((64, 48), (66, 48)),
                ((64, 48), (64, 50)),
                ((64, 48), (96, 72)),
                ((32, 24), (128, 96)),
                ((60, 40), (64, 40)),
                ((64, 48), (130, 50)),
            ],
        ),
        (
            "luma rows not scaled",
            &[
                ((128, 48), (64, 48)),
                ((100, 60), (50, 60)),
                ((200, 100), (40, 100)),
                ((64, 64), (32, 64)),
            ],
        ),
        (
            "destination too short",
            &[((64, 48), (32, 4)), ((64, 48), (32, 2)), ((200, 100), (100, 4))],
        ),
        (
            "odd destination",
            &[
                ((64, 48), (31, 24)),
                ((64, 48), (32, 23)),
                ((100, 80), (51, 41)),
                ((100, 80), (33, 40)),
            ],
        ),
        (
            "odd source",
            &[
                ((65, 48), (32, 24)),
                ((64, 49), (32, 24)),
                ((101, 81), (50, 40)),
                ((99, 80), (40, 30)),
            ],
        ),
    ];
    let mut report_lines = Vec::new();
    let mut unneeded = Vec::new();
    for (name, sizes) in groups {
        let (mut total, mut differ) = (0, 0);
        for &(src, dst) in *sizes {
            assert!(
                drawn_by_the_plan(src, dst).is_some(),
                "{name}: {src:?} -> {dst:?} is not refused by the plan"
            );
            // (An odd picture has chroma planes of ceil(size / 2): `YuvFrame` rounds down, the picture FFmpeg is given is the same.)
            let even = (src.0 & !1, src.1 & !1);
            if even != src {
                // The decode of an odd picture is outside this model; hold the model's own limit instead.
                continue;
            }
            for (n, kind) in [Kind::Noise, Kind::Edges, Kind::Ramp, Kind::Saturated]
                .into_iter()
                .enumerate()
            {
                let pair = pairs()[(n + src.0 as usize) % 6];
                let mut case = Case::new(src, dst, kind, pair, n);
                case.pix_fmt = "yuv420p";
                if dst.0 % 2 == 1 || dst.1 % 2 == 1 {
                    // FFmpeg's chroma of an odd destination is another size: the model's planes cannot be compared, the cascade refuses it.
                    let frame = case.frame();
                    assert!(
                        cascade_anyway(&frame, dst, pair.0, pair.1).is_none(),
                        "{name}: {src:?} -> {dst:?}"
                    );
                    total += 1;
                    differ += 1;
                    continue;
                }
                let frame = case.frame();
                let want = ffmpeg_planes(&case, &frame);
                total += 1;
                match cascade_anyway(&frame, dst, pair.0, pair.1) {
                    None => differ += 1,
                    Some(got) => {
                        let same = got[0] == want[0] && got[1] == want[1] && got[2] == want[2];
                        if same {
                            unneeded.push(format!("{name}: {src:?} -> {dst:?} {kind:?} {:?}->{:?}", pair.0, pair.1));
                        } else {
                            differ += 1;
                        }
                    }
                }
            }
        }
        report_lines.push(format!("{name}: {differ} of {total} pictures differ when drawn anyway"));
        if *name == "destination too short" {
            // (Measured, not held: the cascade is exact there; see the report.)
            continue;
        }
        assert!(
            total == 0 || differ > 0,
            "{name}: the refusal never mattered in {total} pictures"
        );
    }
    for l in &report_lines {
        eprintln!("{l}");
    }
    eprintln!(
        "exact when drawn anyway (a refusal wider than that picture needed): {}",
        unneeded.len()
    );
    for u in unneeded.iter().take(20) {
        eprintln!("  {u}");
    }
}

/// A Cover fit crops what the last `scale` made. The cascade is at the scale's own size and the crop is a window of it: is the refusal
/// (`keep != whole`) wider than it has to be? Held as a measurement: FFmpeg's `scale ... , crop`, against the cascade at the scale's size
/// cut to the same window.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_a_cover_crop_after_the_cascade_is_the_cascade_cut() {
    if !cascades() {
        return;
    }
    let mut exact = 0;
    let mut differ = 0;
    // (source, the scale's size, the window kept)
    let cases: &[(Size, Size, Rect)] = &[
        (
            (160, 90),
            (96, 54),
            Rect {
                x: 8,
                y: 0,
                w: 80,
                h: 54,
            },
        ),
        (
            (160, 90),
            (96, 54),
            Rect {
                x: 0,
                y: 0,
                w: 96,
                h: 44,
            },
        ),
        (
            (200, 100),
            (100, 50),
            Rect {
                x: 10,
                y: 0,
                w: 60,
                h: 50,
            },
        ),
        (
            (120, 200),
            (60, 100),
            Rect {
                x: 0,
                y: 20,
                w: 60,
                h: 60,
            },
        ),
    ];
    for (n, &(src, scaled, keep)) in cases.iter().enumerate() {
        for (k, pair) in pairs().into_iter().enumerate() {
            let frame = picture(Kind::Noise, src.0, src.1, 70 + (n * 7 + k) as u64);
            let vf = format!(
                "setparams=colorspace={}:range=tv,scale={}:{}:out_color_matrix={},crop={}:{}:{}:{},format=yuv420p",
                tags(pair.0)[0],
                scaled.0,
                scaled.1,
                tags(pair.1)[0],
                keep.w,
                keep.h,
                keep.x,
                keep.y
            );
            let mut input = frame.y.clone();
            input.extend(&frame.u);
            input.extend(&frame.v);
            let mut cmd = kerf_core::ffmpeg_command();
            cmd.args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
            ])
            .arg(format!("{}x{}", src.0, src.1))
            .args([
                "-i",
                "-",
                "-vf",
                &vf,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
            let mut child = cmd.spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let w = std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
            let out = child.wait_with_output().unwrap();
            w.join().unwrap();
            assert!(out.status.success(), "{vf}: {}", String::from_utf8_lossy(&out.stderr));
            let (kw, kh) = (keep.w as usize, keep.h as usize);
            let got = [
                out.stdout[..kw * kh].to_vec(),
                out.stdout[kw * kh..kw * kh * 5 / 4].to_vec(),
                out.stdout[kw * kh * 5 / 4..kw * kh * 3 / 2].to_vec(),
            ];
            let cascade = cascade_anyway(&frame, scaled, pair.0, pair.1).expect("an even shrink");
            let cut = |plane: &[u8], pw: usize, x: usize, y: usize, w: usize, h: usize| -> Vec<u8> {
                (y..y + h)
                    .flat_map(|r| plane[r * pw + x..r * pw + x + w].iter().copied())
                    .collect()
            };
            let (sw, kx, ky) = (scaled.0 as usize, keep.x as usize, keep.y as usize);
            let want = [
                cut(&cascade[0], sw, kx, ky, kw, kh),
                cut(&cascade[1], sw / 2, kx / 2, ky / 2, kw / 2, kh / 2),
                cut(&cascade[2], sw / 2, kx / 2, ky / 2, kw / 2, kh / 2),
            ];
            if got == want {
                exact += 1;
            } else {
                differ += 1;
                eprintln!(
                    "cover {src:?} -> {scaled:?} keep {keep:?} {:?}->{:?}: differs",
                    pair.0, pair.1
                );
            }
        }
    }
    eprintln!("Cover crop after the cascade: {exact} exact, {differ} differ (a refusal wider than it has to be if none differ)");
}

/// A 4:2:2 or 4:4:4 source: the format changes in the same call, so the cascade of the picture reduced to 4:2:0 first (what the compositor's
/// decode gives) is not FFmpeg's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_mx_a_picture_that_is_not_420_is_not_the_cascade_of_its_420() {
    if !cascades() {
        return;
    }
    let mut differ = 0;
    let mut total = 0;
    for (src, dst) in [
        ((128u32, 96u32), (64u32, 48u32)),
        ((200, 120), (100, 80)),
        ((90, 60), (46, 30)),
    ] {
        for pix in ["yuv422p", "yuv444p"] {
            for (k, pair) in pairs().into_iter().enumerate() {
                total += 1;
                let frame420 = picture(Kind::Noise, src.0, src.1, 9 + k as u64);
                // The 4:2:2 / 4:4:4 file FFmpeg is given: the 4:2:0 picture's planes, chroma replicated up, so that reducing it back to
                // 4:2:0 loses nothing and any difference is the cascade's own.
                let (cw, ch) = (src.0 / 2, src.1 / 2);
                let (xs, ys) = if pix == "yuv444p" { (2, 2) } else { (2, 1) };
                let up = |p: &[u8]| -> Vec<u8> {
                    let (ow, oh) = (cw * xs, ch * ys);
                    (0..oh)
                        .flat_map(|j| (0..ow).map(move |i| (i, j)))
                        .map(|(i, j)| p[((j / ys) * cw + i / xs) as usize])
                        .collect()
                };
                let mut input = frame420.y.clone();
                input.extend(up(&frame420.u));
                input.extend(up(&frame420.v));
                let vf = format!(
                    "setparams=colorspace={}:range=tv,scale={}:{}:out_color_matrix={},format=yuv420p",
                    tags(pair.0)[0],
                    dst.0,
                    dst.1,
                    tags(pair.1)[0]
                );
                let mut cmd = kerf_core::ffmpeg_command();
                cmd.args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    pix,
                    "-s",
                ])
                .arg(format!("{}x{}", src.0, src.1))
                .args([
                    "-i",
                    "-",
                    "-vf",
                    &vf,
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
                let mut child = cmd.spawn().unwrap();
                let mut stdin = child.stdin.take().unwrap();
                let w = std::thread::spawn(move || {
                    let _ = stdin.write_all(&input);
                });
                let out = child.wait_with_output().unwrap();
                w.join().unwrap();
                assert!(out.status.success(), "{vf}: {}", String::from_utf8_lossy(&out.stderr));
                let n = (dst.0 * dst.1) as usize;
                let got = [
                    out.stdout[..n].to_vec(),
                    out.stdout[n..n * 5 / 4].to_vec(),
                    out.stdout[n * 5 / 4..n * 3 / 2].to_vec(),
                ];
                let want = cascade_anyway(&frame420, dst, pair.0, pair.1).expect("an even shrink");
                if got != want {
                    differ += 1;
                }
            }
        }
    }
    eprintln!("a 4:2:2 / 4:4:4 source: {differ} of {total} pictures differ from the cascade of their 4:2:0 reduction");
    assert!(differ > 0, "the refusal of a picture that is not 4:2:0 never mattered");
}
