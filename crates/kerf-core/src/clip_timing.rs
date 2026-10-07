//! What a clip does in time: the pure side of `video_clip_chain`.
//!
//! A transition decorates the clips either side of a cut ([`ClipFx`], computed by
//! [`transition_fx`]), and a clip's chain then means different things in time: it
//! plays on for a `tail` under the incoming one, its source window moves with it,
//! it fades, dips, dissolves or travels across the frame. The FFmpeg graph
//! *formats* all of that into filter text; a renderer that draws a frame at a
//! given time has to *evaluate* it, and the two must agree on every number.
//! [`ClipTiming`] is the one place those numbers are decided: `video_clip_chain`
//! and `build_filter_complex` print what it returns (`fades`, `window`,
//! `motion_keys`), and a plan asks it the same question at a time `t`
//! (`enabled`, `motion_at`). Moving a number here moves both.
//!
//! **What FFmpeg does with those numbers is not the number.** The overlay's `enable`
//! window is necessary but not sufficient for a clip to be drawn (the source has to
//! have a frame there, and the `fps` pick decides which); FFmpeg evaluates `enable`
//! and every time expression at `t = k * (den / num)` ([`ffmpeg_frame_time`]), which
//! is not `k / fps` — one ulp below it for a third of the frame-aligned starts at
//! 24 fps, hiding the clip's first frame; and a `fade` counts frames
//! ([`FadeStep::progress_at_frame`]) instead of interpolating time. Exact rationals
//! belong to the frame *pick* alone. Each of these is pinned against rendered pixels
//! by an `#[ignore]`d test in `engine/cli.rs`.
//!
//! **Video only.** `audio_clip_chain` composes the same [`ClipFx`] differently
//! (`afade_in` joins the fade-in, and the tail is its fade-out), so it keeps its
//! own arithmetic and shares only [`ClipTiming::duration`], the window and the seek.
//!
//! Pure: no I/O, no machine reads, no dependency on the engine.
//!
//! The reading half (`enabled`, `motion_at`, `ffmpeg_frame_time`, `clips_with_fx`,
//! `FadeStep::progress_at_frame`) is what [`crate::planner::Planner`]
//! evaluates; `FadeStep::progress` and `ClipTiming::motion_at` are kept for a caller
//! that has a time rather than a frame and are exempt from the dead-code lint.

use std::path::Path;

use crate::model::{interpolate, Asset, Clip, Hdr, Timeline};

/// The marker a head-padded proxy carries in its file name, ahead of `.mp4`.
pub(crate) const HEAD_PADDED_SUFFIX: &str = ".lead.mp4";

/// Whether `path` is a proxy `generate_proxy` made with a padded head (pure,
/// unit-tested): `.../kerf/proxies/<16 hex digits>.lead.mp4`.
///
/// That it is padded is a fact about the *file*, so it travels in the file's name
/// instead of in a flag every caller of the graph builders would have to carry
/// beside the path — a preview asset is its original with the path swapped, and
/// the graph needs to know, per input, to drop the pad's clone frame before it
/// trims (see `video_clip_chain`). An original, or any proxy that was not padded,
/// does not match, and its argv and graph are what they always were.
pub(crate) fn is_head_padded_proxy(path: &str) -> bool {
    let path = Path::new(path);
    let Some(hash) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(HEAD_PADDED_SUFFIX))
    else {
        return false;
    };
    let dir = |p: Option<&Path>| p.and_then(|p| p.file_name()).and_then(|n| n.to_str()).map(str::to_owned);
    hash.len() == 16
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && dir(path.parent()).as_deref() == Some("proxies")
        && dir(path.parent().and_then(Path::parent)).as_deref() == Some("kerf")
}

/// Per-clip render adjustments derived from transitions. `tail` extends an
/// outgoing clip so it keeps showing under the incoming one; `xfade_in` is the
/// incoming clip's alpha dissolve; `black_in`/`black_out` and `white_in`/
/// `white_out` are the dip fades on either side of a cut; `move_in`/`move_out`
/// carry a clip across the frame for a slide or a push.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClipFx {
    pub tail: f64,
    pub xfade_in: f64,
    pub black_in: f64,
    pub black_out: f64,
    /// Dip-to-white fades: the same shape as `black_in`/`black_out`, through white.
    pub white_in: f64,
    pub white_out: f64,
    /// How long the incoming clip's **sound** dissolves up. Equal to `xfade_in`
    /// for a crossfade; a motion transition sets it too, because the picture
    /// sliding is no reason for the audio to cut hard.
    pub afade_in: f64,
    /// Motion transitions, as `(dx, dy, seconds)` with the offsets in frame
    /// widths and heights. `move_in` is where the incoming clip starts before
    /// travelling to its position; `move_out` is where the outgoing clip is
    /// carried to over its tail (a push only — a slide covers it where it sits).
    pub move_in: Option<(f64, f64, f64)>,
    pub move_out: Option<(f64, f64, f64)>,
    /// The clip's source is HDR and its picture is tone-mapped to SDR in the
    /// chain. `None` for SDR — and for a preview asset swapped to its proxy,
    /// which was converted when it was encoded.
    pub hdr: Option<Hdr>,
    /// The clip's source carries an alpha channel (a transparent PNG, an FFV1 or ProRes
    /// 4444 clip, a GIF: the probed pixel format says so), which the chain keeps to its
    /// end instead of flattening the picture onto black. `false` when the format was
    /// never recorded: an unknown picture is treated as it always was.
    pub alpha: bool,
    /// The clip's input is a head-padded proxy (see `is_head_padded_proxy`). Read
    /// from the start with no seek, such an input opens with the pad's clone of the
    /// first frame, which the original it stands for has no frame for.
    pub head_pad: bool,
}

/// Compute the [`ClipFx`] for every clip, resolving each `transition_in` against the
/// clip that precedes it on the same track in timeline order.
///
/// The result is indexed by **flat clip storage index**: tracks in order, and within
/// a track the clips in the order they are *stored* (not timeline order, and not an
/// ffmpeg input index — inputs are deduplicated). `timeline` has to be the one the
/// graph is built from (`Timeline::for_render()`, with the same slice or delivery
/// applied), and `assets` the same list the graph sees, including a preview's proxy
/// swap: a different list gives a different tail, HDR flag or head-pad answer.
/// [`clips_with_fx`] pairs each clip with its entry so a caller cannot mis-pair them.
pub fn transition_fx(timeline: &Timeline, assets: &[Asset]) -> Vec<ClipFx> {
    let total_clips: usize = timeline.tracks.iter().map(|t| t.clips.len()).sum();
    let mut fx = vec![ClipFx::default(); total_clips];
    for (flat, clip) in timeline.tracks.iter().flat_map(|t| t.clips.iter()).enumerate() {
        let asset = assets.iter().find(|a| a.id == clip.asset_id);
        fx[flat].hdr = asset.and_then(|a| a.hdr());
        fx[flat].alpha = asset.is_some_and(|a| a.has_alpha());
        fx[flat].head_pad = asset.is_some_and(|a| is_head_padded_proxy(&a.path));
    }
    let asset_dur = |id| assets.iter().find(|a| a.id == id).map(|a| a.duration);
    let is_still = |id| assets.iter().find(|a| a.id == id).is_some_and(|a| a.is_image());

    let mut base = 0;
    for track in &timeline.tracks {
        let n = track.clips.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for w in 0..n {
            let j = order[w];
            let clip = &track.clips[j];
            let Some(tr) = clip.transition_in else { continue };
            let d = tr.duration.max(0.0);
            if d <= 0.0 {
                continue;
            }
            // The transition partner is the immediately preceding clip on the
            // track — but only when it is actually adjacent (no gap before this
            // clip); otherwise the transition resolves against black.
            let prev = (w > 0)
                .then(|| order[w - 1])
                .filter(|&pj| (track.clips[pj].timeline_end() - clip.timeline_start).abs() < 1e-3);
            match tr.kind.dip_color() {
                // A dip happens either side of the cut — the two clips never share
                // the screen, so neither needs a handle and neither is extended.
                Some(color) => {
                    let white = color == "white";
                    let inn = (d / 2.0).min(clip.duration());
                    if white {
                        fx[base + j].white_in = inn;
                    } else {
                        fx[base + j].black_in = inn;
                    }
                    if let Some(pj) = prev {
                        let p = &track.clips[pj];
                        let out = (d / 2.0).min(p.duration());
                        if white {
                            fx[base + pj].white_out = fx[base + pj].white_out.max(out);
                        } else {
                            fx[base + pj].black_out = fx[base + pj].black_out.max(out);
                        }
                    }
                }
                // A dissolve or a motion transition plays both sides at once, so
                // the outgoing clip keeps rolling underneath on its unused handle.
                None => {
                    let slide = tr.kind.slide_from();
                    let overlap = match prev {
                        Some(pj) => {
                            let p = &track.clips[pj];
                            // The tail borrows the outgoing clip's unused source: for a
                            // forward clip that is the handle past source_out, for a
                            // reversed clip the handle below source_in.
                            // A still loops (`-loop 1`), so it never runs out
                            // of source: its handle is unbounded, the same
                            // reason the timeline lets a still extend freely.
                            let avail = if is_still(p.asset_id) {
                                f64::INFINITY
                            } else if p.is_reversed() {
                                p.source_in / p.speed_mag()
                            } else {
                                asset_dur(p.asset_id).map(|ad| (ad - p.source_out).max(0.0)).unwrap_or(0.0) / p.speed_mag()
                            };
                            // Both sides share the achievable overlap so the transition
                            // length matches the tail (no fade-from-black when there is
                            // no handle — it just becomes a hard cut).
                            let overlap = d.min(p.duration()).min(clip.duration()).min(avail.max(0.0));
                            fx[base + pj].tail = fx[base + pj].tail.max(overlap);
                            if overlap > 0.0 && tr.kind.pushes() {
                                if let Some((dx, dy)) = slide {
                                    // The outgoing clip leaves the way the incoming one
                                    // arrives: at rest, then a whole frame the other way.
                                    fx[base + pj].move_out = Some((-dx, -dy, overlap));
                                }
                            }
                            overlap
                        }
                        // No adjacent predecessor: dissolve up from black, or travel in
                        // over it.
                        None => d.min(clip.duration()),
                    };
                    if overlap <= 0.0 {
                        continue;
                    }
                    match slide {
                        Some((dx, dy)) => fx[base + j].move_in = Some((dx, dy, overlap)),
                        None => fx[base + j].xfade_in = overlap,
                    }
                    fx[base + j].afade_in = overlap;
                }
            }
        }
        base += n;
    }
    fx
}

/// Every clip of `timeline` with its [`ClipFx`], in `transition_fx`'s order:
/// `(track index, index in the track's storage, the clip, its fx)`. The pairing the
/// graph builders do with a running flat index, done once, so a renderer that walks
/// the clips cannot index the table with a different count.
pub fn clips_with_fx<'a>(
    timeline: &'a Timeline,
    assets: &[Asset],
) -> impl Iterator<Item = (usize, usize, &'a Clip, ClipFx)> + 'a {
    let fx = transition_fx(timeline, assets);
    timeline
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(ti, track)| track.clips.iter().enumerate().map(move |(ci, clip)| (ti, ci, clip)))
        .zip(fx)
        .map(|((ti, ci, clip), fx)| (ti, ci, clip, fx))
}

/// The source-time window `[start, end]` a clip needs from its asset, accounting
/// for reverse playback and any crossfade tail (which borrows unused handle past
/// `source_out`, or below `source_in` when reversed). The single source of truth
/// for both the per-input `-ss` fast-seek and the in-graph `trim` / `atrim`, so
/// the seek and the trim window can never drift out of lockstep.
pub fn clip_source_window(clip: &Clip, fx: &ClipFx) -> (f64, f64) {
    let s = clip.speed_mag();
    if clip.is_reversed() {
        ((clip.source_in - fx.tail * s).max(0.0), clip.source_out)
    } else {
        (clip.source_in, clip.source_out + fx.tail * s)
    }
}

/// The input-side fast-seek for a clip's window start: seek there when it is past
/// the head (so ffmpeg decodes from a nearby keyframe instead of t=0), else `0.0`
/// for no seek — head clips keep byte-identical args. `SEEK_EPS` skips a
/// pointless sub-millisecond seek. Callers must express the in-graph trim
/// relative to this value.
pub fn clip_seek(window_start: f64) -> f64 {
    const SEEK_EPS: f64 = 1e-3;
    if window_start > SEEK_EPS {
        window_start
    } else {
        0.0
    }
}

/// Which side of a clip's life a [`FadeStep`] sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeEdge {
    In,
    Out,
}

/// What a [`FadeStep`] fades to (or up from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeTint {
    /// The frame itself, to black: a clip's own fade and a dip to black.
    Black,
    /// The frame itself, through white: a dip to white.
    White,
    /// The alpha plane, so a lower track shows through: a dissolve's incoming side.
    Alpha,
}

/// One `fade` of a clip's picture. `st` is on the **timeline** (`setpts` has
/// already moved the frames there), `d` is clamped to the clip's length with its
/// tail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FadeStep {
    pub edge: FadeEdge,
    pub tint: FadeTint,
    pub st: f64,
    pub d: f64,
}

/// `av_parse_time` on the decimal text a duration option is handed (the graph
/// prints `{}` of the `f64`): whole microseconds, the digits past the sixth
/// dropped rather than rounded.
pub(crate) fn parse_micros(seconds: f64) -> i64 {
    parse_micros_text(&format!("{seconds}"))
}

/// [`parse_micros`] for the text itself: what `sendcmd` makes of a printed time.
pub(crate) fn parse_micros_text(text: &str) -> i64 {
    let negative = text.starts_with('-');
    let text = text.trim_start_matches('-');
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let micros: String = frac.chars().take(6).collect();
    let magnitude =
        whole.parse::<i64>().unwrap_or(0).saturating_mul(1_000_000) + format!("{micros:0<6}").parse::<i64>().unwrap_or(0);
    if negative {
        -magnitude
    } else {
        magnitude
    }
}

/// `av_rescale_q(parse_time(seconds), 1/1000000, den/num)`: the frames FFmpeg's
/// `fade` turns a time into at the filter's input time base (`1/fps` after the
/// chain's `fps`), rounded to nearest, halves away from zero.
fn fade_ticks(seconds: f64, fps_num: u32, fps_den: u32) -> i64 {
    let n = i128::from(parse_micros(seconds)) * i128::from(fps_num);
    let d = 1_000_000 * i128::from(fps_den.max(1));
    let half = d / 2;
    let rounded = if n >= 0 { (n + half) / d } else { -((-n + half) / d) };
    rounded as i64
}

impl FadeStep {
    /// How much of the picture is left at output frame `k` (timeline frame
    /// `k` of an `fps_num/fps_den` export): `1` untouched, `0` faded away, so a
    /// fade-in rises `0 -> 1` and a fade-out falls `1 -> 0`.
    ///
    /// FFmpeg's `fade` does not interpolate time. It converts `st` and `d` to
    /// frames of its input time base — `S = round(st * fps)`, `N = round(d * fps)`
    /// (at least 1) — and from frame `S` moves in `N` equal steps: frame `S + i`
    /// carries `i / N`, held at the end value past `S + N` (and, before `S`, at the
    /// start value). A fade is therefore never `(t - st) / d`: it starts on a frame
    /// boundary, which for an off-grid `st` is up to half a frame away from `st`,
    /// and it is up to half a frame longer or shorter than `d`. The same counting
    /// serves the black and white dips and a dissolve's alpha ramp (`tint` only says
    /// what is faded to). Pinned against rendered alpha on 6.1 and 9.0.
    pub fn progress_at_frame(&self, k: i64, fps_num: u32, fps_den: u32) -> f64 {
        let start = fade_ticks(self.st, fps_num, fps_den);
        let steps = fade_ticks(self.d, fps_num, fps_den).max(1);
        let done = (k - start).clamp(0, steps) as f64 / steps as f64;
        match self.edge {
            FadeEdge::In => done,
            FadeEdge::Out => 1.0 - done,
        }
    }

    /// [`Self::progress_at_frame`] for a time on the frame grid: `t` is rounded to
    /// the nearest frame of `fps_num/fps_den` first.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn progress(&self, t: f64, fps_num: u32, fps_den: u32) -> f64 {
        let k = (t * f64::from(fps_num) / f64::from(fps_den.max(1))).round() as i64;
        self.progress_at_frame(k, fps_num, fps_den)
    }
}

/// The time FFmpeg evaluates a frame at: the frame's pts times `av_q2d` of the time
/// base, in doubles — `k * (den / num)`, **not** `k * den / num` and not `k / fps`.
/// The three differ by an ulp, and an `enable='between(t,s,e)'` or a `fade` start
/// decided on that ulp keeps or drops a whole frame: at 24 fps about a third of the
/// frame-aligned clip starts evaluate to just under their own start and lose their
/// first frame. Use this wherever a graph expression is evaluated for output frame
/// `k`; keep exact rationals for the frame *pick* only.
pub fn ffmpeg_frame_time(k: u64, fps_num: u32, fps_den: u32) -> f64 {
    k as f64 * (f64::from(fps_den) / f64::from(fps_num))
}

/// A frame rate as FFmpeg holds it: the rational it makes of the text the graph carries.
///
/// The graph prints `{}` of an `f64` and FFmpeg turns that text back into a rational
/// with `av_d2q`, so `29.97` is `2997/100` and its neighbour `29.97002997002997` is
/// `30000/1001` — two different frame grids, and which one an export runs on is decided
/// by the number's spelling, not by what it is near. **It is not one parse but two**, with
/// different limits, and a rate with more than 1001000 in a term gets different grids from
/// them: the `color=r=` base canvas (and so the overlay's clock, `t`) is
/// [`Rational::from_fps`] (`av_d2q(x, 1001000)`), and each clip's `fps=` filter is
/// [`Rational::from_fps_filter`] (`av_d2q(x, INT_MAX)`; `29.970029` is `92997/3103` on the
/// canvas and `29970029/1000000` on the clip). Every standard rate — anything that is a
/// small ratio, `24`, `25`, `29.97`, `30000/1001`, `59.94`, ... — gets the same rational
/// from both. Every time the graph evaluates at output frame `k` is `k * (den / num)`
/// (`ffmpeg_frame_time`); [`Rational::exact_time`] is the exact slot boundary, for
/// the source-frame *pick* alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    /// `num / den`; `None` when either is zero (FFmpeg refuses such a rate).
    pub fn new(num: u32, den: u32) -> Option<Self> {
        (num > 0 && den > 0).then_some(Self { num, den })
    }

    /// What a `color=r={fps}` source (the export's canvas) parses `{fps}` to: `av_d2q`
    /// with the 1001000 limit of an option of video-rate type. `None` for a rate it
    /// would refuse (not positive and finite, or one that reduces to nothing).
    pub fn from_fps(fps: f64) -> Option<Self> {
        Self::reduced(av_d2q(fps, 1_001_000)?)
    }

    /// What the `fps={fps}` filter parses `{fps}` to: `av_d2q` with `INT_MAX` as the limit,
    /// the grid a clip's frames are placed on. Equal to [`Rational::from_fps`] for every
    /// rate whose terms fit 1001000.
    pub fn from_fps_filter(fps: f64) -> Option<Self> {
        Self::reduced(av_d2q(fps, i128::from(i32::MAX))?)
    }

    fn reduced((num, den): (i128, i128)) -> Option<Self> {
        Self::new(u32::try_from(num).ok()?, u32::try_from(den).ok()?)
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// The time FFmpeg evaluates output frame `k` at (`ffmpeg_frame_time`).
    pub fn frame_time(self, k: u64) -> f64 {
        ffmpeg_frame_time(k, self.num, self.den)
    }

    /// The exact start of output frame `k`'s slot, `k * den / num`, correctly rounded.
    pub fn exact_time(self, k: u64) -> f64 {
        (k as f64 * f64::from(self.den)) / f64::from(self.num)
    }

    /// The output frame whose slot start is nearest `t` (`0` for a negative time).
    pub fn frame_at(self, t: f64) -> u64 {
        (t.max(0.0) * f64::from(self.num) / f64::from(self.den)).round() as u64
    }

    /// The output frame on screen at `t`: the one whose slot `[k/fps, (k+1)/fps)` holds it
    /// (`0` for a negative time). A time a millionth of a frame short of a boundary is on
    /// it, so `k / fps` computed in floating point is frame `k` and not `k - 1`.
    pub fn frame_containing(self, t: f64) -> u64 {
        (t.max(0.0) * f64::from(self.num) / f64::from(self.den) + 1e-6).floor() as u64
    }
}

/// libavutil's `av_reduce`, on 128-bit integers (the C one multiplies in 64 and
/// relies on the operands having shrunk by the time they are compared).
fn av_reduce(num: i128, den: i128, max: i128) -> (i128, i128) {
    let (mut num, mut den) = (num.abs(), den.abs());
    let (mut a0, mut a1) = ((0i128, 1i128), (1i128, 0i128));
    let gcd = {
        let (mut a, mut b) = (num, den);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    if gcd != 0 {
        (num, den) = (num / gcd, den / gcd);
    }
    if num <= max && den <= max {
        a1 = (num, den);
        den = 0;
    }
    while den != 0 {
        let x = num / den;
        let next_den = num - den * x;
        let (a2n, a2d) = (x * a1.0 + a0.0, x * a1.1 + a0.1);
        if a2n > max || a2d > max {
            let mut x = x;
            if a1.0 != 0 {
                x = (max - a0.0) / a1.0;
            }
            if a1.1 != 0 {
                x = x.min((max - a0.1) / a1.1);
            }
            if den * (2 * x * a1.1 + a0.1) > num * a1.1 {
                a1 = (x * a1.0 + a0.0, x * a1.1 + a0.1);
            }
            break;
        }
        (a0, a1) = (a1, (a2n, a2d));
        (num, den) = (den, next_den);
    }
    a1
}

/// libavutil's `av_d2q` for a positive finite `d`: the best rational with both terms
/// at most `max`, found by a continued fraction over `d` scaled to 61 bits.
fn av_d2q(d: f64, max: i128) -> Option<(i128, i128)> {
    if !d.is_finite() || d <= 0.0 || d > f64::from(i32::MAX) {
        return None;
    }
    // `frexp`'s exponent less one is the binary exponent of the leading bit.
    let exponent = (((d.to_bits() >> 52) & 0x7ff) as i32 - 1023).max(0);
    let den = 1i128 << (61 - exponent);
    let scaled = (d * den as f64 + 0.5).floor() as i128;
    let (n, dd) = av_reduce(scaled, den, max);
    if n != 0 && dd != 0 {
        return Some((n, dd));
    }
    // The C code retries with the whole `int` range when the limit left nothing.
    let (n, dd) = av_reduce(scaled, den, i128::from(i32::MAX));
    (n != 0 && dd != 0).then_some((n, dd))
}

/// The offset a motion transition puts on a clip, as keyframes over **clip-local**
/// seconds in frame widths (`x`) and heights (`y`). Both channels share their
/// times; they are kept apart because the graph prints each as its own expression.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionKeys {
    pub x: Vec<(f64, f64)>,
    pub y: Vec<(f64, f64)>,
}

impl MotionKeys {
    /// The offset `(dx, dy)` at clip-local time `local`, in **frame fractions**:
    /// piecewise linear, held flat beyond the first and last key — the same curve
    /// `keyframe_expr` writes out. It is exact; the overlay it drives does not
    /// place at a fraction of a pixel but at `(int)(fraction * extent) & ~1` per axis
    /// (truncated toward zero, then to the 4:2:0 chroma grid), which a renderer that
    /// wants the same pixels has to apply.
    pub fn at(&self, local: f64) -> (f64, f64) {
        let sample = |pts: &[(f64, f64)]| {
            let mut pts = pts.to_vec();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            interpolate(&pts, local).unwrap_or(0.0)
        };
        (sample(&self.x), sample(&self.y))
    }
}

/// A clip together with the [`ClipFx`] its transitions gave it: the questions the
/// export graph and a frame renderer both ask about when and how it is on screen.
#[derive(Clone, Copy, Debug)]
pub struct ClipTiming<'a> {
    pub clip: &'a Clip,
    pub fx: &'a ClipFx,
}

impl<'a> ClipTiming<'a> {
    pub fn new(clip: &'a Clip, fx: &'a ClipFx) -> Self {
        Self { clip, fx }
    }

    /// How long the clip plays: its own length plus the tail it keeps under the
    /// clip that replaces it.
    pub fn duration(&self) -> f64 {
        self.clip.duration() + self.fx.tail
    }

    /// The overlay's `enable` window on the **timeline**, `(start, end)`, tail
    /// included: what the graph prints as `enable='between(t,start,end)'`.
    pub fn window(&self) -> (f64, f64) {
        (self.clip.timeline_start, self.clip.timeline_end() + self.fx.tail)
    }

    /// Whether the overlay's `enable` expression is true at `t`: the window with
    /// both ends included, as `between` has it. **Necessary, not sufficient, for the
    /// clip to be drawn**: it is not "on screen at `t`". The export draws nothing at
    /// `t == end` from an equal-rate source (the source has no frame there), draws a
    /// slower source's last frame at it through the `fps` filter, and is evaluated
    /// at [`ffmpeg_frame_time`], not at an exact `k / fps`. Which frames are drawn is
    /// the pick's decision.
    pub fn enabled(&self, t: f64) -> bool {
        let (start, end) = self.window();
        t >= start && t <= end
    }

    /// The fades of the clip's picture, in the order the chain applies them: its
    /// own fade-in and out merged with a dip to black's, then a dip to white's,
    /// then a dissolve's alpha ramp. A step is only present when it has length.
    pub fn fades(&self) -> Vec<FadeStep> {
        let (clip, fx) = (self.clip, self.fx);
        let dur = self.duration();
        let t0 = clip.timeline_start;
        let step = |edge, tint, len: f64| FadeStep {
            edge,
            tint,
            st: match edge {
                FadeEdge::In => t0,
                FadeEdge::Out => t0 + (dur - len).max(0.0),
            },
            d: len.clamp(0.0, dur),
        };
        let fi = clip.fade_in + fx.black_in;
        let fo = clip.fade_out + fx.black_out;
        let mut steps = Vec::new();
        for (len, edge, tint) in [
            (fi, FadeEdge::In, FadeTint::Black),
            (fo, FadeEdge::Out, FadeTint::Black),
            (fx.white_in, FadeEdge::In, FadeTint::White),
            (fx.white_out, FadeEdge::Out, FadeTint::White),
            (fx.xfade_in, FadeEdge::In, FadeTint::Alpha),
        ] {
            if len > 0.0 {
                steps.push(step(edge, tint, len));
            }
        }
        steps
    }

    /// The keyframes of the offset a slide or push gives the clip, or `None` when
    /// it does not move (which is what keeps every other graph byte-identical).
    ///
    /// An incoming clip holds its starting offset before the transition and travels
    /// to zero; an outgoing one sits at zero until its own end, then travels away
    /// over its tail.
    pub fn motion_keys(&self) -> Option<MotionKeys> {
        let mut xs: Vec<(f64, f64)> = Vec::new();
        let mut ys: Vec<(f64, f64)> = Vec::new();
        if let Some((dx, dy, secs)) = self.fx.move_in {
            xs.push((0.0, dx));
            xs.push((secs, 0.0));
            ys.push((0.0, dy));
            ys.push((secs, 0.0));
        }
        if let Some((dx, dy, secs)) = self.fx.move_out {
            let t0 = self.clip.duration();
            if xs.is_empty() {
                xs.push((0.0, 0.0));
                ys.push((0.0, 0.0));
            }
            xs.push((t0, 0.0));
            xs.push((t0 + secs, dx));
            ys.push((t0, 0.0));
            ys.push((t0 + secs, dy));
        }
        (!xs.is_empty()).then_some(MotionKeys { x: xs, y: ys })
    }

    /// The offset `(dx, dy)` a motion transition puts on the clip at timeline time
    /// `t`, in **frame fractions** (widths and heights); `(0, 0)` for a clip that does
    /// not move. Exact: see [`MotionKeys::at`] for how the overlay truncates it to
    /// pixels.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn motion_at(&self, t: f64) -> (f64, f64) {
        self.motion_keys()
            .map_or((0.0, 0.0), |keys| keys.at(t - self.clip.timeline_start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{img_asset, make_clip, single, test_asset, video_stream};
    use crate::model::{Transition, TransitionKind};

    fn asset() -> Asset {
        test_asset(vec![video_stream(1920, 1080, 30.0)])
    }

    /// Two adjacent 10 s clips of `asset` (source 0..10 and 20..30), the second
    /// entering through `kind` over `secs`.
    fn pair(asset: &Asset, kind: TransitionKind, secs: f64) -> (Timeline, Vec<ClipFx>) {
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 20.0, 30.0, 10.0);
        b.transition_in = Some(Transition { kind, duration: secs });
        let timeline = single(vec![a, b]);
        let fx = transition_fx(&timeline, std::slice::from_ref(asset));
        (timeline, fx)
    }

    fn timing<'a>(tl: &'a Timeline, fx: &'a [ClipFx], i: usize) -> ClipTiming<'a> {
        ClipTiming::new(&tl.tracks[0].clips[i], &fx[i])
    }

    fn close(a: (f64, f64), b: (f64, f64)) {
        assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9, "{a:?} != {b:?}");
    }

    #[test]
    fn a_plain_clip_has_the_enable_window_of_its_own_length_and_nothing_else_moves() {
        let asset = asset();
        let tl = single(vec![make_clip(asset.id, 5.0, 9.0, 3.0)]);
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        assert_eq!(fx, vec![ClipFx::default()]);
        let t = timing(&tl, &fx, 0);
        assert_eq!((t.duration(), t.window()), (4.0, (3.0, 7.0)));
        // `between(t,3,7)` includes both ends. That is the *enable* expression, not
        // what is drawn: the export draws nothing at t == 7 from an equal-rate source
        // (`a_clip_is_drawn_on_the_frames_its_window_and_its_source_leave_it`).
        assert!(t.enabled(3.0) && t.enabled(7.0) && t.enabled(5.0));
        assert!(!t.enabled(2.999) && !t.enabled(7.001));
        assert!(t.fades().is_empty());
        assert_eq!(t.motion_keys(), None);
        assert_eq!(t.motion_at(4.0), (0.0, 0.0));
        assert_eq!(clip_source_window(t.clip, t.fx), (5.0, 9.0));
    }

    #[test]
    fn a_clips_own_fades_are_timed_on_the_timeline_and_clamped_to_its_length() {
        let asset = asset();
        let mut clip = make_clip(asset.id, 0.0, 4.0, 5.0);
        clip.fade_in = 0.5;
        clip.fade_out = 1.0;
        let tl = single(vec![clip]);
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        let step = |edge, st, d| FadeStep {
            edge,
            tint: FadeTint::Black,
            st,
            d,
        };
        assert_eq!(
            timing(&tl, &fx, 0).fades(),
            vec![step(FadeEdge::In, 5.0, 0.5), step(FadeEdge::Out, 8.0, 1.0)]
        );
        // A fade longer than the clip is clamped to it, and cannot start before it.
        let mut long = make_clip(asset.id, 0.0, 4.0, 5.0);
        long.fade_out = 10.0;
        let tl = single(vec![long]);
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        assert_eq!(timing(&tl, &fx, 0).fades(), vec![step(FadeEdge::Out, 5.0, 4.0)]);
    }

    #[test]
    fn a_dissolve_borrows_the_outgoing_handle_and_ramps_the_incoming_alpha() {
        let asset = asset();
        let (tl, fx) = pair(&asset, TransitionKind::Crossfade, 1.0);
        let (out, inc) = (timing(&tl, &fx, 0), timing(&tl, &fx, 1));
        // The outgoing clip keeps playing for the transition, on source past its out point.
        assert_eq!((out.duration(), out.window()), (11.0, (0.0, 11.0)));
        assert!(out.enabled(10.5) && !out.enabled(11.5));
        assert_eq!(clip_source_window(out.clip, out.fx), (0.0, 11.0));
        // Its picture is not faded: the incoming clip's alpha ramp does the mixing.
        assert!(out.fades().is_empty());
        let ramp = inc.fades();
        assert_eq!(ramp.len(), 1);
        assert_eq!((ramp[0].edge, ramp[0].tint), (FadeEdge::In, FadeTint::Alpha));
        assert_eq!((ramp[0].st, ramp[0].d), (10.0, 1.0));
        assert_eq!((inc.fx.xfade_in, inc.fx.afade_in), (1.0, 1.0));
    }

    #[test]
    fn a_dissolve_with_no_source_left_to_borrow_is_a_hard_cut() {
        let asset = asset();
        // The outgoing clip ends on the last frame of its footage (100 s).
        let a = make_clip(asset.id, 90.0, 100.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let tl = single(vec![a, b]);
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        assert_eq!(fx, vec![ClipFx::default(); 2]);
        // A still never runs out: it loops, so its handle is unbounded.
        let still = img_asset(uuid::Uuid::new_v4());
        let (tl, fx) = pair(&still, TransitionKind::Crossfade, 1.0);
        assert_eq!(timing(&tl, &fx, 0).fx.tail, 1.0);
    }

    #[test]
    fn a_dip_fades_either_side_of_the_cut_and_merges_with_the_clips_own_fades() {
        let asset = asset();
        let (tl, fx) = pair(&asset, TransitionKind::DipToBlack, 2.0);
        // Neither side is extended: the two clips never share the screen.
        assert_eq!(
            (timing(&tl, &fx, 0).fx.tail, timing(&tl, &fx, 0).window()),
            (0.0, (0.0, 10.0))
        );
        let fade = |t: &ClipTiming, i: usize| {
            let f = t.fades()[i];
            (f.edge, f.tint, f.st, f.d)
        };
        assert_eq!(fade(&timing(&tl, &fx, 0), 0), (FadeEdge::Out, FadeTint::Black, 9.0, 1.0));
        assert_eq!(fade(&timing(&tl, &fx, 1), 0), (FadeEdge::In, FadeTint::Black, 10.0, 1.0));

        // The dip joins the clip's own fade-in (they are one `fade`), and a dip to
        // white follows the black fades as a second step through white.
        let (mut tl, _) = pair(&asset, TransitionKind::DipToWhite, 2.0);
        tl.tracks[0].clips[1].fade_in = 0.25;
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        let steps = timing(&tl, &fx, 1).fades();
        assert_eq!(steps.len(), 2);
        assert_eq!((steps[0].tint, steps[0].d), (FadeTint::Black, 0.25));
        assert_eq!((steps[1].tint, steps[1].st, steps[1].d), (FadeTint::White, 10.0, 1.0));
        assert_eq!(fade(&timing(&tl, &fx, 0), 0), (FadeEdge::Out, FadeTint::White, 9.0, 1.0));
    }

    #[test]
    fn a_slide_travels_the_incoming_clip_in_and_a_push_carries_the_outgoing_one_out() {
        let asset = asset();
        let (tl, fx) = pair(&asset, TransitionKind::SlideLeft, 2.0);
        let (out, inc) = (timing(&tl, &fx, 0), timing(&tl, &fx, 1));
        // Incoming: a frame to the right at the cut, in place two seconds later.
        close(inc.motion_at(9.0), (1.0, 0.0));
        close(inc.motion_at(10.0), (1.0, 0.0));
        close(inc.motion_at(11.0), (0.5, 0.0));
        close(inc.motion_at(12.0), (0.0, 0.0));
        close(inc.motion_at(30.0), (0.0, 0.0));
        // A slide covers the outgoing clip where it sits.
        assert_eq!(out.motion_keys(), None);
        assert_eq!(out.fx.tail, 2.0);

        let (tl, fx) = pair(&asset, TransitionKind::PushUp, 2.0);
        let (out, inc) = (timing(&tl, &fx, 0), timing(&tl, &fx, 1));
        close(inc.motion_at(10.0), (0.0, 1.0));
        // Outgoing: at rest until its own end, then a whole frame the other way.
        close(out.motion_at(5.0), (0.0, 0.0));
        close(out.motion_at(10.0), (0.0, 0.0));
        close(out.motion_at(11.0), (0.0, -0.5));
        close(out.motion_at(12.0), (0.0, -1.0));
        assert!(out.enabled(12.0) && !out.enabled(12.001));
        // The keys are what the graph prints: clip-local seconds.
        assert_eq!(inc.motion_keys().unwrap().y, vec![(0.0, 1.0), (2.0, 0.0)]);
    }

    #[test]
    fn a_reversed_or_retimed_clips_tail_borrows_source_at_its_own_rate() {
        let asset = asset();
        // Reversed: plays 30 -> 20, so the handle is below `source_in`.
        let mut rev = make_clip(asset.id, 20.0, 30.0, 0.0);
        rev.speed = -1.0;
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let tl = single(vec![rev, b]);
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        assert_eq!(clip_source_window(timing(&tl, &fx, 0).clip, &fx[0]), (19.0, 30.0));
        // At 2x a second of tail is two seconds of source.
        let mut fast = make_clip(asset.id, 0.0, 20.0, 0.0);
        fast.speed = 2.0;
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let tl = single(vec![fast, b]);
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        assert_eq!(timing(&tl, &fx, 0).window(), (0.0, 11.0));
        assert_eq!(clip_source_window(timing(&tl, &fx, 0).clip, &fx[0]), (0.0, 22.0));
    }

    #[test]
    fn a_seek_is_only_made_past_the_head() {
        assert_eq!(clip_seek(0.0), 0.0);
        assert_eq!(clip_seek(0.0005), 0.0);
        assert_eq!(clip_seek(0.002), 0.002);
        assert_eq!(clip_seek(12.5), 12.5);
    }

    #[test]
    fn what_the_asset_says_about_its_file_rides_along() {
        let mut hdr = asset();
        hdr.streams[0].color_transfer = Some("arib-std-b67".into());
        hdr.path = "/cache/kerf/proxies/0123456789abcdef.lead.mp4".into();
        let tl = single(vec![make_clip(hdr.id, 0.0, 4.0, 0.0)]);
        let fx = transition_fx(&tl, std::slice::from_ref(&hdr));
        assert_eq!((fx[0].hdr, fx[0].head_pad), (Some(Hdr::Hlg), true));
        let sdr = asset();
        let tl = single(vec![make_clip(sdr.id, 0.0, 4.0, 0.0)]);
        assert_eq!(transition_fx(&tl, std::slice::from_ref(&sdr)), vec![ClipFx::default()]);
    }

    #[test]
    fn ffmpegs_frame_time_is_not_the_exact_one() {
        // `k * (den / num)`, the way `pts * av_q2d(time_base)` multiplies, lands an ulp
        // off `k / fps`: below it for a third of the frames at 24 fps (so a clip that
        // starts exactly on one is enabled one frame late), about half at 29.97, and
        // above it for 13 % at 25. Where it is exact nothing moves.
        let share = |num: u32, den: u32, cmp: fn(f64, f64) -> bool| {
            let n = 2400;
            (1..=n)
                .filter(|&k| cmp(ffmpeg_frame_time(k, num, den), (k * u64::from(den)) as f64 / f64::from(num)))
                .count() as f64
                / n as f64
        };
        assert!((share(24, 1, |a, b| a < b) - 1.0 / 3.0).abs() < 0.01);
        assert!((share(2997, 100, |a, b| a < b) - 0.5).abs() < 0.01);
        assert!((share(25, 1, |a, b| a > b) - 0.13).abs() < 0.01);
        assert_eq!(share(25, 1, |a, b| a < b), 0.0);
        // The first one it bites at 24 fps: frame 5 starts a clip at 5/24 s, and FFmpeg
        // evaluates it just before that.
        assert!(ffmpeg_frame_time(5, 24, 1) < 5.0 / 24.0);
        assert_eq!(ffmpeg_frame_time(0, 24, 1), 0.0);
    }

    #[test]
    fn microseconds_are_truncated_like_av_parse_time_and_frames_are_rounded() {
        assert_eq!(parse_micros(5.3), 5_300_000);
        assert_eq!(parse_micros(5.299999999999999), 5_299_999);
        assert_eq!(parse_micros(2.0), 2_000_000);
        assert_eq!(parse_micros(0.0000005), 0);
        assert_eq!(parse_micros(-1.5), -1_500_000);
        // Halves go up, as av_rescale_q rounds: 12.5 -> 13, 82.5 -> 83, 132.5 -> 133 ...
        assert_eq!(fade_ticks(0.5, 25, 1), 13);
        assert_eq!(fade_ticks(3.3, 25, 1), 83);
        assert_eq!(fade_ticks(5.3, 25, 1), 133);
        // ... but `5.299999999999999` is 5.299999 s to FFmpeg, 132.499975 frames.
        assert_eq!(fade_ticks(5.299999999999999, 25, 1), 132);
        // Fractional frame rates: 29.97 is 2997/100.
        assert_eq!((fade_ticks(3.0, 2997, 100), fade_ticks(1.0, 2997, 100)), (90, 30));
    }

    fn step(edge: FadeEdge, st: f64, d: f64) -> FadeStep {
        FadeStep {
            edge,
            tint: FadeTint::Alpha,
            st,
            d,
        }
    }

    #[test]
    fn a_fade_counts_frames_from_the_rounded_start_in_equal_steps() {
        // 24 fps, in from 3.0 s over 1.0 s: frame 72, 24 steps.
        let f = step(FadeEdge::In, 3.0, 1.0);
        let p = |k| f.progress_at_frame(k, 24, 1);
        assert_eq!((p(0), p(71), p(72)), (0.0, 0.0, 0.0));
        assert_eq!((p(73), p(84), p(96), p(97), p(500)), (1.0 / 24.0, 0.5, 1.0, 1.0, 1.0));
        // ... and out is the same count read the other way.
        let f = step(FadeEdge::Out, 3.0, 1.0);
        assert_eq!(
            (
                f.progress_at_frame(72, 24, 1),
                f.progress_at_frame(84, 24, 1),
                f.progress_at_frame(96, 24, 1)
            ),
            (1.0, 0.5, 0.0)
        );
        assert_eq!(f.progress_at_frame(0, 24, 1), 1.0);
        // Off the grid: out from 3.3 s over 0.7 s at 24 fps starts at frame 79 (79.2)
        // and takes 17 steps (16.8), not 16.8, and not from 3.3 exactly.
        let f = step(FadeEdge::Out, 3.3, 0.7);
        assert_eq!(f.progress_at_frame(79, 24, 1), 1.0);
        assert_eq!(f.progress_at_frame(80, 24, 1), 16.0 / 17.0);
        assert_eq!(f.progress_at_frame(96, 24, 1), 0.0);
        // A fade shorter than half a frame is one step, not a division by zero.
        let f = step(FadeEdge::In, 1.5, 0.01);
        assert_eq!((f.progress_at_frame(36, 24, 1), f.progress_at_frame(37, 24, 1)), (0.0, 1.0));
        // `progress(t)` is the same read at the frame `t` is on.
        let f = step(FadeEdge::In, 3.0, 1.0);
        assert_eq!(f.progress(3.5, 24, 1), 0.5);
        assert_eq!(f.progress(3.0 + 1.0 / 29.97, 2997, 100), f.progress_at_frame(91, 2997, 100));
    }

    #[test]
    fn a_motion_offset_is_in_frame_fractions_and_exact() {
        let asset = asset();
        let (tl, fx) = pair(&asset, TransitionKind::SlideLeft, 2.0);
        let keys = timing(&tl, &fx, 1).motion_keys().unwrap();
        // Public, so a renderer can evaluate the curve the graph prints.
        close(keys.at(1.0), (0.5, 0.0));
        close(keys.at(-3.0), (1.0, 0.0));
        // 4:2:0 overlay placement truncates the pixel offset to an even number: 0.5 of
        // a 33 px frame is 16.5 px, which the overlay draws at 16.
        assert_eq!(((keys.at(1.0).0 * 33.0) as i32) & !1, 16);
    }

    #[test]
    fn clips_come_with_their_own_fx_even_when_stored_out_of_order() {
        let asset = asset();
        let (mut shuffled, _) = pair(&asset, TransitionKind::Crossfade, 1.0);
        // Store the incoming clip first: the table is indexed by storage, not by time.
        shuffled.tracks[0].clips.reverse();
        let rows: Vec<_> = clips_with_fx(&shuffled, std::slice::from_ref(&asset)).collect();
        assert_eq!(rows.len(), 2);
        let (ti, ci, incoming, fx_in) = &rows[0];
        assert_eq!((*ti, *ci, incoming.timeline_start), (0, 0, 10.0));
        assert_eq!((fx_in.xfade_in, fx_in.tail), (1.0, 0.0));
        let (ti, ci, outgoing, fx_out) = &rows[1];
        assert_eq!((*ti, *ci, outgoing.timeline_start), (0, 1, 0.0));
        assert_eq!((fx_out.xfade_in, fx_out.tail), (0.0, 1.0));
        // ... and it is the same table `transition_fx` returns.
        let table = transition_fx(&shuffled, std::slice::from_ref(&asset));
        assert_eq!(rows.iter().map(|r| r.3).collect::<Vec<_>>(), table);
    }

    #[test]
    fn a_head_padded_proxy_is_told_by_its_path_alone() {
        let hash = "0123456789abcdef";
        assert!(is_head_padded_proxy(&format!(
            "/cache/kerf/proxies/{hash}{HEAD_PADDED_SUFFIX}"
        )));
        assert!(!is_head_padded_proxy(&format!("/cache/kerf/proxies/{hash}.mp4")));
        assert!(!is_head_padded_proxy(&format!("/cache/kerf/elsewhere/{hash}.lead.mp4")));
        assert!(!is_head_padded_proxy(&format!("/cache/proxies/{hash}.lead.mp4")));
        assert!(!is_head_padded_proxy("/cache/kerf/proxies/short.lead.mp4"));
        assert!(!is_head_padded_proxy("/media/clip.lead.mp4"));
    }

    #[test]
    fn a_frame_rate_is_the_rational_ffmpeg_parses_its_text_to() {
        let r = |fps: f64| Rational::from_fps(fps).map(|r| (r.num, r.den));
        assert_eq!(r(24.0), Some((24, 1)));
        assert_eq!(r(25.0), Some((25, 1)));
        assert_eq!(r(60.0), Some((60, 1)));
        // The spelling decides the grid: 29.97 is 2997/100 and its neighbour, the exact
        // NTSC rate, 30000/1001 (`nominal_fps` snaps jittery footage to the former).
        assert_eq!(r(29.97), Some((2997, 100)));
        assert_eq!(r(30000.0 / 1001.0), Some((30000, 1001)));
        assert_eq!(r(24000.0 / 1001.0), Some((24000, 1001)));
        assert_eq!(r(23.976), Some((2997, 125)));
        assert_eq!(r(0.5), Some((1, 2)));
        assert_eq!(r(59.94), Some((2997, 50)));
        // The `fps=` filter parses with a larger limit than the `color=r=` canvas: the two
        // agree on every standard rate and part at a rate with big terms.
        let f = |fps: f64| Rational::from_fps_filter(fps).map(|r| (r.num, r.den));
        for same in [24.0, 25.0, 29.97, 30000.0 / 1001.0, 59.94, 23.976, 0.5, 144.0] {
            assert_eq!(r(same), f(same), "{same}");
        }
        assert_eq!(
            (r(29.970029), f(29.970029)),
            (Some((92997, 3103)), Some((29_970_029, 1_000_000)))
        );
        assert_eq!(f(1.23456789012345), Some((1_973_935_681, 1_598_887_916)));
        // A rate FFmpeg refuses.
        for bad in [0.0, -24.0, f64::NAN, f64::INFINITY] {
            assert_eq!(r(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_slot_of_a_frame_is_exact_and_the_time_the_graph_reads_is_not() {
        let ntsc = Rational::from_fps(29.97).unwrap();
        // The exact boundary of frame 2997 is 100 s; FFmpeg evaluates it a hair off.
        assert_eq!(ntsc.exact_time(2997), 100.0);
        assert_eq!(ntsc.exact_time(0), 0.0);
        let k = (1..5000u64).find(|&k| ntsc.frame_time(k) != ntsc.exact_time(k)).unwrap();
        assert!((ntsc.frame_time(k) - ntsc.exact_time(k)).abs() < 1e-9);
        // The nearest frame to a time, and to a negative one.
        let r24 = Rational::new(24, 1).unwrap();
        assert_eq!((r24.frame_at(1.0), r24.frame_at(1.02), r24.frame_at(-3.0)), (24, 24, 0));
        // The frame on screen is the one whose slot holds the time — including a boundary
        // that floating point put a hair short of.
        assert_eq!(
            (
                r24.frame_containing(1.02),
                r24.frame_containing(1.05),
                r24.frame_containing(-3.0)
            ),
            (24, 25, 0)
        );
        assert!((0..2000u64).all(|k| r24.frame_containing(k as f64 / 24.0) == k));
        assert!((0..2000u64).all(|k| ntsc.frame_containing(ntsc.exact_time(k)) == k));
        assert_eq!(ntsc.frame_at(ntsc.exact_time(90)), 90);
        assert!(Rational::new(0, 1).is_none() && Rational::new(1, 0).is_none());
    }
}
