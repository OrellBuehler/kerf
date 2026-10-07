//! The [`Planner`]: a cut prepared once, a [`RenderPlan`] asked for at any frame.
//!
//! `RenderPlan::at` works out a frame from scratch — the rendered timeline, the
//! asset facts, every clip's transitions — which is right for one still and wrong for
//! a playback loop that wants thirty plans a second. A `Planner` does the per-cut
//! work in [`Planner::new`] (`for_render`, `transition_fx`, geometry, the asset map,
//! each clip's source window, fades, motion keys and fonts) plus a
//! **per-track clip index** (sorted by start, with a running maximum of window ends), so
//! [`Planner::at`] and [`Planner::at_frame`] binary-search to the clips that have started
//! and walk back only while an earlier window could still reach the frame — `O(log n)` plus
//! the clips on screen for a cut whose clips follow one another, and `O(n)` for a track
//! with one very long clip under many short ones (the running maximum never lets the walk
//! stop early).
//!
//! **Still and Motion.** [`PlanMode::Still`] is A0's contract — which clips
//! `active_video_clips` returns, the transform sampled at `t`. [`PlanMode::Motion`]
//! is the export graph at output frame `k`, and differs from the still where the graph
//! does (every difference is in this file, none in the renderers):
//!
//! 1. every expression the graph evaluates — `enable`, keyframes, a fade, the overlay
//!    position, `drawtext` — is evaluated at FFmpeg's own frame time
//!    ([`Rational::frame_time`]: `k * (den / num)`, an ulp off `k / fps`), and every
//!    exact quantity (the slot boundary the source-frame pick works from) is
//!    [`Rational::exact_time`];
//! 2. an outgoing clip plays on its `tail`: its window and source reach past its end;
//! 3. fades, dips and the dissolve ramp are timed from the clip's start, over a
//!    duration that includes the tail; slide and push offsets join the position;
//! 4. a keyframed clip is never padded and always runs `scale eval=frame`
//!    ([`Placement`](crate::layer_geometry::Placement));
//! 5. keyframed opacity is a `geq` alpha rather than the RGB round trip, and a keyframed
//!    zoom is read at the *source* frame's time and not always shown at all
//!    ([`Animated`]);
//! 6. a text overlay is on `between(t,start,end)`, end included;
//! 7. the source frame is the `fps` filter's pick (carried as [`PlanTiming`], decided
//!    by `Pick` in the next slice — until then a Motion plan holds **candidates** at a
//!    clip's closing edge: the layers whose `enable` window contains the frame time);
//! 8. a reframe resamples with `cubic` (the still's is `line`);
//! 9. tone mapping follows the fit `scale` instead of preceding the geometry;
//! 10. the canvas carries the delivery's pixel format and gif.
//!
//! The delivery's `fps` is the rational FFmpeg makes of the text the graph prints.

use std::collections::HashMap;

use uuid::Uuid;

use crate::clip_timing::{
    clip_source_window, clips_with_fx, ffmpeg_frame_time, ClipFx, ClipTiming, FadeStep, MotionKeys, Rational,
};
use crate::engine::{render_geometry, safe_color, valid_color, Container, ExportOptions};
use crate::error::{Error, Result};
use crate::model::{Asset, Hdr, Projection, StreamKind, TextOverlay, Timeline, VideoEffect};
use crate::plan_caps::LayerRef;
use crate::render_plan::{
    clip_source_time, composite_matrix, Animated, CompositeColorPolicy, LayerFx, PlanCanvas, PlanLayer, PlanMode, PlanReframe,
    PlanStream, PlanText, PlanTiming, ReframeInterp, RenderPlan, YuvMatrix,
};

fn frame_index(k: u64) -> i64 {
    i64::try_from(k).unwrap_or(i64::MAX)
}

/// What a [`Planner`] is asked to plan. Build one with [`PlanRequest::still`] or
/// [`PlanRequest::motion`]: the next slice adds the media resolver to it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PlanRequest {
    pub mode: PlanMode,
    /// The composite colour policy of the FFmpeg in use (`composite_color_policy()`): an
    /// input, so a test pins any of the three without an FFmpeg.
    pub color: CompositeColorPolicy,
}

impl PlanRequest {
    /// What `build_still_args` draws ([`PlanMode::Still`]).
    pub fn still(color: CompositeColorPolicy) -> Self {
        Self {
            mode: PlanMode::Still,
            color,
        }
    }

    /// What the export graph draws at an output frame ([`PlanMode::Motion`]).
    pub fn motion(color: CompositeColorPolicy) -> Self {
        Self {
            mode: PlanMode::Motion,
            color,
        }
    }
}

/// What a plan needs of an asset, taken once.
#[derive(Debug, Clone)]
struct AssetFacts {
    name: String,
    path: String,
    is_image: bool,
    duration: f64,
    /// `None` when the asset has no video picture of a known size.
    stream: Option<PlanStream>,
    projection: Option<Projection>,
}

/// One video clip, with everything about it that does not change from frame to frame.
struct PlannedClip {
    /// Index of the clip's track in the rendered timeline, and of the clip in it.
    track: usize,
    index: usize,
    asset: Option<AssetFacts>,
    fx: ClipFx,
    /// The overlay's `enable` window, tail included.
    window: (f64, f64),
    source_window: (f64, f64),
    hdr: Option<Hdr>,
    fades: Vec<FadeStep>,
    keys: Option<MotionKeys>,
    tail: bool,
    animated: Option<Animated>,
    effects: Vec<VideoEffect>,
}

/// The clips of one video track sorted by start, with the running maximum of their
/// window ends — enough to answer "which windows contain `t`" without a full scan.
struct TrackIndex {
    /// Indices into the planner's `clips`, in timeline order (ties in storage order).
    order: Vec<usize>,
    /// `max_end[i]` is the largest window end among `order[..=i]`.
    max_end: Vec<f64>,
}

/// A text overlay with the machine-dependent part (its font file) resolved once.
struct PlannedText {
    overlay: TextOverlay,
    color: String,
    bg: Option<String>,
    font_file: Option<std::path::PathBuf>,
    synthetic_bold: bool,
}

/// A cut prepared for planning: see the [module](self).
pub struct Planner {
    mode: PlanMode,
    canvas: PlanCanvas,
    rendered: Timeline,
    clips: Vec<PlannedClip>,
    tracks: Vec<TrackIndex>,
    overlays: Vec<PlannedText>,
}

impl Planner {
    /// Prepare `timeline` for planning. Like the still and the export, it plans the cut as
    /// [`Timeline::for_render`] sees it (a muted or solo-shadowed track, or a disabled
    /// clip, is absent) — so a range, a slice or a delivery is applied by the caller first
    /// (`Timeline::slice`, `Timeline::for_delivery`), as the graph builders have it.
    ///
    /// A clip whose asset is not in `assets` is an error only when a frame asks for it,
    /// as for the still. `Err` for a frame rate FFmpeg would refuse, in either mode.
    pub fn new(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, req: PlanRequest) -> Result<Self> {
        let rendered = timeline.for_render();
        let geom = render_geometry(&rendered, assets, opts);
        // A rate FFmpeg would not parse is an error in either mode: nothing sensible can be
        // said about frames of a graph that does not build.
        let parse = |r: Option<Rational>| {
            r.ok_or_else(|| Error::InvalidArgument(format!("frame rate {} is not one FFmpeg parses", geom.fps)))
        };
        let (fps, pick_fps) = (
            parse(Rational::from_fps(geom.fps))?,
            parse(Rational::from_fps_filter(geom.fps))?,
        );
        let facts: HashMap<Uuid, AssetFacts> = assets
            .iter()
            .map(|a| {
                let stream = a
                    .streams
                    .iter()
                    .find(|s| s.kind == StreamKind::Video)
                    .and_then(PlanStream::of);
                let facts = AssetFacts {
                    name: a.name.clone(),
                    path: a.path.clone(),
                    is_image: a.is_image(),
                    duration: a.duration,
                    stream,
                    projection: a.projection(),
                };
                (a.id, facts)
            })
            .collect();

        let mut clips: Vec<PlannedClip> = Vec::new();
        let mut by_track: Vec<(usize, Vec<usize>)> = Vec::new();
        for (ti, ci, clip, fx) in clips_with_fx(&rendered, assets) {
            if rendered.tracks[ti].kind != StreamKind::Video {
                continue;
            }
            let timing = ClipTiming::new(clip, &fx);
            let kf = clip.sorted_keyframes();
            let effects = clip
                .effects
                .iter()
                .map(|e| match e {
                    VideoEffect::ChromaKey {
                        color,
                        similarity,
                        blend,
                    } => VideoEffect::ChromaKey {
                        color: safe_color(color, "green").to_string(),
                        similarity: *similarity,
                        blend: *blend,
                    },
                    other => other.clone(),
                })
                .collect();
            if by_track.last().is_none_or(|(t, _)| *t != ti) {
                by_track.push((ti, Vec::new()));
            }
            if let Some((_, members)) = by_track.last_mut() {
                members.push(clips.len());
            }
            clips.push(PlannedClip {
                track: ti,
                index: ci,
                asset: facts.get(&clip.asset_id).cloned(),
                fx,
                window: timing.window(),
                source_window: clip_source_window(clip, &fx),
                hdr: fx.hdr,
                fades: timing.fades(),
                keys: timing.motion_keys(),
                tail: fx.tail > 0.0,
                animated: clip.is_animated().then(|| Animated {
                    rotates: kf.iter().any(|k| k.rotation != 0.0),
                    opacity: kf.iter().any(|k| k.opacity < 1.0),
                    zooms: kf.iter().any(|k| k.scale != kf[0].scale),
                }),
                effects,
            });
        }
        let tracks = by_track
            .into_iter()
            .map(|(_, mut order)| {
                // Stable, as the export's own ordering is: a tie keeps storage order.
                order.sort_by(|&a, &b| clips[a].window.0.total_cmp(&clips[b].window.0));
                let mut running = f64::NEG_INFINITY;
                let max_end = order
                    .iter()
                    .map(|&c| {
                        running = running.max(clips[c].window.1);
                        running
                    })
                    .collect();
                TrackIndex { order, max_end }
            })
            .collect();

        let overlays = rendered
            .overlays
            .iter()
            .map(|o| {
                // Resolving a font loads the system font database: only when one is asked for.
                let resolved = o.font.as_deref().and_then(|f| crate::fonts::resolve_font_file(f, o.bold));
                PlannedText {
                    color: safe_color(&o.color, "white").to_string(),
                    bg: o.bg.clone().filter(|bg| valid_color(bg)),
                    synthetic_bold: o.bold && resolved.as_ref().map(|r| r.1) != Some(true),
                    font_file: resolved.map(|r| r.0),
                    overlay: o.clone(),
                }
            })
            .collect();

        Ok(Self {
            mode: req.mode,
            canvas: PlanCanvas {
                width: geom.width,
                height: geom.height,
                fit: geom.fit,
                scaler: geom.scaler,
                matrix: YuvMatrix::Bt601,
                policy: req.color,
                fps,
                pick_fps,
                pix_fmt: geom.pix_fmt,
                gif: opts.container == Container::Gif,
            },
            rendered,
            clips,
            tracks,
            overlays,
        })
    }

    pub fn mode(&self) -> PlanMode {
        self.mode
    }

    pub fn canvas(&self) -> &PlanCanvas {
        &self.canvas
    }

    /// The plan for timeline time `t`: the output frame **on screen** at `t`, the one whose
    /// slot `[k/fps, (k+1)/fps)` holds it ([`Rational::frame_containing`]; a `t` that is
    /// `k / fps` in floating point is frame `k`). A still is planned at `t` itself, with that
    /// frame deciding how far into a fade it is; an export frame at the frame's own time.
    pub fn at(&self, t: f64) -> Result<RenderPlan> {
        let k = self.canvas.fps.frame_containing(t);
        match self.mode {
            PlanMode::Still => {
                let t = t.max(0.0);
                self.plan(t, t, frame_index(k))
            }
            PlanMode::Motion => self.at_frame(k),
        }
    }

    /// The plan for output frame `k`. [`PlanMode::Motion`] evaluates the graph's expressions
    /// at FFmpeg's time for the frame; a still takes the exact start of its slot.
    pub fn at_frame(&self, k: u64) -> Result<RenderPlan> {
        let fps = self.canvas.fps;
        let slot = fps.exact_time(k);
        let eval = match self.mode {
            PlanMode::Still => slot,
            PlanMode::Motion => ffmpeg_frame_time(k, fps.num, fps.den),
        };
        self.plan(slot, eval, frame_index(k))
    }

    /// The clips of `track` that could be on screen at `eval`, bottom to top within the track.
    fn candidates(&self, track: &TrackIndex, eval: f64) -> Vec<usize> {
        let upto = track.order.partition_point(|&c| self.clips[c].window.0 <= eval);
        let mut found = Vec::new();
        let mut i = upto;
        // The prefix maximum bounds the scan: once no earlier window reaches `eval`, stop.
        while i > 0 && track.max_end[i - 1] >= eval {
            i -= 1;
            if self.clips[track.order[i]].window.1 >= eval {
                found.push(track.order[i]);
            }
        }
        found.reverse();
        found
    }

    /// `slot` is the plan's time (and the source-time map's), `eval` the time the graph's
    /// expressions are evaluated at, `frame` the output frame fades are counted in.
    fn plan(&self, slot: f64, eval: f64, frame: i64) -> Result<RenderPlan> {
        let motion = self.mode == PlanMode::Motion;
        let mut layers = Vec::new();
        let mut pictureless = Vec::new();
        for track in &self.tracks {
            for ci in self.candidates(track, eval) {
                let planned = &self.clips[ci];
                let clip = &self.rendered.tracks[planned.track].clips[planned.index];
                // The index only narrows to windows that could hold `eval`. The export asks
                // the overlay's own `enable` (tail included, closed at the end); a still shows
                // the clip's own span, half open.
                let on = if motion {
                    ClipTiming::new(clip, &planned.fx).enabled(eval)
                } else {
                    eval < clip.timeline_end()
                };
                if !on {
                    continue;
                }
                let asset = planned.asset.as_ref().ok_or(Error::AssetNotFound(clip.asset_id))?;
                let Some(stream) = asset.stream.clone() else {
                    pictureless.push(LayerRef {
                        index: layers.len(),
                        name: asset.name.clone(),
                    });
                    continue;
                };
                let local = (eval - clip.timeline_start).max(0.0);
                let reframe = clip.reframe_at(local).map(|pose| PlanReframe {
                    pose,
                    interp: if motion { ReframeInterp::Cubic } else { ReframeInterp::Line },
                });
                layers.push(PlanLayer {
                    clip_id: clip.id,
                    asset_id: clip.asset_id,
                    track: planned.track,
                    path: asset.path.clone(),
                    is_image: asset.is_image,
                    source_time: clip_source_time(clip, asset.duration, slot),
                    clip_time: local,
                    stream,
                    transform: clip.transform_at(local),
                    color: clip.color,
                    name: asset.name.clone(),
                    projection: asset.projection,
                    hdr: planned.hdr,
                    effects: planned.effects.clone(),
                    mask: clip.mask.map(|m| m.normalized()),
                    reframe,
                    fx: LayerFx {
                        fades: planned.fades.clone(),
                        motion: planned.keys.as_ref().map_or((0.0, 0.0), |k| k.at(eval - clip.timeline_start)),
                        tail: motion && planned.tail && eval >= clip.timeline_end(),
                    },
                    animated: planned.animated,
                    timing: PlanTiming {
                        window: planned.window,
                        source_window: planned.source_window,
                        speed: clip.speed_mag(),
                        reversed: clip.is_reversed(),
                    },
                });
            }
        }
        // A still has no layer for a clip playing on under a transition, and the export keeps
        // drawing it — around an incoming clip that does not cover it, in front of black.
        // Whether its tail window is open is read at the time the graph would evaluate it for
        // this frame, and at `t` itself.
        let mut tails = Vec::new();
        if !motion {
            let fps = self.canvas.fps;
            let graph_time = ffmpeg_frame_time(frame.max(0) as u64, fps.num, fps.den);
            let mut open: Vec<usize> = Vec::new();
            for track in &self.tracks {
                for at in [eval, graph_time] {
                    for ci in self.candidates(track, at) {
                        let clip = &self.rendered.tracks[self.clips[ci].track].clips[self.clips[ci].index];
                        if self.clips[ci].fx.tail > 0.0 && at >= clip.timeline_end() && !open.contains(&ci) {
                            open.push(ci);
                            tails.push(LayerRef {
                                index: layers.len(),
                                name: self.clips[ci].asset.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                            });
                        }
                    }
                }
            }
        }
        let matrix = composite_matrix(&layers, self.canvas.policy).0;
        let overlays = self
            .overlays
            .iter()
            .filter(|p| {
                let o = &p.overlay;
                eval >= o.start && if motion { eval <= o.end } else { eval < o.end }
            })
            .map(|p| {
                let o = &p.overlay;
                let (x, y, alpha) = o.sample(eval);
                PlanText {
                    id: o.id,
                    text: o.text.clone(),
                    size: o.size,
                    pos: (x, y),
                    alpha,
                    color: p.color.clone(),
                    bg: p.bg.clone(),
                    font_file: p.font_file.clone(),
                    synthetic_bold: p.synthetic_bold,
                }
            })
            .collect();
        Ok(RenderPlan {
            time: slot,
            mode: self.mode,
            frame,
            canvas: PlanCanvas {
                matrix,
                ..self.canvas.clone()
            },
            layers,
            overlays,
            pictureless,
            tails,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip_timing::FadeTint;
    use crate::engine::test_support::{make_clip, single, test_asset, timeline_of, video_stream, video_track};
    use crate::model::{Delivery, Fit, Framing, Keyframe, Mask, Track, Transform, Transition, TransitionKind};
    use crate::plan_caps::{EffectKinds, GpuCaps, Unsupported};
    use crate::render_plan::active_video_clips;

    const FIXED: CompositeColorPolicy = CompositeColorPolicy::FixedBt601;

    fn asset() -> Asset {
        let mut a = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        a.streams[0].pix_fmt = Some("yuv420p".into());
        a
    }

    fn opts(fps: f64) -> ExportOptions {
        ExportOptions {
            fps: Some(fps),
            ..ExportOptions::default()
        }
    }

    fn planner(tl: &Timeline, assets: &[Asset], mode: PlanMode, opts: &ExportOptions) -> Planner {
        Planner::new(tl, assets, opts, PlanRequest { mode, color: FIXED }).unwrap()
    }

    fn keyed(time: f64, scale: f64, pos_x: f64) -> Keyframe {
        Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
        }
    }

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    type Gone = fn(&Unsupported) -> bool;

    fn has(why: &[Unsupported], pick: Gone) -> bool {
        why.iter().any(pick)
    }

    #[test]
    fn a_still_plan_is_what_active_video_clips_returns_across_a_busy_cut() {
        let (a, b) = (asset(), asset());
        let mut fast = make_clip(a.id, 10.0, 30.0, 2.0);
        fast.speed = 2.0;
        let mut back = make_clip(b.id, 5.0, 15.0, 4.0);
        back.speed = -1.5;
        let mut moving = make_clip(a.id, 0.0, 6.0, 7.5);
        moving.keyframes = vec![keyed(0.0, 1.0, 0.0), keyed(3.0, 0.5, 0.2)];
        let mut off = make_clip(a.id, 0.0, 3.0, 5.0);
        off.enabled = false;
        // Stored out of timeline order, one clip overlapping another on its track, a
        // disabled clip, and a muted track: none of which the plan may show or reorder.
        let tl = timeline_of(vec![
            video_track(vec![moving, fast, off]),
            video_track(vec![back.clone(), make_clip(a.id, 0.0, 4.0, 5.0)]),
            Track {
                muted: true,
                ..video_track(vec![make_clip(b.id, 0.0, 99.0, 0.0)])
            },
        ]);
        let assets = [a, b];
        let rendered = tl.for_render();
        let planner = planner(&tl, &assets, PlanMode::Still, &ExportOptions::default());
        let edges = [2.0, 4.0, 5.0, 7.5, 9.0, 12.0, 13.5, back.timeline_end()];
        for t in (0..=300).map(|i| f64::from(i) * 0.05).chain(edges) {
            let want = active_video_clips(&rendered, &assets, t);
            let plan = planner.at(t).unwrap();
            assert_eq!(plan.layers.len(), want.len(), "t = {t}");
            for (l, w) in plan.layers.iter().zip(&want) {
                assert_eq!(
                    (l.clip_id, l.source_time, l.clip_time, l.transform),
                    (w.clip.id, w.source_time, w.local_time, w.transform()),
                    "t = {t}"
                );
                assert!(rendered.tracks[l.track].clips.iter().any(|c| c.id == l.clip_id));
            }
        }
    }

    #[test]
    fn the_index_finds_what_a_scan_of_every_clip_finds_across_a_random_cut() {
        let a = asset();
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut clips_seen = 0;
        for fps in [24.0, 25.0, 29.97, 30.0, 60.0] {
            // Three tracks of overlapping clips off the frame grid, some retimed or reversed,
            // some dissolving in (which gives the one before them a tail).
            let tracks = (0..3)
                .map(|_| {
                    video_track(
                        (0..40)
                            .map(|_| {
                                let src_in = (xorshift(&mut rng) % 5000) as f64 / 100.0;
                                let len = 0.1 + (xorshift(&mut rng) % 800) as f64 / 100.0;
                                let mut c = make_clip(a.id, src_in, src_in + len, (xorshift(&mut rng) % 4000) as f64 / 100.0);
                                if xorshift(&mut rng).is_multiple_of(7) {
                                    c.speed = [0.5, 2.0, -1.0][(xorshift(&mut rng) % 3) as usize];
                                }
                                if xorshift(&mut rng).is_multiple_of(5) {
                                    c.transition_in = Some(Transition {
                                        kind: TransitionKind::Crossfade,
                                        duration: 0.3,
                                    });
                                }
                                c
                            })
                            .collect(),
                    )
                })
                .collect();
            let tl = timeline_of(tracks);
            let assets = [a.clone()];
            let planner = planner(&tl, &assets, PlanMode::Motion, &opts(fps));
            let rate = planner.canvas().fps;
            let rows: Vec<_> = clips_with_fx(&tl, &assets).collect();
            for k in 0..(48.0 * fps) as u64 {
                let t = ffmpeg_frame_time(k, rate.num, rate.den);
                let mut want = Vec::new();
                for ti in 0..3 {
                    let mut track: Vec<_> = rows.iter().filter(|r| r.0 == ti).collect();
                    track.sort_by(|x, y| x.2.timeline_start.total_cmp(&y.2.timeline_start));
                    want.extend(track.iter().filter(|r| ClipTiming::new(r.2, &r.3).enabled(t)).map(|r| r.2.id));
                }
                let got: Vec<_> = planner.at_frame(k).unwrap().layers.iter().map(|l| l.clip_id).collect();
                clips_seen += got.len();
                assert_eq!(got, want, "{fps} fps, frame {k}, t = {t:?}");
            }
        }
        assert!(clips_seen > 5_000, "the cut must keep clips on screen ({clips_seen})");
    }

    #[test]
    fn a_frame_aligned_clip_misses_its_first_frame_where_ffmpeg_reads_the_time_just_under_it() {
        // `rendered.rs` pins that the export does not draw a clip on the frame it starts on
        // when `k * (den / num)` lands an ulp below the start: frame 5 of a 24 fps export.
        let a = asset();
        let tl = single(vec![make_clip(a.id, 0.0, 1.0, 5.0 / 24.0)]);
        let m24 = planner(&tl, std::slice::from_ref(&a), PlanMode::Motion, &opts(24.0));
        assert!(m24.at_frame(5).unwrap().layers.is_empty());
        assert_eq!(m24.at_frame(6).unwrap().layers.len(), 1);
        // 25 fps never drops one, and a still (which has no graph clock) shows it at 5/24.
        let tl = single(vec![make_clip(a.id, 0.0, 1.0, 0.2)]);
        let m25 = planner(&tl, std::slice::from_ref(&a), PlanMode::Motion, &opts(25.0));
        assert_eq!(m25.at_frame(5).unwrap().layers.len(), 1);
        let tl = single(vec![make_clip(a.id, 0.0, 1.0, 5.0 / 24.0)]);
        let still = planner(&tl, std::slice::from_ref(&a), PlanMode::Still, &opts(24.0));
        assert_eq!(still.at_frame(5).unwrap().layers.len(), 1);
        // A candidate at the closing edge carries the window the pick needs.
        let layer = &m24.at_frame(6).unwrap().layers[0];
        assert_eq!(layer.timing.window, (5.0 / 24.0, 5.0 / 24.0 + 1.0));
        assert_eq!(
            (layer.timing.speed, layer.timing.reversed, layer.timing.source_window),
            (1.0, false, (0.0, 1.0))
        );
    }

    #[test]
    fn a_dissolve_is_two_layers_the_outgoing_on_its_tail_and_the_incoming_on_a_ramp() {
        let a = asset();
        let out = make_clip(a.id, 0.0, 2.0, 0.0);
        let mut inc = make_clip(a.id, 10.0, 14.0, 2.0);
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let tl = single(vec![out, inc]);
        let assets = [a];
        let fps = Rational::new(30, 1).unwrap();
        let motion = planner(&tl, &assets, PlanMode::Motion, &opts(30.0));
        let mid = motion.at_frame(75).unwrap();
        assert_eq!(mid.layers.len(), 2);
        let (outgoing, incoming) = (&mid.layers[0], &mid.layers[1]);
        // The outgoing clip plays on under the cut, from source past its out point.
        assert!(outgoing.fx.tail && outgoing.fx.fades.is_empty());
        assert!((outgoing.source_time - 2.5).abs() < 1e-9);
        assert_eq!(outgoing.timing.window, (0.0, 3.0));
        // The incoming one is on an alpha ramp that fades `d` frames in from frame 60.
        assert!(!incoming.fx.tail);
        assert_eq!(incoming.fx.strength(FadeTint::Alpha, mid.frame, fps), 0.5);
        assert_eq!(incoming.fx.strength(FadeTint::Black, mid.frame, fps), 1.0);
        // Before the cut there is only the outgoing clip, and not on its tail.
        let before = motion.at_frame(59).unwrap();
        assert_eq!((before.layers.len(), before.layers[0].fx.tail), (1, false));
        // A still at the same time has no tail layer (the clip's own span ended) but
        // the incoming clip's ramp, which is what makes A0 refuse it.
        let still = planner(&tl, &assets, PlanMode::Still, &ExportOptions::default())
            .at(2.5)
            .unwrap();
        assert_eq!(still.layers.len(), 1);
        assert_eq!(
            still.layers[0].fx.strength(FadeTint::Alpha, still.frame, still.canvas.fps),
            0.5
        );
        assert!(!still.layers[0].fx.tail);
    }

    #[test]
    fn a_still_is_refused_while_a_tail_window_is_open_even_after_the_ramp_is_over() {
        // 24 fps: A on 0..2 s, B from 2.0 s on a 0.6 s dissolve, B at half size so that A, which the
        // export keeps drawing until 2.6 s, shows around it. The ramp counts 14 frames from frame 48
        // and is over at frame 62, where A's window (read at 2.5833) is still open.
        let a = asset();
        let mut inc = make_clip(a.id, 10.0, 14.0, 2.0);
        inc.transform.scale = 0.5;
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.6,
        });
        let tl = single(vec![make_clip(a.id, 0.0, 2.0, 0.0), inc]);
        let assets = [a];
        let still = planner(&tl, &assets, PlanMode::Still, &opts(24.0));
        let why = |k: u64| {
            let plan = still.at(k as f64 / 24.0).unwrap();
            assert_eq!(plan.frame, k as i64);
            (plan.reasons(&GpuCaps::A0, plan.size(u32::MAX)), plan)
        };
        assert!(why(47).0.is_empty());
        for k in 48..=61 {
            assert!(has(&why(k).0, |u| matches!(u, Unsupported::Fade(..))), "frame {k}");
        }
        // Frame 62: the ramp is over, the tail window is not; nothing but the tail says so.
        let (reasons, plan) = why(62);
        assert!(
            matches!(reasons.as_slice(), [Unsupported::StillTail(t)] if plan.tails == [t.clone()]),
            "{reasons:?}"
        );
        // No cap opens it: a still plan has no layer for that clip.
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            ..GpuCaps::A0
        };
        assert!(has(&plan.reasons(&all, plan.size(u32::MAX)), |u| matches!(
            u,
            Unsupported::StillTail(_)
        )));
        assert!(why(63).0.is_empty() && why(70).0.is_empty());
        // The export frame is the two layers, A on its tail and B all the way up.
        let motion = planner(&tl, &assets, PlanMode::Motion, &opts(24.0)).at_frame(62).unwrap();
        assert_eq!(motion.layers.len(), 2);
        assert!(motion.layers[0].fx.tail && !motion.layers[1].fx.tail);
        assert_eq!(motion.strength(&motion.layers[1], FadeTint::Alpha), 1.0);
        assert!(motion.tails.is_empty());
    }

    #[test]
    fn the_second_half_of_a_dip_is_an_ordinary_frame() {
        // A dip fades the outgoing clip out over the half second before the cut and the incoming
        // one in over the half second after it; it has no tail, and the frames after it are plain.
        let a = asset();
        let mut inc = make_clip(a.id, 10.0, 14.0, 2.0);
        inc.transition_in = Some(Transition {
            kind: TransitionKind::DipToBlack,
            duration: 1.0,
        });
        let tl = single(vec![make_clip(a.id, 0.0, 2.0, 0.0), inc]);
        let still = planner(&tl, std::slice::from_ref(&a), PlanMode::Still, &opts(24.0));
        let refused = |k: u64| {
            let plan = still.at(k as f64 / 24.0).unwrap();
            assert!(plan.tails.is_empty());
            !plan.reasons(&GpuCaps::A0, plan.size(u32::MAX)).is_empty()
        };
        // Out: frames 36 (full) to 48; in: 48 to 60 (full). Both ends are plain.
        let verdicts: Vec<_> = [35, 36, 37, 47, 48, 59, 60, 61, 72].into_iter().map(refused).collect();
        assert_eq!(verdicts, [false, false, true, true, true, true, false, false, false]);
    }

    #[test]
    fn a_time_is_the_frame_on_screen_and_a_rate_ffmpeg_refuses_is_an_error() {
        let a = asset();
        let tl = single(vec![make_clip(a.id, 0.0, 40.0, 0.0)]);
        for mode in [PlanMode::Still, PlanMode::Motion] {
            let p = planner(&tl, std::slice::from_ref(&a), mode, &opts(24.0));
            for k in 0..500u64 {
                let t = k as f64 / 24.0;
                // On the boundary (however floating point spelled it), a hair short of it, and
                // most of the way to the next frame.
                for t in [t, t - 1e-9, t + 0.95 / 24.0] {
                    assert_eq!(p.at(t.max(0.0)).unwrap().frame, k as i64, "{mode:?} t = {t}");
                }
            }
            assert_eq!(p.at(-1.0).unwrap().frame, 0);
            let refused = Planner::new(
                &tl,
                std::slice::from_ref(&a),
                &opts(f64::INFINITY),
                PlanRequest { mode, color: FIXED },
            );
            assert!(matches!(refused, Err(Error::InvalidArgument(_))), "{mode:?}");
        }
    }

    #[test]
    fn a_slide_travels_the_whole_padded_frame_and_a_keyframed_clip_is_not_padded() {
        let a = asset();
        let out = make_clip(a.id, 0.0, 2.0, 0.0);
        let mut inc = make_clip(a.id, 10.0, 14.0, 2.0);
        inc.transition_in = Some(Transition {
            kind: TransitionKind::SlideLeft,
            duration: 2.0,
        });
        let tl = single(vec![out, inc]);
        let plan = planner(&tl, std::slice::from_ref(&a), PlanMode::Motion, &opts(30.0))
            .at_frame(90)
            .unwrap();
        let layer = &plan.layers[1];
        assert_eq!(layer.fx.motion, (0.5, 0.0));
        let (w, h) = (plan.canvas.width, plan.canvas.height);
        let geom = plan.layer_geometry(layer, (w, h)).unwrap();
        // Half a frame to the right, truncated to the 4:2:0 grid; the matte travels with it.
        assert_eq!((geom.origin, geom.matte), ((960, 0), false));
        assert_eq!(geom.layer, (w, h));

        // A keyframed clip that happens to sit at identity is centred by the overlay rather
        // than padded, in the export; the still pads it.
        let mut clip = make_clip(a.id, 0.0, 4.0, 0.0);
        clip.keyframes = vec![keyed(0.0, 1.0, 0.0), keyed(2.0, 1.0, 0.0)];
        let mut tl = single(vec![clip]);
        tl.format = Some(Delivery::new(1080, 1920, Fit::Contain));
        let geometry = |mode| {
            let plan = planner(&tl, std::slice::from_ref(&a), mode, &opts(30.0))
                .at_frame(30)
                .unwrap();
            plan.layer_geometry(&plan.layers[0], (1080, 1920)).unwrap()
        };
        let (still, export) = (geometry(PlanMode::Still), geometry(PlanMode::Motion));
        assert!(still.matte && still.stages.len() == 1);
        assert!(!export.matte && export.stages.len() == 2);
        assert_eq!((export.picture, export.origin), ((1080, 608), (0, 656)));
    }

    #[test]
    fn what_the_compositor_may_not_draw_is_a_function_of_the_plan_and_its_caps() {
        let a = asset();
        let mut clip = make_clip(a.id, 0.0, 4.0, 0.0);
        clip.effects = vec![
            VideoEffect::Blur { sigma: 2.0 },
            VideoEffect::ChromaKey {
                color: "green".into(),
                similarity: 0.1,
                blend: 0.1,
            },
        ];
        clip.mask = Some(Mask::default());
        clip.fade_in = 2.0;
        let mut tl = single(vec![clip]);
        tl.overlays.push(TextOverlay::new("hi", 0.0, 4.0));
        let plan = planner(&tl, std::slice::from_ref(&a), PlanMode::Still, &ExportOptions::default())
            .at(0.5)
            .unwrap();
        let size = plan.size(u32::MAX);
        let why = plan.reasons(&GpuCaps::A0, size);
        assert!(has(&why, |u| matches!(u, Unsupported::Effects(_))));
        assert!(has(&why, |u| matches!(u, Unsupported::Mask(_))));
        assert!(has(&why, |u| matches!(u, Unsupported::Fade(_, _, false))));
        assert!(has(&why, |u| matches!(u, Unsupported::Text)));
        // A0's answer is the answer `unsupported_reasons_at` has always given, as text.
        assert_eq!(
            plan.unsupported_reasons_at(size),
            why.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
        // Each ability removes its own reason and no other; effects one kind at a time.
        let blur = GpuCaps {
            effects: EffectKinds::BLUR,
            ..GpuCaps::A0
        };
        assert!(has(&plan.reasons(&blur, size), |u| matches!(u, Unsupported::Effects(_))));
        let both = EffectKinds::BLUR.union(EffectKinds::CHROMA_KEY);
        let flips: [(GpuCaps, Gone); 3] = [
            (
                GpuCaps {
                    effects: both,
                    ..GpuCaps::A0
                },
                |u| matches!(u, Unsupported::Effects(_)),
            ),
            (
                GpuCaps {
                    mask: true,
                    ..GpuCaps::A0
                },
                |u| matches!(u, Unsupported::Mask(_)),
            ),
            (
                GpuCaps {
                    text: true,
                    ..GpuCaps::A0
                },
                |u| matches!(u, Unsupported::Text),
            ),
        ];
        for (caps, gone) in flips {
            let now = plan.reasons(&caps, size);
            assert!(!has(&now, gone), "{now:?}");
            assert_eq!(now.len(), why.len() - 1 + usize::from(caps.text), "{now:?}");
        }
        // Fades are the export's: the FFmpeg still draws none, so a *still* plan inside one is
        // refused whatever the caps say, and a Motion plan (frame 15 of 30 fps is 0.5 s) by `caps.fades`.
        let fading = GpuCaps {
            fades: true,
            transitions: true,
            ..GpuCaps::A0
        };
        assert!(has(&plan.reasons(&fading, size), |u| matches!(u, Unsupported::Fade(..))));
        let export = planner(&tl, std::slice::from_ref(&a), PlanMode::Motion, &opts(30.0))
            .at_frame(15)
            .unwrap();
        let motion = GpuCaps {
            motion: true,
            ..GpuCaps::A0
        };
        assert!(has(&export.reasons(&motion, size), |u| matches!(u, Unsupported::Fade(..))));
        let now = export.reasons(&GpuCaps { fades: true, ..motion }, size);
        assert!(!has(&now, |u| matches!(u, Unsupported::Fade(..))), "{now:?}");
        let all = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            keyed_opacity: true,
            keyed_zoom: true,
            mask: true,
            text: true,
            reframe: true,
            hdr: true,
            effects: EffectKinds::ALL,
        };
        // (The overlay has no font, which even a text pass cannot draw, and a still plan is
        // still inside the fade.)
        let left = plan.reasons(&all, size);
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(has(&left, |u| matches!(u, Unsupported::TextWithoutFont)) && has(&left, |u| matches!(u, Unsupported::Fade(..))));
        let left = export.reasons(&all, size);
        assert_eq!(left, vec![Unsupported::TextWithoutFont]);
    }

    #[test]
    fn an_export_frame_is_refused_for_what_only_the_export_graph_does() {
        let a = asset();
        let mut fade = make_clip(a.id, 0.0, 4.0, 0.0);
        fade.keyframes = vec![
            Keyframe {
                opacity: 0.2,
                ..keyed(0.0, 1.0, 0.0)
            },
            keyed(2.0, 1.0, 0.0),
        ];
        let tl = single(vec![fade]);
        let assets = [a];
        let at = |mode, o: &ExportOptions| {
            let plan = planner(&tl, &assets, mode, o).at_frame(15).unwrap();
            plan.reasons(&GpuCaps::A0, plan.size(u32::MAX))
        };
        // The still samples the opacity and takes the RGB round trip A0 matches; the export
        // draws a `geq` alpha, which is another arithmetic, and a whole other plan.
        assert!(at(PlanMode::Still, &opts(30.0)).is_empty());
        let why = at(PlanMode::Motion, &opts(30.0));
        assert!(has(&why, |u| matches!(u, Unsupported::Motion)) && has(&why, |u| matches!(u, Unsupported::KeyedOpacity(_))));
        // ... and only an 8-bit 4:2:0 delivery that is not a gif is drawn at all.
        let prores = ExportOptions {
            video_codec: Some("prores_ks".into()),
            ..opts(30.0)
        };
        assert!(has(
            &at(PlanMode::Motion, &prores),
            |u| matches!(u, Unsupported::Delivery(f) if f == "yuv422p10le")
        ));
        assert!(!has(&at(PlanMode::Still, &prores), |u| matches!(u, Unsupported::Delivery(_))));
        let gif = ExportOptions {
            container: Container::Gif,
            ..opts(30.0)
        };
        assert!(has(&at(PlanMode::Motion, &gif), |u| matches!(u, Unsupported::Gif)));
        // A zoom that moves is another thing the export does its own way (`rendered.rs`).
        let mut zoom = make_clip(assets[0].id, 0.0, 4.0, 0.0);
        zoom.keyframes = vec![keyed(0.0, 1.0, 0.0), keyed(2.0, 0.5, 0.0)];
        let zoomed = single(vec![zoom]);
        let plan = planner(&zoomed, &assets, PlanMode::Motion, &opts(30.0)).at_frame(15).unwrap();
        let animated = plan.layers[0].animated.unwrap();
        assert_eq!((animated.zooms, animated.rotates, animated.opacity), (true, false, false));
        assert!(has(&plan.reasons(&GpuCaps::A0, plan.size(u32::MAX)), |u| matches!(
            u,
            Unsupported::KeyedZoom(_)
        )));
        // Keys that never change the scale have no zoom to flag.
        let plan = planner(&tl, &assets, PlanMode::Motion, &opts(30.0)).at_frame(15).unwrap();
        assert!(!plan.layers[0].animated.unwrap().zooms);
        // Nothing else refuses a Motion frame once the compositor says it draws them.
        let caps = GpuCaps {
            motion: true,
            keyed_opacity: true,
            ..GpuCaps::A0
        };
        let plan = planner(&tl, &assets, PlanMode::Motion, &opts(30.0)).at_frame(15).unwrap();
        assert!(plan.reasons(&caps, plan.size(u32::MAX)).is_empty());
    }

    #[test]
    fn a_text_overlay_is_live_to_its_end_in_an_export_frame_and_not_in_a_still() {
        let a = asset();
        let mut title = TextOverlay::new("Fish:chips", 1.0, 2.0);
        title.bold = true;
        title.color = "not a colour".into();
        title.bg = Some("also;bad".into());
        title.keyframes = vec![
            crate::model::TextKeyframe {
                time: 0.0,
                pos_x: 0.2,
                pos_y: 0.8,
                opacity: 0.0,
            },
            crate::model::TextKeyframe {
                time: 1.0,
                pos_x: 0.8,
                pos_y: 0.8,
                opacity: 1.0,
            },
        ];
        let mut tl = single(vec![make_clip(a.id, 0.0, 5.0, 0.0)]);
        tl.overlays.push(title.clone());
        let assets = [a];
        let export = planner(&tl, &assets, PlanMode::Motion, &opts(25.0));
        let rate = export.canvas().fps;
        // Frame 50 is read as `2.0` or an ulp after it; make the overlay end exactly there.
        tl.overlays[0].end = rate.frame_time(50);
        let export = planner(&tl, &assets, PlanMode::Motion, &opts(25.0));
        let still = planner(&tl, &assets, PlanMode::Still, &opts(25.0));
        assert_eq!(export.at_frame(50).unwrap().overlays.len(), 1);
        assert!(export.at_frame(51).unwrap().overlays.is_empty());
        assert!(still.at(rate.frame_time(50)).unwrap().overlays.is_empty());
        // The text is sampled where the graph evaluates it, and what is free-form is made safe.
        let text = &export.at_frame(30).unwrap().overlays[0];
        let t = rate.frame_time(30);
        assert_eq!(
            (text.pos, text.alpha),
            ((title.sample(t).0, title.sample(t).1), title.sample(t).2)
        );
        assert_eq!((text.color.as_str(), text.bg.as_deref()), ("white", None));
        assert!(text.font_file.is_none() && text.synthetic_bold);
    }

    #[test]
    fn a_slice_plans_like_the_cut_it_was_taken_from() {
        let a = asset();
        let mut moving = make_clip(a.id, 5.0, 25.0, 2.0);
        moving.speed = 2.0;
        moving.keyframes = vec![keyed(0.0, 1.0, 0.0), keyed(4.0, 0.5, 0.25), keyed(10.0, 2.0, -0.1)];
        let mut back = make_clip(a.id, 30.0, 40.0, 6.0);
        back.speed = -1.0;
        let tl = timeline_of(vec![video_track(vec![moving]), video_track(vec![back])]);
        let assets = [a];
        let full = planner(&tl, &assets, PlanMode::Still, &ExportOptions::default());
        // A slice cuts clips at both edges: the front one has its source and its keyframes moved.
        let (from, to) = (4.0, 9.5);
        let part = planner(&tl.slice(from, to), &assets, PlanMode::Still, &ExportOptions::default());
        let mut compared = 0;
        for i in 0..110 {
            let t = f64::from(i) * 0.05;
            let (p, f) = (part.at(t).unwrap(), full.at(from + t).unwrap());
            assert_eq!(p.layers.len(), f.layers.len(), "t = {t}");
            compared += p.layers.len();
            for (pl, fl) in p.layers.iter().zip(&f.layers) {
                let (a, b) = (pl.transform, fl.transform);
                assert_eq!(pl.asset_id, fl.asset_id);
                assert!((pl.source_time - fl.source_time).abs() < 1e-9, "t = {t}");
                for (x, y) in [
                    (a.scale, b.scale),
                    (a.pos_x, b.pos_x),
                    (a.rotation, b.rotation),
                    (a.opacity, b.opacity),
                ] {
                    assert!((x - y).abs() < 1e-9, "t = {t}: {a:?} vs {b:?}");
                }
            }
        }
        assert!(compared > 150, "the slice must keep clips on screen ({compared})");
    }

    #[test]
    fn a_delivery_plans_its_own_canvas_and_the_crop_the_clips_carry_for_it() {
        let a = asset();
        let mut clip = make_clip(a.id, 0.0, 4.0, 0.0);
        clip.framings = vec![Framing {
            aspect_w: 9,
            aspect_h: 16,
            crop_left: 0.2,
            crop_right: 0.3,
            crop_top: 0.0,
            crop_bottom: 0.0,
        }];
        let tl = single(vec![clip]);
        let assets = [a];
        let vertical = Delivery::new(1080, 1920, Fit::Cover);
        let plan = |tl: &Timeline| {
            planner(tl, &assets, PlanMode::Still, &ExportOptions::default())
                .at(1.0)
                .unwrap()
        };
        let (landscape, delivered) = (plan(&tl), plan(&tl.for_delivery(vertical)));
        assert_eq!((landscape.canvas.width, landscape.canvas.fit), (1920, Fit::Contain));
        assert_eq!(
            (delivered.canvas.width, delivered.canvas.height, delivered.canvas.fit),
            (1080, 1920, Fit::Cover)
        );
        let crop = |t: Transform| (t.crop_left, t.crop_right);
        assert_eq!(crop(landscape.layers[0].transform), (0.0, 0.0));
        assert_eq!(crop(delivered.layers[0].transform), (0.2, 0.3));
    }
}
