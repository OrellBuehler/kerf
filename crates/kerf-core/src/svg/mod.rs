//! Animated SVG overlays: the sanitiser, the SMIL subset, and the presets.
//!
//! An overlay stores **sanitised source** ([`sanitize`]); [`SvgDoc`] reads it once and answers
//! [`SvgDoc::at`] with a *static* SVG for one moment of the document's own clock — animation
//! elements cut out, the animated attributes spliced in. A renderer (resvg, for every frame path)
//! never sees an animation, so a preview and an export of the same moment are the same picture.
//!
//! **Splicing, not re-serialising.** Everything is a byte range of the original text: the document
//! is read twice at most (the sanitiser, then the animation pass) and `at` only deletes ranges and
//! replaces attribute values, so ids, formatting, comments, CSS and every attribute that is not
//! animated come out byte for byte. A CSS property (`opacity`, `fill`, `visibility`…) is written by
//! appending `;prop:value` to the element's `style`, because a rule in a `<style>` element beats a
//! presentation attribute and an inline declaration beats the rule; geometry attributes are
//! replaced or inserted as attributes.
//!
//! **The subset** (anything else is a warning, never a failure): `animate`, `animateColor`
//! (= `animate`), `set` and `animateTransform` (translate, scale, rotate with a centre, skewX,
//! skewY). `begin` / `end` take clock values, `id.begin` / `id.end` syncbases with offsets and
//! `indefinite`; events, `repeat()`, `accessKey()` and wallclock never fire. `dur`, `repeatCount`,
//! `repeatDur`, `min`, `max`, `fill`, `restart`, `values`, `from` / `to` / `by`, `keyTimes`,
//! `calcMode` (linear, discrete, paced, spline with `keySplines`), `additive`, `accumulate` and the
//! sandwich (by begin time, then document order) are evaluated. `animateMotion` is dropped with a
//! warning, as is `animate` on `transform` and CSS `@keyframes`.

mod anim;
mod cost;
mod curve;
mod glyph_smil;
pub(crate) mod images;
mod outline;
mod presets;
pub mod raster;
mod rules;
mod sanitize;
pub(crate) mod splice;
mod text_animation;
mod text_box;
mod values;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use roxmltree::{Document, Node, NodeId, NodeType};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use anim::{Calc, Resolver, Restart, Sample, Spec, TimeItem, Timing, INF};
use sanitize::{escape_attr, is_attribute_name, plain_attribute, value_range, SVG_NS, XLINK_NS};
use values::{TransformType, Value};

pub use outline::restore_text;

/// Whether a title holds any text set as glyph outlines (`<g data-kerf-text="">`).
pub fn has_outlines(svg: &str) -> bool {
    svg.contains(outline::TEXT_MARK)
}

/// The attribute of a `<text>` that holds its animation (JSON of a [`TextAnimation`]).
pub const ANIMATE_ATTRIBUTE: &str = text_animation::ANIMATE_MARK;
pub use curve::{curve_preset, curve_presets, Curve, CurvePreset, Curves};
pub use presets::{check_params, preset_infos, render_headline, render_preset, SvgPresetInfo, SvgPresetParamInfo, DEFAULT_FRAME};
pub use text_animation::{
    set_text_animation, text_animation_of, text_animation_preset, text_animation_presets, AnimateBy, Easing, Offset, Properties,
    RangeSelector, Shape, TextAnimation, TextAnimationPreset, Wiggle,
};
pub use text_box::{set_text_box, text_box_of, TextBox, TextFit, MAX_BOX_WIDTH};

/// What [`SvgDoc::parse`] says of artwork with a text box (`inline-size`, `data-kerf-fit`) and no outlines:
/// custom artwork is drawn as written, so its box is not honoured until its text is converted.
pub const BOX_NOTE: &str = "a text box (inline-size, data-kerf-fit) is drawn only when the title's text is glyph outlines: \
                            this artwork draws its text with its fonts, running on, until its text is converted to outlines \
                            (convert_title_text, or Draw as outlines in the Inspector)";

/// What [`SvgDoc::parse`] says of artwork with a text animation (`data-kerf-animate`) and no outlines: the
/// animation is compiled for glyph outlines only, so this artwork's text stays still until it is converted.
pub const ANIMATE_NOTE: &str = "a text animation (data-kerf-animate) plays only when the title's text is glyph outlines: \
                                this artwork draws its text with its fonts, still, until its text is converted to outlines \
                                (convert_title_text, or Draw as outlines in the Inspector)";

/// Whether sanitised `src` gives a text an animation but holds no outlines.
fn has_unoutlined_animation(src: &str) -> bool {
    src.contains(text_animation::ANIMATE_MARK) && !src.contains(outline::GLYPHS_MARK)
}

/// Whether sanitised `src` gives a text a box (an `inline-size` other than `auto`, in an attribute, a
/// `style` or a sheet) but holds no outlines.
fn has_unoutlined_box(src: &str) -> bool {
    !src.contains(outline::GLYPHS_MARK)
        && src.match_indices("inline-size").any(|(i, m)| {
            let value = src[i + m.len()..].trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '=' | '"' | '\''));
            !value.get(..4).is_some_and(|v| v.eq_ignore_ascii_case("auto"))
        })
}

/// Whether sanitised source could move at all: whether it holds an animation element (`animate`,
/// `animateColor`, `animateTransform`, `animateMotion` or `set`, with or without a prefix). A cheap
/// text test that needs no parse, and that errs on the side of "yes" (a comment that says
/// "animate" counts): **`false` is a promise** that [`SvgDoc::at`] gives the same picture at every
/// moment, which is what lets the engine keep such a title as one PNG instead of one a frame.
pub fn may_animate(src: &str) -> bool {
    src.contains("animate") || src.contains("<set") || src.contains(":set")
}

pub(crate) const MAX_BYTES: usize = 128 * 1024;
pub(crate) const MAX_ELEMENTS: usize = 20_000;
pub(crate) const MAX_DEPTH: usize = 64;
const MAX_NEST: usize = 128;
const MAX_EXPANDED: usize = 100_000;
pub const MAX_ANIMATIONS: usize = 2_000;
pub const MAX_VALUES: usize = 1_000;

pub(crate) fn xml_options<'a>() -> roxmltree::ParsingOptions<'a> {
    roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: 200_000,
        ..Default::default()
    }
}

pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidArgument(msg.into())
}

/// What `check_svg` found: the text that would be stored, and what could not be honoured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SvgCheck {
    /// The sanitised source (what an overlay stores and what is rendered).
    pub svg: String,
    pub warnings: Vec<String>,
    /// When the animations have all finished, in document seconds; `None` for a still or a
    /// document that loops forever.
    pub natural_duration: Option<f64>,
}

/// Sanitise and read `src` without storing it.
pub fn check_svg(src: &str) -> Result<SvgCheck> {
    let doc = SvgDoc::parse(src)?;
    Ok(SvgCheck {
        svg: doc.src,
        warnings: doc.warnings,
        natural_duration: doc.natural,
    })
}

/// A sanitised SVG and its animations, ready to be sampled at any moment.
#[derive(Debug, Clone)]
pub struct SvgDoc {
    src: String,
    warnings: Vec<String>,
    anims: Vec<Anim>,
    /// Animation elements to cut out, in order.
    cuts: Vec<Range<usize>>,
    targets: Vec<Target>,
    slots: Vec<Slot>,
    natural: Option<f64>,
    /// The most pixels any one embedded image has (what a frame's decode has to hold at once).
    widest_image: u64,
}

#[derive(Debug, Clone)]
struct Target {
    /// Where an attribute can be inserted (right after the tag name).
    name_end: usize,
    style: Option<(Range<usize>, char)>,
}

/// One animated attribute of one element, with every animation that drives it.
#[derive(Debug, Clone)]
struct Slot {
    target: usize,
    attr: String,
    css: bool,
    existing: Option<(Range<usize>, char)>,
    base: Value,
    anims: Vec<usize>,
}

#[derive(Debug, Clone)]
struct Anim {
    set: bool,
    transform: Option<TransformType>,
    values: Vec<Value>,
    /// A `to` animation: the underlying value is the first of the two.
    to_only: bool,
    key_times: Option<Vec<f64>>,
    key_splines: Option<Vec<[f64; 4]>>,
    calc: Calc,
    additive: bool,
    accumulate: bool,
    timing: Timing,
}

impl Anim {
    fn apply(&self, under: Value, s: Sample) -> Value {
        if self.set {
            return self.values[0].clone();
        }
        let owned;
        let vals: &[Value] = if self.to_only {
            let from = match self.transform {
                Some(ty) => Value::List(ty.identity()),
                None => under.clone(),
            };
            owned = vec![from, self.values[0].clone()];
            &owned
        } else {
            &self.values
        };
        let mut v = anim::curve(
            vals,
            self.key_times.as_deref(),
            self.key_splines.as_deref(),
            self.calc,
            s.frac,
        );
        // SMIL: accumulate is ignored for an animation with only a `to`.
        if self.accumulate && !self.to_only && s.iter > 0.0 {
            if let Some(sum) = vals
                .last()
                .and_then(|last| values::scale(last, s.iter))
                .and_then(|more| values::add(&v, &more))
            {
                v = sum;
            }
        }
        if let (Some(ty), Value::List(n)) = (self.transform, &v) {
            v = Value::Transform(ty.format(n));
        }
        if self.additive && !self.to_only {
            values::add(&under, &v).unwrap_or(v)
        } else {
            v
        }
    }
}

/// What reading one animation element gave, before its timing is resolved.
struct Parsed {
    spec: Spec,
    target: NodeId,
    attr: String,
    anim: Anim,
}

impl SvgDoc {
    /// Sanitise `src` and read its animations. Bad XML, a root that is not `<svg>`, a size
    /// resvg cannot use, or any cap exceeded is an [`Error::InvalidArgument`]; whatever merely cannot
    /// be honoured is a [`SvgDoc::warnings`] entry.
    pub fn parse(src: &str) -> Result<SvgDoc> {
        let clean = sanitize::sanitize(src)?;
        let mut warnings = clean.warnings;
        let text = clean.text;
        if has_unoutlined_box(&text) {
            warnings.push(BOX_NOTE.to_string());
        }
        if has_unoutlined_animation(&text) {
            warnings.push(ANIMATE_NOTE.to_string());
        }
        let doc = Document::parse_with_options(&text, xml_options()).map_err(|e| invalid(format!("not valid XML: {e}")))?;
        if !has_size(&doc.root_element()) {
            return Err(invalid(
                "the <svg> needs a viewBox (or a width and height in pixels) so it has a shape to fit",
            ));
        }
        let mut by_id: HashMap<String, NodeId> = HashMap::new();
        for node in doc.descendants().filter(|n| n.node_type() == NodeType::Element) {
            // The sanitiser leaves each id on one element, so first and only are the same.
            if let Some(id) = node.attribute("id") {
                by_id.entry(id.to_string()).or_insert(node.id());
            }
        }
        let widest_image = check_expansion(&doc, &by_id)?;
        let usvg_opts = resvg::usvg::Options {
            image_href_resolver: raster::image_resolver(),
            ..resvg::usvg::Options::default()
        };
        let tree = resvg::usvg::Tree::from_xmltree(&doc, &usvg_opts).map_err(|e| invalid(format!("not a drawable SVG: {e}")))?;
        cost::check_nominal(&tree)?;

        let mut reader = Reader {
            warnings: &mut warnings,
            by_id,
            parsed: Vec::new(),
            cuts: Vec::new(),
        };
        let mut last_cut = 0;
        for node in doc.descendants().filter(|n| n.node_type() == NodeType::Element) {
            if !is_animation(&node) || node.range().start < last_cut {
                continue;
            }
            last_cut = node.range().end;
            reader.cuts.push(node.range());
            if reader.parsed.len() >= MAX_ANIMATIONS {
                return Err(invalid(format!("more than {MAX_ANIMATIONS} animations")));
            }
            reader.read(node)?;
        }
        let Reader { parsed, cuts, .. } = reader;

        let specs: Vec<Spec> = parsed.iter().map(|p| p.spec.clone()).collect();
        let ids: HashMap<String, usize> = specs
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.id.clone().map(|id| (id, i)))
            .collect();
        let (intervals, timing_warnings) = Resolver::new(&specs, &ids).resolve();
        warnings.extend(timing_warnings);

        let natural = natural_duration(&specs, &intervals);
        let mut targets: Vec<Target> = Vec::new();
        let mut target_of: HashMap<NodeId, usize> = HashMap::new();
        let mut slots: Vec<Slot> = Vec::new();
        let mut slot_of: HashMap<(usize, String), usize> = HashMap::new();
        let mut anims: Vec<Anim> = Vec::new();
        for (p, iv) in parsed.into_iter().zip(intervals) {
            let mut anim = p.anim;
            anim.timing = Timing::new(&p.spec, iv);
            let node = doc.get_node(p.target).expect("targets come from this document");
            let t = *target_of.entry(p.target).or_insert_with(|| {
                targets.push(target_of_node(&text, &node));
                targets.len() - 1
            });
            let idx = *slot_of.entry((t, p.attr.clone())).or_insert_with(|| {
                slots.push(slot_for(&text, &node, t, &p.attr));
                slots.len() - 1
            });
            slots[idx].anims.push(anims.len());
            anims.push(anim);
        }
        let mut seen = Vec::new();
        warnings.retain(|w| {
            let fresh = !seen.contains(w);
            if fresh {
                seen.push(w.clone());
            }
            fresh
        });
        drop(doc);
        Ok(SvgDoc {
            src: text,
            warnings,
            anims,
            cuts,
            targets,
            slots,
            natural,
            widest_image,
        })
    }

    /// The document with its text set as glyph outlines where it can be, with the faces of `db` (see
    /// `outline.rs`), and read again; what only outlines honour and could not be is added to the
    /// warnings. A document whose outlines do not read (over a cap the text was under) stays as it was.
    /// Slow (the text engine, and the sanitiser once more when anything was outlined): not for under the
    /// project lock.
    pub(crate) fn outlined(self, db: &fontdb::Database) -> Result<SvgDoc> {
        Ok(self.outlined_counted(db)?.0)
    }

    /// [`SvgDoc::outlined`], with how many texts are outlines now and each that stays text and why. The
    /// generic [`BOX_NOTE`] and [`ANIMATE_NOTE`] go: what is outlined draws its box and plays its animation, and a text
    /// that is not says so itself.
    pub(crate) fn outlined_counted(mut self, db: &fontdb::Database) -> Result<(SvgDoc, usize, Vec<String>)> {
        self.warnings.retain(|w| w != BOX_NOTE && w != ANIMATE_NOTE);
        let out = outline::outline_text(&self.src, db)?;
        let (mut doc, count) = match out.count {
            0 => (self, 0),
            n => match SvgDoc::parse(&out.svg) {
                Ok(mut doc) => {
                    doc.warnings = self.warnings;
                    (doc, n)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "a title's outlines did not read; its text is drawn with its fonts");
                    (self, 0)
                }
            },
        };
        doc.warnings.extend(out.warnings);
        Ok((doc, count, out.kept))
    }

    /// The sanitised source.
    pub fn sanitized(&self) -> &str {
        &self.src
    }

    /// What could not be honoured, in the order it was met.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// When every animation has finished, in document seconds. `None` for a still, or when
    /// something repeats without end.
    pub fn natural_duration(&self) -> Option<f64> {
        self.natural
    }

    /// How long a title made from this document runs when nobody says: until its animations have
    /// finished, held to 2-30 s, and four seconds for a still or something that loops forever.
    pub fn default_span(&self) -> f64 {
        self.natural.map_or(4.0, |n| n.clamp(2.0, 30.0))
    }

    /// Whether anything in the document moves.
    pub fn is_animated(&self) -> bool {
        !self.slots.is_empty()
    }

    /// The most pixels any one embedded image of the document has: with a frame, what one draw
    /// has to hold at once (decoded, four bytes a pixel, twice while it is converted).
    pub fn widest_image_pixels(&self) -> u64 {
        self.widest_image
    }

    /// The document as a static SVG at `doc_t` seconds of its own clock: animation elements removed,
    /// each animated attribute carrying its value at that moment.
    pub fn at(&self, doc_t: f64) -> String {
        let t = if doc_t.is_finite() { doc_t.max(0.0) } else { 0.0 };
        let mut cuts: Vec<(Range<usize>, String)> = self.cuts.iter().map(|r| (r.clone(), String::new())).collect();
        let mut adds: Vec<(String, String)> = vec![(String::new(), String::new()); self.targets.len()];
        for slot in &self.slots {
            let Some(value) = self.eval(slot, t) else {
                continue;
            };
            let text = values::format(&value);
            if slot.css {
                adds[slot.target].1.push_str(&format!(";{}:{}", slot.attr, text));
            } else if let Some((range, quote)) = &slot.existing {
                cuts.push((range.clone(), escape_attr(&text, *quote)));
            } else {
                adds[slot.target]
                    .0
                    .push_str(&format!(" {}=\"{}\"", slot.attr, escape_attr(&text, '"')));
            }
        }
        for (i, (attrs, style)) in adds.iter_mut().enumerate() {
            let target = &self.targets[i];
            if !style.is_empty() {
                match &target.style {
                    Some((range, quote)) => {
                        // ";opacity:.5" after nothing (or after a declaration that already ended in
                        // one) is an empty declaration, which resvg's CSS reader drops with the rest.
                        let before = &self.src[range.clone()];
                        let add = if before.trim().is_empty() || before.trim_end().ends_with(';') {
                            style.trim_start_matches(';')
                        } else {
                            style.as_str()
                        };
                        cuts.push((range.end..range.end, escape_attr(add, *quote)));
                    }
                    None => attrs.push_str(&format!(" style=\"{}\"", escape_attr(style.trim_start_matches(';'), '"'))),
                }
            }
            if !attrs.is_empty() {
                cuts.push((target.name_end..target.name_end, std::mem::take(attrs)));
            }
        }
        sanitize::apply(&self.src, cuts)
    }

    /// The value a slot has at `t`, or `None` when nothing drives it then.
    fn eval(&self, slot: &Slot, t: f64) -> Option<Value> {
        let mut active: Vec<(f64, usize, Sample)> = slot
            .anims
            .iter()
            .filter_map(|&i| self.anims[i].timing.sample(t).map(|s| (s.begin, i, s)))
            .collect();
        if active.is_empty() {
            return None;
        }
        active.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut acc = slot.base.clone();
        for (_, i, s) in active {
            acc = self.anims[i].apply(acc, s);
        }
        Some(acc)
    }
}

/// A shape of its own: a `viewBox`, or a pixel `width` and `height`. Without one resvg sizes the
/// canvas to whatever is drawn, which an animation would change from frame to frame.
fn has_size(root: &Node) -> bool {
    let positive = |s: &str, unit_ok: bool| match values::parse(values::Kind::Len, s) {
        Value::Len(v, u) => v > 0.0 && (unit_ok || u != "%"),
        _ => false,
    };
    if let Some(Value::List(l)) = root.attribute("viewBox").map(|v| values::parse(values::Kind::List, v)) {
        return l.len() == 4 && l[2] > 0.0 && l[3] > 0.0;
    }
    matches!((root.attribute("width"), root.attribute("height")), (Some(w), Some(h)) if positive(w, false) && positive(h, false))
}

/// The shape of a stored title in user units: its `viewBox`, else its pixel `width` and `height` (the two
/// things [`has_size`] accepts). `None` for a document that has neither.
pub fn intrinsic_size(src: &str) -> Option<(f64, f64)> {
    let doc = Document::parse_with_options(src, xml_options()).ok()?;
    let root = doc.root_element();
    if let Some(Value::List(l)) = root.attribute("viewBox").map(|v| values::parse(values::Kind::List, v)) {
        return (l.len() == 4 && l[2] > 0.0 && l[3] > 0.0).then(|| (l[2], l[3]));
    }
    let len = |name: &str| match values::parse(values::Kind::Len, root.attribute(name)?) {
        Value::Len(v, u) if v > 0.0 && u != "%" => Some(v),
        _ => None,
    };
    Some((len("width")?, len("height")?))
}

/// The link of an element the way resvg reads it: the unprefixed `href`, then `xlink:href` (SVG 2
/// lets the first win whatever their order; reading them the other way round let a cheap target
/// stand in for an expensive one here and be drawn as the expensive one).
fn href_of<'a>(node: &Node<'a, '_>) -> Option<&'a str> {
    plain_attribute(node, "href").or_else(|| node.attribute((XLINK_NS, "href")))
}

/// The ids a property value points at with `url(#id)` (a `filter` may name several).
fn url_ids(value: &str) -> Vec<&str> {
    // `URL(#a)` is as much a reference as `url(#a)`: found in a lower-cased copy (same byte offsets),
    // read from the original (an id is case-sensitive).
    let lower = value.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("url(") {
        let from = at + i + 4;
        let end = value[from..].find(')').map_or(value.len(), |e| from + e);
        let arg = value[from..end].trim().trim_matches(['"', '\'']).trim();
        if let Some(id) = arg.strip_prefix('#') {
            out.push(id);
        }
        at = end;
    }
    out
}

/// The properties whose `url(#id)` makes the element draw another one (a paint server, a mask, a
/// clip, a filter, a marker): each use costs what the target does, which the expansion guard counts.
const REFERENCING: [&str; 9] = [
    "fill",
    "stroke",
    "mask",
    "clip-path",
    "filter",
    // The `marker` shorthand is read by usvg in CSS (a `style` attribute, a rule) as the three below.
    "marker",
    "marker-start",
    "marker-mid",
    "marker-end",
];

/// The elements `node` draws through `url(#id)` properties: in an attribute, in `style` (every
/// declaration of it: the last wins in resvg, and the first is not the one to count), and in the
/// `<style>` rules that could apply to it (`rules`).
fn referenced(node: &Node, by_id: &HashMap<String, NodeId>, rules: &rules::Rules) -> Vec<NodeId> {
    let style = node.attribute("style").unwrap_or("");
    let mut ids: Vec<&str> = Vec::new();
    for prop in REFERENCING {
        if let Some(v) = node.attribute(prop) {
            ids.extend(url_ids(v));
        }
        for (_, v) in style
            .split(';')
            .filter_map(|d| d.split_once(':'))
            .filter(|(p, _)| p.trim().eq_ignore_ascii_case(prop))
        {
            ids.extend(url_ids(v));
        }
    }
    ids.extend(rules.refs_for(node));
    ids.into_iter().filter_map(|id| by_id.get(id).copied()).collect()
}

/// What a document costs once its `<use>` (and `feImage`) references are expanded — the way
/// resvg draws it. A chain of `<use>`s that each reference the next twice is a few hundred bytes
/// that draw a million nodes (usvg stops at a million, per frame is far too late), so the
/// expanded size and nesting are counted from the XML first and a bomb is refused. Embedded image
/// pixels are counted the same way (each draw decodes the file again). Returns the pixels of the
/// widest single image.
///
/// This reads the document **as stored**: the properties that make an element draw another one are
/// kept out of everything that could change them afterwards — an animation may not drive
/// `mask` / `filter` / `clip-path` / `marker*` / `id` / `class` or set a `url()` but a gradient
/// `fill` / `stroke` (`sanitize`) — and what a `<style>` rule points an element at is counted on
/// every element the rule could apply to (`rules`).
fn check_expansion(doc: &Document, by_id: &HashMap<String, NodeId>) -> Result<u64> {
    struct Walk<'a, 'input> {
        doc: &'a Document<'input>,
        by_id: &'a HashMap<String, NodeId>,
        rules: rules::Rules,
        /// (nodes drawn, nesting, image pixels decoded) of the subtree, each reference expanded.
        memo: HashMap<NodeId, (usize, usize, u64)>,
        busy: HashSet<NodeId>,
        widest: u64,
    }
    impl Walk<'_, '_> {
        fn visit(&mut self, node: Node, nest: usize) -> Result<(usize, usize, u64)> {
            let too_deep = || invalid(format!("the SVG nests deeper than {MAX_NEST} levels once <use> is expanded"));
            if let Some(&(w, d, p)) = self.memo.get(&node.id()) {
                return if nest + d > MAX_NEST { Err(too_deep()) } else { Ok((w, d, p)) };
            }
            if nest > MAX_NEST {
                return Err(too_deep());
            }
            if !self.busy.insert(node.id()) {
                return Err(invalid("an href (or url()) refers back to itself"));
            }
            let (mut weight, mut depth) = (1usize, 1usize);
            let mut pixels = 0u64;
            if matches!(node.tag_name().name(), "image" | "feImage") {
                pixels = href_of(&node)
                    .filter(|h| !h.trim_start().starts_with('#'))
                    .and_then(|h| images::check_href(h).ok())
                    .unwrap_or(0);
                self.widest = self.widest.max(pixels);
            }
            let mut next: Vec<Node> = node.children().filter(Node::is_element).collect();
            // A paint server or filter may take its content from another (`href`); a gradient or
            // pattern chain is as cheap as it looks, but a pattern that borrows a heavy one is not.
            let name = node.tag_name().name();
            if matches!(
                name,
                "use" | "feImage" | "pattern" | "linearGradient" | "radialGradient" | "filter"
            ) {
                let target = href_of(&node)
                    .and_then(|h| h.trim().strip_prefix('#'))
                    .and_then(|id| self.by_id.get(id))
                    .and_then(|id| self.doc.get_node(*id))
                    // usvg breaks a loop of gradients (each only ever lends its stops, so a ring of
                    // them is as cheap as it looks); every other loop it draws is one that grows.
                    .filter(|t| !(matches!(name, "linearGradient" | "radialGradient") && self.busy.contains(&t.id())));
                next.extend(target);
            }
            next.extend(
                referenced(&node, self.by_id, &self.rules)
                    .into_iter()
                    .filter_map(|id| self.doc.get_node(id)),
            );
            for n in next {
                let (w, d, p) = self.visit(n, nest + 1)?;
                weight = weight.saturating_add(w);
                depth = depth.max(d + 1);
                pixels = pixels.saturating_add(p);
                // Stop at the first excess: a rule that points every element at a few hundred more
                // would otherwise be added up in full before it is refused.
                if weight > MAX_EXPANDED {
                    break;
                }
            }
            self.busy.remove(&node.id());
            if weight > MAX_EXPANDED {
                return Err(invalid(format!(
                    "the SVG draws more than {MAX_EXPANDED} nodes once <use> is expanded"
                )));
            }
            if pixels > images::MAX_FRAME_IMAGE_PIXELS {
                return Err(invalid(format!(
                    "the SVG draws more than {} megapixels of embedded images once <use> is expanded",
                    images::MAX_FRAME_IMAGE_PIXELS / 1_000_000
                )));
            }
            self.memo.insert(node.id(), (weight, depth, pixels));
            Ok((weight, depth, pixels))
        }
    }
    let mut walk = Walk {
        doc,
        by_id,
        rules: rules::Rules::read(doc),
        memo: HashMap::new(),
        busy: HashSet::new(),
        widest: 0,
    };
    walk.visit(doc.root_element(), 0)?;
    Ok(walk.widest)
}

fn is_animation(node: &Node) -> bool {
    node.tag_name().namespace() == Some(SVG_NS)
        && matches!(
            node.tag_name().name(),
            "animate" | "animateColor" | "animateTransform" | "animateMotion" | "set"
        )
}

/// Past which every animation has settled; `None` when one loops forever or nothing animates.
fn natural_duration(specs: &[Spec], intervals: &[Vec<anim::Interval>]) -> Option<f64> {
    let mut longest: Option<f64> = None;
    for (spec, ivs) in specs.iter().zip(intervals) {
        let Some(last) = ivs.last() else { continue };
        if spec.loops_forever() {
            return None;
        }
        let settled = if last.end.is_finite() { last.end } else { last.begin };
        longest = Some(longest.map_or(settled, |l| l.max(settled)));
    }
    longest.filter(|d| *d > 0.0)
}

fn target_of_node(src: &str, node: &Node) -> Target {
    let range = node.range();
    let name_end = src[range.start + 1..]
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .map_or(range.end, |i| range.start + 1 + i);
    let style = node
        .attributes()
        .find(|a| a.name() == "style" && a.namespace().is_none())
        .and_then(|a| value_range(src, a.range()));
    Target { name_end, style }
}

fn slot_for(src: &str, node: &Node, target: usize, attr: &str) -> Slot {
    let css = values::is_css_property(attr);
    let transform = matches!(attr, "transform" | "gradientTransform" | "patternTransform");
    let existing = node
        .attributes()
        .find(|a| a.name() == attr && matches!(a.namespace(), None | Some(XLINK_NS)))
        .and_then(|a| value_range(src, a.range()));
    let declared = if css {
        node.attribute("style").and_then(|s| style_declaration(s, attr))
    } else {
        None
    };
    // What an inherited property (a fill, a font size, a visibility) is when the element does not
    // say: the nearest ancestor's, as a browser would — the animation's `from` / `by` and its
    // additive sum start from that, not from the initial value.
    let inherited = || {
        node.ancestors().skip(1).filter(Node::is_element).find_map(|a| {
            a.attribute("style")
                .and_then(|s| style_declaration(s, attr))
                .or_else(|| a.attribute(attr).map(str::to_string))
        })
    };
    let text = declared
        .or_else(|| node.attribute(attr).map(str::to_string))
        .or_else(|| (css && values::is_inherited(attr)).then(inherited).flatten())
        .or_else(|| values::default_text(attr).map(str::to_string));
    let base = if transform {
        Value::Transform(text.unwrap_or_default().trim().to_string())
    } else {
        text.map_or_else(|| Value::Text(String::new()), |t| values::parse(values::kind_of(attr), &t))
    };
    Slot {
        target,
        attr: attr.to_string(),
        css,
        existing: if css { None } else { existing },
        base,
        anims: Vec::new(),
    }
}

/// The last `prop: value` of a `style` attribute.
fn style_declaration(style: &str, prop: &str) -> Option<String> {
    style
        .split(';')
        .filter_map(|d| d.split_once(':'))
        .filter(|(p, _)| p.trim().eq_ignore_ascii_case(prop))
        .map(|(_, v)| v.trim().to_string())
        .next_back()
}

// ---- reading one animation element ------------------------------------------------

struct Reader<'a> {
    warnings: &'a mut Vec<String>,
    by_id: HashMap<String, NodeId>,
    parsed: Vec<Parsed>,
    cuts: Vec<Range<usize>>,
}

impl Reader<'_> {
    fn warn(&mut self, msg: String) {
        self.warnings.push(msg);
    }

    fn read(&mut self, node: Node) -> Result<()> {
        let name = node.tag_name().name();
        if name == "animateMotion" {
            self.warn("animateMotion is not supported yet and was dropped".to_string());
            return Ok(());
        }
        let Some(target) = self.target(&node) else {
            self.warn(format!("<{name}> has no element to animate and was dropped"));
            return Ok(());
        };
        let Some(raw) = node.attribute("attributeName") else {
            self.warn(format!("<{name}> without attributeName was dropped"));
            return Ok(());
        };
        // The name is written into the document as markup when a frame is made, so it must be a
        // name and nothing else (the sanitiser drops these too; this is for a document read as is).
        if !is_attribute_name(raw.trim()) {
            self.warn(format!(
                "<{name}> animates \"{}\", which is not an attribute name, and was dropped",
                raw.chars().take(40).collect::<String>()
            ));
            return Ok(());
        }
        let attr = raw.rsplit(':').next().unwrap_or(raw).trim();
        if attr.is_empty() {
            self.warn(format!("<{name}> without attributeName was dropped"));
            return Ok(());
        }
        let is_transform = name == "animateTransform";
        if is_transform != matches!(attr, "transform" | "gradientTransform" | "patternTransform") {
            self.warn(if is_transform {
                format!("animateTransform on \"{attr}\" was dropped (it animates transform lists)")
            } else if attr == "transform" {
                "animate on \"transform\" is not allowed in SVG; use animateTransform (dropped)".to_string()
            } else {
                format!("<{name}> on \"{attr}\" was dropped")
            });
            return Ok(());
        }
        let spec = self.spec(&node);
        let attr = attr.to_string();
        let Some(anim) = self.anim(&node, name, &attr)? else {
            return Ok(());
        };
        self.parsed.push(Parsed {
            spec,
            target,
            attr,
            anim,
        });
        Ok(())
    }

    fn target(&self, node: &Node) -> Option<NodeId> {
        let target = match href_of(node) {
            Some(h) => {
                let id = h.trim().strip_prefix('#')?;
                *self.by_id.get(id)?
            }
            None => node.parent_element()?.id(),
        };
        let found = node.document().get_node(target)?;
        (found.tag_name().namespace() == Some(SVG_NS) && !is_animation(&found)).then_some(target)
    }

    fn spec(&mut self, node: &Node) -> Spec {
        let name = node.tag_name().name();
        let mut spec = Spec {
            id: node.attribute("id").map(str::to_string),
            begin: vec![TimeItem::Offset(0.0)],
            end: Vec::new(),
            has_end: false,
            dur: None,
            repeat_count: None,
            repeat_dur: None,
            min: 0.0,
            max: INF,
            restart: Restart::Always,
            freeze: node.attribute("fill") == Some("freeze"),
        };
        if let Some(b) = node.attribute("begin") {
            let items = anim::parse_time_list(b);
            if !items.is_empty() {
                if items.iter().all(|i| matches!(i, TimeItem::Indefinite)) {
                    self.warn(format!("<{name}> begin=\"indefinite\" never starts"));
                }
                spec.begin = items;
            }
        }
        if let Some(e) = node.attribute("end") {
            spec.end = anim::parse_time_list(e);
            spec.has_end = !spec.end.is_empty();
        }
        if name != "set" && node.attribute("dur").is_none() && node.attribute("repeatDur").is_none() {
            self.warn(format!("<{name}> has no dur, so it never moves off its first value"));
        }
        if let Some(d) = node.attribute("dur").map(str::trim) {
            match d {
                "indefinite" | "media" => {}
                _ => match anim::parse_clock(d).filter(|v| *v > 0.0) {
                    Some(v) => spec.dur = Some(v),
                    None => self.warn(format!("<{name}> dur=\"{d}\" is not a positive time; treated as indefinite")),
                },
            }
        }
        if let Some(c) = node.attribute("repeatCount").map(str::trim) {
            match c {
                "indefinite" => spec.repeat_count = Some(INF),
                _ => match values::parse_number(c).filter(|v| *v > 0.0) {
                    Some(v) => spec.repeat_count = Some(v),
                    None => self.warn(format!("repeatCount=\"{c}\" was ignored")),
                },
            }
        }
        if let Some(d) = node.attribute("repeatDur").map(str::trim) {
            match d {
                "indefinite" => spec.repeat_dur = Some(INF),
                _ => match anim::parse_clock(d).filter(|v| *v > 0.0) {
                    Some(v) => spec.repeat_dur = Some(v),
                    None => self.warn(format!("repeatDur=\"{d}\" was ignored")),
                },
            }
        }
        if let Some(m) = node.attribute("min").and_then(anim::parse_clock).filter(|v| *v >= 0.0) {
            spec.min = m;
        }
        if let Some(m) = node.attribute("max").and_then(anim::parse_clock).filter(|v| *v > 0.0) {
            spec.max = m;
        }
        spec.restart = match node.attribute("restart").map(str::trim) {
            Some("whenNotActive") => Restart::WhenNotActive,
            Some("never") => Restart::Never,
            _ => Restart::Always,
        };
        spec
    }

    /// The value part of an animation; `Ok(None)` (with a warning) when it cannot be honoured.
    fn anim(&mut self, node: &Node, name: &str, attr: &str) -> Result<Option<Anim>> {
        let transform = if name == "animateTransform" {
            let ty = node.attribute("type").unwrap_or("translate");
            match TransformType::parse(ty) {
                Some(t) => Some(t),
                None => {
                    self.warn(format!("animateTransform type=\"{ty}\" was dropped"));
                    return Ok(None);
                }
            }
        } else {
            None
        };
        let kind = values::kind_of(attr);
        let parse_one = |s: &str| -> Option<Value> {
            match transform {
                Some(ty) => ty.numbers(s).map(Value::List),
                None => Some(values::parse(kind, s)),
            }
        };
        let mut anim = Anim {
            set: name == "set",
            transform,
            values: Vec::new(),
            to_only: false,
            key_times: None,
            key_splines: None,
            calc: Calc::Linear,
            additive: node.attribute("additive") == Some("sum"),
            accumulate: node.attribute("accumulate") == Some("sum"),
            timing: Timing::none(),
        };
        if anim.set {
            let Some(to) = node.attribute("to").and_then(parse_one) else {
                self.warn(format!("<set> on \"{attr}\" without a usable \"to\" was dropped"));
                return Ok(None);
            };
            anim.values = vec![to];
            return Ok(Some(anim));
        }
        let (from, to, by) = (node.attribute("from"), node.attribute("to"), node.attribute("by"));
        if let Some(list) = node.attribute("values") {
            let mut parts: Vec<&str> = list.split(';').map(str::trim).collect();
            if parts.last() == Some(&"") {
                parts.pop();
            }
            if parts.len() > MAX_VALUES {
                return Err(invalid(format!("an animation has more than {MAX_VALUES} values")));
            }
            let vals: Option<Vec<Value>> = parts.iter().map(|p| parse_one(p)).collect();
            match vals.filter(|v| !v.is_empty()) {
                Some(v) => anim.values = v,
                None => {
                    self.warn(format!(
                        "values=\"{list}\" on \"{attr}\" could not be read; the animation was dropped"
                    ));
                    return Ok(None);
                }
            }
        } else {
            match (from.and_then(parse_one), to.and_then(parse_one), by.and_then(parse_one)) {
                (Some(f), Some(t), _) => anim.values = vec![f, t],
                (Some(f), None, Some(b)) => match values::add(&f, &b) {
                    Some(t) => anim.values = vec![f, t],
                    None => {
                        self.warn(format!("from/by on \"{attr}\" cannot be added; the animation was dropped"));
                        return Ok(None);
                    }
                },
                (None, Some(t), _) if from.is_none() => {
                    anim.values = vec![t];
                    anim.to_only = true;
                }
                (None, None, Some(b)) if from.is_none() && to.is_none() => match values::zero_like(&b) {
                    Some(z) => {
                        anim.values = vec![z, b];
                        anim.additive = true;
                    }
                    None => {
                        self.warn(format!("by on \"{attr}\" cannot be added; the animation was dropped"));
                        return Ok(None);
                    }
                },
                _ => {
                    self.warn(format!(
                        "<{name}> on \"{attr}\" needs values, from/to or to/by; the animation was dropped"
                    ));
                    return Ok(None);
                }
            }
        }
        let n = if anim.to_only { 2 } else { anim.values.len() };
        anim.calc = match node.attribute("calcMode").map(str::trim) {
            Some("discrete") => Calc::Discrete,
            Some("paced") => Calc::Paced,
            Some("spline") => Calc::Spline,
            _ => Calc::Linear,
        };
        if anim.calc != Calc::Paced {
            if let Some(kt) = node.attribute("keyTimes") {
                match key_times(kt, n, anim.calc == Calc::Discrete) {
                    Some(k) => anim.key_times = Some(k),
                    None => self.warn(format!("keyTimes=\"{kt}\" is not valid for {n} values and was ignored")),
                }
            }
        }
        if anim.calc == Calc::Spline {
            let valid = node.attribute("keySplines").and_then(|s| key_splines(s, n));
            match valid {
                Some(s) => anim.key_splines = Some(s),
                None => {
                    self.warn("calcMode=\"spline\" needs one valid keySplines per interval; using linear".to_string());
                    anim.calc = Calc::Linear;
                }
            }
        }
        Ok(Some(anim))
    }
}

fn key_times(s: &str, n: usize, discrete: bool) -> Option<Vec<f64>> {
    let mut parts: Vec<&str> = s.split(';').map(str::trim).collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    let k: Vec<f64> = parts.iter().map(|p| values::parse_number(p)).collect::<Option<_>>()?;
    let ok = k.len() == n
        && k.first() == Some(&0.0)
        && (discrete || n < 2 || k.last() == Some(&1.0))
        && k.iter().all(|t| (0.0..=1.0).contains(t))
        && k.windows(2).all(|w| w[0] <= w[1]);
    ok.then_some(k)
}

fn key_splines(s: &str, n: usize) -> Option<Vec<[f64; 4]>> {
    let mut parts: Vec<&str> = s.split(';').map(str::trim).collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    if parts.len() + 1 != n {
        return None;
    }
    parts
        .iter()
        .map(|p| {
            let n = values::parse_list(p)?;
            let [x1, y1, x2, y2] = <[f64; 4]>::try_from(n).ok()?;
            ((0.0..=1.0).contains(&x1) && (0.0..=1.0).contains(&x2)).then_some([x1, y1, x2, y2])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPEN: &str =
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 100 100\">";

    fn svg(inner: &str) -> String {
        format!("{OPEN}{inner}</svg>")
    }

    fn doc(inner: &str) -> SvgDoc {
        SvgDoc::parse(&svg(inner)).unwrap_or_else(|e| panic!("{e}: {inner}"))
    }

    /// The document at `t` without the wrapper, for compact expectations.
    fn at(inner: &str, t: f64) -> String {
        let out = doc(inner).at(t);
        out.strip_prefix(OPEN)
            .and_then(|s| s.strip_suffix("</svg>"))
            .expect("wrapper kept")
            .to_string()
    }

    #[test]
    fn numbers_move_between_from_and_to() {
        let rect = |anim: &str| format!("<rect x=\"0\" width=\"5\">{anim}</rect>");
        let lin = rect("<animate attributeName=\"x\" from=\"0\" to=\"100\" dur=\"10s\"/>");
        for (t, want) in [
            (0.0, "<rect x=\"0\" width=\"5\"></rect>"),
            (2.5, "<rect x=\"25\" width=\"5\"></rect>"),
            (5.0, "<rect x=\"50\" width=\"5\"></rect>"),
            (9.99, "<rect x=\"99.9\" width=\"5\"></rect>"),
            (10.0, "<rect x=\"0\" width=\"5\"></rect>"),
            (50.0, "<rect x=\"0\" width=\"5\"></rect>"),
        ] {
            assert_eq!(at(&lin, t), want, "t = {t}");
        }
        let frozen = rect("<animate attributeName=\"x\" from=\"0\" to=\"100\" dur=\"10s\" fill=\"freeze\"/>");
        assert_eq!(at(&frozen, 12.0), "<rect x=\"100\" width=\"5\"></rect>");
        let late = rect("<animate attributeName=\"x\" from=\"0\" to=\"100\" begin=\"5s\" dur=\"10s\"/>");
        assert_eq!(
            at(&late, 4.0),
            "<rect x=\"0\" width=\"5\"></rect>",
            "nothing before the begin"
        );
        assert_eq!(at(&late, 10.0), "<rect x=\"50\" width=\"5\"></rect>");
    }

    #[test]
    fn a_missing_attribute_is_inserted_after_the_tag_name() {
        let inner = "<rect width=\"5\"><animate attributeName=\"x\" from=\"0\" to=\"100\" dur=\"10s\"/></rect>";
        assert_eq!(at(inner, 5.0), "<rect x=\"50\" width=\"5\"></rect>");
        let empty = "<circle><animate attributeName=\"cx\" from=\"0\" to=\"10\" dur=\"2s\"/></circle>";
        assert_eq!(at(empty, 1.0), "<circle cx=\"5\"></circle>");
        let lone = "<g/><rect width=\"1\"/><animate href=\"#nope\" attributeName=\"x\" to=\"1\" dur=\"1s\"/>";
        let d = doc(lone);
        assert!(
            d.warnings().iter().any(|w| w.contains("no element to animate")),
            "{:?}",
            d.warnings()
        );
    }

    #[test]
    fn to_and_by_animations_start_from_what_is_there() {
        let to = "<rect x=\"20\"><animate attributeName=\"x\" to=\"100\" dur=\"10s\"/></rect>";
        assert_eq!(at(to, 5.0), "<rect x=\"60\"></rect>");
        let by = "<rect x=\"20\"><animate attributeName=\"x\" by=\"10\" dur=\"10s\" fill=\"freeze\"/></rect>";
        assert_eq!(at(by, 5.0), "<rect x=\"25\"></rect>");
        assert_eq!(at(by, 20.0), "<rect x=\"30\"></rect>");
        let from_by = "<rect x=\"0\"><animate attributeName=\"x\" from=\"10\" by=\"30\" dur=\"2s\"/></rect>";
        assert_eq!(at(from_by, 1.0), "<rect x=\"25\"></rect>");
        let absent = "<rect><animate attributeName=\"opacity\" to=\"0\" dur=\"2s\"/></rect>";
        assert_eq!(
            at(absent, 1.0),
            "<rect style=\"opacity:0.5\"></rect>",
            "the default opacity is 1"
        );
    }

    #[test]
    fn values_and_key_times_shape_the_curve() {
        let x =
            |extra: &str| format!("<rect x=\"0\"><animate attributeName=\"x\" values=\"0;10;100\" dur=\"10s\"{extra}/></rect>");
        for (extra, t, want) in [
            ("", 1.25, "2.5"),
            ("", 5.0, "10"),
            ("", 7.5, "55"),
            (" keyTimes=\"0;0.2;1\"", 1.0, "5"),
            (" keyTimes=\"0;0.2;1\"", 2.0, "10"),
            (" keyTimes=\"0;0.2;1\"", 6.0, "55"),
            (" calcMode=\"discrete\"", 1.0, "0"),
            (" calcMode=\"discrete\"", 4.0, "10"),
            (" calcMode=\"discrete\"", 9.0, "100"),
            (" calcMode=\"paced\"", 1.0, "10"),
            (" calcMode=\"paced\"", 5.5, "55"),
        ] {
            assert_eq!(at(&x(extra), t), format!("<rect x=\"{want}\"></rect>"), "{extra} @ {t}");
        }
        let bad = doc(&x(" keyTimes=\"0;0.5\""));
        assert!(bad.warnings().iter().any(|w| w.contains("keyTimes")), "{:?}", bad.warnings());
        assert_eq!(
            bad.at(5.0).matches("x=\"10\"").count(),
            1,
            "a bad keyTimes is ignored, the curve stays"
        );
    }

    #[test]
    fn splines_ease_and_a_linear_spline_is_a_line() {
        let spline = |s: &str| {
            format!(
                "<rect x=\"0\"><animate attributeName=\"x\" from=\"0\" to=\"100\" dur=\"10s\" calcMode=\"spline\" keyTimes=\"0;1\" keySplines=\"{s}\"/></rect>"
            )
        };
        assert_eq!(at(&spline("0 0 1 1"), 5.0), "<rect x=\"50\"></rect>");
        let out = at(&spline("0.42 0 1 1"), 5.0);
        let x: f64 = out.split('"').nth(1).unwrap().parse().unwrap();
        assert!(x < 50.0 && x > 10.0, "ease-in is behind the line: {x}");
        let missing =
            doc("<rect x=\"0\"><animate attributeName=\"x\" from=\"0\" to=\"1\" dur=\"1s\" calcMode=\"spline\"/></rect>");
        assert!(missing.warnings().iter().any(|w| w.contains("keySplines")));
        let wild = doc("<rect x=\"0\"><animate attributeName=\"x\" from=\"0\" to=\"1\" dur=\"1s\" calcMode=\"spline\" keySplines=\"2 0 0 1\"/></rect>");
        assert!(wild.warnings().iter().any(|w| w.contains("keySplines")));
    }

    #[test]
    fn css_properties_are_written_through_style() {
        let fade = |attrs: &str| {
            format!("<rect{attrs}><animate attributeName=\"opacity\" from=\"0\" to=\"1\" dur=\"2s\" fill=\"freeze\"/></rect>")
        };
        assert_eq!(at(&fade(""), 1.0), "<rect style=\"opacity:0.5\"></rect>");
        assert_eq!(
            at(&fade(" style=\"fill:red\""), 1.0),
            "<rect style=\"fill:red;opacity:0.5\"></rect>"
        );
        assert_eq!(
            at(&fade(" opacity=\"0.2\""), 1.0),
            "<rect style=\"opacity:0.5\" opacity=\"0.2\"></rect>",
            "the presentation attribute stays; the inline style beats it and any rule"
        );
        assert_eq!(
            at(&fade(" style='font-family:\"A\"'"), 2.0),
            "<rect style='font-family:\"A\";opacity:1'></rect>"
        );
        let two = "<rect><animate attributeName=\"opacity\" to=\"0\" dur=\"2s\" fill=\"freeze\"/><animate attributeName=\"fill\" to=\"red\" dur=\"2s\" fill=\"freeze\"/></rect>";
        assert_eq!(at(two, 3.0), "<rect style=\"opacity:0;fill:#ff0000\"></rect>");
    }

    #[test]
    fn colours_blend_per_channel() {
        let c = |from: &str, to: &str| {
            format!("<rect><animate attributeName=\"fill\" from=\"{from}\" to=\"{to}\" dur=\"2s\"/></rect>")
        };
        assert_eq!(at(&c("#000", "#f00"), 1.0), "<rect style=\"fill:#800000\"></rect>");
        assert_eq!(at(&c("black", "rgb(0,0,255)"), 0.5), "<rect style=\"fill:#000040\"></rect>");
        assert_eq!(
            at(&c("white", "#00000000"), 1.0),
            "<rect style=\"fill:rgba(128,128,128,0.5)\"></rect>"
        );
        let step = at(&c("red", "none"), 0.5);
        assert_eq!(step, "<rect style=\"fill:#ff0000\"></rect>", "a colour and a keyword step");
        assert_eq!(at(&c("red", "none"), 1.5), "<rect style=\"fill:none\"></rect>");
        let cc = "<rect><animateColor attributeName=\"fill\" from=\"#000\" to=\"#fff\" dur=\"2s\"/></rect>";
        assert_eq!(at(cc, 1.0), "<rect style=\"fill:#808080\"></rect>");
    }

    #[test]
    fn a_fill_steps_between_gradients_and_colours_and_a_pattern_is_not_among_them() {
        let defs = "<linearGradient id=\"g\"/><radialGradient id=\"h\"/><pattern id=\"p\" width=\"1\" height=\"1\"/>";
        let c = |from: &str, to: &str| {
            format!("{defs}<rect><animate attributeName=\"fill\" from=\"{from}\" to=\"{to}\" dur=\"2s\"/></rect>")
        };
        let rect = |style: &str| format!("{defs}<rect style=\"{style}\"></rect>");
        assert_eq!(at(&c("url(#g)", "url(#h)"), 0.5), rect("fill:url(#g)"));
        assert_eq!(at(&c("url(#g)", "url(#h)"), 1.5), rect("fill:url(#h)"));
        assert_eq!(at(&c("red", "url(#h)"), 1.5), rect("fill:url(#h)"));
        assert_eq!(at(&c("url(#g)", "red"), 1.5), rect("fill:#ff0000"));
        // A pattern is another matter: the animation is dropped, the rect keeps what it had.
        let d = doc(&c("red", "url(#p)"));
        assert!(
            !d.is_animated() && d.warnings().iter().any(|w| w.contains("url()")),
            "{:?}",
            d.warnings()
        );
    }

    #[test]
    fn set_switches_a_value_for_a_while() {
        let show =
            "<rect style=\"visibility:hidden\"><set attributeName=\"visibility\" to=\"visible\" begin=\"1s\" dur=\"2s\"/></rect>";
        assert_eq!(at(show, 0.5), "<rect style=\"visibility:hidden\"></rect>");
        assert_eq!(at(show, 1.0), "<rect style=\"visibility:hidden;visibility:visible\"></rect>");
        assert_eq!(at(show, 3.0), "<rect style=\"visibility:hidden\"></rect>");
        let forever = "<rect><set attributeName=\"display\" to=\"none\" begin=\"1s\"/></rect>";
        assert_eq!(at(forever, 1000.0), "<rect style=\"display:none\"></rect>");
        assert_eq!(
            at("<rect><set attributeName=\"x\" to=\"3\"/></rect>", 0.0),
            "<rect x=\"3\"></rect>"
        );
        let missing = doc("<rect><set attributeName=\"x\"/></rect>");
        assert!(
            missing.warnings().iter().any(|w| w.contains("\"to\"")),
            "{:?}",
            missing.warnings()
        );
    }

    #[test]
    fn repeats_accumulate_and_freeze() {
        let rep =
            |extra: &str| format!("<rect x=\"0\"><animate attributeName=\"x\" from=\"0\" to=\"10\" dur=\"1s\"{extra}/></rect>");
        assert_eq!(at(&rep(" repeatCount=\"3\""), 2.5), "<rect x=\"5\"></rect>");
        assert_eq!(at(&rep(" repeatCount=\"3\""), 3.5), "<rect x=\"0\"></rect>");
        assert_eq!(at(&rep(" repeatCount=\"2.5\" fill=\"freeze\""), 9.0), "<rect x=\"5\"></rect>");
        assert_eq!(at(&rep(" repeatCount=\"2\" fill=\"freeze\""), 9.0), "<rect x=\"10\"></rect>");
        assert_eq!(at(&rep(" repeatDur=\"1.5s\" fill=\"freeze\""), 9.0), "<rect x=\"5\"></rect>");
        assert_eq!(at(&rep(" repeatCount=\"indefinite\""), 1000.25), "<rect x=\"2.5\"></rect>");
        let acc = " repeatCount=\"3\" accumulate=\"sum\" fill=\"freeze\"";
        assert_eq!(at(&rep(acc), 2.5), "<rect x=\"25\"></rect>");
        assert_eq!(
            at(&rep(acc), 9.0),
            "<rect x=\"30\"></rect>",
            "frozen on the end of the last repeat"
        );
        assert_eq!(
            at(&rep(" begin=\"0;5\" fill=\"freeze\""), 5.25),
            "<rect x=\"2.5\"></rect>",
            "a second begin restarts it"
        );
        assert_eq!(
            at(&rep(" begin=\"-0.5s\""), 0.0),
            "<rect x=\"5\"></rect>",
            "a negative begin is already under way"
        );
        assert_eq!(at(&rep(" end=\"0.5s\""), 0.25), "<rect x=\"2.5\"></rect>");
        assert_eq!(at(&rep(" end=\"0.5s\""), 0.75), "<rect x=\"0\"></rect>");
        assert_eq!(at(&rep(" max=\"0.5s\""), 0.75), "<rect x=\"0\"></rect>");
    }

    #[test]
    fn transforms_replace_or_append_to_the_base() {
        let t = |ty: &str, from: &str, to: &str, extra: &str| {
            format!(
                "<g transform=\"scale(2)\"><animateTransform attributeName=\"transform\" type=\"{ty}\" from=\"{from}\" to=\"{to}\" dur=\"2s\"{extra}/></g>"
            )
        };
        assert_eq!(
            at(&t("translate", "0 0", "10 20", ""), 1.0),
            "<g transform=\"translate(5 10)\"></g>"
        );
        assert_eq!(
            at(&t("translate", "0 0", "10 20", " additive=\"sum\""), 1.0),
            "<g transform=\"scale(2) translate(5 10)\"></g>"
        );
        assert_eq!(at(&t("scale", "1", "3", ""), 1.0), "<g transform=\"scale(2 2)\"></g>");
        assert_eq!(
            at(&t("rotate", "0 50 50", "360 50 50", ""), 0.5),
            "<g transform=\"rotate(90 50 50)\"></g>"
        );
        assert_eq!(at(&t("rotate", "0", "180", ""), 1.0), "<g transform=\"rotate(90)\"></g>");
        assert_eq!(at(&t("skewX", "0", "30", ""), 1.0), "<g transform=\"skewX(15)\"></g>");
        let bare = "<g><animateTransform attributeName=\"transform\" type=\"rotate\" from=\"0\" to=\"360\" dur=\"4s\"/></g>";
        assert_eq!(at(bare, 1.0), "<g transform=\"rotate(90)\"></g>");
        let pop = "<g transform=\"translate(50 50)\"><animateTransform attributeName=\"transform\" type=\"scale\" values=\"0.6;1.2;1\" keyTimes=\"0;0.5;1\" dur=\"2s\" additive=\"sum\"/></g>";
        assert_eq!(at(pop, 1.0), "<g transform=\"translate(50 50) scale(1.2 1.2)\"></g>");
        let to_only = "<g><animateTransform attributeName=\"transform\" type=\"translate\" to=\"10 0\" dur=\"2s\"/></g>";
        assert_eq!(at(to_only, 1.0), "<g transform=\"translate(5 0)\"></g>");
        let bad = doc("<g><animateTransform attributeName=\"transform\" type=\"spin\" to=\"1\" dur=\"1s\"/></g>");
        assert!(bad.warnings().iter().any(|w| w.contains("spin")));
        let wrong = doc("<g><animateTransform attributeName=\"x\" type=\"scale\" to=\"2\" dur=\"1s\"/></g>");
        assert!(wrong.warnings().iter().any(|w| w.contains("animateTransform")));
        let plain = doc("<g><animate attributeName=\"transform\" to=\"scale(2)\" dur=\"1s\"/></g>");
        assert!(plain.warnings().iter().any(|w| w.contains("animateTransform")));
        assert_eq!(plain.at(0.5), svg("<g></g>"));
    }

    #[test]
    fn the_sandwich_orders_by_begin_then_document() {
        let two = |a: &str, b: &str| format!("<rect x=\"0\">{a}{b}</rect>");
        let ramp = "<animate attributeName=\"x\" from=\"0\" to=\"100\" dur=\"10s\" fill=\"freeze\"/>";
        let over = "<set attributeName=\"x\" to=\"7\" begin=\"5s\"/>";
        assert_eq!(at(&two(ramp, over), 3.0), "<rect x=\"30\"></rect>");
        assert_eq!(at(&two(ramp, over), 6.0), "<rect x=\"7\"></rect>", "the later begin replaces");
        assert_eq!(
            at(&two(over, ramp), 6.0),
            "<rect x=\"7\"></rect>",
            "document order does not beat begin time"
        );
        let add = "<animate attributeName=\"x\" from=\"0\" to=\"10\" dur=\"10s\" additive=\"sum\"/>";
        assert_eq!(at(&two(ramp, add), 5.0), "<rect x=\"55\"></rect>");
        assert_eq!(
            at(&two(add, ramp), 5.0),
            "<rect x=\"50\"></rect>",
            "same begin: the later in the document wins"
        );
        let late_add = "<animate attributeName=\"x\" from=\"0\" to=\"10\" begin=\"2s\" dur=\"10s\" additive=\"sum\"/>";
        assert_eq!(at(&two(ramp, late_add), 7.0), "<rect x=\"75\"></rect>");
    }

    #[test]
    fn syncbases_chain_one_animation_after_another() {
        let inner = "<rect x=\"0\" y=\"0\">\
            <animate id=\"a\" attributeName=\"x\" from=\"0\" to=\"10\" begin=\"1s\" dur=\"2s\"/>\
            <animate attributeName=\"y\" from=\"0\" to=\"8\" begin=\"a.end+0.5s\" dur=\"4s\"/>\
            <animate attributeName=\"width\" from=\"1\" to=\"2\" begin=\"a.begin\" dur=\"2s\"/></rect>";
        assert_eq!(at(inner, 0.5), "<rect x=\"0\" y=\"0\"></rect>");
        assert_eq!(at(inner, 2.0), "<rect width=\"1.5\" x=\"5\" y=\"0\"></rect>");
        assert_eq!(at(inner, 4.5), "<rect x=\"0\" y=\"2\"></rect>");
        let cyc = doc("<rect><animate id=\"p\" attributeName=\"x\" to=\"1\" begin=\"q.end\" dur=\"1s\"/><animate id=\"q\" attributeName=\"y\" to=\"1\" begin=\"p.end\" dur=\"1s\"/></rect>");
        assert!(
            cyc.warnings().iter().any(|w| w.contains("refers back")),
            "{:?}",
            cyc.warnings()
        );
        assert_eq!(cyc.natural_duration(), None);
    }

    #[test]
    fn paths_and_lists_morph() {
        let path = "<path d=\"M0 0L10 0\"><animate attributeName=\"d\" from=\"M0 0L10 0\" to=\"M0 0L30 0\" dur=\"2s\"/></path>";
        assert_eq!(at(path, 1.0), "<path d=\"M0 0L20 0\"></path>");
        let step = "<path d=\"M0 0L10 0\"><animate attributeName=\"d\" from=\"M0 0L10 0\" to=\"M0 0L30 0Z\" dur=\"2s\"/></path>";
        assert_eq!(
            at(step, 0.5),
            "<path d=\"M0 0L10 0\"></path>",
            "different command lists step instead of blending"
        );
        assert_eq!(at(step, 1.5), "<path d=\"M0 0L30 0Z\"></path>");
        let dash = "<rect><animate attributeName=\"stroke-dasharray\" from=\"0 10\" to=\"10 0\" dur=\"2s\"/></rect>";
        assert_eq!(at(dash, 1.0), "<rect style=\"stroke-dasharray:5 5\"></rect>");
        let root = format!("{OPEN}<animate attributeName=\"viewBox\" from=\"0 0 100 100\" to=\"0 0 50 50\" dur=\"2s\"/></svg>");
        assert!(SvgDoc::parse(&root).unwrap().at(1.0).contains("viewBox=\"0 0 75 75\""));
    }

    #[test]
    fn a_target_can_be_named_by_href() {
        let inner = "<rect id=\"r\" x=\"0\"/><animate href=\"#r\" attributeName=\"x\" to=\"10\" dur=\"1s\" fill=\"freeze\"/>\
            <circle id=\"c\"/><animate xlink:href=\"#c\" attributeName=\"r\" to=\"4\" dur=\"2s\"/>";
        assert_eq!(at(inner, 1.0), "<rect id=\"r\" x=\"10\"/><circle r=\"2\" id=\"c\"/>");
        assert_eq!(at(inner, 5.0), "<rect id=\"r\" x=\"10\"/><circle id=\"c\"/>");
    }

    #[test]
    fn values_are_escaped_for_the_attribute_they_land_in() {
        let inner = "<text font-family=\"X\"><set attributeName=\"font-family\" to=\"A &amp; &quot;B&quot;\"/>t</text>";
        let out = at(inner, 0.0);
        assert_eq!(
            out,
            "<text style=\"font-family:A &amp; &quot;B&quot;\" font-family=\"X\">t</text>"
        );
        let whole = svg(&out);
        let reparsed = Document::parse(&whole).unwrap();
        let t = reparsed.descendants().find(|n| n.has_tag_name("text")).unwrap();
        assert_eq!(t.attribute("style"), Some("font-family:A & \"B\""));
    }

    #[test]
    fn what_cannot_be_honoured_is_a_warning() {
        let cases: [(&str, &str); 9] = [
            ("<rect><animateMotion path=\"M0 0L9 9\" dur=\"1s\"/></rect>", "animateMotion"),
            (
                "<rect><animate attributeName=\"x\" to=\"1\" begin=\"click\" dur=\"1s\"/></rect>",
                "not evaluated",
            ),
            ("<rect><animate to=\"1\" dur=\"1s\"/></rect>", "without attributeName"),
            ("<rect><animate attributeName=\"x\" dur=\"1s\"/></rect>", "needs values"),
            ("<rect><animate attributeName=\"x\" to=\"1\" dur=\"0\"/></rect>", "dur=\"0\""),
            (
                "<rect><animate attributeName=\"x\" to=\"1\" begin=\"indefinite\" dur=\"1s\"/></rect>",
                "never starts",
            ),
            (
                "<style>@keyframes k{from{x:y}} .a{animation:k 1s}</style><rect class=\"a\"/>",
                "CSS animations",
            ),
            (
                "<rect><animate attributeName=\"x\" to=\"1\" repeatCount=\"-2\" dur=\"1s\"/></rect>",
                "repeatCount",
            ),
            ("<rect><animate attributeName=\"x\" to=\"1\"/></rect>", "has no dur"),
        ];
        for (inner, needle) in cases {
            let d = doc(inner);
            assert!(
                d.warnings().iter().any(|w| w.contains(needle)),
                "{needle}: {:?}",
                d.warnings()
            );
            assert!(
                !d.at(0.5).contains("<animate"),
                "{needle}: animation elements never reach the renderer"
            );
        }
        assert!(doc("<rect/>").warnings().is_empty());
    }

    #[test]
    fn a_title_runs_for_its_natural_duration_held_to_two_to_thirty_seconds() {
        let span = |dur: &str| doc(&format!("<rect><animate attributeName=\"x\" to=\"1\" dur=\"{dur}\"/></rect>")).default_span();
        assert_eq!((span("0.5s"), span("3.5s"), span("100s")), (2.0, 3.5, 30.0));
        assert_eq!(doc("<rect/>").default_span(), 4.0, "a still");
        assert_eq!(
            doc("<rect><animate attributeName=\"x\" to=\"1\" dur=\"1s\" repeatCount=\"indefinite\"/></rect>").default_span(),
            4.0,
            "a loop"
        );
    }

    #[test]
    fn natural_duration_is_when_everything_has_settled() {
        let nat = |inner: &str| doc(inner).natural_duration();
        assert_eq!(nat("<rect/>"), None);
        assert_eq!(
            nat("<rect><animate attributeName=\"x\" to=\"1\" begin=\"1s\" dur=\"2s\"/></rect>"),
            Some(3.0)
        );
        assert_eq!(
            nat("<rect><animate attributeName=\"x\" to=\"1\" dur=\"2s\"/><animate attributeName=\"y\" to=\"1\" begin=\"2s\" dur=\"4s\" repeatCount=\"2\"/></rect>"),
            Some(10.0)
        );
        assert_eq!(
            nat("<rect><animate attributeName=\"x\" to=\"1\" dur=\"2s\" repeatCount=\"indefinite\"/></rect>"),
            None
        );
        assert_eq!(
            nat("<rect><animate attributeName=\"x\" to=\"1\" dur=\"2s\" repeatDur=\"indefinite\"/></rect>"),
            None
        );
        assert_eq!(
            nat("<rect><set attributeName=\"display\" to=\"none\" begin=\"2s\"/></rect>"),
            Some(2.0)
        );
        assert_eq!(
            nat("<rect><animate attributeName=\"x\" to=\"1\" dur=\"4s\" end=\"1.5s\"/></rect>"),
            Some(1.5)
        );
        assert_eq!(
            nat("<rect><animate attributeName=\"x\" to=\"1\" begin=\"click\" dur=\"4s\"/></rect>"),
            None
        );
        assert_eq!(
            nat("<rect><animate id=\"a\" attributeName=\"x\" to=\"1\" dur=\"2s\"/><animate attributeName=\"y\" to=\"1\" begin=\"a.end\" dur=\"3s\"/></rect>"),
            Some(5.0)
        );
        assert!(!doc("<rect/>").is_animated());
        assert!(doc("<rect><set attributeName=\"x\" to=\"1\"/></rect>").is_animated());
    }

    #[test]
    fn check_reports_the_stored_text_and_what_was_cut() {
        let src = svg("<script>alert(1)</script><rect onclick=\"x()\"><animate attributeName=\"x\" to=\"1\" dur=\"3s\"/></rect>");
        let check = check_svg(&src).unwrap();
        assert_eq!(
            check.svg,
            svg("<rect><animate attributeName=\"x\" to=\"1\" dur=\"3s\"/></rect>"),
            "animation stays in the stored text"
        );
        assert_eq!(check.natural_duration, Some(3.0));
        assert_eq!(check.warnings.len(), 2, "{:?}", check.warnings);
        let again = check_svg(&check.svg).unwrap();
        assert_eq!(again.svg, check.svg, "the stored text is its own sanitised form");
        assert!(again.warnings.is_empty());
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json["natural_duration"], 3.0);
    }

    #[test]
    fn what_resvg_cannot_draw_is_refused_up_front() {
        assert!(
            SvgDoc::parse("<svg xmlns=\"http://www.w3.org/2000/svg\"/>").is_err(),
            "no size at all"
        );
        assert!(SvgDoc::parse("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" height=\"50%\"/>").is_err());
        assert!(SvgDoc::parse("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 0 10\"/>").is_err());
        assert!(SvgDoc::parse("nope").is_err());
        assert!(SvgDoc::parse("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>").is_ok());
        let d = doc("<image href=\"http://x/y.png\" width=\"10\" height=\"10\"/>");
        assert!(!d.sanitized().contains("http://x"));
    }

    #[test]
    fn the_caps_hold() {
        let one = "<rect><animate attributeName=\"x\" to=\"1\" dur=\"1s\"/></rect>";
        let ok = svg(&one.repeat(100));
        assert!(SvgDoc::parse(&ok).is_ok());
        let packed = svg(&"<g><set attributeName=\"x\" to=\"1\"/></g>".repeat(MAX_ANIMATIONS + 1));
        assert!(packed.len() <= MAX_BYTES, "{}", packed.len());
        assert!(SvgDoc::parse(&packed).unwrap_err().to_string().contains("animations"));
        let vals = vec!["1"; MAX_VALUES + 1].join(";");
        let src = svg(&format!(
            "<rect><animate attributeName=\"x\" values=\"{vals}\" dur=\"1s\"/></rect>"
        ));
        assert!(SvgDoc::parse(&src).unwrap_err().to_string().contains("values"));
        let at_cap = vec!["1"; MAX_VALUES].join(";");
        let src = svg(&format!(
            "<rect><animate attributeName=\"x\" values=\"{at_cap}\" dur=\"1s\"/></rect>"
        ));
        assert!(SvgDoc::parse(&src).is_ok());
    }

    #[test]
    fn a_use_bomb_is_refused_before_resvg_expands_it() {
        let mut bomb = String::from("<defs><g id=\"g0\"><rect width=\"1\" height=\"1\"/></g>");
        for i in 1..=24 {
            bomb.push_str(&format!(
                "<g id=\"g{i}\"><use href=\"#g{}\"/><use href=\"#g{}\"/></g>",
                i - 1,
                i - 1
            ));
        }
        bomb.push_str("</defs><use href=\"#g24\"/>");
        let e = SvgDoc::parse(&svg(&bomb)).unwrap_err().to_string();
        assert!(e.contains("more than 100000 nodes"), "{e}");

        let mut chain = String::from("<defs><g id=\"g0\"><rect width=\"1\" height=\"1\"/></g>");
        for i in 1..=200 {
            chain.push_str(&format!("<g id=\"g{i}\"><use href=\"#g{}\"/></g>", i - 1));
        }
        chain.push_str("</defs><use href=\"#g200\"/>");
        let e = SvgDoc::parse(&svg(&chain)).unwrap_err().to_string();
        assert!(e.contains("nests deeper"), "{e}");

        let sheet = format!(
            "<defs><symbol id=\"s\">{}</symbol></defs>{}",
            "<rect width=\"1\" height=\"1\"/>".repeat(100),
            (0..100).map(|i| format!("<use href=\"#s\" x=\"{i}\"/>")).collect::<String>()
        );
        assert!(
            SvgDoc::parse(&svg(&sheet)).is_ok(),
            "a hundred copies of a hundred shapes is fine"
        );
        let cycle = "<g id=\"a\"><use href=\"#b\"/></g><g id=\"b\"><use href=\"#a\"/></g><use href=\"#a\"/>";
        let e = SvgDoc::parse(&svg(cycle)).unwrap_err().to_string();
        assert!(
            e.contains("refers back"),
            "usvg would expand a cycle to its node limit first: {e}"
        );
        let own = "<g id=\"a\"><use href=\"#a\"/></g>";
        assert!(SvgDoc::parse(&svg(own)).is_err());
        let fe = "<rect id=\"r\" width=\"1\" height=\"1\"/><filter id=\"f\"><feImage href=\"#r\"/></filter><rect filter=\"url(#f)\" width=\"1\" height=\"1\"/>";
        assert!(SvgDoc::parse(&svg(fe)).is_ok());
    }

    /// A gallery of documents run through every moment: whatever `at` emits parses, carries no
    /// animation and is deterministic.
    #[test]
    fn every_moment_is_a_valid_static_document() {
        let gallery = [
            "<rect x=\"1\"><animate attributeName=\"x\" from=\"0\" to=\"50\" dur=\"3s\" repeatCount=\"indefinite\"/></rect>",
            "<g transform=\"rotate(5)\"><animateTransform attributeName=\"transform\" type=\"rotate\" from=\"0 50 50\" to=\"360 50 50\" dur=\"5s\" additive=\"sum\" repeatCount=\"2\"/><rect width=\"9\" height=\"9\"/></g>",
            "<text x=\"5\" y=\"20\" font-family=\"Geist\">hi<animate attributeName=\"fill\" values=\"red;blue;#0f0\" dur=\"4s\" fill=\"freeze\"/><animate attributeName=\"opacity\" values=\"0;1\" begin=\"1s\" dur=\"1s\" calcMode=\"discrete\"/></text>",
            "<path d=\"M0 0L9 9\"><animate attributeName=\"d\" values=\"M0 0L9 9;M0 0L30 9;M0 0L9 30\" dur=\"3s\"/><animateMotion path=\"M0 0\" dur=\"1s\"/></path>",
            "<defs><clipPath id=\"c\"><rect width=\"0\" height=\"10\"><animate attributeName=\"width\" to=\"90\" dur=\"2s\" fill=\"freeze\"/></rect></clipPath></defs><circle r=\"9\" clip-path=\"url(#c)\"/>",
        ];
        for inner in gallery {
            let d = doc(inner);
            let mut t = 0.0;
            while t < 12.0 {
                let out = d.at(t);
                assert_eq!(out, d.at(t), "deterministic");
                assert!(
                    !out.contains("<animate") && !out.contains("<set") && !out.contains("<animateTransform"),
                    "{out}"
                );
                let parsed = Document::parse(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
                resvg::usvg::Tree::from_xmltree(&parsed, &resvg::usvg::Options::default())
                    .unwrap_or_else(|e| panic!("{e}: {out}"));
                t += 0.37;
            }
        }
    }

    // ---- review fixes -------------------------------------------------------------------------

    #[test]
    fn an_attribute_name_cannot_inject_markup_into_a_frame() {
        let inner = "<rect id=\"r\" width=\"2\" height=\"2\"><animate attributeName='x=&quot;1&quot; href=&quot;/etc/passwd&quot; onload=&quot;a&quot; y' to=\"5\" dur=\"1s\" fill=\"freeze\"/><set attributeName=\"a&lt;b\" to=\"1\"/><set attributeName=\"a;b\" to=\"1\"/></rect>";
        let d = doc(inner);
        for t in [0.0, 0.5, 2.0] {
            let out = d.at(t);
            assert!(!out.contains("passwd") && !out.contains("onload"), "{out}");
            Document::parse(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        }
        assert!(
            d.warnings().iter().any(|w| w.contains("not an attribute name")),
            "{:?}",
            d.warnings()
        );
        assert!(!d.is_animated());
    }

    #[test]
    fn a_use_with_two_links_is_counted_as_the_one_resvg_draws() {
        // 5 * 2^12 - 3 nodes under g12: fine once defined, more than the limit drawn four times.
        let mut defs = String::from("<defs><g id=\"g0\"><rect width=\"1\" height=\"1\"/></g>");
        for i in 1..=12 {
            defs.push_str(&format!(
                "<g id=\"g{i}\"><use href=\"#g{}\"/><use href=\"#g{}\"/></g>",
                i - 1,
                i - 1
            ));
        }
        defs.push_str("</defs>");
        assert!(SvgDoc::parse(&svg(&format!("{defs}<use href=\"#g12\"/>"))).is_ok());
        for each in [
            "<use xlink:href=\"#g0\" href=\"#g12\"/>",
            "<use href=\"#g12\" xlink:href=\"#g0\"/>",
            "<use href=\"#g12\"/>",
        ] {
            let e = SvgDoc::parse(&svg(&format!("{defs}{}", each.repeat(4))))
                .unwrap_err()
                .to_string();
            assert!(e.contains("more than 100000 nodes"), "{each}: {e}");
        }
        // The cheap target alone is cheap, and so is a cheap `href` beside an `xlink:href` to the heavy one.
        for each in ["<use xlink:href=\"#g0\"/>", "<use href=\"#g0\" xlink:href=\"#g12\"/>"] {
            assert!(SvgDoc::parse(&svg(&format!("{defs}{}", each.repeat(4)))).is_ok(), "{each}");
        }
    }

    #[test]
    fn a_mask_or_a_paint_used_many_times_costs_what_it_holds_each_time() {
        // 5 * 2^11 - 3 nodes in a mask: fine once, over the limit when eight shapes draw it, from a few hundred bytes.
        let mut heavy = String::from("<g id=\"g0\"><rect width=\"1\" height=\"1\"/></g>");
        for i in 1..=11 {
            heavy.push_str(&format!(
                "<g id=\"g{i}\"><use href=\"#g{}\"/><use href=\"#g{}\"/></g>",
                i - 1,
                i - 1
            ));
        }
        for (prop, def) in [
            ("mask", "mask id=\"m\""),
            ("clip-path", "clipPath id=\"m\""),
            ("filter", "filter id=\"m\""),
            (
                "fill",
                "pattern id=\"m\" width=\"4\" height=\"4\" patternUnits=\"userSpaceOnUse\"",
            ),
            ("marker-end", "marker id=\"m\""),
            ("marker", "marker id=\"m\""),
        ] {
            let close = def.split(' ').next().unwrap();
            let doc = |users: &str| {
                svg(&format!("<defs>{heavy}<{def}><use href=\"#g11\"/></{def}></defs>{users}")
                    .replace(&format!("</{def}>"), &format!("</{close}>")))
            };
            let user = format!("<rect width=\"9\" height=\"9\" {prop}=\"url(#m)\"/>");
            let styled = format!("<rect width=\"9\" height=\"9\" style=\"{prop}: url(#m)\"/>");
            assert!(SvgDoc::parse(&doc(&user)).is_ok(), "{prop}: once is fine");
            for each in [&user, &styled] {
                let e = SvgDoc::parse(&doc(&each.repeat(8))).unwrap_err().to_string();
                assert!(e.contains("more than 100000 nodes"), "{prop}: {e}");
            }
        }
        // An ordinary gradient used a hundred times is nothing.
        let grad = "<defs><linearGradient id=\"g\"><stop offset=\"0\"/><stop offset=\"1\"/></linearGradient></defs>";
        let many = "<rect width=\"1\" height=\"1\" fill=\"url(#g)\" stroke=\"url(#g)\"/>".repeat(100);
        assert!(SvgDoc::parse(&svg(&format!("{grad}{many}"))).is_ok());
    }

    #[test]
    fn source_with_no_animation_element_in_it_cannot_move() {
        let still =
            svg("<defs><linearGradient id=\"g\"/></defs><rect width=\"5\" height=\"5\" fill=\"url(#g)\" style=\"opacity:.5\"/>");
        assert!(!may_animate(&still));
        let d = SvgDoc::parse(&still).unwrap();
        assert!(!d.is_animated());
        for t in [0.0, 0.3, 7.0, 1e6] {
            assert_eq!(d.at(t), still, "t = {t}");
        }
        // Every spelling of an animation element is "maybe": a document that does move says so.
        for element in [
            "animate",
            "animateColor",
            "animateTransform type=\"rotate\"",
            "animateMotion",
            "set",
            "x:animate xmlns:x=\"http://www.w3.org/2000/svg\"",
            "x:set xmlns:x=\"http://www.w3.org/2000/svg\"",
        ] {
            let moving = svg(&format!(
                "<rect width=\"5\" height=\"5\"><{element} attributeName=\"{}\" to=\"3\" dur=\"1s\"/></rect>",
                if element.starts_with("animateTransform") {
                    "transform"
                } else {
                    "x"
                }
            ));
            assert!(may_animate(&moving), "{element}");
        }
        // ...and what the reader says moves is always among them.
        for inner in [
            "<rect width=\"5\"><animate attributeName=\"x\" to=\"3\" dur=\"1s\"/></rect>",
            "<rect width=\"5\"><set attributeName=\"fill\" to=\"red\" begin=\"1s\"/></rect>",
            "<rect width=\"5\"><animateTransform attributeName=\"transform\" type=\"rotate\" to=\"90\" dur=\"1s\"/></rect>",
        ] {
            let d = SvgDoc::parse(&svg(inner)).unwrap();
            assert!(d.is_animated() && may_animate(d.sanitized()), "{inner}");
        }
    }

    /// A bomb: `copies` references of a heavy mask, none of them written as an attribute the guard
    /// reads from the document as it is stored.
    fn heavy_mask() -> String {
        let mut heavy = String::from("<g id=\"g0\"><rect width=\"1\" height=\"1\"/></g>");
        for i in 1..=11 {
            heavy.push_str(&format!(
                "<g id=\"g{i}\"><use href=\"#g{}\"/><use href=\"#g{}\"/></g>",
                i - 1,
                i - 1
            ));
        }
        format!(
            "<defs>{heavy}<mask id=\"m\"><use href=\"#g11\"/></mask><filter id=\"f\"><feImage href=\"#g11\"/></filter></defs>"
        )
    }

    #[test]
    fn nothing_a_document_animates_or_styles_can_reach_a_reference_the_guard_did_not_count() {
        let defs = heavy_mask();
        // SMIL driving the reference properties (or the id / class that selects them) is dropped, so
        // the document that was counted is the document that is drawn.
        for attr in ["mask", "filter", "clip-path", "marker-start", "id", "class"] {
            let rects = format!(
                "<rect width=\"9\" height=\"9\" id=\"r\"><animate attributeName=\"{attr}\" values=\"url(#m);url(#m)\" dur=\"1s\"/><set attributeName=\"{attr}\" to=\"m\"/></rect>"
            )
            .repeat(8);
            let d = SvgDoc::parse(&svg(&format!("{defs}{rects}"))).unwrap_or_else(|e| panic!("{attr}: {e}"));
            assert!(!d.is_animated(), "{attr}: {:?}", d.warnings());
            assert!(
                d.warnings().iter().any(|w| w.contains("animation was removed")),
                "{attr}: {:?}",
                d.warnings()
            );
        }
        let fills = format!(
            "{defs}{}",
            "<rect width=\"9\" height=\"9\" fill=\"red\"><animate attributeName=\"fill\" values=\"red;url(#m)\" dur=\"1s\"/></rect>".repeat(8)
        );
        assert!(
            !SvgDoc::parse(&svg(&fills)).unwrap().is_animated(),
            "a url() in a value is dropped"
        );
        // A gradient in a value is allowed to animate, a pattern is not.
        let gradient_fills = format!(
            "<linearGradient id=\"g\"><stop offset=\"0\"/></linearGradient><linearGradient id=\"h\"/>{}",
            "<rect width=\"9\" height=\"9\" fill=\"url(#g)\"><animate attributeName=\"fill\" values=\"url(#g);url(#h)\" dur=\"1s\"/></rect>".repeat(8)
        );
        let d = SvgDoc::parse(&svg(&gradient_fills)).unwrap();
        assert!(d.is_animated() && d.warnings().is_empty(), "{:?}", d.warnings());
        // A <style> rule is counted on everything it could apply to, so a class cannot spend a mask
        // a thousand times over without the guard seeing it...
        let ruled = format!(
            "{defs}<style>.a {{ mask: url(#m); filter: url(#f) }}</style>{}",
            "<rect class=\"a\" width=\"9\" height=\"9\"/>".repeat(8)
        );
        let e = SvgDoc::parse(&svg(&ruled)).unwrap_err().to_string();
        assert!(e.contains("more than 100000 nodes"), "{e}");
        // ...whichever way it names them: a type, an id, a list, one with an ancestor or a
        // pseudo-class, a rule inside @media, and a property spelled with a vendor prefix. (A rule that
        // also reaches the elements inside the mask it points at -- `*`, `[width]` -- makes the mask
        // refer to itself, which is refused as any loop is.)
        let circles = "<circle class=\"a\" id=\"r1\" r=\"9\"/>".repeat(8);
        for selector in [
            "circle",
            "#r1, circle.a",
            "g circle.a:first-child",
            "svg > circle",
            ".a",
            "circle.a[r]",
            "@media screen { circle",
        ] {
            let close = if selector.starts_with('@') { "}" } else { "" };
            let doc = format!("{defs}<style>{selector} {{ -webkit-mask: url(#m) }}{close}</style>{circles}");
            let e = SvgDoc::parse(&svg(&doc)).unwrap_err().to_string();
            assert!(e.contains("more than 100000 nodes"), "{selector}: {e}");
        }
        for selector in ["*", "[r]", ":not(.zzz)"] {
            let doc = format!("{defs}<style>{selector} {{ mask: url(#m) }}</style>{circles}");
            assert!(SvgDoc::parse(&svg(&doc)).is_err(), "{selector}");
        }
        // A rule no element could match costs nothing, and neither does one that points at a gradient or
        // at a clip that is small.
        let idle = format!(
            "{defs}<style>.nobody {{ mask: url(#m) }} circle {{ filter: url(#f) }}</style>{}",
            "<rect class=\"a\" width=\"9\" height=\"9\"/>".repeat(8)
        );
        assert!(SvgDoc::parse(&svg(&idle)).is_ok());
        let grad = "<linearGradient id=\"g\"><stop offset=\"0\"/></linearGradient><style>.a { fill: url(#g) }</style>";
        let d = SvgDoc::parse(&svg(&format!(
            "{grad}{}",
            "<rect class=\"a\" width=\"9\" height=\"9\"/>".repeat(50)
        )))
        .unwrap();
        assert!(d.sanitized().contains("fill: url(#g)"));
        // Illustrator's internal CSS: a clip per class, each used by a few elements.
        let clips = "<clipPath id=\"SVGID_1_\"><rect width=\"4\" height=\"4\"/></clipPath><style>.st0 { clip-path: url(#SVGID_1_) } .st1 { fill: #fff }</style>";
        let d = SvgDoc::parse(&svg(&format!(
            "{clips}{}",
            "<g class=\"st0\"><rect class=\"st1\" width=\"9\" height=\"9\"/></g>".repeat(20)
        )))
        .unwrap();
        assert!(d.sanitized().contains(".st0 { clip-path: url(#SVGID_1_) }"), "kept");
        assert!(d.warnings().is_empty(), "{:?}", d.warnings());
        // The last `mask` of a `style` attribute is the one resvg draws, and every one is counted.
        let last = format!(
            "{defs}{}",
            "<rect width=\"9\" height=\"9\" style=\"mask:none;mask:url(#m)\"/>".repeat(8)
        );
        assert!(SvgDoc::parse(&svg(&last)).is_err());
        // ...while the same references written on the elements are counted and refused.
        let attributes = format!("{defs}{}", "<rect width=\"9\" height=\"9\" mask=\"url(#m)\"/>".repeat(8));
        assert!(SvgDoc::parse(&svg(&attributes))
            .unwrap_err()
            .to_string()
            .contains("more than 100000 nodes"));
    }

    #[test]
    fn an_id_a_later_element_repeats_cannot_hide_a_heavier_target_from_the_guard() {
        fn drawn(group: &resvg::usvg::Group) -> usize {
            use resvg::usvg::Node;
            1 + group.mask().map_or(0, |m| drawn(m.root()))
                + group
                    .children()
                    .iter()
                    .map(|n| match n {
                        Node::Group(g) => drawn(g),
                        _ => 1,
                    })
                    .sum::<usize>()
        }
        // usvg reaches the last element of an id through `url(#id)`, the guard counted the first: with a
        // cheap mask first and the heavy one after it, eight rects looked like eight cheap masks and
        // were drawn as eight heavy ones. Now the heavy one has no id to be reached by.
        let cheap_first = format!(
            "<mask id=\"m\"><rect width=\"1\" height=\"1\"/></mask>{}{}",
            heavy_mask(),
            "<rect width=\"9\" height=\"9\" mask=\"url(#m)\"/>".repeat(8)
        );
        let d = SvgDoc::parse(&svg(&cheap_first)).unwrap();
        assert_eq!(d.sanitized().matches("id=\"m\"").count(), 1, "{}", d.sanitized());
        assert!(d.warnings().iter().any(|w| w.contains("repeated")), "{:?}", d.warnings());
        let tree = resvg::usvg::Tree::from_str(d.sanitized(), &resvg::usvg::Options::default()).unwrap();
        assert!(
            drawn(tree.root()) < 1000,
            "what is drawn is what was counted: {}",
            drawn(tree.root())
        );
        // The other way round, the heavy one is the one that stays, and is counted and refused.
        let heavy_first = format!(
            "{}<mask id=\"m\"><rect width=\"1\" height=\"1\"/></mask>{}",
            heavy_mask(),
            "<rect width=\"9\" height=\"9\" mask=\"url(#m)\"/>".repeat(8)
        );
        assert!(SvgDoc::parse(&svg(&heavy_first)).is_err());
    }

    #[test]
    fn a_pattern_or_filter_that_borrows_content_by_href_costs_what_it_borrows() {
        let defs = heavy_mask();
        // `p` has no children of its own: it takes the heavy ones from the mask's group through `href`... a
        // pattern may only inherit from a pattern, so the chain ends in a real one holding the bomb.
        let chain = format!(
            "{defs}<pattern id=\"base\" width=\"4\" height=\"4\" patternUnits=\"userSpaceOnUse\"><use href=\"#g11\"/></pattern><pattern id=\"p\" href=\"#base\"/>"
        );
        let once = format!("{chain}<rect width=\"9\" height=\"9\" fill=\"url(#p)\"/>");
        assert!(SvgDoc::parse(&svg(&once)).is_ok());
        let many = format!("{chain}{}", "<rect width=\"9\" height=\"9\" fill=\"url(#p)\"/>".repeat(8));
        let e = SvgDoc::parse(&svg(&many)).unwrap_err().to_string();
        assert!(e.contains("more than 100000 nodes"), "{e}");
        // A ring of gradients is tolerated as usvg tolerates it (each only lends its stops, so it is as
        // cheap as it looks); a loop of anything that draws more than stops is refused like one of
        // <use>s.
        let ring = "<linearGradient id=\"a\" href=\"#b\"/><linearGradient id=\"b\" href=\"#a\"/><linearGradient id=\"c\" href=\"#c\"/><rect fill=\"url(#a)\" width=\"1\" height=\"1\"/><rect fill=\"url(#c)\" width=\"1\" height=\"1\"/>";
        assert!(SvgDoc::parse(&svg(ring)).is_ok());
        let cyclic =
            "<pattern id=\"a\" href=\"#b\"/><pattern id=\"b\" href=\"#a\"/><rect fill=\"url(#a)\" width=\"1\" height=\"1\"/>";
        assert!(SvgDoc::parse(&svg(cyclic))
            .unwrap_err()
            .to_string()
            .contains("refers back to itself"));
        let uses = "<use id=\"a\" href=\"#b\"/><use id=\"b\" href=\"#a\"/>";
        assert!(SvgDoc::parse(&svg(uses)).is_err());
        // A chain of gradients is what Illustrator writes, and is fine.
        let gradients = "<linearGradient id=\"a\" href=\"#b\"/><linearGradient id=\"b\"><stop offset=\"0\"/></linearGradient><rect fill=\"url(#a)\" width=\"1\" height=\"1\"/>";
        assert!(SvgDoc::parse(&svg(gradients)).is_ok());
    }

    #[test]
    fn embedded_images_are_counted_by_the_pixels_each_draw_decodes() {
        let image = |w: u32, h: u32| images::tests::data_url("image/png", &images::tests::png_claiming(w, h));
        let ten_mp = image(4000, 2500);
        // One image: fine, and the document knows its widest.
        let one = format!("<image id=\"i\" href=\"{ten_mp}\" width=\"9\" height=\"9\"/>");
        let d = SvgDoc::parse(&svg(&one)).unwrap();
        assert_eq!(d.widest_image_pixels(), 10_000_000);
        assert_eq!(
            SvgDoc::parse(&svg("<rect width=\"1\" height=\"1\"/>"))
                .unwrap()
                .widest_image_pixels(),
            0
        );
        // Four draws of 10 MP are 40 MP, seven are 70 MP: <use> draws the file again each time.
        let uses = |n: usize| format!("<defs>{one}</defs>{}", "<use href=\"#i\"/>".repeat(n));
        assert!(SvgDoc::parse(&svg(&uses(3))).is_ok(), "30 MP");
        let e = SvgDoc::parse(&svg(&uses(7))).unwrap_err().to_string();
        assert!(e.contains("megapixels of embedded images"), "{e}");
        // The same uses of a 4 x 4 logo cost nothing.
        let small = format!(
            "<defs><image id=\"i\" href=\"data:image/png;base64,{}\" width=\"9\" height=\"9\"/></defs>",
            images::tests::RED_4X4
        );
        assert!(SvgDoc::parse(&svg(&format!("{small}{}", "<use href=\"#i\"/>".repeat(500)))).is_ok());
        // One over the per-image cap is cut by the sanitiser and so never counted.
        let d = SvgDoc::parse(&svg(&format!(
            "<image href=\"{}\" width=\"9\" height=\"9\"/>",
            image(5000, 5000)
        )))
        .unwrap();
        assert!(!d.sanitized().contains("data:"), "{}", d.sanitized());
        assert!(d.warnings().iter().any(|w| w.contains("5000 x 5000")), "{:?}", d.warnings());
    }

    #[test]
    fn a_filter_too_slow_to_draw_a_frame_is_refused_when_the_document_is_read() {
        let heavy = "<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology radius=\"2\"/></filter><rect width=\"100\" height=\"100\" filter=\"url(#f)\"/>";
        let e = SvgDoc::parse(&svg(heavy)).unwrap_err().to_string();
        assert!(e.contains("feMorphology"), "{e}");
        let light = "<filter id=\"f\"><feGaussianBlur stdDeviation=\"4\"/><feMorphology radius=\"0.05\"/></filter><rect width=\"100\" height=\"100\" filter=\"url(#f)\"/>";
        assert!(SvgDoc::parse(&svg(light)).is_ok());
    }

    #[test]
    fn a_style_that_is_empty_or_already_ends_in_a_semicolon_gets_no_empty_declaration() {
        let anim = "<animate attributeName=\"opacity\" from=\"0\" to=\"1\" dur=\"2s\" fill=\"freeze\"/>";
        for (style, want) in [
            ("", "<rect style=\"opacity:0.5\"></rect>"),
            ("  ", "<rect style=\"  opacity:0.5\"></rect>"),
            ("fill:red", "<rect style=\"fill:red;opacity:0.5\"></rect>"),
            ("fill:red;", "<rect style=\"fill:red;opacity:0.5\"></rect>"),
            ("fill:red; ", "<rect style=\"fill:red; opacity:0.5\"></rect>"),
        ] {
            let got = at(&format!("<rect style=\"{style}\">{anim}</rect>"), 1.0);
            assert_eq!(got, want, "style = {style:?}");
        }
        // The frame reads: resvg keeps the declaration (it is the group's opacity).
        let out = doc(&format!("<rect style=\"\" width=\"9\" height=\"9\">{anim}</rect>")).at(1.0);
        let parsed = Document::parse(&out).unwrap();
        let tree = resvg::usvg::Tree::from_xmltree(&parsed, &resvg::usvg::Options::default()).unwrap();
        assert!(
            tree.root()
                .children()
                .iter()
                .any(|n| matches!(n, resvg::usvg::Node::Group(g) if (g.opacity().get() - 0.5).abs() < 1e-6)),
            "{out}"
        );
    }

    #[test]
    fn an_end_that_never_resolves_leaves_the_interval_open() {
        let rect = |anim: &str| format!("<rect x=\"0\">{anim}</rect>");
        // A syncbase on nothing, on an animation that never begins, and an event: all "unresolved".
        for end in [
            "nothing.end",
            "never.begin",
            "click",
            "nothing.end; click",
            "nothing.begin+1s",
        ] {
            let inner = rect(&format!(
                "<animate id=\"never\" begin=\"indefinite\" attributeName=\"y\" to=\"1\" dur=\"1s\"/>\
                 <animate attributeName=\"x\" from=\"0\" to=\"10\" dur=\"2s\" end=\"{end}\" fill=\"freeze\"/>"
            ));
            let d = doc(&inner);
            assert_eq!(at(&inner, 1.0), "<rect x=\"5\"></rect>", "end = {end}");
            assert_eq!(d.natural_duration(), Some(2.0), "end = {end}");
        }
        // A list with a time before the begin is "no interval", as ever; so is an end at the begin.
        for end in ["1s", "2s", "0s"] {
            let inner = rect(&format!(
                "<animate attributeName=\"x\" from=\"0\" to=\"10\" begin=\"2s\" dur=\"4s\" end=\"{end}\" fill=\"freeze\"/>"
            ));
            assert_eq!(at(&inner, 4.0), "<rect x=\"0\"></rect>", "end = {end}");
        }
        // A resolved end still ends it: a syncbase on an animation that does end.
        let inner = rect(
            "<animate id=\"a\" attributeName=\"y\" to=\"1\" dur=\"1s\"/>\
             <animate attributeName=\"x\" from=\"0\" to=\"10\" dur=\"4s\" end=\"a.end\"/>",
        );
        let early = at(&inner, 0.5);
        assert!(early.contains("x=\"1.25\"") && early.contains("y=\"0.5\""), "{early}");
        assert_eq!(at(&inner, 2.0), "<rect x=\"0\"></rect>");
    }

    #[test]
    fn accumulate_means_nothing_to_a_to_only_animation() {
        let rect = |anim: &str| format!("<rect x=\"0\">{anim}</rect>");
        let to_only = rect("<animate attributeName=\"x\" to=\"10\" dur=\"1s\" repeatCount=\"3\" accumulate=\"sum\"/>");
        let plain = rect("<animate attributeName=\"x\" to=\"10\" dur=\"1s\" repeatCount=\"3\"/>");
        for t in [0.5, 1.5, 2.5] {
            assert_eq!(at(&to_only, t), at(&plain, t), "t = {t}");
        }
        assert_eq!(at(&to_only, 2.5), "<rect x=\"5\"></rect>");
        // With a from it does add up.
        let from_to = rect("<animate attributeName=\"x\" from=\"0\" to=\"10\" dur=\"1s\" repeatCount=\"3\" accumulate=\"sum\"/>");
        assert_eq!(at(&from_to, 2.5), "<rect x=\"25\"></rect>");
    }

    #[test]
    fn an_animation_starts_from_what_an_inherited_property_is_here() {
        // The font size comes from the group, so `by` adds to 20, not to the default.
        let by = "<g font-size=\"20\"><text><animate attributeName=\"font-size\" by=\"10\" additive=\"sum\" dur=\"2s\" fill=\"freeze\"/>t</text></g>";
        assert!(at(by, 2.0).contains("font-size:30"), "{}", at(by, 2.0));
        // A `to` animation of a colour starts from the ancestor's fill, midway between red and blue.
        let to = "<g fill=\"#ff0000\"><rect width=\"9\" height=\"9\"><animate attributeName=\"fill\" to=\"#0000ff\" dur=\"2s\" fill=\"freeze\"/></rect></g>";
        let mid = at(to, 1.0);
        assert!(mid.contains("fill:#800080") || mid.contains("fill:#7f007f"), "{mid}");
        // The nearest ancestor wins, a style declaration beats its attribute, and the element's own beats both.
        let nearest = "<g fill=\"#00ff00\"><g style=\"fill:#ff0000\" fill=\"#0000ff\"><rect width=\"9\" height=\"9\"><animate attributeName=\"fill\" to=\"#0000ff\" dur=\"2s\" fill=\"freeze\"/></rect></g></g>";
        assert!(
            at(nearest, 1.0).contains("fill:#800080") || at(nearest, 1.0).contains("fill:#7f007f"),
            "{}",
            at(nearest, 1.0)
        );
        let own = "<g fill=\"#00ff00\"><rect fill=\"#ff0000\" width=\"9\" height=\"9\"><animate attributeName=\"fill\" to=\"#0000ff\" dur=\"2s\" fill=\"freeze\"/></rect></g>";
        assert!(
            at(own, 1.0).contains("fill:#800080") || at(own, 1.0).contains("fill:#7f007f"),
            "{}",
            at(own, 1.0)
        );
        // A property that is not inherited starts from its own initial value whatever the group says.
        let opacity = "<g opacity=\"0.2\"><rect width=\"9\" height=\"9\"><animate attributeName=\"opacity\" to=\"0\" dur=\"2s\" fill=\"freeze\"/></rect></g>";
        assert!(at(opacity, 1.0).contains("opacity:0.5"), "{}", at(opacity, 1.0));
    }

    #[test]
    fn a_repeat_count_past_what_a_u32_holds_still_counts_repeats() {
        let inner = "<rect x=\"0\"><animate attributeName=\"x\" from=\"0\" to=\"1\" dur=\"1s\" repeatCount=\"indefinite\" accumulate=\"sum\"/></rect>";
        // 5e9 repeats in: more than u32::MAX (4.29e9), which `as u32` used to clamp to.
        assert_eq!(at(inner, 5_000_000_000.5), "<rect x=\"5000000000.5\"></rect>");
        assert_eq!(at(inner, 3.5), "<rect x=\"3.5\"></rect>");
    }
}
