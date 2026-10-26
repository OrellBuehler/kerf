//! What a compositor can draw, and why a frame is refused.
//!
//! [`GpuCaps`] is *data*: a compositor reports which parts of a plan it renders
//! exactly (`Compositor::caps()` in `kerf-gpu`), and
//! [`RenderPlan::reasons`](crate::render_plan::RenderPlan::reasons) is a pure
//! function of the plan's fields and those caps — nothing is decided while planning
//! and stored as text, so one plan answers for every caps value and a span of
//! frames can be checked without re-planning. An A5 pass (fades, masks, text,
//! effects, reframe, HDR) is a caps flip beside the pass and its parity cases.
//!
//! [`Unsupported`] is the reason as a value; its `Display` is the message the plan
//! has always given, so a caller that logs or matches the text is unaffected.

use std::fmt;

use crate::clip_timing::{FadeEdge, Rational};
use crate::model::{BlendMode, VideoEffect};
use crate::render_plan::{YuvMatrix, MAX_AMPLIFIED_ERROR, MAX_SHRINK, MIN_TEXT_PX};

/// A set of [`VideoEffect`] kinds: what a compositor draws one by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EffectKinds(u8);

impl EffectKinds {
    pub const NONE: Self = Self(0);
    pub const BLUR: Self = Self(1);
    pub const SHARPEN: Self = Self(1 << 1);
    pub const GRAYSCALE: Self = Self(1 << 2);
    pub const INVERT: Self = Self(1 << 3);
    pub const VIGNETTE: Self = Self(1 << 4);
    pub const CHROMA_KEY: Self = Self(1 << 5);
    pub const ALL: Self = Self(0b11_1111);

    /// The kind of one effect.
    pub fn of(effect: &VideoEffect) -> Self {
        match effect {
            VideoEffect::Blur { .. } => Self::BLUR,
            VideoEffect::Sharpen { .. } => Self::SHARPEN,
            VideoEffect::Grayscale => Self::GRAYSCALE,
            VideoEffect::Invert => Self::INVERT,
            VideoEffect::Vignette => Self::VIGNETTE,
            VideoEffect::ChromaKey { .. } => Self::CHROMA_KEY,
        }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A set of the parts of a clip's [`Grade`](crate::model::Grade): what a compositor draws one by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GradeKinds(u8);

impl GradeKinds {
    pub const NONE: Self = Self(0);
    /// Colour wheels (`colorbalance`).
    pub const WHEELS: Self = Self(1);
    /// Tone curves (`lutrgb`).
    pub const CURVES: Self = Self(1 << 1);
    /// A 3D LUT (`lut3d`).
    pub const LUT: Self = Self(1 << 2);
    pub const ALL: Self = Self(0b111);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Which parts of a [`RenderPlan`](crate::render_plan::RenderPlan) a compositor
/// draws exactly. [`GpuCaps::A0`] (also the default) is what shipped first: layers,
/// transform, crop, fit, `eq`, opacity, stills and sampled keyframes — and nothing
/// below.
///
/// **Which plans an ability applies to.** `motion`, `keyed_opacity` and `keyed_color` are about
/// the export graph and apply to [`PlanMode::Motion`] plans only. `fades` and `transitions` hold for
/// both: a still plan holds the fades still changing the picture, the slide / push travel and the
/// outgoing clip on its tail exactly as the FFmpeg still draws them (`engine::cli::still_fx`), so a
/// compositor that sets them draws a paused frame inside a transition as the file shows it.
/// (`keyed_zoom` also holds for a still's moving zoom, which the FFmpeg still runs as the last stage like the
/// export does: with a grade, rotation, opacity, mask or effect in front of it the still is
/// refused unless the compositor says it orders them that way.) `mask`, `text`,
/// `reframe`, `hdr` and `effects` hold for both, since both graphs draw them.
///
/// `#[non_exhaustive]`: another crate starts from [`GpuCaps::A0`] and sets the fields it
/// draws (`let mut caps = GpuCaps::A0; caps.mask = true;`).
///
/// [`PlanMode::Motion`]: crate::render_plan::PlanMode::Motion
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct GpuCaps {
    /// Plans of an export frame ([`PlanMode::Motion`](crate::render_plan::PlanMode)):
    /// the frame pick that follows the `fps` filter and the per-frame geometry of a
    /// keyframed clip.
    pub motion: bool,
    /// `fade` steps in progress: a clip's own fades, the dips and a dissolve's ramp.
    pub fades: bool,
    /// A slide or push's travel, and a clip playing on past its end under one (the outgoing side of
    /// a dissolve, a slide or a push).
    pub transitions: bool,
    /// A dip to white: `fade c=white` takes only RGB, so FFmpeg converts the picture to `rgb24`
    /// and back around it, in every frame of the clip in an export (and in a still while the fade runs).
    pub dip_white: bool,
    /// Keyframed opacity (a `geq` alpha, a different arithmetic from the RGB round
    /// trip a constant opacity takes).
    pub keyed_opacity: bool,
    /// Keyframed zoom: FFmpeg runs it as the last stage of the clip's chain (see
    /// `Animated`), so a compositor that sets this draws the grade, the effects, the mask and the
    /// fades at the fit size first and the zoom after them. A rotation, or an opacity below 1,
    /// in front of a moving zoom is refused whatever the caps (`ZoomBehind`): both would have to
    /// be scaled with it.
    pub keyed_zoom: bool,
    /// Keyframed colour: FFmpeg writes the clip's `eq` per frame (`eval=frame`, an expression
    /// of the frame's time for each keyed number), which a compositor that sets this draws
    /// from the numbers an export frame's plan carries (`PlanLayer::color`: those expressions
    /// evaluated at the frame's time) by building the `eq` tables again every frame. Export
    /// frames only: a still plan samples the colour and draws it like a static one.
    pub keyed_color: bool,
    /// A mask (`geq` rewriting the alpha plane): drawn as a per-sample alpha. Still refused on a
    /// rotated layer and on one below full opacity (`MaskWith`).
    pub mask: bool,
    pub text: bool,
    pub reframe: bool,
    pub hdr: bool,
    pub effects: EffectKinds,
    /// The parts of a clip's grade (wheels, curves, a 3D LUT) drawn: FFmpeg carries the picture through RGB
    /// for them, and a compositor that sets a part reproduces that round trip and the filter. Still refused
    /// whatever the caps (`GradeWith`): a layer the scaler resizes, a letterbox, an odd picture, a format other
    /// than 8-bit limited-range `yuv420p`, video effects behind the grade, a running black or white fade and a moving zoom.
    pub grade: GradeKinds,
}

impl GpuCaps {
    /// What the A0 compositor draws.
    pub const A0: Self = Self {
        motion: false,
        fades: false,
        transitions: false,
        dip_white: false,
        keyed_opacity: false,
        keyed_zoom: false,
        keyed_color: false,
        mask: false,
        text: false,
        reframe: false,
        hdr: false,
        effects: EffectKinds::NONE,
        grade: GradeKinds::NONE,
    };
}

/// A layer of a plan, as a reason names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRef {
    /// Position in the plan's layers (bottom first).
    pub index: usize,
    /// The asset's name.
    pub name: String,
}

/// How a dip to white is not drawn ([`Unsupported::DipToWhite`]). `Display` finishes the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DipWhy {
    /// The compositor has no round trip for it (`GpuCaps::dip_white`).
    NoRoundTrip,
    /// The chain is on an alpha plane (a rotation, an opacity, a dissolve or a mask): FFmpeg converts through
    /// another RGB format.
    AlphaPlane,
    /// A black fade of the same clip runs ahead of it, a filter of its own in the YUV.
    BlackFade,
    /// The picture is not 8-bit limited-range `yuv420p`.
    Format,
    /// The picture's colour matrix has no measured round trip.
    Matrix,
    /// The layer is letterboxed: FFmpeg pads before the round trip, so the bars whiten too.
    Letterboxed,
    /// The picture has an odd side `(w, h)`: the round trip reads past it.
    OddSize(u32, u32),
    /// The scaler resizes the layer: the `scale` filter converts to RGB itself.
    Resized,
}

impl fmt::Display for DipWhy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoundTrip => write!(f, "(fade through an RGB round trip)"),
            Self::AlphaPlane => write!(
                f,
                "on an alpha plane (a rotation, an opacity, a dissolve or a mask in the same chain), where FFmpeg converts through another RGB format"
            ),
            Self::BlackFade => write!(f, "while a black fade of the same clip runs ahead of it"),
            Self::Format => write!(
                f,
                "from a picture that is not 8-bit limited-range `yuv420p`, which FFmpeg converts to RGB from its own chroma grid and range (9 to 21 levels off the round trip the compositor draws on 9.0.2)"
            ),
            Self::Matrix => write!(f, "from a picture whose colour matrix has no measured round trip"),
            Self::Letterboxed => write!(
                f,
                "on a letterboxed picture, whose black bars FFmpeg pads in before the round trip and so whitens too"
            ),
            Self::OddSize(w, h) => write!(f, "on a picture of odd size {w}x{h} (FFmpeg's RGB round trip reads past it)"),
            Self::Resized => write!(
                f,
                "on a layer the scaler resizes (the scale filter converts to RGB itself, from chroma the compositor has already scaled)"
            ),
        }
    }
}

/// Why a compositor cannot draw a frame exactly. `Display` is the message.
#[derive(Debug, Clone, PartialEq)]
pub enum Unsupported {
    /// The source has no video picture of a known size (never drawn, whatever the caps).
    NoPicture(LayerRef),
    /// The plan is of an export frame and the compositor does not draw those yet.
    Motion,
    /// The delivery is not 8-bit 4:2:0, which is all the compositor produces.
    Delivery(String),
    /// The delivery is a gif, which goes through a palette the compositor does not build.
    Gif,
    /// The delivery fits footage over a blurred copy of itself ([`crate::model::Fit::Blur`]): FFmpeg's
    /// `boxblur` on a thumbnail scaled back up, with no compositor pass or parity case yet.
    BlurFill,
    /// The canvas and each clip's `fps` filter parse the delivery rate to different rationals
    /// (`canvas`, `clips`), so a clip's frame slots are not the canvas's frames: the pick
    /// assumes one grid.
    PickRate {
        canvas: Rational,
        clips: Rational,
    },
    Hdr(LayerRef),
    /// The probed pixel format carries alpha.
    AlphaPicture(LayerRef, String),
    /// A pixel format that is not on the allow-list of known-opaque ones.
    NotKnownOpaque(LayerRef, String),
    /// Opacity below 1 on a picture whose colour matrix is not known (the detail says why).
    TranslucentMatrix(LayerRef, String),
    /// Colour correction on a picture that may be full range (the detail says what is known).
    GradedFullRange(LayerRef, String),
    /// Colour correction whose tables step by more than [`MAX_AMPLIFIED_ERROR`] levels between neighbouring
    /// values (the second number is the steepest step), on a picture the compositor's scaler resizes ahead of
    /// the `eq`: the scaler is a level from swscale's, and the tables multiply that by their step.
    GradeSteep(LayerRef, u32),
    /// Curves, colour wheels or a 3D LUT (the detail names which) that the compositor has no pass or parity
    /// case for (`GpuCaps::grade`): drawn by FFmpeg in RGB.
    Grade(LayerRef, &'static str),
    /// A grade the compositor draws in general but not in this layer's chain (the detail says why): FFmpeg
    /// carries the picture through RGB for the filters and back, and the way in and out is only reproduced for
    /// 8-bit limited-range `yuv420p` at the size it is drawn at.
    GradeWith(LayerRef, &'static str),
    /// A video effect the compositor does not draw (the detail names it and says why), or one
    /// whose value FFmpeg's own filter refuses.
    Effects(LayerRef, &'static str),
    Mask(LayerRef),
    /// A mask in a chain the compositor draws no mask in: the detail says which.
    MaskWith(LayerRef, &'static str),
    /// An effect on a letterboxed picture: FFmpeg's `pad` hands the filter the whole
    /// padded frame, black bars included, and the compositor draws the picture and the bars
    /// apart (the detail names what runs on the frame).
    Matte(LayerRef, &'static str),
    Reframe(LayerRef),
    /// A fade step is in progress: `true` when it is a dissolve's ramp.
    Fade(LayerRef, FadeEdge, bool),
    /// A black or white fade is running on a layer whose opacity is below 1: `colorchannelmixer` leaves the
    /// picture in RGBA and `fade` runs on that, alpha plane included, ahead of the conversion back to
    /// `yuva420p` (a dissolve's ramp is drawn: it touches the alpha alone).
    FadeOnTranslucent(LayerRef),
    /// A slide or push is moving the layer.
    Travel(LayerRef),
    /// A dip to white is in the layer's chain (`fade c=white`, through an RGB round trip) in a way the
    /// compositor does not draw: [`DipWhy`] says how.
    DipToWhite(LayerRef, DipWhy),
    /// A full-range picture inside a fade, a dip or a dissolve where FFmpeg 9 works on its raw values
    /// and converts the range afterwards (the composite colour policy is not the one that matches the
    /// compositor's order, range first), or a `yuv420p` flagged full range, whose raw values reach the fade
    /// on every FFmpeg. A still's refusal; an export frame's is [`Unsupported::FullRangeCut`], except under
    /// FFmpeg 6, which has no such cut and refuses only the flagged `yuv420p` here, in the frames of the fade.
    FullRangeNegotiated(LayerRef),
    /// The cut holds a full-range picture where FFmpeg 9 works on raw values: the export as a whole is
    /// flagged full range, every limited-range frame of it converted, and the full-range clip's own
    /// kept raw. No frame of the cut, whatever it shows (a gap and a clip before the picture included),
    /// is the compositor's limited-range one. An export frame's refusal; a plan-level one, not a layer's.
    FullRangeCut,
    /// A full-range picture the scaler resizes, in an export frame or inside a fade (the detail says
    /// which): swscale converts the range inside the resize, from chroma the compositor has already
    /// converted and scaled, which reads up to 15 levels of chroma off (FFmpeg 6 as well as 9).
    FullRangeResized(LayerRef, &'static str),
    /// The layer's clip has a speed ramp: the export retimes its frames with an expression of closed
    /// forms and `fps` picks among them, which the plan does not reproduce (a frame pick over a rate
    /// that moves), and a still of it is the frame the map names, not the one the file shows.
    SpeedRamp(LayerRef),
    /// The layer's clip is stabilized: the export moves each of its frames by the clip's analysis
    /// (vid.stab's transform, or `deshake`), which no compositor pass draws.
    Stabilized(LayerRef),
    /// The layer's clip composites with a blend mode: FFmpeg mixes it with the composite by `blend` in RGB, after a
    /// conversion each way, which no compositor pass reproduces.
    BlendMode(LayerRef, BlendMode),
    /// The layer plays on past its end, under the clip that replaces it.
    Tail(LayerRef),
    KeyedOpacity(LayerRef),
    /// Keyframed colour correction in an export frame (`eq` with `eval=frame`).
    KeyedColor(LayerRef),
    /// A keyframed mask in an export frame (its `geq` reads the mask's numbers at the frame's time).
    KeyedMask(LayerRef),
    /// A keyframed crop (a `perspective` reading the picture through its window, in the export and
    /// the FFmpeg still alike).
    KeyedCrop(LayerRef),
    /// A keyframed zoom in an export frame, which FFmpeg draws as the last stage of the chain.
    KeyedZoom(LayerRef),
    /// A moving zoom with something in front of it that the compositor does not draw in FFmpeg's
    /// order (the detail says what): FFmpeg applies those to the picture at its fit size and
    /// magnifies the result.
    ZoomBehind(LayerRef, &'static str),
    /// The picture is taken from the source in **one** resample to the frame times the clip's zoom
    /// (the detail says which case: [`crate::model::FitOrder`]), where the compositor fits the
    /// picture to the frame and scales that in a second stage.
    FoldedFit(LayerRef, &'static str),
    /// A text overlay is live.
    Text,
    /// A live text overlay names no font file the compositor can draw with (the shipped default font
    /// could not be written to disk).
    TextWithoutFont,
    /// A live text overlay is bold in a font with no bold face, which `drawtext` thickens with a
    /// same-colour border: each glyph's border bitmap is blended in turn and they overlap, which
    /// a compositor that has only their sum cannot reproduce (up to 33 levels of chroma at those pixels).
    TextBorder,
    /// A live text overlay is drawn in a font under [`MIN_TEXT_PX`] pixels (the smallest, in pixels):
    /// glyph bitmaps share too many pixels for one mask to stand for them.
    TextTooSmall(u32),
    /// An animated SVG title is on screen: resvg draws it and FFmpeg overlays its pictures.
    Svg,
    Scaler(String),
    /// The layer's colour matrix is unknown while the composite takes its matrix from the tags.
    MatrixUnknown(usize),
    MixedMatrices,
    /// The composite colour policy could not be measured and the stack is not BT.601.
    UnmeasuredPolicy(YuvMatrix),
    RgbInStack(YuvMatrix),
    /// Colour correction in a stack with a full-range picture (layer index).
    GradedInFullRangeStack(usize),
    /// A shrink steeper than [`MAX_SHRINK`]: `(layer, from, to)`.
    Shrink(usize, (u32, u32), (u32, u32)),
    /// A resize of a picture FFmpeg scales in its own format: `(layer, from, to, pix_fmt)`.
    FormatResize(usize, (u32, u32), (u32, u32), String),
    /// The geometry of this layer at this size cannot be placed (`layer`, why).
    Geometry(usize, String),
}

impl Unsupported {
    /// What kind of refusal this is, without the layer names and numbers its message carries: a
    /// stable key to count refusals by (`mixed_matrices`, `text_too_small`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NoPicture(_) => "no_picture",
            Self::Motion => "motion",
            Self::Delivery(_) => "delivery",
            Self::Gif => "gif",
            Self::BlurFill => "blur_fill",
            Self::PickRate { .. } => "pick_rate",
            Self::Hdr(_) => "hdr",
            Self::AlphaPicture(..) => "alpha_picture",
            Self::NotKnownOpaque(..) => "not_known_opaque",
            Self::TranslucentMatrix(..) => "translucent_matrix",
            Self::GradedFullRange(..) => "graded_full_range",
            Self::GradeSteep(..) => "grade_steep",
            Self::Grade(..) => "grade",
            Self::GradeWith(..) => "grade_with",
            Self::Effects(..) => "effects",
            Self::Mask(_) => "mask",
            Self::MaskWith(..) => "mask_with",
            Self::Matte(..) => "matte",
            Self::Reframe(_) => "reframe",
            Self::Fade(..) => "fade",
            Self::FadeOnTranslucent(_) => "fade_on_translucent",
            Self::Travel(_) => "travel",
            Self::DipToWhite(..) => "dip_to_white",
            Self::FullRangeNegotiated(_) => "full_range_negotiated",
            Self::FullRangeCut => "full_range_cut",
            Self::FullRangeResized(..) => "full_range_resized",
            Self::SpeedRamp(_) => "speed_ramp",
            Self::Stabilized(_) => "stabilized",
            Self::BlendMode(..) => "blend_mode",
            Self::Tail(_) => "tail",
            Self::KeyedOpacity(_) => "keyed_opacity",
            Self::KeyedColor(_) => "keyed_color",
            Self::KeyedMask(_) => "keyed_mask",
            Self::KeyedCrop(_) => "keyed_crop",
            Self::KeyedZoom(_) => "keyed_zoom",
            Self::ZoomBehind(..) => "zoom_behind",
            Self::FoldedFit(..) => "folded_fit",
            Self::Text => "text",
            Self::TextWithoutFont => "text_without_font",
            Self::TextBorder => "text_border",
            Self::TextTooSmall(_) => "text_too_small",
            Self::Svg => "svg",
            Self::Scaler(_) => "scaler",
            Self::MatrixUnknown(_) => "matrix_unknown",
            Self::MixedMatrices => "mixed_matrices",
            Self::UnmeasuredPolicy(_) => "unmeasured_policy",
            Self::RgbInStack(_) => "rgb_in_stack",
            Self::GradedInFullRangeStack(_) => "graded_in_full_range_stack",
            Self::Shrink(..) => "shrink",
            Self::FormatResize(..) => "format_resize",
            Self::Geometry(..) => "geometry",
        }
    }
}

impl fmt::Display for LayerRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "layer {} ({})", self.index, self.name)
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPicture(l) => write!(f, "{l}: the source has no video picture of a known size"),
            Self::Motion => f.write_str("the plan is of an export frame (motion mode), which the compositor does not draw yet"),
            Self::Delivery(pf) => write!(f, "the delivery is {pf}, and the compositor draws 8-bit 4:2:0 only"),
            Self::Gif => f.write_str("a gif delivery goes through a palette the compositor does not build"),
            Self::BlurFill => f.write_str("the delivery fills the frame with a blurred copy of the footage, which the compositor does not draw"),
            Self::PickRate { canvas, clips } => write!(
                f,
                "the delivery rate is {}/{} on the canvas and {}/{} on a clip's fps filter, which are different frame grids",
                canvas.num, canvas.den, clips.num, clips.den
            ),
            Self::Hdr(l) => write!(f, "{l}: HDR footage needs tone mapping"),
            Self::AlphaPicture(l, pf) => write!(f, "{l}: the picture has an alpha channel ({pf})"),
            Self::NotKnownOpaque(l, pf) => {
                write!(f, "{l}: the pixel format {pf} is not one known to be opaque (it may carry alpha)")
            }
            Self::TranslucentMatrix(l, detail) => {
                write!(f, "{l}: opacity below 1 on a picture whose colour matrix is {detail}")
            }
            Self::GradedFullRange(l, detail) => write!(f, "{l}: colour correction on {detail}"),
            Self::GradeSteep(l, step) => write!(
                f,
                "{l}: colour correction whose tables step {step} levels between neighbouring values, on a picture the compositor scales ahead of it (the scaler's one-level difference from FFmpeg's becomes {step}, past the {MAX_AMPLIFIED_ERROR} levels held)"
            ),
            Self::Grade(l, what) => write!(f, "{l}: {what} (drawn by FFmpeg only)"),
            Self::GradeWith(l, what) => write!(f, "{l}: a grade {what}"),
            Self::Effects(l, what) => write!(f, "{l}: video effects ({what})"),
            Self::Mask(l) => write!(f, "{l}: mask"),
            Self::MaskWith(l, what) => write!(f, "{l}: a mask {what}, which the compositor does not draw"),
            Self::Matte(l, what) => write!(
                f,
                "{l}: {what} on a letterboxed picture (FFmpeg runs it on the padded frame, black bars included)"
            ),
            Self::Reframe(l) => write!(f, "{l}: 360 reframe"),
            Self::Fade(l, _, true) => write!(f, "{l}: inside a dissolve"),
            Self::Fade(l, FadeEdge::In, false) => write!(f, "{l}: inside its fade-in"),
            Self::Fade(l, FadeEdge::Out, false) => write!(f, "{l}: inside its fade-out"),
            Self::FadeOnTranslucent(l) => write!(
                f,
                "{l}: a fade to black or white on a layer below full opacity (FFmpeg runs it on the RGBA picture the opacity leaves, alpha included)"
            ),
            Self::Travel(l) => write!(f, "{l}: moving through a slide or push transition"),
            Self::DipToWhite(l, why) => write!(f, "{l}: a dip to white {why}"),
            Self::FullRangeNegotiated(l) => write!(
                f,
                "{l}: a full-range picture inside a fade, a dip or a dissolve, where FFmpeg works on its raw values and converts the range afterwards (FFmpeg 9 for any full-range picture, FFmpeg 6 too for a yuv420p flagged full range)"
            ),
            Self::FullRangeCut => write!(
                f,
                "the cut holds a full-range picture: FFmpeg 9 exports the whole stream as full range (its raw values kept, every limited-range frame converted), so no frame of it reads as drawn"
            ),
            Self::FullRangeResized(l, where_) => write!(
                f,
                "{l}: a full-range picture {where_} that the scaler resizes (swscale converts its range inside the resize: chroma up to 15 levels off)"
            ),
            Self::Stabilized(l) => write!(
                f,
                "{l}: stabilized (the export moves each frame by the clip's analysis, which the compositor does not draw)"
            ),
            Self::SpeedRamp(l) => write!(
                f,
                "{l}: a speed ramp (the export retimes its frames by an expression and picks among them, which the compositor does not reproduce)"
            ),
            Self::BlendMode(l, mode) => write!(
                f,
                "{l}: the {} blend mode (FFmpeg mixes it with the composite by `blend` in RGB, which the compositor does not reproduce)",
                mode.label().to_lowercase()
            ),
            Self::Tail(l) => write!(f, "{l}: playing on past its end under a transition"),
            Self::KeyedOpacity(l) => write!(f, "{l}: keyframed opacity (a geq alpha written per frame, which no compositor pass draws or has a parity case for yet)"),
            Self::KeyedColor(l) => write!(
                f,
                "{l}: keyframed colour correction (an eq whose numbers are evaluated per frame, which this compositor does not rebuild its tables for)"
            ),
            Self::KeyedMask(l) => write!(
                f,
                "{l}: a keyframed mask (a geq alpha written per frame, which no compositor pass draws or has a parity case for yet)"
            ),
            Self::KeyedCrop(l) => write!(
                f,
                "{l}: a keyframed crop (a perspective reading the picture through its window, which no compositor pass draws)"
            ),
            Self::KeyedZoom(l) => write!(
                f,
                "{l}: keyframed zoom (FFmpeg draws it as the last stage of the clip's chain, after the effects, mask and rotation)"
            ),
            Self::ZoomBehind(l, what) => write!(
                f,
                "{l}: a moving zoom with {what} ahead of it (FFmpeg applies it at the fit size and magnifies the result)"
            ),
            Self::FoldedFit(l, what) => write!(
                f,
                "{l}: {what} (FFmpeg takes the picture from the source in one resample, to the frame times the zoom; the compositor fits it to the frame and scales that)"
            ),
            Self::Text => f.write_str("a text overlay is live"),
            Self::TextWithoutFont => f.write_str("a text overlay names no font file to draw with"),
            Self::TextTooSmall(px) => write!(
                f,
                "a text overlay in a {px} px font, under the {MIN_TEXT_PX} px whose glyph bitmaps a mask stands for exactly"
            ),
            Self::TextBorder => f.write_str(
                "a bold text overlay in a font with no bold face, which drawtext thickens with an overlapping border",
            ),
            Self::Svg => f.write_str("an animated SVG title is on screen"),
            Self::Scaler(s) => write!(f, "scaler '{s}'"),
            Self::MatrixUnknown(n) => write!(
                f,
                "layer {n}: its colour matrix is unknown, and FFmpeg's composite takes its matrix from the layers' tags"
            ),
            Self::MixedMatrices => f.write_str(
                "layers tagged with different YCbCr matrices: FFmpeg converts them into the bottom layer's, which is not reproduced",
            ),
            Self::UnmeasuredPolicy(m) => write!(
                f,
                "a {m:?} stack, and this FFmpeg's composite colour policy could not be measured (FFmpeg 6 and 9 convert BT.709 and BT.2020 footage differently)"
            ),
            Self::RgbInStack(m) => write!(
                f,
                "an RGB picture in a {m:?} stack: FFmpeg converts it with the stack's matrix, which is not reproduced"
            ),
            Self::GradedInFullRangeStack(n) => write!(
                f,
                "layer {n}: colour correction in a stack with a full-range picture, whose range FFmpeg negotiates across the layers"
            ),
            Self::Shrink(n, from, to) => write!(
                f,
                "layer {n}: shrinks a {}x{} picture to {}x{}, steeper than the {MAX_SHRINK}:1 the scaler comparison covers",
                from.0, from.1, to.0, to.1
            ),
            Self::FormatResize(n, from, to, pf) => write!(
                f,
                "layer {n}: scales a {}x{} picture to {}x{}, and its format ({pf}) is not 8/10-bit 4:2:0 or gray, which FFmpeg scales in its own format",
                from.0, from.1, to.0, to.1
            ),
            Self::Geometry(n, why) => write!(f, "layer {n}: {why}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(index: usize, name: &str) -> LayerRef {
        LayerRef {
            index,
            name: name.into(),
        }
    }

    #[test]
    fn a_kind_names_the_refusal_and_not_the_layer_or_the_numbers() {
        assert_eq!(
            Unsupported::Mask(layer(0, "a.mp4")).kind(),
            Unsupported::Mask(layer(3, "b.mov")).kind()
        );
        assert_eq!(Unsupported::TextTooSmall(8).kind(), Unsupported::TextTooSmall(11).kind());
        assert_eq!(Unsupported::Shrink(0, (4000, 2000), (50, 20)).kind(), "shrink");
        assert_ne!(Unsupported::Text.kind(), Unsupported::TextBorder.kind());
        assert_eq!(Unsupported::MixedMatrices.kind(), "mixed_matrices");
        assert_eq!(Unsupported::SpeedRamp(layer(1, "a.mp4")).kind(), "speed_ramp");
        assert_eq!(
            Unsupported::BlendMode(layer(0, "a.mp4"), BlendMode::Multiply).kind(),
            Unsupported::BlendMode(layer(2, "b.mov"), BlendMode::Screen).kind(),
            "the mode is not part of the key"
        );
        assert!(Unsupported::BlendMode(layer(1, "a.mp4"), BlendMode::HardLight)
            .to_string()
            .starts_with("layer 1 (a.mp4): the hard light blend mode"));
        assert!(Unsupported::SpeedRamp(layer(1, "a.mp4"))
            .to_string()
            .starts_with("layer 1 (a.mp4): a speed ramp"));
        assert_eq!(
            Unsupported::DipToWhite(layer(0, "a.mp4"), DipWhy::Resized).kind(),
            Unsupported::DipToWhite(layer(2, "b.mov"), DipWhy::Format).kind(),
            "the why of a dip is not part of the key"
        );
        let kinds = [
            Unsupported::Motion.kind(),
            Unsupported::Gif.kind(),
            Unsupported::BlurFill.kind(),
            Unsupported::Text.kind(),
            Unsupported::TextWithoutFont.kind(),
            Unsupported::TextBorder.kind(),
            Unsupported::Svg.kind(),
            Unsupported::Hdr(layer(0, "")).kind(),
            Unsupported::Effects(layer(0, ""), "a blur").kind(),
            Unsupported::Grade(layer(0, ""), "curves").kind(),
            Unsupported::GradeWith(layer(0, ""), "on a letterboxed picture").kind(),
            Unsupported::GradeSteep(layer(0, ""), 12).kind(),
            Unsupported::MaskWith(layer(0, ""), "on a rotated layer").kind(),
            Unsupported::Matte(layer(0, ""), "of a translucent layer").kind(),
            Unsupported::FadeOnTranslucent(layer(0, "")).kind(),
            Unsupported::DipToWhite(layer(0, ""), DipWhy::NoRoundTrip).kind(),
            Unsupported::FullRangeNegotiated(layer(0, "")).kind(),
            Unsupported::FullRangeCut.kind(),
            Unsupported::FullRangeResized(layer(0, ""), "an export frame").kind(),
            Unsupported::SpeedRamp(layer(0, "")).kind(),
            Unsupported::Stabilized(layer(0, "")).kind(),
            Unsupported::BlendMode(layer(0, ""), BlendMode::Screen).kind(),
        ];
        for kind in kinds {
            assert!(
                kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{kind} is a key, not a sentence"
            );
        }
    }
}
