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
use crate::render_plan::{YuvMatrix, MAX_AMPLIFIED_ERROR};

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

    /// The kinds of `self` that are not `other`.
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// How FFmpeg makes the fit picture of a still whose placement is drawn ([`crate::FitCopy`]), as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FitShape {
    /// A Cover frame fitted by one stage: the engine's own `scale=W:H:force_original_aspect_ratio=increase,crop=W:H`.
    Engine,
    /// The same behind the clip's static `crop` (`crop=w=iw*..:h=ih*..:x=iw*..:y=ih*..`).
    EngineCrop,
    /// Any other fit (Contain, a held zoom, a second stage): the geometry's `scale` stages written out at the probed sizes behind a
    /// guard `crop` (`exact=1`) that refuses a picture of another size, and **no other `crop`**.
    Staged,
    /// The geometry's stages with a `crop` of their own besides the guard: the clip's static crop ahead of a scale (a Contain frame behind
    /// a crop) or the Cover overhang cut after one. No toy cut of a probe takes this text, so it is **not a chain that can be
    /// measured** ([`StillChain::measurable`]): the caps never hold it and the plan refuses it (FFmpeg draws it).
    Cropped,
}

/// One chain FFmpeg makes a moving still's picture by, and so one thing a compositor measures: which route draws it (the move inside
/// the frame, [`crate::still_motion`], or the fit picture of a placement), the shape of the fit's text and whether the delivery
/// names the `bicubic` scaler (`:flags=bicubic` is part of every `scale` of the text, a different graph from the default one's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StillChain {
    /// `None`: the moving still's own route. `Some`: the fit picture of a placement, in the shape its text has.
    pub fit: Option<FitShape>,
    pub bicubic: bool,
}

impl StillChain {
    /// How many chains there are.
    pub const COUNT: usize = 8;

    /// The moving still's own route.
    pub const fn inside(bicubic: bool) -> Self {
        Self { fit: None, bicubic }
    }

    /// The fit picture of a placement.
    pub const fn edge(shape: FitShape, bicubic: bool) -> Self {
        Self {
            fit: Some(shape),
            bicubic,
        }
    }

    /// Every chain, in the order of [`StillChain::index`].
    pub const ALL: [Self; Self::COUNT] = [
        Self::inside(false),
        Self::edge(FitShape::Engine, false),
        Self::edge(FitShape::EngineCrop, false),
        Self::edge(FitShape::Staged, false),
        Self::inside(true),
        Self::edge(FitShape::Engine, true),
        Self::edge(FitShape::EngineCrop, true),
        Self::edge(FitShape::Staged, true),
    ];

    /// The chain's place in [`StillChain::ALL`]: 0 to 3 the default scaler's (the move inside the frame, then the fit's three shapes), 4 to 7 the
    /// `bicubic` named ones in the same order. A chain that cannot be measured ([`StillChain::measurable`]) has none: [`StillChain::COUNT`].
    pub const fn index(self) -> usize {
        let shape = match self.fit {
            None => 0,
            Some(FitShape::Engine) => 1,
            Some(FitShape::EngineCrop) => 2,
            Some(FitShape::Staged) => 3,
            Some(FitShape::Cropped) => return Self::COUNT,
        };
        shape + if self.bicubic { 4 } else { 0 }
    }

    /// Whether a probe has a toy cut that takes this chain's text: every chain of [`StillChain::ALL`]. Any other is FFmpeg's.
    pub const fn measurable(self) -> bool {
        self.index() < Self::COUNT
    }

    /// Whether this is one of the two chains every kind is measured for first, with the default scaler: the move inside the frame and
    /// the engine's own fit of a Cover frame.
    pub const fn is_base(self) -> bool {
        self.index() <= 1
    }
}

/// A set of the chains a moving still is drawn by, per kind of picture ([`GpuCaps::still_motion`]): which kind (a JPEG, an RGB picture) and
/// which [`StillChain`] of it this FFmpeg was measured to make as the model does. The kind constants ([`StillKinds::YUVJ420P`], ...) are
/// each kind's two **base** chains, the ones a plain Ken Burns slideshow takes; [`StillKinds::chain`] is one chain of one kind.
///
/// Besides the chains there is one bit per kind and **named scaler** ([`StillKinds::named_route`]): the move inside the frame
/// ([`StillChain::inside`]) of a delivery that names `bilinear`, `lanczos`, `neighbor` or `spline`, whose text carries `:flags=<name>` in
/// every `scale`. (The fit pictures of a placement are not measured under a name: they stay FFmpeg's.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StillKinds(u64);

impl StillKinds {
    const PER_KIND: u32 = StillChain::COUNT as u32;
    const BASE: u64 = 0b11;
    /// Where the named routes' bits start: after the chains of the three kinds; then four names a kind.
    const NAMED_AT: u32 = 3 * Self::PER_KIND;
    const NAMES: u32 = 4;

    pub const NONE: Self = Self(0);
    /// A JPEG: 8-bit 4:2:0, full range (`yuvj420p`): its base chains.
    pub const YUVJ420P: Self = Self(Self::BASE);
    /// A PNG without alpha, or any other `rgb24` picture: its base chains.
    pub const RGB24: Self = Self(Self::BASE << Self::PER_KIND);
    /// 8-bit limited-range 4:2:0 (`yuv420p`: a WebP, a frame saved as a picture): its base chains.
    pub const YUV420P: Self = Self(Self::BASE << (2 * Self::PER_KIND));
    /// Every chain of every kind, and every named route.
    pub const ALL: Self = Self((1 << (Self::NAMED_AT + 3 * Self::NAMES)) - 1);

    /// The kind of a picture of this pixel format, if it is one a compositor can measure.
    pub fn of_pix_fmt(fmt: &str) -> Option<Self> {
        match fmt.to_ascii_lowercase().as_str() {
            "yuvj420p" => Some(Self::YUVJ420P),
            "rgb24" => Some(Self::RGB24),
            "yuv420p" => Some(Self::YUV420P),
            _ => None,
        }
    }

    /// Which of the three kinds `kind` is (a kind constant, or any set with a chain of one kind only), if it is one.
    const fn kind_index(self) -> Option<u32> {
        let mut k = 0;
        while k < 3 {
            if self.0 & (((1 << Self::PER_KIND) - 1) << (k * Self::PER_KIND)) == self.0 && self.0 != 0 {
                return Some(k);
            }
            k += 1;
        }
        None
    }

    /// The place of a scaler name among the four a route is measured under: `bilinear`, `lanczos`, `neighbor`, `spline` (the order of
    /// [`ScalerKinds`]' bits). `None` for no name, for several, and for one that is no name here.
    const fn name_index(name: ScalerKinds) -> Option<u32> {
        match name.0 {
            1 => Some(0),
            2 => Some(1),
            4 => Some(2),
            8 => Some(3),
            _ => None,
        }
    }

    /// The one bit of the move inside the frame ([`StillChain::inside`]) of a delivery that names the scaler `name`, for the kind `self`
    /// names (a kind constant): what a plan needs of the caps to draw a layer of that kind on the route under that name.
    /// [`StillKinds::NONE`] for a set that is not one kind's and for a `name` that is not one of the four.
    pub const fn named_route(self, name: ScalerKinds) -> Self {
        match (self.kind_index(), Self::name_index(name)) {
            (Some(k), Some(n)) => Self(1 << (Self::NAMED_AT + k * Self::NAMES + n)),
            _ => Self::NONE,
        }
    }

    /// The route of `kind` under the scaler `name` ([`StillKinds::named_route`]), measured: the set of one bit.
    pub const fn contains_named_route(self, kind: Self, name: ScalerKinds) -> bool {
        self.contains(kind.named_route(name)) && kind.named_route(name).0 != 0
    }

    /// The one chain `chain` of the kind `self` names (a kind constant): what a plan needs of the caps to draw a layer of that kind
    /// whose picture is made that way. [`StillKinds::NONE`] for a set that is not one kind's.
    pub const fn chain(self, chain: StillChain) -> Self {
        match self.kind_index() {
            Some(k) if chain.measurable() => Self(1 << (k * Self::PER_KIND + chain.index() as u32)),
            _ => Self::NONE,
        }
    }

    /// Every chain of the kind `self` names (a kind constant).
    pub const fn all_chains(self) -> Self {
        match self.kind_index() {
            Some(k) => Self(((1 << Self::PER_KIND) - 1) << (k * Self::PER_KIND)),
            None => Self::NONE,
        }
    }

    /// The chain `chain` of `kind` ([`StillKinds::chain`]), measured: the set of one bit.
    pub const fn contains_chain(self, kind: Self, chain: StillChain) -> bool {
        self.contains(kind.chain(chain)) && kind.chain(chain).0 != 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// This set without the chain `chain` of the kind `kind` ([`StillKinds::chain`]).
    pub const fn minus_chain(self, kind: Self, chain: StillChain) -> Self {
        Self(self.0 & !kind.chain(chain).0)
    }

    /// This set without the route of `kind` under the scaler `name` ([`StillKinds::named_route`]).
    pub const fn minus_named_route(self, kind: Self, name: ScalerKinds) -> Self {
        Self(self.0 & !kind.named_route(name).0)
    }
}

/// A set of [`BlendMode`]s: the modes a compositor draws one by one (`Normal` is plain alpha-over, always drawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlendKinds(u16);

impl BlendKinds {
    pub const NONE: Self = Self(0);
    pub const ALL: Self = Self(0b1_1111_1111);

    /// The kind of one mode; none for `Normal`.
    pub const fn of(mode: BlendMode) -> Self {
        match mode {
            BlendMode::Normal => Self(0),
            BlendMode::Multiply => Self(1),
            BlendMode::Screen => Self(1 << 1),
            BlendMode::Overlay => Self(1 << 2),
            BlendMode::HardLight => Self(1 << 3),
            BlendMode::Add => Self(1 << 4),
            BlendMode::Darken => Self(1 << 5),
            BlendMode::Lighten => Self(1 << 6),
            BlendMode::Difference => Self(1 << 7),
            BlendMode::Exclusion => Self(1 << 8),
        }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The kinds of both.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// The scalers a delivery can name (`scale=…:flags=<name>`, the engine's `SCALERS`) that a compositor draws besides `bicubic` and
/// the unnamed one: each a fact about the FFmpeg it ran against that a probe measured (swscale's Lanczos and spline tables are
/// computed in `double` with `libm`, and a build's arithmetic is not every build's). `bicubic` is drawn wherever the scaler is
/// exact at all, so it is no member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScalerKinds(u8);

impl ScalerKinds {
    pub const NONE: Self = Self(0);
    pub const BILINEAR: Self = Self(1);
    pub const LANCZOS: Self = Self(1 << 1);
    pub const NEIGHBOR: Self = Self(1 << 2);
    pub const SPLINE: Self = Self(1 << 3);
    pub const ALL: Self = Self(0b1111);

    /// The kind a delivery's scaler name is, spelled as the engine's list has it (`SCALERS`); none for `bicubic` and for a name
    /// the engine does not know (it writes no flags for one, and the plan refuses it).
    pub fn of_name(name: &str) -> Option<Self> {
        match name {
            "bilinear" => Some(Self::BILINEAR),
            "lanczos" => Some(Self::LANCZOS),
            "neighbor" => Some(Self::NEIGHBOR),
            "spline" => Some(Self::SPLINE),
            _ => None,
        }
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// What a compositor draws of a blend beyond the modes ([`BlendKinds`]), each a fact about the FFmpeg it ran against that a probe
/// measured: pictures that are RGB (the graph's `format=yuva420p` is the decode's conversion on this build), a BT.2020 composite and a
/// clip below full opacity (the export's own `colorchannelmixer` ahead of the blend's `split` is the RGB round trip the model makes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlendExtras(u8);

impl BlendExtras {
    pub const NONE: Self = Self(0);
    /// An RGB picture under a blend: the graph converts it to `yuva420p`, the compositor has the decode's 4:2:0.
    pub const RGB_PICTURE: Self = Self(1);
    /// A composite whose conversions name BT.2020.
    pub const BT2020: Self = Self(1 << 1);
    /// A clip below full opacity under a blend: the RGB round trip's layer, `lutyuv`ed and placed twice by the blend chain, is the model's.
    pub const TRANSLUCENT: Self = Self(1 << 2);
    pub const ALL: Self = Self(0b111);

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
    /// fades at the fit size first and the zoom after them. A rotation, an opacity below 1 and a dip to white
    /// in front of a moving zoom are refused whatever the caps (`ZoomBehind`): the placement would act on the rotated
    /// alpha plane, on the RGB round trip's or on the RGB frame `fade c=white` leaves, and no case holds any of them to
    /// FFmpeg's (a grade is not here: it is drawn ahead of the placement). This is the order only; the zoom itself is drawn
    /// by the placement ([`GpuCaps::placement`]), without which a moving zoom is refused (`KeyedPlacement`).
    pub keyed_zoom: bool,
    /// A moving zoom or position placed at sub-pixel positions: FFmpeg ends the clip's chain in a
    /// `perspective` that lays the picture on a canvas-sized frame (bilinear, fixed-point; the export,
    /// the playback stream and the still alike). A compositor that sets it reproduces `pad`, `perspective`
    /// and `crop` ([`crate::perspective`]) after the rest of the chain, which it draws at the fit size
    /// (`keyed_zoom`); without it every frame of such a layer is refused (`KeyedPlacement`). **A compositor sets it
    /// only where it measured that this FFmpeg's `pad`, `perspective` and `crop` place a picture as its model does**
    /// (a build with another rounding in any of them would be drawn wrong, sample for sample, on every moving zoom).
    /// With it set,
    /// a rotation, an opacity below 1 and a dip to white ahead of the placement are still refused
    /// (`ZoomBehind`), and so are an odd-height picture and a pose beyond the table's range (`Geometry`); a grade
    /// ahead of it is drawn where the compositor draws the grade (`grade`).
    pub placement: bool,
    /// A 4:4:4 or 4:2:2 (8-bit, planar, limited range) picture under a placement: the placement chain converts it to
    /// `yuva420p` in the graph, and the compositor's decode converted it to 4:2:0 ahead of it. They are the same arithmetic
    /// where this FFmpeg's `scale` filter defaults to the scaler the decode uses and not where it does not (4.4.2's default
    /// is bilinear: 24 levels of chroma off on 4:4:4, 16 on 4:2:2), so a compositor sets this only where it **measured**
    /// that the two conversions agree on the FFmpeg in use; without it such a layer is refused (`PlacedFormat`).
    pub placed_chroma: bool,
    /// The host asks a placed layer's file for its range ahead of the plans it judges (`kerf_gpu::prepare`,
    /// `prepare_ahead`, the render bar's hook: the compositor's `session_caps` set this), so the plan may read what that left in the cache
    /// ([`source_is_full_range_known`](crate::source_is_full_range_known)): where the composite's range is negotiated, a placed
    /// layer whose asset did not record its range is refused (`PlacedFormat`) when the file is full range **or has not said**,
    /// instead of that being found out at draw time, a frame into a session. It reads the cache, never a file; **a plan
    /// judged without it is a pure function of its inputs** (every other host, every test), and the compositor's own entry
    /// points leave it off because its decode is what asks the file for the frame it draws.
    pub file_range: bool,
    /// Live behaviours (`Clip.behaviors`: a sway, breath, spin, drift or shake on top of the pose). The
    /// plan carries the exact pose they give at the frame's time, so a compositor that draws the pose
    /// draws them; every frame of such a layer is refused (`Behavior`) until one sets this (it is the placement's,
    /// measured with it, and the `sin` and `cos` of the filter's machine are part of what is measured), and a sway or a
    /// spin (which turns the picture: a rotation no compositor draws to the sample) is refused whatever the caps.
    pub behaviors: bool,
    /// Keyframed colour: FFmpeg writes the clip's `eq` per frame (`eval=frame`, an expression
    /// of the frame's time for each keyed number), which a compositor that sets this draws
    /// from the numbers an export frame's plan carries (`PlanLayer::color`: those expressions
    /// evaluated at the frame's time) by building the `eq` tables again every frame. Export
    /// frames only: a still plan samples the colour and draws it like a static one.
    pub keyed_color: bool,
    /// A mask (`geq` rewriting the alpha plane): drawn as a per-sample alpha. Still refused (`MaskWith`) on a
    /// rotated layer, over a chroma key and on a blended layer below full opacity. A constant opacity below 1 is drawn:
    /// `colorchannelmixer` maps each level of the mask's alpha ([`crate::layer_geometry::ffmpeg_alpha_of`]), where the translucent
    /// round trip is (`translucent_approximate`); a keyed one is the same `geq`'s `f64` product with the mask's keep
    /// ([`crate::mask_alpha_plane_keyed`]).
    pub mask: bool,
    /// Text overlays: drawn as the glyph bitmaps `drawtext` blends, each in turn (their sum is not their
    /// blends: a synthetic-bold border's bitmaps overlap along every letter gap, small print shares samples
    /// all along the line). A compositor sets it only where it can take this build's `drawtext` apart and
    /// knows how it converts colours; the plan then still refuses the texts it can tell are beyond that
    /// ([`Unsupported::TextSequence`]) and the render refuses, per text, what it cannot reproduce exactly.
    pub text: bool,
    /// Floating animated SVG titles: resvg draws the picture, FFmpeg itself converts it to the `yuva420p` its `overlay` takes and the
    /// compositor blends it with `overlay`'s own arithmetic (`kerf_gpu::svg`). A compositor sets it only where it **measured** that this
    /// FFmpeg's `overlay` over a picture and a composite is that conversion blended by the model, at several sizes and offsets; without it
    /// every title is FFmpeg's ([`Unsupported::SvgOverlay`]). A title clip (a clip chain, not an overlay) is drawn by no compositor whatever the caps.
    pub svg: bool,
    pub reframe: bool,
    pub hdr: bool,
    pub effects: EffectKinds,
    /// The pictures a moving still ([`crate::still_motion`]) is drawn for: FFmpeg scales the still once and
    /// moves it with one `perspective`, and the compositor has the scaled copy from FFmpeg itself and draws the
    /// `perspective` from its own tables. A compositor sets a chain of a kind ([`StillKinds::chain`]) only where it **measured**
    /// that this FFmpeg's chain for a picture of that pixel format, in that shape (the move inside the frame, the fit of a
    /// placement in the engine's text, behind a static crop or staged; with the delivery's scaler named or not) is the one it
    /// reproduces; a still of another kind or in a chain not measured is refused (`StillMotion`).
    pub still_motion: StillKinds,
    /// The parts of a clip's grade (wheels, curves, a 3D LUT) drawn: FFmpeg carries the picture through RGB
    /// for them, and a compositor that sets a part reproduces that round trip and the filter. Still refused
    /// whatever the caps (`GradeWith`): a layer the scaler resizes, a letterbox, an odd picture, a format other
    /// than 8-bit limited-range `yuv420p`, video effects behind the grade and a running black or white fade. (A moving zoom
    /// or position after the grade is drawn: the placement reads the planes the grade hands on.)
    pub grade: GradeKinds,
    /// The compositor's scaler is swscale's **to the level** on this machine: the vertical kernel FFmpeg runs
    /// (its x86 SIMD one or the C one) was measured and is the one the compositor draws with. Without it a resized
    /// layer is a level from FFmpeg's (swscale's x86 kernel truncates every tap) and what that level would grow
    /// into is refused: a resized layer under a grade, the blurred fill.
    pub exact_scaler: bool,
    /// The scalers a delivery can name that this compositor's tables were measured to be swscale's for on this machine
    /// ([`ScalerKinds`]): a delivery that names one not in the set is FFmpeg's ([`Unsupported::Scaler`]). `bicubic` and the unnamed
    /// scaler need no entry; [`GpuCaps::exact_scaler`] decides them.
    pub scalers: ScalerKinds,
    /// The blurred fill of a `Fit::Blur` delivery ([`crate::blur_fill`]): the thumbnail chain scaled, box-blurred and
    /// scaled back up behind the fitted picture. Needs [`GpuCaps::exact_scaler`] as well: the fill is two bilinear
    /// scales and a blur of the picture, and the picture a bicubic one.
    pub blur_fill: bool,
    /// A letterboxed layer's **padded frame** is laid out whole before what follows `pad` in the chain (`LayerGeometry::matte`): a
    /// blur, a sharpen, a vignette and a grade see the bars as picture, and run on a frame of the canvas' size. Without it a
    /// blur, a sharpen, a vignette or a grade on a letterbox is FFmpeg's ([`Unsupported::Matte`]). The plan still refuses the chains it was
    /// not held to ([`Unsupported::Effects`], [`Unsupported::GradeWith`]).
    pub matte_effects: bool,
    /// The compositor takes a layer into the composite's colour matrix the way FFmpeg 9 does where a stack or a cut
    /// holds pictures of more than one ([`crate::render_plan::CompositeColorPolicy::BottomLayerTag`]): swscale's two-step conversion through
    /// RGB (its x86 `yuv420p -> bgr24` kernel, then `ff_rgb24toyv12`, or the generic scaler for a picture on an alpha
    /// plane), **measured** on this FFmpeg's graph (`kerf_gpu::convert`). Without it a stack or a cut of mixed matrices is
    /// FFmpeg's.
    pub matrix_convert: bool,
    /// ... and a **scale that resizes** the picture as it changes its matrix (FFmpeg 9's `scale` with different matrices in and out:
    /// a still's last `scale`, a dissolving clip's fit `scale`) is drawn as swscale's cascade runs it when the picture shrinks
    /// (`yuv420p -> bgr24` at the destination's size in one scaler, then `ff_rgb24toyv12`), **measured** on this FFmpeg's own graph
    /// (`kerf_gpu::convert::scaled`). Needs [`GpuCaps::matrix_convert`] as well. Without it that layer is FFmpeg's
    /// ([`Unsupported::MixedMatricesWith`]).
    pub matrix_convert_scaled: bool,
    /// The compositor's scaler is **not** measured to be swscale's (a probe that found it differs, could not tell, or has not run):
    /// it draws a resized layer a level from FFmpeg's, which the exact rule does not allow, so the plan refuses every layer that a
    /// scale resizes ([`Unsupported::ScalerApproximate`]). [`GpuCaps::A0`] leaves it off: what A0 draws, it draws as it always did.
    pub scaler_approximate: bool,
    /// The blend modes drawn: FFmpeg mixes a blended clip with the composite by `blend` in RGB, through a conversion
    /// each way, and a compositor that sets a mode reproduces that chain, to the sample, on this FFmpeg. Refused whatever
    /// the caps (`BlendMode` with its cause): a layer the compositor scales, one turned by anything but a quarter
    /// turn `rotate` lands on whole samples for, a letterbox, a graded layer, an opacity below 1 (the RGB round trip),
    /// a clip or a bottom layer tagged with a matrix no table is proven for (`fcc`, `smpte240m`, `bt2020c`, `ycgco`, ...), and a
    /// BT.2020 composite or an RGB picture unless `blend_extras` says this FFmpeg's chain was measured to match for it.
    pub blend: BlendKinds,
    /// What a blend is drawn for beyond its modes: RGB pictures, a BT.2020 composite and a clip below full opacity (`BlendExtras`).
    pub blend_extras: BlendExtras,
    /// The compositor's RGB round trip for a layer below full opacity is **not** measured to be this FFmpeg's to the sample (a probe
    /// that found a difference, could not tell, or has not run): `colorchannelmixer` only takes RGB, so FFmpeg takes the layer
    /// `yuva420p -> argb -> yuva420p`, and the way back runs on this machine's swscale kernels (x86 SIMD, NEON, plain C: they differ
    /// by a level of chroma). Every layer that takes the round trip is then refused ([`Unsupported::TranslucentApproximate`]).
    /// [`GpuCaps::A0`] leaves it off: what A0 draws, it draws as it always did.
    pub translucent_approximate: bool,
    /// The compositor draws the RGB round trip of a layer below full opacity whose picture has an **odd side** as this FFmpeg's swscale
    /// does it (`kerf-gpu`'s `roundtrip_probe::odd_translucency`, measured on the export's own filter text over odd pictures of every
    /// shape: the unscaled converter leaves an odd width's last column unwritten, an odd height takes the scaler's generic path, and
    /// the way back makes the last chroma column of an odd width from pixel pairs (6.1) or from a scaled row (9.0)). Off, such a layer
    /// is refused (`Unsupported::Geometry`, "a translucent layer of odd size"). Only meaningful with `translucent_approximate` off;
    /// [`GpuCaps::A0`] leaves it off.
    pub translucent_odd: bool,
    /// The vector width (8 or 16 floats) of this FFmpeg's `gblur` kernel as the blur probe found it, or `0` when it is not known which
    /// (a pinned blur, a unit test): the plan then judges a blur's planes against both. `gblur`'s AVX kernels are not the recursion on
    /// some plane shapes ([`crate::gblur::KernelHazard`]), which depend on this width and on the graph's slice threads.
    pub blur_lanes: u8,
    /// The slice threads the filter graph runs on, when a compositor pinned them (`Compositor::with_graph_threads`): the plan judges a
    /// blur at exactly that count. `None`: the plan's own ([`RenderPlan::graph_thread_range`]).
    pub graph_threads: Option<u32>,
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
        placement: false,
        placed_chroma: false,
        file_range: false,
        behaviors: false,
        keyed_color: false,
        mask: false,
        text: false,
        svg: false,
        reframe: false,
        hdr: false,
        effects: EffectKinds::NONE,
        still_motion: StillKinds::NONE,
        grade: GradeKinds::NONE,
        exact_scaler: false,
        scalers: ScalerKinds::NONE,
        blur_fill: false,
        matte_effects: false,
        matrix_convert: false,
        matrix_convert_scaled: false,
        scaler_approximate: false,
        blend: BlendKinds::NONE,
        blend_extras: BlendExtras::NONE,
        translucent_approximate: false,
        translucent_odd: false,
        blur_lanes: 0,
        graph_threads: None,
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
    /// The picture is a `yuv420p` flagged full range: the export's round trip runs on every frame of the clip in the
    /// picture's own range, which the compositor's limited-range tables are not.
    FullRange,
    /// The clip carries an `invert` or a `vignette`: both take RGB, so the `scale` ahead of them converts the picture to RGB and
    /// FFmpeg runs the effect there, on every frame of the clip.
    RgbEffect,
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
            Self::FullRange => write!(
                f,
                "from a `yuv420p` picture flagged full range, which FFmpeg's round trip converts in that range on every frame of the clip (the compositor's tables are limited-range's: Y 2, UV 1 levels off)"
            ),
            Self::RgbEffect => write!(
                f,
                "under an invert or a vignette, which take RGB: the fit `scale` converts the picture to RGB ahead of them and FFmpeg runs the effect there (invert 3 levels off, vignette 47)"
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

/// Why a floating animated SVG title is not drawn ([`Unsupported::SvgOverlay`]). `Display` finishes the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgWhy {
    /// This FFmpeg's `overlay` of a picture is not measured to be the compositor's model ([`GpuCaps::svg`]).
    Unmeasured,
    /// No clip is under it: FFmpeg's black base is not a YUV composite the title is blended into.
    NoLayer,
    /// The composite is not BT.601: FFmpeg 9 converts the picture to `yuva420p` with the composite's matrix, which no case holds the
    /// compositor to (it asks FFmpeg for the planes of the untagged conversion).
    Matrix(YuvMatrix),
}

impl fmt::Display for SvgWhy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unmeasured => f.write_str("(this FFmpeg's overlay of a picture is not measured to be the compositor's blend)"),
            Self::NoLayer => f.write_str("over a frame with no clip (FFmpeg's black base is not a composite the picture is blended into)"),
            Self::Matrix(m) => write!(
                f,
                "over a {} composite (FFmpeg converts the picture to YCbCr with the composite's matrix, which is not held to the compositor's)",
                match m {
                    YuvMatrix::Bt601 => "BT.601",
                    YuvMatrix::Bt709 => "BT.709",
                    YuvMatrix::Bt2020 => "BT.2020",
                }
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
    /// The delivery fits footage over a blurred copy of itself ([`crate::model::Fit::Blur`]) and the compositor
    /// does not draw the fill, or has no exact scaler under it (`GpuCaps::blur_fill`, `GpuCaps::exact_scaler`):
    /// FFmpeg's `boxblur` on a thumbnail scaled back up.
    BlurFill,
    /// A blurred fill the compositor draws in general but not for this layer or at this size (the detail says why).
    BlurFillWith(LayerRef, &'static str),
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
    /// padded frame, black bars included. A compositor that lays that frame out whole
    /// ([`GpuCaps::matte_effects`]) draws a blur, a sharpen, a vignette and a grade on it; otherwise, and always for a
    /// chroma key (an alpha plane cut from that frame), the compositor draws the picture and the bars
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
    /// conversion each way. The compositor draws the mode's chain (`GpuCaps::blend`) on a layer at its own size, opaque
    /// and upright, over an even frame; the string is what keeps this one from being drawn.
    BlendMode(LayerRef, BlendMode, &'static str),
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
    /// A still image whose picture moves, drawn by FFmpeg in one `perspective` of a copy scaled once
    /// ([`crate::still_motion`]), which a compositor draws only where it measured this FFmpeg's chain for the picture's pixel
    /// format and the plan holds the route at the size it renders (the detail says which is missing).
    StillMotion(LayerRef, String),
    /// A keyframed zoom in an export frame, which FFmpeg draws as the last stage of the chain.
    KeyedZoom(LayerRef),
    /// A moving zoom or position: FFmpeg places the picture on the canvas with a `perspective` at
    /// sub-pixel positions (in the export and the still alike), which a compositor draws only where it sets
    /// `GpuCaps::placement` (`pad`, `perspective` and `crop`, reproduced to the sample, and **measured** against the
    /// FFmpeg in use: the flag is not set where that measurement found a difference or could not tell).
    KeyedPlacement(LayerRef),
    /// A rotation the compositor does not draw (the detail says which): FFmpeg's `rotate` samples a turned picture at fixed-point
    /// positions with an apron of its own, and only a quarter, half or three quarters of a turn (where its integer sine comes out exact,
    /// [`crate::quarter_turn`]) lands on whole samples; any other angle, a rotation a key or a behaviour moves, and a constant opacity below 1 under a quarter or three quarters of a turn
    /// (FFmpeg converts to 4:2:0 after the turn, so other chroma pairs are averaged), is FFmpeg's.
    Rotation(LayerRef, &'static str),
    /// A moving zoom or position on a picture that is not 4:2:0 or gray (the detail names its pixel format, or says it was
    /// never recorded), or that may be full range where the composite's range is negotiated (the detail says `full-range`):
    /// the placement chain converts it to `yuva420p` itself, in the graph, where the compositor's decode converted it to
    /// 8-bit limited-range 4:2:0 ahead of it, and swscale's two paths agree on one FFmpeg and not on the other (the chroma
    /// of a 4:4:4 picture 24 levels off on 4.4.2, a 4:2:2 one 16, exact on 9.0.2; a full-range picture 3 to 5 levels off in
    /// a still on 9.0.2, exact on 4.4.2).
    PlacedFormat(LayerRef, String),
    /// A clip with live behaviours (sway, breathe, spin, drift, shake): the pose each frame is a formula of
    /// the clip's clock added to its keys, which FFmpeg evaluates in the graph. A breath, a drift and a shake are the
    /// pose the placement draws (`GpuCaps::behaviors`, measured with it); a sway and a spin turn the picture, which
    /// no compositor draws to the sample, so they are refused whatever the caps.
    Behavior(LayerRef),
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
    /// A live text overlay cannot be taken apart into the glyph bitmaps `drawtext` blends in turn, which is
    /// all a compositor can reproduce of it exactly (their sum is not their blends: up to 33 levels of chroma at
    /// the pixels two share): what the plan can tell ([`crate::text_sequence_limit`], the reason): a tab,
    /// more than [`crate::MAX_SEQUENCE_CHARS`] characters, a text too wide to measure. What only the render
    /// can see (a glyph that does not shape one to a unit, a face whose bitmaps overhang) is its refusal.
    TextSequence(String),
    /// An animated SVG title clip (a generated clip on a track) is on screen. It is no overlay but a clip chain: the pictures resvg
    /// draws are fitted, faded and transitioned on an alpha plane and laid over the tracks under it, which no compositor pass does.
    Svg,
    /// A floating animated SVG title is on screen that the compositor does not draw (the detail says why): resvg draws its picture,
    /// FFmpeg converts it and `overlay`s it, and the compositor draws it where it measured that blend ([`GpuCaps::svg`]).
    SvgOverlay(SvgWhy),
    Scaler(String),
    /// The layer's colour matrix is unknown while the composite takes its matrix from the tags.
    MatrixUnknown(usize),
    MixedMatrices,
    /// A layer whose matrix is not the composite's (FFmpeg 9 converts it into the bottom layer's) that the compositor
    /// does not convert in this chain (the detail says why), or has no measured conversion for (`GpuCaps::matrix_convert`).
    MixedMatricesWith(LayerRef, &'static str),
    /// A layer a scale resizes, where the compositor's scaler is not known to be swscale's to the level
    /// ([`GpuCaps::scaler_approximate`]).
    ScalerApproximate(LayerRef),
    /// The composite colour policy could not be measured and the stack is not BT.601.
    UnmeasuredPolicy(YuvMatrix),
    RgbInStack(YuvMatrix),
    /// Colour correction in a stack with a full-range picture (layer index).
    GradedInFullRangeStack(usize),
    /// A shrink steeper than the scaler comparison covers for the delivery's scaler ([`crate::max_shrink_for`]: [`crate::MAX_SHRINK`], less for
    /// the two names with wider windows): `(layer, from, to, the limit that applied)`.
    Shrink(usize, (u32, u32), (u32, u32), u32),
    /// A resize of a picture FFmpeg scales in its own format: `(layer, from, to, pix_fmt)`.
    FormatResize(usize, (u32, u32), (u32, u32), String),
    /// The geometry of this layer at this size cannot be placed (`layer`, why).
    Geometry(usize, String),
    /// A layer below full opacity takes the RGB round trip, and this machine's FFmpeg is not measured to run it as the compositor does
    /// ([`GpuCaps::translucent_approximate`]).
    TranslucentApproximate(LayerRef),
    /// A full-range picture in a still, where the composite's colour is negotiated (FFmpeg 9): the still's graph keeps the picture's range
    /// through the overlay and converts it last, so its planes are full-range ones (up to 17 levels from the limited-range composite the
    /// compositor makes) and the PNG's RGB is 3 to 4 levels from the compositor's.
    FullRangeStill(LayerRef),
}

impl Unsupported {
    /// The layer (its index in the plan) this refusal names, if it names one: a refusal of the frame as a whole, of the delivery or of the
    /// stack has none.
    pub fn layer(&self) -> Option<usize> {
        match self {
            Self::NoPicture(l)
            | Self::BlurFillWith(l, _)
            | Self::Hdr(l)
            | Self::AlphaPicture(l, _)
            | Self::NotKnownOpaque(l, _)
            | Self::TranslucentMatrix(l, _)
            | Self::GradedFullRange(l, _)
            | Self::GradeSteep(l, _)
            | Self::Grade(l, _)
            | Self::GradeWith(l, _)
            | Self::Effects(l, _)
            | Self::Mask(l)
            | Self::MaskWith(l, _)
            | Self::Matte(l, _)
            | Self::Reframe(l)
            | Self::Fade(l, _, _)
            | Self::FadeOnTranslucent(l)
            | Self::Travel(l)
            | Self::DipToWhite(l, _)
            | Self::FullRangeNegotiated(l)
            | Self::FullRangeResized(l, _)
            | Self::SpeedRamp(l)
            | Self::Stabilized(l)
            | Self::BlendMode(l, _, _)
            | Self::Tail(l)
            | Self::KeyedOpacity(l)
            | Self::KeyedColor(l)
            | Self::KeyedMask(l)
            | Self::KeyedCrop(l)
            | Self::StillMotion(l, _)
            | Self::KeyedZoom(l)
            | Self::KeyedPlacement(l)
            | Self::Rotation(l, _)
            | Self::PlacedFormat(l, _)
            | Self::Behavior(l)
            | Self::ZoomBehind(l, _)
            | Self::FoldedFit(l, _)
            | Self::MixedMatricesWith(l, _)
            | Self::ScalerApproximate(l)
            | Self::TranslucentApproximate(l)
            | Self::FullRangeStill(l) => Some(l.index),
            Self::Shrink(n, ..)
            | Self::FormatResize(n, ..)
            | Self::Geometry(n, _)
            | Self::MatrixUnknown(n)
            | Self::GradedInFullRangeStack(n) => Some(*n),
            Self::Motion
            | Self::Delivery(_)
            | Self::Gif
            | Self::BlurFill
            | Self::PickRate { .. }
            | Self::FullRangeCut
            | Self::Text
            | Self::TextWithoutFont
            | Self::TextSequence(_)
            | Self::Svg
            | Self::SvgOverlay(_)
            | Self::Scaler(_)
            | Self::MixedMatrices
            | Self::UnmeasuredPolicy(_)
            | Self::RgbInStack(_) => None,
        }
    }

    /// Whether this refusal is about the frame as a whole **and no measurement lifts it**: a scaler the compositor does not implement, an
    /// animated title, a GIF or a delivery format it does not draw, a pick rate, a cut FFmpeg 9 flags full range. Such a frame is FFmpeg's whatever
    /// else is measured, so a host does not measure or make anything for its layers. (A refusal of the frame as a whole that a probe's answer
    /// moves — the text layout, the colour matrix, the motion pass — is not one: it is `false` here, as are all the refusals that name a layer,
    /// [`Unsupported::layer`].) Every variant is classified, so a new one cannot be left out of either.
    pub fn frame_wide(&self) -> bool {
        match self {
            Self::Svg | Self::Gif | Self::Delivery(_) | Self::PickRate { .. } | Self::FullRangeCut => true,
            // A scaler the compositor has no tables for. (One it has — `bilinear`, `lanczos`, `neighbor`, `spline` — is drawn where the
            // probe measured them to be this FFmpeg's: a measurement lifts it, so it is not settled.)
            Self::Scaler(name) => ScalerKinds::of_name(name).is_none(),
            // Of the frame as a whole, and measured or negotiated: not settled.
            Self::Motion
            | Self::Text
            | Self::TextWithoutFont
            | Self::TextSequence(_)
            | Self::SvgOverlay(_)
            | Self::MixedMatrices
            | Self::UnmeasuredPolicy(_)
            | Self::RgbInStack(_) => false,
            // A layer's (or the stack's about a layer).
            Self::NoPicture(_)
            | Self::BlurFill
            | Self::BlurFillWith(..)
            | Self::Hdr(_)
            | Self::AlphaPicture(..)
            | Self::NotKnownOpaque(..)
            | Self::TranslucentMatrix(..)
            | Self::GradedFullRange(..)
            | Self::GradeSteep(..)
            | Self::Grade(..)
            | Self::GradeWith(..)
            | Self::Effects(..)
            | Self::Mask(_)
            | Self::MaskWith(..)
            | Self::Matte(..)
            | Self::Reframe(_)
            | Self::Fade(..)
            | Self::FadeOnTranslucent(_)
            | Self::Travel(_)
            | Self::DipToWhite(..)
            | Self::FullRangeNegotiated(_)
            | Self::FullRangeResized(..)
            | Self::SpeedRamp(_)
            | Self::Stabilized(_)
            | Self::BlendMode(..)
            | Self::Tail(_)
            | Self::KeyedOpacity(_)
            | Self::KeyedColor(_)
            | Self::KeyedMask(_)
            | Self::KeyedCrop(_)
            | Self::StillMotion(..)
            | Self::KeyedZoom(_)
            | Self::KeyedPlacement(_)
            | Self::Rotation(..)
            | Self::PlacedFormat(..)
            | Self::Behavior(_)
            | Self::ZoomBehind(..)
            | Self::FoldedFit(..)
            | Self::MatrixUnknown(_)
            | Self::MixedMatricesWith(..)
            | Self::ScalerApproximate(_)
            | Self::TranslucentApproximate(_)
            | Self::FullRangeStill(_)
            | Self::GradedInFullRangeStack(_)
            | Self::Shrink(..)
            | Self::FormatResize(..)
            | Self::Geometry(..) => false,
        }
    }

    /// What kind of refusal this is, without the layer names and numbers its message carries: a
    /// stable key to count refusals by (`mixed_matrices`, `text_sequence`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NoPicture(_) => "no_picture",
            Self::Motion => "motion",
            Self::Delivery(_) => "delivery",
            Self::Gif => "gif",
            Self::BlurFill => "blur_fill",
            Self::BlurFillWith(..) => "blur_fill_with",
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
            Self::StillMotion(..) => "still_motion",
            Self::KeyedZoom(_) => "keyed_zoom",
            Self::KeyedPlacement(_) => "keyed_placement",
            Self::Rotation(..) => "rotation",
            Self::PlacedFormat(..) => "placed_format",
            Self::Behavior(_) => "behavior",
            Self::ZoomBehind(..) => "zoom_behind",
            Self::FoldedFit(..) => "folded_fit",
            Self::Text => "text",
            Self::TextWithoutFont => "text_without_font",
            Self::TextSequence(_) => "text_sequence",
            Self::Svg => "svg",
            Self::SvgOverlay(_) => "svg_overlay",
            Self::Scaler(_) => "scaler",
            Self::MatrixUnknown(_) => "matrix_unknown",
            Self::MixedMatrices => "mixed_matrices",
            Self::MixedMatricesWith(..) => "mixed_matrices_with",
            Self::ScalerApproximate(_) => "scaler_approximate",
            Self::UnmeasuredPolicy(_) => "unmeasured_policy",
            Self::RgbInStack(_) => "rgb_in_stack",
            Self::GradedInFullRangeStack(_) => "graded_in_full_range_stack",
            Self::Shrink(..) => "shrink",
            Self::FormatResize(..) => "format_resize",
            Self::Geometry(..) => "geometry",
            Self::TranslucentApproximate(_) => "translucent_approximate",
            Self::FullRangeStill(_) => "full_range_still",
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
            Self::BlurFill => f.write_str("the delivery fills the frame with a blurred copy of the footage, which this compositor does not draw (or draws without an exact scaler under it)"),
            Self::BlurFillWith(l, why) => write!(f, "{l}: the blurred fill behind a picture that leaves bars is not drawn for {why}"),
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
            Self::BlendMode(l, mode, why) => write!(
                f,
                "{l}: the {} blend mode (FFmpeg mixes it with the composite by `blend` in RGB): {why}",
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
            Self::StillMotion(l, why) => write!(
                f,
                "{l}: a still whose picture moves (FFmpeg draws its fit, zoom, position and crop in one perspective of a scaled copy): {why}"
            ),
            Self::KeyedZoom(l) => write!(
                f,
                "{l}: keyframed zoom (FFmpeg draws it as the last stage of the clip's chain, after the effects, mask and rotation)"
            ),
            Self::KeyedPlacement(l) => write!(
                f,
                "{l}: a moving zoom or position (a perspective placing the picture at sub-pixel positions, which this compositor draws only where this FFmpeg's pad, perspective and crop were measured to place a picture as its model does)"
            ),
            Self::PlacedFormat(l, fmt) => write!(
                f,
                "{l}: a moving zoom or position on a {fmt} picture (the placement chain converts it to yuva420p itself, which is not the decode's conversion on every FFmpeg)"
            ),
            Self::Rotation(l, what) => write!(f, "{l}: {what}"),
            Self::Behavior(l) => write!(
                f,
                "{l}: live behaviours (a sway, breath, spin, drift or shake FFmpeg evaluates in the graph on top of the clip's keys; drawn only where the compositor says it draws them, and never one that turns the picture)"
            ),
            Self::ZoomBehind(l, what) => write!(
                f,
                "{l}: a moving zoom or position with {what} ahead of it (FFmpeg applies it at the fit size and places the result)"
            ),
            Self::FoldedFit(l, what) => write!(
                f,
                "{l}: {what} (FFmpeg takes the picture from the source in one resample, to the frame times the zoom; the compositor fits it to the frame and scales that)"
            ),
            Self::Text => f.write_str("a text overlay is live"),
            Self::TextWithoutFont => f.write_str("a text overlay names no font file to draw with"),
            Self::TextSequence(why) => write!(
                f,
                "a text overlay the compositor cannot take apart into the glyph bitmaps drawtext blends in turn: {why}"
            ),
            Self::Svg => f.write_str(
                "an animated SVG title clip is on screen (a clip chain of fit, fades and transitions on an alpha picture, not an overlay)",
            ),
            Self::SvgOverlay(why) => write!(f, "a floating animated SVG title is on screen {why}"),
            Self::Scaler(s) => match ScalerKinds::of_name(s) {
                Some(_) => write!(f, "scaler '{s}' (its tables are not measured to be this FFmpeg's)"),
                None => write!(f, "scaler '{s}'"),
            },
            Self::MatrixUnknown(n) => write!(
                f,
                "layer {n}: its colour matrix is unknown, and FFmpeg's composite takes its matrix from the layers' tags"
            ),
            Self::MixedMatrices => f.write_str(
                "layers tagged with different YCbCr matrices: FFmpeg converts them into the bottom layer's, which is not reproduced",
            ),
            Self::ScalerApproximate(l) => write!(
                f,
                "{l}: resized by a scale, and this compositor's scaler is not measured to be swscale's (it draws the picture a level off)"
            ),
            Self::MixedMatricesWith(l, why) => write!(
                f,
                "{l}: a colour matrix other than the composite's, which FFmpeg converts into the bottom layer's and the compositor does not for {why}"
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
            Self::Shrink(n, from, to, limit) => write!(
                f,
                "layer {n}: shrinks a {}x{} picture to {}x{}, steeper than the {limit}:1 the scaler comparison covers for this delivery's scaler",
                from.0, from.1, to.0, to.1
            ),
            Self::FormatResize(n, from, to, pf) => write!(
                f,
                "layer {n}: scales a {}x{} picture to {}x{}, and its format ({pf}) is not planar 8-bit 4:2:0, the only format the exact scaler is proven for (FFmpeg scales the others in their own format)",
                from.0, from.1, to.0, to.1
            ),
            Self::Geometry(n, why) => write!(f, "layer {n}: {why}"),
            Self::FullRangeStill(l) => write!(
                f,
                "{l}: a full-range picture in a still, where this FFmpeg keeps the picture's range through the graph and converts it last (its planes are full-range ones; the compositor's composite is limited-range)"
            ),
            Self::TranslucentApproximate(l) => write!(
                f,
                "{l}: opacity below 1 (FFmpeg takes the layer through RGB and back, on swscale kernels this machine's build has not been measured to run as the compositor's round trip does)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_plan::MAX_SHRINK;

    fn layer(index: usize, name: &str) -> LayerRef {
        LayerRef {
            index,
            name: name.into(),
        }
    }

    #[test]
    fn a_still_kind_holds_each_chain_by_itself_and_the_kind_constants_are_the_base_chains() {
        let kinds = [StillKinds::YUVJ420P, StillKinds::RGB24, StillKinds::YUV420P];
        for (k, kind) in kinds.into_iter().enumerate() {
            // The constant is the kind's two base chains (the move inside the frame and the engine's fit, the default scaler).
            for chain in StillChain::ALL {
                assert_eq!(kind.contains(kind.chain(chain)), chain.is_base(), "{k} {chain:?}");
                assert!(kind.all_chains().contains(kind.chain(chain)));
                assert!(StillKinds::ALL.contains_chain(kind, chain));
                assert!(!StillKinds::NONE.contains_chain(kind, chain));
                // ... and one kind's chain is not another's.
                for other in kinds.into_iter().filter(|o| *o != kind) {
                    assert!(!other.all_chains().contains(kind.chain(chain)));
                    assert!(!StillKinds::ALL.minus_chain(kind, chain).contains_chain(kind, chain));
                    assert!(StillKinds::ALL.minus_chain(kind, chain).contains_chain(other, chain));
                }
            }
            assert_eq!(StillChain::ALL.iter().filter(|c| c.is_base()).count(), 2);
        }
        // Every chain has a place of its own, and ALL lists them in order.
        for (i, chain) in StillChain::ALL.into_iter().enumerate() {
            assert_eq!(chain.index(), i);
        }
        // A text no toy cut takes is no chain of any kind: the caps never hold it.
        let cropped = StillChain::edge(FitShape::Cropped, false);
        assert!(!cropped.measurable() && !cropped.is_base() && cropped.index() == StillChain::COUNT);
        assert!(StillChain::ALL.iter().all(|c| c.measurable()));
        for kind in kinds {
            assert_eq!(kind.chain(cropped), StillKinds::NONE);
            assert!(!StillKinds::ALL.contains_chain(kind, cropped));
            assert!(!StillKinds::ALL.contains_chain(kind, StillChain::edge(FitShape::Cropped, true)));
        }
        // A set that is not one kind's names no chain.
        assert_eq!(StillKinds::ALL.chain(StillChain::inside(false)), StillKinds::NONE);
        assert_eq!(StillKinds::NONE.chain(StillChain::inside(false)), StillKinds::NONE);
        assert!(!StillKinds::ALL.contains_chain(StillKinds::ALL, StillChain::inside(false)));
        assert_eq!(StillKinds::of_pix_fmt("RGB24"), Some(StillKinds::RGB24));
        assert_eq!(StillKinds::of_pix_fmt("nv12"), None);
    }

    #[test]
    fn a_still_kind_holds_the_move_inside_the_frame_under_each_named_scaler_by_itself() {
        let kinds = [StillKinds::YUVJ420P, StillKinds::RGB24, StillKinds::YUV420P];
        let names = [
            ScalerKinds::BILINEAR,
            ScalerKinds::LANCZOS,
            ScalerKinds::NEIGHBOR,
            ScalerKinds::SPLINE,
        ];
        let mut bits = Vec::new();
        for kind in kinds {
            for name in names {
                let route = kind.named_route(name);
                assert_ne!(route, StillKinds::NONE, "{kind:?} {name:?}");
                // One bit each, apart from every chain and from every other route.
                assert!(!bits.contains(&route), "{kind:?} {name:?} shares a bit");
                bits.push(route);
                assert!(!kind.all_chains().contains(route));
                assert!(StillKinds::ALL.contains_named_route(kind, name));
                assert!(!StillKinds::NONE.contains_named_route(kind, name));
                // The base chains of the kind are not the route, nor the route the base chains.
                assert!(!kind.contains_named_route(kind, name));
                assert!(!route.contains(kind));
                assert!(!kind.union(kind.all_chains()).contains_named_route(kind, name));
                // Taking it out leaves every other pair and every chain.
                let without = StillKinds::ALL.minus_named_route(kind, name);
                assert!(!without.contains_named_route(kind, name));
                assert!(without.contains(kind.all_chains()));
                for other in names.into_iter().filter(|n| *n != name) {
                    assert!(without.contains_named_route(kind, other));
                }
                for other in kinds.into_iter().filter(|k| *k != kind) {
                    assert!(without.contains_named_route(other, name));
                }
            }
        }
        assert_eq!(bits.len(), 12);
        // A set that is not one kind's, and a scaler that is not one name of the four, name no route: the caps never hold it.
        for name in [
            ScalerKinds::NONE,
            ScalerKinds::ALL,
            ScalerKinds::LANCZOS.union(ScalerKinds::SPLINE),
        ] {
            assert_eq!(StillKinds::RGB24.named_route(name), StillKinds::NONE, "{name:?}");
            assert!(!StillKinds::ALL.contains_named_route(StillKinds::RGB24, name));
        }
        assert_eq!(StillKinds::ALL.named_route(ScalerKinds::LANCZOS), StillKinds::NONE);
        assert_eq!(StillKinds::NONE.named_route(ScalerKinds::LANCZOS), StillKinds::NONE);
        assert!(!StillKinds::ALL.contains_named_route(StillKinds::ALL, ScalerKinds::LANCZOS));
        // The kind constants are still the base chains alone.
        for kind in kinds {
            assert_eq!(kind.all_chains().union(StillKinds::NONE), kind.all_chains());
            assert!(!kind.all_chains().contains(kind.named_route(ScalerKinds::LANCZOS)));
        }
    }

    #[test]
    fn a_refusal_names_the_layer_it_is_about_and_a_refusal_of_the_frame_names_none() {
        let l = layer(3, "x");
        for named in [
            Unsupported::Hdr(l.clone()),
            Unsupported::StillMotion(l.clone(), String::new()),
            Unsupported::ScalerApproximate(l),
            Unsupported::Shrink(3, (2, 2), (1, 1), MAX_SHRINK),
            Unsupported::FormatResize(3, (2, 2), (1, 1), String::new()),
            Unsupported::Geometry(3, String::new()),
            // (a stack's refusals that name the layer they are about)
            Unsupported::MatrixUnknown(3),
            Unsupported::GradedInFullRangeStack(3),
        ] {
            assert_eq!(named.layer(), Some(3), "{named:?}");
        }
        // A refusal that is frame-wide in this sense names no layer.
        for settled in [
            Unsupported::Scaler("Lanczos".into()),
            Unsupported::Svg,
            Unsupported::Gif,
            Unsupported::Delivery("gif".into()),
            Unsupported::PickRate {
                canvas: crate::clip_timing::Rational { num: 30, den: 1 },
                clips: crate::clip_timing::Rational { num: 25, den: 1 },
            },
            Unsupported::FullRangeCut,
        ] {
            assert!(settled.frame_wide() && settled.layer().is_none(), "{settled:?}");
        }
        // ... and the frame-level ones a probe's answer moves are not, nor is any refusal of a layer.
        for open in [
            Unsupported::Motion,
            Unsupported::Text,
            Unsupported::TextWithoutFont,
            Unsupported::SvgOverlay(SvgWhy::Unmeasured),
            Unsupported::MixedMatrices,
            Unsupported::RgbInStack(YuvMatrix::Bt709),
            Unsupported::UnmeasuredPolicy(YuvMatrix::Bt709),
            Unsupported::StillMotion(layer(3, "x"), String::new()),
            Unsupported::MatrixUnknown(3),
        ] {
            assert!(!open.frame_wide(), "{open:?}");
        }
        for frame_wide in [
            Unsupported::Motion,
            Unsupported::Gif,
            Unsupported::Svg,
            Unsupported::Text,
            Unsupported::FullRangeCut,
            Unsupported::MixedMatrices,
            Unsupported::Delivery("gif".into()),
            Unsupported::Scaler("lanczos".into()),
            Unsupported::RgbInStack(YuvMatrix::Bt709),
            Unsupported::UnmeasuredPolicy(YuvMatrix::Bt709),
        ] {
            assert_eq!(frame_wide.layer(), None, "{frame_wide:?}");
        }
    }

    #[test]
    fn a_kind_names_the_refusal_and_not_the_layer_or_the_numbers() {
        assert_eq!(
            Unsupported::Mask(layer(0, "a.mp4")).kind(),
            Unsupported::Mask(layer(3, "b.mov")).kind()
        );
        assert_eq!(
            Unsupported::TextSequence("a tab".into()).kind(),
            Unsupported::TextSequence("200 characters".into()).kind()
        );
        assert_eq!(Unsupported::Shrink(0, (4000, 2000), (50, 20), MAX_SHRINK).kind(), "shrink");
        assert_ne!(Unsupported::Text.kind(), Unsupported::TextSequence(String::new()).kind());
        assert_eq!(Unsupported::MixedMatrices.kind(), "mixed_matrices");
        assert_eq!(
            Unsupported::MixedMatricesWith(layer(0, ""), "an opacity below 1").kind(),
            "mixed_matrices_with"
        );
        assert_eq!(Unsupported::SpeedRamp(layer(1, "a.mp4")).kind(), "speed_ramp");
        assert_eq!(
            Unsupported::BlendMode(layer(0, "a.mp4"), BlendMode::Multiply, "a").kind(),
            Unsupported::BlendMode(layer(2, "b.mov"), BlendMode::Screen, "b").kind(),
            "the mode is not part of the key"
        );
        assert!(
            Unsupported::BlendMode(layer(1, "a.mp4"), BlendMode::HardLight, "a rotated layer")
                .to_string()
                .starts_with("layer 1 (a.mp4): the hard light blend mode")
        );
        assert!(Unsupported::SpeedRamp(layer(1, "a.mp4"))
            .to_string()
            .starts_with("layer 1 (a.mp4): a speed ramp"));
        assert_eq!(
            Unsupported::TranslucentApproximate(layer(1, "a.mp4")).kind(),
            "translucent_approximate"
        );
        assert_eq!(Unsupported::FullRangeStill(layer(1, "a.mp4")).kind(), "full_range_still");
        assert_eq!(
            Unsupported::TranslucentApproximate(layer(0, "a.mp4")).kind(),
            Unsupported::TranslucentApproximate(layer(3, "b.mov")).kind(),
            "the layer is not part of the key"
        );
        assert!(Unsupported::TranslucentApproximate(layer(1, "a.mp4"))
            .to_string()
            .starts_with("layer 1 (a.mp4): "));
        assert!(Unsupported::FullRangeStill(layer(1, "a.mp4"))
            .to_string()
            .starts_with("layer 1 (a.mp4): a full-range picture in a still"));
        assert_eq!(
            Unsupported::DipToWhite(layer(0, "a.mp4"), DipWhy::Resized).kind(),
            Unsupported::DipToWhite(layer(2, "b.mov"), DipWhy::Format).kind(),
            "the why of a dip is not part of the key"
        );
        let kinds = [
            Unsupported::Motion.kind(),
            Unsupported::Gif.kind(),
            Unsupported::BlurFill.kind(),
            Unsupported::BlurFillWith(layer(0, ""), "an HDR picture").kind(),
            Unsupported::Text.kind(),
            Unsupported::TextWithoutFont.kind(),
            Unsupported::TextSequence(String::new()).kind(),
            Unsupported::Svg.kind(),
            Unsupported::SvgOverlay(SvgWhy::Unmeasured).kind(),
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
            Unsupported::BlendMode(layer(0, ""), BlendMode::Screen, "a rotated layer").kind(),
            Unsupported::TranslucentApproximate(layer(0, "")).kind(),
            Unsupported::FullRangeStill(layer(0, "")).kind(),
        ];
        for kind in kinds {
            assert!(
                kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{kind} is a key, not a sentence"
            );
        }
    }

    #[test]
    fn a_pixel_format_is_a_still_kind_only_where_a_chain_can_be_measured_for_it() {
        assert_eq!(StillKinds::of_pix_fmt("yuvj420p"), Some(StillKinds::YUVJ420P));
        assert_eq!(StillKinds::of_pix_fmt("RGB24"), Some(StillKinds::RGB24));
        assert_eq!(StillKinds::of_pix_fmt("yuv420p"), Some(StillKinds::YUV420P));
        for other in [
            "yuvj422p",
            "yuvj444p",
            "rgba",
            "gray",
            "gray16le",
            "pal8",
            "bgr24",
            "yuv420p10le",
            "",
        ] {
            assert_eq!(StillKinds::of_pix_fmt(other), None, "{other}");
        }
        let both = StillKinds::YUVJ420P.union(StillKinds::RGB24);
        assert!(both.contains(StillKinds::RGB24) && both.contains(StillKinds::YUVJ420P) && !both.contains(StillKinds::YUV420P));
        assert!(StillKinds::ALL.contains(both) && !StillKinds::NONE.contains(StillKinds::RGB24));
    }
}
