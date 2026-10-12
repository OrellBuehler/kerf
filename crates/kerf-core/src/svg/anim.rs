//! SMIL timing for the subset: clock values, `begin` / `end` lists, intervals and the animation
//! function. Pure; nothing here touches XML.
//!
//! **Intervals are worked out once**, when the document is read: a syncbase (`a.end + 1s`) is the
//! begin or finite end of another animation's intervals, resolved recursively (memoised, 32 deep, a
//! cycle is an unresolved time). Per frame an animation only looks up which interval `t` falls in.
//! What is not evaluated — events, `repeat()`, `accessKey()`, wallclock — is an unresolved time and a
//! warning, never an error.

use std::collections::HashMap;

use super::values::{self, Value};

pub(super) const INF: f64 = f64::INFINITY;
const EPS: f64 = 1e-9;
const MAX_TIMES: usize = 64;
const MAX_DEPTH: usize = 32;

pub(super) fn parse_clock(s: &str) -> Option<f64> {
    let s = s.trim();
    let (neg, body) = match s.as_bytes().first()? {
        b'-' => (true, s[1..].trim_start()),
        b'+' => (false, s[1..].trim_start()),
        _ => (false, s),
    };
    if body.is_empty() {
        return None;
    }
    let secs = if body.contains(':') {
        let parts: Vec<&str> = body.split(':').collect();
        if !(2..=3).contains(&parts.len()) {
            return None;
        }
        let mut total = 0.0;
        for (i, p) in parts.iter().enumerate() {
            let last = i == parts.len() - 1;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit() || (last && b == b'.')) {
                return None;
            }
            total = total * 60.0 + p.parse::<f64>().ok()?;
        }
        total
    } else {
        let (num, unit) = match body.find(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E') {
            Some(i) => (&body[..i], &body[i..]),
            None => (body, "s"),
        };
        let scale = match unit.trim() {
            "h" => 3600.0,
            "min" => 60.0,
            "s" => 1.0,
            "ms" => 0.001,
            _ => return None,
        };
        values::parse_number(num)? * scale
    };
    let secs = if neg { -secs } else { secs };
    secs.is_finite().then_some(secs)
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum TimeItem {
    Offset(f64),
    Sync { id: String, end: bool, offset: f64 },
    Indefinite,
    Unresolved(String),
}

pub(super) fn parse_time_list(s: &str) -> Vec<TimeItem> {
    s.split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(parse_time_item)
        .collect()
}

fn parse_time_item(t: &str) -> TimeItem {
    if t == "indefinite" {
        return TimeItem::Indefinite;
    }
    if let Some(c) = parse_clock(t) {
        return TimeItem::Offset(c);
    }
    for (needle, end) in [(".begin", false), (".end", true)] {
        let mut from = 0;
        while let Some(i) = t[from..].find(needle) {
            let at = from + i;
            let rest = &t[at + needle.len()..];
            let r = rest.trim_start();
            if (rest.is_empty() || r.starts_with(['+', '-'])) && at > 0 {
                let offset = if r.is_empty() { Some(0.0) } else { parse_clock(r) };
                if let Some(offset) = offset {
                    return TimeItem::Sync {
                        id: t[..at].replace('\\', ""),
                        end,
                        offset,
                    };
                }
            }
            from = at + needle.len();
        }
    }
    TimeItem::Unresolved(t.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Restart {
    Always,
    WhenNotActive,
    Never,
}

/// Everything an animation element says about *when*.
#[derive(Debug, Clone)]
pub(super) struct Spec {
    pub id: Option<String>,
    pub begin: Vec<TimeItem>,
    pub end: Vec<TimeItem>,
    pub has_end: bool,
    /// The simple duration; `None` is indefinite.
    pub dur: Option<f64>,
    pub repeat_count: Option<f64>,
    pub repeat_dur: Option<f64>,
    pub min: f64,
    pub max: f64,
    pub restart: Restart,
    pub freeze: bool,
}

impl Spec {
    /// The active duration before `end` and `min` / `max`: `dur` times `repeatCount`, or `repeatDur`,
    /// whichever is shorter.
    fn repeated(&self) -> f64 {
        let by_count = self.dur.zip(self.repeat_count).map(|(d, c)| d * c);
        match (self.dur, by_count, self.repeat_dur) {
            (None, _, rd) => rd.unwrap_or(INF),
            (Some(d), None, None) => d,
            (Some(_), Some(c), None) => c,
            (Some(_), None, Some(r)) => r,
            (Some(_), Some(c), Some(r)) => c.min(r),
        }
    }

    /// Repeating without end: the document never settles.
    pub fn loops_forever(&self) -> bool {
        self.dur.is_some() && self.repeated().is_infinite()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Interval {
    pub begin: f64,
    /// Where the active duration ends (`INF` when it does not).
    pub end: f64,
}

pub(super) struct Resolver<'a> {
    specs: &'a [Spec],
    ids: &'a HashMap<String, usize>,
    memo: Vec<Memo>,
    warnings: Vec<String>,
}

enum Memo {
    New,
    Busy,
    Done(Vec<Interval>),
}

impl<'a> Resolver<'a> {
    pub fn new(specs: &'a [Spec], ids: &'a HashMap<String, usize>) -> Self {
        Self {
            specs,
            ids,
            memo: specs.iter().map(|_| Memo::New).collect(),
            warnings: Vec::new(),
        }
    }

    pub fn resolve(mut self) -> (Vec<Vec<Interval>>, Vec<String>) {
        let all = (0..self.specs.len()).map(|i| self.intervals(i, 0)).collect();
        (all, self.warnings)
    }

    fn warn(&mut self, msg: String) {
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }

    fn intervals(&mut self, i: usize, depth: usize) -> Vec<Interval> {
        match &self.memo[i] {
            Memo::Done(v) => return v.clone(),
            Memo::Busy => {
                self.warn("animation timing refers back to itself; those times were ignored".to_string());
                return Vec::new();
            }
            Memo::New => {}
        }
        if depth > MAX_DEPTH {
            self.warn("animation timing chains are nested too deeply; the deepest were ignored".to_string());
            return Vec::new();
        }
        self.memo[i] = Memo::Busy;
        let out = self.compute(i, depth);
        self.memo[i] = Memo::Done(out.clone());
        out
    }

    fn times(&mut self, items: &[TimeItem], depth: usize) -> Vec<f64> {
        let mut out = Vec::new();
        for item in items {
            match item {
                TimeItem::Offset(o) => out.push(*o),
                TimeItem::Indefinite => {}
                TimeItem::Unresolved(s) => self.warn(format!(
                    "timing \"{s}\" is not evaluated (events, repeat(), accessKey() and wallclock never fire)"
                )),
                TimeItem::Sync { id, end, offset } => match self.ids.get(id).copied() {
                    None => self.warn(format!("timing refers to \"{id}\", which is not an animation")),
                    Some(j) => {
                        for iv in self.intervals(j, depth + 1) {
                            let t = if *end { iv.end } else { iv.begin };
                            if t.is_finite() {
                                out.push(t + offset);
                            }
                        }
                    }
                },
            }
        }
        out.sort_by(f64::total_cmp);
        out.dedup_by(|a, b| (*a - *b).abs() < EPS);
        out.truncate(MAX_TIMES);
        out
    }

    fn compute(&mut self, i: usize, depth: usize) -> Vec<Interval> {
        let spec = self.specs[i].clone();
        let begins = self.times(&spec.begin, depth);
        let ends = if spec.has_end {
            self.times(&spec.end, depth)
        } else {
            Vec::new()
        };
        let open_end = !spec.has_end
            || spec
                .end
                .iter()
                .any(|e| matches!(e, TimeItem::Indefinite | TimeItem::Unresolved(_)));
        let natural = spec.repeated();
        let mut out: Vec<Interval> = Vec::new();
        for (k, &b) in begins.iter().enumerate() {
            match spec.restart {
                Restart::Never if !out.is_empty() => break,
                Restart::WhenNotActive if out.last().is_some_and(|p| b < p.end) => continue,
                _ => {}
            }
            let cap = ends.iter().copied().find(|e| *e > b + EPS);
            if cap.is_none() && !open_end {
                continue;
            }
            let mut e = b + natural;
            if let Some(c) = cap {
                e = e.min(c);
            }
            let mut ad = e - b;
            if spec.max < ad && spec.max >= spec.min {
                ad = spec.max;
            }
            if spec.min > ad && spec.min <= spec.max {
                ad = spec.min;
            }
            let mut end = b + ad;
            if spec.restart == Restart::Always {
                if let Some(next) = begins.get(k + 1) {
                    end = end.min(*next);
                }
            }
            if end > b {
                out.push(Interval { begin: b, end });
            }
        }
        out
    }
}

/// Where in its simple duration an animation is at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Sample {
    /// When the interval it is in began, for the sandwich order.
    pub begin: f64,
    pub iter: u32,
    pub frac: f64,
}

#[derive(Debug, Clone)]
pub(super) struct Timing {
    pub intervals: Vec<Interval>,
    pub dur: Option<f64>,
    /// How long it can play at all (`dur` x `repeatCount` / `repeatDur`).
    repeated: f64,
    freeze: bool,
}

impl Timing {
    pub fn new(spec: &Spec, intervals: Vec<Interval>) -> Self {
        Self {
            intervals,
            dur: spec.dur,
            repeated: spec.repeated(),
            freeze: spec.freeze,
        }
    }

    /// A timing that never begins (until the document's intervals are resolved).
    pub fn none() -> Self {
        Self {
            intervals: Vec::new(),
            dur: None,
            repeated: INF,
            freeze: false,
        }
    }

    pub fn sample(&self, t: f64) -> Option<Sample> {
        let iv = self.intervals.iter().rev().find(|iv| iv.begin <= t + EPS)?;
        if t < iv.end - EPS {
            Some(self.position(iv.begin, t - iv.begin, false))
        } else if self.freeze {
            Some(self.position(iv.begin, iv.end - iv.begin, true))
        } else {
            None
        }
    }

    /// The `(iteration, fraction)` `local` seconds in; at `held` the end of an exact multiple is the
    /// *last* value of the last iteration, not the first of the next.
    fn position(&self, begin: f64, local: f64, held: bool) -> Sample {
        let Some(d) = self.dur else {
            return Sample {
                begin,
                iter: 0,
                frac: 0.0,
            };
        };
        let (local, held) = if local >= self.repeated - EPS {
            (self.repeated.min(local), true)
        } else {
            (local, held)
        };
        let mut q = local / d;
        let r = q.round();
        if (q - r).abs() < 1e-9 {
            q = r;
        }
        if held && q >= 1.0 && q == r {
            return Sample {
                begin,
                iter: (q - 1.0) as u32,
                frac: 1.0,
            };
        }
        let iter = q.floor();
        Sample {
            begin,
            iter: iter as u32,
            frac: q - iter,
        }
    }
}

// ---- the animation function ------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Calc {
    Linear,
    Discrete,
    Paced,
    Spline,
}

/// The value `frac` of the way through a simple duration.
pub(super) fn curve(vals: &[Value], key_times: Option<&[f64]>, splines: Option<&[[f64; 4]]>, calc: Calc, frac: f64) -> Value {
    let n = vals.len();
    if n == 0 {
        return Value::Text(String::new());
    }
    if n == 1 || frac >= 1.0 {
        return vals[n - 1].clone();
    }
    let frac = frac.max(0.0);
    let calc = if vals.windows(2).all(|w| values::compatible(&w[0], &w[1])) {
        calc
    } else {
        Calc::Discrete
    };
    let (i, p) = match calc {
        Calc::Discrete => {
            let i = match key_times {
                Some(k) => k.iter().rposition(|t| *t <= frac).unwrap_or(0),
                None => ((frac * n as f64) as usize).min(n - 1),
            };
            return vals[i].clone();
        }
        Calc::Paced => match paced(vals, frac) {
            Some(hit) => hit,
            None => linear_segment(n, key_times, frac),
        },
        Calc::Linear | Calc::Spline => linear_segment(n, key_times, frac),
    };
    let p = match (calc, splines) {
        (Calc::Spline, Some(s)) if i < s.len() => values::spline_ease(p, s[i]),
        _ => p,
    };
    values::lerp(&vals[i], &vals[i + 1], p)
}

fn linear_segment(n: usize, key_times: Option<&[f64]>, frac: f64) -> (usize, f64) {
    match key_times {
        Some(k) => {
            let i = k.iter().rposition(|t| *t <= frac).unwrap_or(0).min(n - 2);
            let span = k[i + 1] - k[i];
            (
                i,
                if span > 0.0 {
                    ((frac - k[i]) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                },
            )
        }
        None => {
            let x = frac * (n - 1) as f64;
            let i = (x.floor() as usize).min(n - 2);
            (i, x - i as f64)
        }
    }
}

fn paced(vals: &[Value], frac: f64) -> Option<(usize, f64)> {
    let d: Vec<f64> = vals
        .windows(2)
        .map(|w| values::distance(&w[0], &w[1]))
        .collect::<Option<_>>()?;
    let total: f64 = d.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let mut at = frac * total;
    for (i, seg) in d.iter().enumerate() {
        if at <= *seg || i == d.len() - 1 {
            return Some((i, if *seg > 0.0 { (at / seg).clamp(0.0, 1.0) } else { 0.0 }));
        }
        at -= seg;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            id: None,
            begin: vec![TimeItem::Offset(0.0)],
            end: Vec::new(),
            has_end: false,
            dur: Some(2.0),
            repeat_count: None,
            repeat_dur: None,
            min: 0.0,
            max: INF,
            restart: Restart::Always,
            freeze: false,
        }
    }

    fn timing(spec: &Spec) -> Timing {
        let ids = HashMap::new();
        let specs = [spec.clone()];
        let (mut all, _) = Resolver::new(&specs, &ids).resolve();
        Timing::new(spec, all.remove(0))
    }

    fn num(v: f64) -> Value {
        Value::Len(v, "")
    }

    #[test]
    fn clock_values_parse() {
        for (s, want) in [
            ("2", Some(2.0)),
            ("2s", Some(2.0)),
            ("1.5s", Some(1.5)),
            ("500ms", Some(0.5)),
            ("2min", Some(120.0)),
            ("1h", Some(3600.0)),
            ("00:01.5", Some(1.5)),
            ("01:02:03", Some(3723.0)),
            ("1:30", Some(90.0)),
            ("-2s", Some(-2.0)),
            ("+ 2s", Some(2.0)),
            ("- 500ms", Some(-0.5)),
            ("  3  ", Some(3.0)),
            ("1e1s", Some(10.0)),
            ("", None),
            ("s", None),
            ("1:2:3:4", None),
            ("1x", None),
            ("click", None),
            ("1:xx", None),
            ("nan", None),
        ] {
            assert_eq!(parse_clock(s), want, "{s:?}");
        }
    }

    #[test]
    fn time_lists_split_into_offsets_syncbases_and_the_unresolved() {
        use TimeItem::*;
        assert_eq!(
            parse_time_list("0s; a.end + 1s ;b.begin-500ms; indefinite; click; rect.mouseover; repeat(2)"),
            vec![
                Offset(0.0),
                Sync {
                    id: "a".into(),
                    end: true,
                    offset: 1.0
                },
                Sync {
                    id: "b".into(),
                    end: false,
                    offset: -0.5
                },
                Indefinite,
                Unresolved("click".into()),
                Unresolved("rect.mouseover".into()),
                Unresolved("repeat(2)".into()),
            ]
        );
        assert_eq!(
            parse_time_list("x.begin"),
            vec![Sync {
                id: "x".into(),
                end: false,
                offset: 0.0
            }]
        );
        assert_eq!(parse_time_list(" ;; "), vec![]);
        assert_eq!(parse_time_list("a.beginx"), vec![Unresolved("a.beginx".into())]);
        assert_eq!(parse_time_list(".begin"), vec![Unresolved(".begin".into())]);
    }

    #[test]
    fn plain_interval_and_fill() {
        let mut s = spec();
        s.begin = vec![TimeItem::Offset(1.0)];
        let t = timing(&s);
        assert_eq!(t.sample(0.5), None);
        let a = t.sample(2.0).unwrap();
        assert_eq!((a.iter, a.frac), (0, 0.5));
        assert_eq!(t.sample(3.0), None, "removed once over");
        s.freeze = true;
        let t = timing(&s);
        let held = t.sample(10.0).unwrap();
        assert_eq!((held.iter, held.frac), (0, 1.0), "freeze holds the last value");
    }

    #[test]
    fn repeats_count_and_freeze_where_they_stop() {
        let mut s = spec();
        s.repeat_count = Some(2.5);
        s.freeze = true;
        let t = timing(&s);
        let a = t.sample(4.5).unwrap();
        assert_eq!(a.iter, 2);
        assert!((a.frac - 0.25).abs() < 1e-9);
        let held = t.sample(100.0).unwrap();
        assert_eq!(held.iter, 2);
        assert!((held.frac - 0.5).abs() < 1e-9, "frozen mid-iteration at AD mod dur");
        s.repeat_count = Some(2.0);
        let held = timing(&s).sample(100.0).unwrap();
        assert_eq!((held.iter, held.frac), (1, 1.0), "an exact multiple holds the last value");
        s.repeat_count = Some(INF);
        let t = timing(&s);
        assert_eq!(t.sample(1001.0).unwrap().iter, 500);
        assert!(s.loops_forever());
    }

    #[test]
    fn repeat_dur_limits_and_dur_indefinite_is_a_still_start() {
        let mut s = spec();
        s.repeat_dur = Some(3.0);
        s.freeze = true;
        let t = timing(&s);
        assert_eq!(t.sample(2.5).unwrap().iter, 1);
        let held = t.sample(50.0).unwrap();
        assert_eq!(held.iter, 1);
        assert!((held.frac - 0.5).abs() < 1e-9);
        s.dur = None;
        let t = timing(&s);
        assert_eq!(t.sample(1.0).map(|x| (x.iter, x.frac)), Some((0, 0.0)));
        assert_eq!(timing(&s).intervals[0].end, 3.0);
    }

    #[test]
    fn float_residue_at_an_iteration_boundary_does_not_flicker() {
        let mut s = spec();
        s.dur = Some(0.2);
        s.repeat_count = Some(INF);
        let t = timing(&s).sample(0.6).unwrap();
        assert_eq!((t.iter, t.frac), (3, 0.0));
    }

    #[test]
    fn end_clips_and_min_max_clamp() {
        let mut s = spec();
        s.end = vec![TimeItem::Offset(1.0)];
        s.has_end = true;
        assert_eq!(timing(&s).intervals[0].end, 1.0);
        s.end = vec![TimeItem::Offset(-1.0)];
        assert!(timing(&s).intervals.is_empty(), "an end before the begin is no interval");
        s.end = vec![TimeItem::Offset(-1.0), TimeItem::Unresolved("click".into())];
        assert_eq!(timing(&s).intervals[0].end, 2.0, "an event end may never come");
        let mut s = spec();
        s.max = 1.5;
        assert_eq!(timing(&s).intervals[0].end, 1.5);
        let mut s = spec();
        s.min = 5.0;
        assert_eq!(timing(&s).intervals[0].end, 5.0);
        s.min = 5.0;
        s.max = 1.0;
        assert_eq!(timing(&s).intervals[0].end, 2.0, "min above max: both ignored");
    }

    #[test]
    fn negative_begin_starts_in_the_past() {
        let mut s = spec();
        s.begin = vec![TimeItem::Offset(-1.0)];
        let t = timing(&s);
        let a = t.sample(0.0).unwrap();
        assert_eq!(a.frac, 0.5);
        assert_eq!(t.intervals[0].end, 1.0);
    }

    #[test]
    fn restart_modes_choose_which_begins_count() {
        let mut s = spec();
        s.begin = vec![TimeItem::Offset(0.0), TimeItem::Offset(1.0), TimeItem::Offset(5.0)];
        let always = timing(&s);
        assert_eq!(
            always.intervals,
            vec![
                Interval { begin: 0.0, end: 1.0 },
                Interval { begin: 1.0, end: 3.0 },
                Interval { begin: 5.0, end: 7.0 }
            ]
        );
        s.restart = Restart::WhenNotActive;
        assert_eq!(timing(&s).intervals.len(), 2);
        s.restart = Restart::Never;
        assert_eq!(timing(&s).intervals.len(), 1);
    }

    #[test]
    fn syncbases_chain_and_cycles_are_unresolved() {
        let mut a = spec();
        a.id = Some("a".into());
        a.begin = vec![TimeItem::Offset(1.0)];
        let mut b = spec();
        b.id = Some("b".into());
        b.dur = Some(0.5);
        b.begin = parse_time_list("a.end + 0.25s");
        let mut c = spec();
        c.begin = parse_time_list("b.begin");
        let ids: HashMap<String, usize> = [("a".to_string(), 0), ("b".to_string(), 1)].into();
        let (all, warnings) = Resolver::new(&[a, b, c], &ids).resolve();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(all[0][0], Interval { begin: 1.0, end: 3.0 });
        assert_eq!(all[1][0], Interval { begin: 3.25, end: 3.75 });
        assert_eq!(all[2][0].begin, 3.25);

        let mut x = spec();
        x.id = Some("x".into());
        x.begin = parse_time_list("y.end");
        let mut y = spec();
        y.id = Some("y".into());
        y.begin = parse_time_list("x.end");
        let ids: HashMap<String, usize> = [("x".to_string(), 0), ("y".to_string(), 1)].into();
        let (all, warnings) = Resolver::new(&[x, y], &ids).resolve();
        assert!(all.iter().all(Vec::is_empty));
        assert!(warnings.iter().any(|w| w.contains("refers back")), "{warnings:?}");

        let mut z = spec();
        z.begin = parse_time_list("nowhere.begin");
        let (all, warnings) = Resolver::new(&[z], &HashMap::new()).resolve();
        assert!(all[0].is_empty());
        assert!(warnings[0].contains("nowhere"));
    }

    #[test]
    fn a_long_syncbase_chain_stops_at_the_depth_limit() {
        let n = 40;
        let mut specs = Vec::new();
        let mut ids = HashMap::new();
        for i in 0..n {
            let mut s = spec();
            s.id = Some(format!("a{i}"));
            s.begin = if i == n - 1 {
                vec![TimeItem::Offset(0.0)]
            } else {
                parse_time_list(&format!("a{}.end", i + 1))
            };
            ids.insert(format!("a{i}"), i);
            specs.push(s);
        }
        let (all, warnings) = Resolver::new(&specs, &ids).resolve();
        assert!(all[0].is_empty(), "the head of a 40-deep chain is unresolved");
        assert_eq!(all[n - 1][0].begin, 0.0);
        assert!(warnings.iter().any(|w| w.contains("deeply")), "{warnings:?}");
        let (all, warnings) = Resolver::new(&specs[20..], &ids_from(20, n)).resolve();
        assert_eq!(all[0][0].begin, 2.0 * 19.0, "a chain within the limit resolves");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    fn ids_from(from: usize, n: usize) -> HashMap<String, usize> {
        (from..n).map(|i| (format!("a{i}"), i - from)).collect()
    }

    #[test]
    fn unresolved_times_warn_and_never_begin() {
        let mut s = spec();
        s.begin = parse_time_list("click");
        let (all, warnings) = Resolver::new(&[s], &HashMap::new()).resolve();
        assert!(all[0].is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn curves_interpolate_step_and_pace() {
        let v = [num(0.0), num(10.0), num(30.0)];
        let at = |calc, kt: Option<&[f64]>, f| match curve(&v, kt, None, calc, f) {
            Value::Len(n, _) => n,
            other => panic!("{other:?}"),
        };
        assert_eq!(at(Calc::Linear, None, 0.25), 5.0);
        assert_eq!(at(Calc::Linear, None, 0.75), 20.0);
        assert_eq!(at(Calc::Linear, None, 1.0), 30.0);
        assert_eq!(at(Calc::Discrete, None, 0.2), 0.0);
        assert_eq!(at(Calc::Discrete, None, 0.5), 10.0);
        assert_eq!(at(Calc::Discrete, None, 0.99), 30.0);
        assert_eq!(at(Calc::Linear, Some(&[0.0, 0.2, 1.0]), 0.1), 5.0);
        assert_eq!(at(Calc::Linear, Some(&[0.0, 0.2, 1.0]), 0.6), 20.0);
        assert_eq!(at(Calc::Discrete, Some(&[0.0, 0.2, 0.5]), 0.3), 10.0);
        assert_eq!(at(Calc::Discrete, Some(&[0.0, 0.2, 0.5]), 0.6), 30.0);
        // paced: constant speed over the distance 10 + 20, so 1/3 of the time is the first segment
        assert!((at(Calc::Paced, None, 1.0 / 3.0) - 10.0).abs() < 1e-9);
        assert!((at(Calc::Paced, None, 0.5) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn spline_easing_applies_per_segment() {
        let v = [num(0.0), num(100.0)];
        let ease = [[0.42, 0.0, 1.0, 1.0]];
        let Value::Len(early, _) = curve(&v, None, Some(&ease), Calc::Spline, 0.5) else {
            panic!()
        };
        assert!(early < 50.0, "an ease-in lags the line: {early}");
        let lin = [[0.0, 0.0, 1.0, 1.0]];
        let Value::Len(mid, _) = curve(&v, None, Some(&lin), Calc::Spline, 0.5) else {
            panic!()
        };
        assert!((mid - 50.0).abs() < 1e-6);
    }

    #[test]
    fn incompatible_values_step() {
        let v = [Value::Text("hidden".into()), Value::Text("visible".into())];
        assert_eq!(curve(&v, None, None, Calc::Linear, 0.4), v[0]);
        assert_eq!(curve(&v, None, None, Calc::Linear, 0.5), v[1]);
        let m = [num(1.0), Value::Len(1.0, "%")];
        assert_eq!(curve(&m, None, None, Calc::Linear, 0.2), m[0]);
    }
}
