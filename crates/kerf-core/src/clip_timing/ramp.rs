//! A speed ramp in the export graph: the picture's retime expression and the audio's steps.
//!
//! The model decides what a ramp *is* (`Clip::along` and `Clip::local_at_along`, in
//! `model/speed_ramp.rs`); this is what the chains print for it.
//!
//! **Picture.** `setpts` maps each frame's source seconds `r` (`(PTS-STARTPTS)*TB`: from the first
//! frame kept, which `reverse` has already put in playing order) to clip-local timeline seconds, the
//! map's inverse in closed form: a hold is `r / v`, a linear segment `2x / (v + sqrt(v² + 2kx))`.
//! The regions between keys are a **balanced tree** of `if(lte(…))` (an expression may nest about a
//! hundred levels, and a chain of sixteen keys would be sixteen of them), and the value is stored once
//! with `st(0, …)` and read back with `ld(0)`. [`Ex`] is both the text and the arithmetic, so the tests
//! evaluate exactly what the graph prints (and a frame pick over a ramp, which the plan does not model
//! yet, would too).
//!
//! **Sound.** `atempo` takes one tempo for its whole stream, and its `tempo` command is no way round
//! that: each command re-origins the stream (it drops the drift control that holds the long-run tempo,
//! losing half a window of source, 1024 samples, per command on transient material; measured on 6.1 and
//! 9.0). So a ramp is cut into steps of constant tempo, each its own `atempo` stream
//! over the part of the source it plays, trimmed to the exact sample count of its share of the timeline
//! and crossfaded into the next ([`audio_pieces`]). Each step's tempo is its source span over its
//! timeline span, so the sound meets the picture at every step edge whatever `atempo` does inside one.

use std::fmt;

use crate::model::{Clip, RampEasing};

// ---- the picture --------------------------------------------------------------------------------

/// The retime expression of a ramp as a tree: printed, it is what `setpts` evaluates; evaluated, it is
/// what that prints. Every binary operation is parenthesised, so nothing depends on FFmpeg's precedence.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Ex {
    Num(f64),
    /// `ld(0)`: the source seconds travelled.
    Along,
    Add(Box<Ex>, Box<Ex>),
    Sub(Box<Ex>, Box<Ex>),
    Mul(Box<Ex>, Box<Ex>),
    Div(Box<Ex>, Box<Ex>),
    Sqrt(Box<Ex>),
    /// `if(lte(ld(0),at),yes,no)`.
    Branch {
        at: f64,
        yes: Box<Ex>,
        no: Box<Ex>,
    },
}

fn num(v: f64) -> Ex {
    Ex::Num(v)
}

fn bin(op: fn(Box<Ex>, Box<Ex>) -> Ex, a: Ex, b: Ex) -> Ex {
    op(Box::new(a), Box::new(b))
}

impl Ex {
    /// The expression's value with the source seconds travelled `r`, in the order FFmpeg evaluates it.
    #[cfg(test)]
    pub(crate) fn eval(&self, r: f64) -> f64 {
        match self {
            Ex::Num(v) => *v,
            Ex::Along => r,
            Ex::Add(a, b) => a.eval(r) + b.eval(r),
            Ex::Sub(a, b) => a.eval(r) - b.eval(r),
            Ex::Mul(a, b) => a.eval(r) * b.eval(r),
            Ex::Div(a, b) => a.eval(r) / b.eval(r),
            Ex::Sqrt(a) => a.eval(r).sqrt(),
            Ex::Branch { at, yes, no } => {
                if r <= *at {
                    yes.eval(r)
                } else {
                    no.eval(r)
                }
            }
        }
    }

    /// The deepest nesting of `if`s: the tree's height.
    #[cfg(test)]
    pub(crate) fn depth(&self) -> usize {
        match self {
            Ex::Branch { yes, no, .. } => 1 + yes.depth().max(no.depth()),
            Ex::Add(a, b) | Ex::Sub(a, b) | Ex::Mul(a, b) | Ex::Div(a, b) => a.depth().max(b.depth()),
            Ex::Sqrt(a) => a.depth(),
            Ex::Num(_) | Ex::Along => 0,
        }
    }
}

impl fmt::Display for Ex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // `{}` of an `f64` never writes an exponent, and a negative number is a unary minus in
            // parentheses wherever it lands.
            Ex::Num(v) if *v < 0.0 => write!(f, "(-{})", -v),
            Ex::Num(v) => write!(f, "{}", if *v == 0.0 { 0.0 } else { *v }),
            Ex::Along => f.write_str("ld(0)"),
            Ex::Add(a, b) => write!(f, "({a}+{b})"),
            Ex::Sub(a, b) => write!(f, "({a}-{b})"),
            Ex::Mul(a, b) => write!(f, "({a}*{b})"),
            Ex::Div(a, b) => write!(f, "({a}/{b})"),
            Ex::Sqrt(a) => write!(f, "sqrt({a})"),
            Ex::Branch { at, yes, no } => write!(f, "if(lte(ld(0),{}),{yes},{no})", Ex::Num(*at)),
        }
    }
}

/// The clip-local timeline seconds at which `ld(0)` source seconds have been travelled — the inverse
/// of `Clip::along`, as an expression — or `None` for a clip without a ramp.
///
/// Region `j` of `n` keys is `r <= along(key j)` after the regions before it: before the first key and
/// after the last the clip plays at that key's speed, between two it holds or moves linearly.
pub(crate) fn retime_expr(clip: &Clip) -> Option<Ex> {
    let keys = clip.ramp()?;
    let n = keys.len();
    let at: Vec<f64> = keys.iter().map(|k| clip.along(k.time)).collect();
    // `time + (r - from) / speed`, without the terms that are zero.
    let constant = |time: f64, from: f64, speed: f64| -> Ex {
        let x = if from == 0.0 {
            Ex::Along
        } else {
            bin(Ex::Sub, Ex::Along, num(from))
        };
        let run = bin(Ex::Div, x, num(speed));
        if time == 0.0 {
            run
        } else {
            bin(Ex::Add, num(time), run)
        }
    };
    let region = |j: usize| -> Ex {
        if j == 0 || j == n {
            let (key, from) = if j == 0 { (keys[0], at[0]) } else { (keys[n - 1], at[n - 1]) };
            return constant(key.time, from, key.speed);
        }
        let (a, b, from) = (keys[j - 1], keys[j], at[j - 1]);
        if a.easing == RampEasing::Hold || a.speed == b.speed {
            return constant(a.time, from, a.speed);
        }
        // `time + 2x / (v + sqrt(v² + 2kx))`, with `x` the source seconds into the segment.
        let k = (b.speed - a.speed) / (b.time - a.time);
        let x = || {
            if from == 0.0 {
                Ex::Along
            } else {
                bin(Ex::Sub, Ex::Along, num(from))
            }
        };
        let root = Ex::Sqrt(Box::new(bin(
            Ex::Add,
            num(a.speed * a.speed),
            bin(Ex::Mul, num(2.0 * k), x()),
        )));
        let run = bin(Ex::Div, bin(Ex::Mul, num(2.0), x()), bin(Ex::Add, num(a.speed), root));
        if a.time == 0.0 {
            run
        } else {
            bin(Ex::Add, num(a.time), run)
        }
    };
    fn tree(lo: usize, hi: usize, at: &[f64], region: &dyn Fn(usize) -> Ex) -> Ex {
        if lo == hi {
            return region(lo);
        }
        let mid = (lo + hi) / 2;
        Ex::Branch {
            at: at[mid],
            yes: Box::new(tree(lo, mid, at, region)),
            no: Box::new(tree(mid + 1, hi, at, region)),
        }
    }
    if n == 1 {
        return Some(region(0));
    }
    Some(tree(0, n, &at, &region))
}

/// The `setpts` of a ramped clip: the frame's clip-local time from its source seconds travelled, put on
/// the timeline at `start`. `offset` is the source seconds already travelled when the first kept frame
/// plays (a window that starts inside a late-starting file's lead), so that frame lands at
/// `start + local_at_along(offset)`. The value is quoted: it holds commas and a `;`.
pub(crate) fn setpts(clip: &Clip, offset: f64, start: f64) -> Option<String> {
    let expr = retime_expr(clip)?;
    let r = if offset > 0.0 {
        format!("(PTS-STARTPTS)*TB+{offset}")
    } else {
        "(PTS-STARTPTS)*TB".to_string()
    };
    Some(format!("setpts='st(0,{r});({expr}+{start})/TB'"))
}

// ---- the sound ----------------------------------------------------------------------------------

/// The longest a step of a moving tempo is.
const STEP_MAX: f64 = 1.0;
/// The shortest a step of a moving tempo is made: the crossfades between steps need room.
const STEP_MIN: f64 = 0.12;
/// A step shorter than this is merged into its neighbour.
const STEP_FLOOR: f64 = 0.03;
/// How far, in source seconds, a step's constant tempo may stray from the curve inside the step.
const STEP_ERROR: f64 = 0.008;
/// The most steps one clip's sound is cut into.
pub(crate) const MAX_AUDIO_STEPS: usize = 128;

/// One stretch of a ramped clip's sound at one tempo: `t0..t1` clip-local timeline seconds over
/// `a0..a1` source seconds travelled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AudioStep {
    pub t0: f64,
    pub t1: f64,
    pub a0: f64,
    pub a1: f64,
}

impl AudioStep {
    /// Source over timeline seconds: the `atempo` this step plays at.
    pub(crate) fn tempo(&self) -> f64 {
        (self.a1 - self.a0) / (self.t1 - self.t0)
    }
}

/// How the speed moves over `lo..hi`, which holds no key inside: its slope per second.
fn slope_over(clip: &Clip, lo: f64, hi: f64) -> f64 {
    let Some(keys) = clip.ramp() else { return 0.0 };
    let mid = (lo + hi) / 2.0;
    match keys.windows(2).find(|w| w[0].time <= mid && mid < w[1].time) {
        Some(w) if w[0].easing == RampEasing::Linear => (w[1].speed - w[0].speed) / (w[1].time - w[0].time),
        _ => 0.0,
    }
}

/// The steps a ramped clip's sound of `total` timeline seconds (its length with any transition tail,
/// which plays on at the last key's speed) is cut into, in order, covering `0..total` exactly (pure).
/// A hold, the stretch before the first key and the one after the last are one step each; a moving
/// speed is cut finely enough that the steps' constant tempo stays within [`STEP_ERROR`] of the curve
/// inside each (the deviation of a chord from a parabola is `slope · step² / 8`), not shorter than
/// [`STEP_MIN`], and the whole within [`MAX_AUDIO_STEPS`].
pub(crate) fn audio_steps(clip: &Clip, total: f64) -> Vec<AudioStep> {
    let step = |t0: f64, t1: f64| AudioStep {
        t0,
        t1,
        a0: clip.along(t0),
        a1: clip.along(t1),
    };
    let Some(keys) = clip.ramp() else {
        return vec![step(0.0, total)];
    };
    if !(total > 0.0 && total.is_finite()) {
        return vec![AudioStep {
            t0: 0.0,
            t1: total.max(0.0),
            a0: 0.0,
            a1: 0.0,
        }];
    }
    let mut edges = vec![0.0];
    edges.extend(keys.iter().map(|k| k.time).filter(|t| *t > 0.0 && *t < total));
    edges.push(total);
    // The steps of each stretch between edges at a `scale` of the finest step the error allows: counted
    // first, so that a ramp which moves for hours is made coarser before a step of it is allocated.
    let counts = |scale: f64| -> Vec<usize> {
        edges
            .windows(2)
            .map(|w| {
                let slope = slope_over(clip, w[0], w[1]).abs();
                if slope > 0.0 {
                    let h = (8.0 * STEP_ERROR / slope).sqrt().clamp(STEP_MIN, STEP_MAX) * scale;
                    ((w[1] - w[0]) / h).ceil().clamp(1.0, 1e6) as usize
                } else {
                    1
                }
            })
            .collect()
    };
    let mut scale = 1.0;
    let mut per_stretch = counts(scale);
    while per_stretch.iter().sum::<usize>() > MAX_AUDIO_STEPS && scale < 1e12 {
        scale *= 1.5;
        per_stretch = counts(scale);
    }
    let mut bounds = vec![0.0];
    for (w, n) in edges.windows(2).zip(per_stretch) {
        bounds.extend((1..=n).map(|i| w[0] + (w[1] - w[0]) * i as f64 / n as f64));
    }
    // A step too short to crossfade is folded into the one before it (the first into the one after).
    let mut i = 1;
    while bounds.len() > 2 && i < bounds.len() {
        let short_before = bounds[i] - bounds[i - 1] < STEP_FLOOR;
        if short_before && i < bounds.len() - 1 {
            bounds.remove(i);
        } else if short_before {
            bounds.remove(i - 1);
            i -= 1;
        } else {
            i += 1;
        }
    }
    bounds.windows(2).map(|w| step(w[0], w[1])).collect()
}

/// How many `atempo` filters play a stream at `tempo`: one within the filter's `0.5..=2` range,
/// otherwise the fewest that bring each within it. Counted by doubling, not by `log2`: no libm.
fn stage_count(tempo: f64) -> usize {
    let span = tempo.max(1.0 / tempo);
    let mut stages = 1;
    let mut limit = 2.0 * (1.0 + 6.9e-10);
    while stages < MAX_STAGES && span > limit {
        stages += 1;
        limit *= 2.0;
    }
    stages
}

/// The most `atempo` stages a chain is made of: `2^64` is further from 1x than any tempo can be asked for.
const MAX_STAGES: usize = 64;

/// The tempo of each of `stages` equal `atempo` stages that together play `tempo`: its `stages`-th root,
/// found by bisection on the product so that it needs nothing but IEEE `+ - * /` (and `sqrt`) and is the same
/// bit for bit on every platform. `powf` is not: its last digit differs between glibc with and without FMA,
/// macOS, Windows and arm, and that digit is printed in the graph (`atempo=1.933656967885146` or
/// `…463`) and moves the `atrim` a piece is read from.
fn stage_tempo(tempo: f64, stages: usize) -> f64 {
    if stages <= 1 {
        return tempo;
    }
    if stages == 2 {
        return tempo.sqrt();
    }
    let power = |x: f64| (1..stages).fold(x, |p, _| p * x);
    let (mut lo, mut hi) = (tempo.min(1.0), tempo.max(1.0));
    for _ in 0..256 {
        let mid = lo + (hi - lo) / 2.0;
        if mid <= lo || mid >= hi {
            break;
        }
        if power(mid) < tempo {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // The two neighbours that bracket the root: the nearer one, so 8 in three stages is 2 and not a hair under.
    if (tempo - power(lo)).abs() < (power(hi) - tempo).abs() {
        lo
    } else {
        hi
    }
}

/// How much of a stream's start one `atempo` stage plays early, in seconds of the stage's input:
/// every stage carries its content about this far ahead of where its tempo puts it. Measured through
/// pieces of every tempo from 0.25 to 5 on FFmpeg 6.1 and 9.0 (the two agree to the sample; the same
/// early onset a constant-speed clip's `atempo` has), 13 to 19 ms, a few more under 0.5.
const STAGE_ADVANCE: f64 = 0.0145;

/// Source seconds by which a chain of `atempo` at `tempo` plays its content early. Each stage hands the
/// next its output, so a stage's advance counts for the tempo of every stage before it: `A (1 + s + s² …)`
/// for stages of tempo `s`. A step cut out of the middle of a stream is read this much later, so that it
/// plays what the picture shows. Held at its value for 0.25x and for 8x outside them (measured to there:
/// within 8 ms of the map from 0.5x to 8x, 16 at 0.25x; a step that slow or fast is a moment of the
/// source however far it is stretched).
pub(crate) fn tempo_advance(tempo: f64) -> f64 {
    if !(tempo.is_finite() && tempo > 0.0) {
        return 0.0;
    }
    let tempo = tempo.clamp(0.25, 8.0);
    let stages = stage_count(tempo);
    let each = stage_tempo(tempo, stages);
    let (mut sum, mut term) = (0.0, 1.0);
    for _ in 0..stages {
        sum += term;
        term *= each;
    }
    STAGE_ADVANCE * sum
}

/// Whether a step at `tempo` goes through `atempo` at all (a step at 1x is the source as it is).
pub(crate) fn tempo_applies(tempo: f64) -> bool {
    (tempo - 1.0).abs() > 1e-9
}

/// One step of the sound, planned in samples: what to cut from the (reversed, resampled) source and how
/// to trim what `atempo` makes of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AudioPiece {
    /// Source seconds travelled to cut the input from and to: the piece's own span with a lead-in for
    /// `atempo` to settle and a tail for it to flush into.
    pub from: f64,
    pub to: f64,
    pub tempo: f64,
    /// Output samples of `atempo` to drop at the front.
    pub skip: u64,
    /// Silence to put ahead of the output when the piece starts before the source does, in samples.
    pub lead: u64,
    /// Output samples the piece keeps: its share of the timeline, with half of each crossfade on
    /// either side.
    pub keep: u64,
}

/// The seconds of `atempo` output kept ahead of a piece's first sample and after its last, so what the
/// filter makes of the edges of its input is never heard.
const ROLL: f64 = 0.06;
const FLUSH: f64 = 0.1;
/// The longest crossfade between two steps: the length of a click-free splice and no more. The pieces are
/// independent `atempo` streams, so what they play of a steady tone differs in phase at a seam, and the longer
/// the two are mixed the more of that is heard (a 440 Hz tone: 2 ms leaves the seams 28 dB under the tone, 10 ms
/// 22, 40 ms 17).
const CROSSFADE: f64 = 0.002;

/// The steps of a ramped clip's sound as pieces of a stream resampled to `sample_rate`, with the
/// samples each crossfade between neighbours overlaps by (even: half on each side). The pieces put end
/// to end with their crossfades come to exactly `round(total · sample_rate)` samples, and the edge
/// between two lands on the same sample whatever `atempo` made of either (pure).
pub(crate) fn audio_pieces(steps: &[AudioStep], sample_rate: u32) -> (Vec<AudioPiece>, Vec<u64>) {
    let sr = f64::from(sample_rate);
    let n = steps.len();
    let edge = |i: usize| -> u64 {
        if i == 0 {
            0
        } else if i == n {
            (steps[n - 1].t1 * sr).round().max(0.0) as u64
        } else {
            (steps[i].t0 * sr).round().max(0.0) as u64
        }
    };
    let length = |i: usize| edge(i + 1).saturating_sub(edge(i));
    let overlaps: Vec<u64> = (0..n.saturating_sub(1))
        .map(|i| {
            let room = (0.4 * length(i).min(length(i + 1)) as f64) as u64;
            let want = (CROSSFADE * sr) as u64;
            want.min(room) & !1
        })
        .collect();
    let pieces = (0..n)
        .map(|i| {
            let step = steps[i];
            let tempo = step.tempo();
            let behind = if i == 0 { 0 } else { overlaps[i - 1] / 2 };
            let ahead = if i + 1 == n { 0 } else { overlaps[i] / 2 };
            let keep = length(i) + behind + ahead;
            // The source second the first kept sample plays, and where `atempo` has to be read from to
            // have it there once it plays its content early.
            let want = step.a0 - behind as f64 / sr * tempo;
            let early = if tempo_applies(tempo) { tempo_advance(tempo) } else { 0.0 };
            let settle = ROLL * tempo;
            let from = (want - early - settle).max(0.0);
            let skip_s = (want - early - from) / tempo;
            let (skip, lead) = if skip_s >= 0.0 {
                ((skip_s * sr).round() as u64, 0)
            } else {
                (0, (-skip_s * sr).round() as u64)
            };
            let to = from + ((skip + keep) as f64 / sr + FLUSH) * tempo;
            AudioPiece {
                from,
                to,
                tempo,
                skip,
                lead,
                keep,
            }
        })
        .collect();
    (pieces, overlaps)
}

/// The `atempo` filters that play a stream at `tempo`: one instance for a tempo within its `0.5..=2`
/// range, otherwise the fewest that bring each within it, sharing the tempo equally.
pub(crate) fn atempo_stages(tempo: f64) -> String {
    let stages = stage_count(tempo);
    let each = stage_tempo(tempo, stages);
    vec![format!("atempo={each}"); stages].join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SpeedKey, MAX_SPEED_KEYS};
    use uuid::Uuid;

    fn hold(time: f64, speed: f64) -> SpeedKey {
        SpeedKey {
            time,
            speed,
            easing: RampEasing::Hold,
        }
    }

    fn clip(keys: Vec<SpeedKey>, source_len: f64) -> Clip {
        let mut c = Clip::new(Uuid::nil(), 3.0, 3.0 + source_len, 2.0);
        c.speed_ramp = keys;
        c
    }

    /// A ramp of `n` keys from a seed: random speeds in the model's range and a hold now and then.
    fn random_keys(seed: u64, n: usize) -> Vec<SpeedKey> {
        let mut state = seed | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 10_000) as f64 / 10_000.0
        };
        let mut time = next() * 0.5;
        (0..n)
            .map(|_| {
                time += 0.05 + next() * 1.5;
                SpeedKey {
                    time,
                    speed: (0.02 + next() * next() * 30.0).min(100.0),
                    easing: if next() < 0.3 { RampEasing::Hold } else { RampEasing::Linear },
                }
            })
            .collect()
    }

    #[test]
    fn the_expression_is_the_maps_inverse_for_every_shape_of_ramp() {
        let ramps = [
            vec![SpeedKey::new(0.0, 2.0)],
            vec![SpeedKey::new(1.0, 0.5)],
            vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 3.0)],
            vec![SpeedKey::new(0.5, 4.0), SpeedKey::new(1.5, 0.25)],
            vec![hold(0.0, 0.5), SpeedKey::new(1.0, 2.0)],
            vec![
                SpeedKey::new(0.0, 1.0),
                hold(1.0, 2.5),
                SpeedKey::new(2.0, 0.5),
                SpeedKey::new(3.0, 0.5),
            ],
            vec![SpeedKey::new(-0.5, 2.0), SpeedKey::new(1.0, 0.2)],
            vec![
                SpeedKey::new(0.0, 1.0),
                SpeedKey::new(1.0, 1.0),
                SpeedKey::new(2.0, 1.0 + 1e-12),
            ],
        ];
        let mut all: Vec<Vec<SpeedKey>> = ramps.to_vec();
        for seed in 1..=60 {
            all.push(random_keys(seed, 1 + seed as usize % MAX_SPEED_KEYS));
        }
        for keys in all {
            let c = clip(keys.clone(), 40.0);
            let expr = retime_expr(&c).expect("a ramp");
            for i in 0..=400 {
                let along = f64::from(i) * 0.1;
                let want = c.local_at_along(along);
                let got = expr.eval(along);
                assert!(
                    (got - want).abs() <= 1e-9 * want.abs().max(1.0),
                    "{keys:?} at {along}: {got} against {want}"
                );
            }
        }
    }

    #[test]
    fn the_regions_nest_as_a_balanced_tree_and_the_text_stays_small() {
        for n in 1..=MAX_SPEED_KEYS {
            let keys: Vec<SpeedKey> = (0..n).map(|i| SpeedKey::new(i as f64, 1.0 + (i % 5) as f64)).collect();
            let expr = retime_expr(&clip(keys, 100.0)).unwrap();
            // n keys cut the line into n + 1 regions: a tree over them is ceil(log2(n + 1)) high.
            let height = (usize::BITS - n.leading_zeros()) as usize;
            assert!(expr.depth() <= height, "{n} keys: {} deep", expr.depth());
            let text = expr.to_string();
            assert!(text.len() < 400 * (n + 1), "{n} keys: {} bytes", text.len());
            // No exponent, no NaN: FFmpeg's number parser reads neither of those as written.
            assert!(!text.as_bytes().windows(2).any(|w| w[0].is_ascii_digit() && w[1] == b'e'));
            assert!(!text.contains("NaN") && !text.contains("inf"), "{text}");
        }
        let sixteen = random_keys(7, MAX_SPEED_KEYS);
        let expr = retime_expr(&clip(sixteen, 400.0)).unwrap();
        assert_eq!(expr.depth(), 5);
    }

    #[test]
    fn a_ramps_setpts_is_one_quoted_value_that_stores_the_source_seconds_once() {
        let c = clip(vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 3.0)], 10.0);
        let text = setpts(&c, 0.0, 2.0).unwrap();
        assert!(
            text.starts_with("setpts='st(0,(PTS-STARTPTS)*TB);(") && text.ends_with("+2)/TB'"),
            "{text}"
        );
        // Both ends of the quote, and none inside it.
        assert_eq!(text.matches('\'').count(), 2);
        assert!(text.contains("sqrt("));
        // A hold is a division, and a window that starts in a late file's lead starts the clock late.
        let held = clip(vec![hold(0.0, 0.5), SpeedKey::new(2.0, 3.0)], 10.0);
        let text = setpts(&held, 0.25, 0.0).unwrap();
        assert!(text.contains("st(0,(PTS-STARTPTS)*TB+0.25);"), "{text}");
        assert!(!text.contains("sqrt("), "{text}");
        // No ramp, no retime.
        assert_eq!(setpts(&Clip::new(Uuid::nil(), 0.0, 4.0, 0.0), 0.0, 0.0), None);
        let mut malformed = clip(vec![SpeedKey::new(1.0, 2.0), SpeedKey::new(1.0, 3.0)], 4.0);
        malformed.speed = 1.0;
        assert_eq!(setpts(&malformed, 0.0, 0.0), None);
    }

    #[test]
    fn a_numbers_text_is_what_ffmpeg_parses_back() {
        for (v, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (0.1, "0.1"),
            (-2.5, "(-2.5)"),
            (1e-7, "0.0000001"),
            (123456789.5, "123456789.5"),
        ] {
            assert_eq!(Ex::Num(v).to_string(), text);
            let back: f64 = text.trim_matches(['(', ')']).parse().unwrap();
            assert_eq!(back, if v == 0.0 { 0.0 } else { v });
        }
    }

    fn total_of(c: &Clip) -> f64 {
        c.duration()
    }

    #[test]
    fn the_sounds_steps_cover_the_clip_and_meet_the_picture_at_every_edge() {
        let ramps = [
            vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 3.0)],
            vec![
                SpeedKey::new(0.0, 1.0),
                hold(1.5, 2.5),
                SpeedKey::new(2.5, 0.5),
                SpeedKey::new(3.5, 0.5),
            ],
            vec![SpeedKey::new(1.0, 2.0), SpeedKey::new(3.0, 0.5)],
            vec![SpeedKey::new(0.0, 0.01), SpeedKey::new(5.0, 100.0)],
        ];
        let mut all = ramps.to_vec();
        all.extend((1..=40).map(|seed| random_keys(seed, 2 + seed as usize % 15)));
        for keys in all {
            let c = clip(keys.clone(), 30.0);
            for tail in [0.0, 0.7] {
                let total = total_of(&c) + tail;
                let steps = audio_steps(&c, total);
                assert!(
                    !steps.is_empty() && steps.len() <= MAX_AUDIO_STEPS,
                    "{keys:?}: {} steps",
                    steps.len()
                );
                assert_eq!(steps[0].t0, 0.0);
                assert_eq!(steps.last().unwrap().t1, total);
                for w in steps.windows(2) {
                    assert_eq!((w[0].t1, w[0].a1), (w[1].t0, w[1].a0), "{keys:?}: a gap");
                }
                for s in &steps {
                    assert!(s.t1 - s.t0 >= STEP_FLOOR - 1e-12 || steps.len() == 1, "{keys:?}: {s:?}");
                    // The step's tempo is its chord, so its edges are where the map has them.
                    assert!((s.a0 - c.along(s.t0)).abs() < 1e-12 && (s.a1 - c.along(s.t1)).abs() < 1e-12);
                    assert!(s.tempo() > 0.0 && s.tempo().is_finite());
                    // A tempo never strays outside the speeds the step spans.
                    let lo = (0..=8)
                        .map(|i| c.speed_at(s.t0 + (s.t1 - s.t0) * f64::from(i) / 8.0))
                        .fold(f64::MAX, f64::min);
                    let hi = (0..=8)
                        .map(|i| c.speed_at(s.t0 + (s.t1 - s.t0) * f64::from(i) / 8.0))
                        .fold(0.0, f64::max);
                    assert!(
                        s.tempo() >= lo * (1.0 - 1e-9) && s.tempo() <= hi * (1.0 + 1e-9),
                        "{keys:?}: {s:?} {lo} {hi}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_constant_stretch_is_one_step_and_a_moving_one_stays_close_to_its_curve() {
        // A hold, and what lies before the first key and after the last, are a step each.
        let c = clip(
            vec![
                hold(1.0, 2.0),
                hold(2.0, 0.5),
                SpeedKey::new(3.0, 0.5),
                SpeedKey::new(4.0, 0.5),
            ],
            20.0,
        );
        let steps = audio_steps(&c, c.duration());
        let edges: Vec<f64> = steps.iter().map(|s| s.t0).collect();
        assert_eq!(edges, [0.0, 1.0, 2.0, 3.0, 4.0]);
        // 1x to 3x over 4 s: the chord of a step is within STEP_ERROR of the parabola in the middle.
        let c = clip(vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(4.0, 3.0)], 40.0);
        let steps = audio_steps(&c, 6.0);
        assert!(steps.len() > 4, "{steps:?}");
        for s in steps.iter().filter(|s| s.t1 <= 4.0) {
            let mid = (s.t0 + s.t1) / 2.0;
            let chord = (s.a0 + s.a1) / 2.0;
            assert!(
                (chord - c.along(mid)).abs() <= STEP_ERROR * 1.05,
                "{s:?}: {chord} {}",
                c.along(mid)
            );
        }
        // A slope too steep for the step budget loosens the error rather than the count.
        let steep = clip(
            (0..16)
                .map(|i| SpeedKey::new(f64::from(i) * 0.5, if i % 2 == 0 { 0.1 } else { 50.0 }))
                .collect(),
            400.0,
        );
        assert!(audio_steps(&steep, 8.0).len() <= MAX_AUDIO_STEPS);
        // A ramp that moves for minutes lengthens its steps to stay in the budget (the first 16 keys of a
        // long edit: nine seconds a segment, two minutes of it moving).
        let long = clip(
            (0..16)
                .map(|i| SpeedKey::new(f64::from(i) * 9.0, [0.5, 3.0, 1.0, 6.0][i as usize % 4]))
                .collect(),
            590.0,
        );
        let steps = audio_steps(&long, long.duration());
        assert!(steps.len() <= MAX_AUDIO_STEPS && steps.len() >= 16, "{} steps", steps.len());
        assert_eq!(steps.last().unwrap().t1, long.duration());
        // Hostile: a ramp over years, and a length that is not a number, allocate nothing like it.
        let huge = clip(vec![SpeedKey::new(0.0, 0.01), SpeedKey::new(3.0e8, 100.0)], 1e9);
        let steps = audio_steps(&huge, huge.duration());
        assert!(steps.len() <= MAX_AUDIO_STEPS, "{} steps", steps.len());
        for bad in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            assert_eq!(audio_steps(&huge, bad).len(), 1, "{bad}");
        }
    }

    #[test]
    fn the_pieces_with_their_crossfades_come_to_the_exact_sample_count_of_the_clip() {
        for rate in [48_000, 44_100, 96_000, 22_050] {
            for seed in 1..=30 {
                let c = clip(random_keys(seed, 2 + seed as usize % 15), 30.0);
                let total = total_of(&c) + if seed % 2 == 0 { 0.4 } else { 0.0 };
                let steps = audio_steps(&c, total);
                let (pieces, overlaps) = audio_pieces(&steps, rate);
                assert_eq!(pieces.len(), steps.len());
                assert_eq!(overlaps.len(), steps.len() - 1);
                let kept: u64 = pieces.iter().map(|p| p.keep).sum();
                let crossfaded: u64 = overlaps.iter().sum();
                assert_eq!(
                    kept - crossfaded,
                    (total * f64::from(rate)).round() as u64,
                    "{rate} Hz, seed {seed}"
                );
                assert!(overlaps.iter().all(|o| o % 2 == 0 && *o <= (0.0025 * f64::from(rate)) as u64));
                for p in &pieces {
                    assert!(p.to > p.from && p.from >= 0.0 && p.keep > 0, "{p:?}");
                    assert!(p.skip == 0 || p.lead == 0, "{p:?}");
                }
            }
        }
    }

    #[test]
    fn a_piece_is_read_where_atempo_will_have_made_up_its_early_start() {
        let c = clip(vec![SpeedKey::new(0.0, 1.0), hold(2.0, 0.5), SpeedKey::new(4.0, 0.5)], 30.0);
        let steps = audio_steps(&c, c.duration());
        let (pieces, overlaps) = audio_pieces(&steps, 48_000);
        // The first piece starts at the head of the source, so atempo's early start is silence put
        // ahead of it; a piece cut from the middle is read later by the advance and trimmed by the roll.
        assert!(
            pieces[0].lead > 0 && pieces[0].skip == 0 && pieces[0].from == 0.0,
            "{:?}",
            pieces[0]
        );
        let (i, mid) = pieces.iter().enumerate().find(|(_, p)| p.from > 0.0).unwrap();
        // It starts half its crossfade early, and what it plays there is `from` plus the roll and the advance.
        let early = overlaps[i - 1] as f64 / 2.0 / 48_000.0 * mid.tempo;
        let rolled = (mid.skip as f64 / 48_000.0) * mid.tempo;
        assert!(
            (steps[i].a0 - early - tempo_advance(mid.tempo) - mid.from - rolled).abs() < 0.001,
            "{mid:?}"
        );
        // A step at 1x is the source as it is: no atempo, so no advance to make up.
        let unit = clip(
            vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 1.0), SpeedKey::new(3.0, 2.0)],
            30.0,
        );
        let steps = audio_steps(&unit, unit.duration());
        let (pieces, _) = audio_pieces(&steps, 48_000);
        assert!(!tempo_applies(pieces[0].tempo) && pieces[0].lead == 0, "{:?}", pieces[0]);
    }

    #[test]
    fn a_tempo_is_split_into_stages_each_inside_atempos_range() {
        for tempo in [0.01, 0.1, 0.4, 0.5, 0.75, 1.5, 2.0, 2.01, 3.0, 7.9, 100.0] {
            let text = atempo_stages(tempo);
            let stages: Vec<f64> = text
                .split(',')
                .map(|s| s.strip_prefix("atempo=").unwrap().parse().unwrap())
                .collect();
            assert!(
                stages.iter().all(|s| (0.5 - 1e-9..=2.0 + 1e-9).contains(s)),
                "{tempo}: {text}"
            );
            let product: f64 = stages.iter().product();
            assert!((product / tempo - 1.0).abs() < 1e-9, "{tempo}: {product}");
        }
        assert_eq!(atempo_stages(1.5), "atempo=1.5");
        assert_eq!(atempo_stages(4.0), "atempo=2,atempo=2");
        assert_eq!(atempo_stages(8.0), "atempo=2,atempo=2,atempo=2");
        assert_eq!(atempo_stages(16.0), "atempo=2,atempo=2,atempo=2,atempo=2");
        assert_eq!(atempo_stages(0.125), "atempo=0.5,atempo=0.5,atempo=0.5");
        assert_eq!(atempo_stages(2.0), "atempo=2");
        assert_eq!(atempo_stages(2.0 * (1.0 + 1e-12)), "atempo=2.000000000002");
        // The advance of a chain counts each stage at the tempo of those before it.
        assert!((tempo_advance(0.5) - STAGE_ADVANCE).abs() < 1e-12);
        assert!((tempo_advance(4.0) - STAGE_ADVANCE * 3.0).abs() < 1e-12);
        assert_eq!(tempo_advance(f64::NAN), 0.0);
        assert_eq!(tempo_advance(0.001), tempo_advance(0.25));
        assert_eq!(tempo_advance(100.0), tempo_advance(8.0));
    }

    #[test]
    fn a_stage_tempo_is_the_nearest_root_found_without_libm() {
        let next = |x: f64| f64::from_bits(x.to_bits() + 1);
        let prev = |x: f64| f64::from_bits(x.to_bits() - 1);
        for i in 0..2000 {
            let tempo = 0.01 + f64::from(i) * 0.0517;
            let stages = stage_count(tempo);
            let each = stage_tempo(tempo, stages);
            let power = |x: f64| (1..stages).fold(x, |p, _| p * x);
            // Whatever a platform's `powf` makes of the last digit, this is the double whose power is nearest.
            let error = |x: f64| (power(x) - tempo).abs();
            assert!(
                error(each) <= error(next(each)) && error(each) <= error(prev(each)),
                "{tempo}"
            );
            assert!((each / tempo.powf(1.0 / stages as f64) - 1.0).abs() < 1e-15, "{tempo}");
            if stages == 2 {
                assert_eq!(each, tempo.sqrt());
            }
        }
        assert_eq!(stage_tempo(1.5, 1), 1.5);
        assert_eq!(stage_count(f64::INFINITY), MAX_STAGES);
        assert_eq!(stage_count(f64::NAN), 1);
    }
}
