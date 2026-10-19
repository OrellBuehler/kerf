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
/// `inline-size`, written first in its `style` and important (an `inline-size` attribute, and one already in
/// its `style`, are taken out), and its fit mark, or neither. Nothing else of the document moves.
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
    // The width goes first in the `style` attribute, marked important: it beats a presentation attribute and
    // any `<style>` rule, as the box the user set should. No box where a rule may still give one is `auto`.
    let declaration = match text_box.width {
        Some(w) => Some(format!("inline-size:{}px !important", number(w))),
        None if sheet_names_inline_size(&doc) => Some("inline-size:auto !important".to_string()),
        None => None,
    };
    let cut_whole = |range: std::ops::Range<usize>| {
        let start = svg[..range.start]
            .rfind(|c: char| !c.is_ascii_whitespace())
            .map_or(range.start, |i| i + 1);
        (start..range.end, String::new())
    };
    let mut cuts = Vec::new();
    let mut styled = false;
    for attr in node.attributes().filter(|a| a.namespace().is_none()) {
        let range = attr.range();
        match attr.name() {
            "inline-size" | FIT_MARK => cuts.push(cut_whole(range)),
            "style" => {
                styled = true;
                // First: a declaration the renderer's tokenizer cannot read (a `drop-shadow(...)` filter, a
                // custom property, one with no colon) ends its reading of the list there.
                let mut kept: Vec<&str> = declaration.as_deref().into_iter().collect();
                kept.extend(
                    declarations(attr.value())
                        .into_iter()
                        .filter(|d| !is_property(d, "inline-size")),
                );
                let value = kept.join(";");
                if value.trim().is_empty() {
                    cuts.push(cut_whole(range));
                } else if value != attr.value() {
                    if let Some((vr, q)) = value_range(svg, range) {
                        cuts.push((vr, escape_attr(&value, q)));
                    }
                }
            }
            _ => {}
        }
    }
    let mut add = String::new();
    if let (false, Some(d)) = (styled, &declaration) {
        add.push_str(&format!(" style=\"{}\"", escape_attr(d, '"')));
    }
    if text_box.width.is_some() && text_box.fit == TextFit::Shrink {
        add.push_str(&format!(" {FIT_MARK}=\"shrink\""));
    }
    if !add.is_empty() {
        let open_end = start_tag_end(svg, node.range().start).ok_or_else(|| invalid("the text cannot be read"))?;
        let at = if svg[..open_end].ends_with("/>") {
            open_end - 2
        } else {
            open_end - 1
        };
        cuts.push((at..at, add));
    }
    Ok(apply(svg, cuts))
}

/// Whether a `<style>` sheet of the document mentions `inline-size` (a rule could give a text a box).
fn sheet_names_inline_size(doc: &Document) -> bool {
    doc.descendants().filter(|n| n.has_tag_name((SVG_NS, "style"))).any(|n| {
        n.descendants()
            .filter_map(|t| t.text())
            .any(|t| t.to_ascii_lowercase().contains("inline-size"))
    })
}

/// The box `svg`'s `<text>` named `element` has, read as the renderer weighs it: an `inline-size` in its
/// `style` first (the last one, `auto` being none), else the attribute. A `<style>` rule is not read. `None`
/// for no such text.
pub fn text_box_of(svg: &str, element: &str) -> Option<TextBox> {
    if svg.len() > MAX_BYTES || nesting_exceeds(svg, MAX_DEPTH) {
        return None;
    }
    let doc = Document::parse_with_options(svg, xml_options()).ok()?;
    let node: Node = doc
        .descendants()
        .find(|n| n.is_element() && n.has_tag_name((SVG_NS, "text")) && plain_attribute(n, "id") == Some(element))?;
    let from_style = plain_attribute(&node, "style").and_then(|style| {
        declarations(style)
            .into_iter()
            .rev()
            .find(|d| is_property(d, "inline-size"))
            .and_then(|d| d.split_once(':'))
            .map(|(_, v)| v.trim().trim_end_matches("!important").trim())
    });
    let width = from_style
        .or_else(|| plain_attribute(&node, "inline-size"))
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

/// Whether the declaration `d` (`prop: value`) sets `prop`.
fn is_property(d: &str, prop: &str) -> bool {
    d.split_once(':').is_some_and(|(p, _)| p.trim().eq_ignore_ascii_case(prop))
}

/// A declaration list's declarations as written, split at the `;` outside quotes, comments and
/// parentheses; empty ones left out.
fn declarations(body: &str) -> Vec<&str> {
    let b = body.as_bytes();
    let mut out: Vec<&str> = Vec::new();
    let (mut start, mut i, mut paren) = (0, 0, 0usize);
    while i < b.len() {
        if starts_quoted(b, i) {
            i = skip_quoted(b, i);
            continue;
        }
        match b[i] {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b';' if paren == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&body[start.min(body.len())..]);
    out.retain(|d| !d.trim().is_empty());
    out
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
            doc(r#"<text id="t" x="5" y="50" style="inline-size:120.5px !important;fill:red;opacity:.5">Hi</text>"#)
        );
        assert_eq!(text_box_of(&out, "t"), Some(boxed(120.5, TextFit::Wrap)));
        let shrunk = set_text_box(&out, "t", &boxed(300.0, TextFit::Shrink)).unwrap();
        assert!(
            shrunk.contains(r#"style="inline-size:300px !important;fill:red;opacity:.5" data-kerf-fit="shrink">Hi"#),
            "{shrunk}"
        );
        assert_eq!(text_box_of(&shrunk, "t"), Some(boxed(300.0, TextFit::Shrink)));
        let none = set_text_box(&shrunk, "t", &TextBox::default()).unwrap();
        assert_eq!(
            none,
            doc(r#"<text id="t" x="5" y="50" style="fill:red;opacity:.5">Hi</text>"#)
        );
        assert_eq!(text_box_of(&none, "t"), Some(TextBox::default()));
        let empty = set_text_box(&doc(r#"<text id="t" x="5" y="50"/>"#), "t", &boxed(10.0, TextFit::Wrap)).unwrap();
        assert_eq!(
            empty,
            doc(r#"<text id="t" x="5" y="50" style="inline-size:10px !important"/>"#)
        );
        let bare = set_text_box(&empty, "t", &TextBox::default()).unwrap();
        assert_eq!(bare, doc(r#"<text id="t" x="5" y="50"/>"#));
        // The style is read first, as the renderer weighs it.
        let both = doc(r#"<text id="t" inline-size="80" style="inline-size: 30px">a</text>"#);
        assert_eq!(text_box_of(&both, "t"), Some(boxed(30.0, TextFit::Wrap)));
        let auto = doc(r#"<text id="t" inline-size="80" style="inline-size:auto">a</text>"#);
        assert_eq!(text_box_of(&auto, "t"), Some(TextBox::default()));
    }

    const CORPUS_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/fixtures/text-box-corpus.json"
    );

    /// Titles, a box set on one of their texts, and what `set_text_box` wrote (or refused) and `text_box_of`
    /// reads back: for the frontend's port (`title-text.ts`'s `withTextBox` and `titleTexts`, replayed by
    /// `text-box-corpus.test.ts`).
    fn corpus() -> serde_json::Value {
        let sheet = "<style>.n{inline-size:60px}</style>";
        let cases: Vec<(&str, String, &str, TextBox)> = vec![
            (
                "replaces an attribute, a fit mark and a style declaration",
                doc(
                    r#"<text id="t" x="5" y="50" inline-size="80" data-kerf-fit="shrink" style="fill:red; inline-size: 9px;opacity:.5">Hi</text>"#,
                ),
                "t",
                boxed(120.5, TextFit::Wrap),
            ),
            (
                "shrink",
                doc(r#"<text id="t" style="fill:red">Hi</text>"#),
                "t",
                boxed(300.0, TextFit::Shrink),
            ),
            (
                "no style yet",
                doc(r#"<text id="t" x="5" y="50"/>"#),
                "t",
                boxed(10.0, TextFit::Wrap),
            ),
            (
                "taken away",
                doc(r#"<text id="t" style="inline-size:10px !important" data-kerf-fit="shrink">a</text>"#),
                "t",
                TextBox::default(),
            ),
            (
                "taken away, others kept",
                doc(r#"<text id="t" style="inline-size:10px !important;fill:red">a</text>"#),
                "t",
                TextBox::default(),
            ),
            (
                "a sheet, none",
                doc(&format!(r#"{sheet}<text id="t" class="n">a</text>"#)),
                "t",
                TextBox::default(),
            ),
            (
                "a sheet, a box",
                doc(&format!(r#"{sheet}<text id="t" class="n">a</text>"#)),
                "t",
                boxed(90.0, TextFit::Wrap),
            ),
            (
                "a quoted semicolon",
                doc(r#"<text id="t" style="font-family:'A;B'; inline-size:9px">a</text>"#),
                "t",
                boxed(20.0, TextFit::Wrap),
            ),
            (
                "entities",
                doc(r#"<text id="t" style="font-family:&quot;A;B&quot;;fill:red;content:'&lt;&amp;&gt;'">a</text>"#),
                "t",
                boxed(20.0, TextFit::Wrap),
            ),
            (
                "single quotes around double ones",
                doc(r#"<text id="t" style='font-family:"A&apos;s";fill:red'>a</text>"#),
                "t",
                boxed(20.0, TextFit::Shrink),
            ),
            (
                "what the tokenizer cannot read",
                doc(r#"<text id="t" style="filter:drop-shadow(0 0 2px rgba(0,0,0,.5));bogus;--x:1px;/* ; */">a</text>"#),
                "t",
                boxed(33.333, TextFit::Wrap),
            ),
            (
                "a comment and spaces",
                doc(r#"<text id="t" style=" /* x */ ; ">a</text>"#),
                "t",
                TextBox::default(),
            ),
            (
                "not a text",
                doc(r#"<rect id="r" width="1" height="1"/>"#),
                "r",
                boxed(10.0, TextFit::Wrap),
            ),
            ("no such id", doc(r#"<text id="t">a</text>"#), "x", boxed(10.0, TextFit::Wrap)),
            (
                "a repeated id",
                doc(r#"<text id="d">b</text><text id="d">c</text>"#),
                "d",
                boxed(10.0, TextFit::Wrap),
            ),
            ("no width", doc(r#"<text id="t">a</text>"#), "t", boxed(0.0, TextFit::Wrap)),
            (
                "too wide",
                doc(r#"<text id="t">a</text>"#),
                "t",
                boxed(MAX_BOX_WIDTH * 2.0, TextFit::Wrap),
            ),
        ];
        let cases: Vec<serde_json::Value> = cases
            .into_iter()
            .map(|(name, svg, element, text_box)| {
                let expect = match set_text_box(&svg, element, &text_box) {
                    Ok(out) => serde_json::json!({ "ok": { "read": text_box_of(&out, element), "svg": out } }),
                    Err(e) => serde_json::json!({ "err": e.to_string() }),
                };
                serde_json::json!({ "name": name, "svg": svg, "element": element, "box": text_box, "expect": expect })
            })
            .collect();
        serde_json::json!({
            "about": "Written by kerf-core (svg/text_box.rs); replayed by text-box-corpus.test.ts. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- text_box_corpus",
            "cases": cases,
        })
    }

    #[test]
    fn text_box_corpus_is_what_this_engine_writes() {
        let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
        if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
            std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
            return;
        }
        let on_disk = std::fs::read_to_string(CORPUS_PATH)
            .expect("frontend/src/lib/fixtures/text-box-corpus.json — generate it with KERF_BLESS_CORPUS=1")
            .replace("\r\n", "\n");
        assert!(
            on_disk == fresh,
            "the text box corpus is stale: regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core \
             --no-default-features -- text_box_corpus, then run `bun test` — the replay says what withTextBox has to follow"
        );
    }

    #[test]
    fn the_box_is_read_whatever_the_rest_of_the_style_holds() {
        // simplecss stops at each of these; written first, the box is read before it.
        let glyphs = |svg: &str| {
            let out = super::super::outline::outline_text(svg, &crate::fonts::svg_fontdb()).unwrap();
            let at = out.svg.find("<g data-kerf-glyphs").expect("outlined");
            out.svg[at..].to_string()
        };
        for style in [
            "filter:drop-shadow(0 0 2px rgba(0,0,0,.5))",
            "bogus;fill:red",
            "--x:1px;fill:red",
        ] {
            let text = format!(
                r#"<text id="t" x="5" y="50" font-family="Geist" font-size="20" style="{style}">one two three four five six</text>"#
            );
            let set = set_text_box(&doc(&text), "t", &boxed(80.0, TextFit::Wrap)).unwrap();
            assert!(set.contains(r#"style="inline-size:80px !important;"#), "{set}");
            let attribute = doc(&text.replace(r#" style="#, r#" inline-size="80" style="#));
            assert_eq!(glyphs(&set), glyphs(&attribute), "{style}");
            let unwrapped = doc(&text);
            assert_ne!(glyphs(&set), glyphs(&unwrapped), "{style}: the box wraps the text");
        }
    }

    #[test]
    fn the_box_written_beats_a_style_rule() {
        let sheet = r#"<style>.n{inline-size:60px}</style>"#;
        let text = r#"<text id="t" class="n" x="5" y="50" font-family="Geist" font-size="20">one two three four five six</text>"#;
        let glyphs = |svg: &str| {
            let out = super::super::outline::outline_text(svg, &crate::fonts::svg_fontdb()).unwrap();
            let at = out.svg.find("<g data-kerf-glyphs").expect("outlined");
            out.svg[at..].to_string()
        };
        let set = set_text_box(&doc(&format!("{sheet}{text}")), "t", &boxed(300.0, TextFit::Wrap)).unwrap();
        let want = doc(&text.replace(r#" class="n""#, r#" inline-size="300""#));
        assert_eq!(glyphs(&set), glyphs(&want));
        // No box, with a rule that would give it one: `auto`.
        let none = set_text_box(&doc(&format!("{sheet}{text}")), "t", &TextBox::default()).unwrap();
        assert!(none.contains("inline-size:auto !important"), "{none}");
        assert_eq!(glyphs(&none), glyphs(&doc(&text.replace(r#" class="n""#, ""))));
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
            declarations(r#"font-family:"a;b"; inline-size:3px ;fill:url(#g;x);/* ; */"#),
            [r#"font-family:"a;b""#, " inline-size:3px ", "fill:url(#g;x)", "/* ; */"]
        );
    }
}
