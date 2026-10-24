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
//! with `st(0, …)` and read back with `ld(0)`. [`Ex`] is both the text and the arithmetic, so the frame
//! pick and the tests evaluate what the graph prints.
//!
//! **Sound.** `atempo` takes one tempo for its whole stream, and its `tempo` command is no way round
//! that: each command re-origins the stream (it drops the drift control that holds the long-run tempo,
//! losing up to half a window of source per command on transient material, and FFmpeg 9 lists no command
//! support for it at all). So a ramp is cut into steps of constant tempo, each its own `atempo` stream
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
pub(crate) const MAX_AUDIO_STEPS: usize = 48;

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
    let cut = |scale: f64| -> Vec<f64> {
        let mut bounds = vec![0.0];
        for w in edges.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            let slope = slope_over(clip, lo, hi).abs();
            let n = if slope > 0.0 {
                let h = ((8.0 * STEP_ERROR / slope).sqrt() * scale).clamp(STEP_MIN, STEP_MAX);
                ((hi - lo) / h).ceil().max(1.0) as usize
            } else {
                1
            };
            for i in 1..=n {
                bounds.push(lo + (hi - lo) * i as f64 / n as f64);
            }
        }
        bounds
    };
    let mut scale = 1.0;
    let mut bounds = cut(scale);
    while bounds.len() > MAX_AUDIO_STEPS && scale < 1e6 {
        scale *= 1.5;
        bounds = cut(scale);
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
/// otherwise the fewest that bring each within it.
fn stage_count(tempo: f64) -> usize {
    ((tempo.max(1.0 / tempo).log2() - 1e-9).ceil().max(1.0)) as usize
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
/// within 3 ms of the output from 0.5x to 8x, 7 ms at 0.25x; a step that slow or fast is a moment of the
/// source however far it is stretched).
pub(crate) fn tempo_advance(tempo: f64) -> f64 {
    if !(tempo.is_finite() && tempo > 0.0) {
        return 0.0;
    }
    let tempo = tempo.clamp(0.25, 8.0);
    let stages = stage_count(tempo);
    let each = tempo.powf(1.0 / stages as f64);
    STAGE_ADVANCE * (0..stages).map(|j| each.powi(j as i32)).sum::<f64>()
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
/// The longest crossfade between two steps.
const CROSSFADE: f64 = 0.010;

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
    let each = tempo.powf(1.0 / stages as f64);
    vec![format!("atempo={each}"); stages].join(",")
}
