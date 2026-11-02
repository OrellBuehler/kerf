//! `checker/e/m`: the end of the plan's window for a quarter turn, to the last bit, against FFmpeg's own `rotate` (no adapter). The filter takes its
//! `simple_rotate` fast path for `fabs(angle - target) < FLT_EPSILON` (the float, 1.1920928955078125e-7, promoted to double) at 0, π/2, π and 3π/2,
//! and its integer path elsewhere; the plan's window ends where the fast path's does. For the doubles three steps either side of `target ± FLT_EPSILON`
//! at each target, on pictures of odd and even sides with a different value in every sample, the output of the export's own `rotate` text is compared
//! with the exact quarter turn: every angle the plan accepts is exact (the fast path copies), and the ones it refuses are printed (`WIDER`) when the
//! integer path turns them exactly too. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_e_m_edge -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

struct Lcg(u64);

impl Lcg {
    fn byte(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u8
    }
}

/// A `yuva420p` picture: Y, U, V, A planes with their sizes.
struct Pic {
    dims: [(usize, usize); 4],
    planes: [Vec<u8>; 4],
}

impl Pic {
    fn noise(w: usize, h: usize, seed: u64) -> Self {
        let mut rng = Lcg(seed);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let dims = [(w, h), (cw, ch), (cw, ch), (w, h)];
        let planes = dims.map(|(pw, ph)| (0..pw * ph).map(|_| rng.byte()).collect());
        Self { dims, planes }
    }

    fn raw(&self) -> Vec<u8> {
        self.planes.iter().flatten().copied().collect()
    }

    /// The picture turned `k` quarters clockwise (what `rotate` lands on whole samples as), plane by plane.
    fn turned(&self, k: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for (plane, &(pw, ph)) in self.planes.iter().zip(&self.dims) {
            let at = |x: usize, y: usize| plane[y * pw + x];
            match k % 4 {
                0 => out.extend_from_slice(plane),
                1 => (0..pw).for_each(|y| (0..ph).for_each(|x| out.push(at(y, ph - 1 - x)))),
                2 => (0..ph).for_each(|y| (0..pw).for_each(|x| out.push(at(pw - 1 - x, ph - 1 - y)))),
                _ => (0..pw).for_each(|y| (0..ph).for_each(|x| out.push(at(pw - 1 - y, x)))),
            }
        }
        out
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-e-turn-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The export's `rotate` for a constant angle in radians (`static_rotate`'s text).
fn rotate_text(rad: f64, fill: &str) -> String {
    format!("rotate={rad}:fillcolor={fill}:ow=rotw({rad}):oh=roth({rad})")
}

/// FFmpeg's `rotate` of `pic` for each angle, one process per batch: the output bytes of each (empty when it failed).
fn rotate_all(dir: &Path, pic: &Pic, rads: &[f64], fill: &str) -> Vec<Vec<u8>> {
    let (w, h) = pic.dims[0];
    let input = dir.join("in.raw");
    std::fs::write(&input, pic.raw()).unwrap();
    let mut results = Vec::new();
    for (b, batch) in rads.chunks(48).enumerate() {
        let n = batch.len();
        let splits: String = (0..n).map(|i| format!("[s{i}]")).collect();
        let mut graph = format!("[0:v]split={n}{splits}");
        for (i, rad) in batch.iter().enumerate() {
            graph.push_str(&format!(";[s{i}]{}[o{i}]", rotate_text(*rad, fill)));
        }
        let mut cmd = kerf_core::ffmpeg_command();
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuva420p",
        ])
        .args(["-s", &format!("{w}x{h}"), "-framerate", "1", "-i"])
        .arg(&input)
        .args(["-filter_complex_threads", "1"])
        .args(["-filter_complex", &graph]);
        let outs: Vec<PathBuf> = (0..n).map(|i| dir.join(format!("out-{b}-{i}.raw"))).collect();
        for (i, out) in outs.iter().enumerate() {
            cmd.args([
                "-map",
                &format!("[o{i}]"),
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
            ])
            .arg(out);
        }
        let run = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).output().expect("run ffmpeg");
        if !run.status.success() {
            eprintln!("batch {b} failed: {}", String::from_utf8_lossy(&run.stderr).trim());
        }
        for out in &outs {
            results.push(std::fs::read(out).unwrap_or_default());
            let _ = std::fs::remove_file(out);
        }
    }
    results
}

fn step(x: f64, n: i64) -> f64 {
    let bits = x.to_bits() as i64;
    f64::from_bits(if x.is_sign_negative() { bits - n } else { bits + n } as u64)
}

fn pictures() -> Vec<Pic> {
    vec![
        Pic::noise(24, 16, 11),
        Pic::noise(23, 15, 12),
        Pic::noise(17, 30, 13),
        Pic::noise(2, 3, 14),
    ]
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_e_m_the_window_ends_where_the_fast_path_does_and_the_filter_is_exact_up_to_it() {
    let dir = scratch("edge");
    let pi = std::f64::consts::PI;
    let eps = f64::from(f32::EPSILON);
    let mut rads = Vec::new();
    let mut turns = Vec::new();
    let mut names = Vec::new();
    for (target, turn) in [(0.0, 0usize), (pi / 2.0, 1), (pi, 2), (3.0 * pi / 2.0, 3)] {
        for sign in [1.0, -1.0] {
            let edge = target + sign * eps;
            for n in -3..=3 {
                let a = step(edge, n);
                rads.push(a);
                turns.push(turn);
                names.push(format!("target {turn} {:+e} rad from the edge {:+} ulps", a - target, n));
            }
        }
    }
    let pics = pictures();
    let mut exact = vec![true; rads.len()];
    for pic in &pics {
        for fill in ["none", "black@0"] {
            let got = rotate_all(&dir, pic, &rads, fill);
            for (i, out) in got.iter().enumerate() {
                if out.is_empty() || *out != pic.turned(turns[i]) {
                    exact[i] = false;
                }
            }
        }
    }
    let (mut inside, mut wider, mut bad) = (0, 0, Vec::new());
    for (i, name) in names.iter().enumerate() {
        let plan = kerf_core::quarter_turn(rads[i]);
        let fast = (rads[i] - [0.0, pi / 2.0, pi, 3.0 * pi / 2.0][turns[i]]).abs() < eps;
        if plan.is_some() != fast {
            bad.push(format!("{name}: the plan says {plan:?}, the fast path {fast}"));
        }
        if plan.is_some() {
            inside += 1;
            if !exact[i] {
                bad.push(format!(
                    "{name} ({:?} rad): accepted and FFmpeg's rotate is not the exact turn",
                    rads[i]
                ));
            }
        } else if exact[i] {
            wider += 1;
            eprintln!("WIDER {name}: refused, and FFmpeg turns it exactly");
        }
    }
    eprintln!(
        "{} angles at the four edges, {inside} accepted, {wider} refused though exact",
        rads.len()
    );
    assert!(inside >= 24 && inside < rads.len(), "{inside} accepted of {}", rads.len());
    assert!(bad.is_empty(), "{bad:#?}");
}
