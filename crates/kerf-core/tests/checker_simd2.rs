//! `checker/simd2/…`: a rectangle mask's rows (a ramp per column, a value per row, a byte `min`, or `max` when
//! inverted) held to `mask_alpha_plane_scalar` byte for byte, on every path, and to the independent `geq`
//! evaluation. The `checker_simd.rs` mask cases run on the same code too. Always run.

mod checker_simd_cases;

use checker_simd_cases as c;

#[test]
fn checker_simd2_rect_the_identity_holds_in_the_geq_oracle() {
    c::rect_identity_holds_in_the_geq_oracle(1500);
}

#[test]
fn checker_simd2_rect_random_masks_at_small_sizes() {
    c::rect_random(2500, 130);
}

#[test]
fn checker_simd2_rect_edges_on_every_phase_of_a_sample() {
    c::rect_edges_on_every_phase();
}

#[test]
fn checker_simd2_rect_one_pixel_and_huge_planes() {
    c::rect_one_pixel_and_huge_planes();
}

#[test]
fn checker_simd2_rect_big_pictures() {
    c::rect_big_pictures();
}
