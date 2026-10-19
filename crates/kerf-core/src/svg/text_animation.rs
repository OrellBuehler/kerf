//! A text's animation, stored as data: a few numbers on the `<text>` (`data-kerf-animate`, JSON) that the text
//! engine compiles into per-glyph SMIL when the text is set as outlines (`glyph_smil.rs`, `outline.rs`).
//!
//! The model is a text animator in the After Effects sense. A **selector** gives every unit of the text (a
//! character, a word or a line) an *amount* between 0 and 1 that changes with time; the animator's
//! **properties** say how far a unit moves, turns, scales, fades and blurs at an amount of 1. A unit at amount
//! 0 is the text as drawn without the animation.
//!
//! * The **range selector** is a window `start..end` (fractions of the text) that slides by `offset`, from
//!   `offset.from` to `offset.to` over the animation, and a `shape` that says the amount inside it. A unit's
//!   place is the middle of its slot (`(i + 0.5) / n`), so a window `0..1` slid from 0 to 1 passes over every
//!   unit in turn: with a `square` shape that is a typewriter.
//! * The **wiggly selector** multiplies the range's amount by smooth noise between `min` and `max`, a new value
//!   `rate` times a second, from a seed (so it is the same on every machine and every export), and
//!   `correlation` says how much the units share one noise. It fades in and out over the animation's ends so
//!   the text comes to rest.
//! * **Properties are in em** (the glyph's own font size) so an animation reads the same at any size: a unit
//!   moves `x` / `y` em, is `scale` times its size, turns `rotation` degrees about its centre, is `opacity` as
//!   opaque and is blurred by `blur` em, all at an amount of 1.
//!
//! The animation runs from `delay` for `duration` seconds on the title's own clock; before and after it a unit
//! is where the selectors put it at the start and at the end.

use roxmltree::Document;
use serde::{Deserialize, Serialize};

use super::outline::TEXT_MARK;
use super::sanitize::{apply, escape_attr, nesting_exceeds, plain_attribute, start_tag_end, SVG_NS};
use super::{invalid, xml_options, MAX_BYTES, MAX_DEPTH};
use crate::error::Result;

/// The attribute of a `<text>` that holds its animation, JSON.
pub(crate) const ANIMATE_MARK: &str = "data-kerf-animate";

/// The longest an animation waits to begin, and the longest it runs, in seconds.
pub const MAX_DELAY: f64 = 30.0;
pub const MAX_DURATION: f64 = 30.0;
pub const MIN_DURATION: f64 = 0.05;
/// How far a window or its offset may reach, in text lengths.
pub const MAX_REACH: f64 = 10.0;
/// The largest a stored animation may be, in bytes of JSON.
const MAX_JSON: usize = 8 * 1024;

/// What a selector counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnimateBy {
    /// Every character that is not a space.
    #[default]
    Character,
    /// Every run of characters between spaces.
    Word,
    /// Every line (a wrapped line, or one a newline ends).
    Line,
}

/// How the window slides: the pace of `offset.from` to `offset.to`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    /// Slow to start (`t²`).
    EaseIn,
    /// Slow to arrive (`1 - (1 - t)²`).
    EaseOut,
    /// Slow at both ends (`t²(3 - 2t)`).
    EaseInOut,
}

/// The amount inside the window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// 1 inside the window, 0 outside.
    #[default]
    Square,
    /// 0 before the window, rising across it, 1 after it (units the window has passed are fully animated).
    RampUp,
    /// 1 before the window, falling across it, 0 after it.
    RampDown,
    /// 0 outside; rising to 1 in the middle of the window and falling again.
    Triangle,
    /// 0 outside; a half circle across the window.
    Round,
    /// 0 outside; a smooth bump across the window.
    Smooth,
}

/// A window of the text and how it slides.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RangeSelector {
    /// Where the window starts, as a fraction of the text (0 is its beginning, 1 its end).
    pub start: f64,
    /// Where it ends; after `start`.
    pub end: f64,
    /// How far the window is moved, from the animation's start to its end, in text lengths. Equal values
    /// leave it where it is.
    pub offset: Offset,
    pub shape: Shape,
}

impl Default for RangeSelector {
    fn default() -> Self {
        Self {
            start: 0.0,
            end: 1.0,
            offset: Offset::default(),
            shape: Shape::Square,
        }
    }
}

/// A window's offset at the start and at the end of the animation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Offset {
    pub from: f64,
    pub to: f64,
}

/// Smooth noise that multiplies the range's amount.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Wiggle {
    /// New noise values a second (0.1 to 30).
    pub rate: f64,
    /// The smallest and largest multiplier, -1 to 1. -1 to 1 swings both ways; 0 to 1 only ever adds.
    pub min: f64,
    pub max: f64,
    /// 0: every unit wiggles on its own; 1: they all wiggle together.
    pub correlation: f64,
    /// Another seed is another pattern; the same seed is the same on every machine.
    pub seed: u32,
}

impl Default for Wiggle {
    fn default() -> Self {
        Self {
            rate: 2.0,
            min: -1.0,
            max: 1.0,
            correlation: 0.0,
            seed: 0,
        }
    }
}

/// What a unit does at an amount of 1. The neutral value of each is what leaves the text as it is.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Properties {
    /// Moves right by this many em (negative: left).
    pub x: f64,
    /// Moves down by this many em (negative: up).
    pub y: f64,
    /// Is this many times its size (1 is neutral; 0 shrinks it away).
    pub scale: f64,
    /// Turns this many degrees clockwise about its centre.
    pub rotation: f64,
    /// Is this opaque, 0 to 1 (1 is neutral).
    pub opacity: f64,
    /// Is blurred by this many em (0 is neutral).
    pub blur: f64,
}

impl Default for Properties {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation: 0.0,
            opacity: 1.0,
            blur: 0.0,
        }
    }
}

impl Properties {
    pub(crate) fn moves(&self) -> bool {
        self.x != 0.0 || self.y != 0.0
    }

    pub(crate) fn turns(&self) -> bool {
        self.rotation != 0.0
    }

    pub(crate) fn scales(&self) -> bool {
        self.scale != 1.0
    }

    pub(crate) fn fades(&self) -> bool {
        self.opacity != 1.0
    }

    pub(crate) fn blurs(&self) -> bool {
        self.blur != 0.0
    }

    fn animates_something(&self) -> bool {
        self.moves() || self.turns() || self.scales() || self.fades() || self.blurs()
    }
}

/// A text's animation. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TextAnimation {
    /// What the selectors count.
    pub by: AnimateBy,
    /// Seconds before it begins (0 to 30).
    pub delay: f64,
    /// Seconds it takes (0.05 to 30).
    pub duration: f64,
    /// The pace of the window's slide.
    pub easing: Easing,
    pub range: RangeSelector,
    /// Noise on top of the range, or none.
    pub wiggle: Option<Wiggle>,
    pub properties: Properties,
}

impl Default for TextAnimation {
    fn default() -> Self {
        Self {
            by: AnimateBy::Character,
            delay: 0.0,
            duration: 1.0,
            easing: Easing::Linear,
            range: RangeSelector::default(),
            wiggle: None,
            properties: Properties::default(),
        }
    }
}

fn within(name: &str, v: f64, lo: f64, hi: f64) -> Result<()> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(invalid(format!("a text animation's {name} is between {lo} and {hi}")))
    }
}

impl TextAnimation {
    /// Every number finite and within its bounds, and something for the properties to do.
    pub fn check(&self) -> Result<()> {
        within("delay", self.delay, 0.0, MAX_DELAY)?;
        within("duration", self.duration, MIN_DURATION, MAX_DURATION)?;
        let r = &self.range;
        within("range start", r.start, -MAX_REACH, MAX_REACH)?;
        within("range end", r.end, -MAX_REACH, MAX_REACH)?;
        if r.end - r.start < 0.001 {
            return Err(invalid("a text animation's range ends after it starts"));
        }
        within("offset from", r.offset.from, -MAX_REACH, MAX_REACH)?;
        within("offset to", r.offset.to, -MAX_REACH, MAX_REACH)?;
        if let Some(w) = &self.wiggle {
            within("wiggle rate", w.rate, 0.1, 30.0)?;
            within("wiggle min", w.min, -1.0, 1.0)?;
            within("wiggle max", w.max, -1.0, 1.0)?;
            if w.min > w.max {
                return Err(invalid("a text animation's wiggle min is at most its max"));
            }
            within("wiggle correlation", w.correlation, 0.0, 1.0)?;
        }
        let p = &self.properties;
        within("x", p.x, -20.0, 20.0)?;
        within("y", p.y, -20.0, 20.0)?;
        within("scale", p.scale, 0.0, 10.0)?;
        within("rotation", p.rotation, -3600.0, 3600.0)?;
        within("opacity", p.opacity, 0.0, 1.0)?;
        within("blur", p.blur, 0.0, 1.0)?;
        if !p.animates_something() {
            return Err(invalid(
                "a text animation changes nothing: give it an x, y, scale, rotation, opacity or blur other than the neutral one",
            ));
        }
        Ok(())
    }

    /// The JSON an attribute holds.
    pub fn to_attribute(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// An animation read from an attribute's JSON and checked, or why not.
    pub fn from_attribute(raw: &str) -> std::result::Result<Self, String> {
        if raw.len() > MAX_JSON {
            return Err(format!("larger than {} KiB", MAX_JSON / 1024));
        }
        let a: TextAnimation = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        a.check().map_err(|e| e.to_string())?;
        Ok(a)
    }

    /// When the animation has run, on the title's clock.
    pub fn end(&self) -> f64 {
        self.delay + self.duration
    }
}

/// `svg` (a title in its text form) with the `<text>` whose `id` is `element` given `animation`, or none: its
/// `data-kerf-animate` attribute is replaced (or taken away), at the end of its start tag. Nothing else of the
/// document moves.
pub fn set_text_animation(svg: &str, element: &str, animation: Option<&TextAnimation>) -> Result<String> {
    if let Some(a) = animation {
        a.check()?;
    }
    if svg.len() > MAX_BYTES {
        return Err(invalid(format!("the SVG is larger than {} KiB", MAX_BYTES / 1024)));
    }
    if nesting_exceeds(svg, MAX_DEPTH) {
        return Err(invalid(format!("the SVG is nested deeper than {MAX_DEPTH} elements")));
    }
    if svg.contains(TEXT_MARK) {
        return Err(invalid(
            "set_text_animation takes a title in its text form (restore_text first)",
        ));
    }
    let doc = Document::parse_with_options(svg, xml_options()).map_err(|e| invalid(format!("not valid XML: {e}")))?;
    let mut named = doc
        .descendants()
        .filter(|n| n.is_element() && plain_attribute(n, "id") == Some(element));
    let node = match (named.next(), named.next()) {
        (Some(only), None) => only,
        (None, _) => return Err(invalid(format!("the title has no element with the id “{element}”"))),
        (Some(_), Some(_)) => return Err(invalid(format!("more than one element has the id “{element}”"))),
    };
    if node.tag_name().name() != "text" || node.tag_name().namespace() != Some(SVG_NS) {
        return Err(invalid(format!(
            "“{element}” is a <{}>; an animation is given to a <text>",
            node.tag_name().name()
        )));
    }
    let mut cuts = Vec::new();
    for attr in node
        .attributes()
        .filter(|a| a.namespace().is_none() && a.name() == ANIMATE_MARK)
    {
        let range = attr.range();
        let start = svg[..range.start]
            .rfind(|c: char| !c.is_ascii_whitespace())
            .map_or(range.start, |i| i + 1);
        cuts.push((start..range.end, String::new()));
    }
    if let Some(a) = animation {
        let open_end = start_tag_end(svg, node.range().start).ok_or_else(|| invalid("the text cannot be read"))?;
        let at = if svg[..open_end].ends_with("/>") {
            open_end - 2
        } else {
            open_end - 1
        };
        cuts.push((at..at, format!(" {ANIMATE_MARK}='{}'", escape_attr(&a.to_attribute(), '\''))));
    }
    Ok(apply(svg, cuts))
}

/// The animation `svg`'s `<text>` named `element` has: `None` for no such text, no animation, or one that does
/// not read. (An outlined text keeps its attribute on the group it became, so that reads too.)
pub fn text_animation_of(svg: &str, element: &str) -> Option<TextAnimation> {
    if svg.len() > MAX_BYTES || nesting_exceeds(svg, MAX_DEPTH) {
        return None;
    }
    let doc = Document::parse_with_options(svg, xml_options()).ok()?;
    let node = doc.descendants().find(|n| {
        n.is_element()
            && (n.has_tag_name((SVG_NS, "text")) || (n.has_tag_name((SVG_NS, "g")) && n.attribute(TEXT_MARK).is_some()))
            && plain_attribute(n, "id") == Some(element)
    })?;
    TextAnimation::from_attribute(plain_attribute(&node, ANIMATE_MARK)?).ok()
}

/// A named animation a user can start from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextAnimationPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub animation: TextAnimation,
}

fn preset(id: &str, name: &str, description: &str, animation: TextAnimation) -> TextAnimationPreset {
    TextAnimationPreset {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        animation,
    }
}

fn sweep(width: f64, shape: Shape) -> RangeSelector {
    RangeSelector {
        start: 0.0,
        end: width,
        offset: Offset { from: -width, to: 1.0 },
        shape,
    }
}

/// The eight presets, in the order a picker lists them. A preset is a plain [`TextAnimation`]: choosing one
/// copies it onto the text, and nothing refers back to it.
pub fn text_animation_presets() -> Vec<TextAnimationPreset> {
    vec![
        preset(
            "rise",
            "Rise",
            "Characters fade in one after another, rising into place.",
            TextAnimation {
                duration: 1.4,
                easing: Easing::EaseOut,
                range: sweep(0.4, Shape::RampUp),
                properties: Properties {
                    y: 0.5,
                    opacity: 0.0,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
        preset(
            "fade_in",
            "Fade in",
            "Words fade in one after another.",
            TextAnimation {
                by: AnimateBy::Word,
                duration: 1.2,
                range: sweep(0.5, Shape::RampUp),
                properties: Properties {
                    opacity: 0.0,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
        preset(
            "typewriter",
            "Typewriter",
            "Characters appear one at a time, left to right.",
            TextAnimation {
                duration: 1.5,
                range: RangeSelector {
                    start: 0.0,
                    end: 1.0,
                    offset: Offset { from: 0.0, to: 1.0 },
                    shape: Shape::Square,
                },
                properties: Properties {
                    opacity: 0.0,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
        preset(
            "pop",
            "Pop",
            "Characters grow from nothing to their size, one after another.",
            TextAnimation {
                duration: 1.0,
                easing: Easing::EaseOut,
                range: sweep(0.3, Shape::RampUp),
                properties: Properties {
                    scale: 0.0,
                    opacity: 0.0,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
        preset(
            "blur_in",
            "Blur in",
            "Characters come into focus one after another.",
            TextAnimation {
                duration: 1.4,
                easing: Easing::EaseOut,
                range: sweep(0.5, Shape::RampUp),
                properties: Properties {
                    opacity: 0.0,
                    blur: 0.25,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
        preset(
            "spin_in",
            "Spin in",
            "Characters turn and grow into place, one after another.",
            TextAnimation {
                duration: 1.4,
                easing: Easing::EaseOut,
                range: sweep(0.35, Shape::RampUp),
                properties: Properties {
                    scale: 0.3,
                    rotation: -90.0,
                    opacity: 0.0,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
        preset(
            "drop_in",
            "Drop in",
            "Words drop in from above, one after another.",
            TextAnimation {
                by: AnimateBy::Word,
                duration: 1.2,
                easing: Easing::EaseOut,
                range: sweep(0.5, Shape::RampUp),
                properties: Properties {
                    y: -0.7,
                    opacity: 0.0,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
        preset(
            "wiggle",
            "Wiggle",
            "Every character jitters on its own, then settles.",
            TextAnimation {
                duration: 3.0,
                wiggle: Some(Wiggle {
                    rate: 6.0,
                    min: -1.0,
                    max: 1.0,
                    correlation: 0.0,
                    seed: 7,
                }),
                properties: Properties {
                    x: 0.02,
                    y: 0.06,
                    rotation: 3.0,
                    ..Properties::default()
                },
                ..TextAnimation::default()
            },
        ),
    ]
}

/// The preset with this id.
pub fn text_animation_preset(id: &str) -> Option<TextAnimationPreset> {
    text_animation_presets().into_iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::super::sanitize::value_range;
    use super::*;

    fn doc(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 200">{body}</svg>"#)
    }

    fn rise() -> TextAnimation {
        text_animation_preset("rise").unwrap().animation
    }

    #[test]
    fn every_preset_is_valid_and_ids_are_unique() {
        let presets = text_animation_presets();
        assert_eq!(presets.len(), 8);
        let mut ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 8);
        for p in &presets {
            p.animation.check().unwrap_or_else(|e| panic!("{}: {e}", p.id));
            let again = TextAnimation::from_attribute(&p.animation.to_attribute()).unwrap();
            assert_eq!(again, p.animation, "{}", p.id);
        }
    }

    #[test]
    fn an_animation_is_written_on_the_text_and_replaced_or_taken_away() {
        let src = doc(r#"<text id="t" x="5" y="50" fill="red">Hi</text>"#);
        let set = set_text_animation(&src, "t", Some(&rise())).unwrap();
        assert!(set.contains(" data-kerf-animate='{"), "{set}");
        assert_eq!(text_animation_of(&set, "t"), Some(rise()));
        let mut other = rise();
        other.delay = 0.5;
        let replaced = set_text_animation(&set, "t", Some(&other)).unwrap();
        assert_eq!(replaced.matches(ANIMATE_MARK).count(), 1);
        assert_eq!(text_animation_of(&replaced, "t"), Some(other));
        assert_eq!(set_text_animation(&replaced, "t", None).unwrap(), src);
        let empty = doc(r#"<text id="t"/>"#);
        let on_empty = set_text_animation(&empty, "t", Some(&rise())).unwrap();
        assert!(on_empty.contains("'/>"), "{on_empty}");
        assert_eq!(set_text_animation(&on_empty, "t", None).unwrap(), empty);
    }

    #[test]
    fn only_one_text_by_its_id_takes_an_animation_and_it_must_do_something() {
        let src = doc(r#"<rect id="r" width="1" height="1"/><text id="t">a</text><text id="d">b</text><text id="d">c</text>"#);
        let err = |el: &str, a: &TextAnimation| set_text_animation(&src, el, Some(a)).unwrap_err().to_string();
        assert!(err("r", &rise()).contains("<rect>"));
        assert!(err("x", &rise()).contains("no element"));
        assert!(err("d", &rise()).contains("more than one"));
        assert!(err("t", &TextAnimation::default()).contains("changes nothing"));
        let mut bad = rise();
        bad.duration = 0.0;
        assert!(err("t", &bad).contains("duration"));
        bad = rise();
        bad.range.end = bad.range.start;
        assert!(err("t", &bad).contains("range"));
        bad = rise();
        bad.properties.opacity = f64::NAN;
        assert!(err("t", &bad).contains("opacity"));
        bad = rise();
        bad.wiggle = Some(Wiggle {
            min: 0.5,
            max: -0.5,
            ..Wiggle::default()
        });
        assert!(err("t", &bad).contains("wiggle"));
        let outlined = doc(r#"<g data-kerf-text="" id="t"></g>"#);
        assert!(set_text_animation(&outlined, "t", Some(&rise())).is_err());
    }

    #[test]
    fn json_from_an_attribute_must_be_ours_and_in_bounds() {
        assert!(TextAnimation::from_attribute(r#"{"properties":{"opacity":0}}"#).is_ok());
        assert!(TextAnimation::from_attribute(r#"{"properties":{"opacity":0},"extra":1}"#).is_err());
        assert!(TextAnimation::from_attribute(r#"{"by":"letter","properties":{"opacity":0}}"#).is_err());
        assert!(TextAnimation::from_attribute(r#"{"duration":1e999,"properties":{"opacity":0}}"#).is_err());
        assert!(TextAnimation::from_attribute("not json").is_err());
        assert!(TextAnimation::from_attribute(&" ".repeat(MAX_JSON + 1)).is_err());
        // Missing fields take their defaults.
        let a = TextAnimation::from_attribute(r#"{"properties":{"y":0.5}}"#).unwrap();
        assert_eq!((a.duration, a.by, a.properties.opacity), (1.0, AnimateBy::Character, 1.0));
    }

    const CORPUS_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/fixtures/text-animation-corpus.json"
    );

    /// Titles, an animation set on one of their texts, and what `set_text_animation` wrote (or refused) and
    /// `text_animation_of` reads back: for the frontend's port (`title-text.ts`'s `withTextAnimation` and
    /// `titleTexts`, replayed by `text-animation-corpus.test.ts`).
    fn corpus() -> serde_json::Value {
        let mut slow = rise();
        slow.delay = 0.25;
        slow.duration = 2.5;
        slow.by = AnimateBy::Word;
        let wiggle = text_animation_preset("wiggle").unwrap().animation;
        let mut tricky = rise();
        tricky.properties.x = 0.125;
        let cases: Vec<(&str, String, &str, Option<TextAnimation>)> = vec![
            (
                "a bare text",
                doc(r#"<text id="t" x="5" y="50">Hi</text>"#),
                "t",
                Some(rise()),
            ),
            (
                "replaces the one it had",
                doc(&format!(
                    r#"<text id="t" x="5" y="50" data-kerf-animate='{}' fill="red">Hi</text>"#,
                    wiggle.to_attribute()
                )),
                "t",
                Some(slow.clone()),
            ),
            (
                "taken away",
                doc(&format!(
                    r#"<text id="t" x="5" y="50" data-kerf-animate='{}'>Hi</text>"#,
                    rise().to_attribute()
                )),
                "t",
                None,
            ),
            (
                "taken away, double quoted and spaced",
                doc(r#"<text id="t"  data-kerf-animate="{&quot;properties&quot;:{&quot;y&quot;:1}}"   x="1">a</text>"#),
                "t",
                None,
            ),
            ("a self-closing text", doc(r#"<text id="t" x="1"/>"#), "t", Some(wiggle)),
            (
                "next to a box",
                doc(r#"<text id="t" style="inline-size:90px !important" data-kerf-fit="shrink">a</text>"#),
                "t",
                Some(slow),
            ),
            (
                "a number that needs no rounding",
                doc(r#"<text id="t">a</text>"#),
                "t",
                Some(tricky),
            ),
            (
                "nothing to change",
                doc(r#"<text id="t">a</text>"#),
                "t",
                Some(TextAnimation::default()),
            ),
            ("not a text", doc(r#"<rect id="r" width="1" height="1"/>"#), "r", Some(rise())),
            ("no such id", doc(r#"<text id="t">a</text>"#), "x", Some(rise())),
            (
                "an outlined title is refused",
                doc(r#"<g data-kerf-text="" id="t"></g>"#),
                "t",
                Some(rise()),
            ),
            (
                "a bad duration is refused first",
                doc(r#"<text id="t">a</text>"#),
                "t",
                Some(TextAnimation { duration: 0.0, ..rise() }),
            ),
            (
                "a space before the end of a self-closing tag",
                doc(r#"<text id="t" x="1" />"#),
                "t",
                Some(rise()),
            ),
            (
                "a repeated id",
                doc(r#"<text id="d">b</text><text id="d">c</text>"#),
                "d",
                Some(rise()),
            ),
        ];
        let cases: Vec<serde_json::Value> = cases
            .into_iter()
            .map(|(name, svg, element, animation)| {
                let expect = match set_text_animation(&svg, element, animation.as_ref()) {
                    Ok(out) => serde_json::json!({ "ok": { "read": text_animation_of(&out, element), "svg": out } }),
                    Err(e) => serde_json::json!({ "err": e.to_string() }),
                };
                serde_json::json!({ "name": name, "svg": svg, "element": element, "animation": animation, "expect": expect })
            })
            .collect();
        serde_json::json!({
            "about": "Written by kerf-core (svg/text_animation.rs); replayed by text-animation-corpus.test.ts. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- text_animation_corpus",
            "presets": text_animation_presets(),
            "cases": cases,
        })
    }

    #[test]
    fn text_animation_corpus_is_what_this_engine_writes() {
        let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
        if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
            std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
            return;
        }
        let on_disk = std::fs::read_to_string(CORPUS_PATH)
            .expect("frontend/src/lib/fixtures/text-animation-corpus.json — generate it with KERF_BLESS_CORPUS=1")
            .replace("\r\n", "\n");
        assert!(
            on_disk == fresh,
            "the text animation corpus is stale: regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core \
             --no-default-features -- text_animation_corpus, then run `bun test` — the replay says what withTextAnimation has to follow"
        );
    }

    #[test]
    fn a_value_in_the_attribute_is_escaped_for_its_quote() {
        // JSON holds `"`, never `'`, so the single quote wraps it; a `'` that does get in is escaped.
        let a = rise();
        let svg = set_text_animation(&doc(r#"<text id="t">a</text>"#), "t", Some(&a)).unwrap();
        let parsed = Document::parse(&svg).unwrap();
        let node = parsed.descendants().find(|n| n.attribute("id") == Some("t")).unwrap();
        assert_eq!(node.attribute(ANIMATE_MARK).unwrap(), a.to_attribute());
        assert!(value_range(&svg, node.attributes().find(|x| x.name() == ANIMATE_MARK).unwrap().range()).is_some());
    }
}
