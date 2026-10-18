//! Making a template out of a title on the timeline: the controls the author picked, the fonts the
//! title's text resolves to, and a poster.
//!
//! **Nothing here needs the project.** `Project::template_source` takes the title under the lock (a
//! cheap read); [`build_template`] reads fonts off the disk, parses the title and draws the poster with
//! the lock released.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::archive::Template;
use super::model::{ManifestFont, TemplateControl, TemplateManifest, FORMAT, VERSION};
use super::validate::invalid;
use super::{MAX_FONTS, MAX_FONT_BYTES, MAX_TOTAL_BYTES};
use crate::error::Result;
use crate::fonts::{self, EmbeddedFont, FaceLookup};
use crate::model::MAX_SVG_SECONDS;
use crate::svg::splice::Elements;

/// What a template is made of, taken from a title on the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateSource {
    /// The title's sanitised `svg`.
    pub svg: String,
    /// The title's name: the template's, unless the request names it.
    pub name: String,
    /// How long it runs (`SvgOverlay::doc_duration`), in seconds.
    pub duration: f64,
}

/// What the author says about the template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemplateSaveRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The controls: which elements of the title can be changed, and how.
    #[serde(default)]
    pub controls: Vec<TemplateControl>,
}

/// A template made, with what could not be carried over.
#[derive(Debug, Clone)]
pub struct BuiltTemplate {
    pub template: Template,
    pub warnings: Vec<String>,
}

/// Every family the title's text can be drawn in, and the weights and slants it asks for. Read off
/// `font-family` / `font-weight` / `font-style` as an attribute, in an element's `style`, in a `<style>`
/// rule and as the target of a SMIL `set` / `animate` — over the whole document, not per element:
/// the faces embedded are the families times the weights, which can only be too many, never too few.
#[derive(Debug, Default, PartialEq)]
struct FontUse {
    families: BTreeSet<String>,
    weights: BTreeSet<u16>,
    italic: bool,
}

impl FontUse {
    fn family_list(&mut self, list: &str) {
        for family in fonts::family_list(list) {
            if !fonts::is_generic_family(family) {
                self.families.insert(family.to_string());
            }
        }
    }

    fn weight(&mut self, w: &str) {
        let w = w.trim().to_ascii_lowercase();
        match w.as_str() {
            "bold" | "bolder" => {
                self.weights.insert(700);
            }
            "lighter" => {
                self.weights.insert(300);
            }
            "normal" => {}
            n => {
                if let Ok(n) = n.parse::<u16>() {
                    if (1..=1000).contains(&n) {
                        self.weights.insert(n);
                    }
                }
            }
        }
    }

    fn style(&mut self, s: &str) {
        let s = s.trim().to_ascii_lowercase();
        if s.starts_with("italic") || s.starts_with("oblique") {
            self.italic = true;
        }
    }

    /// One `property: value` pair (a CSS declaration).
    fn declaration(&mut self, property: &str, value: &str) {
        let value = value.split("!important").next().unwrap_or(value);
        match property.trim().to_ascii_lowercase().as_str() {
            "font-family" => self.family_list(value),
            "font-weight" => self.weight(value),
            "font-style" => self.style(value),
            _ => {}
        }
    }

    /// Declarations in `css`, whether a `style` attribute (`a:b;c:d`) or a whole sheet (`.x{a:b}`): split
    /// on `;`, `{` and `}`, then on the first `:`.
    fn css(&mut self, css: &str) {
        for part in css.split([';', '{', '}']) {
            if let Some((property, value)) = part.split_once(':') {
                self.declaration(property, value);
            }
        }
    }
}

fn font_use(svg: &str) -> Result<FontUse> {
    let doc = roxmltree::Document::parse_with_options(svg, crate::svg::xml_options())
        .map_err(|e| invalid(format!("not valid XML: {e}")))?;
    let mut used = FontUse::default();
    for node in doc.descendants().filter(roxmltree::Node::is_element) {
        for attr in node.attributes().filter(|a| a.namespace().is_none()) {
            match attr.name() {
                "font-family" => used.family_list(attr.value()),
                "font-weight" => used.weight(attr.value()),
                "font-style" => used.style(attr.value()),
                "style" => used.css(attr.value()),
                _ => {}
            }
        }
        match node.tag_name().name() {
            "style" => {
                let sheet: String = node.children().filter_map(|c| c.text()).collect();
                used.css(&sheet);
            }
            "set" | "animate" => {
                let name = node.attribute("attributeName").unwrap_or_default();
                if matches!(name, "font-family" | "font-weight" | "font-style") {
                    for key in ["from", "to", "by", "values"] {
                        for value in node.attribute(key).unwrap_or_default().split(';') {
                            used.declaration(name, value);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(used)
}

/// The font files `used` resolves to in the font database (system fonts, the open project's embedded
/// ones), each at most [`MAX_FONT_BYTES`], [`MAX_FONTS`] of them and keeping the template under
/// [`MAX_TOTAL_BYTES`] with room for the title. What could not be carried is a warning. Generic families
/// and the Geist every Kerf ships are never embedded.
fn collect_fonts(used: &FontUse, budget: u64, warnings: &mut Vec<String>) -> Vec<EmbeddedFont> {
    let mut found: Vec<EmbeddedFont> = Vec::new();
    let mut spent = 0u64;
    let mut weights: BTreeSet<u16> = used.weights.clone();
    weights.insert(400);
    for family in used.families.iter().filter(|f| f.as_str() != fonts::DEFAULT_FAMILY) {
        let mut carried = false;
        let mut said = false;
        for italic in [false, true].into_iter().filter(|i| !i || used.italic) {
            for &weight in &weights {
                match fonts::lookup_face(family, weight, italic, MAX_FONT_BYTES) {
                    FaceLookup::Found(font) => {
                        carried = true;
                        if found.iter().any(|f| f.sha256 == font.sha256) {
                            continue;
                        }
                        let size = font.data.len() as u64;
                        if found.len() >= MAX_FONTS || spent + size > budget {
                            if !said {
                                warnings.push(format!(
                                    "the template would hold more fonts than fit ({MAX_FONTS}, or {} MiB); some faces of “{family}” were left out",
                                    MAX_TOTAL_BYTES >> 21
                                ));
                                said = true;
                            }
                            continue;
                        }
                        spent += size;
                        found.push(font);
                    }
                    FaceLookup::Missing => {}
                    FaceLookup::NotSingleFile => {
                        if !said {
                            warnings.push(format!(
                                "the font “{family}” is in a collection or is not a plain TrueType / OpenType file, so it was not embedded"
                            ));
                            said = true;
                        }
                    }
                    FaceLookup::TooLarge(bytes) => {
                        if !said {
                            warnings.push(format!(
                                "the font “{family}” is {} MiB; fonts over {} MiB are not embedded",
                                bytes.div_ceil(1 << 20),
                                MAX_FONT_BYTES >> 20
                            ));
                            said = true;
                        }
                    }
                    FaceLookup::Restricted => {
                        if !said {
                            warnings.push(format!(
                                "the font “{family}” does not allow embedding (its licence is restricted), so it was not embedded"
                            ));
                            said = true;
                        }
                    }
                }
            }
        }
        if !carried && !said {
            warnings.push(format!(
                "the font “{family}” is not installed, so it was not embedded; the template is drawn in {} where it is missing",
                fonts::DEFAULT_FAMILY
            ));
        }
    }
    found
}

/// The file name a font is stored under: its family and the start of its digest, with the extension its
/// header says.
fn font_file_name(font: &EmbeddedFont) -> String {
    let family: String = font
        .family
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let ext = if font.data.starts_with(b"OTTO") { "otf" } else { "ttf" };
    format!("fonts/{}-{}.{ext}", family.trim_matches('-'), &font.sha256[..8])
}

/// Make a template from a title and what the author says about it. Checks the controls against the
/// title (every target element once, the attribute there), embeds the fonts the text resolves to, draws
/// the poster with them, and returns the template as it would be written. Slow (the sanitiser, the
/// poster, the font files): not for under the project lock. `id` is the new template's.
pub fn build_template(source: &TemplateSource, request: TemplateSaveRequest, id: Uuid) -> Result<BuiltTemplate> {
    if !source.duration.is_finite() || source.duration <= 0.0 || source.duration > MAX_SVG_SECONDS {
        return Err(invalid("the title has no length to make a template of"));
    }
    let (w, h) = crate::svg::intrinsic_size(&source.svg)
        .ok_or_else(|| invalid("the title has no size (a viewBox, or a width and height)"))?;
    let size = |v: f64| (v.round() as u32).clamp(1, 16_384);
    let name = if request.name.trim().is_empty() {
        source.name.clone()
    } else {
        request.name.trim().to_string()
    };
    let mut warnings = Vec::new();
    let used = font_use(&source.svg)?;
    let fonts = collect_fonts(&used, MAX_TOTAL_BYTES / 2, &mut warnings);
    let manifest = TemplateManifest {
        format: FORMAT.to_string(),
        version: VERSION,
        id: id.hyphenated().to_string(),
        name,
        description: request.description.trim().to_string(),
        duration: source.duration,
        width: size(w),
        height: size(h),
        controls: request.controls,
        fonts: fonts
            .iter()
            .map(|f| ManifestFont {
                file: font_file_name(f),
                family: f.family.clone(),
            })
            .collect(),
    };
    let mut template = Template {
        manifest,
        svg: source.svg.clone(),
        fonts,
        poster: None,
    };
    let checked = template.validate()?;
    warnings.extend(checked.warnings.iter().cloned());
    match template.render_poster(&checked.doc) {
        Ok(poster) => template.poster = Some(poster),
        Err(e) => warnings.push(format!("no poster could be drawn: {e}")),
    }
    // A title's controls with no target would be caught above; one with no controls at all is allowed (a
    // template can be just artwork), but worth a word.
    if template.manifest.controls.is_empty() {
        warnings.push("the template has no controls: it can be applied but not changed".to_string());
    }
    Ok(BuiltTemplate { template, warnings })
}

/// The `id`s of the title's elements, for a surface that offers to make controls out of them: every
/// element with an `id` and the tag it is. A document over the size a title may be (128 KiB), nested
/// deeper than one may be, or not XML is refused before anything is listed: the text may come straight
/// from a webview.
pub fn element_ids(svg: &str) -> Result<Vec<(String, String)>> {
    Ok(Elements::parse(svg)?.ids())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::model::{TemplateKind, TemplateTarget};

    fn used(svg: &str) -> FontUse {
        font_use(svg).unwrap()
    }

    fn doc(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100">{body}</svg>"#)
    }

    #[test]
    fn families_weights_and_slants_are_found_wherever_they_are_written() {
        let u = used(&doc(
            r#"<style>.a { font-family: "Barlow Condensed", 'Open Sans', sans-serif; font-weight: 700 } .b{font-style:italic}</style>
               <text font-family="Roboto Slab" font-weight="bold">a</text>
               <text style="font-family:Lora;font-weight:300 !important">b</text>
               <set attributeName="font-family" to="Caveat"/>
               <animate attributeName="font-weight" values="400;900"/>
               <text font-family="monospace, serif">c</text>"#,
        ));
        let families: Vec<&str> = u.families.iter().map(String::as_str).collect();
        assert_eq!(families, ["Barlow Condensed", "Caveat", "Lora", "Open Sans", "Roboto Slab"]);
        assert_eq!(u.weights.iter().copied().collect::<Vec<_>>(), [300, 400, 700, 900]);
        assert!(u.italic);
    }

    #[test]
    fn generic_families_and_nothing_at_all_give_no_fonts() {
        let u = used(&doc(r#"<text font-family="sans-serif">a</text><text>b</text>"#));
        assert!(u.families.is_empty() && u.weights.is_empty() && !u.italic);
    }

    #[test]
    fn a_family_nothing_has_is_a_warning_and_geist_is_never_embedded() {
        let mut warnings = Vec::new();
        let u = used(&doc(
            r#"<text font-family="Geist">a</text><text font-family="Surely Not Installed Anywhere">b</text>"#,
        ));
        let found = collect_fonts(&u, MAX_TOTAL_BYTES, &mut warnings);
        assert!(found.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("Surely Not Installed Anywhere") && warnings[0].contains("not installed"));
    }

    #[test]
    fn an_embedded_family_is_found_by_the_database_and_carried_once() {
        let font = EmbeddedFont::new("Zeist", fonts::shipped_font_renamed(true, "Zeist"));
        fonts::register_embedded(std::slice::from_ref(&font));
        let u = used(&doc(
            r#"<text font-family="Zeist">a</text><text font-family="Zeist" font-weight="700">b</text>"#,
        ));
        let mut warnings = Vec::new();
        let found = collect_fonts(&u, MAX_TOTAL_BYTES, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(found.len(), 1, "regular and bold are the one face this font has");
        assert_eq!(found[0].sha256, font.sha256);
        assert_eq!(font_file_name(&found[0]), format!("fonts/Zeist-{}.ttf", &font.sha256[..8]));
        // A budget too small to hold it leaves it out and says so.
        let mut warnings = Vec::new();
        assert!(collect_fonts(&u, 10, &mut warnings).is_empty());
        assert!(warnings[0].contains("more fonts than fit"), "{warnings:?}");
        fonts::replace_embedded(Vec::new());
    }

    const TITLE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90">
<rect id="bg" width="160" height="90" fill="#102030"/>
<text id="tag" x="10" y="50" font-size="20" fill="#ffffff" font-family="Zeist">Hello</text>
</svg>"##;

    fn request(controls: Vec<TemplateControl>) -> TemplateSaveRequest {
        TemplateSaveRequest {
            name: "  Card ".into(),
            description: " A card ".into(),
            controls,
        }
    }

    fn text_control() -> TemplateControl {
        TemplateControl {
            id: "tagline".into(),
            label: "Tagline".into(),
            kind: TemplateKind::Text,
            targets: vec![TemplateTarget {
                element: "tag".into(),
                attribute: None,
            }],
            min: None,
            max: None,
        }
    }

    #[test]
    fn a_template_is_built_with_its_size_fonts_and_a_poster() {
        let font = EmbeddedFont::new("Zeist", fonts::shipped_font_renamed(true, "Zeist"));
        fonts::register_embedded(std::slice::from_ref(&font));
        let source = TemplateSource {
            svg: TITLE.into(),
            name: "My title".into(),
            duration: 3.0,
        };
        let built = build_template(&source, request(vec![text_control()]), Uuid::new_v4()).unwrap();
        let t = &built.template;
        assert_eq!((t.manifest.width, t.manifest.height), (160, 90));
        assert_eq!(t.manifest.name, "Card");
        assert_eq!(t.manifest.description, "A card");
        assert_eq!(t.manifest.duration, 3.0);
        assert_eq!(t.fonts.len(), 1);
        assert_eq!(t.manifest.fonts[0].family, "Zeist");
        assert!(built.warnings.is_empty(), "{:?}", built.warnings);
        let poster = t.poster.as_ref().expect("a poster");
        let size = imagesize::blob_size(poster).unwrap();
        assert_eq!((size.width, size.height), (512, 288));
        // It reads back as the same template.
        let back = Template::read(&t.to_bytes().unwrap()).unwrap();
        assert_eq!(back.digest(), t.digest());
        fonts::replace_embedded(Vec::new());
    }

    #[test]
    fn the_poster_is_drawn_with_the_embedded_font_not_the_fallback() {
        let source = TemplateSource {
            svg: TITLE.into(),
            name: "t".into(),
            duration: 2.0,
        };
        // Without the font in the database the family falls back to Geist ...
        let plain = build_template(&source, request(vec![]), Uuid::new_v4()).unwrap();
        // ... with it, the bold face is drawn.
        fonts::register_embedded(&[EmbeddedFont::new("Zeist", fonts::shipped_font_renamed(true, "Zeist"))]);
        let bold = build_template(&source, request(vec![]), Uuid::new_v4()).unwrap();
        fonts::replace_embedded(Vec::new());
        assert!(plain.template.fonts.is_empty() && !bold.template.fonts.is_empty());
        assert_ne!(plain.template.poster, bold.template.poster);
    }

    #[test]
    fn a_bad_request_is_refused_with_the_reason() {
        let source = TemplateSource {
            svg: TITLE.into(),
            name: "t".into(),
            duration: 2.0,
        };
        let mut bad = text_control();
        bad.targets[0].element = "nope".into();
        assert!(build_template(&source, request(vec![bad]), Uuid::new_v4())
            .unwrap_err()
            .to_string()
            .contains("nope"));
        let mut none = source.clone();
        none.duration = 0.0;
        assert!(build_template(&none, request(vec![]), Uuid::new_v4()).is_err());
        let mut unsized_ = source.clone();
        unsized_.svg = r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#.into();
        assert!(build_template(&unsized_, request(vec![]), Uuid::new_v4()).is_err());
        let mut nameless = request(vec![]);
        nameless.name = " ".into();
        let built = build_template(&source, nameless, Uuid::new_v4()).unwrap();
        assert_eq!(built.template.manifest.name, "t", "the title's name stands in");
        assert!(built.warnings.iter().any(|w| w.contains("no controls")));
    }

    #[test]
    fn element_ids_lists_what_a_control_could_point_at() {
        let ids = element_ids(TITLE).unwrap();
        assert_eq!(
            ids,
            [
                ("bg".to_string(), "rect".to_string()),
                ("tag".to_string(), "text".to_string())
            ]
        );
    }

    #[test]
    fn element_ids_refuses_what_a_title_cannot_be_before_parsing_it() {
        let big = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><g id="a"/><!--{}--></svg>"#,
            "x".repeat(crate::svg::MAX_BYTES)
        );
        let e = element_ids(&big).unwrap_err().to_string();
        assert!(e.contains("larger than 128 KiB"), "{e}");
        let deep = format!("{}{}", "<g>".repeat(5000), "</g>".repeat(5000));
        assert!(element_ids(&format!("<svg xmlns=\"http://www.w3.org/2000/svg\">{deep}</svg>")).is_err());
        assert!(element_ids("not xml").is_err());
        assert!(
            element_ids(&format!("<!DOCTYPE svg [<!ENTITY a \"b\">]>{TITLE}")).is_err(),
            "no DTDs"
        );
    }
}
