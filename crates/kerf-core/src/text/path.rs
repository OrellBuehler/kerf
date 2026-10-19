//! Glyph outlines as SVG path data.
//!
//! An outline is drawn in font units (y up) and written in user units (y down) at the glyph's
//! origin, every point rounded to a hundredth of a unit. The first point of a glyph is absolute, the
//! rest relative to the rounded point before, so rounding never accumulates and a title of outlines
//! stays small (a Latin glyph is a few hundred bytes).

use skrifa::instance::Size;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};

use super::layout::Glyph;

/// Hundredths of a unit.
type Fixed = i64;

fn fixed(v: f64) -> Fixed {
    (v * 100.0).round() as Fixed
}

/// Writes path data into a string.
struct Writer<'a> {
    out: &'a mut String,
    /// Font units to user units, and the origin.
    k: f64,
    x: f64,
    y: f64,
    at: (Fixed, Fixed),
    start: (Fixed, Fixed),
    last: Option<char>,
    first: bool,
}

impl Writer<'_> {
    fn point(&self, x: f32, y: f32) -> (Fixed, Fixed) {
        (fixed(self.x + f64::from(x) * self.k), fixed(self.y - f64::from(y) * self.k))
    }

    fn number(&mut self, v: Fixed, lead: bool) {
        if !lead && v >= 0 {
            self.out.push(' ');
        }
        let (sign, v) = if v < 0 { ("-", -v) } else { ("", v) };
        let (whole, frac) = (v / 100, v % 100);
        self.out.push_str(sign);
        self.out.push_str(&whole.to_string());
        if frac != 0 {
            if frac % 10 == 0 {
                self.out.push_str(&format!(".{}", frac / 10));
            } else {
                self.out.push_str(&format!(".{frac:02}"));
            }
        }
    }

    fn command(&mut self, c: char, points: &[(Fixed, Fixed)]) {
        let relative = c.is_ascii_lowercase();
        let lead = self.last != Some(c);
        if lead {
            self.out.push(c);
        }
        let mut first = lead;
        let base = self.at;
        for &(x, y) in points {
            let (px, py) = if relative { (x - base.0, y - base.1) } else { (x, y) };
            self.number(px, first);
            self.number(py, false);
            first = false;
        }
        if let Some(&end) = points.last() {
            self.at = end;
        }
        self.last = Some(c);
    }
}

impl OutlinePen for Writer<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.point(x, y);
        let c = if self.first { 'M' } else { 'm' };
        self.first = false;
        self.last = None;
        self.command(c, &[p]);
        self.start = p;
        // A move's implicit followers are line-tos: never let the next command be read as one.
        self.last = None;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.point(x, y);
        self.command('l', &[p]);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.point(cx0, cy0), self.point(x, y));
        self.command('q', &[c, p]);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.point(cx0, cy0), self.point(cx1, cy1), self.point(x, y));
        self.command('c', &[c0, c1, p]);
    }

    fn close(&mut self) {
        self.out.push('z');
        self.at = self.start;
        self.last = None;
    }
}

/// Append the outline of `glyph` to `out` as path data, in user units at the glyph's origin. Returns
/// whether anything was written (a space has no outline).
pub fn glyph_path(glyph: &Glyph, out: &mut String) -> bool {
    let face = &glyph.face;
    let font = face.font();
    let Some(outline) = font.outline_glyphs().get(GlyphId::new(glyph.id)) else {
        return false;
    };
    let location = face.location(&glyph.variations);
    let before = out.len();
    let mut pen = Writer {
        out,
        k: f64::from(glyph.size) / f64::from(face.units_per_em()),
        x: f64::from(glyph.x),
        y: f64::from(glyph.y),
        at: (0, 0),
        start: (0, 0),
        last: None,
        first: true,
    };
    if outline
        .draw(DrawSettings::unhinted(Size::unscaled(), &location), &mut pen)
        .is_err()
    {
        out.truncate(before);
        return false;
    }
    out.len() > before
}
