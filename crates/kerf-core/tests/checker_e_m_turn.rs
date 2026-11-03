//! `checker/e/m`: `kerf_core::quarter_turn` against a port of `vf_rotate.c` written from the C source alone (FFmpeg n6.1.1 and n9.0.2: the file is
//! the same in both for this), with no FFmpeg and no adapter. The filter lands on whole samples where its `simple_rotate` fast path is taken
//! (`fabs(angle - target) < FLT_EPSILON` for 0, π/2, π and 3π/2 only) or where its integer sine and cosine of the angle are exactly 0 and ±65536.
//! The plan's window is the fast path's: an angle within `FLT_EPSILON` (1.1920928955078125e-7, the float promoted to double) radians of a quarter
//! is a quarter if and only if the filter is exact for it, and an angle outside that window is never one.
//!
//! The runs against the real filter are `tests/checker_e_turn.rs` (kerf-gpu).

#![allow(clippy::print_stderr)]

const EPS: f64 = 1.192_092_895_507_812_5e-7;
const MAX_ANGLE: f64 = 1000.0;

/// `int_sin` of `vf_rotate.c`, from the C.
fn int_sin(mut a: i64) -> i64 {
    const FIXP2: i64 = 1 << 20;
    const INT_PI: i64 = 3_294_199;
    if a < 0 {
        a = INT_PI - a;
    }
    a %= 2 * INT_PI;
    if a >= INT_PI * 3 / 2 {
        a -= 2 * INT_PI;
    }
    if a >= INT_PI / 2 {
        a = INT_PI - a;
    }
    let a2 = (a * a) / FIXP2;
    let mut res = 0;
    let mut i = 2;
    while i < 11 {
        res += a;
        a = -a * a2 / (FIXP2 * i * (i + 1));
        i += 2;
    }
    (res + 8) >> 4
}

/// The quarter (0 to 3) the filter turns exactly for `angle`, if it does, as the C reads: the fast path for the four targets in [0, 2π), else
/// the integer sine and cosine, which must be exactly one of the four (c, s) pairs.
fn filter_turn(angle: f64) -> Option<u8> {
    let pi = std::f64::consts::PI;
    for (target, turn) in [(0.0, 0), (pi / 2.0, 1), (pi, 2), (3.0 * pi / 2.0, 3)] {
        if (angle - target).abs() < EPS {
            return Some(turn);
        }
    }
    let angle_int = (angle * 65536.0 * 16.0) as i32 as i64;
    let s = int_sin(angle_int);
    let c = int_sin(angle_int + 3_294_199 / 2);
    match (c, s) {
        (65536, 0) => Some(0),
        (0, 65536) => Some(1),
        (-65536, 0) => Some(2),
        (0, -65536) => Some(3),
        _ => None,
    }
}

fn nearest_quarter(angle: f64) -> f64 {
    (angle / std::f64::consts::FRAC_PI_2).round()
}

/// Whatever the plan accepts is a turn the filter makes whole, and the quarter it names is the nearest; and inside the filter's own window the
/// plan accepts exactly what the filter turns whole. 1,401 quarters, 33 offsets each, either side of every one.
#[test]
fn checker_e_m_a_quarter_turn_is_what_the_filter_turns_whole_inside_its_own_window() {
    let offs: Vec<f64> = [
        0.0,
        1e-12,
        1e-10,
        1e-9,
        1e-8,
        3e-8,
        5e-8,
        8e-8,
        1.0e-7,
        1.1e-7,
        1.15e-7,
        1.19e-7,
        1.1920e-7,
        1.19209e-7,
        1.192092e-7,
        1.1920928e-7,
    ]
    .iter()
    .flat_map(|&o| [o, -o])
    .skip(1)
    .chain([0.0])
    .collect();
    let (mut accepted, mut refused_inside, mut exact_inside) = (0, 0, 0);
    let mut wrong = Vec::new();
    for q in -700_i32..=700 {
        let base = f64::from(q) * std::f64::consts::FRAC_PI_2;
        for &off in &offs {
            let angle = base + off;
            if angle.abs() > MAX_ANGLE {
                continue;
            }
            let plan = kerf_core::quarter_turn(angle);
            let inside = (angle - nearest_quarter(angle) * std::f64::consts::FRAC_PI_2).abs() < EPS;
            if let Some(turn) = plan {
                accepted += 1;
                if f64::from(turn) != nearest_quarter(angle).rem_euclid(4.0) {
                    wrong.push(format!("{q} quarters {off:+e}: names turn {turn}"));
                }
                if filter_turn(angle) != Some(turn) {
                    wrong.push(format!(
                        "{q} quarters {off:+e} ({angle:?} rad): ACCEPTED as turn {turn}, the filter makes {:?}",
                        filter_turn(angle)
                    ));
                }
            }
            if inside {
                exact_inside += 1;
                let filter = filter_turn(angle).filter(|t| f64::from(*t) == nearest_quarter(angle).rem_euclid(4.0));
                if plan != filter {
                    refused_inside += 1;
                    wrong.push(format!(
                        "{q} quarters {off:+e} ({angle:?} rad): inside the window, the plan says {plan:?}, the filter {filter:?}"
                    ));
                }
            } else if plan.is_some() {
                wrong.push(format!("{q} quarters {off:+e} ({angle:?} rad): accepted outside the window"));
            }
        }
    }
    eprintln!("{accepted} accepted, {exact_inside} inside the window, {refused_inside} of them disagree");
    assert!(
        accepted > 1000 && exact_inside > 20_000,
        "{accepted} accepted, {exact_inside} inside"
    );
    assert!(
        wrong.is_empty(),
        "{} disagreements, first: {:#?}",
        wrong.len(),
        &wrong[..wrong.len().min(12)]
    );
}

/// `n` representable doubles up (down for a negative `n`) from `x`.
fn step(x: f64, n: i64) -> f64 {
    let bits = x.to_bits() as i64;
    let moved = if x.is_sign_negative() { bits - n } else { bits + n };
    f64::from_bits(moved as u64)
}

/// The fast path's window to the last bit: at each of its four targets, the doubles around `target ± FLT_EPSILON` are accepted while
/// `fabs(angle - target) < FLT_EPSILON` and refused from there on, three steps either side, for the double arithmetic the C does.
#[test]
fn checker_e_m_the_window_ends_where_the_fast_path_does_to_the_last_bit() {
    let pi = std::f64::consts::PI;
    let mut checked = 0;
    let (mut inside, mut outside) = (0, 0);
    for (target, turn) in [(0.0, 0u8), (pi / 2.0, 1), (pi, 2), (3.0 * pi / 2.0, 3)] {
        for sign in [1.0, -1.0] {
            let edge = target + sign * EPS;
            for n in -3..=3 {
                let a = step(edge, n);
                let plan = kerf_core::quarter_turn(a);
                if (a - target).abs() < EPS {
                    inside += 1;
                    assert_eq!(plan, Some(turn), "{a:?} is inside the fast path of target {target}");
                } else {
                    outside += 1;
                    assert_eq!(plan, None, "{a:?} is outside the fast path of target {target}");
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 56);
    assert!(
        inside >= 8 && outside >= 8,
        "{inside} inside, {outside} outside: the edge is crossed at every target"
    );
}

/// Past the cap the conversion of the angle to the filter's `int` is not defined, and nothing that is not a finite number is a turn.
#[test]
fn checker_e_m_what_is_not_an_angle_is_no_turn_and_the_widened_window_does_not_move_the_cap() {
    for a in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        1e300,
        -1e300,
        MAX_ANGLE + 1e-3,
        -(MAX_ANGLE + 1e-3),
    ] {
        assert_eq!(kerf_core::quarter_turn(a), None, "{a:e}");
        assert!(!kerf_core::is_quarter_turn(a));
    }
    for a in [0.0, -0.0, 5e-324, -5e-324, 1e-7, -1e-7, EPS * 0.999_999, -EPS * 0.999_999] {
        assert_eq!(kerf_core::quarter_turn(a), Some(0), "{a:e}");
    }
    for a in [EPS, -EPS, 1.2e-7, -1.2e-7, 1.3e-7, -1.3e-7, 1e-6, 1e-3] {
        assert_eq!(
            kerf_core::quarter_turn(a),
            None,
            "{a:e}: the filter's own window is open at FLT_EPSILON"
        );
    }
}
