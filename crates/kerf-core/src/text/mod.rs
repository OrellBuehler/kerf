//! The text engine: shaping, measurement, line breaking and glyph outlines.
//!
//! * [`Face`] (`face.rs`) — one face of a font file: the shaper's tables (harfrust), its metrics and
//!   its outlines (skrifa), read from the same bytes the SVG font database holds.
//! * [`layout`] (`layout.rs`) — a paragraph in style [`Run`]s set into lines of positioned
//!   [`Glyph`]s: fallback to the next face of a run for a character the first lacks, bidi (UAX #9),
//!   shaping, letter and word spacing, line breaking at a width (UAX #14), alignment on an anchor
//!   and line spacing. [`Layout`] measures what it set.
//! * [`glyph_path`] (`path.rs`) — a glyph's outline as SVG path data; [`glyph_bounds`] its box.
//!
//! What uses it: `svg::outline`, which sets the text of a template title as glyph outlines so the
//! title draws without its fonts. Ported from EffectCraft's `crates/text` (MIT OR Apache-2.0; see
//! `THIRD-PARTY-NOTICES.md`).

mod face;
mod layout;
mod path;

pub use face::{Face, Metrics, Variation};
pub use layout::{layout, Align, Baseline, Glyph, Layout, Line, Paragraph, Run};
pub use path::{glyph_bounds, glyph_path};
