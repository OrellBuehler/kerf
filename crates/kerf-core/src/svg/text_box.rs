//! A text's box: the width its lines wrap to (SVG 2 `inline-size`), or shrink to fit on one line
//! (`data-kerf-fit="shrink"`). Only a text set as glyph outlines honours it (`outline.rs`); drawn with its
//! font, a text runs on as if it had none.

use roxmltree::{Document, Node};
use serde::{Deserialize, Serialize};

use super::outline::FIT_MARK;
use super::sanitize::{
    apply, escape_attr, nesting_exceeds, plain_attribute, skip_quoted, start_tag_end, starts_quoted, value_range, SVG_NS,
};
use super::{invalid, xml_options, MAX_BYTES, MAX_DEPTH};
use crate::error::Result;

/// How a text fits its box.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextFit {
    /// Its lines wrap at the box's width (and at a newline with `white-space: pre-line`).
    #[default]
    Wrap,
    /// It stays on one line (one per newline) and is made smaller until the widest fits.
    Shrink,
}

/// A text's box. No width is no box: the text runs on and `fit` means nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextBox {
    /// The box's width in the title's own units (its `viewBox`), from the text's anchor (`x`, placed by
    /// `text-anchor`). `null` takes the box away.
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub fit: TextFit,
}

/// The widest box a text may have, in user units.
pub const MAX_BOX_WIDTH: f64 = 100_000.0;

/// `svg` (a title in its text form) with the `<text>` whose `id` is `element` given `text_box`: its
/// `inline-size` (an attribute; one in its `style` is taken out, as it would win) and its fit, or neither.
/// Nothing else of the document moves.
pub fn set_text_box(svg: &str, element: &str, text_box: &TextBox) -> Result<String> {
    if let Some(w) = text_box.width {
        if !(w.is_finite() && w > 0.0 && w <= MAX_BOX_WIDTH) {
            return Err(invalid(format!(
                "a text box is wider than nothing and at most {MAX_BOX_WIDTH} units"
            )));
        }
    }
    if svg.len() > MAX_BYTES {
        return Err(invalid(format!("the SVG is larger than {} KiB", MAX_BYTES / 1024)));
    }
    if nesting_exceeds(svg, MAX_DEPTH) {
        return Err(invalid(format!("the SVG is nested deeper than {MAX_DEPTH} elements")));
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
            "“{element}” is a <{}>; a box is given to a <text>",
            node.tag_name().name()
        )));
    }
    let mut cuts = Vec::new();
    for attr in node.attributes().filter(|a| a.namespace().is_none()) {
        let range = attr.range();
        match attr.name() {
            "inline-size" | FIT_MARK => {
                let start = svg[..range.start]
                    .rfind(|c: char| !c.is_ascii_whitespace())
                    .map_or(range.start, |i| i + 1);
                cuts.push((start..range.end, String::new()));
            }
            "style" => {
                let kept = without_declaration(attr.value(), "inline-size");
                if kept != attr.value() {
                    if let Some((vr, q)) = value_range(svg, range) {
                        cuts.push((vr, escape_attr(&kept, q)));
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(w) = text_box.width {
        let open_end = start_tag_end(svg, node.range().start).ok_or_else(|| invalid("the text cannot be read"))?;
        let at = if svg[..open_end].ends_with("/>") {
            open_end - 2
        } else {
            open_end - 1
        };
        let mut add = format!(" inline-size=\"{}\"", number(w));
        if text_box.fit == TextFit::Shrink {
            add.push_str(&format!(" {FIT_MARK}=\"shrink\""));
        }
        cuts.push((at..at, add));
    }
    Ok(apply(svg, cuts))
}

/// The box `svg`'s `<text>` named `element` has, as [`set_text_box`] writes it (an `inline-size` attribute,
/// else one in its `style`). `None` for no such text.
pub fn text_box_of(svg: &str, element: &str) -> Option<TextBox> {
    if svg.len() > MAX_BYTES || nesting_exceeds(svg, MAX_DEPTH) {
        return None;
    }
    let doc = Document::parse_with_options(svg, xml_options()).ok()?;
    let node: Node = doc
        .descendants()
        .find(|n| n.is_element() && n.has_tag_name((SVG_NS, "text")) && plain_attribute(n, "id") == Some(element))?;
    let from_style = || {
        plain_attribute(&node, "style")?
            .split(';')
            .filter_map(|d| d.split_once(':'))
            .find(|(p, _)| p.trim().eq_ignore_ascii_case("inline-size"))
            .map(|(_, v)| v.trim())
    };
    let width = plain_attribute(&node, "inline-size")
        .or_else(from_style)
        .and_then(|v| v.trim().trim_end_matches("px").trim().parse::<f64>().ok())
        .filter(|w| w.is_finite() && *w > 0.0);
    let fit = match plain_attribute(&node, FIT_MARK).map(str::trim) {
        Some("shrink") => TextFit::Shrink,
        _ => TextFit::Wrap,
    };
    Some(TextBox { width, fit })
}

/// A width to a thousandth, without trailing zeros.
fn number(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A declaration list with every declaration of `prop` left out; the rest as written.
fn without_declaration(body: &str, prop: &str) -> String {
    let b = body.as_bytes();
    let mut kept: Vec<&str> = Vec::new();
    let (mut start, mut i, mut paren) = (0, 0, 0usize);
    let keep = |d: &str| d.split_once(':').is_none_or(|(p, _)| !p.trim().eq_ignore_ascii_case(prop));
    while i < b.len() {
        if starts_quoted(b, i) {
            i = skip_quoted(b, i);
            continue;
        }
        match b[i] {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b';' if paren == 0 => {
                if keep(&body[start..i]) {
                    kept.push(&body[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = &body[start.min(body.len())..];
    if !tail.trim().is_empty() && keep(tail) {
        kept.push(tail);
    }
    kept.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(body: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 200">{body}</svg>"#)
    }

    fn boxed(width: f64, fit: TextFit) -> TextBox {
        TextBox { width: Some(width), fit }
    }

    #[test]
    fn a_box_is_written_on_the_text_and_replaces_the_one_it_had() {
        let src = doc(
            r#"<text id="t" x="5" y="50" inline-size="80" data-kerf-fit="shrink" style="fill:red; inline-size: 9px;opacity:.5">Hi</text>"#,
        );
        let out = set_text_box(&src, "t", &boxed(120.5, TextFit::Wrap)).unwrap();
        assert_eq!(
            out,
            doc(r#"<text id="t" x="5" y="50" style="fill:red;opacity:.5" inline-size="120.5">Hi</text>"#)
        );
        assert_eq!(text_box_of(&out, "t"), Some(boxed(120.5, TextFit::Wrap)));
        let shrunk = set_text_box(&out, "t", &boxed(300.0, TextFit::Shrink)).unwrap();
        assert!(shrunk.contains(r#"inline-size="300" data-kerf-fit="shrink">Hi"#), "{shrunk}");
        assert_eq!(text_box_of(&shrunk, "t"), Some(boxed(300.0, TextFit::Shrink)));
        let none = set_text_box(&shrunk, "t", &TextBox::default()).unwrap();
        assert_eq!(
            none,
            doc(r#"<text id="t" x="5" y="50" style="fill:red;opacity:.5">Hi</text>"#)
        );
        assert_eq!(text_box_of(&none, "t"), Some(TextBox::default()));
        let empty = set_text_box(&doc(r#"<text id="t" x="5" y="50"/>"#), "t", &boxed(10.0, TextFit::Wrap)).unwrap();
        assert_eq!(empty, doc(r#"<text id="t" x="5" y="50" inline-size="10"/>"#));
    }

    #[test]
    fn only_one_text_by_its_id_takes_a_box_of_a_sane_width() {
        let src = doc(r#"<rect id="r" width="1" height="1"/><text id="t">a</text><text id="d">b</text><text id="d">c</text>"#);
        let err = |el: &str, b: TextBox| set_text_box(&src, el, &b).unwrap_err().to_string();
        assert!(err("r", boxed(10.0, TextFit::Wrap)).contains("<rect>"));
        assert!(err("x", boxed(10.0, TextFit::Wrap)).contains("no element"));
        assert!(err("d", boxed(10.0, TextFit::Wrap)).contains("more than one"));
        for w in [0.0, -1.0, f64::NAN, f64::INFINITY, MAX_BOX_WIDTH * 2.0] {
            assert!(err("t", boxed(w, TextFit::Wrap)).contains("wider than nothing"), "{w}");
        }
    }

    #[test]
    fn a_declaration_with_a_quoted_semicolon_stays_whole() {
        assert_eq!(
            without_declaration(r#"font-family:"a;b"; inline-size:3px ;fill:url(#g)"#, "inline-size"),
            r#"font-family:"a;b";fill:url(#g)"#
        );
    }
}
