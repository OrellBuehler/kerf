//! How a clip's picture and sound are treated: transform, colour, transition, mask, blend and effects.

use super::*;

/// Per-clip geometric transform applied when compositing at export. A default
/// transform is the identity (full-frame, centered, opaque, uncropped).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Uniform scale multiplier applied after the clip is fit to the frame
    /// (1.0 = fit). Values < 1.0 shrink the picture for picture-in-picture.
    #[serde(default = "one")]
    pub scale: f64,
    /// Horizontal offset as a fraction of the frame width (0.0 = centered).
    #[serde(default)]
    pub pos_x: f64,
    /// Vertical offset as a fraction of the frame height (0.0 = centered).
    #[serde(default)]
    pub pos_y: f64,
    /// Clockwise rotation in degrees.
    #[serde(default)]
    pub rotation: f64,
    /// Opacity in 0.0–1.0 (1.0 = fully opaque).
    #[serde(default = "one")]
    pub opacity: f64,
    /// Fraction of the source cropped from each edge (0.0 = no crop).
    #[serde(default)]
    pub crop_left: f64,
    #[serde(default)]
    pub crop_right: f64,
    #[serde(default)]
    pub crop_top: f64,
    #[serde(default)]
    pub crop_bottom: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            crop_left: 0.0,
            crop_right: 0.0,
            crop_top: 0.0,
            crop_bottom: 0.0,
        }
    }
}

impl Transform {
    /// True when the transform leaves the picture untouched (full-frame fit).
    pub fn is_identity(&self) -> bool {
        *self == Transform::default()
    }

    /// True when compositing this clip needs an alpha channel (rotation leaves
    /// transparent corners; opacity blends; both require alpha).
    pub fn needs_alpha(&self) -> bool {
        self.opacity < 1.0 || self.rotation != 0.0
    }

    /// True when any edge crop is requested.
    pub fn has_crop(&self) -> bool {
        self.crop_left > 0.0 || self.crop_right > 0.0 || self.crop_top > 0.0 || self.crop_bottom > 0.0
    }
}

/// Some numbers of a [`Transform`], as a partial edit names them: `None` leaves a number alone.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransformPatch {
    pub scale: Option<f64>,
    pub pos_x: Option<f64>,
    pub pos_y: Option<f64>,
    pub rotation: Option<f64>,
    pub opacity: Option<f64>,
    pub crop_left: Option<f64>,
    pub crop_right: Option<f64>,
    pub crop_top: Option<f64>,
    pub crop_bottom: Option<f64>,
}

impl TransformPatch {
    /// The animatable numbers it names, with their values.
    pub fn animatable(&self) -> Vec<(Property, f64)> {
        [
            (Property::Scale, self.scale),
            (Property::PosX, self.pos_x),
            (Property::PosY, self.pos_y),
            (Property::Rotation, self.rotation),
            (Property::Opacity, self.opacity),
        ]
        .into_iter()
        .filter_map(|(p, v)| v.map(|v| (p, v)))
        .collect()
    }

    /// Write the numbers it names into `t`.
    pub fn apply(&self, t: &mut Transform) {
        let fields = [
            (&mut t.scale, self.scale),
            (&mut t.pos_x, self.pos_x),
            (&mut t.pos_y, self.pos_y),
            (&mut t.rotation, self.rotation),
            (&mut t.opacity, self.opacity),
            (&mut t.crop_left, self.crop_left),
            (&mut t.crop_right, self.crop_right),
            (&mut t.crop_top, self.crop_top),
            (&mut t.crop_bottom, self.crop_bottom),
        ];
        for (field, value) in fields {
            if let Some(v) = value {
                *field = v;
            }
        }
    }
}

/// Per-clip color correction applied at export via the `eq` filter. A default
/// is the identity (no change).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Color {
    /// Additive brightness in -1.0–1.0 (0.0 = unchanged).
    #[serde(default)]
    pub brightness: f64,
    /// Contrast multiplier (1.0 = unchanged).
    #[serde(default = "one")]
    pub contrast: f64,
    /// Saturation multiplier (1.0 = unchanged).
    #[serde(default = "one")]
    pub saturation: f64,
    /// Gamma (1.0 = unchanged).
    #[serde(default = "one")]
    pub gamma: f64,
    /// Warm/cool shift in -1.0–1.0 (0.0 = unchanged): positive warms the
    /// picture (lifts red, lowers blue), negative cools it. Rendered as
    /// opposing per-channel gammas — what makes one-click warm / cool looks
    /// possible, since plain saturation/gamma can't tint.
    #[serde(default)]
    pub temperature: f64,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
            temperature: 0.0,
        }
    }
}

impl Color {
    /// True when the color correction leaves the picture untouched.
    pub fn is_identity(&self) -> bool {
        *self == Color::default()
    }

    /// The `(gamma_r, gamma_b)` the `eq` filter is given for the warm / cool
    /// shift, or `None` at 0 (so a temperature-free clip's graph is unchanged).
    ///
    /// `eq` has no white-balance knob, but opposing per-channel gammas warm / cool
    /// convincingly: ±1.0 maps to a ±30% split. Kept here, not in the graph
    /// builder, because every renderer of a [`Color`] — the FFmpeg `eq` filter
    /// and the GPU compositor's port of it — has to start from the same two
    /// numbers.
    pub fn temperature_gammas(&self) -> Option<(f64, f64)> {
        (self.temperature != 0.0).then(|| {
            let t = self.temperature.clamp(-1.0, 1.0);
            (1.0 + 0.3 * t, 1.0 - 0.3 * t)
        })
    }
}

/// How a clip blends with the preceding clip on its track.
///
/// Three families, and the family is what decides how the cut is rendered:
/// a **dip** takes both sides through a solid colour, a **dissolve** mixes them,
/// and a **motion** transition slides the incoming clip in over the outgoing one
/// (`Slide*`) or shoves the outgoing one out of frame with it (`Push*`). All of
/// them borrow the outgoing clip's unused source handle to keep it playing under
/// the transition, so a cut with no handle left degrades to a hard cut rather
/// than to a fade from black.
///
/// The direction in a motion transition names the direction of **travel**, the
/// way an editor says it: `SlideLeft` brings the new shot in from the right edge
/// and moves it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Dissolve: the incoming clip fades up over the outgoing clip's tail.
    Crossfade,
    /// Dip to black: the outgoing clip fades to black, the incoming up from it.
    DipToBlack,
    /// Dip to white — the same shape as [`Self::DipToBlack`], through white.
    /// Reads as a brighter, faster beat than black, which is why a montage of
    /// daylight footage usually wants it instead.
    DipToWhite,
    /// The incoming clip travels in from the right edge over the held outgoing one.
    SlideLeft,
    /// The incoming clip travels in from the left edge.
    SlideRight,
    /// The incoming clip travels up from the bottom edge.
    SlideUp,
    /// The incoming clip travels down from the top edge.
    SlideDown,
    /// Both clips travel left: the incoming pushes the outgoing out of frame.
    PushLeft,
    /// Both clips travel right.
    PushRight,
    /// Both clips travel up.
    PushUp,
    /// Both clips travel down.
    PushDown,
}

impl TransitionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "crossfade",
            TransitionKind::DipToBlack => "dip_to_black",
            TransitionKind::DipToWhite => "dip_to_white",
            TransitionKind::SlideLeft => "slide_left",
            TransitionKind::SlideRight => "slide_right",
            TransitionKind::SlideUp => "slide_up",
            TransitionKind::SlideDown => "slide_down",
            TransitionKind::PushLeft => "push_left",
            TransitionKind::PushRight => "push_right",
            TransitionKind::PushUp => "push_up",
            TransitionKind::PushDown => "push_down",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "crossfade" => Some(TransitionKind::Crossfade),
            "dip_to_black" | "diptoblack" => Some(TransitionKind::DipToBlack),
            "dip_to_white" | "diptowhite" => Some(TransitionKind::DipToWhite),
            "slide_left" => Some(TransitionKind::SlideLeft),
            "slide_right" => Some(TransitionKind::SlideRight),
            "slide_up" => Some(TransitionKind::SlideUp),
            "slide_down" => Some(TransitionKind::SlideDown),
            "push_left" => Some(TransitionKind::PushLeft),
            "push_right" => Some(TransitionKind::PushRight),
            "push_up" => Some(TransitionKind::PushUp),
            "push_down" => Some(TransitionKind::PushDown),
            _ => None,
        }
    }

    /// Every kind, in the order a picker should offer them.
    pub const ALL: [TransitionKind; 11] = [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ];

    /// Every kind's wire name, quoted and comma-joined — so an error message
    /// listing what was expected cannot drift from the enum.
    pub fn wire_names() -> String {
        Self::ALL
            .iter()
            .map(|k| format!("\"{}\"", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The solid colour this transition dips through, if it is a dip.
    pub fn dip_color(self) -> Option<&'static str> {
        match self {
            TransitionKind::DipToBlack => Some("black"),
            TransitionKind::DipToWhite => Some("white"),
            _ => None,
        }
    }

    /// Where the incoming clip starts, as an offset from its final position in
    /// frame widths and heights, for a motion transition. It travels from here
    /// to `(0, 0)` over the transition, so the vector points back along the
    /// direction of travel: a `SlideLeft` starts one full frame to the right.
    pub fn slide_from(self) -> Option<(f64, f64)> {
        match self {
            TransitionKind::SlideLeft | TransitionKind::PushLeft => Some((1.0, 0.0)),
            TransitionKind::SlideRight | TransitionKind::PushRight => Some((-1.0, 0.0)),
            TransitionKind::SlideUp | TransitionKind::PushUp => Some((0.0, 1.0)),
            TransitionKind::SlideDown | TransitionKind::PushDown => Some((0.0, -1.0)),
            _ => None,
        }
    }

    /// True when the outgoing clip is carried out of frame by the incoming one
    /// instead of being covered where it stands.
    pub fn pushes(self) -> bool {
        matches!(
            self,
            TransitionKind::PushLeft | TransitionKind::PushRight | TransitionKind::PushUp | TransitionKind::PushDown
        )
    }

    /// True when both sides play at once — a dissolve or any motion transition.
    /// Such a transition needs the outgoing clip's source handle; a dip does not,
    /// because the two halves happen either side of the cut.
    pub fn overlaps(self) -> bool {
        !matches!(self, TransitionKind::DipToBlack | TransitionKind::DipToWhite)
    }

    /// Human name, for a diff line or a picker label.
    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "Crossfade",
            TransitionKind::DipToBlack => "Dip to black",
            TransitionKind::DipToWhite => "Dip to white",
            TransitionKind::SlideLeft => "Slide left",
            TransitionKind::SlideRight => "Slide right",
            TransitionKind::SlideUp => "Slide up",
            TransitionKind::SlideDown => "Slide down",
            TransitionKind::PushLeft => "Push left",
            TransitionKind::PushRight => "Push right",
            TransitionKind::PushUp => "Push up",
            TransitionKind::PushDown => "Push down",
        }
    }
}

/// A transition blending the **start** of a clip with the clip that precedes it
/// on the same track. Realized at export.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Duration of the transition in seconds.
    pub duration: f64,
}

/// The outline of a [`Mask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaskShape {
    /// An axis-aligned rectangle — a sign, a screen, a lower band of the frame.
    #[default]
    Rect,
    /// An ellipse — a face, a spotlight.
    Ellipse,
}

impl MaskShape {
    pub fn as_str(self) -> &'static str {
        match self {
            MaskShape::Rect => "rect",
            MaskShape::Ellipse => "ellipse",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rect" | "rectangle" => Some(MaskShape::Rect),
            "ellipse" | "circle" | "oval" => Some(MaskShape::Ellipse),
            _ => None,
        }
    }
}

/// A shape cut out of a clip: inside the shape the clip is kept, outside it goes
/// transparent (or the other way round, `inverted`). Everything is a **fraction
/// of the clip's rendered frame**, so a mask does not have to be redone when the
/// delivery frame changes.
///
/// Deliberately one primitive rather than a masking *mode* per use. A mask makes
/// a clip see-through, and the timeline already stacks tracks — so blurring a
/// face is a copy of the shot on the track above, blurred, masked to the face;
/// a picture-in-picture vignette is a mask on the upper clip; a region grade is
/// a masked copy with its own colour. One thing to learn, and it composes with
/// what is already there instead of adding a second compositor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Mask {
    #[serde(default)]
    pub shape: MaskShape,
    /// Centre of the shape, as a fraction of the frame. 0.5, 0.5 is the middle.
    #[serde(default = "half")]
    pub x: f64,
    #[serde(default = "half")]
    pub y: f64,
    /// Size of the shape as a fraction of the frame (its full width / height,
    /// not a radius).
    #[serde(default = "half")]
    pub width: f64,
    #[serde(default = "half")]
    pub height: f64,
    /// Softness of the edge, as a fraction of the shape's own half-size. 0 is a
    /// hard cut — which on a face reads as a sticker, so the default is soft.
    #[serde(default = "default_feather")]
    pub feather: f64,
    /// Keep what is *outside* the shape instead of inside it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inverted: bool,
}

/// The feather a new mask gets.
pub const DEFAULT_MASK_FEATHER: f64 = 0.15;
/// The smallest and largest a mask's side may be, as a fraction of the frame.
pub const MIN_MASK_SIZE: f64 = 0.01;
pub const MAX_MASK_SIZE: f64 = 2.0;

fn default_feather() -> f64 {
    DEFAULT_MASK_FEATHER
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            shape: MaskShape::default(),
            x: 0.5,
            y: 0.5,
            width: 0.5,
            height: 0.5,
            feather: default_feather(),
            inverted: false,
        }
    }
}

impl Mask {
    /// The mask with every field clamped into range: sizes to a visible minimum,
    /// the centre to the frame, feather to 0..1. A zero-width mask would blank
    /// the clip entirely, which is never what was meant.
    pub fn normalized(self) -> Self {
        Self {
            shape: self.shape,
            x: clamp01(self.x),
            y: clamp01(self.y),
            width: finite_or(self.width, 0.5).clamp(MIN_MASK_SIZE, MAX_MASK_SIZE),
            height: finite_or(self.height, 0.5).clamp(MIN_MASK_SIZE, MAX_MASK_SIZE),
            feather: clamp01(self.feather),
            inverted: self.inverted,
        }
    }
}

/// How a clip's picture is laid onto the tracks below it. `Normal` is plain alpha-over; every
/// other mode mixes the clip's colour with the composite under it by a formula of the two, and
/// the clip's alpha (opacity, mask, key, a dissolve) then decides how much of that mix replaces
/// the composite — outside the clip's picture the lower tracks are untouched.
///
/// Deliberately the separable modes FFmpeg's `blend` computes as the textbook defines them, per
/// 8-bit channel in RGB (`engine::blend_chains`): its soft light, dodge, burn and subtract are
/// other formulas, so they are not offered. Its `overlay` and `hardlight` condition on the *top*
/// input and the bottom one respectively, the other way round from the usual names, which
/// [`BlendMode::ffmpeg`] swaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal,
    /// Darkens: white is neutral, black stays black.
    Multiply,
    /// Lightens: black is neutral, white stays white.
    Screen,
    /// Multiply in the composite's shadows, screen in its highlights: contrast from the clip.
    Overlay,
    /// Overlay with the roles swapped: the clip's own tones decide.
    HardLight,
    /// Linear dodge: the two summed, clipped at white.
    Add,
    Darken,
    Lighten,
    Difference,
    /// Difference with less contrast.
    Exclusion,
}

impl BlendMode {
    pub const ALL: [BlendMode; 10] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::HardLight,
        BlendMode::Add,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::Exclusion,
    ];

    pub fn is_normal(&self) -> bool {
        *self == BlendMode::Normal
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::HardLight => "hard_light",
            BlendMode::Add => "add",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
        }
    }

    /// The mode a name means: the serialized one, or a common spelling of it.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match s.as_str() {
            "normal" | "none" => Some(BlendMode::Normal),
            "multiply" => Some(BlendMode::Multiply),
            "screen" => Some(BlendMode::Screen),
            "overlay" => Some(BlendMode::Overlay),
            "hard_light" | "hardlight" => Some(BlendMode::HardLight),
            "add" | "linear_dodge" | "addition" => Some(BlendMode::Add),
            "darken" => Some(BlendMode::Darken),
            "lighten" => Some(BlendMode::Lighten),
            "difference" => Some(BlendMode::Difference),
            "exclusion" => Some(BlendMode::Exclusion),
            _ => None,
        }
    }

    /// FFmpeg `blend`'s name for the mode with the clip as its **top** input, `None` for `Normal`
    /// (which is `overlay`, not `blend`).
    pub fn ffmpeg(self) -> Option<&'static str> {
        Some(match self {
            BlendMode::Normal => return None,
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "hardlight",
            BlendMode::HardLight => "overlay",
            BlendMode::Add => "addition",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
        })
    }

    /// The mode in one channel, `top` the clip and `bottom` the composite, both `0..=1`: the
    /// textbook formula FFmpeg's integer arithmetic rounds.
    pub fn apply(self, top: f64, bottom: f64) -> f64 {
        let (a, b) = (top, bottom);
        let screen = |a: f64, b: f64| a + b - a * b;
        let hard = |a: f64, b: f64| if a <= 0.5 { 2.0 * a * b } else { screen(2.0 * a - 1.0, b) };
        match self {
            BlendMode::Normal => a,
            BlendMode::Multiply => a * b,
            BlendMode::Screen => screen(a, b),
            BlendMode::Overlay => hard(b, a),
            BlendMode::HardLight => hard(a, b),
            BlendMode::Add => (a + b).min(1.0),
            BlendMode::Darken => a.min(b),
            BlendMode::Lighten => a.max(b),
            BlendMode::Difference => (a - b).abs(),
            BlendMode::Exclusion => a + b - 2.0 * a * b,
        }
    }

    /// The words a menu and a diff use.
    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::HardLight => "Hard light",
            BlendMode::Add => "Add",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
        }
    }
}

fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

fn clamp01(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// A per-clip video effect, realized as a filter inserted into the clip's video
/// chain at export (after color correction). The order in `Clip::effects` is the
/// order they are applied. `ChromaKey` is the one effect that establishes an
/// alpha channel, so the clip composites with transparency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VideoEffect {
    /// Gaussian blur (`gblur`); larger `sigma` = softer.
    Blur { sigma: f64 },
    /// Unsharp-mask sharpen; `amount` is the luma strength.
    Sharpen { amount: f64 },
    /// Desaturate to grayscale.
    Grayscale,
    /// Invert colors (negative).
    Invert,
    /// Darken the frame edges.
    Vignette,
    /// Key out a color to transparency (green/blue screen). `color` is any ffmpeg
    /// color (e.g. `green`, `0x00ff00`); `similarity`/`blend` in 0.0–1.0.
    ChromaKey { color: String, similarity: f64, blend: f64 },
}

impl VideoEffect {
    /// Short name, for listing a chain in a diff or a log line.
    pub fn name(&self) -> &'static str {
        match self {
            VideoEffect::Blur { .. } => "blur",
            VideoEffect::Sharpen { .. } => "sharpen",
            VideoEffect::Grayscale => "grayscale",
            VideoEffect::Invert => "invert",
            VideoEffect::Vignette => "vignette",
            VideoEffect::ChromaKey { .. } => "chroma key",
        }
    }

    /// True when applying this effect leaves the frame with an alpha channel.
    pub fn produces_alpha(&self) -> bool {
        matches!(self, VideoEffect::ChromaKey { .. })
    }
}

/// A per-clip audio effect, realized as a filter inserted into the clip's audio
/// chain at export (after the clip gain). The order in `Clip::audio` is the order
/// they are applied. Thresholds/gains are in dB at the model boundary and
/// converted to the linear units ffmpeg's dynamics filters want by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AudioEffect {
    /// High-pass: attenuate below `hz` (cut rumble / handling noise).
    Highpass { hz: f64 },
    /// Low-pass: attenuate above `hz` (cut hiss).
    Lowpass { hz: f64 },
    /// Single parametric EQ band at `hz`, `width` Hz wide, `gain_db` boost/cut.
    Equalizer { hz: f64, width: f64, gain_db: f64 },
    /// Dynamic-range compressor.
    Compressor {
        threshold_db: f64,
        ratio: f64,
        attack_ms: f64,
        release_ms: f64,
        makeup_db: f64,
    },
    /// Noise gate: silence audio below `threshold_db`.
    Gate { threshold_db: f64 },
    /// Spectral noise reduction (FFmpeg's `afftdn`): take up to `reduction_db` off the noise
    /// under `floor_db` (dBFS). With `track_noise` the filter follows the floor itself from
    /// the `floor_db` it starts at, for a recording whose noise moves.
    Denoise {
        reduction_db: f64,
        floor_db: f64,
        #[serde(default)]
        track_noise: bool,
    },
}

impl AudioEffect {
    /// Whether the effect reacts to *level* (a compressor, a gate, a denoiser): a gain put
    /// ahead of it changes what it does, so the clip's level cannot be moved across it. A
    /// denoiser's floor is an absolute level, so a gain moves the sound against it and changes
    /// what is taken for noise (tracking the floor only follows it from where it starts).
    /// Filters and EQ are linear and commute with any gain.
    pub fn is_dynamic(&self) -> bool {
        matches!(
            self,
            AudioEffect::Compressor { .. } | AudioEffect::Gate { .. } | AudioEffect::Denoise { .. }
        )
    }

    /// Short name, for listing a chain in a diff or a log line.
    pub fn name(&self) -> &'static str {
        match self {
            AudioEffect::Highpass { .. } => "highpass",
            AudioEffect::Lowpass { .. } => "lowpass",
            AudioEffect::Equalizer { .. } => "EQ",
            AudioEffect::Compressor { .. } => "compressor",
            AudioEffect::Gate { .. } => "gate",
            AudioEffect::Denoise { .. } => "denoise",
        }
    }
}
