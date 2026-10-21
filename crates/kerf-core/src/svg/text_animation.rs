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
//!
//! Each property can have a **curve** (`curve.rs`) for the path it takes to its value: After Effects' speed and
//! influence ease, or a spring. Without one it moves in a straight line, as it always did.

use roxmltree::Document;
use serde::{Deserialize, Serialize};

use super::curve::{Curve, Curves};
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
/// The largest a property may be at an amount of 1 (and so the farthest a curve may carry it): a move in em, a
/// turn in degrees, a scale, a blur in em.
pub(crate) const MAX_MOVE: f64 = 20.0;
pub(crate) const MAX_TURN: f64 = 3600.0;
pub(crate) const MAX_SCALE: f64 = 10.0;
pub(crate) const MAX_BLUR: f64 = 1.0;
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
    /// How each property travels to its value (`curve.rs`): a straight line unless one is given.
    #[serde(skip_serializing_if = "Curves::is_empty")]
    pub curves: Curves,
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
            curves: Curves::default(),
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
        within("x", p.x, -MAX_MOVE, MAX_MOVE)?;
        within("y", p.y, -MAX_MOVE, MAX_MOVE)?;
        within("scale", p.scale, 0.0, MAX_SCALE)?;
        within("rotation", p.rotation, -MAX_TURN, MAX_TURN)?;
        within("opacity", p.opacity, 0.0, 1.0)?;
        within("blur", p.blur, 0.0, MAX_BLUR)?;
        self.curves.check()?;
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

    /// Whether it moves, scales or turns the text: the properties a motion curve is for.
    pub fn has_motion(&self) -> bool {
        self.properties.moves() || self.properties.scales() || self.properties.turns()
    }

    /// This animation with `curve` (`None`: a straight line) on each of x, y, scale and rotation that it changes, and
    /// no curve on those it does not; the opacity and blur curves stay as they are (a fade has nothing to overshoot,
    /// and a spring on it only trembles below 1). A curve for an animation that moves, scales and turns nothing is
    /// refused rather than dropped.
    pub fn with_motion_curve(&self, curve: Option<Curve>) -> Result<TextAnimation> {
        if let Some(c) = &curve {
            c.check("the curve")?;
            if !self.has_motion() {
                return Err(invalid(
                    "this animation changes no x, y, scale or rotation, so a motion curve has nothing to shape",
                ));
            }
        }
        let p = &self.properties;
        let mut out = self.clone();
        out.curves.x = curve.filter(|_| p.x != 0.0);
        out.curves.y = curve.filter(|_| p.y != 0.0);
        out.curves.scale = curve.filter(|_| p.scales());
        out.curves.rotation = curve.filter(|_| p.turns());
        Ok(out)
    }

    /// The curve every one of x, y, scale and rotation that this animation changes shares (`Some(None)`: a straight
    /// line, which an animation that changes none of them has too), or `None` when they differ.
    pub fn motion_curve(&self) -> Option<Option<Curve>> {
        let p = &self.properties;
        let used = [
            (p.x != 0.0).then_some(self.curves.x),
            (p.y != 0.0).then_some(self.curves.y),
            p.scales().then_some(self.curves.scale),
            p.turns().then_some(self.curves.rotation),
        ];
        let mut used = used.into_iter().flatten();
        let first = used.next().unwrap_or(None);
        used.all(|c| c == first).then_some(first)
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
    use super::super::curve::{curve_preset, curve_presets};
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

    fn spring() -> Curve {
        curve_preset("spring").unwrap().curve.unwrap()
    }

    #[test]
    fn an_animation_without_curves_writes_none_and_reads_curves_back() {
        // The presets are what they were: no `curves` in their JSON.
        for p in text_animation_presets() {
            assert!(!p.animation.to_attribute().contains("curves"), "{}", p.id);
        }
        let a = rise().with_motion_curve(Some(spring())).unwrap();
        let json = a.to_attribute();
        assert!(json.ends_with(r#""curves":{"y":{"kind":"spring","bounce":0.5}}}"#), "{json}");
        assert_eq!(TextAnimation::from_attribute(&json).unwrap(), a);
        // Curves are optional, null is none, and an unknown property or field is refused.
        assert!(TextAnimation::from_attribute(r#"{"properties":{"y":1},"curves":{}}"#).is_ok());
        let nulls = TextAnimation::from_attribute(r#"{"properties":{"y":1},"curves":{"y":null}}"#).unwrap();
        assert_eq!(nulls.curves, Curves::default());
        assert!(TextAnimation::from_attribute(r#"{"properties":{"y":1},"curves":{"z":null}}"#).is_err());
        assert!(TextAnimation::from_attribute(r#"{"properties":{"y":1},"curves":{"y":{"kind":"spring"}}}"#).is_err());
        assert!(TextAnimation::from_attribute(r#"{"properties":{"y":1},"curves":null}"#).is_err());
    }

    #[test]
    fn a_curve_out_of_bounds_is_refused_by_the_property_it_is_on() {
        let on = |f: fn(&mut Curves, Curve)| {
            let mut a = rise();
            f(&mut a.curves, Curve::Spring { bounce: 0.99 });
            a.check().unwrap_err().to_string()
        };
        assert!(on(|c, v| c.y = Some(v)).contains("y curve") && on(|c, v| c.y = Some(v)).contains("bounce"));
        assert!(on(|c, v| c.x = Some(v)).contains("x curve"));
        assert!(on(|c, v| c.scale = Some(v)).contains("scale curve"));
        assert!(on(|c, v| c.rotation = Some(v)).contains("rotation curve"));
        assert!(on(|c, v| c.opacity = Some(v)).contains("opacity curve"));
        assert!(on(|c, v| c.blur = Some(v)).contains("blur curve"));
        let mut b = rise();
        b.curves.y = Some(Curve::Speed {
            out_speed: 9.0,
            out_influence: 0.3,
            in_speed: 0.0,
            in_influence: 0.3,
        });
        assert!(set_text_animation(&doc(r#"<text id="t">a</text>"#), "t", Some(&b))
            .unwrap_err()
            .to_string()
            .contains("out speed"));
    }

    #[test]
    fn a_motion_curve_goes_on_what_the_animation_moves_and_nothing_else() {
        // rise changes y and opacity; pop scale and opacity; spin_in scale, rotation and opacity.
        let r = rise().with_motion_curve(Some(spring())).unwrap();
        assert_eq!(
            r.curves,
            Curves {
                y: Some(spring()),
                ..Curves::default()
            },
            "only y: not x (unchanged), not opacity"
        );
        let spin = text_animation_preset("spin_in")
            .unwrap()
            .animation
            .with_motion_curve(Some(spring()))
            .unwrap();
        assert_eq!((spin.curves.scale, spin.curves.rotation), (Some(spring()), Some(spring())));
        assert_eq!(
            (spin.curves.x, spin.curves.y, spin.curves.opacity, spin.curves.blur),
            (None, None, None, None)
        );
        // A straight line takes them off again, and leaves a fade's own curve alone.
        let mut fading = r.clone();
        fading.curves.opacity = Some(Curve::Spring { bounce: 0.2 });
        let straight = fading.with_motion_curve(None).unwrap();
        assert_eq!(straight.curves.y, None);
        assert_eq!(straight.curves.opacity, Some(Curve::Spring { bounce: 0.2 }));
        // What it reads back: one curve, none, or a mix.
        assert_eq!(r.motion_curve(), Some(Some(spring())));
        assert_eq!(rise().motion_curve(), Some(None));
        let mut mixed = spin.clone();
        mixed.curves.rotation = curve_preset("overshoot").unwrap().curve;
        assert_eq!(mixed.motion_curve(), None);
        let mut half = spin;
        half.curves.rotation = None;
        assert_eq!(half.motion_curve(), None, "one of two moved properties has it");
        // A property the animation does not change is not read, so a stale curve on it does not make a mix.
        let mut stale = r;
        stale.curves.x = curve_preset("overshoot").unwrap().curve;
        assert_eq!(stale.motion_curve(), Some(Some(spring())));
        // Nothing to move: a curve is refused, a straight line is not.
        let fade = text_animation_preset("fade_in").unwrap().animation;
        assert!(!fade.has_motion() && rise().has_motion());
        let err = fade.with_motion_curve(Some(spring())).unwrap_err().to_string();
        assert!(err.contains("nothing to shape"), "{err}");
        assert_eq!(fade.with_motion_curve(None).unwrap(), fade);
        let wild = Curve::Spring { bounce: 2.0 };
        assert!(rise()
            .with_motion_curve(Some(wild))
            .unwrap_err()
            .to_string()
            .contains("bounce"));
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
                "a spring on a rise",
                doc(r#"<text id="t" x="5" y="50">Hi</text>"#),
                "t",
                Some(rise().with_motion_curve(curve_preset("spring").unwrap().curve).unwrap()),
            ),
            (
                "curves of every kind, replacing a plain animation",
                doc(&format!(
                    r#"<text id="t" data-kerf-animate='{}'>Hi</text>"#,
                    rise().to_attribute()
                )),
                "t",
                Some({
                    let mut a = text_animation_preset("spin_in").unwrap().animation;
                    a.curves = Curves {
                        scale: curve_preset("overshoot").unwrap().curve,
                        rotation: Some(Curve::Speed {
                            out_speed: -1.5,
                            out_influence: 0.25,
                            in_speed: 0.5,
                            in_influence: 0.125,
                        }),
                        opacity: Some(Curve::Spring { bounce: 0.05 }),
                        ..Curves::default()
                    };
                    a
                }),
            ),
            (
                "a curve out of bounds is refused",
                doc(r#"<text id="t">a</text>"#),
                "t",
                Some({
                    let mut a = rise();
                    a.curves.y = Some(Curve::Spring { bounce: 0.95 });
                    a
                }),
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
            "curves": curve_presets(),
            "cases": cases,
            "parses": parse_corpus(),
            "motion": motion_corpus(),
        })
    }

    /// `with_motion_curve` and `motion_curve` over animations that move different things, with every preset curve, one
    /// of the extremes and some that are refused: for the frontend's `withMotionCurve` and `motionCurveOf`.
    fn motion_corpus() -> Vec<serde_json::Value> {
        let preset = |id: &str| text_animation_preset(id).unwrap().animation;
        let read = |a: &TextAnimation| match a.motion_curve() {
            Some(curve) => serde_json::json!({ "mixed": false, "curve": curve }),
            None => serde_json::json!({ "mixed": true, "curve": null }),
        };
        let extreme = Curve::Speed {
            out_speed: -5.0,
            out_influence: 0.01,
            in_speed: 5.0,
            in_influence: 1.0,
        };
        let mut curves: Vec<(String, Option<Curve>)> = curve_presets().into_iter().map(|p| (p.id, p.curve)).collect();
        curves.push(("an extreme".into(), Some(extreme)));
        curves.push(("a spring that bounces too much".into(), Some(Curve::Spring { bounce: 0.9 })));
        curves.push((
            "an influence of nothing".into(),
            Some(Curve::Speed {
                out_speed: 1.0,
                out_influence: 0.0,
                in_speed: 1.0,
                in_influence: 0.5,
            }),
        ));
        let animations = [
            ("rise", preset("rise")),
            ("pop", preset("pop")),
            ("spin_in", preset("spin_in")),
            ("fade_in", preset("fade_in")),
            ("wiggle", preset("wiggle")),
            ("blur_in", preset("blur_in")),
        ];
        let mut cases = Vec::new();
        for (name, animation) in &animations {
            for (curve_name, curve) in &curves {
                let expect = match animation.with_motion_curve(*curve) {
                    Ok(a) => serde_json::json!({ "ok": { "motion": read(&a), "animation": a } }),
                    Err(e) => serde_json::json!({ "err": e.to_string() }),
                };
                cases.push(serde_json::json!({
                    "name": format!("{name} with {curve_name}"),
                    "animation": animation,
                    "curve": curve,
                    "expect": expect,
                }));
            }
        }
        // Animations that already have curves: replaced, cleared, left alone on a fade, mixed.
        let spring = curve_preset("spring").unwrap().curve;
        let over = curve_preset("overshoot").unwrap().curve;
        let mut with_fade = preset("rise");
        with_fade.curves.opacity = Some(Curve::Spring { bounce: 0.3 });
        with_fade.curves.x = over;
        let mut mixed = preset("spin_in");
        mixed.curves.scale = spring;
        mixed.curves.rotation = over;
        let mut half = preset("spin_in");
        half.curves.scale = spring;
        for (name, animation) in [
            ("a fade's curve and a stale x curve", with_fade),
            ("two moved properties with two curves", mixed),
            ("one of two moved properties curved", half),
        ] {
            for (curve_name, curve) in [("none", None), ("a spring", spring)] {
                let expect = match animation.with_motion_curve(curve) {
                    Ok(a) => serde_json::json!({ "ok": { "motion": read(&a), "animation": a } }),
                    Err(e) => serde_json::json!({ "err": e.to_string() }),
                };
                cases.push(serde_json::json!({
                    "name": format!("{name} given {curve_name}"),
                    "animation": animation,
                    "curve": curve,
                    "expect": expect,
                }));
            }
            cases.push(serde_json::json!({
                "name": format!("{name} read"),
                "animation": animation,
                "read": read(&animation),
            }));
        }
        cases
    }

    /// Attribute JSON, as `from_attribute` reads it: for `parseAnimation`, which has to refuse what serde does.
    fn parse_corpus() -> Vec<serde_json::Value> {
        let wiggle = |w: &str| format!(r#"{{"properties":{{"opacity":0}},"wiggle":{w}}}"#);
        let curved = |c: &str| format!(r#"{{"properties":{{"y":0.5,"scale":0}},"curves":{c}}}"#);
        let cases: Vec<(&str, String)> = vec![
            ("only what differs from the defaults", r#"{"properties":{"y":0.5}}"#.into()),
            ("every field", rise().to_attribute()),
            ("a wiggle with its defaults", wiggle("{}")),
            ("no wiggle", wiggle("null")),
            ("a seed", wiggle(r#"{"seed":7}"#)),
            ("the largest seed", wiggle(r#"{"seed":4294967295}"#)),
            ("a seed past a u32", wiggle(r#"{"seed":4294967296}"#)),
            ("a negative seed", wiggle(r#"{"seed":-1}"#)),
            ("a seed with a fraction", wiggle(r#"{"seed":7.5}"#)),
            ("a seed written 7.0", wiggle(r#"{"seed":7.0}"#)),
            ("a seed in exponent form", wiggle(r#"{"seed":1e3}"#)),
            ("a seed that is a string", wiggle(r#"{"seed":"7"}"#)),
            ("a seed that is null", wiggle(r#"{"seed":null}"#)),
            ("a seed of minus zero", wiggle(r#"{"seed":-0}"#)),
            ("an unknown field", r#"{"properties":{"opacity":0},"extra":1}"#.into()),
            (
                "a field named like an inherited one",
                r#"{"constructor":1,"properties":{"opacity":0}}"#.into(),
            ),
            (
                "a field named __proto__",
                r#"{"__proto__":{},"properties":{"opacity":0}}"#.into(),
            ),
            (
                "a field named toString",
                r#"{"toString":1,"properties":{"opacity":0}}"#.into(),
            ),
            (
                "an inherited name inside the properties",
                r#"{"properties":{"opacity":0,"hasOwnProperty":1}}"#.into(),
            ),
            ("an inherited name inside the wiggle", wiggle(r#"{"constructor":1}"#)),
            (
                "an inherited name inside the range",
                r#"{"properties":{"opacity":0},"range":{"valueOf":1}}"#.into(),
            ),
            (
                "an unknown name inside the offset",
                r#"{"properties":{"opacity":0},"range":{"offset":{"by":1}}}"#.into(),
            ),
            ("a wrong type", r#"{"duration":"1","properties":{"opacity":0}}"#.into()),
            (
                "a null where a number goes",
                r#"{"delay":null,"properties":{"opacity":0}}"#.into(),
            ),
            (
                "a null where an object goes",
                r#"{"range":null,"properties":{"opacity":0}}"#.into(),
            ),
            ("an unknown word", r#"{"by":"letter","properties":{"opacity":0}}"#.into()),
            (
                "a number out of range",
                r#"{"duration":31,"properties":{"opacity":0}}"#.into(),
            ),
            (
                "a number too big for a float",
                r#"{"duration":1e999,"properties":{"opacity":0}}"#.into(),
            ),
            ("a spring", curved(r#"{"y":{"kind":"spring","bounce":0.5}}"#)),
            (
                "a speed ease with whole numbers",
                curved(r#"{"scale":{"kind":"speed","out_speed":4,"out_influence":1,"in_speed":0,"in_influence":1}}"#),
            ),
            ("a curve that is null", curved(r#"{"y":null}"#)),
            ("no curves at all", curved("{}")),
            ("curves that are null", curved("null")),
            ("an unknown property", curved(r#"{"z":{"kind":"spring","bounce":0.5}}"#)),
            ("an inherited name as a property", curved(r#"{"constructor":null}"#)),
            ("an unknown kind", curved(r#"{"y":{"kind":"wobble","bounce":0.5}}"#)),
            ("a curve with no kind", curved(r#"{"y":{"bounce":0.5}}"#)),
            ("a spring with no bounce", curved(r#"{"y":{"kind":"spring"}}"#)),
            (
                "a spring with another field",
                curved(r#"{"y":{"kind":"spring","bounce":0.5,"out_speed":1}}"#),
            ),
            (
                "a spring that bounces too much",
                curved(r#"{"y":{"kind":"spring","bounce":0.9}}"#),
            ),
            ("a spring of a string", curved(r#"{"y":{"kind":"spring","bounce":"0.5"}}"#)),
            (
                "a speed ease missing a handle",
                curved(r#"{"y":{"kind":"speed","out_speed":1,"out_influence":0.5,"in_speed":1}}"#),
            ),
            (
                "a speed ease with no influence",
                curved(r#"{"y":{"kind":"speed","out_speed":1,"out_influence":0,"in_speed":1,"in_influence":0.5}}"#),
            ),
            (
                "a speed ease too fast",
                curved(r#"{"y":{"kind":"speed","out_speed":5.5,"out_influence":0.5,"in_speed":1,"in_influence":0.5}}"#),
            ),
            ("a curve that is a string", curved(r#"{"y":"spring"}"#)),
            ("a curve that is an array", curved(r#"{"y":[]}"#)),
            ("a kind that is a number", curved(r#"{"y":{"kind":1,"bounce":0.5}}"#)),
            ("nothing that changes", "{}".into()),
            ("an array", "[]".into()),
            ("null", "null".into()),
            ("not json", "not json".into()),
        ];
        cases
            .into_iter()
            .map(|(name, raw)| {
                let read = TextAnimation::from_attribute(&raw).ok();
                serde_json::json!({ "name": name, "raw": raw, "read": read })
            })
            .collect()
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
