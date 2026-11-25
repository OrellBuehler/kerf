//! A still image whose picture moves across the frame — a keyed zoom, a keyed position, the animated
//! crop, any of them together (the Ken Burns move) — drawn in one sub-pixel resample from a copy of
//! the still scaled once (pure; `video_clip_chain` and the scrubbed still both build from it).
//!
//! The general chain draws such a clip as footage: the animated crop a `perspective` at the source's
//! size (45 MP 4:4:4 a frame for a camera's JPEG), a keyed zoom a `scale` to whole pixels of the
//! picture *already fitted to the frame* placed by an `overlay` at whole pixels. A slow zoom grows by
//! less than a pixel a frame, so the picture stepped and its centre wobbled as `(W-w)/2` changed
//! parity, and what the zoom showed was the fitted picture enlarged.
//!
//! Here the still (decoded once and held by `loop`, as every still is) is scaled once with the
//! delivery's scaler, Lanczos by default, to the smallest size that still has a pixel for every pixel
//! of the frame at the clip's greatest magnification ([`HEADROOM`] over it), cut once to what any frame
//! of the clip shows, and each frame is the one `perspective` (bilinear, 1/256 px) that maps the frame
//! onto the window the model says: the fit, the transform's zoom and position
//! ([`Clip::transform_at`]) and the crop's window ([`Clip::crop_window_at`]), every number per frame.
//! The frame comes out at the delivery's size and is placed at the canvas's corner; a slide or push
//! still moves it.
//!
//! It is taken only where it draws what the general chain would at every frame, sub-pixel
//! ([`StillMotion::new`]): a still with no rotation, mask, reframe, tone-map or spatial effect, whose
//! picture covers the whole frame at every frame of the clip (a zoom in, a pan inside it). Anything
//! else keeps the general chain.

use super::*;

/// The numbers that place the picture at one moment, each as the text the graph evaluates (a curve
/// over the frame's time in the export, a number in the still).
struct Pose {
    scale: String,
    pos_x: String,
    pos_y: String,
    crop_zoom: String,
    crop_x: String,
    crop_y: String,
}

/// The still's size as the graph decodes it ([`displayed_size`]: autorotated).
pub(super) fn displayed(asset: Option<&Asset>) -> Option<(u32, u32)> {
    let stream = asset?.streams.iter().find(|s| s.kind == StreamKind::Video)?;
    match displayed_size(stream.width, stream.height, stream.rotation) {
        (Some(w), Some(h)) => Some((w, h)),
        _ => None,
    }
}

pub(super) use crate::still_motion::StillMotion;

impl StillMotion {
    /// The plan for `clip` (a still image of `picture` pixels) on a `frame` delivered at `fit`, the
    /// moves judged at the clip-local `times` (every frame it is drawn on), or `None` where the
    /// general chain draws it: a still that does not move, or whose move this cannot draw as that
    /// chain does. `judge` is the delivery's own frame, which decides the fit order (as it does for
    /// a scrubbed still drawn smaller).
    pub(super) fn new(
        clip: &Clip,
        picture: Option<(u32, u32)>,
        judge: (u32, u32),
        frame: (u32, u32),
        fit: Fit,
        times: &[f64],
    ) -> Option<Self> {
        let stat = crate::still_motion::StillStatic::of_clip(clip, picture, judge, fit)?;
        let samples: Vec<_> = times
            .iter()
            .map(|&local| crate::still_motion::StillSample::at(clip, local))
            .collect();
        Self::build(&stat, frame, &samples)
    }

    /// The `perspective` corners of the frame for a pose: output pixel `x` of the window reads the
    /// scaled copy at `x0 + x * (x1 - x0) / w`, which the pose's fit, zoom, position and crop window
    /// put at `(cx0 + ((x + 0.5) - left) / rw * cw) * Q - 0.5 - X` (pixel centres; `left` the
    /// picture's edge on the frame, `rw` its width, `cx0`/`cw` the part of the still it shows).
    fn corner_text(&self, p: &Pose) -> String {
        let axis = |q: u32, start: f64, len: f64, at: u32, frame: u32, fitted: f64, pos: &str, pan: &str, out: u32| {
            let q = f64::from(q);
            let base = fnum(q * start - 0.5 - f64::from(at));
            let k = fnum(q * len);
            let edge = |x: u32| {
                let xc = fnum(f64::from(x) + 0.5 - f64::from(frame) / 2.0);
                format!(
                    "{base}+{k}*((1-1/({cz}))*(1+({pan}))/2+({xc}-({pos})*{f})/(({cz})*{fitted}*({s}))+0.5/({cz}))",
                    cz = p.crop_zoom,
                    f = frame,
                    fitted = fnum(fitted),
                    s = p.scale,
                )
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
            &p.pos_x,
            &p.crop_x,
            ww,
        );
        let (ya, yb) = axis(
            self.q.1,
            self.crop.1,
            self.crop.3,
            wy,
            self.frame.1,
            self.fit.1,
            &p.pos_y,
            &p.crop_y,
            wh,
        );
        format!("x0='{xa}':y0='{ya}':x1='{xb}':y1='{ya}':x2='{xa}':y2='{yb}':x3='{xb}':y3='{yb}'")
    }

    /// The per-frame filters of the export: `perspective` over the pose's curves at the time of
    /// output frame `first + in - 1` (the `fps` ahead of it pins its first frame there, as a keyed
    /// crop's does: `keyed_crop_fps`), then the frame cut from the window's corner.
    pub(super) fn each_frame(&self, clip: &Clip, rate: Rational, first: i64) -> String {
        let at = first - 1;
        let frame = match at {
            0 => "in".to_string(),
            a if a > 0 => format!("in+{a}"),
            a => format!("in-{}", -a),
        };
        let t = format!("(({frame})*{}/{})", rate.den, rate.num);
        let n = |p: Property| driven_expr(clip, p, &t);
        let pose = self.pose(clip, &n);
        format!(
            "perspective={}:interpolation=linear:eval=frame,{}",
            self.corner_text(&pose),
            self.cut()
        )
    }

    /// The scrubbed still's filters: the pose at `local`, as numbers, through the same corners.
    pub(super) fn at(&self, clip: &Clip, local: f64) -> String {
        let n = |p: Property| fnum(clip.property_at(p, local));
        let pose = self.pose(clip, &n);
        format!("perspective={}:interpolation=linear,{}", self.corner_text(&pose), self.cut())
    }

    fn pose(&self, clip: &Clip, n: &dyn Fn(Property) -> String) -> Pose {
        // A crop that never zooms in draws the whole picture (`Clip::crop_window_at`).
        let crop = |p: Property| if clip.crop_animated() { n(p) } else { fnum(p.neutral()) };
        Pose {
            scale: n(Property::Scale),
            pos_x: n(Property::PosX),
            pos_y: n(Property::PosY),
            crop_zoom: crop(Property::CropZoom),
            crop_x: crop(Property::CropX),
            crop_y: crop(Property::CropY),
        }
    }

    fn cut(&self) -> String {
        format!("crop={}:{}:0:0", self.frame.0, self.frame.1)
    }
}
