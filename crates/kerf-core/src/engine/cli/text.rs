//! Text drawn by `drawtext`: its escaping, its look and the filters of an overlay.

use super::*;

/// Escape a value for a single-quoted `drawtext` option inside a filtergraph passed as one argv
/// argument. The value is read by **three parsers, innermost first**, and each has its own escape:
///
/// 1. the *expansion* of `text=` (`expansion` here): a backslash escapes the next character, `%{...}`
///    is a function and a bare `%` a configuration error that silently blanks the whole overlay
///    (ffmpeg still exits 0). So `\` -> `\\` and `%` -> `\%`. `fontfile=` is a path, not expanded
///    text: escaping `%` there would corrupt a path that legitimately contains one;
/// 2. the filter's *option string* (`av_get_token` with `:` as the terminator): `:` ends a value, a
///    backslash escapes the next character, an apostrophe starts a quotation, and blanks at either end
///    are dropped. So `\` -> `\\`, `'` -> `\'`, `:` -> `\:`, and a leading or trailing blank gets a
///    backslash (which is also how a Windows font path, `C:\Fonts\a.ttf`, survives);
/// 3. the *filtergraph*: `'...'` protects `[],;` and is stripped, a backslash inside it is literal,
///    and an apostrophe is `'\''` (close, escaped quote, reopen).
///
/// Applying only the third level, as this once did, made an apostrophe swallow every option after it
/// (the text came out as garbage in black at the top left), a colon fail the whole render on FFmpeg 9
/// and cut the text short on 6.1, and a backslash vanish. `drawtext_text_is_what_the_user_typed`
/// renders each against the literal text on whichever FFmpeg runs it.
///
/// A line break (LF, CR, CRLF as one, the Unicode line and paragraph separators) collapses to a
/// space: the overlays are one line, and FFmpeg's `drawtext` would otherwise start another one at a
/// CR as well as at an LF. Control characters other than tab are dropped: a NUL cannot be passed in an
/// argv at all ("nul byte found in provided data" — every preview and export would fail to spawn until
/// the title was found and deleted) and the rest (ESC, the C1 block) draw as nothing useful.
fn escape_drawtext(s: &str, expansion: bool) -> String {
    escape_drawtext_with(s, expansion, false)
}

/// [`escape_drawtext`] that can keep line feeds as line breaks (`lines`), for the compositor's
/// measuring graphs, which draw one prefix of a title per line.
fn escape_drawtext_with(s: &str, expansion: bool, lines: bool) -> String {
    let blank = |c: char| matches!(c, ' ' | '\t');
    let clean = clean_drawtext(s, lines);
    let expanded = if expansion {
        clean.replace('\\', "\\\\").replace('%', "\\%")
    } else {
        clean
    };
    let start = expanded.len() - expanded.trim_start_matches(blank).len();
    let end = expanded.trim_end_matches(blank).len();
    let mut option = String::with_capacity(expanded.len() + 8);
    for (i, c) in expanded.char_indices() {
        match c {
            '\\' => option.push_str("\\\\"),
            '\'' => option.push_str("\\'"),
            ':' => option.push_str("\\:"),
            c => {
                if blank(c) && (i < start || i >= end) {
                    option.push('\\');
                }
                option.push(c);
            }
        }
    }
    option.replace('\'', "'\\''")
}

/// The text `drawtext` is given for `s`, before any escaping: what [`escape_drawtext`] keeps of it (a line
/// break is a blank, or a break of its own with `lines`; control characters other than tab are dropped).
fn clean_drawtext(s: &str, lines: bool) -> String {
    let blank = |c: char| matches!(c, ' ' | '\t');
    let mut clean = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' if lines => clean.push('\n'),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => clean.push(' '),
            c if c.is_control() && !blank(c) => {}
            c => clean.push(c),
        }
    }
    clean
}

/// The characters `drawtext` draws for an overlay's text, in order: the text the compositor measures
/// and rasterises piece by piece ([`text_piece_filter`]).
pub fn text_as_drawn(s: &str) -> String {
    clean_drawtext(s, false)
}

/// The most characters of a text the compositor takes apart bitmap by bitmap (the measuring graph holds
/// a prefix of it per line); more is FFmpeg's.
pub const MAX_SEQUENCE_CHARS: usize = 160;

/// The largest side, in pixels, of the canvas a text is measured on.
pub const MAX_MEASURE_SIDE: f64 = 16_384.0;

/// Why a text cannot be taken apart into the bitmaps `drawtext` blends, when that can be told before
/// anything is run: a tab (`drawtext` lays it out by its own tab stops), more than [`MAX_SEQUENCE_CHARS`]
/// characters, and, once the size is known (`px`, the font's pixel height), a text too wide to be measured
/// on a canvas of [`MAX_MEASURE_SIDE`] pixels. Everything else the compositor cannot take apart (a glyph that
/// does not shape one to a unit, a face whose bitmaps overhang the dictionary's cell) only the render can see.
pub fn text_sequence_limit(text: &str, px: Option<u32>) -> Option<String> {
    let drawn = text_as_drawn(text);
    if drawn.contains('\t') {
        return Some("a tab in the text".to_string());
    }
    let n = drawn.chars().count();
    if n > MAX_SEQUENCE_CHARS {
        return Some(format!("{n} characters of text (over {MAX_SEQUENCE_CHARS})"));
    }
    let px = f64::from(px?);
    if px * 1.8 * n as f64 + 64.0 > MAX_MEASURE_SIDE || px * 3.0 + 64.0 > MAX_MEASURE_SIDE {
        return Some(format!("{n} characters at {px} px are too wide to measure"));
    }
    None
}

/// [`escape_drawtext`] for `text=`.
pub(super) fn escape_drawtext_text(s: &str) -> String {
    escape_drawtext(s, true)
}

/// [`escape_drawtext`] for `fontfile=`, a raw filesystem path rather than drawtext-expanded text.
pub(super) fn escape_drawtext_path(s: &str) -> String {
    escape_drawtext(s, false)
}

/// How an overlay looks, as `drawtext` is told it: what the export, the still and the
/// compositor's rasteriser ([`text_raster_graph`]) all spell the same way. Every field is already
/// safe to splice (`safe_color`, `valid_color`, a resolved font).
pub(super) struct TextLook<'a> {
    pub(super) text: &'a str,
    /// Font height as a fraction of the frame height.
    pub(super) size: f64,
    pub(super) color: &'a str,
    /// Whether there is a box, and its colour.
    pub(super) bg: Option<&'a str>,
    /// The font file; `None` leaves `drawtext` its own default (the shipped font could not be written).
    pub(super) font_file: Option<&'a Path>,
    /// No bold face was found for a bold request: a same-colour border thickens the glyphs.
    pub(super) border: bool,
}

/// The width of the border [`TextLook::border`] draws, and the padding of a box, in pixels.
const TEXT_BORDER_W: u32 = 2;
const TEXT_BOX_BORDER_W: u32 = 12;

impl TextLook<'_> {
    /// `drawtext` options shared by the export and still paths (text, size, color,
    /// font, bold approximation, box) — everything except position / alpha /
    /// enable. `frame_h` is the target canvas height (export height, or the
    /// still's height).
    pub(super) fn options(&self, frame_h: f64) -> Vec<String> {
        self.options_coloured(frame_h, self.color, self.color, self.bg)
    }

    /// [`TextLook::options`] with the colours of the glyphs, the border and the box named: the
    /// rasteriser draws each part white and the others invisible.
    fn options_coloured(&self, frame_h: f64, font: &str, border: &str, boxed: Option<&str>) -> Vec<String> {
        let fontsize = (frame_h * self.size).round().max(1.0);
        let mut parts = vec![
            format!("text='{}'", escape_drawtext_text(self.text)),
            format!("fontsize={}", fnum(fontsize)),
            format!("fontcolor={font}"),
        ];
        if let Some(path) = self.font_file {
            parts.push(format!("fontfile='{}'", escape_drawtext_path(&path.to_string_lossy())));
        }
        if self.border {
            parts.push(format!("borderw={TEXT_BORDER_W}"));
            parts.push(format!("bordercolor={border}"));
        }
        // An invalid box colour drops the box entirely rather than falling back to
        // a visible default — a caller that asked for no readable box colour gets
        // no box, not a random one.
        if let Some(bg) = self.bg.and(boxed) {
            parts.push("box=1".to_string());
            parts.push(format!("boxcolor={bg}"));
            parts.push(format!("boxborderw={TEXT_BOX_BORDER_W}"));
        }
        parts
    }
}

/// `drawtext` options for an overlay of the timeline (see [`TextLook::options`]). The font is the
/// family it names if that is installed, else the shipped default (`crate::fonts`); a bold
/// request the face cannot satisfy is a same-colour border.
fn drawtext_common(o: &TextOverlay, frame_h: f64) -> Vec<String> {
    let font = crate::fonts::resolve_text_font(o.font.as_deref(), o.bold);
    TextLook {
        text: &o.text,
        size: o.size,
        color: safe_color(&o.color, "white"),
        bg: o.bg.as_deref().filter(|bg| valid_color(bg)),
        font_file: font.as_ref().map(|f| f.path.as_path()),
        border: o.bold && !font.as_ref().is_some_and(|f| f.bold),
    }
    .options(frame_h)
}

/// The export-path `drawtext` for an overlay: `enable`-gated to its lifetime,
/// with per-frame position / alpha expressions when it is animated.
pub(super) fn drawtext_export(o: &TextOverlay, fmt: &ExportFormat) -> String {
    let mut parts = drawtext_common(o, fmt.height as f64);
    if o.keyframes.is_empty() {
        parts.push(format!("x=(w*{}-text_w/2)", fnum(o.pos_x)));
        parts.push(format!("y=(h*{}-text_h/2)", fnum(o.pos_y)));
    } else {
        let xs: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.pos_x)).collect();
        let ys: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.pos_y)).collect();
        let al: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.opacity)).collect();
        // Quoted: a piecewise expression contains commas, and an unquoted comma
        // ends the filter as far as the graph parser is concerned.
        parts.push(format!("x='(w*({})-text_w/2)'", keyframe_expr(&xs, "t", o.start)));
        parts.push(format!("y='(h*({})-text_h/2)'", keyframe_expr(&ys, "t", o.start)));
        parts.push(format!("alpha='{}'", keyframe_expr(&al, "t", o.start)));
    }
    parts.push(format!("enable='between(t,{},{})'", fnum(o.start), fnum(o.end)));
    format!("drawtext={}", parts.join(":"))
}

/// The still-path `drawtext` for an overlay active at time `t`: position / alpha
/// sampled to constants (the still pipeline has no timeline clock), no `enable`.
pub(super) fn drawtext_still(o: &TextOverlay, frame_h: u32, t: f64) -> String {
    let mut parts = drawtext_common(o, frame_h as f64);
    let (px, py, op) = o.sample(t);
    parts.push(format!("x=(w*{}-text_w/2)", fnum(px)));
    parts.push(format!("y=(h*{}-text_h/2)", fnum(py)));
    if op < 1.0 {
        parts.push(format!("alpha={}", fnum(op)));
    }
    format!("drawtext={}", parts.join(":"))
}

/// The `drawtext` the still draws for `t`, which is what a plan's overlay stands for: the same
/// options, position and opacity as [`drawtext_still`], taken from the plan's own resolved fields
/// instead of the timeline's (a test holds the two equal).
pub fn text_still_filter(t: &crate::render_plan::PlanText, frame_h: u32) -> String {
    let mut parts = TextLook {
        text: &t.text,
        size: t.size,
        color: &t.color,
        bg: t.bg.as_deref(),
        font_file: t.font_file.as_deref(),
        border: t.synthetic_bold,
    }
    .options(f64::from(frame_h));
    parts.push(format!("x=(w*{}-text_w/2)", fnum(t.pos.0)));
    parts.push(format!("y=(h*{}-text_h/2)", fnum(t.pos.1)));
    if t.alpha < 1.0 {
        parts.push(format!("alpha={}", fnum(t.alpha)));
    }
    format!("drawtext={}", parts.join(":"))
}

/// One piece of an overlay's `drawtext`, drawn on its own: a string of it, in colours of the caller's
/// choosing. The compositor measures and rasterises text a piece at a time (the prefixes of a title, the
/// glyph units of its dictionary) and reads what `drawtext` makes of each; the options are the overlay's
/// own ([`TextLook`]: size, font, border, box) so a piece is laid out and drawn as the overlay is.
#[derive(Debug, Clone, Copy)]
pub struct TextPiece<'a> {
    /// What to draw (cleaned and escaped here, [`text_as_drawn`]).
    pub text: &'a str,
    /// Keep line feeds in `text` as the line breaks they are (one prefix per line), rather than blanks.
    pub lines: bool,
    /// The glyphs' colour.
    pub font: &'a str,
    /// The border's colour, drawn only when the overlay has a synthetic-bold border.
    pub border: Option<&'a str>,
    /// A box behind the text: its colour and its `boxborderw`.
    pub boxed: Option<(&'a str, u32)>,
}

/// The `drawtext` of `piece` in the look of the overlay `t` on a frame `frame_h` high, followed by
/// `extra` options (the position, `y_align`, ...): `drawtext=<options>`, to put in a graph.
pub fn text_piece_filter(t: &crate::render_plan::PlanText, frame_h: u32, piece: &TextPiece<'_>, extra: &[&str]) -> String {
    let fontsize = (f64::from(frame_h) * t.size).round().max(1.0);
    // The colours are spliced unquoted: every one goes through `safe_color`, which leaves a colour ffmpeg parses
    // as it is (the argv is the same) and cannot be made to carry an option of its own. The callers' are
    // constants and probed hex, so an invalid one is a bug: loud in a test, an invisible colour otherwise
    // (and the picture it makes is not the one `drawtext` draws, which the compositor's proof refuses).
    let colour = |c: &str| {
        debug_assert!(valid_color(c), "a text piece colour ffmpeg may not parse: {c:?}");
        safe_color(c, "white@0").to_string()
    };
    let mut parts = vec![
        format!("text='{}'", escape_drawtext_with(piece.text, true, piece.lines)),
        format!("fontsize={}", fnum(fontsize)),
        format!("fontcolor={}", colour(piece.font)),
    ];
    if let Some(path) = t.font_file.as_deref() {
        parts.push(format!("fontfile='{}'", escape_drawtext_path(&path.to_string_lossy())));
    }
    if let (true, Some(border)) = (t.synthetic_bold, piece.border) {
        parts.push(format!("borderw={TEXT_BORDER_W}"));
        parts.push(format!("bordercolor={}", colour(border)));
    }
    if let Some((box_colour, border)) = piece.boxed {
        parts.push("box=1".to_string());
        parts.push(format!("boxcolor={}", colour(box_colour)));
        parts.push(format!("boxborderw={border}"));
    }
    parts.extend(extra.iter().map(|e| (*e).to_string()));
    format!("drawtext={}", parts.join(":"))
}

/// The width of the border a synthetic-bold overlay draws, in pixels.
pub const TEXT_BORDER_PX: u32 = TEXT_BORDER_W;

/// The padding of the box behind an overlay's text, in pixels.
pub const TEXT_BOX_PX: u32 = TEXT_BOX_BORDER_W;

/// The parts [`text_raster_graph`] draws, in the order they are stacked in its one output picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextPart {
    /// The glyphs, as coverage.
    Glyphs,
    /// The box behind the text: 255 where it is.
    Box,
}

/// The `ffmpeg` graph that makes an overlay's coverage masks. The compositor draws text by
/// blending what FFmpeg's own `drawtext` rasterised (layout, hinting, shaping and sub-pixel
/// placement are FFmpeg's, on whatever build this machine has), so it cannot differ from the still.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRasterGraph {
    /// The `-filter_complex` text: its output `[o]` is the `parts` stacked top to bottom, each
    /// `size` big, as 8-bit gray (`rawvideo`).
    pub filter_complex: String,
    pub parts: Vec<TextPart>,
    pub size: (u32, u32),
}

/// The graph that rasterises `t` on a black `size` canvas, **at the position and in the pixel
/// format the still draws it**: the same `drawtext` options and the same `x` / `y` expressions
/// ([`drawtext_still`]), so every pixel of coverage is where the still puts it (glyph bitmaps are
/// rendered at the quarter-pixel phase of a fractional position, so the position is part of the
/// picture, not something to move afterwards).
///
/// Each part is drawn white on black in `rgb24` and its red plane kept: full coverage is 255 and
/// the blend of 255 over 0 at coverage `m` is exactly `m` (a gray or YUV canvas would write 235 on
/// an FFmpeg that ignores the range tag, as 6.1 does). What a part leaves out is drawn invisibly
/// (`@0`), never omitted, so `drawtext` clips to the region the still does: its clip rectangle
/// grows with the box and with the border. A synthetic-bold border is part of the glyphs' picture,
/// drawn white in the same pass `drawtext` draws it in; the compositor takes the picture of the passes
/// as the check of the bitmaps it blends one by one, not as the mask it blends.
pub fn text_raster_graph(t: &crate::render_plan::PlanText, size: (u32, u32)) -> TextRasterGraph {
    let (w, h) = size;
    let look = TextLook {
        text: &t.text,
        size: t.size,
        color: "white",
        bg: t.bg.as_deref(),
        font_file: t.font_file.as_deref(),
        border: t.synthetic_bold,
    };
    let at = format!("x=(w*{}-text_w/2):y=(h*{}-text_h/2)", fnum(t.pos.0), fnum(t.pos.1));
    let mut parts = vec![TextPart::Glyphs];
    if t.bg.is_some() {
        parts.push(TextPart::Box);
    }
    const SHOWN: &str = "white";
    const HIDDEN: &str = "white@0";
    let draws: Vec<String> = parts
        .iter()
        .map(|part| {
            let (font, border, boxed) = match part {
                TextPart::Glyphs => (SHOWN, SHOWN, HIDDEN),
                TextPart::Box => (HIDDEN, HIDDEN, SHOWN),
            };
            let options = look.options_coloured(f64::from(h), font, border, Some(boxed));
            format!("drawtext={}:{at},extractplanes=r", options.join(":"))
        })
        .collect();
    let base = format!("color=c=black:s={w}x{h}:r=1:d=1,format=rgb24");
    let filter_complex = if let [only] = draws.as_slice() {
        format!("{base},{only}[o]")
    } else {
        let n = draws.len();
        let outs: String = (0..n).map(|i| format!("[a{i}]")).collect();
        let branches: Vec<String> = draws.iter().enumerate().map(|(i, d)| format!("[a{i}]{d}[p{i}]")).collect();
        let stacked: String = (0..n).map(|i| format!("[p{i}]")).collect();
        format!("{base},split={n}{outs};{};{stacked}vstack=inputs={n}[o]", branches.join(";"))
    };
    TextRasterGraph {
        filter_complex,
        parts,
        size,
    }
}
