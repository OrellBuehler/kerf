//! The four ready-made animated titles, as generated SVG + SMIL.
//!
//! A preset is *only* SVG the SMIL subset already understands — there is no preset renderer — so a
//! preset overlay is stored, previewed and exported exactly like a custom one. `render_preset` is
//! pure: `(kind, params, duration, frame)` in, one document out, its `viewBox` the delivery frame.
//! Every preset ends **blank and static** at `duration` (the last animation finishes there), so an
//! overlay whose span is the preset's duration leaves no frame half-faded.
//!
//! Text is not measured (the generator has no fonts); sizes are fractions of the frame height,
//! shrunk by character count to stay inside the frame, and the text is centred or anchored so a
//! different face only changes the width, never the layout.

use std::str::FromStr;

use serde::Serialize;

use super::values::fmt_num;
use crate::error::Result;
use crate::fonts::DEFAULT_FAMILY;
use crate::model::{SvgPresetKind, SvgPresetParams};
use crate::svg::invalid;

/// Full HD, the frame presets are drawn for when the project has no delivery format.
pub const DEFAULT_FRAME: (u32, u32) = (1920, 1080);

const MAX_TEXT_CHARS: usize = 200;
const MAX_FONT_CHARS: usize = 100;
const MAX_COLOR_CHARS: usize = 64;
const EASE_OUT: &str = "0.16 1 0.3 1";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SvgPresetParamInfo {
    pub name: &'static str,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SvgPresetInfo {
    pub kind: SvgPresetKind,
    pub label: &'static str,
    pub params: Vec<SvgPresetParamInfo>,
}

/// The presets and the parameters each takes (what `list_svg_presets` shows).
pub fn preset_infos() -> Vec<SvgPresetInfo> {
    let params = |subtitle: bool| {
        let mut p = vec![SvgPresetParamInfo {
            name: "text",
            required: true,
            default: None,
        }];
        if subtitle {
            p.push(SvgPresetParamInfo {
                name: "subtitle",
                required: false,
                default: None,
            });
        }
        p.extend([
            SvgPresetParamInfo {
                name: "color",
                required: false,
                default: Some("#ffffff"),
            },
            SvgPresetParamInfo {
                name: "accent",
                required: false,
                default: Some("#ffb000"),
            },
            SvgPresetParamInfo {
                name: "font",
                required: false,
                default: Some(DEFAULT_FAMILY),
            },
        ]);
        p
    };
    SvgPresetKind::ALL
        .iter()
        .map(|&kind| SvgPresetInfo {
            kind,
            label: kind.label(),
            params: params(kind != SvgPresetKind::WordPop),
        })
        .collect()
}

/// Whether `params` can make a preset: some text, colours SVG reads, bounded lengths.
pub fn check_params(params: &SvgPresetParams) -> Result<()> {
    if clean_text(&params.text).is_empty() {
        return Err(invalid("a preset needs some text"));
    }
    for (what, text) in [("text", Some(&params.text)), ("subtitle", params.subtitle.as_ref())] {
        if text.is_some_and(|t| t.chars().count() > MAX_TEXT_CHARS) {
            return Err(invalid(format!("{what} is limited to {MAX_TEXT_CHARS} characters")));
        }
    }
    for (what, color) in [("color", &params.color), ("accent", &params.accent)] {
        // A colour is a few characters; a long one is not a colour, and is not echoed back whole.
        let shown: String = color.chars().take(MAX_COLOR_CHARS).collect();
        if color.chars().count() > MAX_COLOR_CHARS
            || color.chars().any(char::is_control)
            || svgtypes::Color::from_str(color.trim()).is_err()
        {
            return Err(invalid(format!("{what} \"{shown}\" is not an SVG colour like #ffffff")));
        }
    }
    if params.font.as_ref().is_some_and(|f| f.chars().count() > MAX_FONT_CHARS) {
        return Err(invalid(format!("font is limited to {MAX_FONT_CHARS} characters")));
    }
    Ok(())
}

fn clean_text(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    flat.chars().take(MAX_TEXT_CHARS).collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn font_family(font: Option<&str>) -> String {
    let name: String = font
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '+' | '(' | ')'))
        .take(MAX_FONT_CHARS)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        DEFAULT_FAMILY.to_string()
    } else {
        name.to_string()
    }
}

/// ` fill="#rrggbb"` (+ ` fill-opacity` when translucent); an unreadable colour is the fallback.
fn fill(color: &str, fallback: &str) -> String {
    let c = svgtypes::Color::from_str(color.trim())
        .or_else(|_| svgtypes::Color::from_str(fallback))
        .unwrap_or_else(|_| svgtypes::Color::white());
    let mut s = format!(" fill=\"#{:02x}{:02x}{:02x}\"", c.red, c.green, c.blue);
    if c.alpha < 255 {
        s.push_str(&format!(" fill-opacity=\"{}\"", fmt_num(f64::from(c.alpha) / 255.0)));
    }
    s
}

/// A font size that keeps `chars` characters of ~0.56 em inside `max_w`.
fn fit(chars: usize, size: f64, max_w: f64) -> f64 {
    let est = chars.max(1) as f64 * 0.56 * size;
    if est > max_w {
        size * max_w / est
    } else {
        size
    }
}

struct Ctx {
    w: f64,
    h: f64,
    dur: f64,
    font: String,
    color: String,
    accent: String,
    text: String,
    subtitle: Option<String>,
}

/// The document for `kind` filled with `params`, lasting `duration` seconds, drawn for `frame`.
pub fn render_preset(kind: SvgPresetKind, params: &SvgPresetParams, duration: f64, frame: (u32, u32)) -> String {
    let (w, h) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
    let ctx = Ctx {
        w,
        h,
        dur: if duration.is_finite() { duration.max(0.2) } else { 4.0 },
        font: esc(&font_family(params.font.as_deref())),
        color: fill(&params.color, "#ffffff"),
        accent: fill(&params.accent, "#ffb000"),
        text: clean_text(&params.text),
        subtitle: params.subtitle.as_deref().map(clean_text).filter(|s| !s.is_empty()),
    };
    let body = match kind {
        SvgPresetKind::FadeUp => fade_up(&ctx),
        SvgPresetKind::Typewriter => typewriter(&ctx),
        SvgPresetKind::LowerThird => lower_third(&ctx),
        SvgPresetKind::WordPop => word_pop(&ctx),
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">{body}</svg>",
        w = fmt_num(w),
        h = fmt_num(h),
    )
}

fn secs(v: f64) -> String {
    format!("{}s", fmt_num(v))
}

/// `<animate>` of one attribute from `from` to `to`, held afterwards.
fn animate(attr: &str, from: &str, to: &str, begin: f64, dur: f64, eased: bool) -> String {
    let ease = if eased {
        format!(" calcMode=\"spline\" keyTimes=\"0;1\" keySplines=\"{EASE_OUT}\"")
    } else {
        String::new()
    };
    format!(
        "<animate attributeName=\"{attr}\" from=\"{from}\" to=\"{to}\" begin=\"{}\" dur=\"{}\" fill=\"freeze\"{ease}/>",
        secs(begin),
        secs(dur)
    )
}

/// One line of text: where, how big, in what paint.
struct Line<'a> {
    x: f64,
    y: f64,
    size: f64,
    bold: bool,
    paint: &'a str,
    anchor: &'a str,
    extra: &'a str,
}

impl<'a> Line<'a> {
    fn new(x: f64, y: f64, size: f64, paint: &'a str) -> Self {
        Self {
            x,
            y,
            size,
            bold: false,
            paint,
            anchor: "middle",
            extra: "",
        }
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn start(mut self) -> Self {
        self.anchor = "start";
        self
    }

    fn extra(mut self, extra: &'a str) -> Self {
        self.extra = extra;
        self
    }

    /// `<text>` around `inner` (escaped text, `<tspan>`s, animations).
    fn render(&self, ctx: &Ctx, inner: &str) -> String {
        format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"{}\" font-family=\"{}\" font-size=\"{}\" font-weight=\"{}\"{}{}>{inner}</text>",
            fmt_num(self.x),
            fmt_num(self.y),
            self.anchor,
            ctx.font,
            fmt_num(self.size),
            if self.bold { "700" } else { "400" },
            self.paint,
            self.extra,
        )
    }
}

fn fade_up(ctx: &Ctx) -> String {
    let (w, h, dur) = (ctx.w, ctx.h, ctx.dur);
    let size = fit(ctx.text.chars().count(), 0.075 * h, 0.88 * w);
    let fin = (0.6f64).min(0.4 * dur);
    let fout = (0.4f64).min(0.3 * dur);
    let mut out = String::from("<g opacity=\"0\">");
    out.push_str(&animate("opacity", "0", "1", 0.0, fin, true));
    out.push_str(&animate("opacity", "1", "0", dur - fout, fout, false));
    out.push_str(&format!(
        "<animateTransform attributeName=\"transform\" type=\"translate\" from=\"0 {}\" to=\"0 0\" begin=\"0s\" dur=\"{}\" fill=\"freeze\" calcMode=\"spline\" keyTimes=\"0;1\" keySplines=\"{EASE_OUT}\"/>",
        fmt_num(0.04 * h),
        secs(fin)
    ));
    let base = if ctx.subtitle.is_some() { 0.47 * h } else { 0.5 * h } + 0.35 * size;
    out.push_str(&Line::new(0.5 * w, base, size, &ctx.color).bold().render(ctx, &esc(&ctx.text)));
    if let Some(sub) = &ctx.subtitle {
        let sub_size = fit(sub.chars().count(), 0.4 * size, 0.88 * w);
        out.push_str(&Line::new(0.5 * w, base + sub_size * 1.7, sub_size, &ctx.accent).render(ctx, &esc(sub)));
    }
    out.push_str("</g>");
    out
}

fn typewriter(ctx: &Ctx) -> String {
    let (w, h, dur) = (ctx.w, ctx.h, ctx.dur);
    let chars: Vec<char> = ctx.text.chars().collect();
    let n = chars.len().max(1);
    let size = fit(n, 0.07 * h, 0.9 * w);
    let first = (0.25f64).min(0.1 * dur);
    let typing = (0.06 * n as f64).min(0.6 * dur);
    let fout = (0.4f64).min(0.3 * dur);
    let mut out = String::from("<g opacity=\"1\">");
    out.push_str(&animate("opacity", "1", "0", dur - fout, fout, false));
    let base = if ctx.subtitle.is_some() { 0.47 * h } else { 0.5 * h } + 0.35 * size;
    let mut spans = String::new();
    for (i, c) in chars.iter().enumerate() {
        let at = first + typing * i as f64 / n as f64;
        spans.push_str(&format!(
            "<tspan visibility=\"hidden\"><set attributeName=\"visibility\" to=\"visible\" begin=\"{}\"/>{}</tspan>",
            secs(at),
            esc(&c.to_string())
        ));
    }
    out.push_str(&Line::new(0.5 * w, base, size, &ctx.color).render(ctx, &spans));
    if let Some(sub) = &ctx.subtitle {
        let sub_size = fit(sub.chars().count(), 0.45 * size, 0.9 * w);
        let fade = animate("opacity", "0", "1", first + typing, (0.4f64).min(0.2 * dur), false);
        out.push_str(
            &Line::new(0.5 * w, base + sub_size * 1.7, sub_size, &ctx.accent)
                .extra(" opacity=\"0\"")
                .render(ctx, &format!("{}{fade}", esc(sub))),
        );
    }
    out.push_str("</g>");
    out
}

fn lower_third(ctx: &Ctx) -> String {
    let (w, h, dur) = (ctx.w, ctx.h, ctx.dur);
    let k = (dur / 3.0).min(1.0);
    let x0 = 0.06 * w;
    let name_size = fit(ctx.text.chars().count(), 0.052 * h, w - 2.0 * x0);
    let bar_h = (0.006 * h).max(2.0);
    let bar_w = (0.3 * w).min(w - 2.0 * x0);
    let top = 0.74 * h;
    let base = top + bar_h + 0.018 * h + 0.8 * name_size;
    let clip_w = fmt_num(w - x0 + 0.02 * w);
    let mut out = String::new();
    out.push_str(&format!(
        "<defs><clipPath id=\"kerf-wipe\"><rect x=\"{}\" y=\"{}\" width=\"0\" height=\"{}\">{}{}</rect></clipPath></defs>",
        fmt_num(x0 - 0.01 * w),
        fmt_num(base - name_size),
        fmt_num(name_size * 1.6),
        animate("width", "0", &clip_w, 0.2 * k, 0.6 * k, true),
        animate("width", &clip_w, "0", dur - 0.75 * k, 0.5 * k, false),
    ));
    let bar_w = fmt_num(bar_w);
    out.push_str(&format!(
        "<rect x=\"{}\" y=\"{}\" width=\"0\" height=\"{}\"{}>{}{}</rect>",
        fmt_num(x0),
        fmt_num(top),
        fmt_num(bar_h),
        ctx.accent,
        animate("width", "0", &bar_w, 0.0, 0.45 * k, true),
        animate("width", &bar_w, "0", dur - 0.45 * k, 0.45 * k, false),
    ));
    out.push_str(
        &Line::new(x0, base, name_size, &ctx.color)
            .bold()
            .start()
            .extra(" clip-path=\"url(#kerf-wipe)\"")
            .render(ctx, &esc(&ctx.text)),
    );
    if let Some(sub) = &ctx.subtitle {
        let sub_size = fit(sub.chars().count(), 0.032 * h, w - 2.0 * x0);
        let fades = format!(
            "{}{}",
            animate("opacity", "0", "1", 0.6 * k, 0.4 * k, false),
            animate("opacity", "1", "0", dur - 0.9 * k, 0.3 * k, false)
        );
        out.push_str(
            &Line::new(x0, base + 0.012 * h + 1.1 * sub_size, sub_size, &ctx.color)
                .start()
                .extra(" opacity=\"0\"")
                .render(ctx, &format!("{}{fades}", esc(sub))),
        );
    }
    out
}

fn word_pop(ctx: &Ctx) -> String {
    let (w, h, dur) = (ctx.w, ctx.h, ctx.dur);
    let words: Vec<&str> = ctx.text.split(' ').filter(|s| !s.is_empty()).take(60).collect();
    let n = words.len().max(1);
    let longest = words.iter().map(|s| s.chars().count()).max().unwrap_or(1);
    let size = fit(longest, 0.14 * h, 0.9 * w);
    let first = (0.1f64).min(0.1 * dur);
    let slot = ((dur - first) / n as f64).min(0.7);
    let pop = (0.22f64).min(slot);
    let fout = (0.3f64).min(0.3 * dur);
    let outline = format!(
        " stroke=\"#000000\" stroke-opacity=\"0.35\" stroke-width=\"{}\" paint-order=\"stroke\"",
        fmt_num(size * 0.06)
    );
    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        let at = first + slot * i as f64;
        let last = i + 1 == words.len();
        let paint = if last && words.len() > 1 { &ctx.accent } else { &ctx.color };
        // the last word stays up to the end and fades out there; the others make way for the next
        let (shown, fade) = if last {
            (dur - at, animate("opacity", "1", "0", dur - fout, fout, false))
        } else {
            (slot, String::new())
        };
        out.push_str(&format!(
            "<g transform=\"translate({} {})\" visibility=\"hidden\"><set attributeName=\"visibility\" to=\"visible\" begin=\"{}\" dur=\"{}\"/><animateTransform attributeName=\"transform\" type=\"scale\" values=\"0.6;1.12;1\" keyTimes=\"0;0.6;1\" begin=\"{}\" dur=\"{}\" additive=\"sum\"/>{fade}",
            fmt_num(0.5 * w),
            fmt_num(0.5 * h),
            secs(at),
            secs(shown),
            secs(at),
            secs(pop),
        ));
        out.push_str(
            &Line::new(0.0, 0.35 * size, size, paint)
                .bold()
                .extra(&outline)
                .render(ctx, &esc(word)),
        );
        out.push_str("</g>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svg::SvgDoc;

    const FRAMES: [(u32, u32); 3] = [(1920, 1080), (1080, 1920), (1080, 1080)];
    const DURATIONS: [f64; 5] = [0.3, 1.0, 2.5, 4.0, 30.0];

    fn params(subtitle: bool) -> SvgPresetParams {
        let mut p = SvgPresetParams::new("Hello & <welcome> to \"Kerf\"");
        if subtitle {
            p.subtitle = Some("A tool for cutting".to_string());
        }
        p
    }

    fn draw(svg: &str) -> tiny_skia::Pixmap {
        let mut opt = resvg::usvg::Options::default();
        let db = opt.fontdb_mut();
        db.load_font_data(include_bytes!("../../assets/fonts/Geist-Regular.ttf").to_vec());
        db.load_font_data(include_bytes!("../../assets/fonts/Geist-Bold.ttf").to_vec());
        let tree = resvg::usvg::Tree::from_str(svg, &opt).unwrap_or_else(|e| panic!("{e}: {svg}"));
        let size = tree.size().to_int_size();
        let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height()).unwrap();
        resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
        pixmap
    }

    fn ink(svg: &str) -> usize {
        draw(svg).pixels().iter().filter(|p| p.alpha() > 0).count()
    }

    #[test]
    fn colours_are_short_and_plain() {
        let with = |color: &str, accent: &str| SvgPresetParams {
            color: color.to_string(),
            accent: accent.to_string(),
            ..SvgPresetParams::new("Hi")
        };
        for ok in ["#fff", "#ffffff", "white", "rgb(10, 20, 30)", "rgba(0,0,0,0.5)", "  #abc  "] {
            assert!(check_params(&with(ok, "#e0a020")).is_ok(), "{ok}");
        }
        // Sixty-four characters is the most a colour may be; one more is refused, however it parses.
        let spaced = format!("#fff{}", " ".repeat(MAX_COLOR_CHARS - 4));
        assert_eq!(spaced.chars().count(), MAX_COLOR_CHARS);
        assert!(check_params(&with(&spaced, "#fff")).is_ok());
        let long = format!("{spaced} ");
        for (color, accent) in [(long.as_str(), "#fff"), ("#fff", long.as_str())] {
            let e = check_params(&with(color, accent)).unwrap_err().to_string();
            assert!(e.contains("is not an SVG colour"), "{e}");
            assert!(e.len() < 200, "a long value is not echoed whole: {} bytes", e.len());
        }
        for bad in ["#ff\u{0}f", "#fff\n", "red\u{7}", "not a colour", "url(#a)", "", "#ggg"] {
            assert!(check_params(&with(bad, "#fff")).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn every_preset_parses_with_no_warnings() {
        for kind in SvgPresetKind::ALL {
            for subtitle in [false, true] {
                for frame in FRAMES {
                    for dur in DURATIONS {
                        let svg = render_preset(kind, &params(subtitle), dur, frame);
                        let doc = SvgDoc::parse(&svg).unwrap_or_else(|e| panic!("{kind:?} {frame:?} {dur}: {e}\n{svg}"));
                        assert!(doc.warnings().is_empty(), "{kind:?} {frame:?} {dur}: {:?}", doc.warnings());
                        assert!(doc.is_animated(), "{kind:?}");
                        assert_eq!(doc.sanitized(), svg, "{kind:?}: a preset is already sanitised");
                        assert!(svg.contains(&format!("viewBox=\"0 0 {} {}\"", frame.0, frame.1)));
                    }
                }
            }
        }
    }

    #[test]
    fn every_preset_is_static_once_it_ends() {
        for kind in SvgPresetKind::ALL {
            for dur in DURATIONS {
                let doc = SvgDoc::parse(&render_preset(kind, &params(true), dur, FRAMES[0])).unwrap();
                let end = doc.at(dur);
                for later in [dur + 0.001, dur + 1.0, dur * 3.0, 1000.0] {
                    assert_eq!(doc.at(later), end, "{kind:?} dur {dur} @ {later}");
                }
                let natural = doc.natural_duration().expect("a preset settles");
                assert!(natural <= dur + 1e-6, "{kind:?} dur {dur}: settles at {natural}");
                assert!(natural >= 0.5 * dur, "{kind:?} dur {dur}: settles at {natural}");
                assert_ne!(doc.at(0.0), doc.at(dur * 0.5), "{kind:?} dur {dur} moves");
            }
        }
    }

    #[test]
    fn presets_draw_something_in_the_middle_and_nothing_at_the_ends() {
        let frame = (320, 180);
        for kind in SvgPresetKind::ALL {
            let doc = SvgDoc::parse(&render_preset(kind, &params(true), 4.0, frame)).unwrap();
            assert_eq!(ink(&doc.at(4.0)), 0, "{kind:?} ends blank");
            assert_eq!(ink(&doc.at(100.0)), 0, "{kind:?} stays blank");
            let mid = ink(&doc.at(2.0));
            assert!(mid > 200, "{kind:?} shows something at 2 s: {mid} px");
        }
    }

    #[test]
    fn the_start_of_each_preset_is_empty_or_just_beginning() {
        let frame = (320, 180);
        for kind in [
            SvgPresetKind::FadeUp,
            SvgPresetKind::Typewriter,
            SvgPresetKind::LowerThird,
            SvgPresetKind::WordPop,
        ] {
            let doc = SvgDoc::parse(&render_preset(kind, &params(true), 4.0, frame)).unwrap();
            assert_eq!(ink(&doc.at(0.0)), 0, "{kind:?} opens empty");
        }
    }

    #[test]
    fn typewriter_adds_letters_over_time() {
        let doc = SvgDoc::parse(&render_preset(
            SvgPresetKind::Typewriter,
            &SvgPresetParams::new("Typing away here"),
            6.0,
            (640, 360),
        ))
        .unwrap();
        let counts: Vec<usize> = (0..=10).map(|i| ink(&doc.at(0.1 + 0.2 * f64::from(i)))).collect();
        assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
        assert!(counts[10] > 3 * counts[1].max(1), "{counts:?}");
        assert_eq!(counts[0], 0, "{counts:?}");
        let held = ink(&doc.at(4.0));
        assert!(held >= counts[10], "the full line is held: {held} vs {counts:?}");
    }

    #[test]
    fn lower_third_wipes_in_and_out() {
        let p = SvgPresetParams::new("Jane Smith");
        let doc = SvgDoc::parse(&render_preset(SvgPresetKind::LowerThird, &p, 6.0, (640, 360))).unwrap();
        let early = ink(&doc.at(0.3));
        let held = ink(&doc.at(3.0));
        let leaving = ink(&doc.at(5.6));
        assert!(early > 0 && early < held, "{early} < {held}");
        assert!(leaving < held, "{leaving} < {held}");
    }

    #[test]
    fn word_pop_shows_one_word_at_a_time() {
        let p = SvgPresetParams::new("one two three");
        let svg = render_preset(SvgPresetKind::WordPop, &p, 3.0, (640, 360));
        let doc = SvgDoc::parse(&svg).unwrap();
        let word_at = |t: f64| {
            let out = doc.at(t);
            let parsed = roxmltree::Document::parse(&out).unwrap();
            parsed
                .descendants()
                .filter(|n| n.has_tag_name("g") && n.attribute("style").is_some_and(|s| s.contains("visibility:visible")))
                .count()
        };
        assert_eq!(word_at(0.5), 1);
        assert_eq!(word_at(1.2), 1);
        assert_eq!(word_at(2.6), 1);
        let pops = |t: f64| ink(&doc.at(t));
        assert!(pops(0.12) > 0 && pops(0.9) > 0 && pops(1.8) > 0);
    }

    #[test]
    fn text_is_escaped_and_bounded() {
        let mut p = SvgPresetParams::new("<script>alert(1)</script> & \"quotes\" 'x'\n\ttabs");
        p.subtitle = Some("<b>sub</b>".to_string());
        for kind in SvgPresetKind::ALL {
            let svg = render_preset(kind, &p, 3.0, FRAMES[0]);
            assert!(!svg.contains("<script") && !svg.contains("<b>"), "{kind:?}");
            let doc = SvgDoc::parse(&svg).unwrap();
            assert!(doc.warnings().is_empty());
            assert!(!svg.contains('\n') && !svg.contains('\t'), "whitespace collapsed");
        }
        let long = "w".repeat(500);
        let svg = render_preset(SvgPresetKind::FadeUp, &SvgPresetParams::new(long), 3.0, FRAMES[0]);
        assert!(svg.contains(&"w".repeat(200)) && !svg.contains(&"w".repeat(201)));
    }

    #[test]
    fn long_text_shrinks_to_the_frame() {
        let text = "This is a rather long headline that would not fit";
        let size_of = |text: &str, frame: (u32, u32)| {
            let svg = render_preset(SvgPresetKind::FadeUp, &SvgPresetParams::new(text), 3.0, frame);
            let i = svg.find("font-size=\"").unwrap() + 11;
            svg[i..svg[i..].find('"').unwrap() + i].parse::<f64>().unwrap()
        };
        let vertical = size_of(text, (1080, 1920));
        assert!(vertical < 0.075 * 1920.0, "{vertical}");
        assert!(vertical * text.len() as f64 * 0.56 <= 0.88 * 1080.0 + 1e-3);
        assert_eq!(
            size_of("Short title", (1920, 1080)),
            0.075 * 1080.0,
            "a roomy frame keeps the full size"
        );
    }

    #[test]
    fn font_and_colours_are_made_safe() {
        let mut p = SvgPresetParams::new("x");
        p.font = Some("Bad\" onload=\"x; Font <>".to_string());
        p.color = "not a colour".to_string();
        p.accent = "#ff000080".to_string();
        p.subtitle = Some("s".to_string());
        let svg = render_preset(SvgPresetKind::FadeUp, &p, 3.0, FRAMES[0]);
        assert!(svg.contains("font-family=\"Bad onloadx Font\""), "{svg}");
        assert!(svg.contains("fill=\"#ffffff\""), "an unreadable colour falls back");
        assert!(svg.contains("fill=\"#ff0000\" fill-opacity=\"0.501961\""), "{svg}");
        assert!(SvgDoc::parse(&svg).unwrap().warnings().is_empty());
        p.font = None;
        assert!(render_preset(SvgPresetKind::WordPop, &p, 3.0, FRAMES[0]).contains(&format!("font-family=\"{DEFAULT_FAMILY}\"")));
        p.font = Some("   ".to_string());
        assert!(render_preset(SvgPresetKind::WordPop, &p, 3.0, FRAMES[0]).contains(DEFAULT_FAMILY));
    }

    #[test]
    fn parameters_are_checked() {
        let ok = SvgPresetParams::new("Title");
        assert!(check_params(&ok).is_ok());
        for bad in [
            SvgPresetParams::new(""),
            SvgPresetParams::new(" \n\t "),
            SvgPresetParams::new("x".repeat(201)),
            SvgPresetParams {
                color: "chartreuse-ish".into(),
                ..ok.clone()
            },
            SvgPresetParams {
                accent: String::new(),
                ..ok.clone()
            },
            SvgPresetParams {
                subtitle: Some("y".repeat(201)),
                ..ok.clone()
            },
            SvgPresetParams {
                font: Some("f".repeat(101)),
                ..ok.clone()
            },
        ] {
            assert!(check_params(&bad).is_err(), "{bad:?}");
        }
        assert!(check_params(&SvgPresetParams {
            color: "rgb(1,2,3)".into(),
            accent: "teal".into(),
            ..ok
        })
        .is_ok());
    }

    #[test]
    fn the_catalogue_lists_every_kind() {
        let infos = preset_infos();
        assert_eq!(infos.iter().map(|i| i.kind).collect::<Vec<_>>(), SvgPresetKind::ALL);
        assert!(infos.iter().all(|i| i.params[0].name == "text" && i.params[0].required));
        let word_pop = infos.iter().find(|i| i.kind == SvgPresetKind::WordPop).unwrap();
        assert!(word_pop.params.iter().all(|p| p.name != "subtitle"));
        let json = serde_json::to_value(&infos[0]).unwrap();
        assert_eq!(json["kind"], "fade_up");
        assert_eq!(json["label"], "Fade up");
        assert_eq!(json["params"][0], serde_json::json!({"name": "text", "required": true}));
    }

    #[test]
    fn a_degenerate_duration_or_frame_still_makes_a_document() {
        for dur in [0.0, -3.0, f64::NAN, f64::INFINITY, 1e-9] {
            for frame in [(0, 0), (1, 1), (7680, 4320)] {
                for kind in SvgPresetKind::ALL {
                    let svg = render_preset(kind, &params(true), dur, frame);
                    assert!(SvgDoc::parse(&svg).is_ok(), "{kind:?} {dur} {frame:?}");
                }
            }
        }
    }

    const FIXTURE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/fixtures/svg-presets.json"
    );

    /// What the browser harness serves for the preset commands: each preset drawn once for the
    /// default frame and length with placeholder text, and the catalogue.
    fn fixture() -> serde_json::Value {
        let (text, subtitle) = ("Your title", "Your subtitle");
        let mut presets = serde_json::Map::new();
        for kind in SvgPresetKind::ALL {
            let mut params = SvgPresetParams::new(text);
            let mut entry = serde_json::Map::new();
            entry.insert("plain".into(), render_preset(kind, &params, 4.0, DEFAULT_FRAME).into());
            if kind != SvgPresetKind::WordPop {
                params.subtitle = Some(subtitle.to_string());
                entry.insert("subtitled".into(), render_preset(kind, &params, 4.0, DEFAULT_FRAME).into());
            }
            presets.insert(kind.as_str().into(), entry.into());
        }
        let defaults = SvgPresetParams::new("");
        serde_json::json!({
            "about": "Written by kerf-core (svg/presets.rs); served by the browser harness. Regenerate: KERF_BLESS_SVG_PRESETS=1 cargo test -p kerf-core --no-default-features -- svg_preset_fixture",
            "frame": [DEFAULT_FRAME.0, DEFAULT_FRAME.1],
            "duration": 4.0,
            "text": text,
            "subtitle": subtitle,
            "defaults": {"color": defaults.color, "accent": defaults.accent, "font": DEFAULT_FAMILY},
            "infos": preset_infos(),
            "presets": presets,
        })
    }

    #[test]
    fn svg_preset_fixture_is_what_this_engine_does() {
        let fresh = format!("{}\n", serde_json::to_string_pretty(&fixture()).expect("serializes"));
        if std::env::var_os("KERF_BLESS_SVG_PRESETS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(FIXTURE_PATH).parent().expect("a directory")).expect("fixtures dir");
            std::fs::write(FIXTURE_PATH, &fresh).expect("writes the fixture");
            return;
        }
        let on_disk = std::fs::read_to_string(FIXTURE_PATH)
            .expect("frontend/src/lib/fixtures/svg-presets.json — generate it with KERF_BLESS_SVG_PRESETS=1")
            .replace("\r\n", "\n");
        assert!(
            on_disk == fresh,
            "the preset fixture is stale: regenerate with KERF_BLESS_SVG_PRESETS=1 cargo test -p kerf-core \
             --no-default-features -- svg_preset_fixture, then run `bun test` in frontend/"
        );
    }
}
