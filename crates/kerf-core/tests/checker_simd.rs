//! `checker/simd/…`: an independent differential check of `kerf_core::simd` and the vector rows of
//! `mask_alpha_plane`. Every path this CPU can run is held to a naive loop, to the kept scalar functions and, for
//! a mask, to the `geq` expression evaluated sample by sample with the exact `hypot` everywhere. Always run.

mod checker_simd_cases;

use checker_simd_cases as c;
use kerf_core::simd::{self, Path};

#[test]
fn checker_simd_scans_every_length_and_position() {
    c::scan_every_length_every_position(300);
}

#[test]
fn checker_simd_scans_on_misaligned_sub_slices_of_a_guarded_buffer() {
    c::scan_misaligned_sub_slices(300);
}

#[test]
fn checker_simd_scans_random_runs() {
    c::scan_random(60_000, 300);
    c::scan_random(400, 70_000);
}

#[test]
fn checker_simd_scans_every_byte_value_as_the_odd_one() {
    c::scan_every_value_as_the_odd_one();
}

#[test]
fn checker_simd_scans_big_buffers() {
    c::scan_big(1 << 16, true);
    c::scan_big((1 << 18) + 37, true);
    c::scan_big(4096 * 2160 + 5, false);
}

#[test]
fn checker_simd_bounds_single_pixels_and_pairs() {
    c::bounds_single_pixels_and_pairs();
}

#[test]
fn checker_simd_bounds_patterns() {
    c::bounds_patterns(true);
}

#[test]
fn checker_simd_bounds_tail_and_short_planes() {
    c::bounds_ignore_the_tail_and_refuse_a_short_plane();
}

#[test]
fn checker_simd_with_path_restores_after_a_panic() {
    c::with_path_restores_after_a_panic();
}

#[test]
fn checker_simd_mask_every_feather_every_width() {
    c::mask_every_feather_every_width();
}

#[test]
fn checker_simd_mask_centres_and_sizes_including_the_ones_the_model_clamps() {
    c::mask_centres_and_sizes(37, 11);
    c::mask_centres_and_sizes(64, 3);
}

#[test]
fn checker_simd_mask_random() {
    c::mask_random(1500, 130);
}

#[test]
fn checker_simd_mask_dense_feather_sweep() {
    c::mask_dense_feather_sweep();
}

#[test]
fn checker_simd_mask_every_size() {
    c::mask_every_size();
}

#[test]
fn checker_simd_mask_picture_sizes() {
    c::mask_picture_sizes(
        &[(1920, 1080), (480, 270), (401, 7), (3, 600), (1, 4096), (4097, 3), (1023, 17)],
        6,
    );
}

#[test]
fn checker_simd_mask_4k() {
    c::mask_picture_sizes(&[(3840, 2160), (4096, 2160), (4095, 2161)], 1);
}

#[test]
fn checker_simd_mask_empty_pictures() {
    c::mask_empty_pictures();
}

#[test]
fn checker_simd_mask_from_threads_that_pick_for_themselves() {
    c::mask_from_threads_that_pick_for_themselves();
}

/// `KERF_SIMD` caps the path of a process, read once: this test runs the binary again (`--exact` on the child
/// below) with each value and checks the path it settles on.
#[test]
fn checker_simd_the_env_cap_picks_the_path_of_a_process() {
    let exe = std::env::current_exe().unwrap();
    let have = simd::available();
    let widest = *have.last().unwrap();
    let pick = |rank: &[Path]| *rank.iter().rev().find(|p| have.contains(p)).unwrap();
    let cases: Vec<(Option<&str>, Path)> = vec![
        (None, widest),
        (Some("scalar"), Path::Scalar),
        (Some("SCALAR"), Path::Scalar),
        (Some("off"), Path::Scalar),
        (Some("none"), Path::Scalar),
        (Some("0"), Path::Scalar),
        (Some(" scalar "), Path::Scalar),
        (Some("sse2"), pick(&[Path::Scalar, Path::Sse2, Path::Neon])),
        (Some("neon"), pick(&[Path::Scalar, Path::Sse2, Path::Neon])),
        (Some("avx2"), widest),
        (Some("AVX2"), widest),
        (Some(""), widest),
        (Some("avx512"), widest),
        (Some("banana"), widest),
    ];
    for (value, want) in cases {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "--exact",
            "checker_simd_child_reports_its_path",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        cmd.env_remove("KERF_SIMD");
        if let Some(v) = value {
            cmd.env("KERF_SIMD", v);
        }
        cmd.env("CHECKER_SIMD_EXPECT", format!("{want:?}"));
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "KERF_SIMD={value:?} expected {want:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("1 passed"),
            "KERF_SIMD={value:?}: the child did not run: {text}"
        );
    }
}

#[test]
#[ignore = "run by the test above with KERF_SIMD set"]
fn checker_simd_child_reports_its_path() {
    let want = std::env::var("CHECKER_SIMD_EXPECT").expect("CHECKER_SIMD_EXPECT");
    assert_eq!(
        format!("{:?}", simd::path()),
        want,
        "KERF_SIMD={:?}",
        std::env::var("KERF_SIMD")
    );
    // The path a process picked is the one its kernels run: the hot calls agree with the scalar functions.
    let m = kerf_core::model::Mask::default();
    assert!(kerf_core::mask_alpha_plane(&m, 211, 13) == kerf_core::mask_plane::mask_alpha_plane_scalar(&m, 211, 13));
    let mut d = vec![0u8; 1000];
    d[777] = 1;
    assert_eq!(simd::position_ne(&d, 0), Some(777));
}
