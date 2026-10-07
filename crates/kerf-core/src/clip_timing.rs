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
//! (`visible`, `motion_at`). Moving a number here moves both.
//!
//! **Video only.** `audio_clip_chain` composes the same [`ClipFx`] differently
//! (`afade_in` joins the fade-in, and the tail is its fade-out), so it keeps its
//! own arithmetic and shares only [`ClipTiming::duration`], the window and the seek.
//!
//! Pure: no I/O, no machine reads. The one outside dependency is the file-name
//! convention `is_head_padded_proxy` reads, which is a string function.

use crate::engine::is_head_padded_proxy;
use crate::model::{interpolate, Asset, Clip, Hdr, Timeline};

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
    /// The clip's input is a head-padded proxy (see `is_head_padded_proxy`). Read
    /// from the start with no seek, such an input opens with the pad's clone of the
    /// first frame, which the original it stands for has no frame for.
    pub head_pad: bool,
}

/// Compute the [`ClipFx`] for every clip (indexed by ffmpeg input index, i.e.
/// track-then-clip order), resolving each `transition_in` against the clip that
/// precedes it on the same track in timeline order.
pub fn transition_fx(timeline: &Timeline, assets: &[Asset]) -> Vec<ClipFx> {
    let total_clips: usize = timeline.tracks.iter().map(|t| t.clips.len()).sum();
    let mut fx = vec![ClipFx::default(); total_clips];
    for (flat, clip) in timeline.tracks.iter().flat_map(|t| t.clips.iter()).enumerate() {
        let asset = assets.iter().find(|a| a.id == clip.asset_id);
        fx[flat].hdr = asset.and_then(|a| a.hdr());
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

/// The offset a motion transition puts on a clip, as keyframes over **clip-local**
/// seconds in frame widths (`x`) and heights (`y`). Both channels share their
/// times; they are kept apart because the graph prints each as its own expression.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionKeys {
    pub x: Vec<(f64, f64)>,
    pub y: Vec<(f64, f64)>,
}

impl MotionKeys {
    /// The offset at clip-local time `local`: piecewise linear, held flat beyond
    /// the first and last key — the same curve `keyframe_expr` writes out.
    fn at(&self, local: f64) -> (f64, f64) {
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

    /// The span of **timeline** time the clip is on screen, `(start, end)`, tail
    /// included.
    pub fn window(&self) -> (f64, f64) {
        (self.clip.timeline_start, self.clip.timeline_end() + self.fx.tail)
    }

    /// Whether the clip is on screen at timeline time `t`. Both ends count, as the
    /// graph's `enable='between(t,start,end)'` does.
    pub fn visible(&self, t: f64) -> bool {
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
    /// `t`, in frame widths and heights; `(0, 0)` for a clip that does not move.
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
    fn a_plain_clip_is_on_screen_for_its_own_length_and_nothing_else_moves() {
        let asset = asset();
        let tl = single(vec![make_clip(asset.id, 5.0, 9.0, 3.0)]);
        let fx = transition_fx(&tl, std::slice::from_ref(&asset));
        assert_eq!(fx, vec![ClipFx::default()]);
        let t = timing(&tl, &fx, 0);
        assert_eq!((t.duration(), t.window()), (4.0, (3.0, 7.0)));
        // Both ends count, as the graph's `between(t,3,7)` does.
        assert!(t.visible(3.0) && t.visible(7.0) && t.visible(5.0));
        assert!(!t.visible(2.999) && !t.visible(7.001));
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
        assert!(out.visible(10.5) && !out.visible(11.5));
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
        assert!(out.visible(12.0) && !out.visible(12.001));
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
}
