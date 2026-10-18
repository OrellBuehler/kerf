//! The shapes of a template: its manifest, its controls, and what a title on the timeline remembers
//! of the template it came from. All serde, snake_case JSON, mirrored in `frontend/src/lib/types.ts`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The `format` field of `manifest.json`.
pub const FORMAT: &str = "kerf-template";
/// The manifest version this build reads and writes. A file with another version is refused by name
/// (a newer one says "made by a newer Kerf").
pub const VERSION: u32 = 1;

/// What a control changes, and so what its value must look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    /// An element's text content (`text`, `tspan`, `textPath`).
    Text,
    /// A colour attribute (`fill`, `stroke`, `stop-color`, ...), through the colour validator.
    Color,
    /// A number in an attribute, finite and within `min` / `max`.
    Number,
    /// A `font-family` (a family the project, a template or the system has).
    Font,
    /// An `<image>`'s `href`: a `data:` URL under the embedded-image rules.
    Image,
}

impl TemplateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Color => "color",
            Self::Number => "number",
            Self::Font => "font",
            Self::Image => "image",
        }
    }
}

/// Where a control's value goes: the element with this `id`, and which attribute (none for `text`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateTarget {
    /// The `id` of an element of the title; exactly one element must have it.
    pub element: String,
    /// The attribute as the source spells it (`fill`, `xlink:href`). Required for every kind but `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
}

/// One thing a person can change about a template without opening the artwork.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateControl {
    /// A short identifier (`tagline`), unique in the template: the key of a value.
    pub id: String,
    /// What the inspector calls it.
    pub label: String,
    pub kind: TemplateKind,
    /// Every place the value is written; one value, many targets (a colour on three gradient stops).
    pub targets: Vec<TemplateTarget>,
    /// The bounds of a `number` control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// A font a template carries: the file inside the zip and the family it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ManifestFont {
    /// The entry name, `fonts/<file>`.
    pub file: String,
    pub family: String,
}

/// `manifest.json` of a `.kerftemplate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateManifest {
    /// Always [`FORMAT`].
    pub format: String,
    pub version: u32,
    /// A UUID (lower case, hyphenated): the library file is `<id>.kerftemplate`.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// How long the title runs, in seconds: the length an applied one gets.
    pub duration: f64,
    /// The title's own shape, in pixels (its `viewBox`): what a project frame is fit against.
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub controls: Vec<TemplateControl>,
    #[serde(default)]
    pub fonts: Vec<ManifestFont>,
}

/// What a title on the timeline remembers of the template it was made from: enough to show its controls
/// and to set them again. The artwork is the title's own `svg`, not stored twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInstance {
    /// The library id it came from (the library is not consulted again).
    pub template_id: String,
    pub name: String,
    pub controls: Vec<TemplateControl>,
    /// Each control's current value, read back from the title's `svg` (a control whose element is gone is
    /// absent). **`image` controls are not mirrored here**: their `data:` URL is in the `svg` already.
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}
