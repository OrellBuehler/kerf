//! Work package E1 (blend modes), checked independently of the implementer's cases: the blend chain's arithmetic and the
//! compositor's blend passes against FFmpeg on pictures, sizes, positions, alphas and thread counts the implementer did not use.
//!
//! The chain the export writes (`kerf_core::blend_probe_graph`) is run by the real `ffmpeg` on noise, with the clip placed by the
//! export's own `overlay` at positions on and off every edge (odd ones included: `overlay` floors them to even), clips of odd sizes,
//! frames of sizes that are no multiple of anything, and `-filter_threads` from 1 to 13. The same graph's first half is run on its
//! own to get FFmpeg's `[cc]` (the clip laid opaque on the composite) and `[aa]` (its alpha plane): the model
//! (`kerf_gpu::blend::reference`) and the compositor's passes (`Compositor::blend_planes`) are then given exactly those and must
//! return the chain's output to the sample. No tolerance anywhere.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_e_blend -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::{BlendMode, YuvMatrix};
use kerf_gpu::blend::{reference, Planes};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

const MODES: [BlendMode; 9] = [
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::HardLight,
    BlendMode::Add,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::Difference,
    BlendMode::Exclusion,
];

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| Compositor::new(gpu()).expect("a compositor"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-e-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One `ffmpeg` run over raw inputs `(pix_fmt, size, path)`; the filter graph's outputs are `(label, pix_fmt, path)`.
fn ffmpeg_graph(
    threads: usize,
    inputs: &[(&str, (usize, usize), &Path)],
    graph: &str,
    outputs: &[(&str, &str, &Path)],
) -> Result<(), String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]
        .map(String::from)
        .to_vec();
    args.extend(["-threads", "1", "-filter_threads"].map(String::from));
    args.push(threads.to_string());
    args.extend(["-filter_complex_threads".to_string(), threads.to_string()]);
    for (pix, (w, h), path) in inputs {
        args.extend(["-f", "rawvideo", "-pix_fmt", pix, "-s", &format!("{w}x{h}"), "-i"].map(String::from));
        args.push(path.to_string_lossy().into_owned());
    }
    args.extend(["-filter_complex".to_string(), graph.to_string()]);
    for (label, pix, path) in outputs {
        args.extend(["-map", label, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", pix].map(String::from));
        args.push(path.to_string_lossy().into_owned());
    }
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn planes_of(bytes: &[u8], w: usize, h: usize) -> Planes {
    Planes::from_frame(bytes, w, h).unwrap_or_else(|| panic!("{} bytes for {w}x{h}", bytes.len()))
}

#[derive(Clone, Copy, Debug)]
enum AlphaKind {
    Noise,
    Opaque,
    Clear,
    Ramp,
    Edges,
    Binary,
}

fn alpha_plane(rng: &mut Lcg, kind: AlphaKind, w: usize, h: usize) -> Vec<u8> {
    match kind {
        AlphaKind::Noise => rng
            .bytes(w * h)
            .into_iter()
            .enumerate()
            .map(|(i, a)| match i % 7 {
                0 => 0,
                1 => 255,
                _ => a,
            })
            .collect(),
        AlphaKind::Opaque => vec![255; w * h],
        AlphaKind::Clear => vec![0; w * h],
        AlphaKind::Ramp => (0..w * h).map(|i| ((i % w) * 255 / w.max(1)) as u8).collect(),
        // Values next to the ones `overlay` short-cuts and next to the rounding of its `>> 2` average.
        AlphaKind::Edges => (0..w * h)
            .map(|i| [1u8, 254, 128, 127, 3, 252, 85, 170][(i * 5 + i / w) % 8])
            .collect(),
        AlphaKind::Binary => (0..w * h)
            .map(|i| if (i % w + i / w).is_multiple_of(3) { 0 } else { 255 })
            .collect(),
    }
}

struct Scenario {
    base: (usize, usize),
    clip: (usize, usize),
    pos: (i32, i32),
    mode: BlendMode,
    tag: Option<&'static str>,
    matrix: YuvMatrix,
    threads: usize,
    alpha: AlphaKind,
}

impl Scenario {
    fn label(&self) -> String {
        format!(
            "{:?} {:?} base {}x{} clip {}x{} at {:?} threads {} alpha {:?}",
            self.mode, self.tag, self.base.0, self.base.1, self.clip.0, self.clip.1, self.pos, self.threads, self.alpha
        )
    }
}

#[derive(Default)]
struct Tally {
    cases: usize,
    failures: Vec<String>,
}

fn diff(plane: &str, got: &[u8], want: &[u8], w: usize) -> Option<String> {
    let bad: Vec<usize> = (0..want.len()).filter(|&i| got[i] != want[i]).collect();
    let first = *bad.first()?;
    let max = bad
        .iter()
        .map(|&i| (i32::from(got[i]) - i32::from(want[i])).abs())
        .max()
        .unwrap_or(0);
    Some(format!(
        "{plane}: {} differ, max {max}, first at ({}, {})",
        bad.len(),
        first % w.max(1),
        first / w.max(1)
    ))
}

fn graphs(s: &Scenario) -> (String, String) {
    let tagged = s
        .tag
        .map_or_else(String::new, |t| format!("setparams=colorspace={t}:range=tv,"));
    let chain = kerf_core::blend_probe_graph(s.mode, s.pos, s.tag).expect("a mode");
    let whole = format!("[0:v]{tagged}format=yuv420p[base];[1:v]{tagged}format=yuva420p[clip];{chain}");
    // The first half of the same text, up to `alphamerge`: the clip laid opaque on the composite and its alpha plane.
    let mut half: Vec<&str> = Vec::new();
    for c in chain.split(';') {
        if c.contains("alphamerge") {
            break;
        }
        half.push(c);
    }
    let first = format!(
        "[0:v]{tagged}format=yuv420p[base];[1:v]{tagged}format=yuva420p[clip];{};[bldpb]nullsink;[bldpm]nullsink",
        half.join(";")
    );
    (whole, first)
}

/// One scenario: FFmpeg's chain against the model and the compositor's passes, both given FFmpeg's `cc` and `aa`.
fn check(rng: &mut Lcg, s: &Scenario, dir: &Path, tally: &mut Tally, with_gpu: bool) {
    tally.cases += 1;
    let (bw, bh) = s.base;
    let (cw, ch) = s.clip;
    let base = Planes {
        w: bw,
        h: bh,
        y: rng.bytes(bw * bh),
        u: rng.bytes(bw * bh / 4),
        v: rng.bytes(bw * bh / 4),
    };
    let ccw = cw.div_ceil(2);
    let cch = ch.div_ceil(2);
    let clip_alpha = alpha_plane(rng, s.alpha, cw, ch);
    let clip_bytes = [rng.bytes(cw * ch), rng.bytes(ccw * cch), rng.bytes(ccw * cch), clip_alpha].concat();
    let (base_path, clip_path) = (dir.join("base.yuv"), dir.join("clip.yuva"));
    std::fs::write(&base_path, base.to_frame()).unwrap();
    std::fs::write(&clip_path, &clip_bytes).unwrap();
    let inputs = [
        ("yuv420p", (bw, bh), base_path.as_path()),
        ("yuva420p", (cw, ch), clip_path.as_path()),
    ];
    let (whole, first) = graphs(s);
    let (out_path, cc_path, aa_path) = (dir.join("out.yuv"), dir.join("cc.yuv"), dir.join("aa.gray"));
    let fail = |why: String| format!("{}: {why}", s.label());
    if let Err(e) = ffmpeg_graph(s.threads, &inputs, &whole, &[("[out]", "yuv420p", &out_path)]) {
        tally.failures.push(fail(format!("ffmpeg refused the chain: {}", e.trim())));
        return;
    }
    if let Err(e) = ffmpeg_graph(
        s.threads,
        &inputs,
        &first,
        &[("[bldpcc]", "yuv420p", &cc_path), ("[bldpaa]", "gray", &aa_path)],
    ) {
        tally
            .failures
            .push(fail(format!("ffmpeg refused the first half: {}", e.trim())));
        return;
    }
    let out = planes_of(&std::fs::read(&out_path).unwrap(), bw, bh);
    let cc = planes_of(&std::fs::read(&cc_path).unwrap(), bw, bh);
    let aa = std::fs::read(&aa_path).unwrap();
    assert_eq!(aa.len(), bw * bh);
    let want = reference(s.mode, s.matrix, &base, &cc, &aa);
    let mut why = Vec::new();
    for (plane, got, exp, w) in [
        ("model Y", &want.y, &out.y, bw),
        ("model U", &want.u, &out.u, bw / 2),
        ("model V", &want.v, &out.v, bw / 2),
    ] {
        if let Some(d) = diff(plane, got, exp, w) {
            why.push(d);
        }
    }
    if with_gpu {
        let gpu = compositor()
            .blend_planes(s.mode, s.matrix, &base, &cc, &aa)
            .expect("the passes");
        for (plane, got, exp, w) in [
            ("gpu Y", &gpu.y, &out.y, bw),
            ("gpu U", &gpu.u, &out.u, bw / 2),
            ("gpu V", &gpu.v, &out.v, bw / 2),
        ] {
            if let Some(d) = diff(plane, got, exp, w) {
                why.push(d);
            }
        }
    }
    if !why.is_empty() {
        tally.failures.push(fail(why.join("; ")));
    }
}

fn report(name: &str, tally: Tally) {
    eprintln!("{name}: {} scenarios, {} failing", tally.cases, tally.failures.len());
    for f in tally.failures.iter().take(25) {
        eprintln!("  FAIL {f}");
    }
    assert!(
        tally.failures.is_empty(),
        "{name}: {} of {} scenarios differ",
        tally.failures.len(),
        tally.cases
    );
}

const TAGS: [(Option<&str>, YuvMatrix); 5] = [
    (None, YuvMatrix::Bt601),
    (Some("bt709"), YuvMatrix::Bt709),
    (Some("smpte170m"), YuvMatrix::Bt601),
    (Some("bt470bg"), YuvMatrix::Bt601),
    (Some("bt2020nc"), YuvMatrix::Bt2020),
];

/// Every mode, both matrices, a clip of any size at any position (on and off every edge, odd positions that `overlay` floors),
/// on frames of sizes that are no multiple of a vector, with the alpha plane of every kind and 1 to 13 filter threads.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_the_chain_is_the_model_and_the_passes_for_random_sizes_positions_alphas_and_threads() {
    let dir = scratch("random");
    let mut rng = Lcg(0xe1e1_b1e4_d001);
    let mut tally = Tally::default();
    let sizes = [2usize, 4, 6, 8, 10, 16, 18, 30, 34, 50, 66, 98, 130, 162, 226, 322];
    let alphas = [
        AlphaKind::Noise,
        AlphaKind::Opaque,
        AlphaKind::Ramp,
        AlphaKind::Edges,
        AlphaKind::Binary,
        AlphaKind::Clear,
    ];
    let threads = [1usize, 2, 3, 5, 8, 13];
    for round in 0..270 {
        let (bw, bh) = (
            sizes[rng.below(sizes.len() as u64) as usize],
            sizes[rng.below(sizes.len() as u64) as usize],
        );
        // A clip as large as the frame or larger, as small as one sample, odd or even.
        let cw = 1 + rng.below(bw as u64 + 6) as usize;
        let ch = 1 + rng.below(bh as u64 + 6) as usize;
        let x = rng.below((bw + cw + 4) as u64) as i32 - cw as i32 - 2;
        let y = rng.below((bh + ch + 4) as u64) as i32 - ch as i32 - 2;
        let (tag, matrix) = TAGS[rng.below(TAGS.len() as u64) as usize];
        let s = Scenario {
            base: (bw, bh),
            clip: (cw, ch),
            pos: (x, y),
            mode: MODES[round % MODES.len()],
            tag,
            matrix,
            threads: threads[rng.below(threads.len() as u64) as usize],
            alpha: alphas[rng.below(alphas.len() as u64) as usize],
        };
        check(&mut rng, &s, &dir, &mut tally, true);
    }
    let _ = std::fs::remove_dir_all(&dir);
    report("random chain scenarios", tally);
}

/// The same clip at the same place, filter threads 1 to 16 and a frame height at which a slice boundary would fall on an odd row:
/// the picture must not depend on how many threads the export has.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_the_chain_does_not_depend_on_the_filter_thread_count() {
    let dir = scratch("threads");
    let mut rng = Lcg(0x7117_ead5);
    let mut tally = Tally::default();
    let mut round = 0;
    for &(bw, bh) in &[(64usize, 34usize), (66, 50), (98, 66), (130, 98), (64, 162), (320, 182)] {
        for threads in [1usize, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 15, 16] {
            let mode = MODES[round % MODES.len()];
            round += 1;
            let (tag, matrix) = TAGS[[0, 1, 4][round % 3]];
            let s = Scenario {
                base: (bw, bh),
                clip: (bw - 2, bh - 2),
                pos: (1, 3),
                mode,
                tag,
                matrix,
                threads,
                alpha: AlphaKind::Noise,
            };
            check(&mut rng, &s, &dir, &mut tally, round % 3 == 0);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    report("thread counts", tally);
}

/// Clips as large as the frame (the case that is drawn: a layer at its own size) at every edge offset of a small frame, every mode:
/// all sixty-odd placements of a 2x2-block frame, which is where the blocks a clip touches are fewest.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_every_placement_of_a_clip_on_a_tiny_frame() {
    let dir = scratch("tiny");
    let mut rng = Lcg(0x7117_0001);
    let mut tally = Tally::default();
    let mut round = 0;
    for &(bw, bh) in &[(2usize, 2usize), (4, 4), (6, 4), (4, 6)] {
        for &(cw, ch) in &[(bw, bh), (1, 1), (3, 3), (bw + 2, bh + 2), (bw - 1, bh)] {
            for x in -(cw as i32) - 1..=bw as i32 + 1 {
                for y in -(ch as i32) - 1..=bh as i32 + 1 {
                    // Three modes and two matrices rotating through the placements keeps the count down.
                    let mode = MODES[round % MODES.len()];
                    let (tag, matrix) = TAGS[[0, 1, 4][(round / 7) % 3]];
                    round += 1;
                    if (x + y).rem_euclid(3) != 0 && round % 4 != 0 {
                        continue;
                    }
                    let s = Scenario {
                        base: (bw, bh),
                        clip: (cw, ch),
                        pos: (x, y),
                        mode,
                        tag,
                        matrix,
                        threads: 1 + round % 4,
                        alpha: if round % 2 == 0 { AlphaKind::Noise } else { AlphaKind::Edges },
                    };
                    check(&mut rng, &s, &dir, &mut tally, true);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    report("tiny frames", tally);
}
