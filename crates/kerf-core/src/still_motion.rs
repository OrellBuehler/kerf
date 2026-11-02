//! A still image on a Ken Burns move, as the export and the FFmpeg still draw it (pure: the
//! numbers of the move; `engine::cli::still_motion` writes them into the graph, the compositor draws them).
//!
//! The still (decoded once and held by `loop`, as every still is) is scaled once with the delivery's
//! scaler, Lanczos by default, to the smallest size that still has a pixel for every pixel of the frame
//! at the clip's greatest magnification ([`HEADROOM`] over it) and cut once to what any frame of the clip
//! shows ([`StillMotion::once`]); each frame is then **one** `perspective` (bilinear, 1/256 of a sample,
//! `eval=frame`) that maps the frame onto the window the model says: the fit, the transform's zoom and
//! position and the crop's window ([`StillMotion::corners`], the four corners the filter's expressions
//! evaluate to, worked out in the order the expression writes them). The result is cut to the frame at
//! the window's corner and is a plain frame-sized layer from there on.
//!
//! It is taken only where it draws what the general chain would at every frame, sub-pixel
//! ([`StillStatic::of_clip`], [`StillMotion::build`]): a still with no rotation, mask, reframe, tone-map
//! or spatial effect, whose picture covers the whole frame at every frame of the clip (a zoom in, a pan
//! inside it). Anything else keeps the general chain (the placement, for a zoom or a position that moves).

use crate::layer_geometry::cover_overflows;
use crate::model::{Clip, Fit, FitOrder, Property, VideoEffect};

/// How much more than the greatest magnification the scaled copy is given: what keeps the bilinear
/// read at a step of a little over one pixel at the clip's tightest frame.
pub const HEADROOM: f64 = 1.02;

/// Half the largest even number a size is worked out as: what the arithmetic of the copy saturates at.
const HALF_MAX: f64 = (u32::MAX / 4) as f64;

/// The most pixels the scaled copy may have.
const MAX_COPY_PIXELS: u64 = 400_000_000;

/// What the move needs of the clip and the picture that does not change over the clip: the still's
/// displayed size, the fit, the static crop (as fractions of the still) and whether the picture shown
/// round its centre is the frame (`Cover`, fitted first) or the whole fitted picture.
#[derive(Debug, Clone, PartialEq)]
pub struct StillStatic {
    /// The still as the graph decodes it (autorotated).
    pub picture: (u32, u32),
    pub fit: Fit,
    /// The static crop: left, top, width, height, as fractions of the still.
    pub crop: (f64, f64, f64, f64),
    /// `Cover` that crops to the frame first: the part of the picture shown is the frame, not the cover.
    pub cropped_first: bool,
}

/// The clip's numbers at one clip-local moment: the transform's zoom and position and the crop's
/// window ([`Clip::crop_window_at`]: left and top as fractions of the cropped still, `side` its width and height).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StillSample {
    pub scale: f64,
    pub pos_x: f64,
    pub pos_y: f64,
    pub wl: f64,
    pub wt: f64,
    pub side: f64,
}

impl StillSample {
    pub fn at(clip: &Clip, local: f64) -> Self {
        let pose = clip.transform_at(local);
        let (wl, wt, side, _) = clip.crop_window_at(local);
        Self {
            scale: pose.scale,
            pos_x: pose.pos_x,
            pos_y: pose.pos_y,
            wl,
            wt,
            side,
        }
    }
}

/// The numbers `perspective`'s expressions evaluate to at one frame: the transform's zoom and position and
/// the crop window's zoom and pan (`1`, `0`, `0` for a crop that does not move).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StillPose {
    pub scale: f64,
    pub pos_x: f64,
    pub pos_y: f64,
    pub crop_zoom: f64,
    pub crop_x: f64,
    pub crop_y: f64,
}

/// The plan of a moving still's picture: the scaled copy, the part of it any frame shows, and the
/// frame the move is drawn into.
#[derive(Debug, Clone, PartialEq)]
pub struct StillMotion {
    /// The still scaled once to `q.0` x `q.1`.
    pub q: (u32, u32),
    /// The part of the scaled copy that any frame reads, `(x, y, w, h)`, at least the frame's size.
    pub window: (u32, u32, u32, u32),
    /// The frame the picture is drawn into.
    pub frame: (u32, u32),
    /// The picture fitted to the frame, before the transform's zoom (frame pixels).
    pub fit: (f64, f64),
    /// The static crop as fractions of the still: left, top, width, height.
    pub crop: (f64, f64, f64, f64),
}

impl StillStatic {
    /// The static part of the move for `clip` (a still image of `picture` pixels) delivered at `judge`
    /// (the delivery's own frame, which decides the fit order as it does for a still drawn smaller) with
    /// `fit`; `None` where the clip does not take this route whatever its times: a still that does not
    /// move, or has a rotation, mask, reframe or spatial effect.
    pub fn of_clip(clip: &Clip, picture: Option<(u32, u32)>, judge: (u32, u32), fit: Fit) -> Option<Self> {
        let t = &clip.transform;
        let still_angle = !clip.is_driven(Property::Rotation) && t.rotation != 0.0;
        let turns = clip.rotation_turns();
        let spatial = clip.effects.iter().any(|e| {
            matches!(
                e,
                VideoEffect::Blur { .. } | VideoEffect::Sharpen { .. } | VideoEffect::Vignette
            )
        });
        if !clip.picture_moves() || still_angle || turns || spatial || clip.mask.is_some() || clip.reframe.is_some() {
            return None;
        }
        let (sw, sh) = picture.filter(|&(w, h)| w > 0 && h > 0)?;
        let crop = if t.has_crop() {
            (
                t.crop_left,
                t.crop_top,
                (1.0 - t.crop_left - t.crop_right).max(0.0),
                (1.0 - t.crop_top - t.crop_bottom).max(0.0),
            )
        } else {
            (0.0, 0.0, 1.0, 1.0)
        };
        if crop.2 <= 1e-6 || crop.3 <= 1e-6 {
            return None;
        }
        let order: FitOrder = clip.fit_order(fit, cover_overflows(Some((sw, sh)), judge, t));
        Some(Self {
            picture: (sw, sh),
            fit,
            crop,
            cropped_first: fit == Fit::Cover && order == FitOrder::Fitted,
        })
    }
}

impl StillMotion {
    /// The plan for a still on a `frame`, the moves judged at `samples` (every frame it is drawn on), or
    /// `None` where the general chain draws it: a move this cannot draw as that chain does.
    pub fn build(stat: &StillStatic, frame: (u32, u32), samples: &[StillSample]) -> Option<Self> {
        if samples.is_empty() || frame.0 < 2 || frame.1 < 2 {
            return None;
        }
        let (sw, sh) = stat.picture;
        let crop = stat.crop;
        let (fw, fh) = (f64::from(frame.0), f64::from(frame.1));
        let aspect = crop.2 * f64::from(sw) / (crop.3 * f64::from(sh));
        let fit_size = match stat.fit {
            Fit::Cover => (fw.max(fh * aspect), fh.max(fw / aspect)),
            Fit::Contain | Fit::Blur => (fw.min(fh * aspect), fh.min(fw / aspect)),
        };
        // What of the picture the general chain shows, round its centre: the frame under a `Cover`
        // that crops to it first, the whole fitted picture otherwise.
        let shown = if stat.cropped_first { (fw, fh) } else { fit_size };
        let mut most = 0.0_f64;
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for sample in samples {
            let s = sample.scale;
            let side = sample.side;
            if !(s.is_finite() && s > 0.0 && side.is_finite() && side > 0.0 && side <= 1.0 + 1e-9) {
                return None;
            }
            let covers = |shown: f64, pos: f64, side: f64| shown * s + 1e-6 * side >= side * (1.0 + 2.0 * pos.abs());
            if !covers(shown.0, sample.pos_x, fw) || !covers(shown.1, sample.pos_y, fh) {
                return None;
            }
            // Frame pixels per width of the whole still, and where the frame's edges fall on it.
            let (rw, rh) = (fit_size.0 * s, fit_size.1 * s);
            let (cw, ch) = (crop.2 * side, crop.3 * side);
            most = most.max(rw / cw);
            let (cx0, cy0) = (crop.0 + crop.2 * sample.wl, crop.1 + crop.3 * sample.wt);
            let left = fw / 2.0 + sample.pos_x * fw - rw / 2.0;
            let top = fh / 2.0 + sample.pos_y * fh - rh / 2.0;
            let u = |x: f64| cx0 + (x - left) / rw * cw;
            let v = |y: f64| cy0 + (y - top) / rh * ch;
            x0 = x0.min(u(0.0));
            x1 = x1.max(u(fw));
            y0 = y0.min(v(0.0));
            y1 = y1.max(v(fh));
        }
        // (A hostile crop makes these astronomically large, or not numbers: they saturate, and the size checks below refuse them.)
        let even = |halves: f64| (halves.clamp(1.0, HALF_MAX) as u32) * 2;
        let ceil_even = |v: f64| even((v / 2.0).ceil());
        let qw = ceil_even(most * HEADROOM);
        let qh = even(((f64::from(qw) * f64::from(sh) / f64::from(sw)) / 2.0).round());
        if qw < frame.0 || qh < frame.1 || u64::from(qw) * u64::from(qh) > MAX_COPY_PIXELS {
            return None;
        }
        // What any frame reads, a couple of pixels wider for the bilinear read at its edge, in even
        // steps (a 4:2:0 still's crop would round an odd corner), never less than the frame.
        let span = |a: f64, b: f64, q: u32, need: u32| {
            let qf = f64::from(q);
            let need = ceil_even(f64::from(need)).min(q);
            let lo = (((a.clamp(0.0, 1.0) * qf - 2.0) / 2.0).floor().clamp(0.0, HALF_MAX) as u32) * 2;
            let hi = (ceil_even(b.clamp(0.0, 1.0) * qf + 2.0)).min(q).max(lo);
            if hi - lo >= need {
                (lo, hi - lo)
            } else {
                let hi = (lo + need).min(q);
                (hi - need, need)
            }
        };
        let (wx, ww) = span(x0, x1, qw, frame.0);
        let (wy, wh) = span(y0, y1, qh, frame.1);
        if ww < frame.0 || wh < frame.1 {
            return None;
        }
        Some(Self {
            q: (qw, qh),
            window: (wx, wy, ww, wh),
            frame,
            fit: fit_size,
            crop,
        })
    }

    /// The four corners of the `perspective` for a pose, top-left, top-right, bottom-left, bottom-right, each
    /// `[x, y]`: the position in the window that the frame's corner reads, **worked out in the order the
    /// expression writes it** (`engine::cli::still_motion`'s `corners`):
    /// `base + k * ((1 - 1/cz) * (1 + pan) / 2 + (xc - pos * frame) / (cz * fitted * scale) + 0.5/cz)` per axis,
    /// with `base = Q * start - 0.5 - window origin`, `k = Q * len` and `xc = x + 0.5 - frame/2` (each
    /// of those three printed and read back as the same double).
    pub fn corners(&self, pose: &StillPose) -> [[f64; 2]; 4] {
        let axis = |q: u32, start: f64, len: f64, at: u32, frame: u32, fitted: f64, pos: f64, pan: f64, out: u32| {
            let q = f64::from(q);
            let base = q * start - 0.5 - f64::from(at);
            let k = q * len;
            let edge = |x: u32| {
                let xc = f64::from(x) + 0.5 - f64::from(frame) / 2.0;
                let cz = pose.crop_zoom;
                let first = (1.0 - 1.0 / cz) * (1.0 + pan) / 2.0;
                let second = (xc - pos * f64::from(frame)) / (cz * fitted * pose.scale);
                let third = 0.5 / cz;
                base + k * (first + second + third)
            };
            (edge(0), edge(out))
        };
        let (wx, wy, ww, wh) = self.window;
        let (xa, xb) = axis(
            self.q.0,
            self.crop.0,
            self.crop.2,
            wx,
            self.frame.0,
            self.fit.0,
            pose.pos_x,
            pose.crop_x,
            ww,
        );
        let (ya, yb) = axis(
            self.q.1,
            self.crop.1,
            self.crop.3,
            wy,
            self.frame.1,
            self.fit.1,
            pose.pos_y,
            pose.crop_y,
            wh,
        );
        [[xa, ya], [xb, ya], [xa, yb], [xb, yb]]
    }

    /// The pose of a clip-local moment (a crop that never zooms draws the whole picture: `crop_zoom` 1).
    pub fn pose_at(clip: &Clip, local: f64) -> StillPose {
        let crop = |p: Property| {
            if clip.crop_animated() {
                clip.property_at(p, local)
            } else {
                p.neutral()
            }
        };
        StillPose {
            scale: clip.property_at(Property::Scale, local),
            pos_x: clip.property_at(Property::PosX, local),
            pos_y: clip.property_at(Property::PosY, local),
            crop_zoom: crop(Property::CropZoom),
            crop_x: crop(Property::CropX),
            crop_y: crop(Property::CropY),
        }
    }

    /// The filters that run once, ahead of the `loop`: the still scaled with `sf` (the delivery's scaler
    /// flags, `:flags=...`; Lanczos when it names none: it runs once) and cut to the window any frame reads.
    pub fn once(&self, sf: &str) -> String {
        let (x, y, w, h) = self.window;
        let sf = if sf.is_empty() { ":flags=lanczos" } else { sf };
        format!("scale={}:{}{sf},crop={w}:{h}:{x}:{y}", self.q.0, self.q.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(picture: (u32, u32), fit: Fit) -> StillStatic {
        StillStatic {
            picture,
            fit,
            crop: (0.0, 0.0, 1.0, 1.0),
            cropped_first: fit == Fit::Cover,
        }
    }

    fn at(scale: f64, pos_x: f64, pos_y: f64) -> StillSample {
        StillSample {
            scale,
            pos_x,
            pos_y,
            wl: 0.0,
            wt: 0.0,
            side: 1.0,
        }
    }

    fn still_pose(s: &StillSample) -> StillPose {
        StillPose {
            scale: s.scale,
            pos_x: s.pos_x,
            pos_y: s.pos_y,
            crop_zoom: 1.0,
            crop_x: 0.0,
            crop_y: 0.0,
        }
    }

    #[test]
    fn a_move_that_leaves_the_frame_bare_is_not_the_routes() {
        let s = stat((1920, 1280), Fit::Cover);
        // In from 1.2x: covered the whole way. At the frame's own size panned off its edge: not.
        assert!(StillMotion::build(&s, (1920, 1080), &[at(1.2, 0.04, 0.03), at(1.0, 0.0, 0.0)]).is_some());
        assert!(StillMotion::build(&s, (1920, 1080), &[at(1.0, -0.03, 0.0)]).is_none());
        assert!(StillMotion::build(&s, (1920, 1080), &[]).is_none());
    }

    #[test]
    fn a_hostile_crop_is_refused_not_overflowed() {
        // A crop a millionth of the picture wide asks for a copy of astronomical size, and one narrower than a double can divide by asks for no number at all.
        for width in [1e-6, 1e-12, 1e-300, 5e-324] {
            let mut s = stat((1920, 1280), Fit::Cover);
            s.crop = (0.0, 0.0, width, 1.0);
            assert!(
                StillMotion::build(&s, (1920, 1080), &[at(1.2, 0.04, 0.03)]).is_none(),
                "{width}"
            );
            s.crop = (0.0, 0.0, 1.0, width);
            assert!(
                StillMotion::build(&s, (1920, 1080), &[at(1.2, 0.04, 0.03)]).is_none(),
                "{width}"
            );
        }
        let s = stat((1920, 1280), Fit::Cover);
        for scale in [1e300, f64::MAX, 1e-300] {
            assert!(
                StillMotion::build(&s, (1920, 1080), &[at(scale, 0.0, 0.0)]).is_none(),
                "{scale}"
            );
        }
    }

    #[test]
    fn the_copy_is_the_smallest_one_with_a_pixel_for_every_frame_pixel_and_the_window_holds_the_frame() {
        let s = stat((1920, 1280), Fit::Cover);
        let m = StillMotion::build(&s, (1920, 1080), &[at(1.2, 0.04, 0.03), at(1.0, 0.0, 0.0)]).unwrap();
        // 1.2x of the cover (1920 wide) with the 2% headroom, even.
        assert_eq!(m.q.0, 2352);
        assert_eq!(m.q.1 % 2, 0);
        let (x, y, w, h) = m.window;
        assert!(w >= 1920 && h >= 1080 && x + w <= m.q.0 && y + h <= m.q.1 && x % 2 == 0 && y % 2 == 0);
        assert!(m.once("").starts_with("scale=2352:") && m.once("").contains(":flags=lanczos,crop="));
        assert!(m.once(":flags=bicubic").contains(":flags=bicubic,crop="));
    }

    #[test]
    fn at_the_home_pose_the_frame_reads_the_copy_across_the_window_at_the_copys_own_scale() {
        // A Cover frame of the picture's own shape, home pose: one frame pixel is `q / fit` of a copy pixel across, and the first corner
        // is the centre of the frame's first pixel (half a sample in), less the window's origin.
        let s = stat((1920, 1080), Fit::Cover);
        let home = at(1.0, 0.0, 0.0);
        let m = StillMotion::build(&s, (1920, 1080), &[at(1.1, 0.02, 0.0), home]).unwrap();
        let [[x0, y0], [x1, _], [_, y2], [x3, y3]] = m.corners(&still_pose(&home));
        let (wx, wy, ww, wh) = m.window;
        let (qw, qh) = (f64::from(m.q.0), f64::from(m.q.1));
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9 * qw.max(qh);
        let step_x = qw / m.fit.0;
        let step_y = qh / m.fit.1;
        assert!(near(x1 - x0, step_x * f64::from(ww)) && near(x3, x1), "{x0} {x1}");
        assert!(near(y2 - y0, step_y * f64::from(wh)) && near(y3, y2), "{y0} {y2}");
        assert!(near(x0, -0.5 - f64::from(wx) + step_x * (0.5 - 960.0 + 960.0)), "{x0}");
        assert!(near(y0, -0.5 - f64::from(wy) + step_y * (0.5 - 540.0 + 540.0)), "{y0}");
    }
}
