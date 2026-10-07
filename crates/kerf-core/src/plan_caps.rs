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
use crate::model::VideoEffect;
use crate::render_plan::{YuvMatrix, MAX_SHRINK};

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

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Which parts of a [`RenderPlan`](crate::render_plan::RenderPlan) a compositor
/// draws exactly. [`GpuCaps::A0`] (also the default) is what shipped first: layers,
/// transform, crop, fit, `eq`, opacity, stills and sampled keyframes — and nothing
/// below.
///
/// **Which plans an ability applies to.** `fades`, `transitions`, `keyed_opacity` and
/// `keyed_zoom` are about the export graph and apply to [`PlanMode::Motion`] plans only:
/// the FFmpeg still draws no fades and a still plan has no tail layers, so a *still* plan
/// inside a fade, a travel or a tail is refused whatever these say. (`keyed_zoom` also
/// holds for a still's moving zoom, which the FFmpeg still runs as the last stage like the
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
    /// A slide or push's travel, and a clip playing on past its end under one.
    pub transitions: bool,
    /// Keyframed opacity (a `geq` alpha, a different arithmetic from the RGB round
    /// trip a constant opacity takes).
    pub keyed_opacity: bool,
    /// Keyframed zoom: FFmpeg runs it as the last stage of the clip's chain (see
    /// `Animated`), so a compositor that sets this draws the effects, mask, rotation and
    /// grade at the fit size first and the zoom after them.
    pub keyed_zoom: bool,
    pub mask: bool,
    pub text: bool,
    pub reframe: bool,
    pub hdr: bool,
    pub effects: EffectKinds,
}

impl GpuCaps {
    /// What the A0 compositor draws.
    pub const A0: Self = Self {
        motion: false,
        fades: false,
        transitions: false,
        keyed_opacity: false,
        keyed_zoom: false,
        mask: false,
        text: false,
        reframe: false,
        hdr: false,
        effects: EffectKinds::NONE,
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
    Effects(LayerRef),
    Mask(LayerRef),
    Reframe(LayerRef),
    /// A fade step is in progress: `true` when it is a dissolve's ramp.
    Fade(LayerRef, FadeEdge, bool),
    /// A slide or push is moving the layer.
    Travel(LayerRef),
    /// The layer plays on past its end, under the clip that replaces it.
    Tail(LayerRef),
    /// A still plan's clip whose tail window is open: the export draws it, the plan has no layer.
    StillTail(LayerRef),
    KeyedOpacity(LayerRef),
    /// A keyframed zoom in an export frame, which FFmpeg draws as the last stage of the chain.
    KeyedZoom(LayerRef),
    /// A moving zoom in a still, with a grade, a rotation or a fade of opacity in front of it:
    /// FFmpeg applies those to the picture at its fit size and magnifies the result.
    ZoomBehind(LayerRef),
    /// A text overlay is live.
    Text,
    /// A live text overlay names no font file the compositor can draw with.
    TextWithoutFont,
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
            Self::Effects(l) => write!(f, "{l}: video effects"),
            Self::Mask(l) => write!(f, "{l}: mask"),
            Self::Reframe(l) => write!(f, "{l}: 360 reframe"),
            Self::Fade(l, _, true) => write!(f, "{l}: inside a dissolve"),
            Self::Fade(l, FadeEdge::In, false) => write!(f, "{l}: inside its fade-in"),
            Self::Fade(l, FadeEdge::Out, false) => write!(f, "{l}: inside its fade-out"),
            Self::Travel(l) => write!(f, "{l}: moving through a slide or push transition"),
            Self::Tail(l) => write!(f, "{l}: playing on past its end under a transition"),
            Self::StillTail(l) => write!(
                f,
                "{}: playing on past its end under a transition, which a still plan does not draw",
                l.name
            ),
            Self::KeyedOpacity(l) => write!(f, "{l}: keyframed opacity (a geq alpha, whose rounding is not measured)"),
            Self::KeyedZoom(l) => write!(
                f,
                "{l}: keyframed zoom (FFmpeg draws it as the last stage of the clip's chain, after the effects, mask and rotation)"
            ),
            Self::ZoomBehind(l) => write!(
                f,
                "{l}: a moving zoom with a grade, rotation or opacity ahead of it (FFmpeg applies those at the fit size and magnifies the result)"
            ),
            Self::Text => f.write_str("a text overlay is live"),
            Self::TextWithoutFont => f.write_str("a text overlay names no font file to draw with"),
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
