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

// ---- where gblur's own kernel is not the recursion --------------------------------------------------------------------------------

/// What FFmpeg's `gblur` does on a plane whose shape its x86 kernels (`x86/vf_gblur.asm`, the same in 6.1 and 9.0) were not written for.
/// The AVX2 and AVX-512 kernels work a block of `lanes` rows or columns at a time (8 and 16) with loops that run once before they test,
/// and `filter_postscale` cuts the buffer into `min(width * height, threads)` slices of `height * job / jobs` rows. Each of these is a
/// property of the kernel that no sigma changes, and each reads FFmpeg's answer off the model by tens of levels (or crashes it): the
/// compositor draws the recursion, so a blur on such a plane is FFmpeg's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelHazard {
    /// A plane one sample wide or tall: the horizontal kernel's loop over `width - 1` samples and the vertical kernel's loop over the
    /// rows below the first both run once too many, into the next row's or the buffer's padding (a plane of width 1 can corrupt the heap).
    OneSample,
    /// More slice threads than rows (`threads > height`): the postscale pass has `min(width * height, threads)` slices, a slice of no rows
    /// still scales one whole vector of the buffer from its start, and the first slice is one of them, so the first `lanes` samples are
    /// scaled twice (a picture 0.65 times as bright over its first row, at sigma 0.5).
    EmptySlices,
    /// A slice of `lanes` rows or more over a plane `lanes` samples wide or fewer: the blocked horizontal kernel runs its block of
    /// `lanes` columns once before it asks whether the row is longer than that, and reads and writes the samples past the row's end.
    NarrowBlocks,
}

impl KernelHazard {
    /// The refusal's words, for a plan: what the kernel does wrong.
    pub fn reason(self) -> &'static str {
        match self {
            Self::OneSample => "a blur on a plane one sample wide or tall, where gblur's loops run once too many",
            Self::EmptySlices => {
                "a blur on a plane with fewer rows than the graph has slice threads, where gblur's empty postscale slice scales a vector again"
            }
            Self::NarrowBlocks => {
                "a blur on a plane no wider than gblur's vector over a slice of a vector of rows, where its blocked loop runs past the row"
            }
        }
    }
}

impl std::fmt::Display for KernelHazard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// The vector widths of `gblur`'s AVX kernels (floats): AVX2's and AVX-512's. `0` in a caller's `lanes` is "not known which", and
/// is judged against both.
pub const KERNEL_LANES: [u32; 2] = [8, 16];

/// Whether `gblur`'s kernel (`lanes` wide) is not the recursion on a `plane` of `width` x `height` samples run on `threads` slice threads
/// ([`KernelHazard`]). Derived from the kernel (`vf_gblur.c`'s slicing and `vf_gblur.asm`'s loops) and held, plane by plane, against
/// FFmpeg 6.1 and 9.0 on both kernels over every shape from 1x1 to 100x256 and every thread count from 1 to 64
/// (`tests/blur_slices.rs`).
pub fn plane_hazard((width, height): (u32, u32), threads: u32, lanes: u32) -> Option<KernelHazard> {
    if width <= 1 || height <= 1 {
        return Some(KernelHazard::OneSample);
    }
    let threads = threads.max(1);
    if threads > height {
        return Some(KernelHazard::EmptySlices);
    }
    // The horizontal pass gives a slice `height * job / jobs` to `height * (job + 1) / jobs` rows: at most `ceil(height / jobs)`.
    let jobs = height.min(threads);
    (width <= lanes && height.div_ceil(jobs) >= lanes).then_some(KernelHazard::NarrowBlocks)
}

/// Where a picture's blur meets a [`KernelHazard`]: the first plane and thread count that does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureHazard {
    pub hazard: KernelHazard,
    /// `"luma"` or `"chroma"`.
    pub plane: &'static str,
    pub dims: (u32, u32),
    pub threads: u32,
}

impl std::fmt::Display for PictureHazard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} plane {}x{} on {} slice thread{}: {}",
            self.plane,
            self.dims.0,
            self.dims.1,
            self.threads,
            if self.threads == 1 { "" } else { "s" },
            self.hazard
        )
    }
}

/// [`plane_hazard`] for the planes of a 4:2:0 `picture` (luma, and the chroma planes at half size, rounded up) at every thread
/// count of `threads` (the range a graph can run on: [`crate::cpu_graph_thread_range`]) and every kernel width `lanes` could mean
/// (`0`: both): the first plane and thread count at which `gblur` is not the recursion.
pub fn picture_hazard((width, height): (u32, u32), threads: std::ops::RangeInclusive<u32>, lanes: u32) -> Option<PictureHazard> {
    let widths: &[u32] = match lanes {
        0 => &KERNEL_LANES,
        8 => &KERNEL_LANES[..1],
        _ => &KERNEL_LANES[1..],
    };
    let planes = [("luma", (width, height)), ("chroma", (width.div_ceil(2), height.div_ceil(2)))];
    for t in threads {
        for (plane, dims) in planes {
            for &l in widths {
                if let Some(hazard) = plane_hazard(dims, t, l) {
                    return Some(PictureHazard {
                        hazard,
                        plane,
                        dims,
                        threads: t,
                    });
                }
            }
        }
    }
    None
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

    #[test]
    fn a_plane_is_judged_by_its_slices_and_the_kernels_block() {
        use KernelHazard::*;
        // One sample wide or tall: every kernel, every thread count.
        for (plane, t, l) in [((1, 40), 1, 16), ((40, 1), 3, 8), ((1, 1), 64, 16), ((0, 4), 1, 8)] {
            assert_eq!(plane_hazard(plane, t, l), Some(OneSample), "{plane:?} {t} {l}");
        }
        // More threads than rows: the postscale pass has empty slices, whatever the width.
        assert_eq!(plane_hazard((100, 7), 8, 16), Some(EmptySlices));
        assert_eq!(plane_hazard((100, 8), 8, 16), None);
        assert_eq!(plane_hazard((2, 2), 3, 8), Some(EmptySlices));
        assert_eq!(plane_hazard((2, 2), 2, 8), None);
        // A slice of a block of rows over a plane no wider than a block of columns.
        assert_eq!(plane_hazard((16, 16), 1, 16), Some(NarrowBlocks));
        assert_eq!(plane_hazard((17, 16), 1, 16), None);
        assert_eq!(plane_hazard((16, 15), 1, 16), None);
        assert_eq!(plane_hazard((8, 8), 1, 8), Some(NarrowBlocks));
        assert_eq!(plane_hazard((9, 8), 1, 8), None);
        assert_eq!(plane_hazard((8, 8), 1, 16), None, "AVX-512 works 16 rows at a time");
        // The largest slice decides: 31 rows over two threads are 15 and 16.
        assert_eq!(plane_hazard((4, 31), 2, 16), Some(NarrowBlocks));
        assert_eq!(plane_hazard((4, 30), 2, 16), None);
        assert_eq!(plane_hazard((4, 241), 16, 16), Some(NarrowBlocks));
        assert_eq!(plane_hazard((4, 240), 16, 16), None);
        // The kernel width not known: both.
        assert_eq!(plane_hazard((12, 16), 1, 16), Some(NarrowBlocks));
        assert!(picture_hazard((24, 32), 1..=1, 0).is_some());
        assert!(picture_hazard((24, 32), 1..=1, 16).is_some(), "chroma 12x16");
        assert!(picture_hazard((24, 32), 1..=1, 8).is_none());
    }

    #[test]
    fn a_picture_is_judged_over_its_chroma_and_over_every_thread_count_it_may_run_on() {
        // 640x360: no plane is hazardous at any thread count up to FFmpeg's default ceiling, nor at 64.
        assert_eq!(picture_hazard((640, 360), 1..=64, 0), None);
        // A chroma plane (rounded up) one row short of the threads.
        assert!(picture_hazard((640, 14), 8..=8, 0).is_some_and(|w| w.to_string().contains("chroma plane 320x7")));
        assert_eq!(picture_hazard((640, 16), 8..=8, 0), None);
        // An odd picture's chroma is rounded up: 15 rows are 8 rows of chroma.
        assert_eq!(picture_hazard((640, 15), 8..=8, 0), None);
        // The range: the picture is judged at each count (a plane clear at 5 threads is not at 16).
        assert_eq!(picture_hazard((640, 30), 5..=5, 0), None);
        assert!(picture_hazard((640, 30), 5..=16, 0).is_some());
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
