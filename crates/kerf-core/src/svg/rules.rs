//! The `url(#id)` references a `<style>` sheet gives elements, for the expansion guard.
//!
//! A rule draws another element for everything its selector matches (`.a { mask: url(#m) }` over a
//! thousand elements is a thousand masks), so the guard has to count them where they land. It has no
//! selector engine, and needs none: it reads each rule's **subject** (the last compound selector:
//! `svg g.a > rect.b:first-child` is `rect.b:first-child`) and takes the rule to apply to every
//! element that subject *could* match — the element name, the classes and the id it spells out, and
//! nothing else. A pseudo-class, an attribute test, an ancestor or a sibling can only narrow what a
//! selector matches, never widen it, so ignoring them over-counts and never under-counts. A selector it
//! cannot read as plain names (a namespace bar, an escape) is taken to match every element.
//!
//! That is what keeps `.stN { clip-path: url(#SVGID_n_) }` — how Illustrator's "Internal CSS" export
//! writes its clips — in the document: the sanitiser used to cut such a declaration because it could
//! not be counted, and now it is.

use std::collections::HashMap;

use roxmltree::{Document, Node};

use super::sanitize::{at_name, skip_quoted, starts_quoted, unprefixed};
use super::url_ids;
use super::REFERENCING;

/// The nested at-rules whose rules apply as written (`@media`, `@supports`...), read to this depth.
const MAX_AT_DEPTH: usize = 8;

/// What a selector spells out about the element it ends in.
#[derive(Debug, Default, PartialEq)]
struct Subject {
    name: Option<String>,
    classes: Vec<String>,
    id: Option<String>,
}

impl Subject {
    fn matches(&self, node: &Node) -> bool {
        if let Some(name) = &self.name {
            if !node.tag_name().name().eq_ignore_ascii_case(name) {
                return false;
            }
        }
        if let Some(id) = &self.id {
            if node.attribute("id") != Some(id.as_str()) {
                return false;
            }
        }
        if self.classes.is_empty() {
            return true;
        }
        let have = node.attribute("class").unwrap_or("");
        self.classes
            .iter()
            .all(|c| have.split_ascii_whitespace().any(|h| h.eq_ignore_ascii_case(c)))
    }
}

/// One rule's selectors and the ids its declarations point at.
#[derive(Debug)]
struct Rule {
    subjects: Vec<Subject>,
    refs: Vec<String>,
}

/// The reference-bearing rules of a document's `<style>` sheets, indexed by what their selectors name.
#[derive(Debug, Default)]
pub(super) struct Rules {
    rules: Vec<Rule>,
    by_class: HashMap<String, Vec<(usize, usize)>>,
    by_id: HashMap<String, Vec<(usize, usize)>>,
    by_name: HashMap<String, Vec<(usize, usize)>>,
    any: Vec<(usize, usize)>,
}

impl Rules {
    /// Every rule of every `<style>` element in `doc` that points a property at an id.
    pub(super) fn read(doc: &Document) -> Rules {
        let mut found: Vec<(String, String)> = Vec::new();
        for style in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "style") {
            let css: String = style.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect();
            scan(&strip_comments(&css), 0, &mut found);
        }
        let mut out = Rules::default();
        for (prelude, body) in found {
            let refs = declared_refs(&body);
            if refs.is_empty() {
                continue;
            }
            let n = out.rules.len();
            let subjects: Vec<Subject> = selectors(&prelude).iter().map(|s| subject(s)).collect();
            for (k, s) in subjects.iter().enumerate() {
                let at = (n, k);
                if let Some(c) = s.classes.first() {
                    out.by_class.entry(c.to_ascii_lowercase()).or_default().push(at);
                } else if let Some(id) = &s.id {
                    out.by_id.entry(id.clone()).or_default().push(at);
                } else if let Some(name) = &s.name {
                    out.by_name.entry(name.to_ascii_lowercase()).or_default().push(at);
                } else {
                    out.any.push(at);
                }
            }
            out.rules.push(Rule { subjects, refs });
        }
        out
    }

    /// The ids the sheets point `node` at (a rule counted once however many of its selectors match).
    pub(super) fn refs_for(&self, node: &Node) -> Vec<&str> {
        if self.rules.is_empty() {
            return Vec::new();
        }
        let mut hit: Vec<usize> = Vec::new();
        let mut look = |list: Option<&Vec<(usize, usize)>>| {
            for &(n, k) in list.into_iter().flatten() {
                if !hit.contains(&n) && self.rules[n].subjects[k].matches(node) {
                    hit.push(n);
                }
            }
        };
        for class in node.attribute("class").unwrap_or("").split_ascii_whitespace() {
            look(self.by_class.get(&class.to_ascii_lowercase()));
        }
        look(node.attribute("id").and_then(|id| self.by_id.get(id)));
        look(self.by_name.get(&node.tag_name().name().to_ascii_lowercase()));
        look(Some(&self.any));
        hit.iter()
            .flat_map(|&n| self.rules[n].refs.iter().map(String::as_str))
            .collect()
    }
}

/// `css` without its comments (each becomes a space), strings left alone.
fn strip_comments(css: &str) -> String {
    let b = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if starts_quoted(b, i) {
            let end = skip_quoted(b, i);
            if b[i] == b'/' {
                out.push_str(&css[copied..i]);
                out.push(' ');
                copied = end;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&css[copied..]);
    out
}

/// The (prelude, body) of every style rule in `css`, looking inside `@media` and the like.
fn scan(css: &str, depth: usize, out: &mut Vec<(String, String)>) {
    let b = css.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let (mut j, mut paren, mut block) = (i, 0usize, false);
        while j < b.len() {
            if starts_quoted(b, j) {
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
                _ => {}
            }
            j += 1;
        }
        if !block {
            i = j.max(start + 1).min(b.len());
            continue;
        }
        let head_end = j;
        let (mut k, mut level) = (head_end + 1, 1usize);
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
        let body_end = if level == 0 { k - 1 } else { k.min(b.len()) };
        let prelude = css[start..head_end].trim();
        let body = &css[head_end + 1..body_end.max(head_end + 1)];
        match at_name(prelude).map(unprefixed) {
            Some(n) if matches!(n.as_str(), "media" | "supports" | "layer" | "container" | "document") => {
                if depth < MAX_AT_DEPTH {
                    scan(body, depth + 1, out);
                }
            }
            Some(_) => {}
            None => out.push((prelude.to_string(), body.to_string())),
        }
        i = k.max(start + 1);
    }
}

/// The ids a rule body points properties at. A property that can draw another element counts, with
/// or without a vendor prefix; a name it cannot read (an escape) counts too, whatever it turns out
/// to be.
fn declared_refs(body: &str) -> Vec<String> {
    let b = body.as_bytes();
    let mut refs = Vec::new();
    let (mut start, mut i, mut paren) = (0, 0, 0usize);
    let declaration = |text: &str, refs: &mut Vec<String>| {
        let Some((prop, value)) = text.split_once(':') else { return };
        let prop = unprefixed(prop);
        if prop.contains('\\') || REFERENCING.contains(&prop.as_str()) {
            refs.extend(url_ids(value).into_iter().map(str::to_string));
        }
    };
    while i < b.len() {
        if starts_quoted(b, i) {
            i = skip_quoted(b, i);
            continue;
        }
        match b[i] {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b';' if paren == 0 => {
                declaration(&body[start..i], &mut refs);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    declaration(&body[start.min(body.len())..], &mut refs);
    refs
}

/// The comma-separated selectors of a prelude (a comma inside brackets or parentheses is not a split).
fn selectors(prelude: &str) -> Vec<String> {
    let b = prelude.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut i, mut depth) = (0, 0, 0usize);
    while i < b.len() {
        if starts_quoted(b, i) {
            i = skip_quoted(b, i);
            continue;
        }
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(prelude[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(prelude[start.min(prelude.len())..].trim().to_string());
    out
}

/// What the last compound selector of `selector` spells out.
fn subject(selector: &str) -> Subject {
    let b = selector.as_bytes();
    let (mut last, mut start, mut i, mut depth) = ((0, 0), 0, 0, 0usize);
    while i < b.len() {
        if starts_quoted(b, i) {
            i = skip_quoted(b, i);
            continue;
        }
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            c if depth == 0 && (c.is_ascii_whitespace() || matches!(c, b'>' | b'+' | b'~')) => {
                if i > start {
                    last = (start, i);
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if b.len() > start {
        last = (start, b.len());
    }
    compound(&selector[last.0..last.1])
}

fn ident_end(s: &str, from: usize) -> usize {
    s[from..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_') || !c.is_ascii()))
        .map_or(s.len(), |e| from + e)
}

/// `rect.a.b#c[x]:not(.d)` as a name, classes and an id; anything it does not understand as a plain
/// name is "every element".
fn compound(text: &str) -> Subject {
    let any = Subject::default();
    let b = text.as_bytes();
    let mut out = Subject::default();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'*' if i == 0 => i += 1,
            b'.' | b'#' => {
                let end = ident_end(text, i + 1);
                if end == i + 1 {
                    return any;
                }
                let name = text[i + 1..end].to_string();
                if b[i] == b'.' {
                    out.classes.push(name);
                } else if out.id.is_some() {
                    // Two ids match nothing; taking either is the wider reading.
                } else {
                    out.id = Some(name);
                }
                i = end;
            }
            b'[' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b']' {
                    j = if starts_quoted(b, j) { skip_quoted(b, j) } else { j + 1 };
                }
                i = (j + 1).min(b.len());
            }
            b':' => {
                while i < b.len() && b[i] == b':' {
                    i += 1;
                }
                i = ident_end(text, i);
                if b.get(i) == Some(&b'(') {
                    let (mut depth, mut j) = (0usize, i);
                    while j < b.len() {
                        if starts_quoted(b, j) {
                            j = skip_quoted(b, j);
                            continue;
                        }
                        match b[j] {
                            b'(' => depth += 1,
                            b')' => {
                                depth -= 1;
                                if depth == 0 {
                                    j += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    i = j.min(b.len());
                }
            }
            c if i == 0 && (c.is_ascii_alphabetic() || c == b'_' || !c.is_ascii()) => {
                let end = ident_end(text, 0);
                // A namespace bar (`svg|rect`) or an escape follows: not a name we can compare.
                if matches!(b.get(end), Some(b'|' | b'\\')) {
                    return any;
                }
                out.name = Some(text[..end].to_string());
                i = end;
            }
            _ => return any,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_of(doc: &str) -> (Document<'_>, Rules) {
        let doc = Document::parse(doc).unwrap();
        let rules = Rules::read(&doc);
        (doc, rules)
    }

    fn svg(css: &str, inner: &str) -> String {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><style>{css}</style>{inner}</svg>")
    }

    fn refs(doc: &Document, rules: &Rules, id: &str) -> Vec<String> {
        let node = doc
            .descendants()
            .find(|n| n.attribute("data-n") == Some(id))
            .unwrap_or_else(|| panic!("no element {id}"));
        let mut r: Vec<String> = rules.refs_for(&node).into_iter().map(str::to_string).collect();
        r.sort();
        r
    }

    #[test]
    fn a_rule_applies_to_every_element_its_subject_could_match() {
        let css = ".a { clip-path: url(#c) } rect.b { mask: url(#m) } #only { filter: url(#f) } svg g > .y .z:first-child { marker-end: url(#k) }";
        let src = svg(
            css,
            "<rect data-n=\"1\" class=\"a\"/><rect data-n=\"2\" class=\"b\"/><circle data-n=\"3\" class=\"b\"/><rect data-n=\"4\" id=\"only\"/><g data-n=\"5\"/><path data-n=\"6\" class=\"z\"/><path data-n=\"7\" class=\"a b\"/><rect data-n=\"8\" id=\"only2\"/>",
        );
        let (doc, rules) = rules_of(&src);
        assert_eq!(refs(&doc, &rules, "1"), ["c"]);
        assert_eq!(refs(&doc, &rules, "2"), ["m"], "rect.b: the name and the class");
        assert!(refs(&doc, &rules, "3").is_empty(), "a circle is not a rect");
        assert_eq!(refs(&doc, &rules, "4"), ["f"]);
        assert!(refs(&doc, &rules, "5").is_empty(), "nothing names a g");
        assert_eq!(
            refs(&doc, &rules, "6"),
            ["k"],
            "the ancestors and the pseudo-class are ignored: wider, never narrower"
        );
        assert_eq!(refs(&doc, &rules, "7"), ["c"], "`rect.b` is not a path");
        assert!(refs(&doc, &rules, "8").is_empty(), "an id is the whole id");
    }

    #[test]
    fn a_selector_it_cannot_read_matches_everything() {
        for selector in ["svg|rect", "\\.a", "[x=y]", ":not(.a)", "*", ":root", ".", "#"] {
            let src = svg(
                &format!("{selector} {{ mask: url(#m) }}"),
                "<rect data-n=\"1\"/><g data-n=\"2\" class=\"q\"/>",
            );
            let (doc, rules) = rules_of(&src);
            assert_eq!(refs(&doc, &rules, "1"), ["m"], "{selector}");
            assert_eq!(refs(&doc, &rules, "2"), ["m"], "{selector}");
        }
    }

    #[test]
    fn selector_lists_at_rules_and_comments() {
        let css = "/* .a { mask: url(#no) } */ .a, .b { fill: url(#g); /* x */ stroke: url(#h) } @media screen { .c { filter: url(#f) } } @font-face { font-family: F } .d { opacity: .5; color: red } .e { /* hidden */ mask /* in the name */ : url(#m) }";
        let src = svg(
            css,
            "<rect data-n=\"1\" class=\"a\"/><rect data-n=\"2\" class=\"b\"/><rect data-n=\"3\" class=\"c\"/><rect data-n=\"4\" class=\"d\"/><rect data-n=\"5\" class=\"e\"/>",
        );
        let (doc, rules) = rules_of(&src);
        assert_eq!(refs(&doc, &rules, "1"), ["g", "h"]);
        assert_eq!(refs(&doc, &rules, "2"), ["g", "h"]);
        assert_eq!(refs(&doc, &rules, "3"), ["f"], "inside @media");
        assert!(refs(&doc, &rules, "4").is_empty(), "no reference");
        assert_eq!(refs(&doc, &rules, "5"), ["m"]);
        assert!(
            !refs(&doc, &rules, "1").contains(&"no".to_string()),
            "a comment is not a rule"
        );
    }

    #[test]
    fn a_name_it_cannot_read_counts_and_vendor_prefixes_are_dropped() {
        let css = ".a { -webkit-mask: url(#m) } .b { \\6d ask: url(#e) } .c { color: url(#nothing) }";
        let src = svg(
            css,
            "<rect data-n=\"1\" class=\"a\"/><rect data-n=\"2\" class=\"b\"/><rect data-n=\"3\" class=\"c\"/>",
        );
        let (doc, rules) = rules_of(&src);
        assert_eq!(refs(&doc, &rules, "1"), ["m"]);
        assert_eq!(refs(&doc, &rules, "2"), ["e"]);
        assert!(refs(&doc, &rules, "3").is_empty());
    }

    #[test]
    fn odd_css_never_panics() {
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
            ".a{mask:url(#m)",
            "@media{.a{mask:url(#m)}",
        ] {
            let src = svg(css, "<rect data-n=\"1\" class=\"a\"/>");
            if let Ok(doc) = Document::parse(&src) {
                let rules = Rules::read(&doc);
                let _ = rules.refs_for(&doc.root_element());
            }
        }
    }
}
