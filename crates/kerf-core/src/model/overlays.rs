//! Text overlays and animated SVG titles, and the patches that edit them.

use super::*;

fn lower_third_y() -> f64 {
    0.82
}
fn default_text_size() -> f64 {
    0.06
}
fn default_text_color() -> String {
    "white".to_string()
}

/// One keyframe of an animated [`TextOverlay`]: position and opacity at `time`
/// (seconds from the overlay's `start`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextKeyframe {
    pub time: f64,
    #[serde(default = "half")]
    pub pos_x: f64,
    #[serde(default = "lower_third_y")]
    pub pos_y: f64,
    #[serde(default = "one")]
    pub opacity: f64,
}

/// A timed text element drawn over the composited picture at export (titles,
/// lower-thirds, captions, watermarks). Positions are fractions of the output
/// frame with the text centered on `(pos_x, pos_y)`; `size` is the font height
/// as a fraction of the frame height. Rendered with `drawtext`. Captions are
/// just a batch of these generated from a transcript (see
/// `Project::captions_from_transcript`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextOverlay {
    pub id: Uuid,
    pub text: String,
    /// When the overlay appears / disappears, in timeline seconds.
    pub start: f64,
    pub end: f64,
    #[serde(default = "half")]
    pub pos_x: f64,
    #[serde(default = "lower_third_y")]
    pub pos_y: f64,
    /// Font height as a fraction of the frame height.
    #[serde(default = "default_text_size")]
    pub size: f64,
    /// Any ffmpeg color (e.g. `white`, `#ffcc00`, `yellow@0.9`).
    #[serde(default = "default_text_color")]
    pub color: String,
    /// Optional box color behind the text (e.g. `black@0.5`); `None` = no box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    /// Optional system font family name (see `fonts::list_system_fonts`);
    /// `None` (or a family that is not installed) = the font Kerf ships (`fonts::DEFAULT_FAMILY`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default)]
    pub bold: bool,
    /// Optional position/opacity animation; with ≥1 keyframe the position and
    /// opacity animate over the overlay's lifetime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<TextKeyframe>,
    /// Written by [`Timeline::captions`] rather than by hand. Regenerating
    /// captions replaces these and leaves everything else alone, so re-running
    /// after a trim does not stack a second set on top of the first — and does
    /// not throw away the title the editor typed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
}

impl TextOverlay {
    pub fn new(text: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            text: text.into(),
            start,
            end,
            pos_x: 0.5,
            pos_y: 0.82,
            size: 0.06,
            color: "white".to_string(),
            bg: None,
            font: None,
            bold: false,
            keyframes: Vec::new(),
            generated: false,
        }
    }

    /// Sample `(pos_x, pos_y, opacity)` at timeline time `t`. Static fields when
    /// the overlay is not animated; the interpolated keyframe values otherwise.
    /// Used by the still / preview path, which can't evaluate the export's
    /// per-frame `drawtext` expressions.
    pub fn sample(&self, t: f64) -> (f64, f64, f64) {
        if self.keyframes.is_empty() {
            return (self.pos_x, self.pos_y, 1.0);
        }
        let local = t - self.start;
        let chan = |get: fn(&TextKeyframe) -> f64, fallback: f64| {
            interpolate(&self.keyframes.iter().map(|k| (k.time, get(k))).collect::<Vec<_>>(), local).unwrap_or(fallback)
        };
        (
            chan(|k| k.pos_x, self.pos_x),
            chan(|k| k.pos_y, self.pos_y),
            chan(|k| k.opacity, 1.0),
        )
    }
}

/// Most animated SVG overlays one timeline holds.
pub const MAX_SVG_OVERLAYS: usize = 50;

/// The latest second an animated title may start or end at (24 hours). A position far out is a
/// mistake, not an edit, and the engine looks for the frame at it.
pub const MAX_SVG_SECONDS: f64 = 24.0 * 3600.0;

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn white_hex() -> String {
    "#ffffff".to_string()
}

fn accent_hex() -> String {
    "#ffb000".to_string()
}

/// The ready-made animated titles; each is generated as SVG with SMIL, so a preset overlay is an
/// ordinary SVG overlay that remembers how it was made (and is re-made when its timing or frame
/// changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SvgPresetKind {
    FadeUp,
    Typewriter,
    LowerThird,
    WordPop,
}

impl SvgPresetKind {
    pub const ALL: [SvgPresetKind; 4] = [Self::FadeUp, Self::Typewriter, Self::LowerThird, Self::WordPop];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FadeUp => "fade_up",
            Self::Typewriter => "typewriter",
            Self::LowerThird => "lower_third",
            Self::WordPop => "word_pop",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::FadeUp => "Fade up",
            Self::Typewriter => "Typewriter",
            Self::LowerThird => "Lower third",
            Self::WordPop => "Word pop",
        }
    }
}

/// What a preset is filled with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SvgPresetParams {
    pub text: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Any SVG colour (`#ffffff`, `white`, `rgb(...)`).
    #[serde(default = "white_hex")]
    pub color: String,
    #[serde(default = "accent_hex")]
    pub accent: String,
    /// A font family; `None` is the font Kerf ships.
    #[serde(default)]
    pub font: Option<String>,
}

impl SvgPresetParams {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            subtitle: None,
            color: white_hex(),
            accent: accent_hex(),
            font: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SvgPreset {
    pub kind: SvgPresetKind,
    pub params: SvgPresetParams,
}

/// An animated SVG title on the timeline: SMIL-animated vector artwork, drawn over the picture
/// (above clips, below text overlays) between `start` and `end`. `svg` is the **sanitised** source
/// (what is stored is what is rendered; see [`crate::svg`]). It is fit "contain", centred, into the
/// rectangle `x, y, width, height` (fractions of the frame, top-left origin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SvgOverlay {
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    pub svg: String,
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default = "one")]
    pub width: f64,
    #[serde(default = "one")]
    pub height: f64,
    #[serde(default = "one")]
    pub opacity: f64,
    /// Seconds of the document's own clock already elapsed at `start`: a slice or range export that
    /// cuts into the overlay starts it part-way through its animation instead of replaying it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f64,
    /// How the SVG was made when it came from a preset; `None` for custom artwork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<SvgPreset>,
    /// The motion template this title was made from, with its controls and their values; `None` for
    /// anything else (and so absent from the stored JSON, which is why a project without templates
    /// reads and writes the bytes it always did). The artwork is `svg` itself: a value is spliced into
    /// it, not kept beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<crate::template::TemplateInstance>,
}

impl SvgOverlay {
    /// The overlay's own clock at timeline time `t`.
    pub fn doc_time(&self, t: f64) -> f64 {
        (t - self.start).max(0.0) + self.offset
    }

    /// How long the document is meant to run: the visible span plus what a slice cut off the front.
    pub fn doc_duration(&self) -> f64 {
        (self.end - self.start).max(0.0) + self.offset
    }

    /// A name to show: the given one, else the preset's text, else a placeholder.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.trim().to_string()
        } else if let Some(p) = &self.preset {
            p.params.text.clone()
        } else {
            "SVG".to_string()
        }
    }

    /// Re-make a preset's artwork for `frame` and the current length. Custom artwork is untouched.
    pub fn regenerate(&mut self, frame: (u32, u32)) {
        if let Some(p) = &self.preset {
            self.svg = crate::svg::render_preset(p.kind, &p.params, self.doc_duration(), frame);
        }
    }
}

/// Fields of an [`SvgOverlay`] to change; `None` leaves one alone. `svg` replaces the artwork with
/// custom source (and so drops the preset); `preset_params` re-fills a preset overlay and
/// `preset_kind` switches it to another kind (both refused on custom artwork, and either one is
/// exclusive with `svg`; together they re-make the preset in one edit).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SvgOverlayPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_params: Option<SvgPresetParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_kind: Option<SvgPresetKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
}

impl SvgOverlayPatch {
    /// What a patch can be refused for without the timeline and without parsing: the artwork and the
    /// preset's parameters or kind are exclusive, the parameters are valid, the times are numbers.
    pub fn check_cheap(&self) -> crate::error::Result<()> {
        use crate::error::Error;
        if self.svg.is_some() && (self.preset_params.is_some() || self.preset_kind.is_some()) {
            return Err(Error::InvalidArgument(
                "pass either svg or preset_params / preset_kind, not both".to_string(),
            ));
        }
        if let Some(params) = &self.preset_params {
            crate::svg::check_params(params)?;
        }
        if [self.start, self.end].iter().flatten().any(|v| !v.is_finite()) {
            return Err(Error::InvalidArgument("start and end must be numbers of seconds".to_string()));
        }
        if [self.start, self.end].iter().flatten().any(|v| *v > MAX_SVG_SECONDS) {
            return Err(Error::InvalidArgument(format!(
                "start and end must be within the first {} hours",
                MAX_SVG_SECONDS / 3600.0
            )));
        }
        Ok(())
    }

    /// Everything about the patch that costs time: [`SvgOverlayPatch::check_cheap`], then the custom
    /// artwork sanitised and parsed (up to a second and a half for a heavy document). A surface calls
    /// this **before it takes the project lock** and gives the result to
    /// `Project::update_svg_overlay_parsed`.
    pub fn check(&self) -> crate::error::Result<Option<crate::svg::SvgDoc>> {
        self.check_cheap()?;
        self.svg.as_deref().map(crate::svg::SvgDoc::parse).transpose()
    }
}
