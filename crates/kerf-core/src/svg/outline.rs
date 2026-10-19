//! Text set as glyph outlines: a `<text>` element becomes the paths of its glyphs, so the title is drawn
//! without the fonts it names (motion templates, phase 2).
//!
//! **The form is reversible.** An outlined text is the same element renamed `<g>`, marked
//! `data-kerf-text=""`, with every attribute, text run, `<tspan>` and animation it had still in it (none
//! of them draws inside a `<g>`), and one generated child at its end, `<g data-kerf-glyphs="">`, holding
//! an invisible path of the text's bounding box and one `<path>` per glyph. [`restore_text`] renames the
//! element back and drops the generated child, byte for byte the text it was, so a template's controls
//! read and set the text as before and the text is set again after every change. The `font-family` stays
//! in the source, which keeps the fonts in the project while the text can still be set again.
//!
//! **It draws what resvg drew.** The engine (`crate::text`) sets the text the way usvg sets one chunk, the
//! glyphs inherit the element's paint (a `<tspan>`'s own paint goes on a `<g>` around its glyphs), the
//! box path gives the group the bounding box usvg gives the text (a filter or mask region measured on it
//! stays put), and animations of the element's paint, opacity, visibility or transform drive the group
//! as they drove the text. It adds what usvg lacks: `inline-size` wraps lines (SVG 2), or with
//! `data-kerf-fit="shrink"` sets the text on one line made smaller until it fits (everything that follows
//! the font size scaled together), and `white-space: pre-line` breaks lines at a newline.
//!
//! **Anything else stays text**, drawn by resvg with its font as before: positioned or nested `<tspan>`s,
//! `textPath`, `dx` / `dy` / `rotate` / `textLength`, vertical or right-to-left text, decorations, font
//! features and variation settings, a gradient or pattern paint measured on the bounding box (each glyph
//! would be its own; one in `userSpaceOnUse` is drawn the same), dashes, an animation of anything that moves the glyphs, a `<style>` rule that would match
//! differently once the element is a `<g>` (or match a generated element), a character no font has, a
//! colour glyph, or a result over the title's size cap.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;

use fontdb::{Database, Family, Query, Stretch, Style, Weight, ID};
use roxmltree::{Document, Node, NodeId, NodeType};
use skrifa::MetadataProvider;

use super::glyph_smil::{self, IdSource, UnitGeom};
use super::sanitize::{apply, escape_attr, nesting_exceeds, SVG_NS, XLINK_NS};
use super::text_animation::{AnimateBy, TextAnimation, ANIMATE_MARK};
use super::{invalid, xml_options, MAX_ANIMATIONS, MAX_BYTES, MAX_DEPTH};
use crate::error::Result;
use crate::fonts;
use crate::text::{self, Align, Baseline, Face, Paragraph, Run, Variation};

/// The mark of a `<text>` set as outlines.
pub(crate) const TEXT_MARK: &str = "data-kerf-text";
/// The mark of the generated group of glyphs inside it.
pub(crate) const GLYPHS_MARK: &str = "data-kerf-glyphs";
/// How a text with an `inline-size` fits its box: absent (or `wrap`) its lines wrap, `shrink` sets it on
/// one line (and at each newline) and makes it smaller until it fits. A text drawn with its font ignores it.
pub(crate) const FIT_MARK: &str = "data-kerf-fit";

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
/// usvg's defaults: `font-size` and the resolution of absolute units.
const FONT_SIZE: f32 = 12.0;
const DPI: f32 = 96.0;
/// Most fallback faces one text takes.
const MAX_FALLBACKS: usize = 8;
/// Most searches of the whole font database for a fallback in one document (each reads every face's
/// character map).
const MAX_SEARCHES: usize = 16;
/// Most `<text>` elements of one document set as outlines; the rest stay text.
const MAX_TEXTS: usize = 256;
/// How far from the origin a glyph may be set, in user units.
const MAX_COORD: f32 = 1e6;
/// The fewest bytes a glyph's path takes (`<path d="M0 0h1v1h-1z"/>`).
const MIN_GLYPH_BYTES: usize = 24;
/// The fewest bytes the group of an animated unit takes beyond its paths.
const MIN_UNIT_BYTES: usize = 40;
/// Most units (characters, words or lines) of one text an animation is compiled for.
const MAX_UNITS: usize = 600;
/// Most amounts a document's animations are sampled at in all (a unit a grid of 120 a second): about a tenth of a second.
const MAX_WORK: usize = 3_000_000;

/// What setting a document's text as outlines did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Outlined {
    pub(crate) svg: String,
    /// How many `<text>` elements are outlines now.
    pub(crate) count: usize,
    pub(crate) warnings: Vec<String>,
    /// Each text that stays text and why ("the text “t” (a <tspan> with dy)").
    pub(crate) kept: Vec<String>,
}

/// Properties whose animation moves glyphs: a text driven by one (on itself or an ancestor) stays text.
const LAYOUT_PROPS: &[&str] = &[
    "font",
    "font-family",
    "font-size",
    "font-size-adjust",
    "font-weight",
    "font-style",
    "font-stretch",
    "font-variant",
    "font-kerning",
    "font-feature-settings",
    "font-variation-settings",
    "font-optical-sizing",
    "kerning",
    "letter-spacing",
    "word-spacing",
    "text-anchor",
    "dominant-baseline",
    "alignment-baseline",
    "baseline-shift",
    "white-space",
    "inline-size",
    "line-height",
    "direction",
    "writing-mode",
    "unicode-bidi",
    "text-decoration",
    "text-rendering",
    "space",
];

/// What an animation of the text element itself may drive.
const ANIMATED_OK: &[&str] = &[
    "opacity",
    "fill",
    "fill-opacity",
    "stroke",
    "stroke-opacity",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dashoffset",
    "visibility",
    "display",
    "color",
    "transform",
];

/// A `<tspan>`'s paint, carried onto the group of its glyphs.
const SPAN_PAINT: &[&str] = &[
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dashoffset",
    "paint-order",
    "visibility",
    "color",
];

/// What may be said of a `<tspan>` (beyond its paint and the inherited text properties, which are read):
/// anything else that draws is not carried.
const SPAN_REFUSED: &[&str] = &[
    "x",
    "y",
    "dx",
    "dy",
    "rotate",
    "textLength",
    "lengthAdjust",
    "opacity",
    "transform",
    "filter",
    "mask",
    "clip-path",
    "display",
];

// ---- restoring ------------------------------------------------------------------------------------

/// `src` with every outlined text back as the `<text>` it was. A document with none comes back as it is.
/// The source may be a stored file's, not yet sanitised: one over the size or nesting cap is refused
/// before it is parsed (roxmltree recurses once per open element).
pub fn restore_text(src: &str) -> Result<String> {
    if !src.contains(TEXT_MARK) {
        return Ok(src.to_string());
    }
    if src.len() > MAX_BYTES {
        return Err(invalid(format!("the SVG is larger than {} KiB", MAX_BYTES / 1024)));
    }
    if nesting_exceeds(src, MAX_DEPTH) {
        return Err(invalid(format!("the SVG is nested deeper than {MAX_DEPTH} elements")));
    }
    let doc = Document::parse_with_options(src, xml_options()).map_err(|e| invalid(format!("not valid XML: {e}")))?;
    let head = format!("<g {TEXT_MARK}=\"\"");
    let mut cuts = Vec::new();
    for node in doc.descendants().filter(|n| is_marked(n)) {
        let r = node.range();
        if !src[r.start..].starts_with(&head) {
            continue;
        }
        let Some(close) = src[r.clone()].rfind("</").map(|i| r.start + i) else {
            continue;
        };
        if src[close + 2..r.end].trim_end_matches('>').trim_end() != "g" {
            continue;
        }
        cuts.push((r.start..r.start + head.len(), "<text".to_string()));
        for child in node
            .children()
            .filter(|c| c.is_element() && c.attribute(GLYPHS_MARK).is_some())
        {
            cuts.push((child.range(), String::new()));
        }
        cuts.push((close + 2..close + 3, "text".to_string()));
    }
    Ok(apply(src, cuts))
}

fn is_marked(n: &Node) -> bool {
    n.is_element() && n.tag_name().name() == "g" && n.tag_name().namespace() == Some(SVG_NS) && n.attribute(TEXT_MARK).is_some()
}

// ---- the style cascade ----------------------------------------------------------------------------

/// An element as a selector sees it, before or after the text is renamed, or one of the generated ones.
#[derive(Clone, Copy)]
enum El<'a, 'i> {
    /// A real element; `renamed` is the text being outlined, which reads as a `<g>`.
    Real(Node<'a, 'i>, Option<NodeId>),
    /// A generated element inside the text `parent` (`depth` 0 is the group of glyphs, 1 a path or a
    /// span's group in it, 2 a path in that).
    Made(&'static str, u8, Node<'a, 'i>),
    /// A generated element of an animated text's units (`glyph_smil.rs`): the group of a unit (1; a span's
    /// group inside it 2), the paths in them (2, 3), a blur's `filter` (2) with its `feGaussianBlur` (3) and the
    /// `animate` in that (4), and the unit's own animations (2). Its parent follows from the tag and depth.
    Anim(&'static str, u8, Node<'a, 'i>),
}

/// The generated elements of an animated text a rule could match, as `(tag, depth)` of [`El::Anim`].
const ANIMATED_MADE: &[(&str, u8)] = &[
    ("g", 1),
    ("g", 2),
    ("path", 2),
    ("path", 3),
    ("filter", 2),
    ("feGaussianBlur", 3),
    ("animate", 2),
    ("animate", 4),
    ("animateTransform", 2),
];

/// The tag and depth of the parent of a generated element of an animated text (below depth 1).
fn anim_parent(tag: &str, depth: u8) -> (&'static str, u8) {
    match (tag, depth) {
        ("feGaussianBlur", 3) => ("filter", 2),
        ("animate", 4) => ("feGaussianBlur", 3),
        (_, d) => ("g", d - 1),
    }
}

impl simplecss::Element for El<'_, '_> {
    fn parent_element(&self) -> Option<Self> {
        match *self {
            El::Real(n, renamed) => n.parent_element().map(|p| El::Real(p, renamed)),
            El::Made(_, 0, text) => Some(El::Real(text, Some(text.id()))),
            El::Made(_, depth, text) => Some(El::Made("g", depth - 1, text)),
            El::Anim(_, 1, text) => Some(El::Made("g", 0, text)),
            El::Anim(tag, depth, text) => {
                let (parent, d) = anim_parent(tag, depth);
                Some(El::Anim(parent, d, text))
            }
        }
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        match *self {
            El::Real(n, renamed) => n.prev_sibling_element().map(|p| El::Real(p, renamed)),
            El::Made(_, 0, text) => text.last_element_child().map(|c| El::Real(c, Some(text.id()))),
            El::Made(..) | El::Anim(..) => None,
        }
    }

    fn has_local_name(&self, name: &str) -> bool {
        match *self {
            El::Real(n, renamed) if renamed == Some(n.id()) => name == "g",
            El::Real(n, _) => n.tag_name().name() == name,
            El::Made(made, ..) | El::Anim(made, ..) => made == name,
        }
    }

    fn attribute_matches(&self, name: &str, operator: simplecss::AttributeOperator) -> bool {
        match *self {
            El::Real(n, renamed) => match n.attribute(name) {
                Some(v) => operator.matches(v),
                None => renamed == Some(n.id()) && name == TEXT_MARK && operator.matches(""),
            },
            El::Made(_, 0, _) => name == GLYPHS_MARK && operator.matches(""),
            // A span's group carries its paint, a path its data (and the box path `fill` / `stroke`):
            // whatever their values, a selector on them is taken to match.
            El::Made("g", ..) => SPAN_PAINT.contains(&name),
            El::Made(..) => matches!(name, "d" | "fill" | "stroke"),
            // Whatever the animation wrote there: a selector on any attribute is taken to match.
            El::Anim(..) => true,
        }
    }

    fn pseudo_class_matches(&self, class: simplecss::PseudoClass) -> bool {
        matches!(class, simplecss::PseudoClass::FirstChild) && self.prev_sibling_element().is_none()
    }
}

type Declared = Rc<HashMap<String, String>>;

/// Every element's declared properties, as usvg reads them: presentation attributes, then the rules of
/// the `<style>` sheets in order of specificity (an `!important` one kept against a later plain one),
/// then the `style` attribute.
struct Cascade<'a> {
    sheet: simplecss::StyleSheet<'a>,
    cache: RefCell<HashMap<NodeId, Declared>>,
}

impl<'a> Cascade<'a> {
    fn new(doc: &'a Document<'a>) -> Self {
        let mut sheet = simplecss::StyleSheet::new();
        for node in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "style") {
            if !matches!(node.attribute("type"), None | Some("text/css")) {
                continue;
            }
            if let Some(text) = node.text() {
                sheet.parse_more(text);
            }
        }
        Self {
            sheet,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn declared(&self, node: Node) -> Declared {
        if let Some(d) = self.cache.borrow().get(&node.id()) {
            return d.clone();
        }
        let mut map: HashMap<String, (String, bool)> = HashMap::new();
        let mut put = |name: &str, value: &str, important: bool| {
            if let Some((_, true)) = map.get(name) {
                if !important {
                    return;
                }
            }
            map.insert(name.to_string(), (value.trim().to_string(), important));
        };
        for a in node.attributes() {
            match a.namespace() {
                None if !matches!(a.name(), "style" | "class" | "font-kerning" | "mix-blend-mode" | "isolation") => {
                    put(a.name(), a.value(), false);
                }
                Some(XML_NS) if a.name() == "space" => put("space", a.value(), false),
                _ => {}
            }
        }
        let geometry = |name: &str| {
            matches!(
                name,
                "x" | "y" | "dx" | "dy" | "rotate" | "textLength" | "lengthAdjust" | "id"
            )
        };
        for rule in &self.sheet.rules {
            if rule.selector.matches(&El::Real(node, None)) {
                for d in rule.declarations.iter().filter(|d| !geometry(d.name)) {
                    put(d.name, d.value, d.important);
                }
            }
        }
        if let Some(style) = node.attribute("style") {
            for d in simplecss::DeclarationTokenizer::from(style).filter(|d| !geometry(d.name)) {
                put(d.name, d.value, d.important);
            }
        }
        let d: Declared = Rc::new(map.into_iter().map(|(k, (v, _))| (k, v)).collect());
        self.cache.borrow_mut().insert(node.id(), d.clone());
        d
    }

    /// The element whose value of `prop` applies to `node`, and the value: for an inherited property the
    /// nearest of it and its ancestors that says one, otherwise it or its parent (usvg's rule). `inherit`
    /// looks further up.
    fn find<'n, 'i>(&self, node: Node<'n, 'i>, prop: &str, inherited: bool) -> Option<(Node<'n, 'i>, String)> {
        for (steps, n) in node.ancestors().filter(Node::is_element).enumerate() {
            if let Some(v) = self.declared(n).get(prop) {
                if v != "inherit" {
                    return Some((n, v.clone()));
                }
            } else if !inherited && steps >= 1 {
                return None;
            }
        }
        None
    }

    fn get(&self, node: Node, prop: &str, inherited: bool) -> Option<String> {
        self.find(node, prop, inherited).map(|(_, v)| v)
    }

    /// Whether a rule matches `node` differently once `text` is a `<g>`.
    fn moves(&self, node: Node, text: NodeId) -> bool {
        !self.sheet.rules.is_empty()
            && self
                .sheet
                .rules
                .iter()
                .any(|r| r.selector.matches(&El::Real(node, None)) != r.selector.matches(&El::Real(node, Some(text))))
    }

    /// Whether a rule matches a generated element of `text` (`animated`: of its units too).
    fn touches_made(&self, text: Node, animated: bool) -> bool {
        let made = [
            El::Made("g", 0, text),
            El::Made("path", 1, text),
            El::Made("g", 1, text),
            El::Made("path", 2, text),
        ];
        let units = ANIMATED_MADE.iter().map(|&(tag, depth)| El::Anim(tag, depth, text));
        self.sheet
            .rules
            .iter()
            .any(|r| made.iter().any(|m| r.selector.matches(m)) || (animated && units.clone().any(|m| r.selector.matches(&m))))
    }

    /// `font-size` at `node` (usvg's: from the root down, relative units on the size above).
    fn font_size(&self, node: Node) -> f32 {
        let chain: Vec<Node> = node.ancestors().filter(Node::is_element).collect();
        let mut size = FONT_SIZE;
        for n in chain.iter().rev() {
            let Some(v) = self.declared(*n).get("font-size").cloned() else {
                continue;
            };
            if let Ok(len) = svgtypes::Length::from_str(&v) {
                let x = len.number as f32;
                size = match len.unit {
                    svgtypes::LengthUnit::Em => x * size,
                    svgtypes::LengthUnit::Ex => x * size / 2.0,
                    svgtypes::LengthUnit::Percent => x * size * 0.01,
                    unit => absolute(x, unit).unwrap_or(size),
                };
            } else {
                let step = match v.as_str() {
                    "xx-small" => -3,
                    "x-small" => -2,
                    "small" | "smaller" => -1,
                    "large" | "larger" => 1,
                    "x-large" => 2,
                    "xx-large" => 3,
                    _ => 0,
                };
                size *= 1.2f32.powi(step);
            }
        }
        size
    }

    /// `font-weight` at `node` (usvg's: from the root down, `bolder` and `lighter` on the weight above).
    fn font_weight(&self, node: Node) -> u16 {
        let chain: Vec<Node> = node.ancestors().filter(Node::is_element).collect();
        let mut weight: i32 = 400;
        for n in chain.iter().rev() {
            let Some(v) = self.declared(*n).get("font-weight").cloned() else {
                continue;
            };
            weight = match v.as_str() {
                "normal" => 400,
                "bold" => 700,
                "bolder" => (weight + if weight == 400 { 300 } else { 100 }).clamp(100, 900),
                "lighter" => (weight - if weight == 400 { 200 } else { 100 }).clamp(100, 900),
                n => match n.parse::<i32>() {
                    Ok(w) if (1..=9).contains(&(w / 100)) && w % 100 == 0 => w,
                    _ => weight,
                },
            };
        }
        weight as u16
    }

    /// A length property at `node` in user units (`em` on the font size where it is said).
    fn length(&self, node: Node, prop: &str, inherited: bool) -> std::result::Result<Option<f32>, String> {
        let Some((at, v)) = self.find(node, prop, inherited) else {
            return Ok(None);
        };
        if v == "normal" || v == "auto" {
            return Ok(None);
        }
        length_at(&v, || self.font_size(at))
            .map(Some)
            .ok_or_else(|| format!("{prop} “{v}”"))
    }
}

fn absolute(x: f32, unit: svgtypes::LengthUnit) -> Option<f32> {
    use svgtypes::LengthUnit::*;
    Some(match unit {
        None | Px => x,
        In => x * DPI,
        Cm => x * DPI / 2.54,
        Mm => x * DPI / 25.4,
        Pt => x * DPI / 72.0,
        Pc => x * DPI / 6.0,
        _ => return Option::None,
    })
}

fn length_at(v: &str, font_size: impl Fn() -> f32) -> Option<f32> {
    let len = svgtypes::Length::from_str(v).ok()?;
    let x = len.number as f32;
    let out = match len.unit {
        svgtypes::LengthUnit::Em => x * font_size(),
        svgtypes::LengthUnit::Ex => x * font_size() / 2.0,
        svgtypes::LengthUnit::Percent => return None,
        unit => absolute(x, unit)?,
    };
    out.is_finite().then_some(out)
}

// ---- animations -----------------------------------------------------------------------------------

struct Animation {
    target: NodeId,
    attr: String,
    url: bool,
}

/// Every element by `id`, the first of a repeated one (as usvg resolves a reference).
fn by_id<'a>(doc: &'a Document) -> HashMap<&'a str, NodeId> {
    doc.descendants()
        .filter(Node::is_element)
        .filter_map(|n| Some((n.attribute("id")?, n.id())))
        .rev()
        .collect()
}

/// The element `n`'s `href` (or `xlink:href`) names in the document.
fn href_target(n: &Node, by_id: &HashMap<&str, NodeId>) -> Option<NodeId> {
    let href = n.attribute("href").or_else(|| n.attribute((XLINK_NS, "href")))?;
    by_id.get(href.trim().strip_prefix('#')?).copied()
}

/// What each `<use>` instances. A text there, or inside it, stays text: an instance takes its inherited
/// properties from the `<use>`, and a `<g>` instanced in a `<clipPath>` clips nothing.
fn use_targets(doc: &Document) -> Vec<NodeId> {
    let by_id = by_id(doc);
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "use")
        .filter_map(|n| href_target(&n, &by_id))
        .collect()
}

fn animations(doc: &Document) -> Vec<Animation> {
    let by_id = by_id(doc);
    doc.descendants()
        .filter(|n| {
            n.is_element()
                && matches!(
                    n.tag_name().name(),
                    "animate" | "set" | "animateColor" | "animateTransform" | "animateMotion"
                )
        })
        .filter_map(|n| {
            let linked = n.attribute("href").is_some() || n.attribute((XLINK_NS, "href")).is_some();
            let target = match linked {
                true => href_target(&n, &by_id)?,
                false => n.parent_element()?.id(),
            };
            let attr = match n.tag_name().name() {
                "animateTransform" => "transform".to_string(),
                "animateMotion" => "motion".to_string(),
                _ => {
                    let a = n.attribute("attributeName")?.trim();
                    a.rsplit(':').next().unwrap_or(a).to_string()
                }
            };
            let url = ["values", "from", "to", "by"]
                .iter()
                .filter_map(|k| n.attribute(*k))
                .any(|v| v.to_ascii_lowercase().contains("url("));
            Some(Animation { target, attr, url })
        })
        .collect()
}

// ---- fonts ----------------------------------------------------------------------------------------

/// Faces read from the database, by id, for one document, and the fallbacks it looked for.
struct Faces<'d> {
    db: &'d Database,
    read: HashMap<ID, Option<Arc<Face>>>,
    fallbacks: HashMap<(char, ID), Option<ID>>,
}

impl Faces<'_> {
    fn get(&mut self, id: ID) -> Option<Arc<Face>> {
        let db = self.db;
        self.read.entry(id).or_insert_with(|| Face::from_db(db, id)).clone()
    }

    /// A face for `c` beyond `used` (usvg's fallback: the first face of the database that maps it and
    /// shares the style, weight or stretch of `base`).
    fn fallback(&mut self, c: char, base: ID, used: &[ID]) -> Option<ID> {
        if let Some(known) = self.fallbacks.get(&(c, base)) {
            return known.filter(|id| !used.contains(id));
        }
        if self.fallbacks.len() >= MAX_SEARCHES {
            return None;
        }
        let base_face = self.db.face(base)?;
        let found = self
            .db
            .faces()
            .filter(|f| !used.contains(&f.id))
            .filter(|f| !(base_face.style != f.style && base_face.weight != f.weight && base_face.stretch != f.stretch))
            .find(|f| {
                self.db
                    .with_face_data(f.id, |data, index| {
                        skrifa::FontRef::from_index(data, index)
                            .ok()
                            .and_then(|font| font.charmap().map(c))
                            .is_some_and(|g| g.to_u32() != 0)
                    })
                    .unwrap_or(false)
            })
            .map(|f| f.id);
        self.fallbacks.insert((c, base), found);
        found
    }
}

/// How one owner of text (the `<text>` or a `<tspan>`) is set.
struct Spec {
    families: Vec<svgtypes::FontFamily>,
    weight: u16,
    style: Style,
    stretch: Stretch,
    size: f32,
    letter_spacing: f32,
    word_spacing: f32,
    kerning: bool,
    opsz: bool,
    baseline: Baseline,
}

impl Spec {
    fn variations(&self) -> Vec<Variation> {
        let mut v: Vec<Variation> = vec![(*b"wght", f32::from(self.weight))];
        let wdth = match self.stretch {
            Stretch::UltraCondensed => 50.0,
            Stretch::ExtraCondensed => 62.5,
            Stretch::Condensed => 75.0,
            Stretch::SemiCondensed => 87.5,
            Stretch::Normal => 100.0,
            Stretch::SemiExpanded => 112.5,
            Stretch::Expanded => 125.0,
            Stretch::ExtraExpanded => 150.0,
            Stretch::UltraExpanded => 200.0,
        };
        if wdth != 100.0 {
            v.push((*b"wdth", wdth));
        }
        match self.style {
            Style::Italic => v.push((*b"ital", 1.0)),
            Style::Oblique => v.push((*b"slnt", -12.0)),
            Style::Normal => {}
        }
        v
    }

    fn query(&self, db: &Database) -> Option<ID> {
        let mut names: Vec<Family> = self
            .families
            .iter()
            .map(|f| match f {
                svgtypes::FontFamily::Serif => Family::Serif,
                svgtypes::FontFamily::SansSerif => Family::SansSerif,
                svgtypes::FontFamily::Cursive => Family::Cursive,
                svgtypes::FontFamily::Fantasy => Family::Fantasy,
                svgtypes::FontFamily::Monospace => Family::Monospace,
                svgtypes::FontFamily::Named(s) => Family::Name(s),
            })
            .collect();
        names.push(Family::Serif);
        db.query(&Query {
            families: &names,
            weight: Weight(self.weight),
            stretch: self.stretch,
            style: self.style,
        })
    }
}

/// Whether a `url(#id)` paint is drawn the same on each glyph's path as on the text: a gradient or a
/// pattern laid out in user space (its own `gradientUnits`, or `patternUnits` and `patternContentUnits`,
/// or the first an `href` chain gives). One measured on the bounding box would be measured on each glyph.
fn user_space_paint(node: Node, value: &str) -> bool {
    let Ok(svgtypes::Paint::FuncIRI(id, _)) = svgtypes::Paint::from_str(value) else {
        return false;
    };
    let doc = node.document();
    let find = |id: &str| doc.descendants().find(|n| n.is_element() && n.attribute("id") == Some(id));
    let Some(mut at) = find(id) else {
        return false;
    };
    let kind = at.tag_name().name();
    let names: &[&str] = match kind {
        "linearGradient" | "radialGradient" => &["gradientUnits"],
        "pattern" => &["patternUnits", "patternContentUnits"],
        _ => return false,
    };
    let mut units: [Option<&str>; 2] = [None, None];
    for _ in 0..16 {
        for (i, name) in names.iter().enumerate() {
            units[i] = units[i].or_else(|| at.attribute(*name).map(str::trim));
        }
        let href = at.attribute("href").or_else(|| at.attribute((XLINK_NS, "href")));
        // usvg follows a gradient's link to another gradient only, a pattern's to a pattern only.
        let family = |n: &Node| match n.tag_name().name() {
            "linearGradient" | "radialGradient" => Some(true),
            "pattern" => Some(false),
            _ => None,
        };
        match href.and_then(|h| h.trim().strip_prefix('#')).and_then(find) {
            Some(next) if next != at && family(&next) == family(&at) => at = next,
            _ => break,
        }
    }
    match kind {
        "pattern" => units[0] == Some("userSpaceOnUse") && units[1] != Some("objectBoundingBox"),
        _ => units[0] == Some("userSpaceOnUse"),
    }
}

fn spec(css: &Cascade, owner: Node) -> std::result::Result<Spec, String> {
    let refuse = |prop: &str, ok: &[&str], inherited: bool| -> std::result::Result<(), String> {
        match css.get(owner, prop, inherited) {
            Some(v) if !ok.contains(&v.as_str()) => Err(format!("{prop} “{v}”")),
            _ => Ok(()),
        }
    };
    refuse("direction", &["ltr"], true)?;
    refuse("writing-mode", &["lr", "lr-tb", "horizontal-tb"], true)?;
    refuse("unicode-bidi", &["normal"], false)?;
    refuse("font-variant", &["normal"], true)?;
    for p in [
        "font-variant-caps",
        "font-variant-ligatures",
        "font-variant-numeric",
        "font-variant-east-asian",
        "font-variant-position",
    ] {
        refuse(p, &["normal"], true)?;
    }
    refuse("font-feature-settings", &["normal"], true)?;
    refuse("font-variation-settings", &["normal"], true)?;
    refuse("font-size-adjust", &["none"], true)?;
    refuse("text-rendering", &["auto", "optimizeLegibility", "geometricPrecision"], true)?;
    refuse("alignment-baseline", &["auto", "baseline"], false)?;
    refuse("baseline-shift", &["baseline", "0"], false)?;
    refuse("stroke-dasharray", &["none"], true)?;
    // The glyphs are paths: a text ignores these, a path does not (aliased edges, a marker at every
    // glyph's vertices).
    refuse("shape-rendering", &["auto", "geometricPrecision"], true)?;
    for p in ["marker", "marker-start", "marker-mid", "marker-end"] {
        refuse(p, &["none"], true)?;
    }
    if css.get(owner, "font", true).is_some() {
        return Err("the font shorthand".into());
    }
    for n in owner.ancestors().filter(Node::is_element) {
        if let Some(v) = css.declared(n).get("text-decoration") {
            if v != "none" {
                return Err(format!("text-decoration “{v}”"));
            }
        }
    }
    for p in ["fill", "stroke"] {
        if let Some(v) = css.get(owner, p, true) {
            if v.starts_with("context-") || (v.contains("url(") && !user_space_paint(owner, &v)) {
                return Err(format!("{p} “{v}”"));
            }
        }
    }
    let families = match css.get(owner, "font-family", true) {
        Some(v) => svgtypes::parse_font_families(&v).unwrap_or_default(),
        None => Vec::new(),
    };
    let families = if families.is_empty() {
        vec![svgtypes::FontFamily::Named(fonts::DEFAULT_FAMILY.to_string())]
    } else {
        families
    };
    let style = match css.get(owner, "font-style", true).as_deref() {
        Some("italic") => Style::Italic,
        Some("oblique") => Style::Oblique,
        _ => Style::Normal,
    };
    let stretch = match css.get(owner, "font-stretch", true).as_deref() {
        Some("narrower" | "condensed") => Stretch::Condensed,
        Some("ultra-condensed") => Stretch::UltraCondensed,
        Some("extra-condensed") => Stretch::ExtraCondensed,
        Some("semi-condensed") => Stretch::SemiCondensed,
        Some("semi-expanded") => Stretch::SemiExpanded,
        Some("wider" | "expanded") => Stretch::Expanded,
        Some("extra-expanded") => Stretch::ExtraExpanded,
        Some("ultra-expanded") => Stretch::UltraExpanded,
        _ => Stretch::Normal,
    };
    let mut dominant = css.get(owner, "dominant-baseline", false);
    if dominant.as_deref() == Some("no-change") {
        dominant = owner.parent_element().and_then(|p| css.get(p, "dominant-baseline", false));
    }
    let baseline = match dominant.as_deref() {
        None | Some("auto" | "use-script" | "no-change" | "reset-size" | "alphabetic") => Baseline::Alphabetic,
        Some("middle") => Baseline::Middle,
        Some("central") => Baseline::Central,
        Some("hanging") => Baseline::Hanging,
        Some("mathematical") => Baseline::Mathematical,
        Some("text-before-edge") => Baseline::TextBeforeEdge,
        Some("text-after-edge" | "ideographic") => Baseline::TextAfterEdge,
        Some(v) => return Err(format!("dominant-baseline “{v}”")),
    };
    let kerning = css.get(owner, "font-kerning", true).as_deref() != Some("none")
        && !matches!(css.length(owner, "kerning", true), Ok(Some(k)) if k == 0.0);
    let size = css.font_size(owner);
    if !(size.is_finite() && size > 0.0) {
        return Err("a font size that is not positive".into());
    }
    let letter_spacing = css.length(owner, "letter-spacing", true)?.unwrap_or(0.0);
    // Glyphs pulled into each other: a text fills all of them and then strokes them, a path per glyph
    // fills over the stroke of the one before it where they overlap.
    if letter_spacing < 0.0 && css.get(owner, "stroke", true).is_some_and(|v| v != "none") {
        return Err("a stroke with negative letter-spacing".into());
    }
    Ok(Spec {
        families,
        weight: css.font_weight(owner),
        style,
        stretch,
        size,
        letter_spacing,
        word_spacing: css.length(owner, "word-spacing", true)?.unwrap_or(0.0),
        kerning,
        opsz: css.get(owner, "font-optical-sizing", true).as_deref() != Some("none"),
        baseline,
    })
}

// ---- white space ----------------------------------------------------------------------------------

/// One text node: its text, how deep it is under the `<text>`, whether `xml:space="preserve"` holds
/// there, and which owner (0 the text, `i + 1` the `i`th span) sets it.
struct Piece {
    text: String,
    depth: usize,
    preserve: bool,
    owner: usize,
}

/// usvg's `trim_text`: CR, LF and tab become spaces; without `preserve` runs of spaces become one.
fn trim(text: &str, preserve: bool) -> String {
    let mut s = String::with_capacity(text.len());
    let mut prev = '0';
    for c in text.chars() {
        let c = if matches!(c, '\r' | '\n' | '\t') { ' ' } else { c };
        if !preserve && c == ' ' && prev == ' ' {
            continue;
        }
        prev = c;
        s.push(c);
    }
    s
}

/// usvg's white space across the text nodes of one `<text>` (SVG 1.1, as Chrome does it): each node
/// trimmed, a space at a boundary between two nodes kept once, the first and last spaces dropped.
fn collapse(pieces: &mut [Piece]) {
    for p in pieces.iter_mut() {
        p.text = trim(&p.text, p.preserve);
    }
    match pieces.len() {
        0 => {}
        1 => {
            let p = &mut pieces[0];
            if !p.preserve {
                if p.text == " " {
                    p.text.clear();
                } else {
                    if p.text.starts_with(' ') {
                        p.text.remove(0);
                    }
                    if p.text.ends_with(' ') {
                        p.text.pop();
                    }
                }
            }
        }
        n => {
            let len = n - 1;
            let mut last_non_empty: Option<usize> = None;
            for i in 0..len {
                let mut a = i;
                if pieces[a].text.is_empty() {
                    if let Some(k) = last_non_empty {
                        a = k;
                    }
                }
                let b = i + 1;
                let c1 = pieces[a].text.chars().next();
                let c2 = pieces[a].text.chars().last();
                let c3 = pieces[b].text.chars().next();
                let c4 = pieces[b].text.chars().last();
                let (pa, pb) = (pieces[a].preserve, pieces[b].preserve);
                if pieces[a].depth < pieces[b].depth {
                    if c3 == Some(' ') && !pb {
                        pieces[b].text.remove(0);
                    }
                } else if c2 == Some(' ') && c2 == c3 {
                    if !pa && !pb {
                        pieces[a].text.pop();
                    } else if pa && !pb {
                        pieces[b].text.remove(0);
                    }
                }
                let first = i == 0;
                let last = i == len - 1;
                if first && c1 == Some(' ') && !pa && !pieces[a].text.is_empty() {
                    pieces[a].text.remove(0);
                } else if last && c4 == Some(' ') && !pieces[b].text.is_empty() && !pb {
                    pieces[b].text.pop();
                }
                if last
                    && c2 == Some(' ')
                    && !pieces[a].text.is_empty()
                    && pieces[b].text.is_empty()
                    && pieces[a].text.ends_with(' ')
                {
                    pieces[a].text.pop();
                }
                if !pieces[a].text.trim().is_empty() {
                    last_non_empty = Some(a);
                }
            }
        }
    }
}

/// CSS `white-space: pre-line` over the whole text: tabs are spaces, a run of spaces is one, and
/// spaces at either side of a newline (and at either end) go; newlines stay.
fn pre_line(pieces: &[Piece]) -> Vec<(char, usize)> {
    let mut out: Vec<(char, usize)> = Vec::new();
    for p in pieces {
        for c in p.text.chars() {
            let c = match c {
                '\t' | '\r' => ' ',
                c => c,
            };
            if c == ' ' && matches!(out.last(), None | Some((' ' | '\n', _))) {
                continue;
            }
            if c == '\n' && matches!(out.last(), Some((' ', _))) {
                out.pop();
            }
            out.push((c, p.owner));
        }
    }
    if matches!(out.last(), Some((' ', _))) {
        out.pop();
    }
    out
}

// ---- one text element -----------------------------------------------------------------------------

/// What a text becomes: the splices that outline it.
type Cuts = Vec<(Range<usize>, String)>;

struct Ctx<'a, 'd> {
    src: &'a str,
    css: &'a Cascade<'a>,
    anims: &'a [Animation],
    /// What the document's `<use>` elements instance.
    uses: &'a [NodeId],
    faces: Faces<'d>,
    /// The bytes the outlines may still add before the title is over its size cap.
    room: usize,
    /// The ids the document has, and the number the next blur filter's id tries.
    ids: HashSet<String>,
    next_blur: usize,
    /// The animation elements the document may still be given before it is over their cap.
    animations: usize,
    /// The amounts the document's animations may still be sampled at (a unit costs a grid of them).
    work: usize,
}

impl Ctx<'_, '_> {
    fn new<'a, 'd>(
        src: &'a str,
        doc: &'a Document,
        css: &'a Cascade<'a>,
        anims: &'a [Animation],
        uses: &'a [NodeId],
        faces: Faces<'d>,
    ) -> Ctx<'a, 'd> {
        let existing = doc
            .descendants()
            .filter(|n| {
                n.is_element()
                    && matches!(
                        n.tag_name().name(),
                        "animate" | "set" | "animateColor" | "animateTransform" | "animateMotion"
                    )
            })
            .count();
        Ctx {
            src,
            css,
            anims,
            uses,
            faces,
            room: MAX_BYTES.saturating_sub(src.len()),
            ids: doc
                .descendants()
                .filter(Node::is_element)
                .filter_map(|n| n.attribute("id").map(str::to_string))
                .collect(),
            next_blur: 0,
            animations: MAX_ANIMATIONS.saturating_sub(existing),
            work: MAX_WORK,
        }
    }
}

/// `node` set by the text engine, `Ok(None)` when it has nothing to draw, or why it stays text.
fn set(cx: &mut Ctx, node: Node) -> std::result::Result<Option<Set>, String> {
    let src = cx.src;
    let r = node.range();
    let named = src.as_bytes().get(r.start + 1..r.start + 5) == Some(b"text".as_slice())
        && src
            .as_bytes()
            .get(r.start + 5)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/');
    if !named {
        return Err("a prefixed element".into());
    }
    let Some(close) = src[r.clone()].rfind("</").map(|i| r.start + i) else {
        return Ok(None);
    };
    let tail = src[close + 2..r.end].trim_end_matches('>').trim_end();
    if tail != "text" {
        return Ok(None);
    }
    for a in node.ancestors().skip(1).filter(Node::is_element) {
        let name = a.tag_name().name();
        if !matches!(
            name,
            "svg" | "g" | "a" | "defs" | "symbol" | "mask" | "pattern" | "marker" | "switch"
        ) {
            return Err(format!("inside <{name}>"));
        }
    }
    if node.ancestors().any(|a| cx.uses.contains(&a.id())) {
        return Err("instanced by a <use>".into());
    }
    for attr in ["dx", "dy", "rotate", "textLength", "lengthAdjust"] {
        if node.attribute(attr).is_some() {
            return Err(attr.to_string());
        }
    }
    let animation = match node.attribute(ANIMATE_MARK) {
        None => None,
        Some(raw) => Some(TextAnimation::from_attribute(raw).map_err(|e| format!("{ANIMATE_MARK} does not read: {e}"))?),
    };
    // The owners of text: the element, then its spans.
    let mut owners: Vec<Node> = vec![node];
    let mut pieces: Vec<Piece> = Vec::new();
    let space = |n: Node| {
        n.ancestors()
            .filter(Node::is_element)
            .find_map(|a| a.attribute((XML_NS, "space")))
            == Some("preserve")
    };
    for child in node.children() {
        match child.node_type() {
            NodeType::Text => pieces.push(Piece {
                text: child.text().unwrap_or_default().to_string(),
                depth: 0,
                preserve: space(node),
                owner: 0,
            }),
            NodeType::Element => match child.tag_name().name() {
                "animate" | "set" | "animateColor" | "animateTransform" | "animateMotion" | "title" | "desc" => {}
                "tspan" => {
                    for attr in SPAN_REFUSED {
                        if cx.css.declared(child).contains_key(*attr) {
                            return Err(format!("a <tspan> with {attr}"));
                        }
                    }
                    owners.push(child);
                    let owner = owners.len() - 1;
                    for g in child.children() {
                        match g.node_type() {
                            NodeType::Text => pieces.push(Piece {
                                text: g.text().unwrap_or_default().to_string(),
                                depth: 1,
                                preserve: space(child),
                                owner,
                            }),
                            NodeType::Element => return Err("an element inside a <tspan>".into()),
                            _ => {}
                        }
                    }
                }
                other => return Err(format!("a <{other}> inside the text")),
            },
            _ => {}
        }
    }
    if cx.css.moves(node, node.id()) || cx.css.touches_made(node, animation.is_some()) {
        return Err("a style rule that would match it differently".into());
    }
    if let Some(next) = node.next_sibling_element() {
        if next
            .descendants()
            .filter(Node::is_element)
            .any(|n| cx.css.moves(n, node.id()))
        {
            return Err("a style rule that would match it differently".into());
        }
    }
    for a in cx.anims {
        let Some(target) = node.document().get_node(a.target) else {
            continue;
        };
        if target == node {
            if !ANIMATED_OK.contains(&a.attr.as_str()) || a.url {
                return Err(format!("an animation of {}", a.attr));
            }
        } else if target.ancestors().any(|n| n == node) {
            return Err("an animated <tspan>".into());
        } else if node.ancestors().any(|n| n == target)
            && (LAYOUT_PROPS.contains(&a.attr.as_str()) || (a.url && matches!(a.attr.as_str(), "fill" | "stroke")))
        {
            return Err(format!("an animation of {} around it", a.attr));
        }
    }
    let white_space = cx.css.get(node, "white-space", true);
    let lines = match white_space.as_deref() {
        None | Some("normal") => false,
        Some("pre-line") => true,
        Some(v) => return Err(format!("white-space “{v}”")),
    };
    for &o in &owners[1..] {
        if cx.css.get(o, "white-space", true) != white_space {
            return Err("white-space set on a <tspan>".into());
        }
    }
    let chars: Vec<(char, usize)> = if lines {
        pre_line(&pieces)
    } else {
        collapse(&mut pieces);
        pieces
            .iter()
            .flat_map(|p| p.text.chars().map(move |c| (c, p.owner)))
            .collect()
    };
    if chars.iter().all(|(c, _)| c.is_whitespace()) {
        return Ok(None);
    }
    // Each character that is not a space is a path of at least this much: a text that cannot fit is not
    // laid out to find that out.
    let visible = chars.iter().filter(|(c, _)| !c.is_whitespace()).count();
    // An animated glyph also carries its group and animations (a hundred bytes at the least).
    let least = MIN_GLYPH_BYTES + if animation.is_some() { MIN_UNIT_BYTES } else { 0 };
    if visible.saturating_mul(least) > cx.room {
        return Err(format!("outlines that would take the title over {} KiB", MAX_BYTES / 1024));
    }
    let specs: Vec<Spec> = owners
        .iter()
        .map(|o| spec(cx.css, *o))
        .collect::<std::result::Result<_, _>>()?;
    let font_size = specs[0].size;
    let coord = |name: &str| -> std::result::Result<f32, String> {
        let Some(v) = node.attribute(name) else { return Ok(0.0) };
        let mut list = svgtypes::LengthListParser::from(v);
        let first = match list.next() {
            None => return Ok(0.0),
            Some(Ok(len)) => len,
            Some(Err(_)) => return Err(format!("{name} “{v}”")),
        };
        if list.next().is_some() {
            return Err(format!("a list of {name} positions"));
        }
        let x = first.number as f32;
        match first.unit {
            svgtypes::LengthUnit::Em => Ok(x * font_size),
            svgtypes::LengthUnit::Ex => Ok(x * font_size / 2.0),
            unit => absolute(x, unit).ok_or_else(|| format!("{name} “{v}”")),
        }
    };
    let (x, y) = (coord("x")?, coord("y")?);
    let anchor = match cx.css.get(owners[chars[0].1], "text-anchor", true).as_deref() {
        None | Some("start") => Align::Start,
        Some("middle") => Align::Middle,
        Some("end") => Align::End,
        Some(v) => return Err(format!("text-anchor “{v}”")),
    };
    // `inline-size` is the element's own (CSS does not inherit it).
    let width = match cx.css.declared(node).get("inline-size").map(String::as_str) {
        None | Some("auto") => None,
        Some(v) => Some(length_at(v, || font_size).ok_or_else(|| format!("inline-size “{v}”"))?),
    }
    .filter(|w| *w > 0.0);
    // Shrunk to its box: set on one line (and at each newline) no wider than `inline-size`.
    let shrink = width.is_some() && node.attribute(FIT_MARK).map(str::trim) == Some("shrink");
    // The line height, and whether it follows the font size (a number, `em`, `ex`, a percentage).
    let line_height = match cx.css.find(node, "line-height", true) {
        None => None,
        Some((_, v)) if v == "normal" => None,
        Some((at, v)) => {
            let fs = cx.css.font_size(at);
            match v.parse::<f32>() {
                Ok(n) if n.is_finite() && n >= 0.0 => Some((n * font_size, true)),
                _ => match svgtypes::Length::from_str(&v) {
                    Ok(len) if len.unit == svgtypes::LengthUnit::Percent => Some((len.number as f32 * fs * 0.01, true)),
                    Ok(len) => {
                        let relative = matches!(len.unit, svgtypes::LengthUnit::Em | svgtypes::LengthUnit::Ex);
                        Some((length_at(&v, || fs).ok_or_else(|| format!("line-height “{v}”"))?, relative))
                    }
                    Err(_) => return Err(format!("line-height “{v}”")),
                },
            }
        }
    };
    // Faces: each owner's own, then the fallbacks the text needs.
    let mut primaries: Vec<ID> = Vec::with_capacity(specs.len());
    for s in &specs {
        primaries.push(s.query(cx.faces.db).ok_or("no font at all")?);
    }
    let mut used: Vec<ID> = primaries.clone();
    let mut fallbacks: Vec<ID> = Vec::new();
    for &(c, owner) in &chars {
        if c.is_whitespace() || c.is_control() {
            continue;
        }
        let covered = std::iter::once(primaries[owner])
            .chain(fallbacks.iter().copied())
            .any(|id| cx.faces.get(id).is_some_and(|f| f.has_char(c)));
        if covered {
            continue;
        }
        if fallbacks.len() >= MAX_FALLBACKS {
            return Err("too many fonts".into());
        }
        let found = cx
            .faces
            .fallback(c, primaries[owner], &used)
            .ok_or_else(|| format!("no font has “{c}”"))?;
        used.push(found);
        fallbacks.push(found);
    }
    let fallback_faces: Vec<Arc<Face>> = fallbacks.iter().filter_map(|id| cx.faces.get(*id)).collect();
    let mut text = String::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut owner_of: Vec<usize> = Vec::new();
    for &(c, owner) in &chars {
        let at = text.len();
        text.push(c);
        if owner_of.last() == Some(&owner) {
            if let Some(run) = runs.last_mut() {
                run.range.end = text.len();
            }
            continue;
        }
        let s = &specs[owner];
        let primary = cx.faces.get(primaries[owner]).ok_or("a font that cannot be read")?;
        let mut faces = vec![primary];
        faces.extend(fallback_faces.iter().cloned());
        runs.push(Run {
            range: at..text.len(),
            faces,
            size: s.size,
            variations: s.variations(),
            optical_sizing: s.opsz,
            letter_spacing: s.letter_spacing,
            word_spacing: s.word_spacing,
            kerning: s.kerning,
            baseline: s.baseline,
        });
        owner_of.push(owner);
    }
    let mut lay = text::layout(
        &text,
        &runs,
        &Paragraph {
            align: anchor,
            width: width.filter(|_| !shrink),
            line_height: line_height.map(|(h, _)| h),
        },
    );
    if let (true, Some(avail)) = (shrink, width) {
        // Everything that follows the font size, scaled together until the widest line fits (the optical
        // size moves with the size, so a pass may land a little wide).
        let mut k = 1.0f32;
        for _ in 0..4 {
            let w = lay.width();
            if w <= avail * (1.0 + 1e-4) || w <= 0.0 {
                break;
            }
            k *= avail / w;
            let scaled: Vec<Run> = runs
                .iter()
                .map(|r| Run {
                    size: r.size * k,
                    letter_spacing: r.letter_spacing * k,
                    word_spacing: r.word_spacing * k,
                    ..r.clone()
                })
                .collect();
            lay = text::layout(
                &text,
                &scaled,
                &Paragraph {
                    align: anchor,
                    width: None,
                    line_height: line_height.map(|(h, relative)| if relative { h * k } else { h }),
                },
            );
        }
    }
    if let Some(c) = lay.missing.first() {
        return Err(format!("no font has “{c}”"));
    }
    if lay.glyphs.iter().any(|g| !g.face.is_outline(g.id)) {
        return Err("a colour glyph".into());
    }
    let far = |v: f32| !v.is_finite() || v.abs() > MAX_COORD;
    if lay.glyphs.iter().any(|g| far(g.x + x) || far(g.y + y)) || lay.bounds.is_some_and(|b| b.iter().any(|v| far(*v))) {
        return Err("glyphs set too far out".into());
    }
    for g in &mut lay.glyphs {
        g.x += x;
        g.y += y;
    }
    lay.bounds = lay.bounds.map(|[x0, y0, x1, y1]| [x0 + x, y0 + y, x1 + x, y1 + y]);
    let units = animation.map(|animation| units_of(&text, &lay, animation));
    if let Some(u) = &units {
        if u.count > MAX_UNITS {
            return Err(format!(
                "{} units to animate (most {MAX_UNITS}): animate by word or line",
                u.count
            ));
        }
    }
    let paint: Vec<String> = owners
        .iter()
        .enumerate()
        .map(|(i, o)| if i == 0 { String::new() } else { span_paint(cx.css, *o) })
        .collect();
    Ok(Some(Set {
        lay,
        owner_of,
        paint,
        units,
        open: r.start + 1..r.start + 5,
        close,
    }))
}

/// Which unit of `animation` each glyph of `lay` belongs to (`None`: none, a glyph of a space), and how many
/// units there are.
fn units_of(text: &str, lay: &text::Layout, animation: TextAnimation) -> Units {
    let (of_glyph, count) = match animation.by {
        AnimateBy::Line => {
            // A line is a unit if a glyph of it has an outline: counted in the order lines are laid out.
            let mut lines: Vec<usize> = lay
                .glyphs
                .iter()
                .filter(|g| {
                    text.get(g.cluster..)
                        .and_then(|t| t.chars().next())
                        .is_some_and(|c| !c.is_whitespace())
                })
                .map(|g| g.line)
                .collect();
            lines.sort_unstable();
            lines.dedup();
            let of = lay
                .glyphs
                .iter()
                .map(|g| {
                    let visible = text
                        .get(g.cluster..)
                        .and_then(|t| t.chars().next())
                        .is_some_and(|c| !c.is_whitespace());
                    visible.then(|| lines.binary_search(&g.line).ok()).flatten()
                })
                .collect();
            (of, lines.len())
        }
        by => {
            let (map, count) = glyph_smil::units_of(text, by);
            (
                lay.glyphs.iter().map(|g| map.get(g.cluster).copied().flatten()).collect(),
                count,
            )
        }
    };
    Units {
        of_glyph,
        count,
        animation,
    }
}

/// A text set: its glyphs placed in the element's user space, which owner each run is, each owner's paint
/// for the group of its glyphs (none for the text itself), and where its tag names are.
struct Set {
    lay: text::Layout,
    owner_of: Vec<usize>,
    paint: Vec<String>,
    /// The units an animation moves, if the text has one.
    units: Option<Units>,
    open: Range<usize>,
    close: usize,
}

/// A text's animation and which unit each of its glyphs is in.
struct Units {
    of_glyph: Vec<Option<usize>>,
    count: usize,
    animation: TextAnimation,
}

/// The splices that set `node` as outlines, `Ok(None)` when it has nothing to draw, or why it stays text.
fn plan(cx: &mut Ctx, node: Node) -> std::result::Result<Option<Cuts>, String> {
    let Some(set) = set(cx, node)? else { return Ok(None) };
    let mut out = format!("<g {GLYPHS_MARK}=\"\">");
    if let Some([x0, y0, x1, y1]) = set.lay.bounds {
        out.push_str(&format!(
            "<path fill=\"none\" stroke=\"none\" d=\"M{} {}H{}V{}H{}z\"/>",
            num(x0),
            num(y0),
            num(x1),
            num(y1),
            num(x0)
        ));
    }
    match &set.units {
        None => glyphs_out(&set, 0..set.lay.glyphs.len(), None, &mut out),
        Some(units) => animated_out(cx, &set, units, &mut out)?,
    }
    out.push_str("</g>");
    if set.units.is_some() && out.len() > cx.room {
        return Err(format!("the animation would take the title over {} KiB", MAX_BYTES / 1024));
    }
    Ok(Some(vec![
        (set.open, format!("g {TEXT_MARK}=\"\"")),
        (set.close..set.close, out),
        (set.close + 2..set.close + 6, "g".to_string()),
    ]))
}

/// The paths of the glyphs at `indices` of `set`, a span's own paint grouped round its glyphs. `shift` is the
/// point they are written relative to.
fn glyphs_out(set: &Set, indices: impl IntoIterator<Item = usize>, shift: Option<(f32, f32)>, out: &mut String) {
    let mut open: Option<usize> = None;
    let mut d = String::new();
    for i in indices {
        let g = &set.lay.glyphs[i];
        let owner = set.owner_of[g.run];
        let group = (!set.paint[owner].is_empty()).then_some(owner);
        if open != group {
            if open.is_some() {
                out.push_str("</g>");
            }
            if let Some(o) = group {
                out.push_str("<g");
                out.push_str(&set.paint[o]);
                out.push('>');
            }
            open = group;
        }
        d.clear();
        let drawn = match shift {
            None => text::glyph_path(g, &mut d),
            Some((sx, sy)) => {
                let mut moved = g.clone();
                moved.x -= sx;
                moved.y -= sy;
                text::glyph_path(&moved, &mut d)
            }
        };
        if drawn {
            out.push_str("<path d=\"");
            out.push_str(&d);
            out.push_str("\"/>");
        }
    }
    if open.is_some() {
        out.push_str("</g>");
    }
}

/// How far above a unit's baseline it turns and scales about, in em.
const PIVOT_LIFT: f32 = 0.35;

/// A text's glyphs by unit, each unit in the group its animation makes (`glyph_smil::wrap`), or bare when the
/// animation leaves it as it is.
fn animated_out(cx: &mut Ctx, set: &Set, units: &Units, out: &mut String) -> std::result::Result<(), String> {
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); units.count];
    let mut loose = Vec::new();
    for (i, unit) in units.of_glyph.iter().enumerate() {
        match unit {
            Some(u) => members[*u].push(i),
            None => loose.push(i),
        }
    }
    let rounded = |v: f32| (v * 100.0).round() / 100.0;
    let cost = members.iter().filter(|m| !m.is_empty()).count() * glyph_smil::grid_points(&units.animation);
    if cost > cx.work {
        return Err("too long or too many units to animate (shorten the animation or animate by word)".into());
    }
    cx.work -= cost;
    let mut budget = cx.animations;
    let mut src = IdSource {
        taken: &cx.ids,
        next: cx.next_blur,
    };
    for (unit, glyphs) in members.iter().enumerate() {
        if glyphs.is_empty() {
            continue;
        }
        let boxes: Vec<[f32; 4]> = glyphs
            .iter()
            .filter_map(|i| text::glyph_bounds(&set.lay.glyphs[*i]))
            .collect();
        let first = &set.lay.glyphs[glyphs[0]];
        let wrap = boxes
            .iter()
            .copied()
            .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
            .and_then(|bounds| {
                let geom = UnitGeom {
                    bounds,
                    pivot: (
                        rounded((bounds[0] + bounds[2]) / 2.0),
                        rounded(first.y - PIVOT_LIFT * first.size),
                    ),
                    size: first.size,
                };
                glyph_smil::wrap(&units.animation, unit, units.count, &geom, &mut src)
            });
        match wrap {
            Some(w) => {
                if w.animations > budget {
                    return Err(format!("more than {MAX_ANIMATIONS} animations in the title"));
                }
                budget -= w.animations;
                out.push_str(&w.open);
                glyphs_out(set, glyphs.iter().copied(), w.pivot, out);
                out.push_str("</g>");
            }
            None => glyphs_out(set, glyphs.iter().copied(), None, out),
        }
        if out.len() > cx.room {
            return Err(format!("the animation would take the title over {} KiB", MAX_BYTES / 1024));
        }
    }
    cx.next_blur = src.next;
    cx.animations = budget;
    glyphs_out(set, loose, None, out);
    Ok(())
}

/// A span's own paint as attributes (` fill="#f00"`), what its glyphs' group carries.
fn span_paint(css: &Cascade, span: Node) -> String {
    let declared = css.declared(span);
    let mut out = String::new();
    for p in SPAN_PAINT {
        if let Some(v) = declared.get(*p) {
            out.push_str(&format!(" {p}=\"{}\"", escape_attr(v, '"')));
        }
    }
    out
}

/// A coordinate to a hundredth, without trailing zeros.
fn num(v: f32) -> String {
    let s = format!("{:.2}", f64::from(v));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Set every `<text>` of sanitised `src` that can be as glyph outlines, with the faces of `db` (the font
/// database the title would be drawn with). A text that cannot stays text; one that uses what only
/// outlines honour (`inline-size`, `white-space: pre-line`) says so in a warning. If the outlines would
/// take the title past its size cap, nothing is outlined.
pub(crate) fn outline_text(src: &str, db: &Database) -> Result<Outlined> {
    let unchanged = |warnings, kept| Outlined {
        svg: src.to_string(),
        count: 0,
        warnings,
        kept,
    };
    if !src.contains("<text") && !src.contains(":text") {
        return Ok(unchanged(Vec::new(), Vec::new()));
    }
    let doc = Document::parse_with_options(src, xml_options()).map_err(|e| invalid(format!("not valid XML: {e}")))?;
    let css = Cascade::new(&doc);
    let anims = animations(&doc);
    let uses = use_targets(&doc);
    let mut cx = Ctx::new(
        src,
        &doc,
        &css,
        &anims,
        &uses,
        Faces {
            db,
            read: HashMap::new(),
            fallbacks: HashMap::new(),
        },
    );
    let mut cuts = Vec::new();
    let mut count = 0;
    let mut warnings = Vec::new();
    let mut kept = Vec::new();
    // The outlined texts: their label, and what drawing them with their font would lose.
    let mut outlined: Vec<(String, Option<String>)> = Vec::new();
    for node in doc
        .descendants()
        .filter(|n| {
            n.is_element()
                && n.tag_name().name() == "text"
                && n.tag_name().namespace() == Some(SVG_NS)
                && !n.ancestors().skip(1).any(|a| a.tag_name().name() == "text")
        })
        .take(MAX_TEXTS)
    {
        let boxed = css.declared(node).get("inline-size").is_some_and(|v| v != "auto");
        let mut loses: Vec<&str> = Vec::new();
        if boxed && node.attribute(FIT_MARK).map(str::trim) == Some("shrink") {
            loses.push("it does not shrink to fit its box");
        } else if boxed || css.get(node, "white-space", true).as_deref() == Some("pre-line") {
            loses.push("its lines do not wrap");
        }
        if node.attribute(ANIMATE_MARK).is_some() {
            loses.push("its animation does not play");
        }
        let loses = (!loses.is_empty()).then(|| format!("so {}", loses.join(" and ")));
        match plan(&mut cx, node) {
            Ok(Some(mut c)) => {
                let grown = c.iter().map(|(r, s)| s.len().saturating_sub(r.len())).sum::<usize>();
                cx.room = cx.room.saturating_sub(grown);
                cuts.append(&mut c);
                count += 1;
                outlined.push((label(&node), loses));
            }
            Ok(None) => {}
            Err(why) => {
                tracing::debug!(text = %label(&node), %why, "text kept as text");
                kept.push(format!("the text {} ({why})", label(&node)));
                if let Some(loses) = loses {
                    warnings.push(format!(
                        "the text {} is drawn with its font, not as outlines ({why}), {loses}",
                        label(&node)
                    ));
                }
            }
        }
    }
    if count == 0 {
        return Ok(unchanged(warnings, kept));
    }
    let svg = apply(src, cuts);
    if svg.len() > MAX_BYTES {
        let over = format!("they would take the title over {} KiB", MAX_BYTES / 1024);
        let mut warnings = warnings;
        for (l, loses) in outlined {
            kept.push(format!("the text {l} ({over})"));
            if let Some(loses) = loses {
                warnings.push(format!(
                    "the text {l} is drawn with its font, not as outlines ({over}), {loses}"
                ));
            }
        }
        return Ok(unchanged(warnings, kept));
    }
    Ok(Outlined {
        svg,
        count,
        warnings,
        kept,
    })
}

fn label(node: &Node) -> String {
    match node.attribute("id") {
        Some(id) => format!("“{id}”"),
        None => {
            let text: String = node.descendants().filter_map(|n| n.text()).collect::<String>();
            let short: String = text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(24)
                .collect();
            format!("“{short}”")
        }
    }
}
#[cfg(test)]
mod tests {
    use resvg::usvg;

    use super::*;
    use crate::svg::raster;
    use crate::svg::SvgDoc;

    fn doc(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 200">{body}</svg>"#)
    }

    fn db() -> Arc<Database> {
        fonts::svg_fontdb()
    }

    fn outline(src: &str) -> Outlined {
        outline_text(src, &db()).unwrap()
    }

    /// Every glyph of every outlined text: id and origin.
    fn ours(src: &str) -> Vec<(u32, f32, f32)> {
        laid(src)
            .iter()
            .flat_map(|lay| lay.glyphs.iter().map(|g| (g.id, g.x, g.y)))
            .collect()
    }

    /// Each text of `src` as the engine sets it.
    fn laid(src: &str) -> Vec<text::Layout> {
        let doc = Document::parse_with_options(src, xml_options()).unwrap();
        let css = Cascade::new(&doc);
        let anims = animations(&doc);
        let db = db();
        let mut cx = Ctx {
            src,
            css: &css,
            anims: &anims,
            uses: &[],
            faces: Faces {
                db: &db,
                read: HashMap::new(),
                fallbacks: HashMap::new(),
            },
            room: MAX_BYTES,
            ids: HashSet::new(),
            next_blur: 0,
            animations: MAX_ANIMATIONS,
            work: MAX_WORK,
        };
        doc.descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "text")
            .map(|node| set(&mut cx, node).unwrap().unwrap().lay)
            .collect()
    }

    /// Every glyph usvg lays the same document out with.
    fn theirs(src: &str) -> Vec<(u32, f32, f32)> {
        fn walk(g: &usvg::Group, out: &mut Vec<(u32, f32, f32)>) {
            for node in g.children() {
                match node {
                    usvg::Node::Group(g) => walk(g, out),
                    usvg::Node::Text(t) => {
                        for span in t.layouted() {
                            for pg in &span.positioned_glyphs {
                                let ts = pg.transform();
                                out.push((pg.id.0, ts.tx, ts.ty));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let opt = usvg::Options {
            font_family: fonts::DEFAULT_FAMILY.to_string(),
            fontdb: db(),
            ..usvg::Options::default()
        };
        let tree = usvg::Tree::from_str(src, &opt).unwrap();
        let mut out = Vec::new();
        walk(tree.root(), &mut out);
        out
    }

    fn same_layout(body: &str) {
        let src = doc(body);
        let (a, b) = (ours(&src), theirs(&src));
        assert_eq!(a.len(), b.len(), "{body}\nours {a:?}\nusvg {b:?}");
        for (g, h) in a.iter().zip(&b) {
            assert_eq!(g.0, h.0, "{body}\nours {a:?}\nusvg {b:?}");
            assert!(
                (g.1 - h.1).abs() < 0.02 && (g.2 - h.2).abs() < 0.02,
                "{body}\nours {g:?}\nusvg {h:?}"
            );
        }
    }

    #[test]
    fn glyphs_sit_where_usvg_puts_them() {
        same_layout(r#"<text x="20" y="100" font-family="Geist" font-size="40">Hello AVA, Wave!</text>"#);
        same_layout(
            r#"<text x="200" y="100" font-family="Geist" font-size="30" text-anchor="middle" letter-spacing="3" word-spacing="9">A middle line</text>"#,
        );
        same_layout(
            r#"<text x="390" y="60" font-family="Geist" font-size="24" text-anchor="end" dominant-baseline="middle">Ends here</text>"#,
        );
        same_layout(
            r#"<text x="10" y="80" font-family="Geist" font-size="30">Big <tspan font-weight="bold" font-size="40" fill="red">bold</tspan> end</text>"#,
        );
        same_layout(
            r#"<style>.t{font-family:Geist;font-size:36px;letter-spacing:.1em}</style><text class="t" x="5" y="50">Styled</text>"#,
        );
        same_layout(
            r#"<g font-size="20"><text x="5" y="50" font-family="Geist" font-size="2em" xml:space="preserve">  two   spaces </text></g>"#,
        );
        same_layout(
            "<text x=\"5\" y=\"150\" font-family=\"Geist\" font-size=\"28\" font-weight=\"bold\" font-kerning=\"none\">\n   AVAV  wrapped\n  source\n</text>",
        );
        same_layout(r#"<text x="5" y="150" font-family="Geist" font-size="28" style="font-kerning:none">AVAV To</text>"#);
        same_layout(
            r#"<text x="5" y="150" font-family="Geist" font-size="28" dominant-baseline="central">a<tspan dominant-baseline="hanging">b</tspan>c</text>"#,
        );
    }

    #[test]
    fn the_box_path_is_usvgs_bounding_box() {
        for body in [
            r#"<text x="20" y="100" font-family="Geist" font-size="40" letter-spacing="4">Box it</text>"#,
            r#"<text x="200" y="50" font-family="Geist" font-size="30" text-anchor="middle" dominant-baseline="middle">Mid <tspan font-size="50">big</tspan></text>"#,
        ] {
            let src = doc(body);
            let doc_ = Document::parse_with_options(&src, xml_options()).unwrap();
            let css = Cascade::new(&doc_);
            let db = db();
            let mut cx = Ctx {
                src: &src,
                css: &css,
                anims: &[],
                uses: &[],
                faces: Faces {
                    db: &db,
                    read: HashMap::new(),
                    fallbacks: HashMap::new(),
                },
                room: MAX_BYTES,
                ids: HashSet::new(),
                next_blur: 0,
                animations: MAX_ANIMATIONS,
                work: MAX_WORK,
            };
            let node = doc_.descendants().find(|n| n.has_tag_name((SVG_NS, "text"))).unwrap();
            let [x0, y0, x1, y1] = set(&mut cx, node).unwrap().unwrap().lay.bounds.unwrap();
            let opt = usvg::Options {
                font_family: fonts::DEFAULT_FAMILY.to_string(),
                fontdb: db.clone(),
                ..usvg::Options::default()
            };
            let tree = usvg::Tree::from_str(&src, &opt).unwrap();
            let Some(usvg::Node::Text(t)) = tree.root().children().first() else {
                panic!("a text")
            };
            let b = t.bounding_box();
            for (ours, theirs) in [(x0, b.left()), (y0, b.top()), (x1, b.right()), (y1, b.bottom())] {
                assert!((ours - theirs).abs() < 0.05, "{body}: {:?} vs {b:?}", [x0, y0, x1, y1]);
            }
        }
    }

    fn pixels(svg: &str) -> Vec<u8> {
        raster::render_with(svg, 400, 200, 1.0, db()).unwrap().data().to_vec()
    }

    #[test]
    fn outlines_draw_what_the_text_drew() {
        let src = doc(concat!(
            r##"<rect width="400" height="200" fill="#123"/>"##,
            r##"<text id="t" x="200" y="90" font-family="Geist" font-size="48" text-anchor="middle" fill="#fe8" stroke="#a20" stroke-width="1.5" opacity=".8">"##,
            r##"Kite <tspan font-weight="bold" fill="#0cf">fun</tspan>!</text>"##,
            r##"<g transform="rotate(-5 200 150)"><text x="40" y="160" font-family="Geist" font-size="30" letter-spacing="2">Tilted text</text></g>"##,
            r##"<filter id="soft"><feGaussianBlur stdDeviation="4"/></filter>"##,
            r##"<g filter="url(#soft)"><text x="250" y="190" font-family="Geist" font-size="26" fill="#fff">Glow</text></g>"##
        ));
        let out = outline(&src);
        assert_eq!(out.count, 3, "{}", out.svg);
        assert!(!out.svg.contains("<text"), "{}", out.svg);
        // Without a single font the outlines are the same picture.
        let bare = raster::render_with(&out.svg, 400, 200, 1.0, Arc::new(Database::new())).unwrap();
        assert_eq!(bare.data(), pixels(&out.svg).as_slice());
        let (a, b) = (pixels(&src), pixels(&out.svg));
        let differ = a
            .chunks(4)
            .zip(b.chunks(4))
            .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 24))
            .count();
        assert!(differ < 8, "{differ} pixels differ");
    }

    const CORPUS_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/fixtures/outline-corpus.json"
    );

    /// Titles and what outlining them wrote, for the frontend's port of `restore_text` (`title-text.ts`'s
    /// `textForm`, replayed by `outline-corpus.test.ts`).
    fn corpus() -> serde_json::Value {
        let cases: Vec<serde_json::Value> = [
            ("plain", r#"<text id="t" x="20" y="100" font-family="Geist" font-size="40">Hi</text>"#),
            (
                "spans and an animation",
                "<text x=\"5\" y=\"50\" font-family=\"Geist\">\n  a <tspan id=\"s\" font-weight=\"bold\" fill=\"#f00\">b</tspan>\n  <animate attributeName=\"opacity\" from=\"0\" to=\"1\" dur=\"1s\"/>\n</text><text x=\"5\" y=\"90\">c</text>",
            ),
            ("cdata", r#"<text x="5" y="50" font-family="Geist"><![CDATA[a < b </text>]]></text>"#),
            ("entities", r#"<text x="5" y="50" font-family="Geist">fish &amp; chips &lt;/text&gt;</text>"#),
            ("comment", r#"<text x="5" y="50" font-family="Geist">a<!-- </text> -->b</text>"#),
            ("processing instruction", r#"<text x="5" y="50" font-family="Geist">a<?pi </text> ?>b</text>"#),
            ("spaced end tag", r#"<text x="5" y="50" font-family="Geist">spaced</text >"#),
            ("attribute", r#"<text x="5" y="50" font-family="Geist" data-x="&lt;/text&gt; &gt;">attr</text>"#),
            ("self-closing", r#"<text x="5" y="50" font-family="Geist"/><text x="5" y="90" font-family="Geist">after</text>"#),
            (
                "a box that shrinks",
                r#"<text id="n" x="5" y="50" font-family="Geist" font-size="30" inline-size="80" data-kerf-fit="shrink">Long name</text>"#,
            ),
            (
                "an animation",
                r#"<text id="a" x="5" y="50" font-family="Geist" font-size="30" data-kerf-animate='{"by":"word","duration":1.5,"range":{"offset":{"from":0.0,"to":1.0}},"properties":{"y":0.5,"opacity":0.0,"blur":0.25,"rotation":-20.0}}'>Kite fun</text>"#,
            ),
            ("one kept as text", r#"<text x="5" y="50" dx="2" font-family="Geist">kept</text><text x="5" y="90" font-family="Geist">set</text>"#),
            ("none", r#"<rect width="1" height="1"/>"#),
        ]
        .into_iter()
        .map(|(name, body)| {
            let text = doc(body);
            let outlined = outline(&text).svg;
            serde_json::json!({ "name": name, "outlined": outlined, "text": text })
        })
        .collect();
        serde_json::json!({
            "about": "Written by kerf-core (svg/outline.rs); replayed by outline-corpus.test.ts. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- outline_corpus",
            "cases": cases,
        })
    }

    #[test]
    fn outline_corpus_is_what_this_engine_writes() {
        let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
        if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
            std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
            return;
        }
        let on_disk = std::fs::read_to_string(CORPUS_PATH)
            .expect("frontend/src/lib/fixtures/outline-corpus.json — generate it with KERF_BLESS_CORPUS=1")
            .replace("\r\n", "\n");
        assert!(
            on_disk == fresh,
            "the outline corpus is stale: regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core \
             --no-default-features -- outline_corpus, then run `bun test` — the replay says what textForm has to follow"
        );
        // Every case is one restore_text gives back.
        for case in corpus()["cases"].as_array().unwrap() {
            let (outlined, text) = (case["outlined"].as_str().unwrap(), case["text"].as_str().unwrap());
            assert_eq!(restore_text(outlined).unwrap(), text, "{}", case["name"]);
        }
    }

    #[test]
    fn restoring_gives_back_the_text_byte_for_byte() {
        for body in [
            r#"<text id="t" x="20" y="100" font-family="Geist" font-size="40">Hello</text>"#,
            "<text x=\"5\" y=\"50\" font-family=\"Geist\">\n  a <tspan id=\"s\" font-weight=\"bold\">b</tspan>\n  <animate attributeName=\"opacity\" from=\"0\" to=\"1\" dur=\"1s\"/>\n</text><text x=\"5\" y=\"90\">c</text>",
            r#"<rect width="1" height="1"/>"#,
            r#"<text x="5" y="50" font-family="Geist"><![CDATA[a < b </text>]]></text>"#,
            r#"<text x="5" y="50" font-family="Geist">fish &amp; chips &#x41;&lt;/text&gt;</text>"#,
            r#"<text x="5" y="50" font-family="Geist">a<!-- </text> -->b</text>"#,
            r#"<text x="5" y="50" font-family="Geist">a<?pi </text> ?>b</text>"#,
            r#"<text x="5" y="50" font-family="Geist">spaced</text >"#,
            r#"<text x="5" y="50" font-family="Geist" data-x="&lt;/text&gt;">attr</text>"#,
            r#"<text x="5" y="50" font-family="Geist"/><text x="5" y="90" font-family="Geist">after</text>"#,
            r#"<s:text xmlns:s="http://www.w3.org/2000/svg" x="5" y="50" font-family="Geist">prefixed</s:text>"#,
        ] {
            let src = doc(body);
            let out = outline(&src);
            assert_eq!(restore_text(&out.svg).unwrap(), src, "{body}");
            assert_eq!(restore_text(&src).unwrap(), src);
        }
    }

    #[test]
    fn restoring_refuses_a_document_nested_past_the_cap_before_reading_it() {
        let deep = 100_000;
        let src = doc(&format!(
            r#"<g {TEXT_MARK}="">{}{}</g>"#,
            "<g>".repeat(deep),
            "</g>".repeat(deep)
        ));
        assert!(restore_text(&src).is_err());
        let big = doc(&format!(r#"<g {TEXT_MARK}="">{}</g>"#, " ".repeat(MAX_BYTES)));
        assert!(restore_text(&big).is_err());
    }

    #[test]
    fn a_text_that_cannot_fit_the_cap_is_not_laid_out() {
        let src = doc(&format!(
            r#"<text x="5" y="50" font-family="Geist" style="inline-size:390px">{}</text>"#,
            "ab ".repeat(30_000)
        ));
        let started = std::time::Instant::now();
        let out = outline(&src);
        assert_eq!(out.count, 0);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(out.warnings.iter().any(|w| w.contains("KiB")), "{:?}", out.warnings);
    }

    #[test]
    fn a_text_a_use_instances_stays_text_and_still_clips() {
        let src = doc(concat!(
            r##"<defs><clipPath id="c"><use href="#t"/></clipPath></defs>"##,
            r##"<text id="t" x="20" y="120" font-family="Geist" font-size="90">CLIP</text>"##,
            r##"<rect width="400" height="200" fill="#0af" clip-path="url(#c)"/>"##,
        ));
        let out = outline(&src);
        assert_eq!(out.count, 0, "{}", out.svg);
        let drawn = pixels(&out.svg);
        assert!(
            drawn.chunks(4).filter(|p| p[3] != 0).count() > 1000,
            "the clipped rect is drawn"
        );
        assert_eq!(drawn, pixels(&src));
    }

    #[test]
    fn the_outlined_form_passes_the_sanitiser_untouched_and_keeps_its_animation() {
        let src = doc(
            r#"<text id="t" x="20" y="100" font-family="Geist" font-size="40" opacity="0">Hi<animate attributeName="opacity" from="0" to="1" dur="2s" fill="freeze"/></text>"#,
        );
        let out = outline(&src);
        assert_eq!(out.count, 1);
        let parsed = SvgDoc::parse(&out.svg).unwrap();
        assert_eq!(parsed.sanitized(), out.svg);
        assert!(parsed.warnings().is_empty(), "{:?}", parsed.warnings());
        assert!(parsed.is_animated());
        let mid = parsed.at(1.0);
        assert!(mid.contains("data-kerf-text=\"\" id=\"t\""), "{mid}");
        assert!(mid.contains("opacity:0.5"), "{mid}");
        assert!(!mid.contains("<animate"), "{mid}");
    }

    #[test]
    fn what_outlines_cannot_draw_stays_text() {
        for body in [
            r##"<defs><path id="p" d="M0 0L100 0"/></defs><text font-family="Geist"><textPath href="#p">on a path</textPath></text>"##,
            r#"<text x="5" y="50" dx="3" font-family="Geist">moved</text>"#,
            r#"<text x="5 9 12" y="50" font-family="Geist">placed</text>"#,
            r#"<text x="5" y="50" font-family="Geist">a<tspan x="9" dy="20">line</tspan></text>"#,
            r#"<linearGradient id="g"><stop offset="0" stop-color="red"/></linearGradient><text x="5" y="50" fill="url(#g)" font-family="Geist">grad</text>"#,
            r#"<text x="5" y="50" font-family="Geist" font-size="10">big<animate attributeName="font-size" to="20" dur="1s"/></text>"#,
            r#"<text x="5" y="50" font-family="Geist">a<tspan>b<set attributeName="visibility" to="hidden" begin="1s"/></tspan></text>"#,
            r#"<g><animate attributeName="letter-spacing" to="4" dur="1s"/><text x="5" y="50" font-family="Geist">spaced</text></g>"#,
            r#"<style>text{fill:red}</style><text x="5" y="50" font-family="Geist">ruled</text>"#,
            r#"<style>path{stroke:blue}</style><text x="5" y="50" font-family="Geist">ruled</text>"#,
            r#"<style>text+rect{fill:red}</style><text x="5" y="50" font-family="Geist">ruled</text><rect width="1" height="1"/>"#,
            r#"<text x="5" y="50" font-family="Geist" text-decoration="underline">under</text>"#,
            r#"<text x="5" y="50" font-family="Geist" writing-mode="tb">down</text>"#,
            r##"<text x="5" y="50" font-family="Geist" stroke="#000" stroke-dasharray="2 2">dashed</text>"##,
            r#"<clipPath id="c"><text x="5" y="50" font-family="Geist">clip</text></clipPath>"#,
            "<text x=\"5\" y=\"50\" font-family=\"Geist\">\u{10FFFD}</text>",
            // An instance takes its inherited properties from the `<use>`.
            r##"<defs><text id="t" x="5" y="50" font-family="Geist">used</text></defs><use href="#t" font-size="30" text-anchor="middle"/>"##,
            r##"<g id="w"><text x="5" y="50" font-family="Geist">in a used group</text></g><use xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="#w" y="40"/>"##,
            r#"<text x="5" y="50" font-family="Geist" shape-rendering="crispEdges">crisp</text>"#,
            r#"<g shape-rendering="optimizeSpeed"><text x="5" y="50" font-family="Geist">fast</text></g>"#,
            r##"<marker id="m"><circle r="1"/></marker><g marker-start="url(#m)"><text x="5" y="50" font-family="Geist">marked</text></g>"##,
            r##"<marker id="m"><circle r="1"/></marker><text x="5" y="50" font-family="Geist" style="marker-mid:url(#m)">marked</text>"##,
            r##"<text x="5" y="50" font-family="Geist" stroke="#000" letter-spacing="-4">tight</text>"##,
        ] {
            let src = doc(body);
            let out = outline(&src);
            assert_eq!(out.count, 0, "{body}\n{}", out.svg);
            assert_eq!(out.svg, src);
        }
    }

    fn lines(src: &str) -> usize {
        let mut ys: Vec<i32> = ours(src).iter().map(|g| g.2.round() as i32).collect();
        ys.dedup();
        ys.len()
    }

    #[test]
    fn inline_size_wraps_and_pre_line_breaks() {
        let wrapped = doc(
            r#"<text x="200" y="40" font-family="Geist" font-size="20" text-anchor="middle" style="inline-size:120px">one two three four five six</text>"#,
        );
        assert!(lines(&wrapped) >= 3, "{:?}", ours(&wrapped));
        for g in ours(&wrapped) {
            assert!(g.1 > 200.0 - 61.0 && g.1 < 200.0 + 60.0, "{g:?}");
        }
        let broken = doc(
            "<text x=\"5\" y=\"40\" font-family=\"Geist\" font-size=\"20\" white-space=\"pre-line\" line-height=\"30px\">first\n  second  \nthird</text>",
        );
        assert_eq!(lines(&broken), 3);
        let ys: Vec<f32> = ours(&broken).iter().map(|g| g.2).collect();
        assert!(ys.contains(&40.0) && ys.contains(&70.0) && ys.contains(&100.0), "{ys:?}");
        let normal = doc("<text x=\"5\" y=\"40\" font-family=\"Geist\" font-size=\"20\">first\nsecond</text>");
        assert_eq!(lines(&normal), 1);
    }

    #[test]
    fn a_wrapping_text_that_stays_text_says_so() {
        let src = doc(r#"<text id="t" x="5" y="50" dx="2" font-family="Geist" style="inline-size:50px">too long to fit</text>"#);
        let out = outline(&src);
        assert_eq!(out.count, 0);
        assert!(
            out.warnings.iter().any(|w| w.contains("“t”") && w.contains("wrap")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn outlines_over_the_size_cap_leave_the_title_as_text() {
        let long = "Lorem ipsum dolor sit amet ".repeat(70);
        let src = doc(&format!(
            r#"<text id="t" x="5" y="50" font-family="Geist" style="inline-size:390px">{long}</text>"#
        ));
        let out = outline(&src);
        assert_eq!(out.count, 0);
        assert_eq!(out.svg, src);
        assert!(out.warnings.iter().any(|w| w.contains("KiB")), "{:?}", out.warnings);
    }

    #[test]
    fn a_span_keeps_its_own_paint_around_its_glyphs() {
        let out = outline(&doc(
            r##"<text x="5" y="50" font-family="Geist" fill="#fff">a<tspan fill="#f00" style="stroke:#00f">b</tspan>c</text>"##,
        ));
        assert!(out.svg.contains(r##"<g fill="#f00" stroke="#00f"><path"##), "{}", out.svg);
        assert_eq!(out.svg.matches("<path d=").count(), 3, "{}", out.svg);
    }

    #[test]
    fn a_text_shrinks_to_fit_its_box_on_one_line() {
        let long = "A name far too long for the box";
        let shrunk = doc(&format!(
            r#"<text x="20" y="80" font-family="Geist" font-size="40" letter-spacing="2" inline-size="200" data-kerf-fit="shrink">{long}</text>"#
        ));
        let lay = laid(&shrunk).remove(0);
        assert_eq!(lay.lines.len(), 1);
        let w = lay.width();
        assert!(w <= 200.02 && w > 199.0, "{w}");
        assert!(lay.glyphs.iter().all(|g| g.size < 40.0 && (g.y - 80.0).abs() < 0.01));
        // One that fits already is left at its size, and a newline still breaks (pre-line), each line fitted.
        let short = doc(
            r#"<text x="20" y="80" font-family="Geist" font-size="20" inline-size="200" data-kerf-fit="shrink">Short</text>"#,
        );
        assert!(laid(&short)[0].glyphs.iter().all(|g| g.size == 20.0));
        let two = doc(&format!(
            "<text x=\"20\" y=\"80\" font-family=\"Geist\" font-size=\"40\" white-space=\"pre-line\" inline-size=\"200\" data-kerf-fit=\"shrink\">{long}\nshort</text>"
        ));
        let lay = laid(&two).remove(0);
        assert_eq!(lay.lines.len(), 2);
        assert!(lay.width() <= 200.02);
        // Middle-anchored, the shrunk line is centred on its anchor.
        let middle = doc(&format!(
            r#"<text x="200" y="80" font-family="Geist" font-size="40" text-anchor="middle" inline-size="200" data-kerf-fit="shrink">{long}</text>"#
        ));
        let line = &laid(&middle)[0].lines[0];
        assert!((line.x + line.width / 2.0).abs() < 0.01, "{line:?}");
        // Without a box, the mark means nothing; kept as text, it says it does not shrink.
        let unboxed = doc(&format!(
            r#"<text x="20" y="80" font-family="Geist" font-size="40" data-kerf-fit="shrink">{long}</text>"#
        ));
        assert!(laid(&unboxed)[0].glyphs.iter().all(|g| g.size == 40.0));
        let kept = outline(&doc(&format!(
            r#"<text id="t" x="20" y="80" dx="1" font-family="Geist" inline-size="200" data-kerf-fit="shrink">{long}</text>"#
        )));
        assert!(kept.warnings.iter().any(|w| w.contains("shrink")), "{:?}", kept.warnings);
    }

    #[test]
    fn a_user_space_gradient_or_pattern_is_drawn_the_same_on_the_glyphs() {
        let src = doc(concat!(
            r##"<linearGradient id="base" gradientUnits="userSpaceOnUse" x1="0" x2="400"><stop offset="0" stop-color="#f40"/><stop offset="1" stop-color="#04f"/></linearGradient>"##,
            r##"<linearGradient id="g" href="#base"/>"##,
            r##"<pattern id="p" patternUnits="userSpaceOnUse" width="12" height="12"><rect width="6" height="12" fill="#0c8"/></pattern>"##,
            r##"<text x="10" y="90" font-family="Geist" font-size="64" fill="url(#g)">Gradient</text>"##,
            r##"<text x="10" y="180" font-family="Geist" font-size="64" fill="#222" stroke="url(#p)" stroke-width="3">Pattern</text>"##,
        ));
        let out = outline(&src);
        assert_eq!(out.count, 2, "{}", out.svg);
        let (a, b) = (pixels(&src), pixels(&out.svg));
        let differ = a
            .chunks(4)
            .zip(b.chunks(4))
            .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 24))
            .count();
        assert!(differ < 8, "{differ} pixels differ");
        // Measured on the bounding box (by default, or through the chain), it stays text.
        for body in [
            r##"<linearGradient id="g"><stop offset="0" stop-color="red"/></linearGradient><text x="5" y="50" fill="url(#g)">a</text>"##,
            r##"<linearGradient id="b" gradientUnits="objectBoundingBox"/><linearGradient id="g" href="#b"/><text x="5" y="50" fill="url(#g)">a</text>"##,
            r##"<pattern id="p" width="1" height="1"/><text x="5" y="50" fill="url(#p)">a</text>"##,
            // A link across families is not followed: the gradient's own default (the bounding box) holds.
            r##"<pattern id="u" gradientUnits="userSpaceOnUse"/><linearGradient id="g" href="#u"/><text x="5" y="50" fill="url(#g)">a</text>"##,
            r##"<pattern id="p" patternUnits="userSpaceOnUse" patternContentUnits="objectBoundingBox" width="9" height="9"/><text x="5" y="50" fill="url(#p)">a</text>"##,
            r##"<filter id="f"/><text x="5" y="50" fill="url(#f)">a</text>"##,
        ] {
            assert_eq!(outline(&doc(body)).count, 0, "{body}");
        }
    }

    #[test]
    fn each_text_that_stays_text_says_why() {
        let out = outline(&doc(concat!(
            r#"<text id="kept" x="5" y="50" dx="3" font-family="Geist">moved</text>"#,
            r#"<text id="set" x="5" y="90" font-family="Geist">set</text>"#,
        )));
        assert_eq!(out.count, 1);
        assert_eq!(out.kept, vec!["the text “kept” (dx)".to_string()]);
    }

    // ---- text animations (`data-kerf-animate`, `glyph_smil.rs`) ----------------------------------------

    use crate::svg::{text_animation_preset, TextAnimation};

    fn animated(text: &str, preset: &str) -> String {
        let a = text_animation_preset(preset).unwrap().animation;
        animated_with(text, &a)
    }

    fn animated_with(text: &str, a: &TextAnimation) -> String {
        doc(&format!(
            r##"<rect width="400" height="200" fill="#123"/><text id="t" x="20" y="110" font-family="Geist" font-size="60" fill="#fe8" data-kerf-animate='{}'>{text}</text>"##,
            a.to_attribute()
        ))
    }

    /// The same title without the attribute: the text at rest.
    fn at_rest(src: &str) -> String {
        let start = src.find(" data-kerf-animate='").unwrap();
        let end = src[start + 20..].find('\'').unwrap() + start + 21;
        format!("{}{}", &src[..start], &src[end..])
    }

    /// Pixels of `parsed` at `t` that are not the background (a light ink on dark).
    fn ink(parsed: &SvgDoc, t: f64) -> Vec<u8> {
        pixels(&parsed.at(t))
    }

    fn lit(frame: &[u8], columns: std::ops::Range<usize>) -> usize {
        frame
            .chunks(4)
            .enumerate()
            .filter(|(i, p)| columns.contains(&(i % 400)) && p[0] > 160 && p[1] > 160)
            .count()
    }

    #[test]
    fn an_animated_text_is_a_group_of_glyph_groups_that_reads_back_and_restores() {
        let src = animated("Hello", "rise");
        let out = outline(&src);
        assert_eq!(out.count, 1, "{:?}", out.kept);
        assert!(out.warnings.is_empty() && out.kept.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.svg.matches("<animateTransform").count(), 5, "one a letter");
        assert_eq!(out.svg.matches("attributeName=\"opacity\"").count(), 5);
        assert_eq!(
            restore_text(&out.svg).unwrap(),
            src,
            "the animation is generated: restoring drops it"
        );
        let parsed = SvgDoc::parse(&out.svg).unwrap();
        assert_eq!(parsed.sanitized(), out.svg, "the sanitiser leaves it as it is");
        assert!(parsed.warnings().is_empty(), "{:?}", parsed.warnings());
        assert!(parsed.is_animated());
        assert_eq!(parsed.natural_duration(), Some(1.4));
        // Making it again gives the same bytes.
        assert_eq!(outline(&src).svg, out.svg);
    }

    #[test]
    fn a_rising_text_is_hidden_at_the_start_and_is_the_text_at_rest_at_the_end() {
        let src = animated("Hello", "rise");
        let out = outline(&src);
        let parsed = SvgDoc::parse(&out.svg).unwrap();
        let backdrop = pixels(&doc(r##"<rect width="400" height="200" fill="#123"/>"##));
        assert_eq!(ink(&parsed, 0.0), backdrop, "every letter starts transparent");
        let rest = SvgDoc::parse(&outline(&at_rest(&src)).svg).unwrap();
        assert_eq!(
            ink(&parsed, 1.4),
            ink(&rest, 0.0),
            "the last frame is the text as it is without the animation"
        );
        assert_eq!(ink(&parsed, 9.0), ink(&parsed, 1.4), "and it holds");
        // In between, letters are part way: some ink, not all of it, and the first letter leads.
        let mid = ink(&parsed, 0.5);
        let full = ink(&rest, 0.0);
        let (mid_ink, full_ink) = (lit(&mid, 0..400), lit(&full, 0..400));
        assert!(mid_ink > 0 && mid_ink < full_ink, "{mid_ink} of {full_ink}");
        assert!(lit(&mid, 20..80) >= lit(&mid, 200..400), "left to right");
    }

    #[test]
    fn a_typewriter_shows_the_letters_in_order_one_at_a_time() {
        let src = animated("Hello world", "typewriter");
        let out = outline(&src);
        let parsed = SvgDoc::parse(&out.svg).unwrap();
        let rest = SvgDoc::parse(&outline(&at_rest(&src)).svg).unwrap();
        let end = ink(&rest, 0.0);
        let mut last = 0;
        for i in 0..=15 {
            let frame = ink(&parsed, f64::from(i) * 0.1);
            let shown = lit(&frame, 0..400);
            assert!(shown >= last, "letters only appear: {shown} after {last} at {i}");
            last = shown;
        }
        assert_eq!(lit(&ink(&parsed, 0.0), 0..400), 0);
        assert_eq!(lit(&ink(&parsed, 1.5), 0..400), lit(&end, 0..400));
        // Half way through, the left half has its letters and the right half none.
        let half = ink(&parsed, 0.75);
        assert!(
            lit(&half, 20..110) > 0 && lit(&half, 300..400) == 0,
            "{} {}",
            lit(&half, 20..110),
            lit(&half, 300..400)
        );
    }

    #[test]
    fn a_word_or_a_line_moves_as_one_and_a_letter_turns_about_itself() {
        let a = TextAnimation {
            by: AnimateBy::Word,
            ..text_animation_preset("drop_in").unwrap().animation
        };
        let out = outline(&animated_with("one two three", &a));
        assert_eq!(
            out.svg.matches("<animate attributeName=\"opacity\"").count(),
            3,
            "{}",
            out.svg
        );
        let lines = TextAnimation {
            by: AnimateBy::Line,
            ..text_animation_preset("drop_in").unwrap().animation
        };
        let src = doc(&format!(
            r##"<text id="t" x="20" y="60" font-family="Geist" font-size="30" style="white-space:pre-line" data-kerf-animate='{}'>one
two
three</text>"##,
            lines.to_attribute()
        ));
        let out = outline(&src);
        assert_eq!(out.count, 1, "{:?}", out.kept);
        assert_eq!(
            out.svg.matches("<animate attributeName=\"opacity\"").count(),
            3,
            "a unit a line"
        );
        // A letter that spins in turns about its own middle: its ink stays where the letter is.
        let src = animated("I", "spin_in");
        let parsed = SvgDoc::parse(&outline(&src).svg).unwrap();
        let rest = pixels(&SvgDoc::parse(&outline(&at_rest(&src)).svg).unwrap().at(0.0));
        let box_of = |frame: &[u8]| {
            let (mut x0, mut y0, mut x1, mut y1) = (400usize, 200usize, 0usize, 0usize);
            for (i, p) in frame.chunks(4).enumerate() {
                if p[0] > 120 && p[1] > 120 {
                    let (x, y) = (i % 400, i / 400);
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
            ((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0)
        };
        let (cx, cy) = box_of(&rest);
        let mid = pixels(&parsed.at(0.45));
        assert!(lit(&mid, 0..400) > 0, "part way in there is a letter");
        let (mx, my) = box_of(&mid);
        assert!(
            (mx - cx).abs() < 6.0 && (my - cy).abs() < 14.0,
            "centre ({mx}, {my}) vs ({cx}, {cy})"
        );
    }

    #[test]
    fn a_blur_comes_into_focus_and_the_text_at_rest_is_sharp() {
        let src = animated("Blur", "blur_in");
        let out = outline(&src);
        assert_eq!(out.count, 1, "{:?}", out.kept);
        assert!(out.svg.contains("<feGaussianBlur"), "{}", out.svg);
        let parsed = SvgDoc::parse(&out.svg).unwrap();
        assert!(parsed.warnings().is_empty(), "{:?}", parsed.warnings());
        let rest = pixels(&SvgDoc::parse(&outline(&at_rest(&src)).svg).unwrap().at(0.0));
        let end = ink(&parsed, parsed.natural_duration().unwrap() + 1.0);
        let differ = rest
            .chunks(4)
            .zip(end.chunks(4))
            .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 6))
            .count();
        assert!(differ < 30, "{differ} pixels differ between sharp and the blur at zero");
        // Part way, a letter is soft: its edge pixels are in between.
        let mid = ink(&parsed, 0.6);
        let soft = mid
            .chunks(4)
            .filter(|p| p[0] > 40 && p[0] < 140 && p[1] > 40 && p[1] < 140)
            .count();
        assert!(lit(&mid, 0..400) > 0 || soft > 0);
    }

    #[test]
    fn a_wiggle_moves_every_letter_on_its_own_the_same_way_every_time() {
        let src = animated("Wiggle", "wiggle");
        let out = outline(&src);
        assert_eq!(out.count, 1);
        assert_eq!(outline(&src).svg, out.svg);
        let parsed = SvgDoc::parse(&out.svg).unwrap();
        assert!(parsed.warnings().is_empty(), "{:?}", parsed.warnings());
        let rest = pixels(&SvgDoc::parse(&outline(&at_rest(&src)).svg).unwrap().at(0.0));
        assert_eq!(ink(&parsed, 0.0), rest, "it starts at rest");
        assert_eq!(ink(&parsed, 3.0), rest, "and settles");
        assert_ne!(ink(&parsed, 1.3), rest, "in between it is off");
        let mut other = text_animation_preset("wiggle").unwrap().animation;
        other.wiggle.as_mut().unwrap().seed = 8;
        assert_ne!(outline(&animated_with("Wiggle", &other)).svg, out.svg);
    }

    #[test]
    fn an_animated_text_that_cannot_be_outlined_stays_text_and_says_the_animation_will_not_play() {
        let a = text_animation_preset("rise").unwrap().animation.to_attribute();
        // Positioned: stays text.
        let out = outline(&doc(&format!(
            r#"<text id="t" x="5" y="50" dx="2" font-family="Geist" data-kerf-animate='{a}'>kept</text>"#
        )));
        assert_eq!(out.count, 0);
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        assert!(out.warnings[0].contains("its animation does not play"), "{:?}", out.warnings);
        // A rule that could match what the animation makes: stays text.
        let out = outline(&doc(&format!(
            r#"<style>g path{{fill:red}}</style><text id="t" x="5" y="50" font-family="Geist" data-kerf-animate='{a}'>kept</text>"#
        )));
        assert_eq!(out.count, 0, "{}", out.svg);
        assert!(out.kept[0].contains("style rule"), "{:?}", out.kept);
        // The same rule leaves an unanimated text alone only if it did already: unchanged behaviour.
        let still = outline(&doc(
            r#"<style>g path{fill:red}</style><text id="t" x="5" y="50" font-family="Geist">kept</text>"#,
        ));
        assert_eq!(still.count, 0);
        // An attribute that is not ours: stays text, saying so.
        let out = outline(&doc(
            r#"<text id="t" x="5" y="50" font-family="Geist" data-kerf-animate='{"by":"letter"}'>kept</text>"#,
        ));
        assert_eq!(out.count, 0);
        assert!(out.kept[0].contains("data-kerf-animate does not read"), "{:?}", out.kept);
        let out = outline(&doc(
            r#"<text id="t" x="5" y="50" font-family="Geist" data-kerf-animate='{"properties":{}}'>kept</text>"#,
        ));
        assert!(out.kept[0].contains("changes nothing"), "{:?}", out.kept);
    }

    #[test]
    fn an_animation_that_would_take_the_title_over_its_cap_leaves_the_text_as_text() {
        let long = "word ".repeat(1_200);
        let out = outline(&animated(&long, "rise"));
        assert_eq!(out.count, 0);
        assert!(out.warnings[0].contains("its animation does not play"), "{:?}", out.warnings);
        // By word the units are fewer, but the glyphs are what weigh: still refused, never half done.
        let a = TextAnimation {
            by: AnimateBy::Word,
            ..text_animation_preset("fade_in").unwrap().animation
        };
        let out = outline(&animated_with(&long, &a));
        assert_eq!(out.count, 0);
        assert!(!out.svg.contains("kf-blur"), "nothing half done");
    }

    #[test]
    fn two_animated_texts_get_blur_filters_of_their_own_and_the_ids_do_not_clash() {
        let a = text_animation_preset("blur_in").unwrap().animation.to_attribute();
        let src = doc(&format!(
            r#"<filter id="kf-blur-0"/><text id="a" x="5" y="50" font-family="Geist" font-size="30" data-kerf-animate='{a}'>ab</text><text id="b" x="5" y="120" font-family="Geist" font-size="30" data-kerf-animate='{a}'>cd</text>"#
        ));
        let out = outline(&src);
        assert_eq!(out.count, 2);
        let ids: Vec<&str> = out
            .svg
            .match_indices("<filter id=\"")
            .map(|(i, _)| &out.svg[i + 12..i + 12 + 10])
            .collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(ids.len(), 5, "the document's own filter and four letters: {ids:?}");
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        let parsed = SvgDoc::parse(&out.svg).unwrap();
        assert_eq!(parsed.sanitized(), out.svg, "no id was dropped");
    }

    #[test]
    fn a_long_animation_of_many_units_is_refused_before_it_is_sampled() {
        let mut slow = text_animation_preset("rise").unwrap().animation;
        slow.delay = 30.0;
        slow.duration = 30.0;
        // 490 units for a minute is 3.5 million amounts: more than a document is given.
        let text = "iiii lll ".repeat(70);
        let out = outline(&animated_with(&text, &slow));
        assert_eq!(out.count, 0);
        assert!(out.kept[0].contains("too long or too many units"), "{:?}", out.kept);
        // By word it is 140 units, well inside.
        slow.by = AnimateBy::Word;
        assert_eq!(outline(&animated_with(&text, &slow)).count, 1);
    }
}
