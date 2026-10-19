//! Paragraph layout: font fallback, bidi (UAX #9), shaping (harfrust), line breaking (UAX #14,
//! greedy), letter and word spacing, alignment on an anchor, line spacing and measurement.
//!
//! Ported from EffectCraft's `crates/text/src/layout.rs`, trimmed to what SVG text asks for and set
//! the way usvg sets a text chunk, so that text drawn from its outlines sits where resvg would have
//! drawn it: letter spacing after every cluster but a line's last (and not in cursive scripts), word
//! spacing after word separators, the anchor on the line's advance, a baseline shifted the way
//! `dominant-baseline` asks, and the metric box usvg calls the text's bounding box.
//!
//! Coordinates are user units, y down. The origin is the anchor of the first line's baseline;
//! lines follow one line height apart.

use std::ops::Range;
use std::sync::Arc;

use harfrust::{Direction, Feature, ShapeOptions, ShaperInstance, Tag, UnicodeBuffer};
use unicode_bidi::{Level, ParagraphBidiInfo};
use unicode_script::UnicodeScript;

use super::face::{Face, Metrics, Variation};

/// Where a line sits on its anchor (`text-anchor`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Middle,
    End,
}

/// The baseline a run sits on (`dominant-baseline`), as usvg places it (Chrome's fixed fractions of
/// the face's metrics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Baseline {
    #[default]
    Alphabetic,
    Middle,
    Central,
    Hanging,
    Mathematical,
    TextBeforeEdge,
    TextAfterEdge,
}

impl Baseline {
    /// How far the glyphs move down, in units of the face.
    fn shift(self, m: &Metrics) -> f32 {
        match self {
            Baseline::Alphabetic => 0.0,
            Baseline::Middle => m.x_height * 0.5,
            Baseline::Central => m.ascent - (m.ascent - m.descent) * 0.5,
            Baseline::Hanging => m.ascent * 0.8,
            Baseline::Mathematical => m.ascent * 0.5,
            Baseline::TextBeforeEdge => m.ascent,
            Baseline::TextAfterEdge => m.descent,
        }
    }
}

/// A style run: a byte range of the text and how it is set.
#[derive(Debug, Clone)]
pub struct Run {
    pub range: Range<usize>,
    /// The faces to set it in: the first that has a character sets it (the first sets white space and
    /// what none has). Never empty.
    pub faces: Vec<Arc<Face>>,
    /// Font size, in user units.
    pub size: f32,
    pub variations: Vec<Variation>,
    /// `opsz` follows the size where the face has the axis and `variations` do not set it (CSS
    /// `font-optical-sizing: auto`).
    pub optical_sizing: bool,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub kerning: bool,
    pub baseline: Baseline,
}

/// How lines are made and placed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Paragraph {
    pub align: Align,
    /// Lines wrap to this width; `None` breaks only at a newline.
    pub width: Option<f32>,
    /// Baseline to baseline; `None` is the face's own (ascent, descent and line gap).
    pub line_height: Option<f32>,
}

/// A positioned glyph: `(x, y)` is its origin on the baseline.
#[derive(Debug, Clone)]
pub struct Glyph {
    pub face: Arc<Face>,
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    /// The design-space position it is drawn at.
    pub variations: Arc<[Variation]>,
    /// Byte offset of its cluster in the text.
    pub cluster: usize,
    pub run: usize,
    pub line: usize,
}

/// One laid-out line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Byte range in the text (without the newline that ends it).
    pub range: Range<usize>,
    pub baseline: f32,
    /// Left edge and advance of the line's content (white space at a wrap excluded).
    pub x: f32,
    pub width: f32,
    pub glyphs: Range<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub glyphs: Vec<Glyph>,
    pub lines: Vec<Line>,
    /// The text's bounding box `[x0, y0, x1, y1]` as usvg reckons it: every cluster's advance times
    /// its face's ascent to descent, not the ink. `None` for no text.
    pub bounds: Option<[f32; 4]>,
    /// Characters no face of their run has (drawn as the first face's missing glyph).
    pub missing: Vec<char>,
}

impl Layout {
    /// The widest line's advance.
    pub fn width(&self) -> f32 {
        self.lines.iter().map(|l| l.width).fold(0.0, f32::max)
    }
}

/// Scripts letter spacing does not apply to (CSS Text 3 "cursive tracking"; the list usvg keeps).
fn spaced(c: char) -> bool {
    use unicode_script::Script::*;
    !matches!(
        c.script(),
        Arabic
            | Syriac
            | Nko
            | Manichaean
            | Psalter_Pahlavi
            | Mandaic
            | Mongolian
            | Phags_Pa
            | Devanagari
            | Bengali
            | Gurmukhi
            | Modi
            | Sharada
            | Syloti_Nagri
            | Tirhuta
            | Ogham
    )
}

fn word_separator(c: char) -> bool {
    matches!(c as u32, 0x0020 | 0x00A0 | 0x1361 | 0x010100 | 0x010101 | 0x01039F | 0x01091F)
}

struct Shaped {
    /// The run of its cluster, and the face of that run it is in.
    run: usize,
    face: usize,
    id: u32,
    /// Byte offset in the paragraph.
    cluster: usize,
    adv: f32,
    dx: f32,
    dy: f32,
}

struct Item {
    glyphs: Vec<Shaped>,
}

/// A run's faces resolved for the layout: each with its design-space position and metrics.
struct RunFaces {
    faces: Vec<(Arc<Face>, Arc<[Variation]>, Metrics)>,
}

impl RunFaces {
    fn of(run: &Run) -> Self {
        let faces = run
            .faces
            .iter()
            .map(|f| {
                let v: Arc<[Variation]> = if run.optical_sizing {
                    f.variations_at(&run.variations, run.size).into()
                } else {
                    run.variations.clone().into()
                };
                let m = f.metrics(&v);
                (f.clone(), v, m)
            })
            .collect();
        Self { faces }
    }

    fn scale(&self, face: usize, size: f32) -> f32 {
        size / self.faces[face].0.units_per_em()
    }

    /// The face's own line height at `size`.
    fn line_height(&self, size: f32) -> f32 {
        let k = self.scale(0, size);
        let m = &self.faces[0].2;
        (m.ascent - m.descent + m.leading) * k
    }
}

/// Shape `chars` (byte, character, run, the run's index of the face) in one face, set the same way in all
/// their runs; each glyph is scaled by the size of its cluster's run. Shaping one item across style runs
/// keeps a kerning pair that straddles them, as usvg (which shapes a whole chunk in each face) does.
fn shape(chars: &[(usize, char, usize, usize)], rtl: bool, runs: &[Run], faces: &[RunFaces]) -> Vec<Shaped> {
    let (first, first_face) = (chars[0].2, chars[0].3);
    let (f, variations, _) = &faces[first].faces[first_face];
    let font = f.font();
    let instance = (!variations.is_empty()).then(|| {
        ShaperInstance::from_variations(
            &font,
            variations.iter().map(|(tag, value)| harfrust::Variation {
                tag: Tag::new(tag),
                value: *value,
            }),
        )
    });
    let shaper = f.shaper_data().shaper(&font).instance(instance.as_ref()).build();
    let mut buf = UnicodeBuffer::new();
    for &(b, c, ..) in chars {
        buf.add(c, b as u32);
    }
    buf.set_direction(if rtl { Direction::RightToLeft } else { Direction::LeftToRight });
    buf.guess_segment_properties();
    let features = if runs[first].kerning {
        Vec::new()
    } else {
        vec![Feature::new(Tag::new(b"kern"), 0, ..)]
    };
    let out = shaper.shape(buf, ShapeOptions::new().features(&features));
    let run_of = |cluster: usize| {
        chars
            .iter()
            .find(|c| c.0 == cluster)
            .map_or((first, first_face), |c| (c.2, c.3))
    };
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| {
            let cluster = i.cluster as usize;
            let (run, face) = run_of(cluster);
            let k = faces[run].scale(face, runs[run].size);
            Shaped {
                run,
                face,
                id: i.glyph_id,
                cluster,
                adv: p.x_advance as f32 * k,
                dx: p.x_offset as f32 * k,
                dy: p.y_offset as f32 * k,
            }
        })
        .collect()
}

/// The run covering byte `b` (the last one starting before it past the end).
fn run_at(runs: &[Run], b: usize) -> usize {
    runs.iter()
        .position(|r| r.range.contains(&b))
        .unwrap_or_else(|| runs.iter().rposition(|r| r.range.start <= b).unwrap_or(0))
}

/// One line of a paragraph before it is placed: x from 0 at the line's start, y on its baseline.
struct ParaLine {
    range: Range<usize>,
    glyphs: Vec<Glyph>,
    /// Per cluster: x, advance, ascent, descent (for the bounding box).
    boxes: Vec<[f32; 4]>,
    /// The least and most any run on the line moves its baseline down.
    shifts: (f32, f32),
    width: f32,
    height: f32,
}

/// Lay out one paragraph (no newline) that starts at byte `base` of the whole text.
fn paragraph(
    text: &str,
    base: usize,
    runs: &[Run],
    faces: &[RunFaces],
    width: Option<f32>,
    missing: &mut Vec<char>,
) -> Vec<ParaLine> {
    if text.is_empty() {
        let r = run_at(runs, base);
        return vec![ParaLine {
            range: base..base,
            glyphs: Vec::new(),
            boxes: Vec::new(),
            shifts: (0.0, 0.0),
            width: 0.0,
            height: faces[r].line_height(runs[r].size),
        }];
    }
    let bidi = ParagraphBidiInfo::new(text, Some(Level::ltr()));
    // Per character: byte, char, run, face, right to left.
    let chars: Vec<(usize, char, usize, usize, bool)> = text
        .char_indices()
        .map(|(b, c)| {
            let r = run_at(runs, base + b);
            let face = if c.is_whitespace() || c.is_control() {
                0
            } else {
                match runs[r].faces.iter().position(|f| f.has_char(c)) {
                    Some(i) => i,
                    None => {
                        if !missing.contains(&c) {
                            missing.push(c);
                        }
                        0
                    }
                }
            };
            (b, c, r, face, bidi.levels[b].is_rtl())
        })
        .collect();
    // Items: runs of characters shaped together, in one face set one way (whatever their style runs).
    let shaping = |c: &(usize, char, usize, usize, bool)| {
        let (f, v, _) = &faces[c.2].faces[c.3];
        (Arc::as_ptr(f), v.clone(), runs[c.2].kerning, c.4)
    };
    let mut items: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let k0 = shaping(&chars[i]);
        let mut j = i + 1;
        while j < chars.len() && shaping(&chars[j]) == k0 {
            j += 1;
        }
        let sub: Vec<(usize, char, usize, usize)> = chars[i..j].iter().map(|c| (c.0, c.1, c.2, c.3)).collect();
        items.push(Item {
            glyphs: shape(&sub, chars[i].4, runs, faces),
        });
        i = j;
    }
    // Spacing goes after the last glyph of each cluster; the advance of each cluster, by its byte.
    let mut spacing = vec![0.0f32; text.len() + 1];
    let mut cluster_adv = vec![0.0f32; text.len() + 1];
    for it in &mut items {
        let n = it.glyphs.len();
        for k in 0..n {
            let cl = it.glyphs[k].cluster;
            let last_of_cluster = k + 1 == n || it.glyphs[k + 1].cluster != cl;
            if last_of_cluster {
                let run = &runs[it.glyphs[k].run];
                let c = text[cl..].chars().next().unwrap_or(' ');
                let letter = if spaced(c) { run.letter_spacing } else { 0.0 };
                let word = if word_separator(c) { run.word_spacing } else { 0.0 };
                spacing[cl] += letter;
                it.glyphs[k].adv += letter + word;
            }
            cluster_adv[cl] += it.glyphs[k].adv;
        }
    }
    let range_w = |r: Range<usize>| -> f32 { cluster_adv[r].iter().sum() };
    let trailing_ws = |r: &Range<usize>| -> usize {
        let mut end = r.end;
        for (b, c) in text[r.clone()].char_indices().rev() {
            if !c.is_whitespace() {
                break;
            }
            end = r.start + b;
        }
        end
    };
    // The cluster that ends a line's content (white space at a wrap left out): its letter spacing does
    // not count, as usvg leaves it off a chunk's last cluster.
    let last_cluster = |r: &Range<usize>, wrapped: bool| -> Option<usize> {
        let end = if wrapped { trailing_ws(r) } else { r.end };
        text[r.start..end]
            .char_indices()
            .rev()
            .map(|(b, _)| r.start + b)
            .find(|b| cluster_adv[*b] != 0.0 || spacing[*b] != 0.0)
    };
    let content_w = |r: &Range<usize>, wrapped: bool| -> f32 {
        let end = if wrapped { trailing_ws(r) } else { r.end };
        range_w(r.start..end) - last_cluster(r, wrapped).map_or(0.0, |b| spacing[b])
    };
    let mut ranges: Vec<Range<usize>> = Vec::new();
    match width {
        None => ranges.push(0..text.len()),
        Some(avail) => {
            let mut start = 0usize;
            let mut last_ok: Option<usize> = None;
            for (p, _) in unicode_linebreak::linebreaks(text) {
                loop {
                    let w = content_w(&(start..p), true);
                    if w <= avail || last_ok.is_none() {
                        last_ok = Some(p);
                        break;
                    }
                    let Some(b) = last_ok.take() else { break };
                    ranges.push(start..b);
                    start = b;
                }
            }
            ranges.push(start..text.len());
            ranges.retain(|r| !r.is_empty());
            if ranges.is_empty() {
                ranges.push(0..text.len());
            }
        }
    }
    let nlines = ranges.len();
    let mut out = Vec::new();
    for (li, r) in ranges.into_iter().enumerate() {
        let wrapped = li + 1 < nlines;
        let end_cluster = last_cluster(&r, wrapped);
        let (levels, vruns) = bidi.visual_runs(r.clone());
        let mut pen = 0.0f32;
        let mut glyphs = Vec::new();
        let mut boxes: Vec<[f32; 4]> = Vec::new();
        let mut height = 0.0f32;
        let mut shifts = (f32::INFINITY, f32::NEG_INFINITY);
        let mut seen_runs: Vec<usize> = Vec::new();
        for vrun in vruns {
            let rtl = levels[vrun.start].is_rtl();
            let mut idx: Vec<usize> = (0..items.len())
                .filter(|&k| {
                    let it = &items[k];
                    it.glyphs.iter().any(|g| vrun.contains(&g.cluster))
                })
                .collect();
            if rtl {
                idx.reverse();
            }
            for k in idx {
                let mut prev: Option<usize> = None;
                for g in items[k].glyphs.iter().filter(|g| vrun.contains(&g.cluster)) {
                    let run = &runs[g.run];
                    let rf = &faces[g.run];
                    if !seen_runs.contains(&g.run) {
                        seen_runs.push(g.run);
                        height = height.max(rf.line_height(run.size));
                    }
                    let (face, variations, metrics) = &rf.faces[g.face];
                    let k = rf.scale(g.face, run.size);
                    let shift = run.baseline.shift(&rf.faces[0].2) * rf.scale(0, run.size);
                    shifts = (shifts.0.min(shift), shifts.1.max(shift));
                    if prev != Some(g.cluster) {
                        let (asc, desc) = (metrics.ascent * k, metrics.descent * k);
                        let mut adv = cluster_adv[g.cluster];
                        if end_cluster == Some(g.cluster) {
                            adv -= spacing[g.cluster];
                        }
                        boxes.push([pen, adv, asc, desc]);
                        prev = Some(g.cluster);
                    }
                    glyphs.push(Glyph {
                        face: face.clone(),
                        id: g.id,
                        x: pen + g.dx,
                        y: shift - g.dy,
                        size: run.size,
                        variations: variations.clone(),
                        cluster: base + g.cluster,
                        run: g.run,
                        line: 0,
                    });
                    pen += g.adv;
                }
            }
        }
        out.push(ParaLine {
            range: base + r.start..base + r.end,
            width: content_w(&r, wrapped),
            glyphs,
            boxes,
            shifts: if shifts.0 <= shifts.1 { shifts } else { (0.0, 0.0) },
            height,
        });
    }
    out
}

/// Lay out `text` (lines broken at `\n`) in `runs` (byte ranges covering it in order, at least one).
pub fn layout(text: &str, runs: &[Run], para: &Paragraph) -> Layout {
    let mut lay = Layout::default();
    if runs.is_empty() || runs.iter().any(|r| r.faces.is_empty()) {
        return lay;
    }
    let faces: Vec<RunFaces> = runs.iter().map(RunFaces::of).collect();
    let mut lines: Vec<ParaLine> = Vec::new();
    let mut start = 0;
    for part in text.split('\n') {
        lines.extend(paragraph(part, start, runs, &faces, para.width, &mut lay.missing));
        start += part.len() + 1;
    }
    let mut baseline = 0.0f32;
    let mut bounds: Option<[f32; 4]> = None;
    for (li, pl) in lines.into_iter().enumerate() {
        if li > 0 {
            baseline += para.line_height.unwrap_or(pl.height);
        }
        let shift = match para.align {
            Align::Start => 0.0,
            Align::Middle => -pl.width / 2.0,
            Align::End => -pl.width,
        };
        let g0 = lay.glyphs.len();
        lay.glyphs.extend(pl.glyphs.into_iter().map(|mut g| {
            g.x += shift;
            g.y += baseline;
            g.line = li;
            g
        }));
        // usvg measures every cluster of a chunk at the baseline of each of its spans in turn.
        let (up, down) = pl.shifts;
        for [x, adv, asc, desc] in pl.boxes {
            let adv = if adv <= 0.0 { 1.0 } else { adv };
            let r = [x + shift, baseline + up - asc, x + shift + adv, baseline + down - desc];
            bounds = Some(match bounds {
                None => r,
                Some(b) => [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])],
            });
        }
        lay.lines.push(Line {
            range: pl.range,
            baseline,
            x: shift,
            width: pl.width,
            glyphs: g0..lay.glyphs.len(),
        });
    }
    lay.bounds = bounds;
    lay
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEIST: &[u8] = include_bytes!("../../assets/fonts/Geist-Regular.ttf");

    fn geist() -> Arc<Face> {
        Face::new(Arc::new(GEIST), 0).unwrap()
    }

    fn run(range: Range<usize>, size: f32) -> Run {
        Run {
            range,
            faces: vec![geist()],
            size,
            variations: Vec::new(),
            optical_sizing: true,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            kerning: true,
            baseline: Baseline::Alphabetic,
        }
    }

    fn set(text: &str, para: Paragraph) -> Layout {
        layout(text, &[run(0..text.len(), 20.0)], &para)
    }

    #[test]
    fn lines_wrap_greedily_at_the_width_and_break_at_newlines() {
        let one = set("one two three four five six", Paragraph::default());
        assert_eq!(one.lines.len(), 1);
        let w = one.width();
        let wrapped = set(
            "one two three four five six",
            Paragraph {
                width: Some(w / 2.5),
                ..Paragraph::default()
            },
        );
        assert!(wrapped.lines.len() >= 3, "{:?}", wrapped.lines);
        for line in &wrapped.lines {
            assert!(line.width <= w / 2.5 + 0.01, "{line:?}");
        }
        let text: Vec<&str> = wrapped
            .lines
            .iter()
            .map(|l| "one two three four five six"[l.range.clone()].trim_end())
            .collect();
        assert_eq!(text.join(" "), "one two three four five six");
        // A word longer than the width has a line of its own rather than vanishing.
        let narrow = set(
            "a extraordinarily b",
            Paragraph {
                width: Some(10.0),
                ..Paragraph::default()
            },
        );
        assert_eq!(narrow.lines.len(), 3);
        let broken = set("first\n\nthird", Paragraph::default());
        assert_eq!(broken.lines.len(), 3);
        assert!(broken.lines[1].glyphs.is_empty());
        assert!(broken.lines[2].baseline > broken.lines[1].baseline && broken.lines[1].baseline > 0.0);
        let spaced = set(
            "a\nb",
            Paragraph {
                line_height: Some(50.0),
                ..Paragraph::default()
            },
        );
        assert_eq!(spaced.lines[1].baseline, 50.0);
    }

    #[test]
    fn kerning_and_spacing_move_the_glyphs() {
        let text = "AVAVAV";
        let kerned = layout(text, &[run(0..6, 40.0)], &Paragraph::default());
        let mut plain_run = run(0..6, 40.0);
        plain_run.kerning = false;
        let plain = layout(text, &[plain_run], &Paragraph::default());
        assert!(kerned.width() < plain.width(), "{} {}", kerned.width(), plain.width());
        let mut wide = run(0..6, 40.0);
        wide.letter_spacing = 10.0;
        let wide = layout(text, &[wide], &Paragraph::default());
        // Spacing after every cluster but the last.
        assert!((wide.width() - kerned.width() - 50.0).abs() < 0.01);
    }

    #[test]
    fn a_line_sits_on_its_anchor() {
        let start = set("centre", Paragraph::default());
        let w = start.width();
        assert_eq!(start.lines[0].x, 0.0);
        let middle = set(
            "centre",
            Paragraph {
                align: Align::Middle,
                ..Paragraph::default()
            },
        );
        assert!((middle.lines[0].x + w / 2.0).abs() < 0.01);
        assert!((middle.glyphs[0].x - start.glyphs[0].x + w / 2.0).abs() < 0.01);
        let end = set(
            "centre",
            Paragraph {
                align: Align::End,
                ..Paragraph::default()
            },
        );
        assert!((end.lines[0].x + w).abs() < 0.01);
        let [x0, y0, x1, y1] = start.bounds.unwrap();
        assert!(x0 <= 0.0 && x1 >= w && y0 < -10.0 && y1 > 0.0, "{:?}", start.bounds);
    }

    #[test]
    fn runs_set_their_own_size_and_a_character_no_face_has_is_reported() {
        let text = "ab";
        let lay = layout(text, &[run(0..1, 10.0), run(1..2, 30.0)], &Paragraph::default());
        assert_eq!(lay.glyphs.len(), 2);
        assert_eq!((lay.glyphs[0].size, lay.glyphs[1].size), (10.0, 30.0));
        assert_eq!((lay.glyphs[0].run, lay.glyphs[1].run), (0, 1));
        let missing = layout("a\u{10FFFD}", &[run(0..5, 10.0)], &Paragraph::default());
        assert_eq!(missing.missing, vec!['\u{10FFFD}']);
        assert!(layout("", &[run(0..0, 10.0)], &Paragraph::default()).bounds.is_none());
    }

    #[test]
    fn outlines_are_compact_path_data_at_the_glyph_origin() {
        let lay = set("H", Paragraph::default());
        let mut g = lay.glyphs[0].clone();
        g.x += 100.0;
        g.y += 50.0;
        let mut d = String::new();
        assert!(super::super::glyph_path(&g, &mut d));
        assert!(d.starts_with('M'), "{d}");
        let first: Vec<f32> = d[1..]
            .split(['l', 'z', 'm'])
            .next()
            .unwrap()
            .split(' ')
            .map(|n| n.parse().unwrap())
            .collect();
        assert!(
            first[0] > 100.0 && first[0] < 120.0 && first[1] <= 50.0 && first[1] > 30.0,
            "{d}"
        );
        let space = set(" ", Paragraph::default());
        let mut d = String::new();
        assert!(!super::super::glyph_path(&space.glyphs[0], &mut d));
    }
}
