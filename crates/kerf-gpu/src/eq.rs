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

use kerf_core::Color;

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
}
