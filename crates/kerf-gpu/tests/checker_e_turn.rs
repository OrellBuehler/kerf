//! `checker/e/r4`: `kerf_core::quarter_turn` against FFmpeg's own `rotate`, with no adapter. The plan draws a rotation only where
//! `quarter_turn` says the filter lands on whole samples, so the set it accepts has to be what the filter turns exactly, on this FFmpeg
//! (`KERF_FFMPEG`; 6.1.1 and 9.0.2 are both held to it). Every angle runs through the export's own `rotate` text
//! (`rotate={rad}:fillcolor=..:ow=rotw({rad}):oh=roth({rad})`, `static_rotate`) on pictures of odd and even sides with a different value in
//! every sample of Y, U, V and A, and the output is compared with the exact quarter turn of the input (a transposition or a flip: no filter
//! needed to say what that is). `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_e_turn -- --ignored --nocapture
//! ```
//!
//! * the accepted angles are drawn exactly (a plan that accepts a turn FFmpeg does not land on whole samples for is wrong);
//! * the refused ones are not (a refusal wider than it has to be is printed as `WIDER`, and fails the exact-set test);
//! * nothing that is not a number, or is out of the filter's `int` range, panics or is accepted.

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

use kerf_core::quarter_turn;

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

fn degrees(d: f64) -> f64 {
    d.to_radians()
}

/// Whether FFmpeg's `rotate` of `rad` is the exact quarter turn `turn` of the picture, for each of the pictures and fills.
fn exact_everywhere(dir: &Path, rads: &[f64], turns: &[usize], pics: &[Pic]) -> Vec<bool> {
    let mut exact = vec![true; rads.len()];
    for pic in pics {
        for fill in ["none", "black@0"] {
            let got = rotate_all(dir, pic, rads, fill);
            for (i, out) in got.iter().enumerate() {
                if out.is_empty() || *out != pic.turned(turns[i]) {
                    exact[i] = false;
                }
            }
        }
    }
    exact
}

fn pictures() -> Vec<Pic> {
    vec![
        Pic::noise(24, 16, 1),
        Pic::noise(23, 15, 2),
        Pic::noise(17, 30, 3),
        Pic::noise(2, 3, 4),
    ]
}

/// The set of quarters the plan accepts is the set FFmpeg's `rotate` turns exactly: every whole number of quarters from -1310 to 1310 (past
/// 1303 quarters the filter's `int` angle itself overflows), each angle as the export's text writes it (`degrees.to_radians()`).
#[test]
#[ignore = "needs ffmpeg"]
fn checker_e_quarter_turn_is_the_set_rotate_turns_exactly_for_every_quarter() {
    let dir = scratch("quarters");
    let quarters: Vec<i32> = (-1310..=1310).collect();
    let rads: Vec<f64> = quarters.iter().map(|q| degrees(f64::from(*q) * 90.0)).collect();
    let turns: Vec<usize> = quarters.iter().map(|q| q.rem_euclid(4) as usize).collect();
    let exact = exact_everywhere(&dir, &rads, &turns, &pictures());
    let mut accepted = Vec::new();
    let mut ffmpeg = Vec::new();
    let mut wrong = Vec::new();
    for (i, q) in quarters.iter().enumerate() {
        let plan = quarter_turn(rads[i]);
        if let Some(turn) = plan {
            accepted.push(*q);
            if usize::from(turn) != turns[i] {
                wrong.push(format!("{q} quarters: the plan turns {turn}, not {}", turns[i]));
            }
        }
        if exact[i] {
            ffmpeg.push(*q);
        }
        match (plan.is_some(), exact[i]) {
            (true, false) => wrong.push(format!(
                "{q} quarters ({} rad): ACCEPTED by the plan and not exact in FFmpeg",
                rads[i]
            )),
            (false, true) => wrong.push(format!(
                "{q} quarters ({} rad): WIDER: refused by the plan and exact in FFmpeg",
                rads[i]
            )),
            _ => {}
        }
    }
    eprintln!("accepted by the plan: {accepted:?}");
    eprintln!("exact in FFmpeg:      {ffmpeg:?}");
    let expected: Vec<i32> = (-47..=-46).chain(-43..=40).chain(43..=44).collect();
    assert_eq!(ffmpeg, expected, "the set this FFmpeg turns exactly");
    assert_eq!(accepted, expected, "the set the plan accepts");
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Angles a hair off a quarter (in radians and in degrees, either side, from 1e-13 up to a hundredth of a radian) and a handful of
/// tiny angles: whatever the plan accepts FFmpeg turns exactly. The ones it refuses and FFmpeg draws exactly are printed (`WIDER`): the plan's
/// window is a billionth of a quarter, FFmpeg's fast path is `FLT_EPSILON` and its integer angle a sixteenth of a millionth of a radian.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_e_a_hair_off_a_quarter_is_accepted_only_where_rotate_is_exact() {
    let dir = scratch("hair");
    let mut rads = Vec::new();
    let mut turns = Vec::new();
    let mut names = Vec::new();
    let around: [i32; 30] = [
        -8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 20, 40, 41, 44, 45, -43, -46, -47, -48,
    ];
    let radian_hairs = [
        0.0, 1e-13, 1e-12, 1e-10, 1e-9, 1.4e-9, 1.6e-9, 3e-9, 1e-8, 1e-7, 1.1e-7, 1.19e-7, 1.2e-7, 1.3e-7, 1e-6, 1e-5, 1e-3, 1e-2,
    ];
    let degree_hairs = [1e-12, 1e-10, 1e-9, 1e-8, 1e-7, 1e-6, 1e-5, 1e-3];
    for q in around {
        let base = f64::from(q) * 90.0;
        for h in radian_hairs {
            for sign in [1.0, -1.0] {
                if h == 0.0 && sign < 0.0 {
                    continue;
                }
                rads.push(degrees(base) + sign * h);
                turns.push(q.rem_euclid(4) as usize);
                names.push(format!("{q} quarters {:+e} rad", sign * h));
            }
        }
        for h in degree_hairs {
            for sign in [1.0, -1.0] {
                rads.push(degrees(base + sign * h));
                turns.push(q.rem_euclid(4) as usize);
                names.push(format!("{q} quarters {:+e} degrees", sign * h));
            }
        }
    }
    for (name, d) in [
        ("5e-324 degrees", 5e-324),
        ("1e-300 degrees", 1e-300),
        ("-1e-300 degrees", -1e-300),
        ("1e-100 degrees", 1e-100),
        ("2.2250738585072014e-308 degrees", f64::MIN_POSITIVE),
        ("1e-20 degrees", 1e-20),
    ] {
        rads.push(degrees(d));
        turns.push(0);
        names.push(name.to_string());
    }
    let exact = exact_everywhere(&dir, &rads, &turns, &pictures());
    let (mut accepted, mut ok, mut wider, mut bad) = (0, 0, Vec::new(), Vec::new());
    for (i, name) in names.iter().enumerate() {
        match (quarter_turn(rads[i]), exact[i]) {
            (Some(turn), true) => {
                accepted += 1;
                ok += 1;
                if usize::from(turn) != turns[i] {
                    bad.push(format!("{name}: accepted as {turn}, not {}", turns[i]));
                }
            }
            (Some(_), false) => {
                accepted += 1;
                bad.push(format!(
                    "{name} ({} rad): ACCEPTED and FFmpeg does not turn it exactly",
                    rads[i]
                ));
            }
            (None, true) => wider.push(format!("{name} ({} rad)", rads[i])),
            (None, false) => {}
        }
    }
    eprintln!(
        "{} angles, {accepted} accepted ({ok} exact in FFmpeg), {} refused though FFmpeg turns them exactly",
        rads.len(),
        wider.len()
    );
    for w in &wider {
        eprintln!("WIDER {w}");
    }
    assert!(
        accepted > 100,
        "{accepted} accepted: the hairs inside the window are quarters"
    );
    assert!(bad.is_empty(), "{bad:#?}");
}

/// Angles that are not numbers, are infinite, are too large for the filter's `int` angle (past 2048 radians, and the plan's own cap of 1000),
/// are denormal or are the extremes of the type: no panic (a debug build checks every overflow), and never a turn but the tiny ones' none.
#[test]
#[ignore = "needs nothing: pure arithmetic"]
fn checker_e_quarter_turn_never_panics_and_refuses_what_is_not_an_angle() {
    let huge = [
        f64::NAN,
        -f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        f64::MIN,
        1e300,
        -1e300,
        1e100,
        1e18,
        9.2e18,
        9.3e18,
        4.5e15,
        2f64.powi(53),
        2f64.powi(62),
        2f64.powi(63),
        2f64.powi(64),
        1e9,
        1e7,
        1e5,
        2048.0,
        2049.0,
        1000.000001,
        -1000.000001,
        1001.0,
        -1001.0,
        (2f64.powi(31) / 1_048_576.0) + 1.0,
        (2f64.powi(31) - 1.0) / 1_048_576.0,
    ];
    for a in huge {
        let t = std::panic::catch_unwind(|| quarter_turn(a));
        assert!(t.is_ok(), "{a:e}: panicked");
        assert_eq!(t.unwrap(), None, "{a:e}");
        assert!(!kerf_core::is_quarter_turn(a), "{a:e}");
    }
    // An exact multiple of a quarter past the cap, whatever its integer sine would say.
    for q in 637..=1_000_000_i64 {
        let a = q as f64 * std::f64::consts::FRAC_PI_2;
        assert_eq!(quarter_turn(a), None, "{q} quarters");
        assert_eq!(quarter_turn(-a), None, "-{q} quarters");
    }
    for q in 637..=3000_i64 {
        let d = q as f64 * 90.0;
        assert_eq!(quarter_turn(d.to_radians()), None, "{q} quarters in degrees");
    }
    // Denormals and the smallest numbers are no turn at all (the angle is 0 for `rotate`), not a panic.
    for a in [
        5e-324,
        -5e-324,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        1e-300,
        -1e-300,
        1e-20,
        0.0,
        -0.0,
    ] {
        assert_eq!(quarter_turn(a), Some(0), "{a:e}");
    }
    // The picture of the angle that sits on the cap: no panic anywhere up to it, and the exact set is inside it.
    let mut found = Vec::new();
    for q in -700..=700_i32 {
        let a = (f64::from(q) * 90.0).to_radians();
        if quarter_turn(a).is_some() {
            found.push(q);
        }
    }
    let expected: Vec<i32> = (-47..=-46).chain(-43..=40).chain(43..=44).collect();
    assert_eq!(found, expected);
}
