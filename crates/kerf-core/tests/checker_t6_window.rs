//! The moving still's geometry, checked on its own (no FFmpeg): the scaled copy has a pixel for every pixel of the frame, the window cut
//! from it holds every sample the `perspective` reads (and the sample after it, for the bilinear read), and the corners are the
//! expressions' for the pose. The export and the compositor build the copy from the same function, so a parity case cannot tell a window
//! that is too small from one that is right (both clamp at its edge alike): this can.

use kerf_core::still_motion::{StillMotion, StillPose, StillSample, StillStatic, HEADROOM};
use kerf_core::Fit;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }

    fn int(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() * f64::from(hi - lo + 1)) as u32
    }
}

/// The read positions (in copy samples, relative to the window) of the frame's first and last pixel on each axis, for `pose`.
fn footprint(m: &StillMotion, pose: &StillPose) -> ((f64, f64), (f64, f64)) {
    let [[xa, ya], [xb, _], [_, yb], _] = m.corners(pose);
    let (_, _, ww, wh) = m.window;
    // Pixel `i` of the window reads `xa + (xb - xa) * i / ww` on a rectangle; the frame keeps the first `frame` of them.
    let at = |a: f64, b: f64, n: u32, i: u32| a + (b - a) * f64::from(i) / f64::from(n);
    (
        (at(xa, xb, ww, 0), at(xa, xb, ww, m.frame.0 - 1)),
        (at(ya, yb, wh, 0), at(ya, yb, wh, m.frame.1 - 1)),
    )
}

#[test]
fn the_window_holds_every_sample_the_perspective_reads_and_the_one_after_it() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut built, mut tried) = (0, 0);
    while built < 3000 && tried < 400_000 {
        tried += 1;
        let picture = (rng.int(16, 6000), rng.int(16, 6000));
        let frame = (rng.int(1, 960) * 2, rng.int(1, 540) * 2);
        let fit = if rng.next() < 0.6 { Fit::Cover } else { Fit::Contain };
        let crop = if rng.next() < 0.4 {
            let (l, t) = (rng.between(0.0, 0.3), rng.between(0.0, 0.3));
            (l, t, rng.between(0.3, 1.0 - l), rng.between(0.3, 1.0 - t))
        } else {
            (0.0, 0.0, 1.0, 1.0)
        };
        let stat = StillStatic {
            picture,
            fit,
            crop,
            cropped_first: fit == Fit::Cover && rng.next() < 0.7,
        };
        // Samples in a region that keeps the frame covered (the plan refuses what leaves it bare), or anywhere: `build` decides.
        let mut samples = Vec::new();
        for _ in 0..rng.int(1, 6) {
            let cz = if rng.next() < 0.3 { rng.between(1.0, 2.5) } else { 1.0 };
            let side = 1.0 / cz;
            let pan = |rng: &mut Rng| rng.between(-1.0, 1.0);
            let (px, py) = (pan(&mut rng), pan(&mut rng));
            samples.push(StillSample {
                scale: rng.between(1.0, 5.0),
                pos_x: rng.between(-0.3, 0.3),
                pos_y: rng.between(-0.3, 0.3),
                wl: (1.0 - side) * (1.0 + px) / 2.0,
                wt: (1.0 - side) * (1.0 + py) / 2.0,
                side,
            });
        }
        let Some(m) = StillMotion::build(&stat, frame, &samples) else {
            continue;
        };
        built += 1;
        let (qw, qh) = m.q;
        let (wx, wy, ww, wh) = m.window;
        assert!(
            qw >= frame.0 && qh >= frame.1 && qw % 2 == 0 && qh % 2 == 0,
            "{stat:?} {frame:?}: q {:?}",
            m.q
        );
        assert!(
            ww >= frame.0
                && wh >= frame.1
                && wx + ww <= qw
                && wy + wh <= qh
                && wx % 2 == 0
                && wy % 2 == 0
                && ww % 2 == 0
                && wh % 2 == 0,
            "{stat:?} {frame:?}: window {:?} of {:?}",
            m.window,
            m.q
        );
        for s in &samples {
            let pose = StillPose {
                scale: s.scale,
                pos_x: s.pos_x,
                pos_y: s.pos_y,
                crop_zoom: 1.0 / s.side,
                crop_x: if s.side < 1.0 {
                    (2.0 * s.wl / (1.0 - s.side)) - 1.0
                } else {
                    0.0
                },
                crop_y: if s.side < 1.0 {
                    (2.0 * s.wt / (1.0 - s.side)) - 1.0
                } else {
                    0.0
                },
            };
            let ((u0, u1), (v0, v1)) = footprint(&m, &pose);
            // Every position read is inside the window, with the sample after it (a bilinear read of `u` takes `floor(u)` and the next);
            // where the window's edge is the copy's own, the picture ends there and `covers` has kept the frame inside it.
            let inside = |lo: f64, hi: f64, wo: u32, n: u32, q: u32| {
                let start_ok = wo == 0 || lo.floor() >= 0.0;
                let end_ok = wo + n == q || hi.floor() + 1.0 <= f64::from(n - 1);
                start_ok && end_ok
            };
            assert!(
                inside(u0.min(u1), u0.max(u1), wx, ww, qw) && inside(v0.min(v1), v0.max(v1), wy, wh, qh),
                "{stat:?} frame {frame:?}: reads x {u0:.3}..{u1:.3}, y {v0:.3}..{v1:.3} of a window {:?} cut from {:?}",
                m.window,
                m.q
            );
        }
        // The copy is the smallest even size with a pixel for every frame pixel at the greatest magnification, with the headroom.
        let (fw, fh) = (f64::from(frame.0), f64::from(frame.1));
        let most = samples
            .iter()
            .map(|s| {
                let aspect = stat.crop.2 * f64::from(picture.0) / (stat.crop.3 * f64::from(picture.1));
                let fit_w = match fit {
                    Fit::Cover => fw.max(fh * aspect),
                    _ => fw.min(fh * aspect),
                };
                fit_w * s.scale / (stat.crop.2 * s.side)
            })
            .fold(0.0_f64, f64::max);
        assert!(
            f64::from(qw) >= most * HEADROOM - 1e-9 && f64::from(qw) < most * HEADROOM + 2.0 + 1e-9,
            "{stat:?}: q {qw} for {most}"
        );
    }
    assert!(built >= 1000, "only {built} of {tried} random moves were built");
}

/// At the home pose of a Cover frame of the picture's own shape the frame reads the copy at its own scale from the window's origin.
#[test]
fn the_corners_of_the_home_pose_are_the_windows_own() {
    let stat = StillStatic {
        picture: (1920, 1080),
        fit: Fit::Cover,
        crop: (0.0, 0.0, 1.0, 1.0),
        cropped_first: true,
    };
    let at = |scale: f64, pos_x: f64, pos_y: f64| StillSample {
        scale,
        pos_x,
        pos_y,
        wl: 0.0,
        wt: 0.0,
        side: 1.0,
    };
    let m = StillMotion::build(&stat, (1920, 1080), &[at(1.3, 0.05, 0.02), at(1.0, 0.0, 0.0)]).unwrap();
    let home = StillPose {
        scale: 1.0,
        pos_x: 0.0,
        pos_y: 0.0,
        crop_zoom: 1.0,
        crop_x: 0.0,
        crop_y: 0.0,
    };
    let [[x0, y0], [x1, _], [_, y2], _] = m.corners(&home);
    let (wx, wy, ww, wh) = m.window;
    let step_x = f64::from(m.q.0) / m.fit.0;
    let step_y = f64::from(m.q.1) / m.fit.1;
    let tol = 1e-9 * f64::from(m.q.0.max(m.q.1));
    assert!((x0 - (-0.5 - f64::from(wx) + step_x * 0.5)).abs() < tol, "{x0}");
    assert!((y0 - (-0.5 - f64::from(wy) + step_y * 0.5)).abs() < tol, "{y0}");
    assert!(((x1 - x0) - step_x * f64::from(ww)).abs() < tol);
    assert!(((y2 - y0) - step_y * f64::from(wh)).abs() < tol);
}
