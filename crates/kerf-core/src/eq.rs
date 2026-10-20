//! A port of FFmpeg's `eq` filter (`libavfilter/vf_eq.c`) as per-plane lookup
//! tables.
//!
//! The filter does **not** work in RGB. It runs on the three planes of a planar
//! YUV frame, each through its own 256-entry table:
//!
//! * luma (plane 0): `contrast`, `brightness` and `gamma * gamma_g`;
//! * U (plane 1): `saturation` as its contrast (about 128), no brightness, and
//!   `gamma = sqrt(gamma_b / gamma_g)`;
//! * V (plane 2): the same with `sqrt(gamma_r / gamma_g)`.
//!
//! So Kerf's "temperature" (opposing `gamma_r` / `gamma_b`) is a power function
//! on the chroma planes, and saturation scales chroma about 128 — which is why
//! this is applied to Y / U / V *before* any RGB conversion. A table is built
//! exactly the way `vf_eq` builds it, including the two code paths it picks
//! between (the integer `process_c` formula when gamma is 1, the `pow` table
//! otherwise) and its `float` clamping of every parameter, so the result is
//! the same byte for every input byte rather than "close".
//!
//! It lives in `kerf-core` because both ends need the tables: the compositor
//! applies them, and the plan judges how steep they are (a layer the
//! compositor scales feeds them a picture a level off FFmpeg's, which the
//! tables multiply: [`table_steps`]).

use crate::model::Color;

/// What `eq` uses for one plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneParams {
    pub contrast: f64,
    pub brightness: f64,
    pub gamma: f64,
}

/// `av_clipf`: clamp, and round to `float` — the filter stores the result in a
/// `double` field, but only after it has been through a `float`.
fn clipf(v: f64, lo: f64, hi: f64) -> f64 {
    f64::from(v.clamp(lo, hi) as f32)
}

/// The three planes' parameters for a colour correction, as `vf_eq`'s
/// `set_gamma` / `set_contrast` / `set_brightness` / `set_saturation` derive
/// them from the `eq=` options Kerf writes (see `eq_filter` in kerf-core).
pub fn plane_params(c: &Color) -> [PlaneParams; 3] {
    let contrast = clipf(c.contrast, -1000.0, 1000.0);
    let brightness = clipf(c.brightness, -1.0, 1.0);
    let saturation = clipf(c.saturation, 0.0, 3.0);
    let gamma = clipf(c.gamma, 0.1, 10.0);
    let (gr, gb) = c.temperature_gammas().unwrap_or((1.0, 1.0));
    let gamma_r = clipf(gr, 0.1, 10.0);
    let gamma_g = 1.0; // Kerf never sets it
    let gamma_b = clipf(gb, 0.1, 10.0);
    [
        PlaneParams {
            contrast,
            brightness,
            gamma: gamma * gamma_g,
        },
        PlaneParams {
            contrast: saturation,
            brightness: 0.0,
            gamma: (gamma_b / gamma_g).sqrt(),
        },
        PlaneParams {
            contrast: saturation,
            brightness: 0.0,
            gamma: (gamma_r / gamma_g).sqrt(),
        },
    ]
}

/// The table for one plane, or `None` where `eq` leaves the plane alone
/// (`contrast == 1`, `brightness == 0`, `gamma == 1` — `adjust = NULL`).
pub fn plane_lut(p: &PlaneParams) -> Option<[u8; 256]> {
    if p.contrast == 1.0 && p.brightness == 0.0 && p.gamma == 1.0 {
        return None;
    }
    let mut lut = [0u8; 256];
    if p.gamma == 1.0 && p.contrast.abs() < 7.9 {
        // `process_c`: fixed-point, and the only path whose brightness is
        // quantised to 0.01.
        let contrast = (p.contrast * 256.0 * 16.0) as i32;
        let brightness = ((100.0 * p.brightness + 100.0) as i32 * 511) / 200 - 128 - contrast / 32;
        for (i, out) in lut.iter_mut().enumerate() {
            let pel = ((i as i32 * contrast) >> 12) + brightness;
            *out = if pel & !255 != 0 {
                // `(-pel) >> 31`: 0 below the range, all ones above it.
                ((-pel) >> 31) as u8
            } else {
                pel as u8
            };
        }
    } else {
        // `create_lut` with gamma_weight 1: `v = pow(v, 1 / gamma)`, truncated.
        let g = 1.0 / p.gamma;
        for (i, out) in lut.iter_mut().enumerate() {
            let v = p.contrast * (i as f64 / 255.0 - 0.5) + 0.5 + p.brightness;
            *out = if v <= 0.0 {
                0
            } else {
                let v = v.powf(g);
                if v >= 1.0 {
                    255
                } else {
                    (256.0 * v) as u8
                }
            };
        }
    }
    Some(lut)
}

/// The tables the compositor applies (rows are Y, U, V), or `None` when the
/// correction is the identity — the neutral value is omitted, like it is from
/// the graph.
pub fn luts(c: &Color) -> Option<[[u8; 256]; 3]> {
    if c.is_identity() {
        return None;
    }
    let identity: [u8; 256] = std::array::from_fn(|i| i as u8);
    let [y, u, v] = plane_params(c).map(|p| plane_lut(&p).unwrap_or(identity));
    Some([y, u, v])
}

/// The steepest step between neighbouring entries of a table: how many levels two pictures one level
/// apart are from each other after it. A plane `eq` leaves alone (no table) is the identity, 1.
fn steepest_step(lut: Option<&[u8; 256]>) -> u32 {
    lut.map_or(1, |l| l.windows(2).map(|w| u32::from(w[0].abs_diff(w[1]))).max().unwrap_or(1))
}

/// The steepest step of each plane's table (Y, U, V) for the correction `c`, as [`luts`] builds them:
/// what one level of difference in the picture ahead of the `eq` becomes after it. A renderer whose scaler is
/// a level from swscale's hands the tables a picture a level from FFmpeg's, and the tables multiply that by
/// this (contrast, and gamma's power toward black, which grows without bound; a clip's black point, where a
/// table leaves 0 for its first entry, is a step of its own). 1 for a plane the correction leaves alone.
///
/// Building the tables costs about 20 microseconds when gamma or the temperature takes the `pow` path (under one
/// when it does not), and a plan asks for every frame of a clip, whose colour is the same on all of them unless it is
/// keyed: the last few answers are kept per thread.
pub fn table_steps(c: &Color) -> [u32; 3] {
    use std::cell::RefCell;

    const KEPT: usize = 4;
    type Key = [u64; 5];
    #[derive(Default)]
    struct Memo {
        entries: [Option<(Key, [u32; 3])>; KEPT],
        next: usize,
    }
    thread_local! {
        static MEMO: RefCell<Memo> = RefCell::new(Memo::default());
    }

    if c.is_identity() {
        return [1; 3];
    }
    let key: Key = [c.brightness, c.contrast, c.saturation, c.gamma, c.temperature].map(f64::to_bits);
    let kept = MEMO.with(|m| {
        m.borrow()
            .entries
            .iter()
            .flatten()
            .find(|(k, _)| *k == key)
            .map(|(_, steps)| *steps)
    });
    if let Some(steps) = kept {
        return steps;
    }
    let steps = plane_params(c).map(|p| steepest_step(plane_lut(&p).as_ref()));
    MEMO.with(|m| {
        let mut m = m.borrow_mut();
        let at = m.next;
        m.entries[at] = Some((key, steps));
        m.next = (at + 1) % KEPT;
    });
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(brightness: f64, contrast: f64, saturation: f64, gamma: f64, temperature: f64) -> Color {
        Color {
            brightness,
            contrast,
            saturation,
            gamma,
            temperature,
        }
    }

    #[test]
    fn the_identity_has_no_tables() {
        assert!(luts(&Color::default()).is_none());
    }

    #[test]
    fn contrast_and_saturation_alone_take_the_fixed_point_path() {
        // contrast 1.2 -> 4915 / 4096; brightness (100*0+100)*511/200 - 128 - 153.
        let [y, u, _] = plane_params(&color(0.0, 1.2, 1.5, 1.0, 0.0));
        let ly = plane_lut(&y).unwrap();
        // Hand-evaluated process_c: ((i * 4915) >> 12) + (255 - 128 - 153).
        for i in [0usize, 16, 128, 235, 255] {
            let want = (((i as i32 * 4915) >> 12) - 26).clamp(0, 255) as u8;
            assert_eq!(ly[i], want, "Y[{i}]");
        }
        // Saturation 1.5 scales U about the neutral 128 (not exactly: the
        // formula's brightness term is quantised, as in FFmpeg).
        let lu = plane_lut(&u).unwrap();
        assert!(lu[200] > lu[128] && lu[60] < lu[128]);
        assert!((i32::from(lu[128]) - 128).abs() <= 2);
    }

    #[test]
    fn gamma_takes_the_pow_table_with_truncation() {
        let [y, _, _] = plane_params(&color(0.0, 1.0, 1.0, 2.0, 0.0));
        let l = plane_lut(&y).unwrap();
        assert_eq!(l[0], 0);
        assert_eq!(l[255], 255);
        // pow(i/255, 1/2) * 256, truncated.
        assert_eq!(l[64], (256.0 * (64.0f64 / 255.0).sqrt()) as u8);
    }

    #[test]
    fn temperature_is_a_chroma_power_not_an_rgb_gain() {
        // Warm (+): U goes down, V up — only the chroma planes move.
        let t = luts(&color(0.0, 1.0, 1.0, 1.0, 0.5)).unwrap();
        assert!((0..256).all(|i| t[0][i] == i as u8), "luma untouched");
        assert!(t[1][100] < 100 && t[2][100] > 100, "U lowered, V raised below mid");
        let cool = luts(&color(0.0, 1.0, 1.0, 1.0, -0.5)).unwrap();
        assert!(cool[1][100] > 100 && cool[2][100] < 100);
    }

    #[test]
    fn out_of_range_values_are_clamped_like_av_clipf() {
        // Saturation is capped at 3 and gamma at [0.1, 10].
        let a = plane_params(&color(0.0, 1.0, 9.0, 1.0, 0.0));
        assert_eq!(a[1].contrast, 3.0);
        let b = plane_params(&color(0.0, 1.0, 1.0, 50.0, 0.0));
        assert_eq!(b[0].gamma, 10.0);
    }
    /// The steepest step of each table, looked at directly.
    fn steps_of_the_tables(c: &Color) -> [u32; 3] {
        match luts(c) {
            None => [1; 3],
            Some(t) => t.map(|l| l.windows(2).map(|w| u32::from(w[0].abs_diff(w[1]))).max().unwrap()),
        }
    }

    #[test]
    fn the_steps_are_those_of_the_tables_the_compositor_applies() {
        assert_eq!(table_steps(&Color::default()), [1, 1, 1]);
        // A grid over the model's ranges (contrast to 4, saturation to 3, gamma 0.1 to 10, brightness +-1), both of
        // `eq`'s paths (the fixed-point one at gamma 1, the `pow` table otherwise) and the temperature's chroma power.
        let mut seen = 0;
        for brightness in [-1.0, -0.3, 0.0, 0.08, 0.3, 1.0] {
            for contrast in [0.0, 0.5, 1.0, 1.1, 1.8, 2.0, 2.05, 4.0, -1.5] {
                for saturation in [0.0, 1.0, 1.9, 2.0, 3.0] {
                    for gamma in [0.1, 0.5, 1.0, 1.25, 2.0, 10.0] {
                        for temperature in [-1.0, 0.0, 0.4] {
                            let c = color(brightness, contrast, saturation, gamma, temperature);
                            assert_eq!(table_steps(&c), steps_of_the_tables(&c), "{c:?}");
                            seen += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(seen, 6 * 9 * 5 * 6 * 3);
    }

    #[test]
    fn a_black_point_above_the_bottom_is_a_step_of_its_own() {
        // Contrast 1.8 and gamma 2: everything under Y 57 is 0 and the table leaves it for 12, then 24.
        let steep = color(0.0, 1.8, 1.0, 2.0, 0.0);
        let l = plane_lut(&plane_params(&steep)[0]).unwrap();
        assert_eq!((l[56], l[57], l[58]), (0, 12, 24));
        assert_eq!(table_steps(&steep), [12, 1, 1]);
        // A brightness of 0.3 on top moves the black point to Y 15: 0, 19, 29.
        let lifted = color(0.3, 1.8, 1.0, 2.0, 0.0);
        let l = plane_lut(&plane_params(&lifted)[0]).unwrap();
        assert_eq!((l[14], l[15], l[16]), (0, 19, 29));
        assert_eq!(table_steps(&lifted)[0], 19);
        // The same gamma with the black point clear of the table (a brightness that keeps every entry above 0) has no cliff.
        assert_eq!(table_steps(&color(0.3, 1.0, 1.0, 2.0, 0.0)), [1, 1, 1]);
        // A gamma above 1 steepens toward black without bound: the first entry's step is 256 * (1/255)^(1/gamma).
        let mut last = 0;
        for gamma in [1.1, 1.25, 1.5, 2.0, 3.0, 10.0] {
            let y = table_steps(&color(0.0, 1.0, 1.0, gamma, 0.0))[0];
            assert!(y >= last, "gamma {gamma}: {y} after {last}");
            last = y;
        }
        assert!(last > 100, "{last}");
        // A negative brightness parks the black point higher, and gamma 1.5 makes the cliff 4 levels (gamma 1.2: 2).
        assert_eq!(table_steps(&color(-0.3, 1.0, 1.0, 1.5, 0.0))[0], 4);
        assert_eq!(table_steps(&color(-0.3, 1.0, 1.0, 1.2, 0.0))[0], 2);
    }

    #[test]
    fn the_fixed_point_path_steps_by_the_contrast_rounded_up_and_chroma_by_saturation_and_temperature() {
        // `(i * contrast) >> 12` over 16.4-bit contrast: neighbouring entries differ by the contrast rounded up at most.
        for (contrast, step) in [(0.5, 1), (1.0, 1), (1.5, 2), (2.0, 2), (2.05, 3), (3.0, 3), (4.0, 4)] {
            assert_eq!(
                table_steps(&color(0.0, contrast, 1.0, 1.0, 0.0))[0],
                step,
                "contrast {contrast}"
            );
        }
        // Saturation is the chroma tables' contrast; a flat table (saturation 0) does not step at all.
        for (saturation, step) in [(0.0, 0), (1.0, 1), (2.0, 2), (2.05, 3), (3.0, 3)] {
            let [y, u, v] = table_steps(&color(0.0, 1.0, saturation, 1.0, 0.0));
            assert_eq!((y, u, v), (1, step, step), "saturation {saturation}");
        }
        // The temperature is a power of the chroma toward 1 and leaves the luma alone: slope just over 1, steps of 2 at most.
        for temperature in [-1.0, -0.3, 0.3, 1.0] {
            assert_eq!(
                table_steps(&color(0.0, 1.0, 1.0, 1.0, temperature)),
                [1, 2, 2],
                "temperature {temperature}"
            );
        }
        assert_eq!(table_steps(&color(0.0, 1.0, 2.0, 1.0, 1.0)), [1, 3, 3]);
    }

    #[test]
    fn the_answers_kept_per_thread_are_the_tables_whatever_the_order_of_the_questions() {
        let colours: Vec<Color> = (0..11)
            .map(|i| {
                color(
                    0.02 * f64::from(i),
                    1.0 + 0.1 * f64::from(i % 5),
                    1.0,
                    1.0 + 0.07 * f64::from(i % 3),
                    0.0,
                )
            })
            .collect();
        for round in 0..3 {
            for c in colours.iter().rev().chain(&colours).chain(&colours[..2]) {
                assert_eq!(table_steps(c), steps_of_the_tables(c), "round {round}: {c:?}");
            }
        }
    }
}
