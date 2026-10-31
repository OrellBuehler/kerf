//! The blurred fill's `boxblur`, as a scalar reference: `vf_boxblur.c`'s `blur8` ported as the C writes it (a running
//! sum, mirrored at both ends), which the compositor's pass (`planeops.wgsl`'s `fs_box`, the same sum taken whole) and
//! the plan's geometry ([`kerf_core::blur_fill`]) are held to in the tests.

/// One line through `blur8`: `len` samples at `radius`, every step the filter's own integer arithmetic. The caller
/// guarantees `2 * radius < len` (a shorter line is read past its end by FFmpeg).
fn blur_line(src: &[u8], radius: usize) -> Vec<u8> {
    let len = src.len();
    let length = (radius * 2 + 1) as i32;
    let inv = ((1 << 16) + length / 2) / length;
    let at = |i: usize| i32::from(src[i]);
    let mut sum = at(radius);
    for x in 0..radius {
        sum += at(x) << 1;
    }
    sum = sum * inv + (1 << 15);
    let mut out = vec![0u8; len];
    let mut x = 0;
    while x <= radius {
        sum += (at(radius + x) - at(radius - x)) * inv;
        out[x] = (sum >> 16) as u8;
        x += 1;
    }
    while x < len - radius {
        sum += (at(radius + x) - at(x - radius - 1)) * inv;
        out[x] = (sum >> 16) as u8;
        x += 1;
    }
    while x < len {
        sum += (at(2 * len - radius - x - 1) - at(x - radius - 1)) * inv;
        out[x] = (sum >> 16) as u8;
        x += 1;
    }
    out
}

/// `boxblur=radius:power` on one `w` x `h` plane: `power` passes along every row, then `power` along every column
/// (`hblur` for each plane and `vblur` after it). `None` where FFmpeg would read past a line (`2 * radius >= w` or `h`).
pub fn box_blur(plane: &[u8], w: usize, h: usize, radius: usize, power: usize) -> Option<Vec<u8>> {
    if plane.len() != w * h || radius == 0 || 2 * radius >= w || 2 * radius >= h {
        return None;
    }
    let mut cur = plane.to_vec();
    for _ in 0..power {
        for y in 0..h {
            let line = blur_line(&cur[y * w..(y + 1) * w], radius);
            cur[y * w..(y + 1) * w].copy_from_slice(&line);
        }
    }
    for _ in 0..power {
        for x in 0..w {
            let column: Vec<u8> = (0..h).map(|y| cur[y * w + x]).collect();
            let line = blur_line(&column, radius);
            for y in 0..h {
                cur[y * w + x] = line[y];
            }
        }
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_line_stays_flat_and_an_impulse_spreads_over_the_box() {
        let flat = vec![90u8; 20];
        assert_eq!(blur_line(&flat, 3), flat);
        let mut impulse = vec![0u8; 21];
        impulse[10] = 255;
        let out = blur_line(&impulse, 2);
        // Five samples of the box share the 255: 255 / 5 = 51, rounded at bit 16.
        assert_eq!(&out[8..13], &[51, 51, 51, 51, 51]);
        assert_eq!((out[7], out[13]), (0, 0));
    }

    #[test]
    fn the_edge_sample_is_mirrored_about_the_edge_including_itself() {
        // A step at the start: the window of sample 0 holds sample 0 twice (index -1 reads 0) and then samples 1, 2.
        let src = [200u8, 0, 0, 0, 0, 0, 0, 0];
        let out = blur_line(&src, 2);
        // window of 0: [1,0 | 0,1,2] -> 200 + 200 + 0 + 0 + 0 = 400 over 5 = 80
        assert_eq!(out[0], 80);
        // window of 1: [-1 -> 0, 0, 1, 2, 3] = 200 + 200 + 0 + 0 + 0
        assert_eq!(out[1], 80);
        // window of 2: [0,1,2,3,4] = 200
        assert_eq!(out[2], 40);
    }

    #[test]
    fn a_box_too_wide_for_the_plane_is_not_blurred() {
        assert!(box_blur(&[0; 16], 4, 4, 2, 2).is_none());
        assert!(box_blur(&[0; 25], 5, 5, 2, 2).is_some());
        assert!(box_blur(&[0; 16], 4, 4, 0, 2).is_none());
    }
}
