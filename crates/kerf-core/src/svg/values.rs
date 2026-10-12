//! The attribute values the SMIL subset interpolates, and how they print back.
//!
//! A value is parsed by what its attribute *is* (`kind_of`), never by sniffing: `class="1"` is a name,
//! not a number to blend. Anything that does not parse as its kind, or does not pair with the value
//! next to it (`10px` with `5%`, a path with a different command sequence), is `Value::Text` /
//! non-interpolable and the animation steps between the two instead of blending them.

use std::str::FromStr;

pub(super) type Unit = &'static str;

const UNITS: [&str; 13] = [
    "%", "px", "em", "ex", "pt", "pc", "cm", "mm", "in", "deg", "rad", "grad", "turn",
];

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    Len(f64, Unit),
    /// Red, green, blue in `0..=255`, alpha in `0..=1`.
    Color([f64; 4]),
    List(Vec<f64>),
    Path(Vec<PathCmd>),
    /// A transform list as text; only ever replaced or appended to.
    Transform(String),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PathCmd {
    pub op: char,
    pub args: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Len,
    Color,
    List,
    Path,
    Discrete,
}

pub(super) fn kind_of(attr: &str) -> Kind {
    match attr {
        "fill" | "stroke" | "stop-color" | "flood-color" | "lighting-color" | "color" => Kind::Color,
        "d" => Kind::Path,
        "viewBox" | "points" | "stroke-dasharray" | "values" | "tableValues" | "kernelMatrix" | "baseFrequency"
        | "stdDeviation" => Kind::List,
        "visibility"
        | "display"
        | "href"
        | "class"
        | "id"
        | "result"
        | "in"
        | "in2"
        | "type"
        | "mode"
        | "operator"
        | "font-family"
        | "font-style"
        | "font-variant"
        | "text-anchor"
        | "text-decoration"
        | "text-rendering"
        | "shape-rendering"
        | "image-rendering"
        | "color-interpolation"
        | "color-interpolation-filters"
        | "stroke-linecap"
        | "stroke-linejoin"
        | "fill-rule"
        | "clip-rule"
        | "paint-order"
        | "mix-blend-mode"
        | "isolation"
        | "overflow"
        | "preserveAspectRatio"
        | "clip-path"
        | "mask"
        | "filter"
        | "marker-start"
        | "marker-mid"
        | "marker-end"
        | "direction"
        | "writing-mode"
        | "dominant-baseline"
        | "alignment-baseline"
        | "spreadMethod"
        | "gradientUnits"
        | "patternUnits"
        | "clipPathUnits"
        | "maskUnits"
        | "maskContentUnits"
        | "primitiveUnits"
        | "filterUnits"
        | "lengthAdjust"
        | "method"
        | "spacing"
        | "startOffset"
        | "xml:space" => Kind::Discrete,
        _ => Kind::Len,
    }
}

/// The CSS properties an animation has to write through `style`: a rule in a `<style>` element beats a
/// presentation attribute, an inline declaration beats the rule, so only the last of these is certain
/// to be seen.
pub(super) fn is_css_property(attr: &str) -> bool {
    matches!(
        attr,
        "opacity"
            | "fill"
            | "fill-opacity"
            | "fill-rule"
            | "stroke"
            | "stroke-width"
            | "stroke-opacity"
            | "stroke-dasharray"
            | "stroke-dashoffset"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-miterlimit"
            | "stop-color"
            | "stop-opacity"
            | "flood-color"
            | "flood-opacity"
            | "lighting-color"
            | "color"
            | "font-size"
            | "font-family"
            | "font-weight"
            | "font-style"
            | "letter-spacing"
            | "word-spacing"
            | "text-anchor"
            | "text-decoration"
            | "visibility"
            | "display"
            | "clip-path"
            | "mask"
            | "filter"
            | "paint-order"
            | "mix-blend-mode"
            | "clip-rule"
            | "dominant-baseline"
            | "marker-start"
            | "marker-mid"
            | "marker-end"
    )
}

/// What an attribute is when the element does not say.
pub(super) fn default_text(attr: &str) -> Option<&'static str> {
    Some(match attr {
        "opacity" | "fill-opacity" | "stroke-opacity" | "stop-opacity" | "flood-opacity" => "1",
        "x" | "y" | "cx" | "cy" | "fx" | "fy" | "x1" | "y1" | "x2" | "y2" | "dx" | "dy" | "r" | "rx" | "ry" | "offset"
        | "stroke-dashoffset" | "letter-spacing" | "word-spacing" => "0",
        "stroke-width" => "1",
        "stroke-miterlimit" => "4",
        "fill" | "stop-color" | "color" => "#000000",
        "stroke" => "none",
        "visibility" => "visible",
        "display" => "inline",
        "font-size" => "16",
        _ => return None,
    })
}

pub(super) fn parse(kind: Kind, s: &str) -> Value {
    let s = s.trim();
    match kind {
        Kind::Len => {
            if let Some((v, u)) = parse_len(s) {
                Value::Len(v, u)
            } else if let Some(list) = parse_list(s).filter(|l| l.len() > 1) {
                Value::List(list)
            } else {
                Value::Text(s.to_string())
            }
        }
        Kind::Color => match svgtypes::Color::from_str(s) {
            Ok(c) => Value::Color([c.red.into(), c.green.into(), c.blue.into(), f64::from(c.alpha) / 255.0]),
            Err(_) => Value::Text(s.to_string()),
        },
        Kind::List => parse_list(s).map_or_else(|| Value::Text(s.to_string()), Value::List),
        Kind::Path => parse_path(s).map_or_else(|| Value::Text(s.to_string()), Value::Path),
        Kind::Discrete => Value::Text(s.to_string()),
    }
}

/// Canonical text: numbers trimmed to at most six decimals, colours as `#rrggbb` (`rgba()` when not opaque).
pub(super) fn format(v: &Value) -> String {
    match v {
        Value::Len(n, u) => format!("{}{u}", fmt_num(*n)),
        Value::Color(c) => fmt_color(*c),
        Value::List(l) => l.iter().map(|n| fmt_num(*n)).collect::<Vec<_>>().join(" "),
        Value::Path(p) => fmt_path(p),
        Value::Transform(t) | Value::Text(t) => t.clone(),
    }
}

pub(super) fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let r = (v * 1e6).round() / 1e6;
    if r == 0.0 {
        return "0".to_string();
    }
    let mut s = format!("{r:.6}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

fn fmt_color(c: [f64; 4]) -> String {
    let ch = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    let (r, g, b) = (ch(c[0]), ch(c[1]), ch(c[2]));
    let a = c[3].clamp(0.0, 1.0);
    if a >= 0.9995 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("rgba({r},{g},{b},{})", fmt_num((a * 1000.0).round() / 1000.0))
    }
}

fn fmt_path(p: &[PathCmd]) -> String {
    let mut out = String::new();
    for c in p {
        out.push(c.op);
        for (i, a) in c.args.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&fmt_num(*a));
        }
    }
    out
}

// ---- scanning -------------------------------------------------------------

/// The end of the number starting at `i`, or `None` when there is none there.
fn scan_number(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    if matches!(b.get(j), Some(b'+' | b'-')) {
        j += 1;
    }
    let digits = |mut k: usize| {
        let s = k;
        while b.get(k).is_some_and(u8::is_ascii_digit) {
            k += 1;
        }
        (k, k > s)
    };
    let (k, int) = digits(j);
    j = k;
    let mut frac = false;
    if b.get(j) == Some(&b'.') {
        let (k, f) = digits(j + 1);
        if f || int {
            j = k;
            frac = f;
        }
    }
    if !int && !frac {
        return None;
    }
    if matches!(b.get(j), Some(b'e' | b'E')) {
        let mut k = j + 1;
        if matches!(b.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let (k, e) = digits(k);
        if e {
            j = k;
        }
    }
    Some(j)
}

pub(super) fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    let end = scan_number(s.as_bytes(), 0)?;
    (end == s.len()).then(|| s.parse::<f64>().ok().filter(|v| v.is_finite()))?
}

fn parse_len(s: &str) -> Option<(f64, Unit)> {
    let end = scan_number(s.as_bytes(), 0)?;
    let v = s[..end].parse::<f64>().ok().filter(|v| v.is_finite())?;
    let unit = s[end..].trim();
    if unit.is_empty() {
        return Some((v, ""));
    }
    UNITS.iter().find(|u| u.eq_ignore_ascii_case(unit)).map(|u| (v, *u))
}

pub(super) fn parse_list(s: &str) -> Option<Vec<f64>> {
    let mut out = Vec::new();
    for part in s
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|p| !p.is_empty())
    {
        out.push(parse_number(part)?);
        if out.len() > 4096 {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

fn arg_count(op: char) -> Option<usize> {
    Some(match op.to_ascii_uppercase() {
        'M' | 'L' | 'T' => 2,
        'H' | 'V' => 1,
        'C' => 6,
        'S' | 'Q' => 4,
        'A' => 7,
        'Z' => 0,
        _ => return None,
    })
}

fn skip_sep(b: &[u8], i: &mut usize) {
    while b.get(*i).is_some_and(u8::is_ascii_whitespace) {
        *i += 1;
    }
    if b.get(*i) == Some(&b',') {
        *i += 1;
        while b.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
    }
}

fn parse_path(s: &str) -> Option<Vec<PathCmd>> {
    let b = s.as_bytes();
    let (mut i, mut cur, mut owed) = (0, None::<char>, false);
    let mut out: Vec<PathCmd> = Vec::new();
    loop {
        skip_sep(b, &mut i);
        let Some(&c) = b.get(i) else { break };
        if c.is_ascii_alphabetic() {
            let op = c as char;
            let n = arg_count(op)?;
            i += 1;
            if n == 0 {
                out.push(PathCmd { op, args: Vec::new() });
                cur = None;
            } else {
                cur = Some(op);
                owed = true;
            }
            continue;
        }
        let op = cur?;
        let n = arg_count(op)?;
        let mut args = Vec::with_capacity(n);
        for k in 0..n {
            skip_sep(b, &mut i);
            if matches!(op, 'A' | 'a') && (k == 3 || k == 4) {
                let f = match b.get(i)? {
                    b'0' => 0.0,
                    b'1' => 1.0,
                    _ => return None,
                };
                i += 1;
                args.push(f);
            } else {
                let end = scan_number(b, i)?;
                args.push(s[i..end].parse::<f64>().ok().filter(|v| v.is_finite())?);
                i = end;
            }
        }
        out.push(PathCmd { op, args });
        owed = false;
        cur = match op {
            'M' => Some('L'),
            'm' => Some('l'),
            o => Some(o),
        };
        if out.len() > 20_000 {
            return None;
        }
    }
    (!owed && matches!(out.first().map(|c| c.op), Some('M' | 'm'))).then_some(out)
}

// ---- algebra --------------------------------------------------------------

fn same_unit(a: Unit, b: Unit) -> bool {
    a == b || (matches!(a, "" | "px") && matches!(b, "" | "px"))
}

fn same_path(a: &[PathCmd], b: &[PathCmd]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.op == y.op && x.args.len() == y.args.len())
}

/// Whether a blend between the two is meaningful (else the animation steps).
pub(super) fn compatible(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Len(_, x), Value::Len(_, y)) => same_unit(x, y),
        (Value::Color(_), Value::Color(_)) => true,
        (Value::List(x), Value::List(y)) => x.len() == y.len(),
        (Value::Path(x), Value::Path(y)) => same_path(x, y),
        _ => false,
    }
}

pub(super) fn lerp(a: &Value, b: &Value, p: f64) -> Value {
    if !compatible(a, b) {
        return if p < 0.5 { a.clone() } else { b.clone() };
    }
    let mix = |x: f64, y: f64| x + (y - x) * p;
    match (a, b) {
        (Value::Len(x, u), Value::Len(y, v)) => Value::Len(mix(*x, *y), if u.is_empty() { v } else { u }),
        (Value::Color(x), Value::Color(y)) => Value::Color([
            mix(x[0], y[0]).clamp(0.0, 255.0),
            mix(x[1], y[1]).clamp(0.0, 255.0),
            mix(x[2], y[2]).clamp(0.0, 255.0),
            mix(x[3], y[3]).clamp(0.0, 1.0),
        ]),
        (Value::List(x), Value::List(y)) => Value::List(x.iter().zip(y).map(|(x, y)| mix(*x, *y)).collect()),
        (Value::Path(x), Value::Path(y)) => Value::Path(
            x.iter()
                .zip(y)
                .map(|(c, d)| PathCmd {
                    op: c.op,
                    args: c
                        .args
                        .iter()
                        .zip(&d.args)
                        .enumerate()
                        .map(|(i, (x, y))| {
                            if matches!(c.op, 'A' | 'a') && (i == 3 || i == 4) {
                                if p < 0.5 {
                                    *x
                                } else {
                                    *y
                                }
                            } else {
                                mix(*x, *y)
                            }
                        })
                        .collect(),
                })
                .collect(),
        ),
        _ => unreachable!("compatible() pairs only matching variants"),
    }
}

/// `a + b` for additive animation; `None` when the two do not add.
pub(super) fn add(a: &Value, b: &Value) -> Option<Value> {
    if let (Value::Transform(x), Value::Transform(y)) = (a, b) {
        let x = x.trim();
        return Some(Value::Transform(if x.is_empty() { y.clone() } else { format!("{x} {y}") }));
    }
    if !compatible(a, b) {
        return None;
    }
    Some(match (a, b) {
        (Value::Len(x, u), Value::Len(y, v)) => Value::Len(x + y, if u.is_empty() { v } else { u }),
        (Value::Color(x), Value::Color(y)) => Value::Color([
            (x[0] + y[0]).clamp(0.0, 255.0),
            (x[1] + y[1]).clamp(0.0, 255.0),
            (x[2] + y[2]).clamp(0.0, 255.0),
            (x[3] + y[3]).clamp(0.0, 1.0),
        ]),
        (Value::List(x), Value::List(y)) => Value::List(x.iter().zip(y).map(|(x, y)| x + y).collect()),
        (Value::Path(x), Value::Path(y)) => Value::Path(
            x.iter()
                .zip(y)
                .map(|(c, d)| PathCmd {
                    op: c.op,
                    args: c.args.iter().zip(&d.args).map(|(x, y)| x + y).collect(),
                })
                .collect(),
        ),
        _ => return None,
    })
}

/// `v * k`, for `accumulate="sum"`.
pub(super) fn scale(v: &Value, k: f64) -> Option<Value> {
    Some(match v {
        Value::Len(x, u) => Value::Len(x * k, u),
        Value::List(l) => Value::List(l.iter().map(|x| x * k).collect()),
        Value::Color(c) => Value::Color([c[0] * k, c[1] * k, c[2] * k, c[3] * k]),
        _ => return None,
    })
}

/// What the zero of the same shape is (the start of a `by` animation).
pub(super) fn zero_like(v: &Value) -> Option<Value> {
    scale(v, 0.0)
}

/// How far apart two values are, for `calcMode="paced"`; `None` when that is not defined.
pub(super) fn distance(a: &Value, b: &Value) -> Option<f64> {
    if !compatible(a, b) {
        return None;
    }
    let euclid = |x: &[f64], y: &[f64]| x.iter().zip(y).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt();
    Some(match (a, b) {
        (Value::Len(x, _), Value::Len(y, _)) => (x - y).abs(),
        (Value::Color(x), Value::Color(y)) => euclid(&x[..3], &y[..3]),
        (Value::List(x), Value::List(y)) => euclid(x, y),
        (Value::Path(x), Value::Path(y)) => {
            let fx: Vec<f64> = x.iter().flat_map(|c| c.args.iter().copied()).collect();
            let fy: Vec<f64> = y.iter().flat_map(|c| c.args.iter().copied()).collect();
            euclid(&fx, &fy)
        }
        _ => return None,
    })
}

// ---- easing ---------------------------------------------------------------

/// The `keySplines` curve `(x1 y1 x2 y2)` at `p`: Newton on the bezier's x, bisection when it stalls.
pub(super) fn spline_ease(p: f64, s: [f64; 4]) -> f64 {
    let p = p.clamp(0.0, 1.0);
    let [x1, y1, x2, y2] = s;
    let bez = |a: f64, b: f64, u: f64| {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };
    let dx = |u: f64| {
        let v = 1.0 - u;
        3.0 * v * v * x1 + 6.0 * v * u * (x2 - x1) + 3.0 * u * u * (1.0 - x2)
    };
    let mut u = p;
    for _ in 0..8 {
        let err = bez(x1, x2, u) - p;
        if err.abs() < 1e-9 {
            return bez(y1, y2, u);
        }
        let d = dx(u);
        if d.abs() < 1e-7 {
            break;
        }
        u = (u - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..48 {
        u = (lo + hi) / 2.0;
        if bez(x1, x2, u) < p {
            lo = u;
        } else {
            hi = u;
        }
    }
    bez(y1, y2, (lo + hi) / 2.0)
}

// ---- transforms -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransformType {
    Translate,
    Scale,
    Rotate,
    SkewX,
    SkewY,
}

impl TransformType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "translate" => Self::Translate,
            "scale" => Self::Scale,
            "rotate" => Self::Rotate,
            "skewX" => Self::SkewX,
            "skewY" => Self::SkewY,
            _ => return None,
        })
    }

    /// The numbers of one value, padded to the type's arity (`scale(2)` is `scale(2 2)`).
    pub fn numbers(self, s: &str) -> Option<Vec<f64>> {
        let mut n = parse_list(s)?;
        match (self, n.len()) {
            (Self::Translate, 1) => n.push(0.0),
            (Self::Translate, 2) | (Self::Scale, 2) | (Self::Rotate, 3) | (Self::SkewX | Self::SkewY, 1) => {}
            (Self::Scale, 1) => n.push(n[0]),
            (Self::Rotate, 1) => n.extend([0.0, 0.0]),
            _ => return None,
        }
        Some(n)
    }

    pub fn identity(self) -> Vec<f64> {
        match self {
            Self::Translate => vec![0.0, 0.0],
            Self::Scale => vec![1.0, 1.0],
            Self::Rotate => vec![0.0, 0.0, 0.0],
            Self::SkewX | Self::SkewY => vec![0.0],
        }
    }

    pub fn format(self, n: &[f64]) -> String {
        let f = |i: usize| fmt_num(n.get(i).copied().unwrap_or(0.0));
        match self {
            Self::Translate => format!("translate({} {})", f(0), f(1)),
            Self::Scale => format!("scale({} {})", f(0), f(1)),
            Self::Rotate if f(1) == "0" && f(2) == "0" => format!("rotate({})", f(0)),
            Self::Rotate => format!("rotate({} {} {})", f(0), f(1), f(2)),
            Self::SkewX => format!("skewX({})", f(0)),
            Self::SkewY => format!("skewY({})", f(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(s: &str) -> Value {
        parse(Kind::Len, s)
    }

    #[test]
    fn numbers_print_canonically() {
        for (v, want) in [
            (1.0, "1"),
            (0.5, "0.5"),
            (-0.0, "0"),
            (1.23456789, "1.234568"),
            (100.0, "100"),
            (0.0000004, "0"),
            (-2.50, "-2.5"),
            (f64::NAN, "0"),
            (f64::INFINITY, "0"),
            (1234567.0, "1234567"),
        ] {
            assert_eq!(fmt_num(v), want, "{v}");
        }
    }

    #[test]
    fn numbers_and_lengths_parse_by_the_book() {
        for (s, want) in [
            ("5", Some((5.0, ""))),
            ("-5.5px", Some((-5.5, "px"))),
            (".5", Some((0.5, ""))),
            ("5.", Some((5.0, ""))),
            ("1e2", Some((100.0, ""))),
            ("1e-2%", Some((0.01, "%"))),
            ("2em", Some((2.0, "em"))),
            ("10PX", Some((10.0, "px"))),
            ("  7  ", Some((7.0, ""))),
            ("1e", None),
            ("abc", None),
            ("", None),
            ("1 2", None),
            ("5furlong", None),
            ("inf", None),
            ("NaN", None),
        ] {
            assert_eq!(parse_len(s.trim()), want, "{s:?}");
        }
        assert_eq!(parse_number("1e"), None);
        assert_eq!(parse_number("-0.25"), Some(-0.25));
        assert_eq!(parse_number("infinity"), None);
    }

    #[test]
    fn colours_parse_and_print_per_channel() {
        for (s, want) in [
            ("#f00", "#ff0000"),
            ("#00ff00", "#00ff00"),
            ("red", "#ff0000"),
            ("rgb(0, 0, 255)", "#0000ff"),
            ("#00000080", "rgba(0,0,0,0.502)"),
            ("rgba(255,255,255,0.5)", "rgba(255,255,255,0.502)"),
            ("transparent", "rgba(0,0,0,0)"),
        ] {
            assert_eq!(format(&parse(Kind::Color, s)), want, "{s}");
        }
        assert_eq!(parse(Kind::Color, "none"), Value::Text("none".into()));
        let mid = lerp(&parse(Kind::Color, "#000000"), &parse(Kind::Color, "#ff8000"), 0.5);
        assert_eq!(format(&mid), "#804000");
    }

    #[test]
    fn lengths_blend_only_in_one_unit() {
        let mid = lerp(&len("10"), &len("20"), 0.25);
        assert_eq!(format(&mid), "12.5");
        assert_eq!(format(&lerp(&len("10px"), &len("20"), 0.5)), "15px");
        assert_eq!(format(&lerp(&len("0%"), &len("100%"), 0.3)), "30%");
        let steps = (lerp(&len("10px"), &len("5%"), 0.49), lerp(&len("10px"), &len("5%"), 0.5));
        assert_eq!((format(&steps.0), format(&steps.1)), ("10px".into(), "5%".into()));
        assert!(!compatible(&len("1em"), &len("1px")));
    }

    #[test]
    fn lists_blend_when_the_length_matches() {
        let a = parse(Kind::List, "0 0 100 50");
        let b = parse(Kind::List, "0,0,200,100");
        assert_eq!(format(&lerp(&a, &b, 0.5)), "0 0 150 75");
        assert!(!compatible(&a, &parse(Kind::List, "1 2")));
        assert_eq!(parse(Kind::List, "none"), Value::Text("none".into()));
        assert_eq!(parse(Kind::Len, "10 20 30"), Value::List(vec![10.0, 20.0, 30.0]));
    }

    #[test]
    fn paths_blend_on_one_command_sequence() {
        let a = parse(Kind::Path, "M0 0 L10 0 L10 10 Z");
        let b = parse(Kind::Path, "m0,0l20,0 l20,20z");
        assert!(!compatible(&a, &b), "different command case");
        let c = parse(Kind::Path, "M0 0L20 0L30 40Z");
        assert_eq!(format(&lerp(&a, &c, 0.5)), "M0 0L15 0L20 25Z");
        let implicit = parse(Kind::Path, "M0 0 10 0 10 10");
        assert_eq!(format(&implicit), "M0 0L10 0L10 10");
        let arc = parse(Kind::Path, "M0 0a5 5 0 01.5.5");
        assert_eq!(format(&arc), "M0 0a5 5 0 0 1 0.5 0.5");
        assert!(matches!(parse(Kind::Path, "L1 1"), Value::Text(_)));
        assert!(matches!(parse(Kind::Path, "M1"), Value::Text(_)));
        assert!(matches!(parse(Kind::Path, "M0 0 Q"), Value::Text(_)));
    }

    #[test]
    fn arc_flags_step_rather_than_blend() {
        let a = parse(Kind::Path, "M0 0A5 5 0 0 0 10 10");
        let b = parse(Kind::Path, "M0 0A5 5 0 1 1 20 20");
        assert_eq!(format(&lerp(&a, &b, 0.25)), "M0 0A5 5 0 0 0 12.5 12.5");
        assert_eq!(format(&lerp(&a, &b, 0.75)), "M0 0A5 5 0 1 1 17.5 17.5");
    }

    #[test]
    fn sums_and_scales_follow_the_type() {
        assert_eq!(format(&add(&len("10"), &len("5")).unwrap()), "15");
        assert_eq!(format(&add(&len("10px"), &len("5")).unwrap()), "15px");
        assert!(add(&len("10px"), &len("5%")).is_none());
        let t = add(
            &Value::Transform("translate(1 2)".into()),
            &Value::Transform("scale(2 2)".into()),
        )
        .unwrap();
        assert_eq!(format(&t), "translate(1 2) scale(2 2)");
        let t = add(&Value::Transform("  ".into()), &Value::Transform("scale(2 2)".into())).unwrap();
        assert_eq!(format(&t), "scale(2 2)");
        assert_eq!(format(&scale(&len("3px"), 4.0).unwrap()), "12px");
        assert!(scale(&Value::Text("x".into()), 2.0).is_none());
        assert_eq!(format(&zero_like(&len("3px")).unwrap()), "0px");
        let c = add(&parse(Kind::Color, "#808080"), &parse(Kind::Color, "#808080")).unwrap();
        assert_eq!(format(&c), "#ffffff");
    }

    #[test]
    fn paced_distance_is_defined_for_numbers_only() {
        assert_eq!(distance(&len("1"), &len("4")), Some(3.0));
        assert_eq!(distance(&parse(Kind::List, "0 0"), &parse(Kind::List, "3 4")), Some(5.0));
        assert_eq!(distance(&Value::Text("a".into()), &Value::Text("b".into())), None);
    }

    #[test]
    fn splines_hit_their_ends_and_ease() {
        let ease_out = [0.16, 1.0, 0.3, 1.0];
        assert!(spline_ease(0.0, ease_out).abs() < 1e-9);
        assert!((spline_ease(1.0, ease_out) - 1.0).abs() < 1e-9);
        assert!(spline_ease(0.2, ease_out) > 0.5, "an ease-out is ahead of linear early");
        let lin = [0.0, 0.0, 1.0, 1.0];
        for i in 0..=10 {
            let p = f64::from(i) / 10.0;
            assert!((spline_ease(p, lin) - p).abs() < 1e-6);
        }
        // monotone in x, so monotone in p
        let mut last = 0.0;
        for i in 0..=100 {
            let y = spline_ease(f64::from(i) / 100.0, [0.42, 0.0, 0.58, 1.0]);
            assert!(y >= last - 1e-9);
            last = y;
        }
        // a flat-x curve (x1 = x2 = 0) falls back to bisection without looping
        assert!(spline_ease(0.5, [0.0, 0.0, 0.0, 1.0]).is_finite());
    }

    #[test]
    fn transform_values_pad_to_their_arity() {
        use TransformType::*;
        assert_eq!(Translate.numbers("10"), Some(vec![10.0, 0.0]));
        assert_eq!(Scale.numbers("2"), Some(vec![2.0, 2.0]));
        assert_eq!(Rotate.numbers("45"), Some(vec![45.0, 0.0, 0.0]));
        assert_eq!(Rotate.numbers("45 10 20"), Some(vec![45.0, 10.0, 20.0]));
        assert_eq!(Rotate.numbers("45 10"), None);
        assert_eq!(SkewX.numbers("1 2"), None);
        assert_eq!(Rotate.format(&[45.0, 0.0, 0.0]), "rotate(45)");
        assert_eq!(Rotate.format(&[45.0, 10.0, 20.0]), "rotate(45 10 20)");
        assert_eq!(Translate.format(&[1.5, -2.0]), "translate(1.5 -2)");
        assert_eq!(Scale.identity(), vec![1.0, 1.0]);
    }

    #[test]
    fn attributes_pick_their_kind() {
        assert_eq!(kind_of("fill"), Kind::Color);
        assert_eq!(kind_of("d"), Kind::Path);
        assert_eq!(kind_of("viewBox"), Kind::List);
        assert_eq!(kind_of("visibility"), Kind::Discrete);
        assert_eq!(kind_of("class"), Kind::Discrete);
        assert_eq!(kind_of("opacity"), Kind::Len);
        assert!(is_css_property("fill-opacity"));
        assert!(!is_css_property("x"));
        assert!(!is_css_property("transform"));
        assert_eq!(default_text("opacity"), Some("1"));
        assert_eq!(default_text("d"), None);
    }
}
