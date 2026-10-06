//! The integer geometry of one layer, worked out the way FFmpeg's filters work
//! it out.
//!
//! The still graph hands a clip to `crop → scale → (pad | scale) → rotate →
//! overlay`, and each of those rounds differently: `crop` rounds with `lrint` and
//! then clears the low bit (4:2:0 chroma), `scale`'s aspect-preserving mode uses
//! `av_rescale` (round half away), the second `scale` truncates, `pad` and
//! `overlay` truncate and then round *down* to an even pixel, `rotate` rounds its
//! box half-up. None of it is hard, but a layer that lands one pixel off from
//! FFmpeg's is a layer whose every edge differs, and a picture-in-picture
//! placed on `x = 16.5` is a different picture at 16 and at 17. So the
//! compositor does not place things "where they should be" — it places them
//! where FFmpeg does, and that arithmetic lives here, pure and testable.
//!
//! The output is a recipe for the GPU: up to two resample stages (the fit scale,
//! the transform's own scale), an optional rotation, and where the result lands
//! on the canvas.

use kerf_core::{Fit, Transform};

/// A pixel rectangle in some picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn whole(w: u32, h: u32) -> Self {
        Self { x: 0, y: 0, w, h }
    }
}

/// One `swscale` resample of a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaleStage {
    /// The window of the input picture read (a `crop` is folded in here: the
    /// scaler clamps at the crop's edges, not at the picture's).
    pub src: Rect,
    /// The full size the window is scaled to — what the kernel's ratio comes from.
    pub scaled: (u32, u32),
    /// The part of the scaled picture that is kept: all of it, or the centre
    /// window a `Fit::Cover` crop keeps. The stage's output is this size.
    pub keep: Rect,
}

/// A `rotate` filter's output box and angle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rotation {
    /// Clockwise radians.
    pub angle: f64,
    /// The rotated layer's size (`rotw` / `roth`, rounded half-up).
    pub out: (u32, u32),
}

/// Everything the compositor needs to draw one layer where FFmpeg would.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerGeometry {
    /// Resample stages, in order (one or two).
    pub stages: Vec<ScaleStage>,
    /// The picture after the stages.
    pub picture: (u32, u32),
    pub rotation: Option<Rotation>,
    /// The layer as it is overlaid: the rotated box, else the picture.
    pub layer: (u32, u32),
    /// Where the layer's top-left lands on the canvas (may be off-canvas).
    pub origin: (i32, i32),
    /// 0..1, quantised to 8 bits like an alpha plane.
    pub opacity: f32,
}

/// A layer FFmpeg would fail to build (zero-sized crop, absurd transform): the
/// compositor refuses it so the frame goes through FFmpeg and fails the same way.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GeometryError(pub String);

fn lrint(d: f64) -> i64 {
    d.round_ties_even() as i64
}

/// `av_rescale` for positive operands (`AV_ROUND_NEAR_INF`).
fn av_rescale(a: i64, b: i64, c: i64) -> i64 {
    (a * b + c / 2) / c
}

/// Round down to a multiple of two — what 4:2:0 chroma makes `crop`, `pad` and
/// `overlay` do to a position.
fn even(v: i64) -> i64 {
    v & !1
}

/// `overlay`'s `normalize_xy`: truncate toward zero, then round down to even.
fn overlay_coord(d: f64) -> i32 {
    ((d as i64) & !1) as i32
}

fn crop_window(iw: u32, ih: u32, tf: &Transform) -> Result<Rect, GeometryError> {
    if !tf.has_crop() {
        return Ok(Rect::whole(iw, ih));
    }
    let (iwf, ihf) = (f64::from(iw), f64::from(ih));
    let cw = (1.0 - tf.crop_left - tf.crop_right).max(0.0);
    let ch = (1.0 - tf.crop_top - tf.crop_bottom).max(0.0);
    let (w, h) = (even(lrint(iwf * cw)), even(lrint(ihf * ch)));
    if w <= 0 || h <= 0 || w > i64::from(iw) || h > i64::from(ih) {
        return Err(GeometryError(format!("the crop leaves {w}x{h} of {iw}x{ih}")));
    }
    let mut x = lrint(iwf * tf.crop_left).max(0);
    let mut y = lrint(ihf * tf.crop_top).max(0);
    if x + w > i64::from(iw) {
        x = i64::from(iw) - w;
    }
    if y + h > i64::from(ih) {
        y = i64::from(ih) - h;
    }
    Ok(Rect {
        x: even(x) as u32,
        y: even(y) as u32,
        w: w as u32,
        h: h as u32,
    })
}

/// `rotw` / `roth`: the bounding box of a `iw x ih` picture turned by `angle`.
/// FFmpeg takes the sine and cosine as `float`, then does the rest in `double`.
fn rotated_box(iw: u32, ih: u32, angle: f64) -> (u32, u32) {
    let (s, c) = (f64::from(angle.sin() as f32), f64::from(angle.cos() as f32));
    let (iwf, ihf) = (f64::from(iw), f64::from(ih));
    let w = (ihf * s).abs() + (iwf * c).abs();
    let h = (iwf * s).abs() + (ihf * c).abs();
    // `outw = res + 0.5` into an int.
    (((w + 0.5) as u32).max(1), ((h + 0.5) as u32).max(1))
}

impl LayerGeometry {
    /// Resolve a `src`-sized picture onto a `canvas`-sized frame: exactly what
    /// `still_clip_chain` + `still_overlay` build for the same inputs.
    pub fn resolve(src: (u32, u32), canvas: (u32, u32), fit: Fit, tf: &Transform) -> Result<Self, GeometryError> {
        let (iw, ih) = src;
        let (ow, oh) = canvas;
        if iw == 0 || ih == 0 || ow == 0 || oh == 0 {
            return Err(GeometryError("an empty picture or canvas".into()));
        }
        if !(tf.scale.is_finite() && tf.scale > 0.0)
            || ![tf.pos_x, tf.pos_y, tf.rotation, tf.opacity].iter().all(|v| v.is_finite())
        {
            return Err(GeometryError("a non-finite transform".into()));
        }

        // 1. crop, 2. the fit scale (`force_original_aspect_ratio`).
        let win = crop_window(iw, ih, tf)?;
        let (tmp_w, tmp_h) = (
            av_rescale(i64::from(oh), i64::from(win.w), i64::from(win.h)),
            av_rescale(i64::from(ow), i64::from(win.h), i64::from(win.w)),
        );
        let (fw, fh) = match fit {
            Fit::Contain => (tmp_w.min(i64::from(ow)), tmp_h.min(i64::from(oh))),
            Fit::Cover => (tmp_w.max(i64::from(ow)), tmp_h.max(i64::from(oh))),
        };
        if fw <= 0 || fh <= 0 {
            return Err(GeometryError(format!("the fit scale leaves {fw}x{fh}")));
        }
        let (fw, fh) = (fw as u32, fh as u32);
        // Cover then crops the overhang: `crop=ow:oh`, centred by `(iw-ow)/2`.
        let keep = match fit {
            Fit::Contain => Rect::whole(fw, fh),
            Fit::Cover => Rect {
                x: even(lrint(f64::from(fw - ow) / 2.0)) as u32,
                y: even(lrint(f64::from(fh - oh) / 2.0)) as u32,
                w: ow,
                h: oh,
            },
        };
        let mut stages = vec![ScaleStage {
            src: win,
            scaled: (fw, fh),
            keep,
        }];
        let mut picture = (keep.w, keep.h);

        // 3. identity: Contain pads, Cover already fills the frame.
        if tf.is_identity() {
            let origin = match fit {
                Fit::Contain => (
                    even(((i64::from(ow) - i64::from(fw)) as f64 / 2.0) as i64) as i32,
                    even(((i64::from(oh) - i64::from(fh)) as f64 / 2.0) as i64) as i32,
                ),
                Fit::Cover => (0, 0),
            };
            // `pad` copies the picture in at a size rounded *down* to even (it
            // hands 4:2:0 chroma whole blocks), so a scaled picture with an odd
            // side loses its last row or column to the padding — measured: a
            // 203-row fit in a 640-row frame is 202 rows in FFmpeg's still.
            let layer = match fit {
                Fit::Contain => (picture.0 & !1, picture.1 & !1),
                Fit::Cover => picture,
            };
            return Ok(Self {
                stages,
                picture,
                rotation: None,
                layer,
                origin,
                opacity: 1.0,
            });
        }

        // 3'. otherwise the transform's own scale: `scale=iw*sc:ih*sc`, truncated
        // (and a result of 0 means "keep the input size").
        if (tf.scale - 1.0).abs() > 1e-9 {
            let w = (f64::from(picture.0) * tf.scale) as i64;
            let h = (f64::from(picture.1) * tf.scale) as i64;
            let (w, h) = (
                if w == 0 {
                    picture.0
                } else {
                    w.min(i64::from(u16::MAX) * 4) as u32
                },
                if h == 0 {
                    picture.1
                } else {
                    h.min(i64::from(u16::MAX) * 4) as u32
                },
            );
            stages.push(ScaleStage {
                src: Rect::whole(picture.0, picture.1),
                scaled: (w, h),
                keep: Rect::whole(w, h),
            });
            picture = (w, h);
        }

        // 4. rotate (clockwise radians, box grown to hold the corners).
        let rotation = (tf.rotation != 0.0).then(|| {
            let angle = tf.rotation.to_radians();
            Rotation {
                angle,
                out: rotated_box(picture.0, picture.1, angle),
            }
        });
        let layer = rotation.map_or(picture, |r| r.out);

        // 5. overlay at `(W-w)/2 + pos*W`.
        let origin = (
            overlay_coord((f64::from(ow) - f64::from(layer.0)) / 2.0 + tf.pos_x * f64::from(ow)),
            overlay_coord((f64::from(oh) - f64::from(layer.1)) / 2.0 + tf.pos_y * f64::from(oh)),
        );
        Ok(Self {
            stages,
            picture,
            rotation,
            layer,
            origin,
            opacity: ((tf.opacity.clamp(0.0, 1.0) * 255.0).round() / 255.0) as f32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Transform {
        Transform::default()
    }

    #[test]
    fn identity_same_shape_is_a_copy_at_the_origin() {
        let g = LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &t()).unwrap();
        assert_eq!(g.picture, (640, 360));
        assert_eq!(g.origin, (0, 0));
        assert_eq!(g.stages.len(), 1);
        assert_eq!(g.stages[0].scaled, (640, 360));
        assert!(g.rotation.is_none());
    }

    #[test]
    fn contain_letterboxes_with_ffmpegs_rounding_and_even_padding() {
        // 16:9 into 9:16: scaled to 360x203 (202.5 rounds away from zero), then
        // padded at y = 218.5 -> 218 (truncate, already even).
        let g = LayerGeometry::resolve((640, 360), (360, 640), Fit::Contain, &t()).unwrap();
        assert_eq!(g.picture, (360, 203));
        assert_eq!(g.origin, (0, 218));
        // ...and `pad` keeps only an even number of the picture's rows.
        assert_eq!(g.layer, (360, 202));
        // An odd gap rounds down to even: 1080-607 = 473 / 2 = 236.5 -> 236.
        let g = LayerGeometry::resolve((1000, 1650), (1080, 1080), Fit::Contain, &t()).unwrap();
        assert_eq!(g.picture.1, 1080);
        assert_eq!(g.origin.0 % 2, 0);
    }

    #[test]
    fn cover_scales_up_and_crops_the_centre() {
        // 16:9 into 9:16: scaled to cover (1138x640), then the middle 360 columns
        // are kept, at x = (1138 - 360) / 2 = 389 -> 388.
        let g = LayerGeometry::resolve((640, 360), (360, 640), Fit::Cover, &t()).unwrap();
        assert_eq!(g.stages[0].scaled, (1138, 640));
        assert_eq!(
            g.stages[0].keep,
            Rect {
                x: 388,
                y: 0,
                w: 360,
                h: 640
            }
        );
        assert_eq!(g.picture, (360, 640));
        assert_eq!(g.origin, (0, 0));
    }

    #[test]
    fn a_crop_is_even_and_folds_into_the_first_stage() {
        let tf = Transform {
            crop_left: 0.1,
            crop_top: 0.05,
            ..t()
        };
        let g = LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        // w = 640 * 0.9 = 576, h = 360 * 0.95 = 342; x = 64, y = lrint(18) = 18.
        assert_eq!(
            g.stages[0].src,
            Rect {
                x: 64,
                y: 18,
                w: 576,
                h: 342
            }
        );
        // Not the identity, so there is no pad: the layer is centred by overlay.
        assert_eq!(g.picture, (g.stages[0].scaled.0, g.stages[0].scaled.1));
        assert_eq!(g.origin.0 % 2, 0);
    }

    #[test]
    fn a_crop_that_leaves_nothing_is_refused() {
        let tf = Transform {
            crop_left: 0.6,
            crop_right: 0.6,
            ..t()
        };
        assert!(LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).is_err());
    }

    #[test]
    fn scale_and_offset_follow_overlay_truncation() {
        let tf = Transform {
            scale: 0.5,
            pos_x: 0.25,
            pos_y: -0.1,
            ..t()
        };
        let g = LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        assert_eq!(g.stages.len(), 2);
        assert_eq!(g.picture, (320, 180));
        // x = (640-320)/2 + 0.25*640 = 320 ; y = (360-180)/2 - 36 = 54.
        assert_eq!(g.origin, (320, 54));
        // An odd position is rounded down to even: 1/3 of 640 = 213.33 + 160 = 373.
        let tf = Transform {
            scale: 0.5,
            pos_x: 1.0 / 3.0,
            ..t()
        };
        let g = LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        assert_eq!(g.origin.0, 372);
    }

    #[test]
    fn a_negative_position_truncates_toward_zero_before_rounding_down() {
        // (640-320)/2 - 0.6*640 = -224 exactly; -224.5 would truncate to -224.
        assert_eq!(overlay_coord(-224.5), -224);
        // -3 (truncated from -3.5) rounds down to -4.
        assert_eq!(overlay_coord(-3.5), -4);
    }

    #[test]
    fn rotation_grows_the_box_and_recentres() {
        let tf = Transform { rotation: 90.0, ..t() };
        let g = LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        let r = g.rotation.unwrap();
        assert_eq!(r.out, (360, 640));
        assert_eq!(g.layer, (360, 640));
        assert_eq!(g.origin, (140, -140));
        // 45 degrees of a square: side * sqrt(2) = 141.42, rounded half-up.
        let tf = Transform { rotation: 45.0, ..t() };
        let g = LayerGeometry::resolve((100, 100), (100, 100), Fit::Contain, &tf).unwrap();
        assert_eq!(g.rotation.unwrap().out, (141, 141));
    }

    #[test]
    fn opacity_is_quantised_like_an_alpha_plane() {
        let tf = Transform { opacity: 0.5, ..t() };
        let g = LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        // 0.5 * 255 = 127.5 -> 128.
        assert!((g.opacity - 128.0 / 255.0).abs() < 1e-6);
        assert!(g.rotation.is_none() && g.stages.len() == 1);
    }

    #[test]
    fn nonsense_transforms_are_refused_not_drawn() {
        let tf = Transform { scale: f64::NAN, ..t() };
        assert!(LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).is_err());
        let tf = Transform { scale: -1.0, ..t() };
        assert!(LayerGeometry::resolve((640, 360), (640, 360), Fit::Contain, &tf).is_err());
    }
}
