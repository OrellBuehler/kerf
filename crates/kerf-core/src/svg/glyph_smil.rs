//! A [`TextAnimation`] compiled into SMIL for the units of one outlined text (`outline.rs` calls this for each
//! unit and wraps its glyph paths in what comes back).
//!
//! **Sampled, then simplified.** A unit's amount is a function of time (the window slides, the noise moves),
//! so it is evaluated on a fine grid (1/120 s) and reduced to the few points a straight-line animation needs
//! (Ramer-Douglas-Peucker on the deviation from the chord, in units of how far each property may be off: a
//! third of a pixel of movement at a 90 px font (0.4 % of an em), a fifth of a degree, 0.4 % of scale or opacity
//! (a level of 8-bit)). A hard edge (a `square` window) stays a step one cell wide; a held stretch is one segment.
//! Every property of a unit shares those points, so one `keyTimes` list serves them all.
//!
//! **At most [`MAX_KEYS`] points, one pass.** The segment that is furthest off its chord is split first, and the
//! splitting stops when every segment is within its tolerance or the points run out. A signal that is simple
//! enough ends by the first rule, with exactly the points plain Ramer-Douglas-Peucker keeps. A noisy one (a fast or
//! long wiggle) ends by the second: it keeps the 96 points that cut its worst errors, never a looser tolerance,
//! so it still moves, at a lower pace than the noise has.
//!
//! **What a unit becomes**: a `<g>` that carries the unit's glyph paths and its animation, in this order of
//! transforms (`additive="sum"`): translate, rotate, scale. Rotation and scale turn about the unit's centre, so
//! when either is in use the paths are written relative to it and the group starts with
//! `translate(cx cy)`. Opacity is animated on the group, and a blur is a `<filter>` inside it (a region as big
//! as the unit and its blur, in the group's own coordinates, so it moves with the unit). A property that does
//! not change over the animation is written as an attribute, not an animation; a unit that is never touched
//! gets no group.

use std::collections::{BinaryHeap, HashSet};

use super::text_animation::{AnimateBy, Easing, Shape, TextAnimation, Wiggle};

/// The grid a unit's amount is evaluated on, seconds.
const STEP: f64 = 1.0 / 120.0;
/// The most points one animation keeps.
const MAX_KEYS: usize = 96;
/// The grid is at most this many points (60 seconds).
const MAX_GRID: usize = 7_200;
/// The smallest scale drawn (a zero scale is not a drawable transform).
const MIN_SCALE: f64 = 0.001;

/// The smoothstep.
fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn ease(easing: Easing, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match easing {
        Easing::Linear => t,
        Easing::EaseIn => t * t,
        Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
        Easing::EaseInOut => smooth(t),
    }
}

/// The amount a `shape` gives at `q` across a window (0 at its start, 1 at its end).
fn shape_amount(shape: Shape, q: f64) -> f64 {
    let inside = (0.0..=1.0).contains(&q);
    match shape {
        Shape::Square => f64::from(u8::from(inside)),
        Shape::RampUp => q.clamp(0.0, 1.0),
        Shape::RampDown => 1.0 - q.clamp(0.0, 1.0),
        Shape::Triangle if inside => 1.0 - (2.0 * q - 1.0).abs(),
        Shape::Round if inside => (1.0 - (2.0 * q - 1.0).powi(2)).max(0.0).sqrt(),
        Shape::Smooth if inside => 0.5 - 0.5 * (std::f64::consts::TAU * q).cos(),
        _ => 0.0,
    }
}

/// The range selector's amount for a unit at `place` (0 to 1 along the text) with the window moved by `offset`.
pub(crate) fn range_amount(a: &TextAnimation, place: f64, offset: f64) -> f64 {
    let (start, end) = (a.range.start + offset, a.range.end + offset);
    shape_amount(a.range.shape, (place - start) / (end - start))
}

/// Where the window is at `t`.
fn offset_at(a: &TextAnimation, t: f64) -> f64 {
    let progress = ease(a.easing, (t - a.delay) / a.duration);
    a.range.offset.from + (a.range.offset.to - a.range.offset.from) * progress
}

/// A number in `[0, 1)` from a seed, a stream and a knot (splitmix64).
fn hash01(seed: u32, stream: u64, knot: i64) -> f64 {
    let mut z = (u64::from(seed) << 32)
        .wrapping_add(stream.wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .wrapping_add(knot as u64)
        .wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth value noise in `[0, 1]`: a random value at each whole `x`, eased between.
fn noise(seed: u32, stream: u64, x: f64) -> f64 {
    let k = x.floor();
    let (a, b) = (hash01(seed, stream, k as i64), hash01(seed, stream, k as i64 + 1));
    a + (b - a) * smooth(x - k)
}

/// The wiggly selector's multiplier for a unit at `t`: noise between `min` and `max`, faded in and out over
/// the animation's ends so the text comes to rest.
fn wiggle_at(a: &TextAnimation, w: &Wiggle, unit: usize, t: f64) -> f64 {
    let fade = (a.duration / 4.0).min(0.25);
    let envelope = smooth((t - a.delay) / fade) * smooth((a.end() - t) / fade);
    if envelope <= 0.0 {
        return 0.0;
    }
    let x = w.rate * (t - a.delay);
    let shared = noise(w.seed, 0, x);
    let own = noise(w.seed, unit as u64 + 1, x);
    let n = w.correlation * shared + (1.0 - w.correlation) * own;
    envelope * (w.min + (w.max - w.min) * n)
}

/// A unit's amount at `t`: its range amount, times the wiggle if there is one.
pub(crate) fn amount_at(a: &TextAnimation, unit: usize, count: usize, t: f64) -> f64 {
    let place = (unit as f64 + 0.5) / count as f64;
    let amount = range_amount(a, place, offset_at(a, t));
    match &a.wiggle {
        Some(w) => amount * wiggle_at(a, w, unit, t),
        None => amount,
    }
}

/// What a unit is, for the animation to turn it about its centre and blur it within a region.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UnitGeom {
    /// The box of its glyphs' outlines `[x0, y0, x1, y1]`, in the text's coordinates.
    pub bounds: [f32; 4],
    /// The point it turns and scales about.
    pub pivot: (f32, f32),
    /// The font size of its first glyph (an em).
    pub size: f32,
}

/// Where the ids of a text's blur filters come from: none that the document or an earlier text has.
pub(crate) struct IdSource<'a> {
    pub taken: &'a HashSet<String>,
    pub next: usize,
}

impl IdSource<'_> {
    fn fresh(&mut self) -> String {
        loop {
            let id = format!("kf-blur-{}", self.next);
            self.next += 1;
            if !self.taken.contains(&id) {
                return id;
            }
        }
    }
}

/// What wraps a unit's glyph paths.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Wrap {
    /// The group's start tag and what goes in it before the paths (a filter, the animations).
    pub open: String,
    /// The paths are written relative to this point (the group starts at it).
    pub pivot: Option<(f32, f32)>,
    /// How many animation elements `open` holds.
    pub animations: usize,
}

/// The number of components a sample has: x, y, rotation, scale, opacity, blur.
const N: usize = 6;

struct Sample {
    t: f64,
    v: [f64; N],
}

/// A stretch of the samples, `lo..=hi`, and the sample inside it furthest off the chord between its ends
/// (`dev` of the tolerance, so 1 is just within it).
struct Stretch {
    dev: f64,
    at: usize,
    lo: usize,
    hi: usize,
}

impl Stretch {
    fn of(samples: &[Sample], tol: &[f64; N], channels: &[usize], lo: usize, hi: usize) -> Self {
        let (a, b) = (&samples[lo], &samples[hi]);
        let span = b.t - a.t;
        let (mut dev, mut at) = (0.0f64, lo);
        for (i, sample) in samples.iter().enumerate().take(hi).skip(lo + 1) {
            let f = (sample.t - a.t) / span;
            let mut off = 0.0f64;
            for &c in channels {
                let line = a.v[c] + (b.v[c] - a.v[c]) * f;
                off = off.max((sample.v[c] - line).abs() / tol[c]);
            }
            if off > dev {
                (dev, at) = (off, i);
            }
        }
        Stretch { dev, at, lo, hi }
    }
}

impl PartialEq for Stretch {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Stretch {}

impl PartialOrd for Stretch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Stretch {
    /// The worst first; of equals, the earlier.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.dev.total_cmp(&other.dev).then(other.lo.cmp(&self.lo))
    }
}

/// Simplify `samples` to at most `max` points a straight line between them stays within `tol` (per component) of
/// where it can, keeping the first and the last: the stretch furthest off its chord is split first, and it stops
/// when none is more than `tol` off or `max` points are kept. Each stretch is read once when it is made, so the
/// work is a few passes over the grid whatever the signal does.
fn simplify(samples: &[Sample], tol: &[f64; N], active: &[bool; N], max: usize) -> Vec<usize> {
    let n = samples.len();
    let channels: Vec<usize> = (0..N).filter(|c| active[*c]).collect();
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut kept = 2;
    let mut open = BinaryHeap::new();
    open.push(Stretch::of(samples, tol, &channels, 0, n - 1));
    while kept < max {
        let Some(worst) = open.pop() else { break };
        if worst.dev <= 1.0 {
            break;
        }
        keep[worst.at] = true;
        kept += 1;
        for (lo, hi) in [(worst.lo, worst.at), (worst.at, worst.hi)] {
            if hi > lo + 1 {
                open.push(Stretch::of(samples, tol, &channels, lo, hi));
            }
        }
    }
    (0..n).filter(|i| keep[*i]).collect()
}

/// A number to `dp` places without trailing zeros (and no `-0`).
fn fmt(v: f64, dp: usize) -> String {
    let s = format!("{v:.dp$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// The element for one animated attribute: `values` (one string per point) at the shared `key_times`.
fn animation(tag: &str, target: &str, values: &[String], key_times: Option<&str>, dur: &str) -> String {
    let mut out = format!("<{tag} {target} values=\"{}\"", values.join(";"));
    if let Some(k) = key_times {
        out.push_str(&format!(" keyTimes=\"{k}\""));
    }
    out.push_str(&format!(" dur=\"{dur}s\" fill=\"freeze\"/>"));
    out
}

/// How many points `wrap` evaluates a unit's amount at: what one unit costs, so a document can be held to a budget.
pub(crate) fn grid_points(a: &TextAnimation) -> usize {
    ((a.end() / STEP).ceil() as usize).clamp(2, MAX_GRID) + 1
}

/// Whether `unit` of `count` is touched by the animation, and the group (with its filter and animations) that
/// carries it if so. `None` when the animation leaves it as it is.
pub(crate) fn wrap(a: &TextAnimation, unit: usize, count: usize, geom: &UnitGeom, ids: &mut IdSource) -> Option<Wrap> {
    let end = a.end();
    let size = f64::from(geom.size);
    let grid = grid_points(a) - 1;
    let p = &a.properties;
    let active = [p.moves(), p.moves(), p.turns(), p.scales(), p.fades(), p.blurs()];
    let tol = [0.004 * size, 0.004 * size, 0.2, 0.004, 0.004, 0.004 * size];
    let mut touched = false;
    let samples: Vec<Sample> = (0..=grid)
        .map(|i| {
            let t = end * i as f64 / grid as f64;
            let m = amount_at(a, unit, count, t);
            touched |= m.abs() >= 1e-9;
            Sample {
                t,
                v: [
                    p.x * m * size,
                    p.y * m * size,
                    p.rotation * m,
                    (1.0 + (p.scale - 1.0) * m).max(MIN_SCALE),
                    (1.0 + (p.opacity - 1.0) * m).clamp(0.0, 1.0),
                    (p.blur * m).max(0.0) * size,
                ],
            }
        })
        .collect();
    if !touched {
        return None;
    }
    let keys = simplify(&samples, &tol, &active, MAX_KEYS);
    let at = |c: usize| keys.iter().map(|&k| samples[k].v[c]).collect::<Vec<f64>>();
    // A component varies when some point is further from the first than it may be off.
    let varies = |cs: &[usize]| {
        cs.iter()
            .any(|&c| active[c] && at(c).iter().any(|v| (v - samples[keys[0]].v[c]).abs() > tol[c]))
    };
    let first = |c: usize| samples[keys[0]].v[c];
    let dur = fmt(end, 4);
    let key_times: String = keys.iter().map(|&k| fmt(samples[k].t / end, 4)).collect::<Vec<_>>().join(";");
    let pivot = (p.turns() || p.scales()).then_some(geom.pivot);

    // The transforms in order; the static ones before the first animated one go in the `transform` attribute.
    struct Op {
        animated: bool,
        neutral: bool,
        text: String,
        kind: &'static str,
        values: Vec<String>,
        constant: String,
    }
    let mut ops: Vec<Op> = Vec::new();
    if p.moves() {
        let (xs, ys) = (at(0), at(1));
        ops.push(Op {
            animated: varies(&[0, 1]),
            neutral: first(0).abs() <= tol[0] && first(1).abs() <= tol[1],
            text: format!("translate({} {})", fmt(first(0), 2), fmt(first(1), 2)),
            kind: "translate",
            values: xs
                .iter()
                .zip(&ys)
                .map(|(x, y)| format!("{} {}", fmt(*x, 2), fmt(*y, 2)))
                .collect(),
            constant: format!("{} {}", fmt(first(0), 2), fmt(first(1), 2)),
        });
    }
    if p.turns() {
        ops.push(Op {
            animated: varies(&[2]),
            neutral: first(2).abs() <= tol[2],
            text: format!("rotate({})", fmt(first(2), 2)),
            kind: "rotate",
            values: at(2).iter().map(|v| fmt(*v, 2)).collect(),
            constant: fmt(first(2), 2),
        });
    }
    if p.scales() {
        ops.push(Op {
            animated: varies(&[3]),
            neutral: (first(3) - 1.0).abs() <= tol[3],
            text: format!("scale({})", fmt(first(3), 3)),
            kind: "scale",
            values: at(3).iter().map(|v| fmt(*v, 3)).collect(),
            constant: fmt(first(3), 3),
        });
    }
    let fades = p.fades().then(|| (varies(&[4]), first(4)));
    let blur = p.blurs().then(|| (varies(&[5]), first(5)));

    let mut base: Vec<String> = Vec::new();
    if let Some((cx, cy)) = pivot {
        base.push(format!("translate({} {})", fmt(f64::from(cx), 2), fmt(f64::from(cy), 2)));
    }
    let split = ops.iter().position(|o| o.animated).unwrap_or(ops.len());
    for o in &ops[..split] {
        if !o.neutral {
            base.push(o.text.clone());
        }
    }
    let mut children = String::new();
    let mut animations = 0;
    for o in &ops[split..] {
        if !o.animated && o.neutral {
            continue;
        }
        let target = format!("attributeName=\"transform\" type=\"{}\" additive=\"sum\"", o.kind);
        if o.animated {
            children.push_str(&animation("animateTransform", &target, &o.values, Some(&key_times), &dur));
        } else {
            let two = [o.constant.clone(), o.constant.clone()];
            children.push_str(&animation("animateTransform", &target, &two, None, &dur));
        }
        animations += 1;
    }

    let mut attrs = String::new();
    if !base.is_empty() {
        attrs.push_str(&format!(" transform=\"{}\"", base.join(" ")));
    }
    match fades {
        Some((true, _)) => {
            let values: Vec<String> = at(4).iter().map(|v| fmt(*v, 3)).collect();
            children.push_str(&animation(
                "animate",
                "attributeName=\"opacity\"",
                &values,
                Some(&key_times),
                &dur,
            ));
            animations += 1;
        }
        Some((false, v)) if (v - 1.0).abs() > 0.004 => attrs.push_str(&format!(" opacity=\"{}\"", fmt(v, 3))),
        _ => {}
    }
    let mut filter = String::new();
    if let Some((varying, still)) = blur {
        let peak = at(5).iter().copied().fold(0.0f64, f64::max);
        if varying || still > tol[5] {
            let id = ids.fresh();
            let margin = 2.5 * peak + 0.1 * size + 1.0;
            let [x0, y0, x1, y1] = geom.bounds.map(f64::from);
            let (ox, oy) = pivot.map_or((0.0, 0.0), |(cx, cy)| (f64::from(cx), f64::from(cy)));
            let region = format!(
                "x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
                fmt(x0 - ox - margin, 2),
                fmt(y0 - oy - margin, 2),
                fmt(x1 - x0 + 2.0 * margin, 2),
                fmt(y1 - y0 + 2.0 * margin, 2)
            );
            filter.push_str(&format!(
                "<filter id=\"{id}\" filterUnits=\"userSpaceOnUse\" color-interpolation-filters=\"sRGB\" {region}>"
            ));
            if varying {
                let values: Vec<String> = at(5).iter().map(|v| fmt(*v, 2)).collect();
                filter.push_str(&format!(
                    "<feGaussianBlur stdDeviation=\"{}\">{}</feGaussianBlur>",
                    fmt(first(5), 2),
                    animation("animate", "attributeName=\"stdDeviation\"", &values, Some(&key_times), &dur)
                ));
                animations += 1;
            } else {
                filter.push_str(&format!("<feGaussianBlur stdDeviation=\"{}\"/>", fmt(still, 2)));
            }
            filter.push_str("</filter>");
            attrs.push_str(&format!(" filter=\"url(#{id})\""));
        }
    }
    if attrs.is_empty() && children.is_empty() {
        return None;
    }
    Some(Wrap {
        open: format!("<g{attrs}>{filter}{children}"),
        pivot,
        animations,
    })
}

/// The unit index each byte of `text` belongs to, for the characters that are not white space, and how many
/// units there are. Words are runs between white space. (Lines are the layout's.)
pub(crate) fn units_of(text: &str, by: AnimateBy) -> (Vec<Option<usize>>, usize) {
    let mut map = vec![None; text.len() + 1];
    let (mut next, mut in_word) = (0usize, false);
    for (at, c) in text.char_indices() {
        if c.is_whitespace() {
            in_word = false;
            continue;
        }
        match by {
            AnimateBy::Character => {
                map[at] = Some(next);
                next += 1;
            }
            AnimateBy::Word => {
                if !in_word {
                    next += 1;
                }
                map[at] = Some(next - 1);
            }
            AnimateBy::Line => map[at] = Some(0),
        }
        in_word = true;
    }
    (map, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svg::text_animation::{text_animation_preset, Offset, Properties, RangeSelector};
    use crate::svg::SvgDoc;

    fn typewriter() -> TextAnimation {
        text_animation_preset("typewriter").unwrap().animation
    }

    fn geom() -> UnitGeom {
        UnitGeom {
            bounds: [10.0, 20.0, 30.0, 50.0],
            pivot: (20.0, 38.0),
            size: 40.0,
        }
    }

    fn ids() -> (HashSet<String>, usize) {
        (HashSet::new(), 0)
    }

    #[test]
    fn a_square_window_slid_over_the_text_shows_the_units_in_order() {
        let a = typewriter();
        // Four units: unit u shows (amount 0) once the window has passed its place.
        let shown_at = |u: usize| {
            (0..=1500)
                .map(|i| f64::from(i) / 1000.0)
                .find(|t| amount_at(&a, u, 4, *t) == 0.0)
                .unwrap()
        };
        let times: Vec<f64> = (0..4).map(shown_at).collect();
        assert!(times.windows(2).all(|w| w[0] < w[1]), "{times:?}");
        // Linear easing: place 0.125, 0.375, ... of a 1.5 s sweep.
        for (u, t) in times.iter().enumerate() {
            let want = (u as f64 + 0.5) / 4.0 * 1.5;
            assert!((t - want).abs() < 0.002, "unit {u}: {t} vs {want}");
        }
        // Hidden at the start, shown at the end.
        assert!((0..4).all(|u| amount_at(&a, u, 4, 0.0) == 1.0 && amount_at(&a, u, 4, 1.5) == 0.0));
    }

    #[test]
    fn the_shapes_are_what_they_say() {
        let at = |s, q| shape_amount(s, q);
        assert_eq!(
            (at(Shape::Square, 0.5), at(Shape::Square, 1.2), at(Shape::Square, -0.1)),
            (1.0, 0.0, 0.0)
        );
        assert_eq!(
            (at(Shape::RampUp, -1.0), at(Shape::RampUp, 0.25), at(Shape::RampUp, 9.0)),
            (0.0, 0.25, 1.0)
        );
        assert_eq!(
            (at(Shape::RampDown, -1.0), at(Shape::RampDown, 0.25), at(Shape::RampDown, 9.0)),
            (1.0, 0.75, 0.0)
        );
        assert_eq!(
            (at(Shape::Triangle, 0.5), at(Shape::Triangle, 0.25), at(Shape::Triangle, 2.0)),
            (1.0, 0.5, 0.0)
        );
        assert!((at(Shape::Round, 0.5) - 1.0).abs() < 1e-12 && at(Shape::Round, 0.0) == 0.0);
        assert!((at(Shape::Smooth, 0.5) - 1.0).abs() < 1e-12 && at(Shape::Smooth, 1.5) == 0.0);
    }

    #[test]
    fn the_easing_runs_from_nothing_to_all_of_it() {
        for e in [Easing::Linear, Easing::EaseIn, Easing::EaseOut, Easing::EaseInOut] {
            assert_eq!(
                (ease(e, -1.0), ease(e, 0.0), ease(e, 1.0), ease(e, 2.0)),
                (0.0, 0.0, 1.0, 1.0),
                "{e:?}"
            );
            assert!(ease(e, 0.5) > 0.0 && ease(e, 0.5) < 1.0);
        }
        assert!(ease(Easing::EaseIn, 0.5) < 0.5 && ease(Easing::EaseOut, 0.5) > 0.5);
    }

    #[test]
    fn the_wiggle_is_seeded_bounded_and_settles() {
        let a = text_animation_preset("wiggle").unwrap().animation;
        let w = a.wiggle.unwrap();
        let mut seen = (f64::INFINITY, f64::NEG_INFINITY);
        for u in 0..6 {
            for i in 0..=300 {
                let t = f64::from(i) * a.end() / 300.0;
                let v = wiggle_at(&a, &w, u, t);
                assert!((-1.0..=1.0).contains(&v), "{v}");
                assert_eq!(v, wiggle_at(&a, &w, u, t), "deterministic");
                seen = (seen.0.min(v), seen.1.max(v));
            }
        }
        assert!(seen.0 < -0.5 && seen.1 > 0.5, "it swings both ways: {seen:?}");
        // At rest at both ends, and not before it starts.
        for u in 0..4 {
            assert_eq!((wiggle_at(&a, &w, u, 0.0), wiggle_at(&a, &w, u, a.end())), (0.0, 0.0));
            assert_eq!(wiggle_at(&a, &w, u, -1.0), 0.0);
        }
        // Another seed is another pattern; full correlation is one pattern for every unit.
        let other = Wiggle { seed: 8, ..w };
        assert_ne!(wiggle_at(&a, &w, 0, 1.3), wiggle_at(&a, &other, 0, 1.3));
        let together = Wiggle { correlation: 1.0, ..w };
        assert_eq!(wiggle_at(&a, &together, 0, 1.3), wiggle_at(&a, &together, 5, 1.3));
        assert_ne!(wiggle_at(&a, &w, 0, 1.3), wiggle_at(&a, &w, 5, 1.3));
    }

    #[test]
    fn a_unit_is_a_group_with_the_animations_it_needs() {
        let a = text_animation_preset("rise").unwrap().animation;
        let (taken, next) = ids();
        let mut src = IdSource { taken: &taken, next };
        let w = wrap(&a, 1, 5, &geom(), &mut src).expect("a unit the window passes");
        assert_eq!(w.pivot, None, "no rotation or scale: absolute paths");
        assert_eq!(w.animations, 2);
        assert!(w.open.starts_with("<g>"), "{}", w.open);
        assert!(w
            .open
            .contains("<animateTransform attributeName=\"transform\" type=\"translate\" additive=\"sum\" values=\""));
        assert!(w.open.contains("<animate attributeName=\"opacity\" values=\"0;"));
        assert!(w.open.contains("keyTimes=\"0;"), "{}", w.open);
        assert!(w.open.contains(" dur=\"1.4s\" fill=\"freeze\"/>"));
        // The points are few: the sweep is a handful of segments, not a hundred and sixty.
        let points = w
            .open
            .split("values=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .split(';')
            .count();
        assert!((3..=12).contains(&points), "{points}");
    }

    #[test]
    fn turning_and_scaling_are_about_the_units_centre() {
        let a = text_animation_preset("spin_in").unwrap().animation;
        let (taken, next) = ids();
        let mut src = IdSource { taken: &taken, next };
        let w = wrap(&a, 2, 8, &geom(), &mut src).unwrap();
        assert_eq!(w.pivot, Some((20.0, 38.0)));
        assert!(w.open.starts_with("<g transform=\"translate(20 38)\">"), "{}", w.open);
        let rotate = w.open.find("type=\"rotate\"").unwrap();
        let scale = w.open.find("type=\"scale\"").unwrap();
        assert!(rotate < scale, "rotate, then scale: {}", w.open);
        assert_eq!(w.open.matches("type=\"translate\"").count(), 0);
    }

    #[test]
    fn a_blur_is_a_filter_inside_the_group_with_room_for_it() {
        let a = text_animation_preset("blur_in").unwrap().animation;
        let (taken, next) = ids();
        let mut src = IdSource { taken: &taken, next };
        let w = wrap(&a, 0, 3, &geom(), &mut src).unwrap();
        assert!(w.open.contains(" filter=\"url(#kf-blur-0)\""), "{}", w.open);
        assert!(w
            .open
            .contains("<filter id=\"kf-blur-0\" filterUnits=\"userSpaceOnUse\" color-interpolation-filters=\"sRGB\" x=\""));
        assert!(w.open.contains("<feGaussianBlur stdDeviation=\""));
        assert!(w.open.contains("attributeName=\"stdDeviation\""));
        assert_eq!(src.next, 1);
        // The next unit gets another id; a taken one is skipped.
        let taken: HashSet<String> = ["kf-blur-1".to_string()].into();
        let mut src = IdSource { taken: &taken, next: 0 };
        wrap(&a, 0, 3, &geom(), &mut src).unwrap();
        let second = wrap(&a, 1, 3, &geom(), &mut src).unwrap();
        assert!(second.open.contains("kf-blur-2"), "{}", second.open);
    }

    #[test]
    fn a_unit_the_window_never_reaches_is_left_alone_and_a_held_one_is_attributes() {
        // A window over the first fifth of the text that does not move: only unit 0 is touched, held.
        let a = TextAnimation {
            range: RangeSelector {
                start: 0.0,
                end: 0.2,
                offset: Offset::default(),
                shape: Shape::Square,
            },
            properties: Properties {
                opacity: 0.4,
                x: 0.5,
                ..Properties::default()
            },
            ..TextAnimation::default()
        };
        let (taken, next) = ids();
        let mut src = IdSource { taken: &taken, next };
        assert_eq!(wrap(&a, 3, 5, &geom(), &mut src), None);
        let held = wrap(&a, 0, 5, &geom(), &mut src).unwrap();
        assert_eq!(held.animations, 0);
        assert_eq!(held.open, "<g transform=\"translate(20 0)\" opacity=\"0.4\">");
    }

    #[test]
    fn units_are_characters_words_or_lines_without_the_spaces() {
        let (chars, n) = units_of("ab  c", AnimateBy::Character);
        assert_eq!(
            (n, chars[0], chars[1], chars[2], chars[4]),
            (3, Some(0), Some(1), None, Some(2))
        );
        let (words, n) = units_of("ab  c d", AnimateBy::Word);
        assert_eq!(
            (n, words[0], words[1], words[4], words[6]),
            (3, Some(0), Some(0), Some(1), Some(2))
        );
        let (_, n) = units_of("   ", AnimateBy::Character);
        assert_eq!(n, 0);
    }

    /// A wiggle of the preset's properties, `rate` a second for `duration` seconds.
    fn wiggling(rate: f64, duration: f64) -> TextAnimation {
        let mut a = text_animation_preset("wiggle").unwrap().animation;
        a.duration = duration;
        a.wiggle = Some(Wiggle {
            rate,
            ..a.wiggle.unwrap()
        });
        a
    }

    /// The `(keyTimes, values)` of the first `animateTransform` of `kind` in `open`, one number a point for
    /// `rotate` and the x of `translate`.
    fn polyline(open: &str, kind: &str) -> (Vec<f64>, Vec<f64>) {
        let tag = open
            .split("<animateTransform")
            .skip(1)
            .find(|t| t.contains(&format!("type=\"{kind}\"")))
            .unwrap();
        let attr = |name: &str| {
            tag.split(&format!("{name}=\""))
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .to_string()
        };
        let times = attr("keyTimes").split(';').map(|v| v.parse().unwrap()).collect();
        let values = attr("values")
            .split(';')
            .map(|v| v.split(' ').next().unwrap().parse().unwrap())
            .collect();
        (times, values)
    }

    fn at_time(times: &[f64], values: &[f64], q: f64) -> f64 {
        let i = times.iter().rposition(|t| *t <= q).unwrap().min(times.len() - 2);
        let f = (q - times[i]) / (times[i + 1] - times[i]);
        values[i] + (values[i + 1] - values[i]) * f
    }

    #[test]
    fn a_long_or_fast_wiggle_still_moves_every_unit_and_follows_the_noise() {
        // Past what 96 points can draw exactly: 180 and 300 new values, and 900 of them.
        for (rate, duration) in [(6.0, 30.0), (8.0, 30.0), (30.0, 10.0), (30.0, 30.0), (12.0, 20.0)] {
            let a = wiggling(rate, duration);
            let (taken, next) = ids();
            let mut src = IdSource { taken: &taken, next };
            for unit in 0..12 {
                let w = wrap(&a, unit, 12, &geom(), &mut src).unwrap();
                assert_eq!(w.animations, 2, "rate {rate} for {duration} s, unit {unit}: {}", w.open);
                let (times, rotation) = polyline(&w.open, "rotate");
                assert!(times.len() <= MAX_KEYS, "{}", times.len());
                assert_eq!((times[0], times[times.len() - 1]), (0.0, 1.0));
                // What it draws is the noise at the points it kept: it swings as far as the noise does, and
                // never further, and sits on it at each of them.
                let signal: Vec<f64> = (0..=(a.end() * 120.0) as usize)
                    .map(|i| a.properties.rotation * amount_at(&a, unit, 12, i as f64 / 120.0))
                    .collect();
                let reach = |v: &[f64]| v.iter().fold(0.0f64, |m, x| m.max(x.abs()));
                let (want, got) = (reach(&signal), reach(&rotation));
                assert!(
                    got > 0.8 * want && got <= want + 0.01,
                    "rate {rate} for {duration} s, unit {unit}: swings {got} of {want}"
                );
                for (q, v) in times.iter().zip(&rotation) {
                    // (The times are written to four places: the nearest grid point is the one meant.)
                    let t = (q * a.end() * 120.0).round() / 120.0;
                    let truth = a.properties.rotation * amount_at(&a, unit, 12, t);
                    assert!((v - truth).abs() < 0.01, "at {q}: {v} vs {truth}");
                }
            }
        }
    }

    #[test]
    fn a_wiggle_within_the_points_it_may_have_stays_within_the_tolerance() {
        // 18 new values over 3 seconds: all the points it needs fit, so nothing is loosened.
        let a = wiggling(6.0, 3.0);
        let (taken, next) = ids();
        let mut src = IdSource { taken: &taken, next };
        for unit in 0..6 {
            let w = wrap(&a, unit, 6, &geom(), &mut src).unwrap();
            let (times, rotation) = polyline(&w.open, "rotate");
            assert!(times.len() < MAX_KEYS, "{}", times.len());
            for i in 0..=360 {
                let t = f64::from(i) / 120.0;
                let drawn = at_time(&times, &rotation, t / a.end());
                let truth = a.properties.rotation * amount_at(&a, unit, 6, t);
                assert!((drawn - truth).abs() <= 0.2 + 0.01, "unit {unit} at {t}: {drawn} vs {truth}");
            }
        }
    }

    #[test]
    fn simplifying_keeps_the_points_that_cut_the_worst_error_within_the_budget() {
        let tol = [1.0; N];
        let mut active = [false; N];
        active[0] = true;
        let samples = |f: &dyn Fn(f64) -> f64| -> Vec<Sample> {
            (0..=7_200)
                .map(|i| {
                    let t = f64::from(i) / 120.0;
                    let mut v = [0.0; N];
                    v[0] = f(t);
                    Sample { t, v }
                })
                .collect()
        };
        // A straight line is its two ends; a triangle wave needs a point at each corner.
        assert_eq!(simplify(&samples(&|t| 3.0 * t), &tol, &active, MAX_KEYS), vec![0, 7_200]);
        let tri = |t: f64| 50.0 * (1.0 - (2.0 * (t / 10.0 - (t / 10.0).floor()) - 1.0).abs());
        let corners = simplify(&samples(&tri), &tol, &active, MAX_KEYS);
        assert_eq!(corners.len(), 2 * 6 + 1, "{corners:?}");
        // Noise no 96 points follow keeps 96, the ends among them, and is the same every time.
        let noise = |t: f64| 40.0 * (hash01(1, 0, (t * 30.0) as i64) - 0.5);
        let noisy = simplify(&samples(&noise), &tol, &active, MAX_KEYS);
        assert_eq!(noisy.len(), MAX_KEYS);
        assert_eq!((noisy[0], noisy[MAX_KEYS - 1]), (0, 7_200));
        assert_eq!(noisy, simplify(&samples(&noise), &tol, &active, MAX_KEYS));
        // A smaller budget keeps a prefix of the order in which points were chosen: its points are among these.
        let fewer = simplify(&samples(&noise), &tol, &active, 24);
        assert_eq!(fewer.len(), 24);
        assert!(fewer.iter().all(|i| noisy.contains(i)));
    }

    /// The group a unit gets, read back as an SVG document: the animations are ones the reader keeps, and what
    /// they say at a moment is where the unit is then.
    #[test]
    fn what_a_unit_gets_is_smil_the_reader_evaluates() {
        let a = text_animation_preset("spin_in").unwrap().animation;
        let (taken, next) = ids();
        let mut src = IdSource { taken: &taken, next };
        let w = wrap(&a, 0, 4, &geom(), &mut src).unwrap();
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\">{}<rect width=\"20\" height=\"20\"/></g></svg>",
            w.open
        );
        let doc = SvgDoc::parse(&svg).unwrap();
        assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
        assert!(doc.is_animated());
        let start = doc.at(0.0);
        let end = doc.at(a.end() + 1.0);
        assert!(start.contains("opacity:0"), "{start}");
        assert!(start.contains("rotate(-90"), "{start}");
        assert!(end.contains("opacity:1") && end.contains("rotate(0)"), "{end}");
        assert!(!end.contains("<animate"), "{end}");
    }
}
