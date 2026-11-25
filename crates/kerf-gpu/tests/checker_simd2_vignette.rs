//! `checker/simd2/…`: `effects::vignette_map` (each distinct `|yy|` row worked out once over the distinct `|xx|`, a row
//! of a magnitude met before copied) held to `vignette.c`'s `get_natural_factor` evaluated for every sample, bit for
//! bit: `xx = (int)((x - x0) * xscale)`, `hypot(xx, yy) / dmax`, `cos(angle * d)` to the fourth power as a `float`. The
//! model's `Vignette` has no parameters (centre, angle and aspect are the filter's defaults), so the map is a function
//! of the picture's size and these are the sizes. No adapter, no FFmpeg. Always run.

use kerf_gpu::effects::vignette_map;

/// Straight from the filter's C source, one sample at a time, with nothing shared with `effects.rs`: `av_clipf` hands
/// the angle back as a `float`.
fn natural(w: u32, h: u32) -> Vec<f32> {
    let angle = f64::from((std::f64::consts::PI / 5.0) as f32);
    let (x0, y0) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let dmax = (f64::from(w) / 2.0).hypot(f64::from(h) / 2.0);
    let mut out = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h {
        for x in 0..w {
            let xx = (f64::from(x) - x0) as i32;
            let yy = (f64::from(y) - y0) as i32;
            let dnorm = f64::from(xx).hypot(f64::from(yy)) / dmax;
            let f = if dnorm > 1.0 {
                0.0
            } else {
                let c = (angle * dnorm).cos();
                (c * c) * (c * c)
            };
            out.push(f as f32);
        }
    }
    out
}

#[track_caller]
fn same(w: u32, h: u32) {
    let (got, want) = (vignette_map(w, h), natural(w, h));
    assert_eq!(got.len(), want.len(), "{w}x{h}");
    let mut differing = 0usize;
    let mut first = None;
    for (i, (a, b)) in got.iter().zip(&want).enumerate() {
        if a.to_bits() != b.to_bits() {
            differing += 1;
            first.get_or_insert((i as u32 % w, i as u32 / w, *a, *b));
        }
    }
    assert_eq!(
        differing, 0,
        "{w}x{h}: samples whose bits differ, the first (x, y, got, want) {first:?}"
    );
}

#[test]
fn checker_simd2_vignette_every_size_from_1_to_70_on_both_axes() {
    for w in 1..=70 {
        for h in 1..=70 {
            same(w, h);
        }
    }
}

#[test]
fn checker_simd2_vignette_slivers_and_odd_pictures() {
    for (w, h) in [
        (1, 4096),
        (4096, 1),
        (2, 4097),
        (4097, 2),
        (3, 8191),
        (65_535, 1),
        (1, 65_535),
        (101, 99),
        (99, 101),
        (127, 129),
        (255, 257),
        (256, 255),
        (640, 360),
        (639, 359),
        (641, 361),
        (960, 540),
        (203, 101),
        (1080, 1920),
        (1079, 1919),
    ] {
        same(w, h);
    }
}

#[test]
fn checker_simd2_vignette_pictures_of_4k_and_a_pixel_off_it() {
    for (w, h) in [(3840, 2160), (3839, 2159), (3841, 2161), (4096, 2160), (4095, 2161)] {
        same(w, h);
    }
}

#[test]
fn checker_simd2_vignette_rows_and_columns_of_one_magnitude_agree_and_the_map_has_the_right_shape() {
    for (w, h) in [(7u32, 9u32), (8, 10), (64, 63), (97, 55), (1920, 1080), (1919, 1081)] {
        let map = vignette_map(w, h);
        assert_eq!(map.len(), w as usize * h as usize);
        let row = |y: u32| &map[y as usize * w as usize..][..w as usize];
        let (x0, y0) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        let mut seen = std::collections::HashMap::new();
        for y in 0..h {
            let first = *seen.entry(((f64::from(y) - y0) as i32).unsigned_abs()).or_insert(y);
            assert!(
                row(y).iter().zip(row(first)).all(|(p, q)| p.to_bits() == q.to_bits()),
                "{w}x{h}: rows {first} and {y} have one offset magnitude"
            );
        }
        for y in [0, h / 2, h - 1] {
            let mut seen = std::collections::HashMap::new();
            for x in 0..w {
                let first = *seen.entry(((f64::from(x) - x0) as i32).unsigned_abs()).or_insert(x);
                assert_eq!(
                    row(y)[x as usize].to_bits(),
                    row(y)[first as usize].to_bits(),
                    "{w}x{h}: row {y}, columns {first} and {x}"
                );
            }
        }
        let (mid, c) = (row(h / 2), (w / 2) as usize);
        assert!(mid[c..].windows(2).all(|p| p[0] >= p[1]), "{w}x{h}: falling off to the right");
        assert!(mid[..=c].windows(2).all(|p| p[0] <= p[1]), "{w}x{h}: rising from the left");
        assert_eq!(mid[c], 1.0, "{w}x{h}: the centre is untouched");
    }
}

#[test]
fn checker_simd2_vignette_no_picture_is_no_map() {
    assert!(vignette_map(0, 0).is_empty() && vignette_map(0, 9).is_empty() && vignette_map(9, 0).is_empty());
}
