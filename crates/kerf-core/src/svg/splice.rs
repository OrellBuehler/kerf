//! Setting a value inside a title by element id, as a byte-range splice.
//!
//! This is what a template's controls ride on (`crate::template`): "put this text in the element
//! `tagline`", "set `stop-color` on `bg-stop`". Like the sanitiser it **splices the original text**
//! rather than re-serialising it, so formatting, ids, comments and every attribute that is not named
//! come out byte for byte. The caller sanitises the result like any other custom SVG: nothing here
//! decides what is safe to draw, only where a value goes and that it cannot break out of the place it
//! was put (text is escaped, attribute values are escaped for their own quote).

use std::collections::HashMap;
use std::ops::Range;

use roxmltree::{Attribute, Document, Node, NodeId, NodeType};

use super::sanitize::{escape_attr, nesting_exceeds, plain_attribute, start_tag_end, value_range, XLINK_NS};
use super::{invalid, xml_options, MAX_BYTES, MAX_DEPTH};
use crate::error::Result;

/// The part of an element a value goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part<'a> {
    /// Everything between the start and the end tag.
    Text,
    /// The value of the attribute with this name as the source spells it (`fill`, `xlink:href`).
    Attribute(&'a str),
}

/// One replacement of a byte range of the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edit {
    range: Range<usize>,
    with: String,
}

/// A title's elements by `id`, read once.
pub(crate) struct Elements<'a> {
    src: &'a str,
    doc: Document<'a>,
    by_id: HashMap<String, Vec<NodeId>>,
}

impl<'a> Elements<'a> {
    /// Read `src`. Not a sanitiser: it refuses only what would make reading it unsafe or pointless
    /// (over the size cap, nested past the cap, not XML, DTDs).
    pub(crate) fn parse(src: &'a str) -> Result<Self> {
        if src.len() > MAX_BYTES {
            return Err(invalid(format!("the SVG is larger than {} KiB", MAX_BYTES / 1024)));
        }
        if nesting_exceeds(src, MAX_DEPTH) {
            return Err(invalid(format!("the SVG is nested deeper than {MAX_DEPTH} elements")));
        }
        let doc = Document::parse_with_options(src, xml_options()).map_err(|e| invalid(format!("not valid XML: {e}")))?;
        let mut by_id: HashMap<String, Vec<NodeId>> = HashMap::new();
        for node in doc.descendants().filter(Node::is_element) {
            if let Some(id) = plain_attribute(&node, "id") {
                by_id.entry(id.to_string()).or_default().push(node.id());
            }
        }
        Ok(Self { src, doc, by_id })
    }

    /// How many elements have this `id`.
    pub(crate) fn count(&self, id: &str) -> usize {
        self.by_id.get(id).map_or(0, Vec::len)
    }

    /// The element with this `id`, when exactly one has it.
    fn one(&self, id: &str) -> Result<Node<'_, 'a>> {
        match self.by_id.get(id).map(Vec::as_slice) {
            Some([only]) => Ok(self.doc.get_node(*only).expect("ids come from this document")),
            Some([]) | None => Err(invalid(format!("the title has no element with the id “{id}”"))),
            Some(_) => Err(invalid(format!("more than one element has the id “{id}”"))),
        }
    }

    /// The tag name (no prefix) of the element with this `id`.
    pub(crate) fn tag(&self, id: &str) -> Result<&str> {
        Ok(self.one(id)?.tag_name().name())
    }

    /// Whether `inner` is inside `outer` (or the same element).
    pub(crate) fn contains(&self, outer: &str, inner: &str) -> Result<bool> {
        let (outer, inner) = (self.one(outer)?, self.one(inner)?);
        Ok(inner.ancestors().any(|a| a == outer))
    }

    /// Whether the element has `part` to set: text always, an attribute when it is written.
    pub(crate) fn has(&self, id: &str, part: Part<'_>) -> Result<bool> {
        let node = self.one(id)?;
        Ok(match part {
            Part::Text => true,
            Part::Attribute(name) => attribute(&node, name).is_some(),
        })
    }

    /// The current value of `part`: the text content (entities decoded) or the attribute's value.
    pub(crate) fn read(&self, id: &str, part: Part<'_>) -> Result<Option<String>> {
        let node = self.one(id)?;
        Ok(match part {
            Part::Text => Some(
                node.descendants()
                    .filter(|n| n.node_type() == NodeType::Text)
                    .filter_map(|n| n.text())
                    .collect(),
            ),
            Part::Attribute(name) => attribute(&node, name).map(|a| a.value().to_string()),
        })
    }

    /// The edit that puts `value` in `part` of the element with this `id`. A value is always data:
    /// text is escaped (`&`, `<`, `>`), an attribute value is escaped for the quote it is written in.
    pub(crate) fn edit(&self, id: &str, part: Part<'_>, value: &str) -> Result<Edit> {
        let node = self.one(id)?;
        match part {
            Part::Attribute(name) => {
                let attr = attribute(&node, name)
                    .ok_or_else(|| invalid(format!("the element “{id}” has no attribute “{name}” to set")))?;
                let (range, quote) = value_range(self.src, attr.range())
                    .ok_or_else(|| invalid(format!("the attribute “{name}” of “{id}” cannot be read")))?;
                Ok(Edit {
                    range,
                    with: escape_attr(value, quote),
                })
            }
            Part::Text => {
                let range = node.range();
                let open_end =
                    start_tag_end(self.src, range.start).ok_or_else(|| invalid(format!("the element “{id}” cannot be read")))?;
                let escaped = escape_text(value);
                if self.src[..open_end].ends_with("/>") {
                    // `<text id="a"/>` becomes `<text id="a">value</text>`.
                    let name_end = self.src[range.start + 1..open_end]
                        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
                        .map_or(open_end, |i| range.start + 1 + i);
                    let name = &self.src[range.start + 1..name_end];
                    Ok(Edit {
                        range: open_end - 2..open_end,
                        with: format!(">{escaped}</{name}>"),
                    })
                } else {
                    let close = self.src[..range.end]
                        .rfind("</")
                        .filter(|close| *close >= open_end)
                        .ok_or_else(|| invalid(format!("the element “{id}” has no end tag")))?;
                    Ok(Edit {
                        range: open_end..close,
                        with: escaped,
                    })
                }
            }
        }
    }
}

/// The attribute `name` as the source spells it: `href` has no namespace, `xlink:href` is the XLink one.
/// (`node.attribute("href")` would answer for both, and which one a reader means is what a link can hide
/// behind.) Other prefixes are not addressable.
fn attribute<'a, 'i>(node: &Node<'a, 'i>, name: &str) -> Option<Attribute<'a, 'i>> {
    let (namespace, local) = match name.split_once(':') {
        Some(("xlink", local)) => (Some(XLINK_NS), local),
        Some(_) => return None,
        None => (None, name),
    };
    node.attributes().find(|a| a.name() == local && a.namespace() == namespace)
}

fn escape_text(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `src` with `edits` made. Two edits that touch the same bytes (two controls on one attribute, or a
/// control on a text whose element another control sets inside) are an error: neither could say what
/// the result is.
pub(crate) fn apply(src: &str, mut edits: Vec<Edit>) -> Result<String> {
    edits.sort_by(|a, b| a.range.start.cmp(&b.range.start).then(a.range.end.cmp(&b.range.end)));
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for edit in &edits {
        if edit.range.start < at {
            return Err(invalid("two controls set the same part of the title"));
        }
        out.push_str(&src[at..edit.range.start]);
        out.push_str(&edit.with);
        at = edit.range.end;
    }
    out.push_str(&src[at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 100 100">
  <rect id="bg" fill='#112233' width="100" height="100"/>
  <text id="tag" x="1" y="2">Hello &amp; welcome</text>
  <text id="empty" x="1" y="2"></text>
  <text id="short" x="1" y="2"/>
  <image id="logo" xlink:href="data:image/png;base64,AAAA" width="1" height="1"/>
  <text id="nest"><tspan id="inner">a</tspan></text>
</svg>"##;

    fn set(id: &str, part: Part<'_>, value: &str) -> String {
        let elements = Elements::parse(DOC).unwrap();
        apply(DOC, vec![elements.edit(id, part, value).unwrap()]).unwrap()
    }

    #[test]
    fn an_attribute_value_is_replaced_in_place_and_only_it() {
        let out = set("bg", Part::Attribute("fill"), "#ff0000");
        assert_eq!(out, DOC.replace("#112233", "#ff0000"));
    }

    #[test]
    fn a_value_cannot_leave_the_quote_it_is_written_in() {
        let out = set("bg", Part::Attribute("fill"), "a'b\"c<d&e");
        assert!(out.contains("fill='a&apos;b\"c&lt;d&amp;e'"), "{out}");
        assert!(Document::parse(&out).is_ok());
    }

    #[test]
    fn text_is_replaced_whole_escaped_and_the_empty_and_self_closed_forms_work() {
        assert!(set("tag", Part::Text, "a < b & c > d").contains(">a &lt; b &amp; c &gt; d</text>"));
        assert!(set("empty", Part::Text, "x").contains(r#"<text id="empty" x="1" y="2">x</text>"#));
        assert!(set("short", Part::Text, "x").contains(r#"<text id="short" x="1" y="2">x</text>"#));
        for id in ["tag", "empty", "short", "nest"] {
            assert!(Document::parse(&set(id, Part::Text, "\u{e9}<\"'>")).is_ok(), "{id}");
        }
    }

    #[test]
    fn an_xlink_attribute_is_found_by_its_prefix_and_a_bare_name_is_not_it() {
        let elements = Elements::parse(DOC).unwrap();
        assert!(elements.has("logo", Part::Attribute("xlink:href")).unwrap());
        assert!(!elements.has("logo", Part::Attribute("href")).unwrap());
        assert!(!elements.has("logo", Part::Attribute("foo:href")).unwrap());
        assert_eq!(
            elements.read("logo", Part::Attribute("xlink:href")).unwrap().as_deref(),
            Some("data:image/png;base64,AAAA")
        );
    }

    #[test]
    fn text_is_read_decoded_and_whole() {
        let elements = Elements::parse(DOC).unwrap();
        assert_eq!(elements.read("tag", Part::Text).unwrap().as_deref(), Some("Hello & welcome"));
        assert_eq!(elements.read("nest", Part::Text).unwrap().as_deref(), Some("a"));
        assert_eq!(elements.read("empty", Part::Text).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn a_missing_or_repeated_id_and_a_missing_attribute_are_errors_naming_them() {
        let elements = Elements::parse(DOC).unwrap();
        assert!(elements
            .edit("nope", Part::Text, "x")
            .unwrap_err()
            .to_string()
            .contains("nope"));
        assert!(elements
            .edit("bg", Part::Attribute("stroke"), "x")
            .unwrap_err()
            .to_string()
            .contains("stroke"));
        let twice = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><g id="a"/><g id="a"/></svg>"#;
        let elements = Elements::parse(twice).unwrap();
        assert_eq!(elements.count("a"), 2);
        assert!(elements
            .edit("a", Part::Text, "x")
            .unwrap_err()
            .to_string()
            .contains("more than one"));
    }

    #[test]
    fn edits_that_touch_the_same_bytes_are_refused() {
        let elements = Elements::parse(DOC).unwrap();
        let a = elements.edit("bg", Part::Attribute("fill"), "red").unwrap();
        let b = elements.edit("bg", Part::Attribute("fill"), "blue").unwrap();
        assert!(apply(DOC, vec![a, b]).is_err());
        let outer = elements.edit("nest", Part::Text, "x").unwrap();
        let inner = elements.edit("inner", Part::Text, "y").unwrap();
        assert!(apply(DOC, vec![outer, inner]).is_err());
        assert!(elements.contains("nest", "inner").unwrap());
        assert!(!elements.contains("inner", "nest").unwrap());
    }

    #[test]
    fn edits_apply_in_any_order() {
        let elements = Elements::parse(DOC).unwrap();
        let a = elements.edit("bg", Part::Attribute("fill"), "red").unwrap();
        let b = elements.edit("tag", Part::Text, "hi").unwrap();
        let one = apply(DOC, vec![a.clone(), b.clone()]).unwrap();
        let two = apply(DOC, vec![b, a]).unwrap();
        assert_eq!(one, two);
        assert!(one.contains("fill='red'") && one.contains(">hi</text>"));
    }
}
