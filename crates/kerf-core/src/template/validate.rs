//! What a manifest, its controls and a control's value must look like. Pure: nothing here reads a
//! file or a font.

use std::collections::HashSet;

use uuid::Uuid;

use super::model::{TemplateControl, TemplateKind, TemplateManifest, FORMAT, VERSION};
use super::{MAX_CONTROLS, MAX_FONTS, MAX_TARGETS};
use crate::error::{Error, Result};
use crate::model::MAX_SVG_SECONDS;
use crate::svg::splice::{Elements, Part};

pub(super) fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidArgument(msg.into())
}

/// The longest name, label or family, in characters.
const MAX_NAME_CHARS: usize = 120;
const MAX_LABEL_CHARS: usize = 80;
const MAX_DESCRIPTION_CHARS: usize = 2000;
/// The longest value of a `text` control, in characters.
pub(super) const MAX_TEXT_CHARS: usize = 2000;
/// The longest family a `font` control may name.
pub(super) const MAX_FAMILY_CHARS: usize = 64;
/// The widest a template may declare itself (an 16K frame).
const MAX_DIMENSION: u32 = 16_384;

/// Attributes a `color` control may write.
const COLOR_ATTRIBUTES: &[&str] = &["fill", "stroke", "stop-color", "flood-color", "lighting-color", "color"];

/// Attributes no control may write: the element's identity, its styling hooks, its links, and anything
/// that is script or namespace plumbing.
fn forbidden_attribute(name: &str) -> bool {
    matches!(name, "id" | "class" | "style" | "href" | "xlink:href") || name.starts_with("xmlns") || name.starts_with("on")
}

/// Whether `s` has a control character other than a tab or a line break (XML cannot carry them, and a
/// label or name should not either).
pub(super) fn has_control_chars(s: &str) -> bool {
    s.chars().any(|c| c.is_control() && !matches!(c, '\t' | '\n'))
}

/// `[A-Za-z][A-Za-z0-9_-]{0,63}`: a control's id is a key in JSON and a name in a tool call.
pub(super) fn safe_control_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// An attribute name as the source spells it: an XML name with at most one prefix.
fn attribute_name_ok(name: &str) -> bool {
    let ncname = |s: &str| {
        let mut chars = s.chars();
        s.len() <= 64
            && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    match name.split_once(':') {
        Some((prefix, local)) => prefix == "xlink" && ncname(local),
        None => ncname(name),
    }
}

/// A template's manifest, field by field: the format and version, an id that is a file name, a name, a
/// length and a shape, and the controls and fonts it lists.
pub(super) fn validate_manifest(m: &TemplateManifest) -> Result<()> {
    if m.format != FORMAT {
        return Err(invalid(format!("not a Kerf template (format “{}”)", m.format)));
    }
    check_version(m.version)?;
    check_id(&m.id)?;
    check_text("the name", &m.name, MAX_NAME_CHARS, true)?;
    check_text("the description", &m.description, MAX_DESCRIPTION_CHARS, false)?;
    if !m.duration.is_finite() || m.duration <= 0.0 || m.duration > MAX_SVG_SECONDS {
        return Err(invalid("the duration must be a number of seconds above 0"));
    }
    if !(1..=MAX_DIMENSION).contains(&m.width) || !(1..=MAX_DIMENSION).contains(&m.height) {
        return Err(invalid(format!("the width and height must be 1 to {MAX_DIMENSION} pixels")));
    }
    validate_controls_shape(&m.controls)?;
    if m.fonts.len() > MAX_FONTS {
        return Err(invalid(format!("a template carries at most {MAX_FONTS} fonts")));
    }
    let mut files = HashSet::new();
    for font in &m.fonts {
        if !font_file_ok(&font.file) {
            return Err(invalid(format!(
                "“{}” is not a font file name (fonts/<name>.ttf or .otf)",
                font.file
            )));
        }
        if !files.insert(font.file.as_str()) {
            return Err(invalid(format!("the font file “{}” is listed twice", font.file)));
        }
        check_text("a font's family", &font.family, MAX_NAME_CHARS, true)?;
    }
    Ok(())
}

/// A manifest `version` this build can read.
pub(super) fn check_version(version: u32) -> Result<()> {
    match version.cmp(&VERSION) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => Err(invalid(format!(
            "this template is format version {version}, made by a newer Kerf (this one reads version {VERSION}) — update Kerf to use it"
        ))),
        std::cmp::Ordering::Less => Err(invalid(format!("template format version {version} is not supported"))),
    }
}

/// The template id as a file name: a UUID in its canonical (lower-case, hyphenated) spelling.
pub(super) fn check_id(id: &str) -> Result<()> {
    match Uuid::parse_str(id) {
        Ok(parsed) if parsed.hyphenated().to_string() == id => Ok(()),
        _ => Err(invalid("a template's id must be a lower-case UUID")),
    }
}

fn check_text(what: &str, text: &str, max: usize, required: bool) -> Result<()> {
    if required && text.trim().is_empty() {
        return Err(invalid(format!("{what} is empty")));
    }
    if text.chars().count() > max {
        return Err(invalid(format!("{what} is longer than {max} characters")));
    }
    if has_control_chars(text) {
        return Err(invalid(format!("{what} has a control character in it")));
    }
    Ok(())
}

/// `fonts/<name>.ttf|otf`, the name made of plain characters (no separator, no `..`).
pub(super) fn font_file_ok(file: &str) -> bool {
    let Some(name) = file.strip_prefix("fonts/") else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    (lower.ends_with(".ttf") || lower.ends_with(".otf"))
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
}

/// The controls, without the title they point into: ids that are unique and safe, a kind and the
/// attributes it takes, targets, bounds only on a number.
pub fn validate_controls_shape(controls: &[TemplateControl]) -> Result<()> {
    if controls.len() > MAX_CONTROLS {
        return Err(invalid(format!("a template has at most {MAX_CONTROLS} controls")));
    }
    let mut ids = HashSet::new();
    for c in controls {
        if !safe_control_id(&c.id) {
            return Err(invalid(format!(
                "“{}” is not a control id: start with a letter, then letters, digits, - or _ (64 at most)",
                c.id
            )));
        }
        if !ids.insert(c.id.as_str()) {
            return Err(invalid(format!("two controls are called “{}”", c.id)));
        }
        check_text(&format!("the label of “{}”", c.id), &c.label, MAX_LABEL_CHARS, true)?;
        if c.targets.is_empty() || c.targets.len() > MAX_TARGETS {
            return Err(invalid(format!("“{}” needs 1 to {MAX_TARGETS} targets", c.id)));
        }
        for t in &c.targets {
            check_text(&format!("a target of “{}”", c.id), &t.element, 200, true)?;
            check_target_attribute(c, t.attribute.as_deref())?;
        }
        if c.kind == TemplateKind::Number {
            for bound in [c.min, c.max].into_iter().flatten() {
                if !bound.is_finite() {
                    return Err(invalid(format!("the bounds of “{}” must be numbers", c.id)));
                }
            }
            if matches!((c.min, c.max), (Some(lo), Some(hi)) if lo > hi) {
                return Err(invalid(format!("the minimum of “{}” is above its maximum", c.id)));
            }
        } else if c.min.is_some() || c.max.is_some() {
            return Err(invalid(format!(
                "only a number control has a minimum or maximum (“{}”)",
                c.id
            )));
        }
    }
    Ok(())
}

fn check_target_attribute(c: &TemplateControl, attribute: Option<&str>) -> Result<()> {
    let id = &c.id;
    match (c.kind, attribute) {
        (TemplateKind::Text, None) => Ok(()),
        (TemplateKind::Text, Some(_)) => Err(invalid(format!("“{id}” is a text control: its targets take no attribute"))),
        (_, None) => Err(invalid(format!(
            "“{id}” is a {} control: each target needs an attribute",
            c.kind.as_str()
        ))),
        (kind, Some(name)) => {
            if !attribute_name_ok(name)
                || forbidden_attribute(name) && !(kind == TemplateKind::Image && matches!(name, "href" | "xlink:href"))
            {
                return Err(invalid(format!("“{name}” cannot be set by a control (“{id}”)")));
            }
            let allowed = match kind {
                TemplateKind::Color => COLOR_ATTRIBUTES.contains(&name),
                TemplateKind::Font => name == "font-family",
                TemplateKind::Image => matches!(name, "href" | "xlink:href"),
                TemplateKind::Number => true,
                TemplateKind::Text => false,
            };
            if allowed {
                Ok(())
            } else {
                Err(invalid(format!("a {} control cannot write “{name}” (“{id}”)", kind.as_str())))
            }
        }
    }
}

/// The controls against the title (already sanitised, so an id is on one element at most): every target
/// element exists exactly once, has the attribute it sets, is the sort of element its kind needs, and no
/// two controls write the same place.
pub fn validate_controls(controls: &[TemplateControl], svg: &str) -> Result<()> {
    validate_controls_shape(controls)?;
    if controls.is_empty() {
        return Ok(());
    }
    let elements = Elements::parse(svg)?;
    let mut places: Vec<(&str, Option<&str>)> = Vec::new();
    for c in controls {
        for t in &c.targets {
            match elements.count(&t.element) {
                1 => {}
                0 => {
                    return Err(invalid(format!(
                        "“{}” targets the element “{}”, which the title does not have",
                        c.id, t.element
                    )))
                }
                n => {
                    return Err(invalid(format!(
                        "“{}” targets “{}”, which {n} elements are called",
                        c.id, t.element
                    )))
                }
            }
            let tag = elements.tag(&t.element)?;
            match (c.kind, tag) {
                (TemplateKind::Text, "text" | "tspan" | "textPath") | (TemplateKind::Image, "image") => {}
                (TemplateKind::Text, _) => {
                    return Err(invalid(format!(
                        "“{}” is a text control but “{}” is a <{tag}>: aim it at a <text> or <tspan>",
                        c.id, t.element
                    )))
                }
                (TemplateKind::Image, _) => {
                    return Err(invalid(format!(
                        "“{}” is an image control but “{}” is a <{tag}>, not an <image>",
                        c.id, t.element
                    )))
                }
                _ => {}
            }
            if let Some(attribute) = t.attribute.as_deref() {
                if !elements.has(&t.element, Part::Attribute(attribute))? {
                    return Err(invalid(format!(
                        "“{}” sets “{attribute}” on “{}”, which has no such attribute",
                        c.id, t.element
                    )));
                }
            }
            let place = (t.element.as_str(), t.attribute.as_deref());
            if places.contains(&place) {
                return Err(invalid(format!(
                    "more than one control writes {} of “{}”",
                    place.1.map_or("the text", |a| a),
                    t.element
                )));
            }
            places.push(place);
        }
    }
    // A control that replaces an element's text replaces what is inside it, including another control's
    // target.
    for (outer, attribute) in &places {
        if attribute.is_some() {
            continue;
        }
        for (inner, _) in &places {
            if inner != outer && elements.contains(outer, inner)? {
                return Err(invalid(format!(
                    "the text of “{outer}” is a control, and it holds “{inner}”, which another control sets"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::model::TemplateTarget;

    fn control(id: &str, kind: TemplateKind, element: &str, attribute: Option<&str>) -> TemplateControl {
        TemplateControl {
            id: id.to_string(),
            label: id.to_string(),
            kind,
            targets: vec![TemplateTarget {
                element: element.to_string(),
                attribute: attribute.map(str::to_string),
            }],
            min: None,
            max: None,
        }
    }

    fn manifest() -> TemplateManifest {
        TemplateManifest {
            format: FORMAT.to_string(),
            version: VERSION,
            id: Uuid::new_v4().to_string(),
            name: "Card".to_string(),
            description: String::new(),
            duration: 4.0,
            width: 1080,
            height: 1920,
            controls: Vec::new(),
            fonts: Vec::new(),
        }
    }

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 100 100">
<rect id="bg" fill="#112233" width="10" height="10"/>
<text id="tag">hi</text>
<text id="outer">a<tspan id="inner">b</tspan></text>
<image id="logo" xlink:href="data:image/png;base64,AAAA"/>
<g id="group"/>
</svg>"##;

    #[test]
    fn a_good_manifest_passes_and_each_field_is_checked() {
        assert!(validate_manifest(&manifest()).is_ok());
        let bad = |edit: &dyn Fn(&mut TemplateManifest)| {
            let mut m = manifest();
            edit(&mut m);
            validate_manifest(&m).unwrap_err().to_string()
        };
        assert!(bad(&|m| m.format = "other".into()).contains("not a Kerf template"));
        assert!(bad(&|m| m.version = 2).contains("newer Kerf"));
        assert!(bad(&|m| m.version = 0).contains("not supported"));
        assert!(bad(&|m| m.id = "../etc".into()).contains("UUID"));
        assert!(bad(&|m| m.id = Uuid::new_v4().to_string().to_uppercase()).contains("UUID"));
        assert!(bad(&|m| m.name = "  ".into()).contains("name is empty"));
        assert!(bad(&|m| m.name = "a\u{7}b".into()).contains("control character"));
        assert!(bad(&|m| m.duration = 0.0).contains("duration"));
        assert!(bad(&|m| m.duration = f64::NAN).contains("duration"));
        assert!(bad(&|m| m.duration = MAX_SVG_SECONDS + 1.0).contains("duration"));
        assert!(bad(&|m| m.width = 0).contains("width and height"));
        assert!(bad(&|m| m.height = MAX_DIMENSION + 1).contains("width and height"));
        assert!(bad(&|m| m.fonts = vec![super::super::ManifestFont {
            file: "../x.ttf".into(),
            family: "A".into()
        }])
        .contains("font file name"));
        assert!(bad(&|m| m.fonts = vec![
            super::super::ManifestFont {
                file: "fonts/a.ttf".into(),
                family: "A".into()
            },
            super::super::ManifestFont {
                file: "fonts/a.ttf".into(),
                family: "B".into()
            },
        ])
        .contains("twice"));
    }

    #[test]
    fn control_ids_are_unique_and_safe_and_each_kind_takes_its_own_attributes() {
        let ok = |c: TemplateControl| validate_controls_shape(&[c]);
        assert!(ok(control("tagline", TemplateKind::Text, "tag", None)).is_ok());
        assert!(ok(control("brand", TemplateKind::Color, "bg", Some("fill"))).is_ok());
        assert!(ok(control("size", TemplateKind::Number, "bg", Some("width"))).is_ok());
        assert!(ok(control("face", TemplateKind::Font, "tag", Some("font-family"))).is_ok());
        assert!(ok(control("logo", TemplateKind::Image, "logo", Some("xlink:href"))).is_ok());
        for id in ["", "1a", "a b", "a\"b", "a.b", &"x".repeat(65)] {
            assert!(ok(control(id, TemplateKind::Text, "tag", None)).is_err(), "{id:?}");
        }
        assert!(validate_controls_shape(&[
            control("a", TemplateKind::Text, "tag", None),
            control("a", TemplateKind::Text, "outer", None),
        ])
        .unwrap_err()
        .to_string()
        .contains("two controls"));
        assert!(
            ok(control("t", TemplateKind::Text, "tag", Some("fill"))).is_err(),
            "text takes none"
        );
        assert!(ok(control("c", TemplateKind::Color, "bg", None)).is_err(), "colour needs one");
        assert!(
            ok(control("c", TemplateKind::Color, "bg", Some("width"))).is_err(),
            "not a colour attribute"
        );
        assert!(ok(control("f", TemplateKind::Font, "tag", Some("fill"))).is_err());
        assert!(ok(control("i", TemplateKind::Image, "logo", Some("src"))).is_err());
        for attr in [
            "id", "style", "class", "href", "onload", "xmlns", "xmlns:x", "a b", "foo:bar", "\"x",
        ] {
            assert!(ok(control("n", TemplateKind::Number, "bg", Some(attr))).is_err(), "{attr}");
        }
        let mut bounded = control("n", TemplateKind::Number, "bg", Some("width"));
        bounded.min = Some(2.0);
        bounded.max = Some(1.0);
        assert!(ok(bounded.clone()).unwrap_err().to_string().contains("above its maximum"));
        bounded.min = Some(f64::NAN);
        assert!(ok(bounded).is_err());
        let mut stray = control("t", TemplateKind::Text, "tag", None);
        stray.min = Some(0.0);
        assert!(ok(stray).unwrap_err().to_string().contains("only a number"));
        let mut none = control("t", TemplateKind::Text, "tag", None);
        none.targets.clear();
        assert!(ok(none).is_err());
        let mut blank = control("t", TemplateKind::Text, "tag", None);
        blank.label = " ".into();
        assert!(ok(blank).is_err());
    }

    #[test]
    fn controls_are_checked_against_the_title() {
        let ok = |cs: Vec<TemplateControl>| validate_controls(&cs, SVG);
        assert!(ok(vec![
            control("tagline", TemplateKind::Text, "tag", None),
            control("brand", TemplateKind::Color, "bg", Some("fill")),
            control("logo", TemplateKind::Image, "logo", Some("xlink:href")),
        ])
        .is_ok());
        let err = |cs: Vec<TemplateControl>| validate_controls(&cs, SVG).unwrap_err().to_string();
        assert!(err(vec![control("a", TemplateKind::Text, "nope", None)]).contains("does not have"));
        assert!(err(vec![control("a", TemplateKind::Color, "bg", Some("stroke"))]).contains("no such attribute"));
        assert!(err(vec![control("a", TemplateKind::Text, "group", None)]).contains("<g>"));
        assert!(err(vec![control("a", TemplateKind::Image, "bg", Some("href"))]).contains("not an <image>"));
        assert!(err(vec![
            control("a", TemplateKind::Color, "bg", Some("fill")),
            control("b", TemplateKind::Color, "bg", Some("fill")),
        ])
        .contains("more than one control"));
        assert!(err(vec![
            control("a", TemplateKind::Text, "outer", None),
            control("b", TemplateKind::Text, "inner", None),
        ])
        .contains("holds"));
        let twice =
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><text id="t">a</text><text id="t">b</text></svg>"#;
        let e = validate_controls(&[control("a", TemplateKind::Text, "t", None)], twice).unwrap_err();
        assert!(e.to_string().contains("2 elements"), "{e}");
    }
}
