//! The floating animated SVG titles of a plan: what a compositor needs to draw one exactly as the export does.
//!
//! ffmpeg never sees an SVG (`engine/cli/svg_layer.rs`): resvg draws the document at a moment into a straight-alpha PNG and
//! the graph `overlay`s it. The same resvg draws the same pixels in a compositor's process, so the plan only has to say *which*
//! picture: the artwork, the fonts it is drawn with, its placement and opacity, and the document's own clock at this frame
//! (an export frame's picture `k - first` of the title's sequence, a still's `doc_time(t)`). **Nothing here re-derives the
//! export's arithmetic**: the windows, the clock and the rectangle are `svg_layer`'s own (`sequence_of`, `rect_of`), read by
//! the [`Planner`](crate::planner::Planner) once and carried; the picture is drawn by the function the export draws with
//! (`svg_layer::picture_png`), and named by the key it names its pictures with.
//!
//! A title clip (a generated clip on a track) is no overlay but a clip chain (fit, fades and transitions on an alpha picture,
//! then `overlay` over the lower tracks): the plan names it ([`PlanSvg::title`] is `None`) and no compositor draws it.

use std::sync::Arc;

use uuid::Uuid;

use crate::engine::svg_layer;
use crate::error::Result;
use crate::fonts::FontSet;

/// One animated SVG title on screen at a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanSvg {
    /// The overlay's id, or a title clip's.
    pub id: Uuid,
    /// What a compositor draws: `None` for a title clip on a track, which no compositor draws.
    pub title: Option<SvgTitle>,
}

/// A floating title's picture at one frame: everything that decides its pixels and where they land.
#[derive(Clone)]
pub struct SvgTitle {
    /// The artwork as stored (the export sanitises and reads it again, as a compositor does).
    pub svg: Arc<str>,
    /// The embedded fonts it is drawn with, taken when it was planned (the export's `FontSet::named_by` of the artwork).
    pub fonts: Arc<FontSet>,
    /// The rectangle it is laid in, as the overlay stores it: `x`, `y`, `width`, `height`, fractions of the frame.
    pub place: [f64; 4],
    /// The opacity it is drawn with, clamped as the export clamps it (it is in the picture's alpha: the graph has no other).
    pub opacity: f64,
    /// The document's own clock at this frame.
    pub doc_t: f64,
}

impl std::fmt::Debug for SvgTitle {
    // (The artwork is up to 128 KiB: a plan that is printed names its size, not its text.)
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SvgTitle")
            .field("svg_bytes", &self.svg.len())
            .field("fonts", &self.fonts)
            .field("place", &self.place)
            .field("opacity", &self.opacity)
            .field("doc_t", &self.doc_t)
            .finish()
    }
}

impl PartialEq for SvgTitle {
    fn eq(&self, other: &Self) -> bool {
        self.svg == other.svg
            && self.fonts.digest() == other.fonts.digest()
            && self.place == other.place
            && self.opacity == other.opacity
            && self.doc_t == other.doc_t
    }
}

/// Where a title lands on a frame, in pixels: its top-left corner (which may lie off the frame) and its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgRect {
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
}

impl PlanSvg {
    /// The rectangle a floating title covers on a frame of `size`, or `None` when none of it is on the frame or it would be too large
    /// to draw (the export leaves such a title out of its graph altogether), and for a title clip, which has no rectangle of its own.
    pub fn rect(&self, size: (u32, u32)) -> Option<SvgRect> {
        let title = self.title.as_ref()?;
        svg_layer::rect_of(self.id, title.place, size.0, size.1).map(|r| SvgRect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        })
    }
}

impl SvgTitle {
    /// The static document resvg draws at this frame: the artwork with its animations at the moment `doc_t` (read once, from the
    /// few documents kept). Errs for artwork that cannot be read, which the export fails on too.
    pub fn text(&self) -> Result<String> {
        svg_layer::text_at(&self.svg, self.doc_t)
    }

    /// What names the picture of `text` (from [`SvgTitle::text`]) at `rect`: the document, the size, the opacity and the fonts,
    /// and the version of the rasteriser. Two frames with the same key are the same picture, whatever moment they are.
    pub fn key(&self, text: &str, rect: SvgRect) -> String {
        svg_layer::picture_key(&self.fonts, text, rect.w, rect.h, self.opacity)
    }

    /// Whether this frame's picture is not the one the frame before it showed (`frame_secs` earlier on the document's clock): the document
    /// differs at the two moments. A document that cannot move never does ([`crate::svg::may_animate`] is false: one picture for the whole
    /// cut), nor does one that holds still between its moves; a moving one is a new picture each frame, and each is a resvg draw and a
    /// conversion. `false` for artwork that cannot be read (the render refuses it).
    pub fn changes_within(&self, frame_secs: f64) -> bool {
        if !crate::svg::may_animate(&self.svg) {
            return false;
        }
        let before = (self.doc_t - frame_secs).max(0.0);
        if before >= self.doc_t {
            return false;
        }
        match (svg_layer::text_at(&self.svg, before), self.text()) {
            (Ok(was), Ok(is)) => was != is,
            _ => false,
        }
    }

    /// The picture of `text` as the PNG the export reads, drawn by the export's own function.
    pub fn png(&self, text: &str, rect: SvgRect) -> Result<Vec<u8>> {
        svg_layer::picture_png(text, rect.w, rect.h, self.opacity, &self.fonts)
    }
}

/// The graph text that lays one floating title's pictures (read from ffmpeg input `input`, restamped to start on output frame
/// `first_frame`) over the pad `from`, ending on `to`, at `at` on the title's `window`: the export's own chains
/// (`[N:v]settb=..,setpts=N+k[svg0];[from][svg0]overlay=x=..:y=..:eof_action=pass:enable='between(t,..)'[to]`), for a compositor's
/// probe to run through the FFmpeg in use.
pub fn svg_overlay_chains(
    fps: crate::Rational,
    first_frame: u64,
    input: usize,
    from: &str,
    to: &str,
    at: (i64, i64),
    window: (f64, f64),
) -> String {
    svg_layer::overlay_chains(0, input, from, to, fps, first_frame, at, window).join(";")
}

/// A `w` x `h` picture of straight-alpha `rgba` pixels as the PNG the export reads (`svg::raster::encode_straight_png`): for a
/// compositor's probe, which makes test pictures of its own.
pub fn straight_rgba_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    crate::svg::raster::encode_straight_png(w, h, rgba)
}
