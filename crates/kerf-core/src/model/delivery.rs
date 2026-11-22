//! How a cut is delivered: the fit, the delivery format, framing and smart crop.

use super::*;

/// How a clip's picture is fitted to an output frame of a different shape.
///
/// This is what makes a vertical or square delivery usable. `Contain` letterboxes,
/// which is right when the delivery matches the footage and wrong the moment it
/// doesn't: a 16:9 shot rendered at 1080x1920 becomes a 1080x608 strip in a
/// mostly-black frame — technically the whole picture, and not something anyone
/// would post. `Cover` fills the frame and throws away the overflow instead; `Blur`
/// keeps the whole picture and fills the bars with a blurred, enlarged copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Scale to fit inside the frame and pad the remainder with black. The
    /// default, and the historical behaviour.
    #[default]
    Contain,
    /// Scale to cover the frame and crop what hangs over the edges.
    Cover,
    /// `Contain` over a blurred copy of the same picture scaled to cover the frame: the
    /// whole shot, with the bars filled by its own colours (the vertical-video habit for
    /// landscape footage). A clip whose geometry is not the plain fit (scaled, moved,
    /// rotated, cropped or animated) is placed as under `Contain`, with no fill.
    Blur,
}

/// How a clip's chain fits its picture to the delivery frame and applies the clip's static zoom
/// ([`Clip::fit_order`]). The picture is resampled once whenever it can be: a zoom taken from
/// a picture already fitted to the frame is a fraction of the source's detail, enlarged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitOrder {
    /// Fit to the frame (`Cover` then crops to it), then zoom: the order every chain had, and
    /// still has for the identity transform, a zoom out, a moving zoom, and a `Cover` clip that
    /// shows nothing more than the frame (or has a mask or a vignette, laid out over the frame).
    Fitted,
    /// `Contain`, zoomed in: one `scale` from the source to the fit of the frame times the zoom.
    Folded,
    /// `Cover` with a zoom or a position (or any zoom out): one `scale` from the source to the
    /// cover of the frame times the zoom, **not cropped to the frame** — the overlay cuts what hangs
    /// over the canvas, so a position pans across the real picture instead of revealing black.
    Uncropped,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Contain => "contain",
            Fit::Cover => "cover",
            Fit::Blur => "blur",
        }
    }
}

/// The frame the project is cut *for* — the shape of the thing being delivered.
///
/// Without one, a timeline's shape is whatever the first video clip happens to
/// be, and a vertical delivery exists only as a resolution typed into the export
/// dialog. That is backwards for short-form work: the 9:16 crop decides which
/// half of every shot survives, so it has to be visible while the cut is made,
/// not discovered in the rendered file. Setting this makes the preview, the
/// scrubbed still, the streamed playback and the export all render the same
/// frame — an explicit export `resolution` still wins, so nothing about the
/// existing dialog changes. The same goes for the frame rate it may carry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Delivery {
    pub width: u32,
    pub height: u32,
    /// How footage of a different shape meets that frame. `Cover` is the useful
    /// default for a reframed delivery; `Contain` keeps the whole picture, and `Blur`
    /// keeps it over a blurred copy of itself.
    #[serde(default)]
    pub fit: Fit,
    /// The frame rate the cut is delivered at: what the export, the streamed playback and
    /// the stills render at when the export options name none. Absent follows the footage
    /// (the first clip with a picture, which in mixed footage is whichever comes first),
    /// and is not written, so a project that never set one stores what it always did. Set
    /// through [`Delivery::with_fps`], which holds it to
    /// [`MIN_DELIVERY_FPS`]`..=`[`MAX_DELIVERY_FPS`] and makes an NTSC rate written short
    /// (`29.97`) the exact `30000/1001`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
}

/// The slowest and the fastest rate a delivery may name.
pub const MIN_DELIVERY_FPS: f64 = 1.0;
pub const MAX_DELIVERY_FPS: f64 = 240.0;

/// How near a rate must be to an NTSC one (`n * 1000 / 1001`) to be taken for it. `23.976`, `29.97`
/// and `59.94` are how `24000/1001`, `30000/1001` and `60000/1001` are written, and FFmpeg reads the
/// short decimals as `2997/100` — a different rate, which counts out of step with the footage a
/// player knows as 29.97.
const NTSC_SNAP: f64 = 0.005;

/// The whole rates that have an NTSC twin.
const NTSC_BASES: [f64; 6] = [24.0, 30.0, 48.0, 60.0, 120.0, 240.0];

/// A delivery frame rate, checked: positive, finite and within
/// [`MIN_DELIVERY_FPS`]`..=`[`MAX_DELIVERY_FPS`], with an NTSC rate written short (`23.976`,
/// `29.97`, `59.94`) made the exact `n * 1000 / 1001`. Any other rate is kept as given.
pub fn check_delivery_fps(fps: f64) -> Result<f64> {
    if !fps.is_finite() || !(MIN_DELIVERY_FPS..=MAX_DELIVERY_FPS).contains(&fps) {
        return Err(Error::InvalidArgument(format!(
            "a delivery frame rate must be between {MIN_DELIVERY_FPS} and {MAX_DELIVERY_FPS} fps, got {fps}"
        )));
    }
    let ntsc = NTSC_BASES
        .iter()
        .map(|base| base * 1000.0 / 1001.0)
        .find(|ntsc| (fps - ntsc).abs() < NTSC_SNAP);
    Ok(ntsc.unwrap_or(fps))
}

/// A frame rate as people write it: `50`, `29.97`, `23.976`.
pub fn fmt_fps(fps: f64) -> String {
    format!("{}", (fps * 1000.0).round() / 1000.0)
}

impl Delivery {
    /// Even-clamped, and never zero — the dimensions reach a filtergraph. Follows the
    /// footage's frame rate; [`Delivery::with_fps`] states one.
    pub fn new(width: u32, height: u32, fit: Fit) -> Self {
        let even = |v: u32| v.max(2) & !1;
        Self {
            width: even(width),
            height: even(height),
            fit,
            fps: None,
        }
    }

    /// This frame delivered at `fps` (`None` follows the footage), checked by
    /// [`check_delivery_fps`].
    pub fn with_fps(self, fps: Option<f64>) -> Result<Self> {
        Ok(Self {
            fps: fps.map(check_delivery_fps).transpose()?,
            ..self
        })
    }

    /// The rate the engine renders at: [`Delivery::fps`] when it is one a delivery may name. A
    /// stored value out of range (a hand-edited file) is ignored rather than reaching a filtergraph.
    pub fn rate(&self) -> Option<f64> {
        self.fps
            .filter(|f| f.is_finite() && (MIN_DELIVERY_FPS..=MAX_DELIVERY_FPS).contains(f))
    }

    pub fn aspect(&self) -> f64 {
        self.width as f64 / self.height.max(1) as f64
    }

    /// The frame's shape reduced to lowest terms — `(9, 16)` for 1080x1920 —
    /// which is what a per-clip [`Framing`] is keyed by, so a 720x1280 render
    /// and a 1080x1920 one share the crop.
    pub fn ratio(&self) -> (u32, u32) {
        reduce_ratio(self.width, self.height)
    }

    /// The shape as people write it: `9:16`.
    pub fn ratio_label(&self) -> String {
        let (w, h) = self.ratio();
        format!("{w}:{h}")
    }

    /// The delivery frame a shape name stands for: `"9:16"`, `"1:1"`, `"4:5"`,
    /// `"16:9"` — the sizes the app's delivery picker uses, with the fit that
    /// picker pairs them with (a vertical or square frame fills and crops, the
    /// landscape one keeps the whole picture) — or an explicit `WxH`, which
    /// covers when it is not 16:9. `None` for anything else.
    pub fn parse(name: &str) -> Option<Delivery> {
        let name = name.trim();
        let (w, h) = match name {
            "9:16" | "vertical" | "portrait" => (1080, 1920),
            "1:1" | "square" => (1080, 1080),
            "4:5" => (1080, 1350),
            "16:9" | "landscape" => (1920, 1080),
            _ => {
                let (w, h) = name.split_once(['x', 'X', '×'])?;
                (w.trim().parse().ok()?, h.trim().parse().ok()?)
            }
        };
        if w == 0 || h == 0 {
            return None;
        }
        let fit = if reduce_ratio(w, h) == (16, 9) {
            Fit::Contain
        } else {
            Fit::Cover
        };
        Some(Delivery::new(w, h, fit))
    }
}

fn reduce_ratio(w: u32, h: u32) -> (u32, u32) {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let d = gcd(w, h).max(1);
    (w / d, h / d)
}

/// A crop a clip carries for **one** delivery shape it is not being cut in.
///
/// Smart crop writes its result into `Transform.crop_*`, which is right for the
/// frame the project is cut for and wrong for every other: the crop that keeps
/// the subject in a 9:16 Reel throws the subject away in a 1:1 post, and
/// re-framing for the second shape overwrote the first. A `Framing` is that
/// second answer kept beside the first, keyed by the reduced shape, so one cut
/// can be delivered at several frames with each shot framed for each. It is
/// only read by [`Timeline::for_delivery`] — the ordinary render of the project
/// frame never sees it, so a project with none renders byte-identically.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Framing {
    /// The delivery shape this crop is for, in lowest terms — `(9, 16)`.
    pub aspect_w: u32,
    pub aspect_h: u32,
    /// Fraction of the source cropped from each edge.
    pub crop_left: f64,
    pub crop_right: f64,
    pub crop_top: f64,
    pub crop_bottom: f64,
}

impl Framing {
    pub fn new(ratio: (u32, u32), crop: &CropFrame) -> Self {
        Self {
            aspect_w: ratio.0,
            aspect_h: ratio.1,
            crop_left: crop.left,
            crop_right: crop.right,
            crop_top: crop.top,
            crop_bottom: crop.bottom,
        }
    }

    pub fn ratio(&self) -> (u32, u32) {
        (self.aspect_w, self.aspect_h)
    }

    pub fn ratio_label(&self) -> String {
        format!("{}:{}", self.aspect_w, self.aspect_h)
    }

    pub(super) fn crop(&self) -> (f64, f64, f64, f64) {
        (self.crop_left, self.crop_right, self.crop_top, self.crop_bottom)
    }
}

/// A coarse map of where a shot's *content* is: `rows`×`cols` non-negative
/// weights sampled across a source window, row-major.
///
/// Built by [`crate::engine::salience_map`] from a handful of tiny grayscale
/// frames — per cell, the edge energy of the picture plus how much it moved.
/// That combination is what makes it usable on both kinds of shot a social cut
/// is made of: a locked-off talking head has no motion but plenty of facial
/// detail against a soft background, and a handheld follow has both. It is
/// deliberately *not* face detection — no model to ship, no licence to carry,
/// and the answer only has to be good enough to beat a centre crop, which is
/// what the alternative actually is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SalienceMap {
    pub cols: usize,
    pub rows: usize,
    /// `rows * cols` weights, row-major.
    pub cells: Vec<f32>,
}

/// How far the salient window is allowed to pull away from centre before the
/// pull has to be *earned*. Scored against the window's share of total
/// salience, so a flat map (an evenly-lit wide shot, a gradient, black) resolves
/// to the centre crop rather than to whichever edge won by rounding.
const CENTER_BIAS: f64 = 0.25;

/// Candidate window positions evaluated across the cropped axis. The window
/// edges are interpolated within a bucket, so this is finer than `cols`.
const CROP_SEARCH_STEPS: usize = 240;

/// Aspect ratios within this relative tolerance are the same shape — 1920x1080
/// into a 1280x720 frame needs no crop, and neither does 1080x1350 into 4:5.
const ASPECT_TOLERANCE: f64 = 0.01;

/// A crop window as the per-edge source fractions [`Transform`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CropFrame {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    /// How far the window sits from a plain centre crop, as a fraction of the
    /// travel available to it (0.0 = dead centre, 1.0 = hard against an edge).
    /// Reported so a caller can say *why* the shot moved.
    pub offset: f64,
}

impl CropFrame {
    /// The centred crop keeping `keep` of `axis` — what a `Cover` fit does on
    /// its own, and the answer whenever the content gives no reason to move.
    fn centered(keep: f64, horizontal: bool) -> Self {
        Self::at(0.5 * (1.0 - keep), keep, horizontal, 0.0)
    }

    fn at(start: f64, keep: f64, horizontal: bool, offset: f64) -> Self {
        let (near, far) = (start, (1.0 - start - keep).max(0.0));
        if horizontal {
            Self {
                left: near,
                right: far,
                top: 0.0,
                bottom: 0.0,
                offset,
            }
        } else {
            Self {
                left: 0.0,
                right: 0.0,
                top: near,
                bottom: far,
                offset,
            }
        }
    }

    /// Whether this window is (near enough) the plain centre crop.
    pub fn is_centered(&self) -> bool {
        self.offset.abs() < 1e-6
    }
}

/// Whether footage of this shape has to lose part of itself to fill a frame of
/// `target_aspect`. False when the two are the same shape within
/// [`ASPECT_TOLERANCE`] — 1920x1080 into a 1280x720 frame keeps all of itself —
/// and when either shape is nonsense.
pub fn needs_crop(source_w: u32, source_h: u32, target_aspect: f64) -> bool {
    let source_aspect = source_w as f64 / source_h.max(1) as f64;
    if !source_aspect.is_finite() || source_aspect <= 0.0 || !target_aspect.is_finite() || target_aspect <= 0.0 {
        return false;
    }
    ((source_aspect - target_aspect) / target_aspect).abs() > ASPECT_TOLERANCE
}

impl SalienceMap {
    pub fn new(cols: usize, rows: usize, cells: Vec<f32>) -> Self {
        Self { cols, rows, cells }
    }

    fn is_valid(&self) -> bool {
        self.cols > 0 && self.rows > 0 && self.cells.len() == self.cols * self.rows
    }

    /// Salience collapsed onto one axis: per column when `horizontal`, else per
    /// row. Negative weights are clamped away so a bad sample can't subtract.
    fn axis(&self, horizontal: bool) -> Vec<f64> {
        let n = if horizontal { self.cols } else { self.rows };
        let mut out = vec![0.0; n];
        for (i, cell) in self.cells.iter().enumerate() {
            let bucket = if horizontal { i % self.cols } else { i / self.cols };
            out[bucket] += (*cell as f64).max(0.0);
        }
        out
    }

    /// The crop that frames this shot's content for `target_aspect`.
    ///
    /// `None` when the source is already that shape — there is nothing to
    /// choose, and writing a no-op crop into every clip would only be noise in
    /// the inspector. Otherwise the long axis is cropped to the target ratio and
    /// the window is placed where the content is, which is the whole point: a
    /// 16:9 interview with the subject on the left third loses their head to a
    /// centre crop, and that is the default every other path here would take.
    pub fn crop_for(&self, source_w: u32, source_h: u32, target_aspect: f64) -> Option<CropFrame> {
        if !needs_crop(source_w, source_h, target_aspect) {
            return None;
        }
        let source_aspect = source_w as f64 / source_h.max(1) as f64;
        // Wider than the frame → crop the width; taller → crop the height.
        let horizontal = source_aspect > target_aspect;
        let keep = if horizontal {
            target_aspect / source_aspect
        } else {
            source_aspect / target_aspect
        }
        .clamp(0.01, 1.0);

        if !self.is_valid() {
            return Some(CropFrame::centered(keep, horizontal));
        }
        let weights = self.axis(horizontal);
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return Some(CropFrame::centered(keep, horizontal));
        }

        let travel = 1.0 - keep;
        let mut best = (f64::NEG_INFINITY, 0.5 * travel);
        for step in 0..=CROP_SEARCH_STEPS {
            let start = travel * step as f64 / CROP_SEARCH_STEPS as f64;
            let share = window_sum(&weights, start, start + keep) / total;
            let drift = ((start + 0.5 * keep) - 0.5).abs() * 2.0;
            let score = share - CENTER_BIAS * drift;
            if score > best.0 {
                best = (score, start);
            }
        }

        let start = best.1;
        // Report — and store — the exact centre when the search landed on it, so
        // an unmoved shot reads as unmoved instead of as a 0.4% pan.
        let offset = if travel > 1e-9 { (start / travel - 0.5) * 2.0 } else { 0.0 };
        if offset.abs() < 0.02 {
            return Some(CropFrame::centered(keep, horizontal));
        }
        Some(CropFrame::at(start, keep, horizontal, offset))
    }
}

/// Salience between two positions on a 0..1 axis, with the end buckets counted
/// by the fraction of them the window actually covers — so sliding the window
/// by less than a bucket changes the score smoothly instead of in steps.
fn window_sum(weights: &[f64], from: f64, to: f64) -> f64 {
    let n = weights.len() as f64;
    let (from, to) = (from.clamp(0.0, 1.0) * n, to.clamp(0.0, 1.0) * n);
    let mut sum = 0.0;
    for (i, w) in weights.iter().enumerate() {
        let (lo, hi) = (i as f64, i as f64 + 1.0);
        let overlap = to.min(hi) - from.max(lo);
        if overlap > 0.0 {
            sum += w * overlap;
        }
    }
    sum
}
