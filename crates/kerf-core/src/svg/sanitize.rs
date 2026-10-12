//! The allow-list an SVG passes through before it is stored (and so before anything draws it).
//!
//! The stored text *is* what is rendered, so this runs once, on the way in, and is an **idempotent
//! splice**: every change deletes or replaces a byte range of the original (nothing is
//! re-serialised, so formatting, ids and comments survive), and the second pass over its own output
//! finds nothing left to change. What goes: scripts and anything that embeds another document or
//! media (`foreignObject`, `iframe`, `audio`, `video`), every element outside the SVG vocabulary
//! resvg draws, `on*` attributes, attributes of foreign namespaces (editor metadata), links that
//! leave the document (`href` other than `#fragment`, or a `data:image/png|jpeg|gif|webp` on an
//! `<image>`; `url()` the same), `@import`, `@font-face` with a `url()`, `@keyframes` and every
//! `animation*` / `transition*` declaration (CSS animation is not rendered; SMIL is).

use std::ops::Range;

use roxmltree::{Document, Node, NodeType};

use super::{invalid, MAX_BYTES, MAX_DEPTH, MAX_ELEMENTS};
use crate::error::Result;

pub(super) const SVG_NS: &str = "http://www.w3.org/2000/svg";
pub(super) const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// SVG elements that are kept; anything else (and everything outside the SVG namespace) is cut.
const ALLOWED: &[&str] = &[
    "svg",
    "g",
    "defs",
    "symbol",
    "use",
    "switch",
    "desc",
    "title",
    "style",
    "a",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "text",
    "tspan",
    "textPath",
    "image",
    "linearGradient",
    "radialGradient",
    "stop",
    "pattern",
    "clipPath",
    "mask",
    "marker",
    "filter",
    "feBlend",
    "feColorMatrix",
    "feComponentTransfer",
    "feComposite",
    "feConvolveMatrix",
    "feDiffuseLighting",
    "feDisplacementMap",
    "feDistantLight",
    "feDropShadow",
    "feFlood",
    "feFuncA",
    "feFuncB",
    "feFuncG",
    "feFuncR",
    "feGaussianBlur",
    "feImage",
    "feMerge",
    "feMergeNode",
    "feMorphology",
    "feOffset",
    "fePointLight",
    "feSpecularLighting",
    "feSpotLight",
    "feTile",
    "feTurbulence",
    "animate",
    "animateColor",
    "animateMotion",
    "animateTransform",
    "set",
    "mpath",
];

/// Elements worth saying were removed (the rest of the cut-outs are editor furniture).
const NOTABLE: &[&str] = &[
    "script",
    "foreignObject",
    "iframe",
    "audio",
    "video",
    "embed",
    "object",
    "canvas",
];

#[derive(Debug)]
pub(super) struct Sanitized {
    pub text: String,
    pub warnings: Vec<String>,
}

struct Edits {
    cuts: Vec<(Range<usize>, String)>,
    warnings: Vec<String>,
}

impl Edits {
    fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }
}

pub(super) fn sanitize(src: &str) -> Result<Sanitized> {
    if src.len() > MAX_BYTES {
        return Err(invalid(format!("the SVG is larger than {} KiB", MAX_BYTES / 1024)));
    }
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let src = strip_doctype(src)?;
    let doc = Document::parse_with_options(&src, super::xml_options()).map_err(|e| invalid(format!("not valid XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NS) {
        return Err(invalid(format!("the root element must be <svg xmlns=\"{SVG_NS}\">")));
    }
    let mut edits = Edits {
        cuts: Vec::new(),
        warnings: Vec::new(),
    };
    let mut elements = 0usize;
    let mut skip_to = 0usize;
    for node in doc.descendants() {
        if node.node_type() != NodeType::Element {
            continue;
        }
        let range = node.range();
        if range.start < skip_to {
            continue;
        }
        elements += 1;
        if elements > MAX_ELEMENTS {
            return Err(invalid(format!("the SVG has more than {MAX_ELEMENTS} elements")));
        }
        if node.ancestors().take(MAX_DEPTH + 2).count() > MAX_DEPTH + 1 {
            return Err(invalid(format!("the SVG is nested deeper than {MAX_DEPTH} elements")));
        }
        if let Some(why) = cut_element(&node) {
            skip_to = range.end;
            edits.cuts.push((range, String::new()));
            if let Some(why) = why {
                edits.warn(why);
            }
            continue;
        }
        attributes(&node, &src, &mut edits);
        if node.tag_name().name() == "style" {
            style_element(&node, &src, &mut edits);
        }
    }
    Ok(Sanitized {
        text: apply(&src, edits.cuts),
        warnings: edits.warnings,
    })
}

/// Splice `cuts` (any order, disjoint) into `src`.
pub(super) fn apply(src: &str, mut cuts: Vec<(Range<usize>, String)>) -> String {
    cuts.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.end.cmp(&b.0.end)));
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for (r, with) in cuts {
        if r.start < at {
            continue;
        }
        out.push_str(&src[at..r.start]);
        out.push_str(&with);
        at = r.end;
    }
    out.push_str(&src[at..]);
    out
}

/// A DOCTYPE (Illustrator and Inkscape write one) is cut; one with an internal subset can declare
/// entities, which is what an entity bomb needs, and is refused.
fn strip_doctype(src: &str) -> Result<String> {
    let Some(start) = src.find("<!DOCTYPE") else {
        return Ok(src.to_string());
    };
    if src[..start].contains("<svg") {
        return Ok(src.to_string());
    }
    let rest = &src[start..];
    let mut quote = None;
    for (i, c) in rest.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[') => return Err(invalid("a DOCTYPE with an internal subset is not supported")),
            (None, '>') => return Ok(format!("{}{}", &src[..start], &rest[i + 1..])),
            _ => {}
        }
    }
    Err(invalid("the DOCTYPE is not closed"))
}

/// `Some(warning)` when the element is cut (the warning may be empty for editor furniture).
fn cut_element(node: &Node) -> Option<Option<String>> {
    let name = node.tag_name().name();
    let in_svg = node.tag_name().namespace() == Some(SVG_NS);
    if in_svg && NOTABLE.contains(&name) {
        return Some(Some(format!("<{name}> is not allowed and was removed")));
    }
    if !in_svg || !ALLOWED.contains(&name) {
        let furniture = !in_svg || name == "metadata";
        return Some((!furniture).then(|| format!("unsupported element <{name}> was removed")));
    }
    if name == "style" {
        if let Some(t) = node.attribute("type") {
            if !t.trim().is_empty() && !t.trim().eq_ignore_ascii_case("text/css") {
                return Some(Some(format!("<style type=\"{t}\"> was removed (only CSS is read)")));
            }
        }
    }
    if matches!(
        name,
        "animate" | "set" | "animateColor" | "animateTransform" | "animateMotion"
    ) {
        if let Some(attr) = node.attribute("attributeName") {
            let local = attr.rsplit(':').next().unwrap_or(attr).trim();
            let lower = local.to_ascii_lowercase();
            if lower.starts_with("on") || lower == "href" || lower == "style" {
                return Some(Some(format!(
                    "animating \"{attr}\" is not allowed and the animation was removed"
                )));
            }
        }
    }
    None
}

/// The byte range of an attribute's value (inside its quotes) and the quote character.
pub(super) fn value_range(src: &str, attr_range: Range<usize>) -> Option<(Range<usize>, char)> {
    let raw = &src[attr_range.clone()];
    let eq = raw.find('=')?;
    let after = &raw[eq + 1..];
    let q_off = after.find(['"', '\''])?;
    let quote = after[q_off..].chars().next()?;
    let start = attr_range.start + eq + 1 + q_off + 1;
    let end = attr_range.end.checked_sub(1)?;
    (end >= start && src[end..].starts_with(quote)).then_some((start..end, quote))
}

fn attributes(node: &Node, src: &str, edits: &mut Edits) {
    let tag = node.tag_name().name();
    for attr in node.attributes() {
        let ns = attr.namespace();
        let name = attr.name();
        let range = attr.range();
        let cut = |edits: &mut Edits, msg: Option<String>| {
            let start = src[..range.start]
                .rfind(|c: char| !c.is_ascii_whitespace())
                .map_or(0, |i| i + src[i..].chars().next().map_or(1, char::len_utf8));
            edits.cuts.push((start..range.end, String::new()));
            if let Some(m) = msg {
                edits.warn(m);
            }
        };
        if !matches!(ns, None | Some(XLINK_NS | XML_NS)) {
            cut(edits, None);
            continue;
        }
        if ns.is_none() && name.len() > 2 && name[..2].eq_ignore_ascii_case("on") {
            cut(edits, Some(format!("event attribute \"{name}\" was removed")));
            continue;
        }
        let value = attr.value();
        if name == "href" && ns != Some(XML_NS) {
            if !href_allowed(tag, value) {
                cut(
                    edits,
                    Some("links and references outside the document were removed".to_string()),
                );
            }
            continue;
        }
        if name == "style" && ns.is_none() {
            let (clean, notes) = declarations(value);
            for n in notes {
                edits.warn(n);
            }
            if clean != value {
                if let Some((vr, q)) = value_range(src, range) {
                    edits.cuts.push((vr, escape_attr(&clean, q)));
                }
            }
            continue;
        }
        if has_external_url(value) {
            cut(
                edits,
                Some("links and references outside the document were removed".to_string()),
            );
        }
    }
}

fn href_allowed(tag: &str, value: &str) -> bool {
    let v = value.trim();
    if v.starts_with('#') {
        return true;
    }
    if !matches!(tag, "image" | "feImage") {
        return false;
    }
    let lower = v.get(..16.min(v.len())).unwrap_or(v).to_ascii_lowercase();
    ["data:image/png", "data:image/jpeg", "data:image/gif", "data:image/webp"]
        .iter()
        .any(|p| lower.starts_with(p))
        && v.contains(',')
}

/// A `url()` that is not `#fragment`.
fn has_external_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("url(") {
        let at = from + i + 4;
        let arg_end = lower[at..].find(')').map_or(lower.len(), |e| at + e);
        let arg = value[at..arg_end].trim().trim_matches(['"', '\'']).trim();
        if !arg.starts_with('#') {
            return true;
        }
        from = at;
    }
    false
}

pub(super) fn escape_attr(value: &str, quote: char) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' if quote == '"' => out.push_str("&quot;"),
            '\'' if quote == '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn escape_text(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;")
}

fn style_element(node: &Node, src: &str, edits: &mut Edits) {
    let range = node.range();
    let Some(open_end) = start_tag_end(src, range.start) else {
        return;
    };
    let Some(close) = src[..range.end].rfind("</") else {
        return;
    };
    if close < open_end || src[..open_end].ends_with("/>") {
        return;
    }
    let css: String = node
        .children()
        .filter(|c| matches!(c.node_type(), NodeType::Text))
        .filter_map(|c| c.text())
        .collect();
    let (clean, notes) = sheet(&css, 0);
    for n in notes {
        edits.warn(n);
    }
    if clean != css {
        edits.cuts.push((open_end..close, escape_text(&clean)));
    }
}

/// Where a start tag beginning at `from` ends (one past its `>`), skipping quoted values.
pub(super) fn start_tag_end(src: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    for (i, c) in src[from..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(from + i + 1),
            _ => {}
        }
    }
    None
}

// ---- CSS ------------------------------------------------------------------

const CSS_NOTE: &str = "CSS animations are not rendered — use SMIL";

fn unprefixed(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    match lower.strip_prefix('-').and_then(|r| r.split_once('-')) {
        Some((_, rest)) => rest.to_string(),
        None => lower,
    }
}

/// Skip a CSS string or comment starting at `i`; returns the index after it.
fn skip_quoted(b: &[u8], i: usize) -> usize {
    match b[i] {
        q @ (b'"' | b'\'') => {
            let mut j = i + 1;
            while j < b.len() {
                match b[j] {
                    b'\\' => j += 2,
                    c if c == q => return j + 1,
                    _ => j += 1,
                }
            }
            b.len()
        }
        _ => {
            // a comment
            let mut j = i + 2;
            while j + 1 < b.len() {
                if b[j] == b'*' && b[j + 1] == b'/' {
                    return j + 2;
                }
                j += 1;
            }
            b.len()
        }
    }
}

fn starts_quoted(b: &[u8], i: usize) -> bool {
    matches!(b[i], b'"' | b'\'') || (b[i] == b'/' && b.get(i + 1) == Some(&b'*'))
}

/// A style sheet with the parts SMIL replaces, and the external fetches, left out.
fn sheet(css: &str, depth: usize) -> (String, Vec<String>) {
    let b = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut notes = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let item_start = i;
        // find the end of this top-level item
        let mut j = i;
        let mut paren = 0usize;
        let mut block = false;
        let mut has_content = false;
        while j < b.len() {
            if starts_quoted(b, j) {
                if b[j] != b'/' {
                    has_content = true;
                }
                j = skip_quoted(b, j);
                continue;
            }
            match b[j] {
                b'(' => paren += 1,
                b')' => paren = paren.saturating_sub(1),
                b';' if paren == 0 => {
                    j += 1;
                    break;
                }
                b'{' if paren == 0 => {
                    block = true;
                    break;
                }
                c if !c.is_ascii_whitespace() => has_content = true,
                _ => {}
            }
            j += 1;
        }
        let head_end = j.min(b.len());
        if !block {
            let item = &css[item_start..head_end];
            if has_content && at_name(item.trim_start()).is_some_and(|n| unprefixed(n) == "import") {
                notes.push("@import was removed".to_string());
            } else {
                out.push_str(item);
            }
            i = head_end;
            continue;
        }
        // a block: find its matching brace
        let mut k = head_end + 1;
        let mut level = 1usize;
        while k < b.len() && level > 0 {
            if starts_quoted(b, k) {
                k = skip_quoted(b, k);
                continue;
            }
            match b[k] {
                b'{' => level += 1,
                b'}' => level -= 1,
                _ => {}
            }
            k += 1;
        }
        let body_end = if level == 0 { k - 1 } else { k };
        let prelude = &css[item_start..head_end];
        let body = &css[head_end + 1..body_end];
        let closed = level == 0;
        match at_name(prelude.trim_start()).map(unprefixed) {
            Some(n) if n == "keyframes" => notes.push(CSS_NOTE.to_string()),
            Some(n) if n == "font-face" => {
                if has_url(body) {
                    notes.push("@font-face with a url() was removed".to_string());
                } else {
                    out.push_str(&css[item_start..if closed { k } else { body_end }]);
                }
            }
            Some(n) if matches!(n.as_str(), "media" | "supports" | "layer" | "container" | "document") && depth < 8 => {
                let (inner, sub) = sheet(body, depth + 1);
                notes.extend(sub);
                out.push_str(prelude);
                out.push('{');
                out.push_str(&inner);
                if closed {
                    out.push('}');
                }
            }
            Some(_) => out.push_str(&css[item_start..if closed { k } else { body_end }]),
            None => {
                let (decls, sub) = declarations(body);
                notes.extend(sub);
                out.push_str(prelude);
                out.push('{');
                out.push_str(&decls);
                if closed {
                    out.push('}');
                }
            }
        }
        i = k.max(body_end);
    }
    notes.dedup();
    (out, notes)
}

fn at_name(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('@')?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn has_url(s: &str) -> bool {
    s.to_ascii_lowercase().contains("url(")
}

/// Whether a declaration (`prop: value`) stays, noting why when it does not.
fn keeps(declaration: &str, notes: &mut Vec<String>) -> bool {
    let prop = declaration.split_once(':').map_or("", |(p, _)| p);
    let prop = unprefixed(prop);
    if prop.starts_with("animation") || prop.starts_with("transition") {
        notes.push(CSS_NOTE.to_string());
        false
    } else if has_external_url(declaration) {
        notes.push("links and references outside the document were removed".to_string());
        false
    } else {
        true
    }
}

/// A declaration list with `animation*` / `transition*` and external `url()` left out. Each kept
/// declaration is its original text, so a second pass keeps exactly the same ones.
fn declarations(body: &str) -> (String, Vec<String>) {
    let b = body.as_bytes();
    let mut kept: Vec<&str> = Vec::new();
    let mut notes = Vec::new();
    let (mut start, mut i, mut paren, mut level) = (0, 0, 0usize, 0usize);
    while i < b.len() {
        if starts_quoted(b, i) {
            i = skip_quoted(b, i);
            continue;
        }
        match b[i] {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'{' => level += 1,
            b'}' => level = level.saturating_sub(1),
            b';' if paren == 0 && level == 0 => {
                if keeps(&body[start..i], &mut notes) {
                    kept.push(&body[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = &body[start.min(body.len())..];
    if tail.trim().is_empty() || keeps(tail, &mut notes) {
        kept.push(tail);
    }
    notes.dedup();
    (kept.join(";"), notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(src: &str) -> Sanitized {
        sanitize(src).unwrap()
    }

    fn svg(inner: &str) -> String {
        format!("<svg xmlns=\"{SVG_NS}\" xmlns:xlink=\"{XLINK_NS}\" viewBox=\"0 0 10 10\">{inner}</svg>")
    }

    #[test]
    fn a_clean_document_comes_back_byte_for_byte() {
        let src = format!(
            "<?xml version=\"1.0\"?>\n<!-- a comment -->\n<svg xmlns=\"{SVG_NS}\" xmlns:xlink=\"{XLINK_NS}\" viewBox='0 0 10 10'>\n  <defs><linearGradient id=\"g\"><stop offset=\"0\" stop-color=\"red\"/></linearGradient></defs>\n  <rect  width=\"5\"\n height=\"5\" fill=\"url(#g)\" style=\"opacity:.5\"/>\n  <use xlink:href=\"#g\"/>\n</svg>\n"
        );
        let out = clean(&src);
        assert_eq!(out.text, src);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn scripts_and_embedded_documents_go() {
        let out = clean(&svg(
            "<script>alert(1)</script><g><foreignObject><div/></foreignObject><iframe/><audio/><video/></g><rect width=\"1\" height=\"1\"/>",
        ));
        assert_eq!(out.text, svg("<g></g><rect width=\"1\" height=\"1\"/>"));
        for tag in ["script", "foreignObject", "iframe", "audio", "video"] {
            assert!(out.warnings.iter().any(|w| w.contains(tag)), "{tag}: {:?}", out.warnings);
        }
    }

    #[test]
    fn event_attributes_and_foreign_ones_go() {
        let src = format!(
            "<svg xmlns=\"{SVG_NS}\" xmlns:inkscape=\"http://www.inkscape.org/namespaces/inkscape\" viewBox=\"0 0 1 1\"><rect onclick=\"x()\" onload='y()' inkscape:label=\"L\" width=\"1\"/></svg>"
        );
        let out = clean(&src);
        assert!(!out.text.contains("onclick") && !out.text.contains("onload") && !out.text.contains("inkscape:label"));
        assert!(out.text.contains("<rect width=\"1\"/>"), "{}", out.text);
    }

    #[test]
    fn foreign_elements_and_metadata_go_quietly() {
        let src = format!(
            "<svg xmlns=\"{SVG_NS}\" xmlns:d=\"http://example.com/d\" viewBox=\"0 0 1 1\"><metadata><d:x/></metadata><d:thing/><rect width=\"1\"/><blink/></svg>"
        );
        let out = clean(&src);
        assert_eq!(
            out.text,
            format!("<svg xmlns=\"{SVG_NS}\" xmlns:d=\"http://example.com/d\" viewBox=\"0 0 1 1\"><rect width=\"1\"/></svg>")
        );
        assert_eq!(out.warnings, vec!["unsupported element <blink> was removed".to_string()]);
    }

    #[test]
    fn only_fragment_links_and_embedded_images_survive() {
        let png = "data:image/png;base64,iVBORw0KGgo=";
        let out = clean(&svg(&format!(
            "<use href=\"#a\"/><use href=\"http://evil/x.svg#a\"/><image href=\"{png}\"/><image href=\"file:///etc/passwd\"/><image href=\"data:image/svg+xml,&lt;svg/&gt;\"/><a href=\"https://x.test\"><text>t</text></a><image href=\"relative.png\"/>"
        )));
        assert_eq!(
            out.text,
            svg(&format!(
                "<use href=\"#a\"/><use/><image href=\"{png}\"/><image/><image/><a><text>t</text></a><image/>"
            ))
        );
        assert!(out.warnings.iter().any(|w| w.contains("outside the document")));
    }

    #[test]
    fn external_url_references_are_dropped_fragments_kept() {
        let out = clean(&svg(
            "<rect fill=\"url(http://x/y)\" stroke=\"url('#g')\" filter=\"url( &quot;#f&quot; )\" width=\"1\"/><rect style=\"fill:url(file:///x);stroke:red\"/>",
        ));
        assert_eq!(
            out.text,
            svg("<rect stroke=\"url('#g')\" filter=\"url( &quot;#f&quot; )\" width=\"1\"/><rect style=\"stroke:red\"/>")
        );
    }

    #[test]
    fn animating_a_link_or_an_event_is_cut() {
        let out = clean(&svg(
            "<image><animate attributeName=\"xlink:href\" to=\"http://x\"/><set attributeName=\"onclick\" to=\"x\"/><set attributeName=\"style\" to=\"x\"/><animate attributeName=\"opacity\" to=\"0\"/></image>",
        ));
        assert_eq!(out.text, svg("<image><animate attributeName=\"opacity\" to=\"0\"/></image>"));
    }

    #[test]
    fn style_sheets_lose_animation_imports_and_remote_fonts() {
        let css = "@import url(http://x/a.css);\n@font-face { font-family: F; src: url(http://x/f.woff); }\n@keyframes k { from { opacity: 0 } to { opacity: 1 } }\n.a { fill: red; animation: k 1s; -webkit-animation-name: k; transition: all 1s; stroke: blue; background: url(http://x/i.png) }\n@media screen { .b { animation-delay: 1s; fill: green } }\n.c { fill: url(#g) }\n";
        let out = clean(&svg(&format!("<style>{css}</style><rect class=\"a\"/>")));
        assert_eq!(
            out.text,
            svg("<style>\n.a { fill: red; stroke: blue}\n@media screen { .b { fill: green } }\n.c { fill: url(#g) }\n</style><rect class=\"a\"/>")
        );
        assert!(out.warnings.contains(&CSS_NOTE.to_string()), "{:?}", out.warnings);
        assert!(out.warnings.iter().any(|w| w.contains("@import")));
        assert!(out.warnings.iter().any(|w| w.contains("@font-face")));
    }

    #[test]
    fn a_cdata_style_is_sanitised_and_escaped() {
        let out = clean(&svg("<style><![CDATA[ .a > .b { fill: red; animation: x 1s } ]]></style>"));
        assert_eq!(out.text, svg("<style> .a > .b { fill: red} </style>"));
        let kept = svg("<style><![CDATA[ .a > .b { fill: red } ]]></style>");
        assert_eq!(clean(&kept).text, kept, "an untouched sheet keeps its CDATA");
    }

    #[test]
    fn style_attributes_are_filtered() {
        let out = clean(&svg("<rect style=\"fill:red; animation: a 1s;opacity:.5\"/>"));
        assert_eq!(out.text, svg("<rect style=\"fill:red;opacity:.5\"/>"));
        let out = clean(&svg("<rect style='font-family:\"A&amp;B\"; transition:x'/>"));
        assert_eq!(out.text, svg("<rect style='font-family:\"A&amp;B\"'/>"));
    }

    #[test]
    fn a_doctype_is_cut_unless_it_declares_entities() {
        let with = format!(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n{}",
            svg("<rect/>")
        );
        let out = clean(&with);
        assert!(!out.text.contains("DOCTYPE") && out.text.contains("<rect/>"));
        let bomb = format!(
            "<!DOCTYPE svg [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;\">]>\n{}",
            svg("<text>&b;</text>")
        );
        assert!(sanitize(&bomb).is_err());
    }

    #[test]
    fn caps_and_shape_errors() {
        assert!(sanitize("not xml").is_err());
        assert!(sanitize("<html/>").is_err());
        assert!(sanitize("<svg viewBox='0 0 1 1'/>").is_err(), "no namespace");
        let big = svg(&"<g/>".repeat(MAX_BYTES / 4));
        assert!(sanitize(&big).unwrap_err().to_string().contains("larger than 128 KiB"));
        let deep = svg(&format!("{}{}", "<g>".repeat(MAX_DEPTH + 5), "</g>".repeat(MAX_DEPTH + 5)));
        assert!(sanitize(&deep).unwrap_err().to_string().contains("nested deeper"));
        let ok_deep = svg(&format!("{}{}", "<g>".repeat(MAX_DEPTH - 5), "</g>".repeat(MAX_DEPTH - 5)));
        assert!(sanitize(&ok_deep).is_ok());
    }

    #[test]
    fn element_count_is_capped() {
        let over = svg(&"<g/>".repeat(MAX_ELEMENTS));
        assert!(
            over.len() < MAX_BYTES,
            "the element cap, not the byte cap, is what stops these"
        );
        assert!(sanitize(&over).unwrap_err().to_string().contains("more than 20000 elements"));
        assert!(
            sanitize(&svg(&"<g/>".repeat(MAX_ELEMENTS - 1))).is_ok(),
            "the root counts as one"
        );
    }

    #[test]
    fn sanitising_twice_changes_nothing() {
        let nasty = [
            svg("<script>x</script><rect onclick='1' width='1' fill='url(http://x)' style='animation:a 1s;fill:red'/>"),
            svg("<style>@import 'x'; .a{animation:k 1s} @keyframes k{0%{x:y}} @media (a){.b{transition:t}}</style>"),
            svg("<style>@im@import 'x';port 'y'; a{ani animation:x mation:y}</style>"),
            svg("<style>@media a { @media b { @keyframes z { } .q { animation: s } } }</style>"),
            svg("<style>/* } */ .a { content: \"}\"; animation: x } .b { fill: red }</style>"),
            svg("<style>.a { fill: red; animation: x </style>"),
            svg("<foo><bar/></foo><g onload=''><image href='javascript:alert(1)'/></g>"),
            svg("<a xlink:href='http://x'/><use xlink:href=\"data:text/html,x\"/>"),
        ];
        for src in nasty {
            let once = clean(&src);
            let twice = clean(&once.text);
            assert_eq!(twice.text, once.text, "from {src}");
            assert!(twice.warnings.is_empty(), "{:?} from {src}", twice.warnings);
        }
    }

    #[test]
    fn unterminated_and_odd_css_never_panics() {
        for css in [
            "",
            "{",
            "}",
            "a {",
            "a { b",
            "@",
            "@media",
            "@media {",
            "/*",
            "\"",
            "a{b:'",
            "a { b: url(",
            ";;;",
            "a{}}}",
            "(((",
            "@font-face{",
            "@keyframes",
        ] {
            let (out, _) = sheet(css, 0);
            let (again, _) = sheet(&out, 0);
            assert_eq!(again, out, "{css:?}");
        }
    }

    #[test]
    fn value_range_finds_the_value_inside_odd_spacing() {
        let src = "<a  b = 'x y'  c=\"z\"/>";
        let doc = Document::parse(src).unwrap();
        let root = doc.root_element();
        let attr = root.attribute_node("b").unwrap();
        let (r, q) = value_range(src, attr.range()).unwrap();
        assert_eq!((&src[r], q), ("x y", '\''));
        let attr = root.attribute_node("c").unwrap();
        let (r, q) = value_range(src, attr.range()).unwrap();
        assert_eq!((&src[r], q), ("z", '"'));
    }

    #[test]
    fn escaping_matches_the_quote() {
        assert_eq!(escape_attr("a&b<\"c'", '"'), "a&amp;b&lt;&quot;c'");
        assert_eq!(escape_attr("a\"c'", '\''), "a\"c&apos;");
    }
}
