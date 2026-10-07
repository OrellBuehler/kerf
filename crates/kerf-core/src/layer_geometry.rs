//! The integer geometry of one layer, worked out the way FFmpeg's filters work
//! it out — shared by [`crate::render_plan::RenderPlan`] (which refuses what the
//! sizes make unrenderable) and the GPU compositor (which draws it).
//!
//! The still graph hands a clip to `crop → scale → (pad | scale) → rotate →
//! overlay`, and each of those rounds differently: `crop` rounds with `lrint` and
//! then rounds down to a whole chroma sample **of the picture's own format** (even
//! for 4:2:0, even columns only for 4:2:2, nothing for 4:4:4, gray or RGB; see
//! [`Subsampling`]). Every filter after it sees the picture in whatever format the
//! chain still has, and the chain converts to 4:2:0 — the one format the overlay at
//! the end accepts — in its **last** `scale`: so with only one `scale` (the
//! identity transform, or a transform that does not resize) the Cover crop and `pad`
//! after it are on the 4:2:0 grid whatever the source was, and with the transform's
//! own `scale` after them the Cover crop is still on the native one. `scale`'s
//! aspect-preserving mode uses `av_rescale` (round half away), the second `scale`
//! truncates, `overlay` truncates and rounds down to even, `rotate` rounds its box
//! half-up. None of it is hard, but a layer that lands one pixel off from//! compositor does not place things "where they should be" — it places them
//! where FFmpeg does, and that arithmetic lives here, pure and testable.
//!
//! The output is a recipe for the GPU: up to two resample stages (the fit scale,
//! the transform's own scale), an optional rotation, and where the result lands
//! on the canvas.

use crate::model::{Fit, Subsampling, Transform};

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
    /// The layer as it is overlaid: the rotated box, else the picture — except
    /// for a letterboxed identity layer, which is the whole canvas (see
    /// [`LayerGeometry::matte`]).
    pub layer: (u32, u32),
    /// Where the picture sits inside the layer (nonzero only with a matte).
    pub picture_at: (u32, u32),
    /// The part of the picture that shows. Smaller than the picture by an odd
    /// last row / column when a matte is added: `pad` drops those.
    pub picture_shows: (u32, u32),
    /// `pad` hands `overlay` a **full-canvas frame**: the letterboxed picture on
    /// opaque black bars, and the `eq` table has already run over the bars too.
    /// So an identity layer that does not fill the frame covers whatever is on
    /// the tracks below with that matte — drawing only the picture lets them show
    /// through the bars.
    pub matte: bool,
    /// Where the layer's top-left lands on the canvas (may be off-canvas).
    pub origin: (i32, i32),
    /// The layer's alpha, 0..1, as FFmpeg's alpha plane ends up holding it (see
    /// [`ffmpeg_alpha`]).
    pub opacity: f32,
    /// The clip is translucent (`opacity < 1`): FFmpeg takes such a layer through
    /// RGB (`colorchannelmixer` has no YUV mode), which the compositor reproduces.
    /// Decided on the raw opacity, like the graph builder does: 0.999 still goes
    /// through RGB even though its alpha plane rounds to opaque.
    pub translucent: bool,
}

/// A layer FFmpeg would fail to build (zero-sized crop, absurd transform): the
/// compositor refuses it so the frame goes through FFmpeg and fails the same way.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GeometryError(pub String);

fn lrint(d: f64) -> i64 {
    d.round_ties_even() as i64
}

/// The alpha plane FFmpeg's still hands `overlay` for a clip of opacity `op`
/// (below 1): `colorchannelmixer` writes `lrint(255 * op)`, and the conversion
/// back to `yuva420p` that follows it scales alpha by 256/255, rounded and
/// clipped at 255 — so 50% is 129, not 128. Verified for every alpha value
/// 0..=255 on FFmpeg 6.1 and 9.0.
pub fn ffmpeg_alpha(op: f64) -> u8 {
    let mixed = lrint(op.clamp(0.0, 1.0) * 255.0).clamp(0, 255);
    ((mixed * 256 + 127) / 255).min(255) as u8
}

/// `av_rescale` for positive operands (`AV_ROUND_NEAR_INF`).
fn av_rescale(a: i64, b: i64, c: i64) -> i64 {
    (a * b + c / 2) / c
}

/// Round down to a multiple of two — what 4:2:0 chroma makes `pad` and `overlay`
/// do to a position: `pad` only runs when there is one `scale`, which has converted
/// the picture to 4:2:0 whatever format it started in, and `overlay` works on the
/// 4:2:0 canvas.
fn even(v: i64) -> i64 {
    v & !1
}

/// `overlay`'s `normalize_xy`: truncate toward zero, then round down to even.
fn overlay_coord(d: f64) -> i32 {
    // Far past any canvas the position is "off it" either way; clamping first
    // keeps the `i32` below from wrapping a huge offset back on-screen.
    ((d.clamp(-1e9, 1e9) as i64) & !1) as i32
}

fn crop_window(iw: u32, ih: u32, tf: &Transform, sub: Subsampling) -> Result<Rect, GeometryError> {
    if !tf.has_crop() {
        return Ok(Rect::whole(iw, ih));
    }
    let (iwf, ihf) = (f64::from(iw), f64::from(ih));
    let cw = (1.0 - tf.crop_left - tf.crop_right).max(0.0);
    let ch = (1.0 - tf.crop_top - tf.crop_bottom).max(0.0);
    let (w, h) = (sub.round_w(lrint(iwf * cw)), sub.round_h(lrint(ihf * ch)));
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
        x: sub.round_w(x) as u32,
        y: sub.round_h(y) as u32,
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
    /// [`LayerGeometry::resolve`] for a picture whose native chroma grid is not
    /// known — a format that was never recorded, or one whose rounding was not
    /// measured ([`crate::model::pix_fmt_subsampling`]). The geometry is returned
    /// only if every grid a format could have gives the same one (no crop to round);
    /// otherwise it is refused, because whichever grid was picked, one of the
    /// formats it stands for would land a pixel off.
    pub fn resolve_any_grid(src: (u32, u32), canvas: (u32, u32), fit: Fit, tf: &Transform) -> Result<Self, GeometryError> {
        let first = Self::resolve(src, canvas, fit, tf, Subsampling::YUV420)?;
        for sub in Subsampling::ALL {
            if Self::resolve(src, canvas, fit, tf, sub)? != first {
                return Err(GeometryError(
                    "its crop rounds differently for 4:2:0 and for other pixel formats, and this one is not known well enough to say which"
                        .into(),
                ));
            }
        }
        Ok(first)
    }

    /// Resolve a `src`-sized picture onto a `canvas`-sized frame: exactly what
    /// `still_clip_chain` + `still_overlay` build for the same inputs. `sub` is the
    /// chroma grid of the picture's **native** format, which the first `crop` rounds
    /// to ([`Subsampling`]) and the Cover crop too when the transform scales the
    /// picture again afterwards; `pad` and the rest are on the 4:2:0 grid.
    pub fn resolve(
        src: (u32, u32),
        canvas: (u32, u32),
        fit: Fit,
        tf: &Transform,
        sub: Subsampling,
    ) -> Result<Self, GeometryError> {
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
        let win = crop_window(iw, ih, tf, sub)?;
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
        // Cover then crops the overhang: `crop=ow:oh`, centred by `(iw-ow)/2`. It
        // sits between the first `scale` and whatever comes next, and the picture is
        // converted to 4:2:0 by the **last** `scale` of the chain: with the
        // transform's own scale after it (below) the crop still sees the native
        // format and rounds to its grid; without one the first scale has already
        // converted, and it is on the 4:2:0 grid whatever the source was.
        let second_scale = !tf.is_identity() && (tf.scale - 1.0).abs() > 1e-9;
        let cover_grid = if second_scale { sub } else { Subsampling::YUV420 };
        let keep = match fit {
            Fit::Contain => Rect::whole(fw, fh),
            Fit::Cover => Rect {
                x: cover_grid.round_w(lrint(f64::from(fw - ow) / 2.0)) as u32,
                y: cover_grid.round_h(lrint(f64::from(fh - oh) / 2.0)) as u32,
                w: cover_grid.round_w(i64::from(ow)) as u32,
                h: cover_grid.round_h(i64::from(oh)) as u32,
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
            // hands 4:2:0 chroma whole blocks — the picture is 4:2:0 by now, from
            // any source), so a scaled picture with an odd side loses its last row
            // or column to the padding — measured: a 203-row fit in a 640-row frame
            // is 202 rows in FFmpeg's still, for 4:4:4 and RGB sources too — and
            // what it emits is the whole canvas, bars included.
            let (layer, picture_at, picture_shows, matte) = match fit {
                Fit::Contain => {
                    let shows = (picture.0 & !1, picture.1 & !1);
                    (
                        (ow, oh),
                        (origin.0.max(0) as u32, origin.1.max(0) as u32),
                        shows,
                        shows != (ow, oh),
                    )
                }
                Fit::Cover => (picture, (0, 0), picture, false),
            };
            let origin = if matte { (0, 0) } else { origin };
            return Ok(Self {
                stages,
                picture,
                rotation: None,
                layer,
                picture_at,
                picture_shows,
                matte,
                origin,
                opacity: 1.0,
                translucent: false,
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

        // A translucent layer goes through FFmpeg's RGB round trip, whose chroma
        // reduction pairs pixels two by two: on a picture with an odd side the
        // last pair reaches one pixel past the line (and, for an odd height, the
        // conversion leaves swscale's unscaled path for a generic one), reading
        // padding the graph never initialised. That is not something to copy; the
        // frame goes through FFmpeg, which has the same garbage to itself.
        if tf.opacity < 1.0 && (picture.0 % 2 == 1 || picture.1 % 2 == 1) {
            return Err(GeometryError(format!(
                "a translucent layer of odd size {}x{} (FFmpeg's RGB round trip reads past the picture)",
                picture.0, picture.1
            )));
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
            picture_at: (0, 0),
            picture_shows: layer,
            matte: false,
            origin,
            opacity: if tf.opacity < 1.0 {
                f32::from(ffmpeg_alpha(tf.opacity)) / 255.0
            } else {
                1.0
            },
            translucent: tf.opacity < 1.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Transform {
        Transform::default()
    }

    /// The geometry of a 4:2:0 picture, which is what most of these assert.
    fn r420(src: (u32, u32), canvas: (u32, u32), fit: Fit, tf: &Transform) -> Result<LayerGeometry, GeometryError> {
        LayerGeometry::resolve(src, canvas, fit, tf, Subsampling::YUV420)
    }

    const S422: Subsampling = Subsampling { log2_w: 1, log2_h: 0 };

    /// An odd 623x347 window at an odd (11, 7) origin of a 640x360 picture.
    fn odd_crop() -> Transform {
        Transform {
            crop_left: 11.0 / 640.0,
            crop_top: 7.0 / 360.0,
            crop_right: 6.0 / 640.0,
            crop_bottom: 6.0 / 360.0,
            ..t()
        }
    }

    #[test]
    fn identity_same_shape_is_a_copy_at_the_origin() {
        let g = r420((640, 360), (640, 360), Fit::Contain, &t()).unwrap();
        assert_eq!(g.picture, (640, 360));
        assert_eq!(g.origin, (0, 0));
        assert!(!g.matte, "a picture that fills the frame has no bars");
        assert_eq!(g.stages.len(), 1);
        assert_eq!(g.stages[0].scaled, (640, 360));
        assert!(g.rotation.is_none());
    }

    #[test]
    fn contain_letterboxes_with_ffmpegs_rounding_and_even_padding() {
        // 16:9 into 9:16: scaled to 360x203 (202.5 rounds away from zero), then
        // padded at y = 218.5 -> 218 (truncate, already even).
        let g = r420((640, 360), (360, 640), Fit::Contain, &t()).unwrap();
        assert_eq!(g.picture, (360, 203));
        // The layer is the whole canvas — `pad` emits black bars — with the
        // picture at y = 218 showing an even 202 of its rows.
        assert!(g.matte);
        assert_eq!((g.layer, g.origin), ((360, 640), (0, 0)));
        assert_eq!((g.picture_at, g.picture_shows), ((0, 218), (360, 202)));
        // An odd gap rounds down to even: 1080-607 = 473 / 2 = 236.5 -> 236.
        let g = r420((1000, 1650), (1080, 1080), Fit::Contain, &t()).unwrap();
        assert_eq!(g.picture.1, 1080);
        assert_eq!(g.picture_at.0 % 2, 0);
    }

    #[test]
    fn cover_scales_up_and_crops_the_centre() {
        // 16:9 into 9:16: scaled to cover (1138x640), then the middle 360 columns
        // are kept, at x = (1138 - 360) / 2 = 389 -> 388.
        let g = r420((640, 360), (360, 640), Fit::Cover, &t()).unwrap();
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
        let g = r420((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
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
        assert!(r420((640, 360), (640, 360), Fit::Contain, &tf).is_err());
    }

    #[test]
    fn scale_and_offset_follow_overlay_truncation() {
        let tf = Transform {
            scale: 0.5,
            pos_x: 0.25,
            pos_y: -0.1,
            ..t()
        };
        let g = r420((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
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
        let g = r420((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        assert_eq!(g.origin.0, 372);
    }

    #[test]
    fn an_absurd_offset_stays_off_canvas_instead_of_wrapping() {
        assert!(overlay_coord(1e18) > 100_000);
        assert!(overlay_coord(-1e18) < -100_000);
        assert_eq!(overlay_coord(f64::INFINITY) % 2, 0);
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
        let g = r420((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        let r = g.rotation.unwrap();
        assert_eq!(r.out, (360, 640));
        assert_eq!(g.layer, (360, 640));
        assert_eq!(g.origin, (140, -140));
        // 45 degrees of a square: side * sqrt(2) = 141.42, rounded half-up.
        let tf = Transform { rotation: 45.0, ..t() };
        let g = r420((100, 100), (100, 100), Fit::Contain, &tf).unwrap();
        assert_eq!(g.rotation.unwrap().out, (141, 141));
    }

    #[test]
    fn opacity_is_the_alpha_ffmpegs_round_trip_leaves() {
        let tf = Transform { opacity: 0.5, ..t() };
        let g = r420((640, 360), (640, 360), Fit::Contain, &tf).unwrap();
        // 0.5 * 255 = 127.5 -> lrint 128, then the conversion's 256/255: 129
        // (measured on FFmpeg 6.1 and 9.0, for all 256 alpha values).
        assert!((g.opacity - 129.0 / 255.0).abs() < 1e-6);
        assert!(g.translucent);
        assert!(g.rotation.is_none() && g.stages.len() == 1);
        // Opaque is untouched and not translucent; 0.999 is translucent (it takes
        // the RGB round trip) with an opaque alpha plane.
        let opaque = r420((640, 360), (640, 360), Fit::Contain, &t()).unwrap();
        assert!(!opaque.translucent && opaque.opacity == 1.0);
        let almost = Transform { opacity: 0.999, ..t() };
        let g = r420((640, 360), (640, 360), Fit::Contain, &almost).unwrap();
        assert!(g.translucent && g.opacity == 1.0);
    }

    #[test]
    fn a_translucent_layer_of_odd_size_is_refused_and_an_opaque_one_is_not() {
        // 640x360 at 0.5 scale is 320x180: fine. 0.33 is 211x118: an odd width.
        let even = Transform {
            scale: 0.5,
            opacity: 0.5,
            ..t()
        };
        assert!(r420((640, 360), (640, 360), Fit::Contain, &even).is_ok());
        let odd = Transform {
            scale: 0.33,
            opacity: 0.5,
            ..t()
        };
        let err = r420((640, 360), (640, 360), Fit::Contain, &odd).unwrap_err();
        assert!(err.0.contains("odd size 211x118"), "{err}");
        let opaque = Transform { scale: 0.33, ..t() };
        assert!(r420((640, 360), (640, 360), Fit::Contain, &opaque).is_ok());
        // Rotation after the scale does not matter: the picture is what is checked.
        let turned = Transform {
            scale: 0.5,
            rotation: 33.0,
            opacity: 0.5,
            ..t()
        };
        assert!(r420((640, 360), (640, 360), Fit::Contain, &turned).is_ok());
    }

    #[test]
    fn the_alpha_table_matches_what_ffmpeg_was_measured_to_write() {
        // `colorchannelmixer=aa=<op>` then `yuva420p`, FFmpeg 6.1 and 9.0.
        for (op, alpha) in [
            (0.1, 26),
            (0.2, 51),
            (0.3, 76),
            (0.5, 129),
            (0.65, 167),
            (0.8, 205),
            (0.9, 231),
            (0.99, 253),
        ] {
            assert_eq!(ffmpeg_alpha(op), alpha, "opacity {op}");
        }
        assert_eq!(ffmpeg_alpha(0.0), 0);
        assert_eq!(ffmpeg_alpha(1.0), 255);
        assert_eq!(ffmpeg_alpha(-3.0), 0);
    }

    #[test]
    fn nonsense_transforms_are_refused_not_drawn() {
        let tf = Transform { scale: f64::NAN, ..t() };
        assert!(r420((640, 360), (640, 360), Fit::Contain, &tf).is_err());
        let tf = Transform { scale: -1.0, ..t() };
        assert!(r420((640, 360), (640, 360), Fit::Contain, &tf).is_err());
    }

    #[test]
    fn a_crop_rounds_to_the_native_chroma_grid_of_the_picture() {
        let at = |sub| {
            LayerGeometry::resolve((640, 360), (320, 180), Fit::Contain, &odd_crop(), sub)
                .unwrap()
                .stages[0]
                .src
        };
        // 4:2:0: even window, even origin.
        assert_eq!(
            at(Subsampling::YUV420),
            Rect {
                x: 10,
                y: 6,
                w: 622,
                h: 346
            }
        );
        // 4:2:2: the columns are paired, the rows are not.
        assert_eq!(
            at(S422),
            Rect {
                x: 10,
                y: 7,
                w: 622,
                h: 347
            }
        );
        // 4:4:4, gray and RGB: exactly the window that was asked for.
        assert_eq!(
            at(Subsampling::NONE),
            Rect {
                x: 11,
                y: 7,
                w: 623,
                h: 347
            }
        );
    }

    #[test]
    fn the_cover_crop_and_the_letterbox_are_on_the_420_grid_whatever_the_source() {
        // With a single `scale` in the chain it has converted the picture to 4:2:0
        // before they run: only the first `crop` sees the native format. Cover 16:9
        // into 178x324: scaled to 576x324, the overhang (576-178)/2 = 199 — odd,
        // kept from 198 for every source.
        for sub in Subsampling::ALL {
            let g = LayerGeometry::resolve((640, 360), (178, 324), Fit::Cover, &t(), sub).unwrap();
            assert_eq!(g.stages[0].keep.x, 198, "{sub:?}");
            // 402x200 scales to 402x226 and (226-200)/2 = 13 rows go: 12 kept.
            let g = LayerGeometry::resolve((640, 360), (402, 200), Fit::Cover, &t(), sub).unwrap();
            assert_eq!(g.stages[0].keep.y, 12, "{sub:?}");
            // A letterbox in a 640x366 frame: a 3-row gap above, placed at row 2...
            let g = LayerGeometry::resolve((640, 360), (640, 366), Fit::Contain, &t(), sub).unwrap();
            assert_eq!(g.picture_at.1, 2, "{sub:?}");
            // ...and 360x203 shows 202 rows, 4:4:4 and RGB sources included.
            let g = LayerGeometry::resolve((640, 360), (360, 640), Fit::Contain, &t(), sub).unwrap();
            assert_eq!(g.picture_shows.1, 202, "{sub:?}");
        }
    }

    #[test]
    fn a_second_scale_leaves_the_cover_crop_on_the_native_grid() {
        // The transform's own `scale` converts to 4:2:0, so the Cover crop before
        // it still sees the picture as decoded: 199 columns, not 198, on 4:4:4 and
        // gray — and the letterbox gap and the identity case stay on 4:2:0's.
        let scaled = Transform { scale: 1.2, ..t() };
        let keep = |tf: &Transform, sub| {
            LayerGeometry::resolve((640, 360), (178, 324), Fit::Cover, tf, sub)
                .unwrap()
                .stages[0]
                .keep
        };
        assert_eq!(keep(&scaled, Subsampling::NONE).x, 199);
        assert_eq!(keep(&scaled, S422).x, 198);
        assert_eq!(keep(&scaled, Subsampling::YUV420).x, 198);
        // A transform that moves the picture without resizing it has one scale only.
        let moved = Transform { pos_x: 0.1, ..t() };
        assert_eq!(keep(&moved, Subsampling::NONE).x, 198);
        // The vertical overhang: 402x200 keeps 13 rows (not 12) on a native grid.
        let y = |tf: &Transform, sub| {
            LayerGeometry::resolve((640, 360), (402, 200), Fit::Cover, tf, sub)
                .unwrap()
                .stages[0]
                .keep
                .y
        };
        assert_eq!(
            (
                y(&scaled, Subsampling::NONE),
                y(&scaled, S422),
                y(&scaled, Subsampling::YUV420)
            ),
            (13, 13, 12)
        );
    }

    #[test]
    fn a_picture_of_unknown_grid_is_refused_only_where_the_grids_disagree() {
        let any = |canvas, fit, tf: &Transform| LayerGeometry::resolve_any_grid((640, 360), canvas, fit, tf);
        // Nothing odd to round: every grid agrees, and the geometry is the plain one.
        let g = any((640, 360), Fit::Contain, &t()).unwrap();
        assert_eq!(g, r420((640, 360), (640, 360), Fit::Contain, &t()).unwrap());
        assert!(any((320, 180), Fit::Contain, &t()).is_ok());
        // An odd crop is where the grids disagree. An odd gap or Cover overhang is
        // not: those are on the 4:2:0 grid for every source.
        let err = any((320, 180), Fit::Contain, &odd_crop()).unwrap_err();
        assert!(err.0.contains("rounds differently"), "{err}");
        for (canvas, fit) in [((640, 366), Fit::Contain), ((178, 324), Fit::Cover), ((402, 200), Fit::Cover)] {
            assert!(any(canvas, fit, &t()).is_ok(), "{canvas:?}");
        }
        // A crop that lands on a multiple of four (every grid, 4:1:1 included, rounds
        // it to itself) is the same on all of them.
        let even_crop = Transform {
            crop_left: 0.05,
            crop_right: 0.05,
            crop_top: 0.1,
            crop_bottom: 0.1,
            ..t()
        };
        assert!(any((320, 180), Fit::Contain, &even_crop).is_ok());
    }
}
