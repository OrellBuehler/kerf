//! `PlacementMap::corners_have_a_map` against `PlacementMap::from_corners`, attacked from outside (no FFmpeg). The plan asks the first, in constant time,
//! where it used to build the tables of a moving still's `perspective` to learn whether the draw would refuse them; it must say what the tables
//! say, for every rectangle the route can make (a zoom and a pan are always one): near-degenerate ones (a side one unit in the last place wide, a side
//! a subnormal wide), values on and just past the fixed-point bound, huge ones that are still finite, mirrored ones (a negative size), zero and
//! non-finite ones. Any other quadrilateral may pass the cheap check and fail in the tables (it says so), but never the other way round.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_t6_round3_corners -- --nocapture
//! ```

#![allow(clippy::print_stderr, clippy::type_complexity)]

use kerf_core::perspective::{PlacementFrame, PlacementMap};
use kerf_core::still_motion::{StillMotion, StillPose, StillSample, StillStatic};
use kerf_core::Fit;

/// `REACH` of the filter's fixed-point tables, in samples (2^21).
const REACH: f64 = 2_097_152.0;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> [[f64; 2]; 4] {
    [[x0, y0], [x1, y0], [x0, y1], [x1, y1]]
}

fn next_up(v: f64) -> f64 {
    if v.is_nan() || v == f64::INFINITY {
        return v;
    }
    let bits = v.to_bits();
    if v == 0.0 {
        return f64::from_bits(1);
    }
    f64::from_bits(if v > 0.0 { bits + 1 } else { bits - 1 })
}

fn next_down(v: f64) -> f64 {
    -next_up(-v)
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn unit(&mut self) -> f64 {
        self.next() as f64 / (1u64 << 53) as f64
    }

    /// A magnitude with a random exponent between `10^lo` and `10^hi`, a random sign.
    fn log(&mut self, lo: f64, hi: f64) -> f64 {
        let v = 10f64.powf(lo + (hi - lo) * self.unit());
        if self.next().is_multiple_of(2) {
            v
        } else {
            -v
        }
    }
}

/// The values the corners are made of: zero, a subnormal, the smallest normal, tiny and huge, the bound and its neighbours, not numbers.
fn values() -> Vec<f64> {
    let mut v = vec![
        0.0,
        -0.0,
        5e-324,
        -5e-324,
        2.2250738585072014e-308,
        1e-300,
        1e-160,
        1e-10,
        0.25,
        0.5,
        1.0,
        1.5,
        7.0,
        63.5,
        100.0,
        1e5,
        1e6,
        REACH - 1.0,
        REACH,
        next_down(REACH),
        next_up(REACH),
        REACH + 1.0,
        4.0e6,
        1e9,
        1e12,
        1e300,
        f64::MAX,
        f64::INFINITY,
        f64::NAN,
    ];
    let negatives: Vec<f64> = v.iter().map(|x| -x).collect();
    v.extend(negatives);
    v.push(next_up(1.0));
    v.push(next_down(1.0));
    v.push(next_up(1e6));
    v
}

fn frames() -> Vec<(&'static str, PlacementFrame)> {
    vec![
        ("64x36 window, 64x36 frame", PlacementFrame::window((64, 36), (64, 36))),
        ("96x54 window, 64x36 frame", PlacementFrame::window((96, 54), (64, 36))),
        ("2x2", PlacementFrame::window((2, 2), (2, 2))),
        ("odd window 7x5, 6x4 frame", PlacementFrame::window((7, 5), (6, 4))),
        ("1x1", PlacementFrame::window((1, 1), (1, 1))),
        ("wide 4000x20, 3000x10", PlacementFrame::window((4000, 20), (3000, 10))),
        ("tall 20x4000, 10x3000", PlacementFrame::window((20, 4000), (10, 3000))),
    ]
}

#[test]
fn for_every_rectangle_the_cheap_check_says_what_the_tables_say() {
    let vals = values();
    let frames = frames();
    let (mut compared, mut both_ok, mut both_err) = (0usize, 0usize, 0usize);
    let mut bad: Vec<String> = Vec::new();
    for (name, frame) in frames
        .iter()
        .filter(|(n, _)| !n.contains("wide") && !n.contains("tall") && !n.contains("2x2"))
    {
        for &x0 in &vals {
            for &x1 in &vals {
                for &y0 in vals.iter().step_by(3) {
                    // (The x's carry every value, the y's the ones that matter: a side of no height, of one unit in the last place, mirrored.)
                    for y1 in [y0, next_up(y0), next_down(y0), -y0, 0.0, 1.0, 1e5, REACH] {
                        let corners = rect(x0, y0, x1, y1);
                        let cheap = PlacementMap::corners_have_a_map(*frame, &corners);
                        let tables = PlacementMap::from_corners(*frame, &corners);
                        compared += 1;
                        match (&cheap, &tables) {
                            (Ok(()), Ok(_)) => both_ok += 1,
                            (Err(a), Err(b)) => {
                                both_err += 1;
                                if a != b {
                                    bad.push(format!("{name} {corners:?}: two different refusals: {a:?} / {b:?}"));
                                }
                            }
                            (Ok(()), Err(e)) => bad.push(format!(
                                "{name} {corners:?}: the cheap check says a map, the tables refuse: {e:?}"
                            )),
                            (Err(e), Ok(_)) => bad.push(format!(
                                "{name} {corners:?}: the cheap check refuses ({e:?}) a map the tables make"
                            )),
                        }
                        if bad.len() > 30 {
                            break;
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "R3 CORNERS rectangles compared {compared}: both a map {both_ok}, both refuse {both_err}, disagree {}",
        bad.len()
    );
    for b in bad.iter().take(30) {
        eprintln!("R3 CORNERS DISAGREE {b}");
    }
    assert!(bad.is_empty(), "{} disagreements; first: {}", bad.len(), bad[0]);
    assert!(
        both_ok > 1000 && both_err > 1000,
        "the sweep reached both answers: {both_ok} / {both_err}"
    );
}

#[test]
fn for_random_rectangles_of_every_magnitude_the_cheap_check_says_what_the_tables_say() {
    let mut rng = Lcg(0x5eed_c0de_1234_5678);
    let frames = frames();
    let (mut compared, mut ok, mut refused, mut noisy_fallback) = (0usize, 0usize, 0usize, 0usize);
    let mut bad: Vec<String> = Vec::new();
    for n in 0..120_000usize {
        let (_, frame) = &frames[n % frames.len()];
        // A zoom and a pan: the centre and the half extent of each axis, in any exponent; a mirrored one has a negative half extent.
        let (cx, cy) = (rng.log(-12.0, 7.2), rng.log(-12.0, 7.2));
        let (hx, hy) = (rng.log(-12.0, 7.2), rng.log(-12.0, 7.2));
        let (x0, x1) = (cx - hx, cx + hx);
        let (y0, y1) = (cy - hy, cy + hy);
        // ... and a side exactly one unit in the last place wide, or the same corner twice, now and then.
        let (x1, y1) = match n % 11 {
            0 => (next_up(x0), y1),
            1 => (x1, next_down(y0)),
            2 => (x0, y1),
            3 => (x1, y0),
            _ => (x1, y1),
        };
        let corners = rect(x0, y0, x1, y1);
        let cheap = PlacementMap::corners_have_a_map(*frame, &corners);
        let tables = PlacementMap::from_corners(*frame, &corners);
        compared += 1;
        match (&cheap, &tables) {
            (Ok(()), Ok(map)) => {
                ok += 1;
                noisy_fallback += usize::from(matches!(map.tables, kerf_core::perspective::Tables::Pixels { .. }));
            }
            (Err(a), Err(b)) => {
                refused += 1;
                if a != b {
                    bad.push(format!("{corners:?}: two different refusals: {a:?} / {b:?}"));
                }
            }
            (Ok(()), Err(e)) => bad.push(format!("{corners:?}: a map by the cheap check, refused by the tables: {e:?}")),
            (Err(e), Ok(_)) => bad.push(format!(
                "{corners:?}: refused by the cheap check ({e:?}), a map in the tables"
            )),
        }
    }
    eprintln!(
        "R3 CORNERS random rectangles {compared}: a map {ok} (of them whole-table fallbacks {noisy_fallback}), refused {refused}, disagree {}",
        bad.len()
    );
    for b in bad.iter().take(30) {
        eprintln!("R3 CORNERS DISAGREE {b}");
    }
    assert!(bad.is_empty(), "{} disagreements; first: {}", bad.len(), bad[0]);
    assert!(ok > 10_000 && refused > 1_000, "{ok} / {refused}");
}

/// Quadrilaterals that are not rectangles: the tables can refuse what the cheap check lets through (it says so), never the reverse.
#[test]
fn for_any_other_quadrilateral_a_map_in_the_tables_is_a_map_by_the_cheap_check() {
    let mut rng = Lcg(0x0ddba11_5ca1ab1e);
    let frames = frames();
    let (mut compared, mut tables_ok, mut cheap_only) = (0usize, 0usize, 0usize);
    let mut bad: Vec<String> = Vec::new();
    for n in 0..60_000usize {
        let (_, frame) = &frames[n % frames.len()];
        let mut c = [[0.0f64; 2]; 4];
        for corner in &mut c {
            *corner = [rng.log(-3.0, 6.6), rng.log(-3.0, 6.6)];
        }
        // Some are nearly rectangles, some are rectangles with one corner moved by a unit in the last place.
        if n % 3 == 0 {
            let r = rect(c[0][0], c[0][1], c[3][0], c[3][1]);
            c = r;
            c[3][0] = next_up(c[3][0]);
        }
        let cheap = PlacementMap::corners_have_a_map(*frame, &c);
        let tables = PlacementMap::from_corners(*frame, &c);
        compared += 1;
        match (cheap.is_ok(), tables.is_ok()) {
            (false, true) => bad.push(format!(
                "{c:?}: refused by the cheap check ({:?}), a map in the tables",
                cheap.err()
            )),
            (true, true) => tables_ok += 1,
            (true, false) => cheap_only += 1,
            (false, false) => {}
        }
    }
    eprintln!(
        "R3 CORNERS quadrilaterals {compared}: a map {tables_ok}, the cheap check alone {cheap_only}, disagree {}",
        bad.len()
    );
    assert!(bad.is_empty(), "{} disagreements; first: {}", bad.len(), bad[0]);
}

/// The corners a moving still really makes, at any pose (a hostile one included): the check the plan asks and the tables the draw builds agree.
#[test]
fn the_corners_of_a_real_move_at_any_pose_are_judged_as_the_tables_judge_them() {
    let mut rng = Lcg(0x0007_15c0_ffee);
    let (mut built, mut tried, mut compared, mut ok, mut refused, mut whole) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut whole_own, mut whole_entries) = (0usize, 0usize);
    let mut bad: Vec<String> = Vec::new();
    while built < 1000 && tried < 300_000 {
        tried += 1;
        let picture = ((rng.next() % 6000 + 16) as u32, (rng.next() % 6000 + 16) as u32);
        let frame = (((rng.next() % 960 + 1) * 2) as u32, ((rng.next() % 540 + 1) * 2) as u32);
        let fit = if rng.unit() < 0.6 { Fit::Cover } else { Fit::Contain };
        let crop = if rng.unit() < 0.4 {
            let (l, t) = (rng.unit() * 0.3, rng.unit() * 0.3);
            (l, t, 0.3 + rng.unit() * (0.7 - l), 0.3 + rng.unit() * (0.7 - t))
        } else {
            (0.0, 0.0, 1.0, 1.0)
        };
        let stat = StillStatic {
            picture,
            fit,
            crop,
            cropped_first: fit == Fit::Cover && rng.unit() < 0.7,
        };
        let cz = if rng.unit() < 0.3 { 1.0 + rng.unit() * 1.5 } else { 1.0 };
        let side = 1.0 / cz;
        let sample = StillSample {
            scale: 1.0 + rng.unit() * 4.0,
            pos_x: (rng.unit() - 0.5) * 0.4,
            pos_y: (rng.unit() - 0.5) * 0.4,
            wl: (1.0 - side) * (1.0 + (rng.unit() * 2.0 - 1.0)) / 2.0,
            wt: (1.0 - side) * (1.0 + (rng.unit() * 2.0 - 1.0)) / 2.0,
            side,
        };
        let Some(m) = StillMotion::build(&stat, frame, &[sample]) else {
            continue;
        };
        built += 1;
        let (_, _, w, h) = m.window;
        let frame_map = PlacementFrame::window((w, h), m.frame);
        // The pose of the sample, and poses of every kind of hostility: far off, huge, tiny, zero, negative, not a number.
        let mut poses = vec![StillPose {
            scale: sample.scale,
            pos_x: sample.pos_x,
            pos_y: sample.pos_y,
            crop_zoom: cz,
            crop_x: 0.0,
            crop_y: 0.0,
        }];
        for _ in 0..40 {
            let wild = |rng: &mut Lcg| match rng.next() % 9 {
                0 => f64::NAN,
                1 => f64::INFINITY,
                2 => 0.0,
                3 => -rng.log(-3.0, 3.0).abs(),
                4 => rng.log(-300.0, 300.0),
                5 => rng.log(-9.0, -1.0),
                6 => rng.log(3.0, 9.0),
                _ => rng.log(-1.0, 1.0),
            };
            poses.push(StillPose {
                scale: wild(&mut rng),
                pos_x: wild(&mut rng),
                pos_y: wild(&mut rng),
                crop_zoom: if rng.unit() < 0.5 { 1.0 } else { wild(&mut rng) },
                crop_x: if rng.unit() < 0.5 { 0.0 } else { wild(&mut rng) },
                crop_y: if rng.unit() < 0.5 { 0.0 } else { wild(&mut rng) },
            });
        }
        // (A pose far enough off keeps the corners finite but beyond the bound, or inside it.)
        for k in 0..40 {
            let s = 10f64.powf(-6.0 + 0.3 * f64::from(k));
            poses.push(StillPose {
                scale: sample.scale,
                pos_x: sample.pos_x * s,
                pos_y: sample.pos_y * s,
                crop_zoom: cz,
                crop_x: 0.0,
                crop_y: 0.0,
            });
            poses.push(StillPose {
                scale: sample.scale * s,
                pos_x: sample.pos_x,
                pos_y: sample.pos_y,
                crop_zoom: cz,
                crop_x: 0.0,
                crop_y: 0.0,
            });
        }
        for (k_pose, pose) in poses.into_iter().enumerate() {
            let corners = m.corners(&pose);
            let cheap = PlacementMap::corners_have_a_map(frame_map, &corners);
            let tables = PlacementMap::from_corners(frame_map, &corners);
            compared += 1;
            match (&cheap, &tables) {
                (Ok(()), Ok(map)) => {
                    ok += 1;
                    if matches!(map.tables, kerf_core::perspective::Tables::Pixels { .. }) {
                        whole += 1;
                        whole_entries = whole_entries.max(m.frame.0 as usize * m.frame.1 as usize);
                        if k_pose == 0 {
                            whole_own += 1;
                        }
                    }
                }
                (Err(a), Err(b)) => {
                    refused += 1;
                    if a != b {
                        bad.push(format!("window {w}x{h} frame {frame:?} {pose:?}: two refusals {a:?} / {b:?}"));
                    }
                }
                (a, b) => bad.push(format!(
                    "window {w}x{h} frame {:?} {pose:?} {corners:?}: cheap {:?}, tables {:?}",
                    m.frame,
                    a.as_ref().err(),
                    b.as_ref().err()
                )),
            }
        }
    }
    eprintln!(
        "R3 CORNERS real moves {built}: {compared} poses, a map {ok} (whole tables {whole}, of them a move's own pose {whole_own} of {built}; the largest {whole_entries} entries), refused {refused}, disagree {}",
        bad.len()
    );
    for b in bad.iter().take(30) {
        eprintln!("R3 CORNERS DISAGREE {b}");
    }
    assert!(built >= 900, "{built} moves built");
    assert!(bad.is_empty(), "{} disagreements; first: {}", bad.len(), bad[0]);
    assert!(ok > 1000 && refused > 1000, "{ok} / {refused}");
}

/// What the check is for: it does not grow with the frame. A frame the tables are millions of entries for is answered at once.
#[test]
fn the_cheap_check_does_not_grow_with_the_frame() {
    let corners = rect(100.5, 40.25, 7900.5, 4400.25);
    let huge = PlacementFrame::window((8192, 4608), (8192, 4608));
    let small = PlacementFrame::window((64, 36), (64, 36));
    let time = |frame: PlacementFrame| {
        let started = std::time::Instant::now();
        for _ in 0..20_000 {
            assert!(PlacementMap::corners_have_a_map(frame, std::hint::black_box(&corners)).is_ok());
        }
        started.elapsed()
    };
    let (a, b) = (time(small), time(huge));
    eprintln!("R3 CORNERS 20000 checks: small frame {a:?}, 8192x4608 frame {b:?}");
    assert!(b < a * 20 + std::time::Duration::from_millis(50), "{a:?} / {b:?}");
    assert!(b < std::time::Duration::from_secs(2), "{b:?}");
    // ... and the tables of that frame are a few kilobytes (axis tables), not the 8192 x 4608 of a whole table.
    let map = PlacementMap::from_corners(huge, &corners).expect("a map");
    match map.tables {
        kerf_core::perspective::Tables::Axes { u, v } => assert_eq!(u.len() + v.len(), 8192 + 4608),
        kerf_core::perspective::Tables::Pixels { u, .. } => {
            eprintln!("R3 CORNERS the whole table of the huge frame: {} entries", u.len());
        }
    }
}
