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
//!
//! What the expansion guard counts is the document **as stored**, so the sanitiser also removes what
//! would change it afterwards or cannot be counted from the XML: an embedded image over the pixel cap
//! (or unreadable), an animation of a reference or of the cost of a filter primitive
//! ([`NOT_ANIMATED`]), one that sets a `url()` other than a gradient as a `fill` or `stroke` (a paint
//! server is a handful of stops, however often it is used), and the `id` of an element whose id an
//! earlier one has already (usvg resolves a reference to the *last* element of an id and a `<use>` to
//! the first; the guard reads one, so there is only ever one to read). A `url(#id)` in a `<style>`
//! rule is not removed: the guard counts it on every element the rule could apply to (`rules`).

use std::collections::HashSet;
use std::ops::Range;

use roxmltree::{Document, Node, NodeType};

use super::{images, invalid, MAX_BYTES, MAX_DEPTH, MAX_ELEMENTS};
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
    // roxmltree recurses once per open element and has no depth limit of its own: a few thousand
    // nested `<g>` (a few KB) overflow the stack of a thread and abort the process, long before
    // the walk below could count them. So the nesting is read off the bytes first.
    if nesting_exceeds(&src, MAX_DEPTH) {
        return Err(invalid(format!("the SVG is nested deeper than {MAX_DEPTH} elements")));
    }
    let doc = Document::parse_with_options(&src, super::xml_options()).map_err(|e| invalid(format!("not valid XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NS) {
        return Err(invalid(format!("the root element must be <svg xmlns=\"{SVG_NS}\">")));
    }
    let mut edits = Edits {
        cuts: Vec::new(),
        warnings: Vec::new(),
    };
    // The gradients an animation may name: those whose id nothing else has, so that there is no doubt
    // which element a `url(#id)` reaches (a later element repeating it loses its id, below, but this
    // does not depend on which of the two is kept).
    let mut by_id: std::collections::HashMap<&str, (usize, bool)> = std::collections::HashMap::new();
    for node in doc.descendants().filter(Node::is_element) {
        if let Some(id) = node.attribute("id") {
            let entry = by_id.entry(id).or_insert((0, false));
            entry.0 += 1;
            entry.1 = node.tag_name().namespace() == Some(SVG_NS)
                && matches!(node.tag_name().name(), "linearGradient" | "radialGradient");
        }
    }
    let gradients: HashSet<String> = by_id
        .into_iter()
        .filter(|(_, (count, gradient))| *count == 1 && *gradient)
        .map(|(id, _)| id.to_string())
        .collect();
    let mut ids: HashSet<String> = HashSet::new();
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
        if let Some(why) = cut_element(&node, &gradients) {
            skip_to = range.end;
            edits.cuts.push((range, String::new()));
            if let Some(why) = why {
                edits.warn(why);
            }
            continue;
        }
        attributes(&node, &src, &mut edits, &mut ids);
        if node.tag_name().name() == "style" {
            style_element(&node, &src, &mut edits);
        }
    }
    Ok(Sanitized {
        text: apply(&src, edits.cuts),
        warnings: edits.warnings,
    })
}

/// Whether elements nest deeper than `limit` in `src` (the root is depth 1), read off the bytes
/// without building anything: comments, CDATA, processing instructions and declarations are
/// skipped whole, quoted attribute values are skipped so a `>` or `<` in one is not a tag, and a
/// `/>` closes the element it opens. It is not a parser — text it cannot make sense of ends the
/// scan with "not too deep", and the real parser then says what is wrong — but it never recurses
/// and reads each byte once, so it is safe to run on anything before roxmltree is.
pub(super) fn nesting_exceeds(src: &str, limit: usize) -> bool {
    let b = src.as_bytes();
    let find = |from: usize, pat: &[u8]| {
        b[from.min(b.len())..]
            .windows(pat.len())
            .position(|w| w == pat)
            .map(|i| from + i)
    };
    let (mut i, mut depth) = (0usize, 0usize);
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            let Some(end) = find(i + 4, b"-->") else { return false };
            i = end + 3;
        } else if rest.starts_with(b"<![CDATA[") {
            let Some(end) = find(i + 9, b"]]>") else { return false };
            i = end + 3;
        } else if rest.starts_with(b"<?") {
            let Some(end) = find(i + 2, b"?>") else { return false };
            i = end + 2;
        } else if rest.starts_with(b"<!") {
            let Some(end) = find(i + 2, b">") else { return false };
            i = end + 1;
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            let Some(end) = find(i + 2, b">") else { return false };
            i = end + 1;
        } else {
            depth += 1;
            if depth > limit {
                return true;
            }
            let mut quote = None;
            let mut j = i + 1;
            let mut closed = false;
            while j < b.len() {
                match (quote, b[j]) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, q @ (b'"' | b'\'')) => quote = Some(q),
                    (None, b'>') => {
                        closed = true;
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            if !closed {
                return false;
            }
            if b[j - 1] == b'/' {
                depth -= 1;
            }
            i = j + 1;
        }
    }
    false
}

/// The value of the attribute `name` that has **no namespace**. roxmltree's `node.attribute("x")` with
/// a bare name matches the local name in any namespace, so `xlink:href` answers for `href` — and which
/// of the two a reader meant is exactly what a link can hide behind.
pub(super) fn plain_attribute<'a>(node: &Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.namespace().is_none() && a.name() == name)
        .map(|a| a.value())
}

/// An attribute name an animation may target: an XML name with at most one prefix
/// (`[A-Za-z_][A-Za-z0-9_.-]*`, optionally `prefix:` of the same shape). Anything else is spliced
/// into the document as markup by `SvgDoc::at`, so anything else is not a name.
pub(super) fn is_attribute_name(name: &str) -> bool {
    let ncname = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    match name.split_once(':') {
        Some((prefix, local)) => ncname(prefix) && ncname(local),
        None => ncname(name),
    }
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
fn cut_element(node: &Node, gradients: &HashSet<String>) -> Option<Option<String>> {
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
            if !is_attribute_name(attr.trim()) {
                return Some(Some(format!(
                    "animating \"{}\" was removed: it is not an attribute name",
                    attr.chars().take(40).collect::<String>()
                )));
            }
            let local = attr.rsplit(':').next().unwrap_or(attr).trim();
            let lower = local.to_ascii_lowercase();
            if lower.starts_with("on") || lower == "href" || lower == "style" {
                return Some(Some(format!(
                    "animating \"{attr}\" is not allowed and the animation was removed"
                )));
            }
            if NOT_ANIMATED.iter().any(|n| n.eq_ignore_ascii_case(local)) {
                return Some(Some(format!(
                    "animating \"{local}\" is not supported and the animation was removed (it changes what is drawn, or how much work drawing is, per frame)"
                )));
            }
        }
        // A reference in a value (`mask` going from `url(#a)` to `url(#b)`) is one the expansion guard
        // read once, in the document as stored, and could not see move. A gradient as a `fill` or
        // `stroke` is the exception: it is a handful of stops however often it is used, and a chain of
        // them is counted from the XML whichever one a frame names.
        let carries_url = ["values", "from", "to", "by"]
            .iter()
            .filter_map(|a| node.attribute(*a))
            .any(|v| v.to_ascii_lowercase().contains("url("));
        if carries_url && !only_gradient_paints(node, gradients) {
            return Some(Some(
                "an animation that sets a url() reference was removed (only a gradient fill or stroke animates between references)"
                    .to_string(),
            ));
        }
    }
    None
}

/// Whether an animation of `fill` or `stroke` names nothing but gradients of this document in the
/// `url()`s of its values.
fn only_gradient_paints(node: &Node, gradients: &HashSet<String>) -> bool {
    if !matches!(node.attribute("attributeName").map(str::trim), Some("fill" | "stroke")) {
        return false;
    }
    ["values", "from", "to", "by"]
        .iter()
        .filter_map(|a| node.attribute(*a))
        .all(|v| !has_external_url(v) && super::url_ids(v).iter().all(|id| gradients.contains(*id)))
}

/// Attributes no animation may drive. Each either **changes what a property points at** — `mask`,
/// `filter`, `clip-path` and `marker*` make an element draw another one, and `id` / `class` change
/// which `<use>`, reference or `<style>` rule reaches an element — which the expansion guard (it
/// counts what is drawn in the document as stored) cannot see move; or sets the **cost of drawing a
/// filter primitive** (`feMorphology`'s `radius`, `feConvolveMatrix`'s `order`, `feTurbulence`'s
/// `numOctaves`: one frame grows from milliseconds to minutes without the document growing a byte).
const NOT_ANIMATED: &[&str] = &[
    "mask",
    "filter",
    "clip-path",
    "clip",
    "marker",
    "marker-start",
    "marker-mid",
    "marker-end",
    "id",
    "class",
    "radius",
    "order",
    "numOctaves",
];

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

fn attributes(node: &Node, src: &str, edits: &mut Edits, ids: &mut HashSet<String>) {
    let tag = node.tag_name().name();
    // What every reader takes this element's id to be is the first attribute called `id`; when an
    // element before it had that id, this one's is dropped (and any other spelling of it with it).
    let repeated = node.attribute("id").is_some_and(|id| !ids.insert(id.to_string()));
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
        // resvg reads `xlink:width` as `width`, and roxmltree answers a bare-name lookup with it: the
        // only XLink attribute that means something is the link itself (the rest is XLink 1.0 furniture).
        if ns == Some(XLINK_NS) && name != "href" {
            cut(edits, None);
            continue;
        }
        if ns.is_none() && name.len() > 2 && name.as_bytes().get(..2).is_some_and(|b| b.eq_ignore_ascii_case(b"on")) {
            cut(edits, Some(format!("event attribute \"{name}\" was removed")));
            continue;
        }
        if name == "id" && repeated && matches!(ns, None | Some(XML_NS)) {
            cut(
                edits,
                Some(format!(
                    "the id \"{}\" was removed from a later element that repeated it (a reference reaches the first one)",
                    attr.value().chars().take(40).collect::<String>()
                )),
            );
            continue;
        }
        let value = attr.value();
        // SVG 2 and resvg both read the unprefixed `href` and ignore the `xlink:` one beside it; the
        // text stored must not hold a second, different link for any other reader to prefer.
        if name == "href" && ns == Some(XLINK_NS) && plain_attribute(node, "href").is_some() {
            cut(edits, None);
            continue;
        }
        if name == "href" && ns != Some(XML_NS) {
            if !href_allowed(tag, value) {
                cut(
                    edits,
                    Some("links and references outside the document were removed".to_string()),
                );
            } else if !value.trim().starts_with('#') {
                // The decoder allocates what the header claims: an image that claims more than a
                // title may draw is never stored.
                if let Err(why) = images::check_href(value) {
                    cut(edits, Some(why.to_string()));
                }
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

pub(super) fn unprefixed(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    match lower.strip_prefix('-').and_then(|r| r.split_once('-')) {
        Some((_, rest)) => rest.to_string(),
        None => lower,
    }
}

/// Skip a CSS string or comment starting at `i`; returns the index after it.
pub(super) fn skip_quoted(b: &[u8], i: usize) -> usize {
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

pub(super) fn starts_quoted(b: &[u8], i: usize) -> bool {
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

pub(super) fn at_name(s: &str) -> Option<&str> {
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
        let png = format!("data:image/png;base64,{}", images::tests::RED_4X4);
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
        let gradient = "<linearGradient id=\"g\"/>";
        let out = clean(&svg(&format!("<style>{css}</style>{gradient}<rect class=\"a\"/>")));
        assert_eq!(
            out.text,
            svg(&format!("<style>\n.a {{ fill: red; stroke: blue}}\n@media screen {{ .b {{ fill: green }} }}\n.c {{ fill: url(#g) }}\n</style>{gradient}<rect class=\"a\"/>"))
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

    // ---- review fixes -------------------------------------------------------------------------

    /// Run `f` on a thread with a 2 MiB stack, as small as any a caller may have: a stack overflow
    /// aborts the whole process, so these tests prove "refused" by still being alive.
    fn on_a_small_stack<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(f)
            .unwrap()
            .join()
            .expect("the thread did not panic")
    }

    #[test]
    fn deep_nesting_is_refused_before_the_parser_can_overflow_its_stack() {
        for depth in [65usize, 5_000, 20_000] {
            let src = format!(
                "<svg xmlns=\"{SVG_NS}\" viewBox=\"0 0 10 10\">{}{}</svg>",
                "<g>".repeat(depth),
                "</g>".repeat(depth)
            );
            let err = on_a_small_stack(move || sanitize(&src).unwrap_err().to_string());
            // Past the byte cap the size is what is said first; below it, the nesting.
            assert!(err.contains("nested deeper") || err.contains("larger than"), "{depth}: {err}");
        }
        let in_a_doc = format!(
            "<svg xmlns=\"{SVG_NS}\" viewBox=\"0 0 10 10\">{}</svg>",
            "<g>".repeat(5_000) + &"</g>".repeat(5_000)
        );
        assert!(in_a_doc.len() < MAX_BYTES);
        let e = on_a_small_stack(move || crate::svg::SvgDoc::parse(&in_a_doc).unwrap_err().to_string());
        assert!(e.contains("nested deeper"), "{e}");
        // Unclosed nesting is refused as deep too, rather than left for the parser to recurse into.
        let open = format!("<svg xmlns=\"{SVG_NS}\" viewBox=\"0 0 10 10\">{}", "<g>".repeat(5_000));
        assert!(on_a_small_stack(move || sanitize(&open).is_err()));
    }

    #[test]
    fn the_depth_scan_counts_elements_and_nothing_else() {
        let nest = |n: usize| format!("{}{}", "<g>".repeat(n), "</g>".repeat(n));
        assert!(!nesting_exceeds(&nest(64), 64), "exactly the limit is allowed");
        assert!(nesting_exceeds(&nest(65), 64));
        // Self-closing elements are not open; siblings do not add up.
        assert!(!nesting_exceeds(
            &format!("{}{}", "<g/>".repeat(500), "<g></g>".repeat(500)),
            3
        ));
        // A comment, CDATA, a processing instruction and a declaration hide what is inside them.
        let hidden = format!(
            "<!--{}--><![CDATA[{}]]><?pi {} ?><!X {}>",
            "<g>".repeat(99),
            "<g>".repeat(99),
            "<g>".repeat(99),
            "<g>"
        );
        assert!(!nesting_exceeds(&hidden, 1), "{hidden}");
        // A quoted attribute value hides its `>` and `/`, and a tag with a `/>` after one closes.
        assert!(!nesting_exceeds("<a t=\"/>\"><b/></a>", 2));
        assert!(!nesting_exceeds("<a t='>'/><a t='>'/><a/>", 1));
        assert!(nesting_exceeds("<a t=\">\"><a t='>'><a/></a></a>", 2));
        // Text it cannot make sense of is the parser's to refuse.
        assert!(!nesting_exceeds("<a><b", 2));
        assert!(!nesting_exceeds("<!-- never closed <a><a><a>", 1));
        assert!(!nesting_exceeds("</a></a></a><a>", 1));
    }

    #[test]
    fn a_multibyte_attribute_name_does_not_panic() {
        for name in ["日本", "o日", "on日", "\u{1F600}x", "ö"] {
            let src = svg(&format!("<rect {name}=\"1\" width=\"2\" height=\"2\"/>"));
            let out = on_a_small_stack(move || sanitize(&src));
            assert!(out.is_ok(), "{name}");
        }
        // The event attributes still go, whatever their case.
        let out = clean(&svg("<rect OnClick=\"x()\" onload=\"y()\" width=\"2\" height=\"2\"/>"));
        assert_eq!(out.text, svg("<rect width=\"2\" height=\"2\"/>"));
    }

    #[test]
    fn an_animation_may_only_target_a_name() {
        for ok in ["x", "stroke-width", "xlink:foo", "_a.b-c", "fill-opacity", "viewBox"] {
            assert!(is_attribute_name(ok), "{ok}");
        }
        for bad in [
            "",
            " ",
            "x=\"1\" href=\"/etc/passwd\" onload=\"a\" y",
            "a b",
            "a\"b",
            "a'b",
            "a<b",
            "a&lt;b",
            "a;b",
            "1x",
            "-x",
            ":x",
            "x:",
            "a:b:c",
            "x/",
            "日本",
        ] {
            assert!(!is_attribute_name(bad), "{bad:?}");
        }
        let inject = "<rect width=\"2\" height=\"2\"><animate attributeName='x=&quot;1&quot; href=&quot;/etc/passwd&quot; onload=&quot;a&quot; y' to=\"5\" dur=\"1s\"/><set attributeName=\"a;b\" to=\"1\"/><animate attributeName=\"fill\" to=\"red\" dur=\"1s\"/></rect>";
        let out = clean(&svg(inject));
        assert_eq!(
            out.text,
            svg("<rect width=\"2\" height=\"2\"><animate attributeName=\"fill\" to=\"red\" dur=\"1s\"/></rect>")
        );
        assert!(
            out.warnings.iter().any(|w| w.contains("not an attribute name")),
            "{:?}",
            out.warnings
        );
        assert_eq!(clean(&out.text).text, out.text, "and the result is its own fixed point");
    }

    #[test]
    fn animated_values_cannot_leave_the_document() {
        let out = clean(&svg(
            "<rect width=\"2\" height=\"2\"><animate attributeName=\"fill\" values=\"red;url(http://evil/x)\" dur=\"1s\"/><set attributeName=\"xlink:href\" to=\"http://evil\"/><animate attributeName=\"fill\" to=\"url( 'file:///etc/passwd' )\" dur=\"1s\"/><animate attributeName=\"mask\" from=\"url(#a)\" to=\"url(#b)\" dur=\"1s\"/></rect>",
        ));
        assert!(!out.text.contains("evil") && !out.text.contains("passwd"), "{}", out.text);
        assert!(!out.text.contains("xlink:href"), "{}", out.text);
    }

    #[test]
    fn an_animation_may_not_move_a_reference_or_set_the_cost_of_a_filter() {
        // What the expansion guard counts is the document as stored; none of these may change it.
        for attr in [
            "mask",
            "filter",
            "clip-path",
            "marker-end",
            "marker",
            "id",
            "class",
            "radius",
            "order",
            "numOctaves",
            "Mask",
            "xml:id",
        ] {
            let src = svg(&format!(
                "<rect width=\"2\" height=\"2\"><animate attributeName=\"{attr}\" values=\"a;b\" dur=\"1s\"/></rect>"
            ));
            let out = clean(&src);
            assert_eq!(out.text, svg("<rect width=\"2\" height=\"2\"></rect>"), "{attr}");
            assert!(
                out.warnings.iter().any(|w| w.contains("is not supported")),
                "{attr}: {:?}",
                out.warnings
            );
        }
        // Ordinary attributes and properties are still animated.
        let ok = svg("<rect width=\"2\" height=\"2\"><animate attributeName=\"fill\" values=\"red;blue\" dur=\"1s\"/><animate attributeName=\"r\" to=\"3\" dur=\"1s\"/></rect>");
        assert_eq!(clean(&ok).text, ok);
    }

    #[test]
    fn an_animation_may_not_set_a_url_unless_it_is_a_gradient_fill_or_stroke() {
        let defs = "<linearGradient id=\"g\"/><radialGradient id=\"r\"/><pattern id=\"p\" width=\"1\" height=\"1\"/>";
        for (attr, extra) in [
            // not a gradient of this document: a pattern, a missing id, a mask
            ("fill", "values=\"red;url(#p)\""),
            ("stroke", "from=\"url(#g)\" to=\"url(#nobody)\""),
            ("fill", "to=\"URL(#p)\""),
            ("fill", "by=\"url( '#p' )\""),
            // a gradient, but not of a paint
            ("color", "values=\"red;url(#g)\""),
            ("stop-color", "to=\"url(#g)\""),
            ("fill-opacity", "to=\"url(#g)\""),
            // a paint that leaves the document
            ("fill", "values=\"url(#g);url(http://evil/x)\""),
        ] {
            let src = svg(&format!(
                "{defs}<rect width=\"2\" height=\"2\"><animate attributeName=\"{attr}\" {extra} dur=\"1s\"/></rect>"
            ));
            let out = clean(&src);
            assert!(!out.text.contains("<animate"), "{extra}: {}", out.text);
            assert!(
                out.warnings.iter().any(|w| w.contains("sets a url()")),
                "{extra}: {:?}",
                out.warnings
            );
        }
        let set = clean(&svg(
            "<rect width=\"2\" height=\"2\"><set attributeName=\"fill\" to=\"url(#nobody)\" begin=\"1s\"/></rect>",
        ));
        assert!(!set.text.contains("<set"), "{}", set.text);
        // A gradient is a handful of stops however often it is used: a fill or stroke may change
        // between gradients (and colours), as Illustrator and After Effects exports do.
        for (attr, extra) in [
            ("fill", "values=\"url(#g);url(#r);red\""),
            ("stroke", "from=\"url(#g)\" to=\"url(#r)\""),
            ("fill", "to=\"url( '#g' )\""),
            ("fill", "by=\"URL(#g)\""),
        ] {
            let src = svg(&format!(
                "{defs}<rect width=\"2\" height=\"2\"><animate attributeName=\"{attr}\" {extra} dur=\"1s\"/></rect>"
            ));
            let out = clean(&src);
            assert_eq!(out.text, src, "{extra}");
            assert!(out.warnings.is_empty(), "{extra}: {:?}", out.warnings);
        }
        let set = svg(&format!(
            "{defs}<rect width=\"2\" height=\"2\"><set attributeName=\"fill\" to=\"url(#g)\" begin=\"1s\"/></rect>"
        ));
        assert_eq!(clean(&set).text, set);
        // Mask, filter and clip-path never animate, a gradient or not.
        let mask = clean(&svg(&format!(
            "{defs}<rect width=\"2\" height=\"2\"><animate attributeName=\"mask\" values=\"url(#g);url(#r)\" dur=\"1s\"/></rect>"
        )));
        assert!(!mask.text.contains("<animate"), "{}", mask.text);
    }

    #[test]
    fn a_gradient_whose_id_is_shared_is_not_one_an_animation_may_name() {
        // Which element `url(#g)` reaches is the question the guard cannot leave open: the pattern is
        // first, so it is the one that stays; the gradient (and so the animation's right to name `g`)
        // is not what is drawn.
        let src = svg(
            "<pattern id=\"g\" width=\"1\" height=\"1\"/><linearGradient id=\"g\"/><rect width=\"2\" height=\"2\"><animate attributeName=\"fill\" values=\"red;url(#g)\" dur=\"1s\"/></rect>",
        );
        let out = clean(&src);
        assert!(!out.text.contains("<animate"), "{}", out.text);
        let swapped = svg(
            "<linearGradient id=\"g\"/><pattern id=\"g\" width=\"1\" height=\"1\"/><rect width=\"2\" height=\"2\"><animate attributeName=\"fill\" values=\"red;url(#g)\" dur=\"1s\"/></rect>",
        );
        assert!(!clean(&swapped).text.contains("<animate"), "either way round");
    }

    #[test]
    fn a_later_element_repeating_an_id_loses_it() {
        // usvg reaches the last element of an id through `url(#id)` and an href chain, and the first
        // through `<use>`; the guard counts one. So there is only ever one: the first, which is also
        // the one a browser reaches.
        let out = clean(&svg(
            "<linearGradient id=\"g\"><stop offset=\"0\"/></linearGradient><linearGradient id=\"g\"/><rect id=\"g\" width=\"2\" height=\"2\"/><g id=\"h\"><rect id=\"h\" xml:id=\"h\"/></g><rect id=\"k\"/><use href=\"#k\" id=\"u\"/>",
        ));
        assert_eq!(
            out.text,
            svg("<linearGradient id=\"g\"><stop offset=\"0\"/></linearGradient><linearGradient/><rect width=\"2\" height=\"2\"/><g id=\"h\"><rect/></g><rect id=\"k\"/><use href=\"#k\" id=\"u\"/>")
        );
        assert!(
            out.warnings.iter().any(|w| w.contains("\"g\"") && w.contains("repeated")),
            "{:?}",
            out.warnings
        );
        assert_eq!(
            clean(&out.text).text,
            out.text,
            "and the pass over its own output changes nothing"
        );
        assert!(clean(&out.text).warnings.is_empty());
        // An id on something that is cut was never in the document: it does not take the id from a
        // kept element after it.
        let cut = clean(&svg("<script id=\"s\"/><rect id=\"s\" width=\"1\" height=\"1\"/>"));
        assert!(cut.text.contains("<rect id=\"s\""), "{}", cut.text);
    }

    #[test]
    fn a_style_rule_keeps_its_references_for_the_guard_to_count() {
        // Illustrator's "Internal CSS" writes its clips this way: they stay, and the expansion guard
        // counts them on every element the rule could apply to (`rules`).
        let defs =
            "<linearGradient id=\"g\"/><radialGradient id=\"r\"/><pattern id=\"p\" width=\"1\" height=\"1\"/><mask id=\"m\"/><clipPath id=\"SVGID_1_\"/>";
        let css = ".st0 { clip-path: url(#SVGID_1_) } .a { fill: url(#g); stroke: url(#r) } .b { fill: url(#p) } .c { mask: url(#m) } .d { clip-path: url(#x); filter: url(#f); marker-end: url(#k) } .e { opacity: .5; fill: URL(#p) }";
        let src = svg(&format!("<style>{css}</style>{defs}<rect class=\"a\"/>"));
        let out = clean(&src);
        assert_eq!(out.text, src);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        // An external reference in a rule still goes.
        let ext = clean(&svg(".x{}").replace(
            ".x{}",
            "<style>.x { filter: url(http://evil/f.svg#f); mask: url(#m) }</style>",
        ));
        assert_eq!(ext.text, svg("<style>.x { mask: url(#m) }</style>"));
        assert!(ext.warnings.iter().any(|w| w.contains("outside the document")));
        // The pass over its own output changes nothing.
        assert_eq!(clean(&out.text).text, out.text);
    }

    #[test]
    fn an_embedded_image_over_the_cap_or_unreadable_is_cut_and_a_small_one_stays() {
        let ok = format!("data:image/png;base64,{}", images::tests::RED_4X4);
        let huge = images::tests::data_url("image/png", &images::tests::png_claiming(60_000, 60_000));
        let bad = "data:image/png;base64,iVBORw0KGgo=";
        let wrong = images::tests::data_url("image/jpeg", &images::tests::png_claiming(4, 4));
        let out = clean(&svg(&format!(
            "<image href=\"{ok}\"/><image href=\"{huge}\"/><image href=\"{bad}\"/><image href=\"{wrong}\"/><feImage href=\"{huge}\"/>"
        )));
        assert_eq!(
            out.text,
            svg(&format!("<image href=\"{ok}\"/><image/><image/><image/><feImage/>"))
        );
        assert!(out.warnings.iter().any(|w| w.contains("60000 x 60000")), "{:?}", out.warnings);
        assert!(
            out.warnings.iter().any(|w| w.contains("could not be read")),
            "{:?}",
            out.warnings
        );
        assert_eq!(clean(&out.text).text, out.text, "idempotent");
    }

    #[test]
    fn the_unprefixed_href_is_the_only_one_that_stays_beside_an_xlink_one() {
        let out = clean(&svg(
            "<use xlink:href=\"#cheap\" href=\"#big\"/><use href=\"#a\" xlink:href=\"#b\"/><use xlink:href=\"#only\"/>",
        ));
        assert_eq!(
            out.text,
            svg("<use href=\"#big\"/><use href=\"#a\"/><use xlink:href=\"#only\"/>")
        );
    }

    #[test]
    fn an_xlink_attribute_that_is_not_the_link_is_dropped() {
        // resvg would read `xlink:width` as `width`, and a bare-name lookup in roxmltree finds it too.
        let out = clean(&svg(
            "<rect xlink:width=\"900\" xlink:title=\"t\" width=\"2\" height=\"2\" xml:space=\"preserve\"/>",
        ));
        assert_eq!(out.text, svg("<rect width=\"2\" height=\"2\" xml:space=\"preserve\"/>"));
    }
}
