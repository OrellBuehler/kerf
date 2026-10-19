//! One font face for the text engine: its bytes, the shaper's tables and its vertical metrics.

use std::sync::{Arc, OnceLock};

use fontdb::{Database, Source, ID};
use skrifa::instance::{Location, Size};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider, Tag};

/// A variation axis setting: the axis tag and its value in user units (`wght` 700).
pub type Variation = ([u8; 4], f32);

const OPSZ: [u8; 4] = *b"opsz";

/// A face, read from its bytes. The bytes are shared with the font database when it holds them in
/// memory (an embedded font, a mapped file), copied once when it holds a path.
pub struct Face {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    index: u32,
    upem: u16,
    has_opsz: bool,
    /// Glyphs may be pictures (an `SVG ` table, a bitmap strike) rather than outlines.
    pictures: bool,
    shaper: OnceLock<harfrust::ShaperData>,
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Face")
            .field("index", &self.index)
            .field("upem", &self.upem)
            .finish()
    }
}

/// Vertical metrics in font units, at a position in the design space: what usvg lays text out with
/// (ascent and descent rounded, the x-height 45% of the height when the font gives none).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub ascent: f32,
    /// Below the baseline, so negative.
    pub descent: f32,
    pub leading: f32,
    pub x_height: f32,
}

impl Face {
    /// The face in `data` (a font file) at `index`, or `None` when it is not a font skrifa reads or
    /// its units per em are outside 16..=16384 (what usvg refuses too).
    pub fn new(data: Arc<dyn AsRef<[u8]> + Send + Sync>, index: u32) -> Option<Arc<Face>> {
        let font = FontRef::from_index((*data).as_ref(), index).ok()?;
        let upem = font.head().ok()?.units_per_em();
        if !(16..=16384).contains(&upem) {
            return None;
        }
        let has_opsz = font.axes().get_by_tag(Tag::new(&OPSZ)).is_some();
        let pictures = font.svg().is_ok() || !font.bitmap_strikes().is_empty();
        Some(Arc::new(Face {
            data: data.clone(),
            index,
            upem,
            has_opsz,
            pictures,
            shaper: OnceLock::new(),
        }))
    }

    /// The face `id` of a font database.
    pub fn from_db(db: &Database, id: ID) -> Option<Arc<Face>> {
        let (source, index) = db.face_source(id)?;
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = match source {
            Source::Binary(data) | Source::SharedFile(_, data) => data,
            Source::File(_) => Arc::new(db.with_face_data(id, |data, _| data.to_vec())?),
        };
        Face::new(data, index)
    }

    pub(crate) fn font(&self) -> FontRef<'_> {
        FontRef::from_index((*self.data).as_ref(), self.index).expect("read when the face was made")
    }

    pub(crate) fn shaper_data(&self) -> &harfrust::ShaperData {
        self.shaper.get_or_init(|| harfrust::ShaperData::new(&self.font()))
    }

    pub fn units_per_em(&self) -> f32 {
        f32::from(self.upem)
    }

    /// Whether the face maps `c` to a glyph.
    pub fn has_char(&self, c: char) -> bool {
        self.font().charmap().map(c).is_some_and(|g| g.to_u32() != 0)
    }

    /// Whether glyph `id` is drawn by its outline alone: not a colour glyph, not from a face that can
    /// draw glyphs as pictures. A glyph that is not can only be drawn by the renderer.
    pub fn is_outline(&self, id: u32) -> bool {
        !self.pictures && self.font().color_glyphs().get(GlyphId::new(id)).is_none()
    }

    /// `variations` with `opsz` set to the size when the face has the axis and it is not set (CSS
    /// `font-optical-sizing: auto`, as usvg does).
    pub fn variations_at(&self, variations: &[Variation], size: f32) -> Vec<Variation> {
        let mut out = variations.to_vec();
        if self.has_opsz && !out.iter().any(|(tag, _)| *tag == OPSZ) {
            out.push((OPSZ, size));
        }
        out
    }

    pub(crate) fn location(&self, variations: &[Variation]) -> Location {
        self.font()
            .axes()
            .location(variations.iter().map(|(tag, value)| (Tag::new(tag), *value)))
    }

    /// The face's vertical metrics at `variations`.
    pub fn metrics(&self, variations: &[Variation]) -> Metrics {
        let location = self.location(variations);
        let m = self.font().metrics(Size::unscaled(), &location);
        let ascent = m.ascent.round();
        let descent = m.descent.round();
        let x_height = m
            .x_height
            .filter(|x| *x > 0.0)
            .map(f32::round)
            .unwrap_or_else(|| ((ascent - descent) * 0.45).round());
        Metrics {
            ascent,
            descent,
            leading: m.leading,
            x_height,
        }
    }
}
