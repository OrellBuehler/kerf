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

/// How much more than the greatest magnification the scaled copy is given: what keeps the bilinear
/// read at a step of a little over one pixel at the clip's tightest frame.
const HEADROOM: f64 = 1.02;

/// The plan of a moving still's picture: the scaled copy, the part of it any frame shows, and the
/// frame the move is drawn into.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct StillMotion {
    /// The still scaled once to `q.0` x `q.1`.
    q: (u32, u32),
    /// The part of the scaled copy that any frame reads, `(x, y, w, h)`, at least the frame's size.
    window: (u32, u32, u32, u32),
    /// The frame the picture is drawn into.
    frame: (u32, u32),
    /// The picture fitted to the frame, before the transform's zoom (frame pixels).
    fit: (f64, f64),
    /// The static crop as fractions of the still: left, top, width, height.
    crop: (f64, f64, f64, f64),
}

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
        let t = &clip.transform;
        let still_angle = !clip.is_driven(Property::Rotation) && t.rotation != 0.0;
        let turns = clip.rotation_turns();
        let spatial = clip.effects.iter().any(|e| {
            matches!(
                e,
                VideoEffect::Blur { .. } | VideoEffect::Sharpen { .. } | VideoEffect::Vignette
            )
        });
        if !clip.picture_moves()
            || still_angle
            || turns
            || spatial
            || clip.mask.is_some()
            || clip.reframe.is_some()
            || times.is_empty()
        {
            return None;
        }
        let (sw, sh) = picture.filter(|&(w, h)| w > 0 && h > 0)?;
        if frame.0 < 2 || frame.1 < 2 {
            return None;
        }
        let (fw, fh) = (f64::from(frame.0), f64::from(frame.1));
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
        let aspect = crop.2 * f64::from(sw) / (crop.3 * f64::from(sh));
        let fit_size = match fit {
            Fit::Cover => (fw.max(fh * aspect), fh.max(fw / aspect)),
            Fit::Contain | Fit::Blur => (fw.min(fh * aspect), fh.min(fw / aspect)),
        };
        // What of the picture the general chain shows, round its centre: the frame under a `Cover`
        // that crops to it first, the whole fitted picture otherwise.
        let shown = match (
            fit,
            clip.fit_order(fit, crate::layer_geometry::cover_overflows(Some((sw, sh)), judge, t)),
        ) {
            (Fit::Cover, FitOrder::Fitted) => (fw, fh),
            _ => fit_size,
        };
        let mut most = 0.0_f64;
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &local in times {
            let pose = clip.transform_at(local);
            let (wl, wt, side, _) = clip.crop_window_at(local);
            let s = pose.scale;
            if !(s.is_finite() && s > 0.0 && side.is_finite() && side > 0.0 && side <= 1.0 + 1e-9) {
                return None;
            }
            let covers = |shown: f64, pos: f64, side: f64| shown * s + 1e-6 * side >= side * (1.0 + 2.0 * pos.abs());
            if !covers(shown.0, pose.pos_x, fw) || !covers(shown.1, pose.pos_y, fh) {
                return None;
            }
            // Frame pixels per width of the whole still, and where the frame's edges fall on it.
            let (rw, rh) = (fit_size.0 * s, fit_size.1 * s);
            let (cw, ch) = (crop.2 * side, crop.3 * side);
            most = most.max(rw / cw);
            let (cx0, cy0) = (crop.0 + crop.2 * wl, crop.1 + crop.3 * wt);
            let left = fw / 2.0 + pose.pos_x * fw - rw / 2.0;
            let top = fh / 2.0 + pose.pos_y * fh - rh / 2.0;
            let u = |x: f64| cx0 + (x - left) / rw * cw;
            let v = |y: f64| cy0 + (y - top) / rh * ch;
            x0 = x0.min(u(0.0));
            x1 = x1.max(u(fw));
            y0 = y0.min(v(0.0));
            y1 = y1.max(v(fh));
        }
        let ceil_even = |v: f64| ((v / 2.0).ceil().max(1.0) as u32) * 2;
        let qw = ceil_even(most * HEADROOM);
        let qh = (((f64::from(qw) * f64::from(sh) / f64::from(sw)) / 2.0).round().max(1.0) as u32) * 2;
        if qw < frame.0 || qh < frame.1 || u64::from(qw) * u64::from(qh) > 400_000_000 {
            return None;
        }
        // What any frame reads, a couple of pixels wider for the bilinear read at its edge, in even
        // steps (a 4:2:0 still's crop would round an odd corner), never less than the frame.
        let span = |a: f64, b: f64, q: u32, need: u32| {
            let qf = f64::from(q);
            let need = ceil_even(f64::from(need)).min(q);
            let lo = ((a.clamp(0.0, 1.0) * qf - 2.0) / 2.0).floor().max(0.0) as u32 * 2;
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

    /// The filters that run once, ahead of the `loop`: the still scaled with `sf` (the delivery's
    /// scaler flags, `:flags=...`; Lanczos when it names none: it runs once) and cut to the window
    /// any frame reads.
    pub(super) fn once(&self, sf: &str) -> String {
        let (x, y, w, h) = self.window;
        let sf = if sf.is_empty() { ":flags=lanczos" } else { sf };
        format!("scale={}:{}{sf},crop={w}:{h}:{x}:{y}", self.q.0, self.q.1)
    }

    /// The `perspective` corners of the frame for a pose: output pixel `x` of the window reads the
    /// scaled copy at `x0 + x * (x1 - x0) / w`, which the pose's fit, zoom, position and crop window
    /// put at `(cx0 + ((x + 0.5) - left) / rw * cw) * Q - 0.5 - X` (pixel centres; `left` the
    /// picture's edge on the frame, `rw` its width, `cx0`/`cw` the part of the still it shows).
    fn corners(&self, p: &Pose) -> String {
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
            self.corners(&pose),
            self.cut()
        )
    }

    /// The scrubbed still's filters: the pose at `local`, as numbers, through the same corners.
    pub(super) fn at(&self, clip: &Clip, local: f64) -> String {
        let n = |p: Property| fnum(clip.property_at(p, local));
        let pose = self.pose(clip, &n);
        format!("perspective={}:interpolation=linear,{}", self.corners(&pose), self.cut())
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
