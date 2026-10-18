//! Motion templates: an animated SVG title packaged with the fonts it needs and a few **controls**
//! (a tagline, a brand colour, a logo) that can be changed without touching the artwork, kept in a
//! per-user library and applied to any project.
//!
//! * [`TemplateManifest`] / [`TemplateControl`] (`model.rs`) — the serde shapes, snake_case JSON, which
//!   the frontend mirrors; [`TemplateInstance`] is what a title on the timeline remembers of its template.
//! * `validate.rs` — what a manifest, its controls and a control's value must look like.
//! * `values.rs` — **the pure splice**: control values go into the SVG by element id, by byte range
//!   (`svg::splice`), escaped, and the result then goes through the sanitiser like any custom SVG.
//! * `archive.rs` — the `.kerftemplate` zip ([`Template`]): read into memory by known names only, never
//!   extracted, with hard limits.
//! * `library.rs` — `<data dir>/kerf/templates/<id>.kerftemplate` ([`Library`]): list, save, import,
//!   export, delete. App-level; applying a template is a timeline edit (`project/templates.rs`).
//! * `save.rs` — making a template out of a title: the fonts its text resolves to, the poster.
//!
//! **A template instance is self-contained.** Applying one copies its SVG into the title and its fonts
//! into the project's `fonts` table; the project never reaches back to the library. The format is
//! modelled on EffectCraft's `.ectemplate` (MIT OR Apache-2.0); see `THIRD-PARTY-NOTICES.md`.

mod archive;
mod library;
mod model;
mod save;
mod validate;
mod values;

pub use archive::{Checked, Template};
pub use library::{
    library_dir, ImportOutcome, ImportStatus, Library, TemplateInfo, TemplateList, TemplateProblem, TemplateSaved,
};
pub use model::{
    ManifestFont, TemplateControl, TemplateInstance, TemplateKind, TemplateManifest, TemplateTarget, FORMAT, VERSION,
};
pub use save::{build_template, element_ids, BuiltTemplate, TemplateSaveRequest, TemplateSource};
pub use validate::{validate_controls, validate_controls_shape};
pub use values::{
    apply_values, image_data_url, read_values, PreparedTemplate, PreparedValues, TemplateEditInputs, MAX_IMAGE_BYTES,
};

/// The file extension of a template.
pub const EXTENSION: &str = "kerftemplate";

/// Most entries a `.kerftemplate` may hold.
pub const MAX_ENTRIES: usize = 64;
/// Most bytes a `.kerftemplate` may hold uncompressed, all entries together (declared sizes are summed
/// before anything is read).
pub const MAX_TOTAL_BYTES: u64 = 32 << 20;
/// Most bytes one font may be.
pub const MAX_FONT_BYTES: u64 = 8 << 20;
/// Most fonts a template may carry.
pub const MAX_FONTS: usize = 16;
/// Most bytes `manifest.json` may be.
pub const MAX_MANIFEST_BYTES: u64 = 256 << 10;
/// Most bytes `poster.png` may be.
pub const MAX_POSTER_BYTES: u64 = 4 << 20;
/// Most controls a template may have.
pub const MAX_CONTROLS: usize = 64;
/// Most targets one control may have.
pub const MAX_TARGETS: usize = 16;
/// Most bytes of fonts a project may carry in its `fonts` table.
pub const MAX_PROJECT_FONT_BYTES: u64 = 128 << 20;
/// The longest side of a poster, in pixels.
pub const POSTER_SIZE: u32 = 512;

/// What the tests of other modules build templates from.
#[cfg(test)]
pub(crate) mod fixtures {
    pub(crate) use super::archive::tests::{template, SVG};
}
