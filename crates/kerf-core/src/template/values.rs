//! Control values going into a title, and the title they make.
//!
//! [`apply_values`] is **pure**: the title's text in, the title's text out, each value checked for its
//! kind, escaped, and spliced in by byte range at the element its control points to. What comes out is
//! not trusted any more than what went in: [`Template::prepare`] and [`TemplateEditInputs::prepare`]
//! hand it to the sanitiser (`SvgDoc::parse`) like any custom SVG, then set its text as glyph outlines
//! (`SvgDoc::outlined`), so the title draws without its fonts; that — the up to a second and a half of
//! parsing, and the text engine — is why they are separate steps from the ones under the project lock.
//! The values are always spliced into the title's text form (`svg::restore_text`): the outlines are made
//! again from it after every change.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::archive::Template;
use super::model::{TemplateControl, TemplateInstance, TemplateKind};
use super::validate::{has_control_chars, invalid, validate_controls_shape, MAX_FAMILY_CHARS, MAX_TEXT_CHARS};
use crate::error::Result;
use crate::fonts::{self, EmbeddedFont};
use crate::svg::splice::{self, Elements, Part};
use crate::svg::{images, restore_text, SvgDoc};

fn part(target: &super::model::TemplateTarget) -> Part<'_> {
    match target.attribute.as_deref() {
        Some(name) => Part::Attribute(name),
        None => Part::Text,
    }
}

/// An SVG colour a `color` control may write: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, a colour name
/// (bare letters, which `svgtypes` must also know; not FFmpeg's table, `transparent` is no name there), `none` or
/// `currentColor`. Nothing with a `@`, a `0x` or a function: those are FFmpeg's spellings, and
/// `rgb(1,2,3)` is a value with commas in it.
pub(super) fn valid_svg_color(s: &str) -> bool {
    use std::str::FromStr;
    if let Some(hex) = s.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    if matches!(s, "none" | "currentColor") {
        return true;
    }
    !s.contains('@')
        && !s.starts_with("0x")
        && !s.starts_with("0X")
        && !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_alphabetic())
        && svgtypes::Color::from_str(s).is_ok()
}

/// `s` as a family name a `font` control may write: letters, digits, spaces and `-`, `_`, `.` — no quote,
/// comma or bracket to end the value early.
fn valid_family(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= MAX_FAMILY_CHARS
        && s == s.trim()
        && s.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// `value` as `control` takes it, in the spelling that is written: text as is, a colour and a family
/// trimmed, a number in its plain form, an image as the `data:` URL it is.
fn check_value(control: &TemplateControl, value: &str) -> Result<String> {
    let id = &control.id;
    // Before anything reads it (a data URL is decoded, every target gets a copy): no value of any kind
    // can be bigger than the title it goes into.
    if value.len() > crate::svg::MAX_BYTES {
        return Err(invalid(format!(
            "the value for “{id}” is {} KiB; a title is at most {} KiB",
            value.len().div_ceil(1024),
            crate::svg::MAX_BYTES / 1024
        )));
    }
    match control.kind {
        TemplateKind::Text => {
            // A line break is a line break however the platform it was typed on spells it.
            let text = if value.contains('\r') {
                value.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                value.to_string()
            };
            if text.chars().count() > MAX_TEXT_CHARS {
                return Err(invalid(format!(
                    "the text of “{id}” is longer than {MAX_TEXT_CHARS} characters"
                )));
            }
            if has_control_chars(&text) {
                return Err(invalid(format!("the text of “{id}” has a control character in it")));
            }
            Ok(text)
        }
        TemplateKind::Color => {
            let color = value.trim();
            if valid_svg_color(color) {
                Ok(color.to_string())
            } else {
                Err(invalid(format!(
                    "“{value}” is not a colour for “{id}” (use #rrggbb, #rrggbbaa or a colour name)"
                )))
            }
        }
        TemplateKind::Number => {
            let n: f64 = value
                .trim()
                .parse()
                .map_err(|_| invalid(format!("“{value}” is not a number for “{id}”")))?;
            if !n.is_finite() {
                return Err(invalid(format!("“{value}” is not a finite number for “{id}”")));
            }
            if control.min.is_some_and(|lo| n < lo) || control.max.is_some_and(|hi| n > hi) {
                return Err(invalid(format!(
                    "{n} is outside what “{id}” takes ({} to {})",
                    control.min.map_or("any".to_string(), |v| v.to_string()),
                    control.max.map_or("any".to_string(), |v| v.to_string())
                )));
            }
            // `-0` is `0`; Rust prints an `f64` in plain decimal, never in exponent form.
            Ok(if n == 0.0 { "0".to_string() } else { n.to_string() })
        }
        TemplateKind::Font => {
            let family = value.trim();
            if valid_family(family) {
                Ok(family.to_string())
            } else {
                Err(invalid(format!(
                    "“{value}” is not a font family for “{id}” (letters, digits, spaces, - _ . — up to {MAX_FAMILY_CHARS})"
                )))
            }
        }
        TemplateKind::Image => {
            if !value.starts_with("data:") {
                return Err(invalid(format!("the image for “{id}” must be a data: URL")));
            }
            images::check_href(value).map_err(|why| invalid(format!("the image for “{id}”: {why}")))?;
            Ok(value.to_string())
        }
    }
}

/// `svg` with each of `values` (control id to value) written where its control points. Only the named
/// controls change; a control not in `values` keeps what the title holds. An unknown control id, a value
/// the control's kind refuses, an element or attribute the title lacks, or two values that land in the
/// same place is an `InvalidArgument` and nothing is returned. The result is **not sanitised**.
pub fn apply_values(svg: &str, controls: &[TemplateControl], values: &BTreeMap<String, String>) -> Result<String> {
    if values.is_empty() {
        return Ok(svg.to_string());
    }
    let unknown: Vec<&str> = values
        .keys()
        .map(String::as_str)
        .filter(|id| !controls.iter().any(|c| c.id == *id))
        .collect();
    if !unknown.is_empty() {
        let known: Vec<&str> = controls.iter().map(|c| c.id.as_str()).collect();
        return Err(invalid(format!(
            "no control called {}; this template has {}",
            unknown.iter().map(|u| format!("“{u}”")).collect::<Vec<_>>().join(", "),
            if known.is_empty() {
                "none".to_string()
            } else {
                known.iter().map(|k| format!("“{k}”")).collect::<Vec<_>>().join(", ")
            }
        )));
    }
    let elements = Elements::parse(svg)?;
    let mut edits = Vec::new();
    for control in controls {
        let Some(value) = values.get(&control.id) else { continue };
        let value = check_value(control, value)?;
        for target in &control.targets {
            edits.push(elements.edit(&target.element, part(target), &value)?);
        }
    }
    splice::apply(svg, edits)
}

/// The value each control holds in `svg` right now: the text content, or the attribute, of its first
/// target. A control whose element is gone, and every `image` control (its value is a `data:` URL that
/// is in the title already), is left out.
pub fn read_values(svg: &str, controls: &[TemplateControl]) -> BTreeMap<String, String> {
    let Ok(elements) = Elements::parse(svg) else {
        return BTreeMap::new();
    };
    controls
        .iter()
        .filter(|c| c.kind != TemplateKind::Image)
        .filter_map(|c| {
            let target = c.targets.first()?;
            let value = elements.read(&target.element, part(target)).ok()??;
            Some((c.id.clone(), value))
        })
        .collect()
}

/// A warning for each `font` control set to a family nothing has (not installed, not embedded, not one of
/// `carried`): the title would be drawn in Geist instead. What the title holds is a `font-family` *list*
/// (`'Barlow Condensed', sans-serif`), read as CSS reads one: split on commas, quotes stripped, names
/// compared without regard to case, and the control is fine when any of them is something the title can
/// be drawn in (a generic family always is).
fn font_warnings(controls: &[TemplateControl], now: &BTreeMap<String, String>, carried: &[&str]) -> Vec<String> {
    controls
        .iter()
        .filter(|c| c.kind == TemplateKind::Font)
        .filter_map(|c| {
            let list = now.get(&c.id)?;
            let first = fonts::family_list(list).next()?;
            let known = fonts::family_list(list).any(|family| {
                carried.iter().any(|have| fonts::same_family(have, family))
                    || fonts::is_generic_family(family)
                    || fonts::has_family(family)
            });
            (!known).then(|| {
                format!(
                    "the font “{first}” is not installed, so “{}” is drawn in {}",
                    c.label,
                    fonts::DEFAULT_FAMILY
                )
            })
        })
        .collect()
}

/// A template applied to values and read: the title it makes, ready to be added to a timeline. Made by
/// [`Template::prepare`] **before the project lock is taken**.
#[derive(Debug, Clone)]
pub struct PreparedTemplate {
    pub template_id: String,
    pub name: String,
    /// The length the manifest gives the title, in seconds.
    pub duration: f64,
    /// The sanitised, read title (`doc.sanitized()` is what is stored).
    pub doc: SvgDoc,
    pub instance: TemplateInstance,
    /// The fonts the template carries, to be copied into the project.
    pub fonts: Vec<EmbeddedFont>,
    /// What the sanitiser could not honour, and fonts the machine lacks.
    pub warnings: Vec<String>,
}

impl Template {
    /// Apply `values` (partial: control id to value) to this template's title, read the result and set
    /// its text as outlines with the fonts the template carries. Pure and slow (the sanitiser, the text
    /// engine): not for under the project lock.
    pub fn prepare(&self, values: &BTreeMap<String, String>) -> Result<PreparedTemplate> {
        let m = &self.manifest;
        let spliced = apply_values(&restore_text(&self.svg)?, &m.controls, values)?;
        let doc = SvgDoc::parse(&spliced)?.outlined(&fonts::svg_fontdb_with(&self.fonts))?;
        let now = read_values(doc.sanitized(), &m.controls);
        let carried: Vec<&str> = m.fonts.iter().map(|f| f.family.as_str()).collect();
        let mut warnings = doc.warnings().to_vec();
        warnings.extend(font_warnings(&m.controls, &now, &carried));
        Ok(PreparedTemplate {
            template_id: m.id.clone(),
            name: m.name.clone(),
            duration: m.duration,
            instance: TemplateInstance {
                template_id: m.id.clone(),
                name: m.name.clone(),
                controls: m.controls.clone(),
                values: now,
            },
            doc,
            fonts: self.fonts.clone(),
            warnings,
        })
    }
}

/// What changing a template title's values needs from the project: the title as it is now and what it
/// remembers of its template. Taken under the lock (`Project::template_edit_inputs`, a cheap read),
/// worked on without it ([`TemplateEditInputs::prepare`]).
#[derive(Debug, Clone)]
pub struct TemplateEditInputs {
    pub overlay_id: Uuid,
    pub(crate) svg: String,
    pub(crate) instance: TemplateInstance,
}

impl TemplateEditInputs {
    pub fn new(overlay_id: Uuid, svg: String, instance: TemplateInstance) -> Self {
        Self {
            overlay_id,
            svg,
            instance,
        }
    }

    /// The controls of the title.
    pub fn controls(&self) -> &[TemplateControl] {
        &self.instance.controls
    }

    /// Apply `values` (partial) to the title as it is now, read the result and set its text as outlines
    /// with the fonts the project embeds that the new title names (a font control may name one the old
    /// one did not). Pure and slow: not for under the project lock. **No values is an error**, not an
    /// edit that changes nothing: it would record a revision that did nothing. The controls were stored
    /// with the title, so they are checked again before they are trusted to say where a value goes.
    pub fn prepare(&self, values: &BTreeMap<String, String>) -> Result<PreparedValues> {
        if values.is_empty() {
            return Err(invalid(
                "no values to set: name at least one of the title's controls and the value it should take",
            ));
        }
        validate_controls_shape(&self.instance.controls)?;
        let spliced = apply_values(&restore_text(&self.svg)?, &self.instance.controls, values)?;
        let doc = SvgDoc::parse(&spliced)?;
        let db = fonts::snapshot_for([doc.sanitized()]).db();
        let doc = doc.outlined(&db)?;
        let now = read_values(doc.sanitized(), &self.instance.controls);
        let mut warnings = doc.warnings().to_vec();
        warnings.extend(font_warnings(&self.instance.controls, &now, &[]));
        Ok(PreparedValues {
            base_svg: self.svg.clone(),
            doc,
            values: now,
            warnings,
        })
    }
}

/// New values for a template title, applied and read. `Project::set_template_values_parsed` stores it if
/// the title is still the one it was made from.
#[derive(Debug, Clone)]
pub struct PreparedValues {
    pub(crate) base_svg: String,
    pub(crate) doc: SvgDoc,
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) warnings: Vec<String>,
}

/// The most bytes of an image file that fit in a title: base64 grows it by a third, and the whole title is
/// held to [`crate::svg::MAX_BYTES`] with some room left for the artwork around it.
pub const MAX_IMAGE_BYTES: usize = crate::svg::MAX_BYTES * 3 / 4 - 2048;

/// An image file's bytes as the `data:` URL an `image` control takes: PNG, JPEG, GIF or WebP, by its
/// first bytes, within the embedded-image pixel cap, and small enough to sit in a title (the whole SVG is
/// held to 128 KiB). Pure; a surface reads the file first.
pub fn image_data_url(bytes: &[u8]) -> Result<String> {
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return Err(invalid("not a PNG, JPEG, GIF or WebP image"));
    };
    images::check(mime, bytes).map_err(|why| invalid(why.to_string()))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid(format!(
            "the image is {} KiB; one that sits in a title is at most {} KiB",
            bytes.len().div_ceil(1024),
            MAX_IMAGE_BYTES / 1024
        )));
    }
    Ok(format!("data:{mime};base64,{}", base64(bytes)))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svg::images::tests::{data_url, png_claiming, RED_4X4};
    use crate::template::model::TemplateTarget;

    pub(crate) fn control(id: &str, kind: TemplateKind, element: &str, attribute: Option<&str>) -> TemplateControl {
        TemplateControl {
            id: id.to_string(),
            label: format!("The {id}"),
            kind,
            targets: vec![TemplateTarget {
                element: element.to_string(),
                attribute: attribute.map(str::to_string),
            }],
            min: None,
            max: None,
        }
    }

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 100 100">
<defs><linearGradient id="g"><stop id="s1" offset="0" stop-color="#112233"/><stop id="s2" offset="1" stop-color="#112233"/></linearGradient></defs>
<rect id="bg" width="100" height="100" fill="url(#g)"/>
<circle id="dot" cx="10" cy="10" r="5" fill="#ff0000"/>
<text id="tag" x="1" y="50" font-family="Geist" font-size="12">Hello</text>
<image id="logo" xlink:href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAAEklEQVR4nGP4z8DwHxkzkC4AADxAH+HggXe0AAAAAElFTkSuQmCC" width="4" height="4"/>
</svg>"##;

    fn controls() -> Vec<TemplateControl> {
        let mut brand = control("brand", TemplateKind::Color, "s1", Some("stop-color"));
        brand.targets.push(TemplateTarget {
            element: "s2".into(),
            attribute: Some("stop-color".into()),
        });
        let mut radius = control("radius", TemplateKind::Number, "dot", Some("r"));
        radius.min = Some(1.0);
        radius.max = Some(40.0);
        vec![
            control("tagline", TemplateKind::Text, "tag", None),
            brand,
            radius,
            control("face", TemplateKind::Font, "tag", Some("font-family")),
            control("logo", TemplateKind::Image, "logo", Some("xlink:href")),
            control("dot", TemplateKind::Color, "dot", Some("fill")),
        ]
    }

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn apply(pairs: &[(&str, &str)]) -> Result<String> {
        apply_values(SVG, &controls(), &values(pairs))
    }

    #[test]
    fn no_values_leave_the_title_byte_for_byte() {
        assert_eq!(apply(&[]).unwrap(), SVG);
    }

    #[test]
    fn each_kind_writes_where_it_points_and_nothing_else_moves() {
        let out = apply(&[
            ("tagline", "Fly <high> & \"far\""),
            ("brand", " #abcdef "),
            ("radius", "12.50"),
            ("face", "Barlow Condensed"),
            ("dot", "cornflowerblue"),
        ])
        .unwrap();
        assert!(out.contains("Fly &lt;high&gt; &amp; \"far\"</text>"), "{out}");
        assert_eq!(out.matches("stop-color=\"#abcdef\"").count(), 2, "one value, both targets");
        assert!(out.contains(r#"r="12.5""#));
        assert!(out.contains(r#"font-family="Barlow Condensed""#));
        assert!(out.contains(r#"fill="cornflowerblue""#));
        assert!(
            out.contains(r#"font-size="12""#) && out.contains(r#"cx="10""#),
            "untouched attributes stay"
        );
        // Parses back, and the sanitiser keeps it.
        let doc = SvgDoc::parse(&out).unwrap();
        assert_eq!(read_values(doc.sanitized(), &controls())["tagline"], "Fly <high> & \"far\"");
    }

    #[test]
    fn an_image_replaces_the_href_and_the_sanitiser_keeps_a_good_one() {
        let other = data_url("image/png", &png_claiming(8, 8));
        let out = apply(&[("logo", other.as_str())]).unwrap();
        assert!(out.contains(&other));
        assert!(SvgDoc::parse(&out).unwrap().sanitized().contains(&other));
        // And the original is not mirrored in the values.
        assert!(!read_values(SVG, &controls()).contains_key("logo"));
        assert!(SVG.contains(RED_4X4));
    }

    #[test]
    fn a_bad_value_is_refused_by_its_kind_and_writes_nothing() {
        let huge = data_url("image/png", &png_claiming(60_000, 60_000));
        let svg_url = "data:image/svg+xml;base64,PHN2Zy8+";
        let cases: &[(&str, &str, &str)] = &[
            ("brand", "red;} x{", "not a colour"),
            ("brand", "url(#g)", "not a colour"),
            ("brand", "rgb(1,2,3)", "not a colour"),
            ("brand", "0xff0000", "not a colour"),
            ("brand", "red@0.5", "not a colour"),
            ("brand", "#12345", "not a colour"),
            ("brand", "notacolourname", "not a colour"),
            ("brand", "", "not a colour"),
            ("radius", "abc", "not a number"),
            ("radius", "NaN", "not a finite"),
            ("radius", "inf", "not a finite"),
            ("radius", "41", "outside"),
            ("radius", "0.5", "outside"),
            ("radius", "5px", "not a number"),
            ("face", "Foo\"><script/>", "not a font family"),
            ("face", "A, B", "not a font family"),
            ("face", "", "not a font family"),
            ("face", &"x".repeat(65), "not a font family"),
            ("logo", "https://example.com/a.png", "data: URL"),
            ("logo", "data:image/png;base64,AAAA", "image for"),
            ("logo", svg_url, "image for"),
            ("logo", &huge, "megapixels"),
            ("tagline", "a\u{7}b", "control character"),
            ("tagline", &"x".repeat(MAX_TEXT_CHARS + 1), "longer than"),
            ("nope", "x", "no control called"),
        ];
        for (id, value, why) in cases {
            let err = apply(&[(id, value)]).unwrap_err().to_string();
            assert!(err.contains(why), "{id}={value:?}: {err}");
        }
    }

    #[test]
    fn colours_accept_the_svg_spellings_only() {
        for good in [
            "#fff",
            "#ffff",
            "#ffffff",
            "#ffffff80",
            "red",
            "cornflowerblue",
            "none",
            "transparent",
            "currentColor",
        ] {
            assert!(valid_svg_color(good), "{good}");
        }
        for bad in [
            "",
            "#",
            "#ggg",
            "red;",
            "red blue",
            "a(b)",
            "0xff0000",
            "#ff0000@0.5",
            "javascript:1",
            "\"red\"",
        ] {
            assert!(!valid_svg_color(bad), "{bad}");
        }
    }

    #[test]
    fn a_missing_element_or_attribute_names_what_is_missing() {
        let mut cs = controls();
        cs[0].targets[0].element = "gone".into();
        let err = apply_values(SVG, &cs, &values(&[("tagline", "x")])).unwrap_err().to_string();
        assert!(err.contains("gone"), "{err}");
        let mut cs = controls();
        cs[2].targets[0].attribute = Some("rx".into());
        let err = apply_values(SVG, &cs, &values(&[("radius", "3")])).unwrap_err().to_string();
        assert!(err.contains("rx"), "{err}");
    }

    #[test]
    fn two_values_that_land_in_one_place_are_refused() {
        let mut cs = controls();
        cs.push(control("dot2", TemplateKind::Color, "dot", Some("fill")));
        let err = apply_values(SVG, &cs, &values(&[("dot", "red"), ("dot2", "blue")])).unwrap_err();
        assert!(err.to_string().contains("same part"), "{err}");
    }

    #[test]
    fn the_result_goes_through_the_sanitiser_which_has_the_last_word() {
        // A value that is legal for its kind but changes nothing the sanitiser would let through.
        let out = apply(&[("tagline", "<script>alert(1)</script>")]).unwrap();
        assert!(!out.contains("<script>"), "escaped as text: {out}");
        let doc = SvgDoc::parse(&out).unwrap();
        assert!(!doc.sanitized().contains("<script"));
    }

    #[test]
    fn values_read_back_are_what_the_title_holds() {
        let read = read_values(SVG, &controls());
        assert_eq!(read["tagline"], "Hello");
        assert_eq!(read["brand"], "#112233");
        assert_eq!(read["radius"], "5");
        assert_eq!(read["face"], "Geist");
        assert_eq!(read["dot"], "#ff0000");
        assert!(!read.contains_key("logo"));
    }

    #[test]
    fn a_data_url_is_made_from_image_bytes_and_refuses_what_is_not_one() {
        let png = {
            // The 4 x 4 red PNG of the image tests, decoded from its base64.
            let url = format!("data:image/png;base64,{RED_4X4}");
            images::decode_data_url(&url).unwrap().1
        };
        let url = image_data_url(&png).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        assert_eq!(images::decode_data_url(&url).unwrap().1, png);
        assert!(image_data_url(b"not an image").is_err());
        assert!(image_data_url(&png_claiming(60_000, 60_000))
            .unwrap_err()
            .to_string()
            .contains("megapixels"));
        let mut big = png;
        big.resize(200 * 1024, 0);
        assert!(image_data_url(&big).unwrap_err().to_string().contains("KiB"));
        for (len, expect) in [(0usize, ""), (1, "AA=="), (2, "AAA="), (3, "AAAA")] {
            assert_eq!(base64(&vec![0u8; len]), expect);
        }
    }

    const ANIMATED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
<text id="tag" x="1" y="50" font-size="12" opacity="0"><animate attributeName="opacity" from="0" to="1" dur="1s" fill="freeze"/>Hello</text>
<text id="rich" x="1" y="80" font-size="12">Big <tspan id="word" font-weight="bold" fill="#ff0000">news</tspan> today</text>
</svg>"##;

    fn animated_controls() -> Vec<TemplateControl> {
        vec![
            control("tagline", TemplateKind::Text, "tag", None),
            control("rich", TemplateKind::Text, "rich", None),
            control("word", TemplateKind::Text, "word", None),
        ]
    }

    #[test]
    fn setting_a_text_keeps_the_animation_and_the_styled_words_inside_it() {
        assert!(SvgDoc::parse(ANIMATED).unwrap().is_animated());
        let out = apply_values(ANIMATED, &animated_controls(), &values(&[("tagline", "Bye")])).unwrap();
        assert!(
            out.contains(r#"<animate attributeName="opacity" from="0" to="1" dur="1s" fill="freeze"/>Bye</text>"#),
            "{out}"
        );
        let doc = SvgDoc::parse(&out).unwrap();
        assert!(doc.is_animated(), "the title still fades in");
        assert!(doc.sanitized().contains("<animate"));
        assert_eq!(read_values(doc.sanitized(), &animated_controls())["tagline"], "Bye");

        // The text around a styled word is the control's; the word has a control of its own.
        let out = apply_values(
            ANIMATED,
            &animated_controls(),
            &values(&[("rich", "Old"), ("word", "stories")]),
        )
        .unwrap();
        assert!(
            out.contains(r##"Old<tspan id="word" font-weight="bold" fill="#ff0000">stories</tspan></text>"##),
            "{out}"
        );
        let read = read_values(SvgDoc::parse(&out).unwrap().sanitized(), &animated_controls());
        assert_eq!((read["rich"].as_str(), read["word"].as_str()), ("Old", "stories"));
        // Reading before any set shows the text a set writes over, not the words inside it.
        let before = read_values(ANIMATED, &animated_controls());
        assert_eq!((before["rich"].as_str(), before["tagline"].as_str()), ("Big  today", "Hello"));
    }

    #[test]
    fn a_line_break_is_one_whatever_it_was_typed_on() {
        let out = apply(&[("tagline", "one\r\ntwo\rthree\nfour")]).unwrap();
        assert!(out.contains(">one\ntwo\nthree\nfour</text>"), "{out:?}");
        assert!(!out.contains('\r'));
        let err = apply(&[("tagline", "a\u{0}b")]).unwrap_err().to_string();
        assert!(err.contains("control character"), "{err}");
    }

    #[test]
    fn a_value_bigger_than_a_title_is_refused_before_it_is_read() {
        let huge = format!("data:image/png;base64,{}", "A".repeat(crate::svg::MAX_BYTES));
        for (id, value) in [
            ("logo", huge.as_str()),
            ("tagline", &huge),
            ("brand", &huge),
            ("face", &huge),
            ("radius", &huge),
        ] {
            let err = apply(&[(id, value)]).unwrap_err().to_string();
            assert!(err.contains("a title is at most 128 KiB"), "{id}: {err}");
        }
    }

    #[test]
    fn a_font_list_is_read_as_css_reads_it() {
        let controls = vec![control("face", TemplateKind::Font, "tag", Some("font-family"))];
        let warn = |list: &str, carried: &[&str]| font_warnings(&controls, &values(&[("face", list)]), carried);
        assert!(warn("Geist", &[]).is_empty());
        assert!(warn("'Geist', sans-serif", &[]).is_empty(), "quotes and a fallback list");
        assert!(warn("\"GEIST\"", &[]).is_empty(), "case does not matter");
        assert!(
            warn("Nope One, sans-serif", &[]).is_empty(),
            "a generic family is always there"
        );
        assert!(warn("zeist , 'Nope'", &["Zeist"]).is_empty(), "carried, whatever the case");
        assert!(warn("", &[]).is_empty());
        let w = warn("'Nope One', \"Nope Two\"", &[]);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("“Nope One”") && w[0].contains("The face"), "{w:?}");
        assert!(warn("Zeist", &[]).len() == 1, "not carried, not installed");
    }

    fn inputs(controls: Vec<TemplateControl>, values: BTreeMap<String, String>) -> Result<PreparedValues> {
        let overlay = uuid::Uuid::new_v4();
        let instance = TemplateInstance {
            template_id: uuid::Uuid::new_v4().to_string(),
            name: "t".into(),
            controls,
            values: BTreeMap::new(),
        };
        TemplateEditInputs::new(overlay, SVG.to_string(), instance).prepare(&values)
    }

    #[test]
    fn setting_no_values_is_an_error_not_an_edit_that_changes_nothing() {
        let err = inputs(controls(), BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("no values to set"), "{err}");
        assert!(inputs(controls(), values(&[("tagline", "x")])).is_ok());
    }

    #[test]
    fn controls_stored_in_a_project_are_checked_before_they_are_trusted() {
        // A project file can hold anything: a control that writes an event handler, one whose id is not a key.
        let mut evil = control("tagline", TemplateKind::Number, "dot", Some("onclick"));
        evil.id = "tagline".into();
        let err = inputs(vec![evil], values(&[("tagline", "1")])).unwrap_err().to_string();
        assert!(err.contains("onclick"), "{err}");
        let mut odd = control("x", TemplateKind::Text, "tag", None);
        odd.id = "a b\"c".into();
        assert!(inputs(vec![odd], values(&[("a b\"c", "1")])).is_err());
        let mut twice = controls();
        twice.push(control("tagline", TemplateKind::Text, "tag", None));
        assert!(inputs(twice, values(&[("tagline", "1")]))
            .unwrap_err()
            .to_string()
            .contains("two controls"));
    }

    #[test]
    fn a_title_is_set_as_outlines_and_set_again_from_its_text_on_every_change() {
        let t = crate::template::fixtures::template();
        let applied = t.prepare(&values(&[("tagline", "Kite fun")])).unwrap();
        let svg = applied.doc.sanitized().to_string();
        assert!(svg.contains("data-kerf-text=\"\"") && !svg.contains("<text"), "{svg}");
        assert_eq!(applied.instance.values["tagline"], "Kite fun");
        let glyphs = |svg: &str| svg.matches("<path d=").count();
        assert_eq!(glyphs(&svg), 7);
        let again = TemplateEditInputs::new(uuid::Uuid::new_v4(), svg.clone(), applied.instance)
            .prepare(&values(&[("tagline", "Kite")]))
            .unwrap();
        assert_eq!(again.values["tagline"], "Kite");
        assert_eq!(glyphs(again.doc.sanitized()), 4);
        assert_eq!(again.base_svg, svg);
        let text = restore_text(again.doc.sanitized()).unwrap();
        assert!(text.contains(">Kite</text>") && !text.contains("data-kerf"), "{text}");
    }

    #[test]
    fn a_font_control_set_to_a_family_the_project_embeds_outlines_in_it() {
        use crate::fonts::shipped_font_renamed;
        let outlined = |svg: &str| {
            SvgDoc::parse(svg)
                .unwrap()
                .outlined(&fonts::svg_fontdb())
                .unwrap()
                .sanitized()
                .to_string()
        };
        let svg = outlined(SVG);
        assert!(svg.contains("data-kerf-text"), "{svg}");
        let set = apply_values(SVG, &controls(), &values(&[("face", "Zeist")])).unwrap();
        let in_geist = outlined(&set);
        assert!(fonts::register_embedded(&[EmbeddedFont::new(
            "Zeist",
            shipped_font_renamed(true, "Zeist")
        )]));
        let in_zeist = outlined(&set);
        assert_ne!(in_zeist, in_geist, "the embedded face is another one");
        let instance = TemplateInstance {
            template_id: "t".into(),
            name: "t".into(),
            controls: controls(),
            values: read_values(&svg, &controls()),
        };
        // Only the new value names the family: the title as it was does not.
        let got = TemplateEditInputs::new(uuid::Uuid::new_v4(), svg, instance)
            .prepare(&values(&[("face", "Zeist")]))
            .unwrap();
        fonts::replace_embedded(Vec::new());
        assert_eq!(got.doc.sanitized(), in_zeist);
    }
}
