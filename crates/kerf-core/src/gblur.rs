//! The numbers FFmpeg's `gblur` (`libavfilter/vf_gblur.c`) derives from a sigma, and which of them the GPU compositor's blur
//! reproduces.
//!
//! It lives in `kerf-core` because the plan decides what is drawn: [`RenderPlan::reasons`](crate::render_plan::RenderPlan) refuses a
//! blur whose derived coefficients are outside [`Gaussian::in_shader_domain`], from the very function the compositor takes its
//! coefficients from ([`gaussian`]), so the two cannot disagree.

/// `gblur`'s numbers for one sigma (the same in both directions: Kerf never sets `sigmaV`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gaussian {
    /// The recursion's coefficient.
    pub nu: f32,
    /// The edge samples' extra weight (`boundaryscale`).
    pub boundary: f32,
    /// The gain applied once at the end: the horizontal and the vertical `postscale`, multiplied.
    pub post: f32,
}

impl Gaussian {
    /// Whether the compositor's blur reproduces a filter with these coefficients to the sample: `nu` in `[0, 1)`, a positive finite
    /// `boundaryscale` and a non-negative finite gain. The shader's fused multiply-add works on the bit patterns of non-negative finite
    /// floats (subnormals and underflow included) and its recursion is then only ever a sum of non-negative terms that cannot overflow.
    ///
    /// `set_params` derives `nu` as `(1 + 2 lambda - sqrt(1 + 4 lambda)) / (2 lambda)` in `double`: for a sigma of about 1e-8, `lambda` is
    /// near the spacing of the doubles around 1 and the difference cancels, so `nu` comes out as large as 1.99 (and `boundaryscale`,
    /// `1 / (1 - nu)`, negative). Mathematically `nu` is in `(0, 1)`; what the filter computes is not, and that is what this tests.
    pub fn in_shader_domain(&self) -> bool {
        (0.0..1.0).contains(&self.nu)
            && self.boundary.is_finite()
            && self.boundary > 0.0
            && self.post.is_finite()
            && self.post >= 0.0
    }
}

/// `set_params` of `vf_gblur.c`, in the filter's own precision: `sigma` is an option of type
/// `float`; `lambda` and `dnu` are computed in `double`; the three results are stored as `float`
/// and any that is not a normal number (zero, subnormal, infinite, NaN) is replaced
/// (`postscale` and `boundaryscale` by 1, `nu` by 0). `None` for a sigma `gblur` refuses (it takes 0 to 1024).
///
/// This is the filter's own answer, whatever it is: see [`gaussian`] for the one the compositor draws.
pub fn gblur_coefficients(sigma: f64) -> Option<Gaussian> {
    if !sigma.is_finite() || !(0.0..=1024.0).contains(&sigma) {
        return None;
    }
    let sigma = sigma as f32;
    let steps = 1.0f64;
    let lambda = f64::from(sigma * sigma) / (2.0 * steps);
    let dnu = (1.0 + 2.0 * lambda - (1.0 + 4.0 * lambda).sqrt()) / (2.0 * lambda);
    let mut post = (dnu / lambda).powf(steps) as f32;
    let mut boundary = (1.0 / (1.0 - dnu)) as f32;
    let mut nu = dnu as f32;
    if !post.is_normal() {
        post = 1.0;
    }
    if !boundary.is_normal() {
        boundary = 1.0;
    }
    if !nu.is_normal() {
        nu = 0.0;
    }
    // (`filter_postscale` multiplies the horizontal and the vertical factor, two floats.)
    Some(Gaussian {
        nu,
        boundary,
        post: post * post,
    })
}

/// The coefficients of a sigma the compositor draws: [`gblur_coefficients`] when they are in [`Gaussian::in_shader_domain`], else
/// `None` (a sigma `gblur` refuses, or one whose coefficients the filter's own cancellation puts outside the shader's domain: about
/// 1.05e-8 to 1.29e-8). The plan refuses exactly the sigmas this answers `None` for.
pub fn gaussian(sigma: f64) -> Option<Gaussian> {
    gblur_coefficients(sigma).filter(Gaussian::in_shader_domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sigma_gblur_refuses_has_no_coefficients() {
        for sigma in [-1.0, 1024.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(gblur_coefficients(sigma), None, "{sigma}");
            assert_eq!(gaussian(sigma), None, "{sigma}");
        }
    }

    #[test]
    fn sigma_zero_and_the_ends_of_the_range_are_in_the_domain() {
        let zero = gaussian(0.0).expect("sigma 0 is the identity");
        assert_eq!((zero.nu, zero.boundary, zero.post), (0.0, 1.0, 1.0));
        for sigma in [1e-9, 1e-6, 1e-3, 0.5, 1.0, 9.0, 250.0, 1023.99, 1024.0] {
            let g = gaussian(sigma).unwrap_or_else(|| panic!("{sigma}"));
            assert!(g.in_shader_domain(), "{sigma}: {g:?}");
        }
    }

    /// The cancellation: for these sigmas the filter's own coefficients are outside the shader's domain, and [`gaussian`] says no where
    /// [`gblur_coefficients`] has a (wild) answer. Found by sweeping, not assumed: the band is where `nu` leaves `[0, 1)`.
    #[test]
    fn the_sigmas_whose_derivation_cancels_are_outside_the_shaders_domain() {
        let mut outside = Vec::new();
        for i in 0..40_000 {
            let sigma = 10f64.powf(-8.0 + 11.0 * f64::from(i) / 40_000.0);
            let g = gblur_coefficients(sigma).expect("in range");
            assert_eq!(g.in_shader_domain(), gaussian(sigma).is_some(), "{sigma}: {g:?}");
            if !g.in_shader_domain() {
                outside.push((sigma, g));
            }
        }
        assert!(!outside.is_empty(), "the sweep did not meet the cancellation band");
        let (first, last) = (outside[0].0, outside[outside.len() - 1].0);
        assert!(
            (1.0e-8..1.4e-8).contains(&first) && (1.0e-8..1.4e-8).contains(&last),
            "the band is at {first:e} to {last:e}"
        );
        assert!(
            outside.iter().any(|(_, g)| g.nu > 1.9) && outside.iter().any(|(_, g)| g.boundary < -2.5),
            "the band reaches a nu above 1.9 and a boundaryscale below -2.5: {:?}",
            outside.iter().map(|(_, g)| *g).take(3).collect::<Vec<_>>()
        );
        for (sigma, g) in &outside {
            assert_eq!(gaussian(*sigma), None, "{sigma}: {g:?}");
        }
    }

    /// Everywhere else in 0 to 1024 the coefficients are in the domain, and the mathematics holds: `nu` below 1, the scales positive.
    #[test]
    fn every_other_sigma_is_in_the_domain() {
        for i in 0..20_000 {
            let sigma = 1.4e-8 + 1023.99 * f64::from(i) / 20_000.0;
            let g = gaussian(sigma).unwrap_or_else(|| panic!("{sigma}"));
            assert!(g.nu < 1.0 && g.boundary >= 1.0 && g.post >= 0.0, "{sigma}: {g:?}");
        }
        for i in 0..2_000 {
            let sigma = 10f64.powf(-12.0 + 3.6 * f64::from(i) / 2_000.0);
            if (1.0e-8..1.4e-8).contains(&sigma) {
                continue;
            }
            assert!(gaussian(sigma).is_some(), "{sigma}");
        }
    }
}
