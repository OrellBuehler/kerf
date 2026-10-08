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
//!    zoom is the **last** stage of the clip's chain, after `fps` and after every effect,
//!    mask and rotation ([`Animated`]);
//! 6. a text overlay is on `between(t,start,end)`, end included;
//! 7. the source frame is the `fps` filter's pick ([`Pick::Fps`], reproduced by
//!    [`fps_pick`](crate::frame_pick::fps_pick)): a Motion plan holds **candidates** at a
//!    clip's closing edge (the layers whose `enable` window contains the frame time), and
//!    the pick says which of them has a frame to draw at all;
//! 8. a reframe resamples with `cubic` (the still's is `line`);
//! 9. tone mapping follows the fit `scale` instead of preceding the geometry;
//! 10. the canvas carries the delivery's pixel format and gif.
//!
//! The delivery's `fps` is the rational FFmpeg makes of the text the graph prints.
//!
//! **Which file is decoded** is the request's [`MediaResolver`]: the plan's stream describes
//! that file (a proxy's size and format, not the original's), while the delivery canvas
//! still derives from the originals. [`Planner::span`] evaluates a stretch of the cut for
//! what a compositor draws, and [`SpanPlan::handover`] says where a stream that carries on
//! from the compositor has to start.

use std::borrow::Cow;
use std::collections::HashMap;

use uuid::Uuid;

use crate::clip_timing::{
    clip_seek, clip_source_window, clips_with_fx, ffmpeg_frame_time, ClipFx, ClipTiming, FadeEdge, FadeStep, MotionKeys, Rational,
};
use crate::engine::{render_geometry, safe_color, valid_color, Container, ExportOptions};
use crate::error::{Error, Result};
use crate::frame_pick::{FpsPick, Pick};
use crate::media::{MediaResolver, OriginalMedia, SourceMedia};
use crate::model::{Asset, Hdr, Projection, StreamKind, TextOverlay, Timeline, VideoEffect};
use crate::plan_caps::{GpuCaps, LayerRef};
use crate::render_plan::{
    clip_source_time, composite_matrix, Animated, CompositeColorPolicy, LayerFx, PlanCanvas, PlanLayer, PlanMode, PlanReframe,
    PlanSource, PlanStream, PlanText, PlanTiming, ReframeInterp, RenderPlan, YuvMatrix,
};

fn frame_index(k: u64) -> i64 {
    i64::try_from(k).unwrap_or(i64::MAX)
}

/// What a [`Planner`] is asked to plan. Build one with [`PlanRequest::still`] or
/// [`PlanRequest::motion`], and [`PlanRequest::with_media`] to plan a proxy preview.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PlanRequest<'a> {
    pub mode: PlanMode,
    /// The composite colour policy of the FFmpeg in use (`composite_color_policy()`): an
    /// input, so a test pins any of the three without an FFmpeg.
    pub color: CompositeColorPolicy,
    /// Which file each asset is decoded from ([`OriginalMedia`] unless asked otherwise).
    pub media: &'a dyn MediaResolver,
}

impl PlanRequest<'static> {
    /// What `build_still_args` draws ([`PlanMode::Still`]).
    pub fn still(color: CompositeColorPolicy) -> Self {
        Self {
            mode: PlanMode::Still,
            color,
            media: &OriginalMedia,
        }
    }

    /// What the export graph draws at an output frame ([`PlanMode::Motion`]).
    pub fn motion(color: CompositeColorPolicy) -> Self {
        Self {
            mode: PlanMode::Motion,
            color,
            media: &OriginalMedia,
        }
    }
}

impl PlanRequest<'_> {
    /// The same request over another [`MediaResolver`].
    pub fn with_media(self, media: &dyn MediaResolver) -> PlanRequest<'_> {
        PlanRequest {
            mode: self.mode,
            color: self.color,
            media,
        }
    }
}

/// What a plan needs of an asset, taken once.
#[derive(Debug, Clone)]
struct AssetFacts {
    name: String,
    path: String,
    proxy: bool,
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
    pub fn new(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, req: PlanRequest<'_>) -> Result<Self> {
        let rendered = timeline.for_render();
        // The canvas is the frame the cut is made for: the originals' streams decide it.
        let geom = render_geometry(&rendered, assets, opts);
        // What decoding each asset yields: the original, or a proxy and *its* picture. The
        // clips' transitions, HDR flag and padded head follow the file that is opened.
        // Only what the cut shows is resolved: a resolver may read the disk, and a library holds
        // assets the timeline never uses.
        let shown: std::collections::HashSet<Uuid> = rendered
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| t.clips.iter().map(|c| c.asset_id))
            .collect();
        let media: Vec<SourceMedia> = assets
            .iter()
            .map(|a| {
                if shown.contains(&a.id) {
                    req.media.resolve(a)
                } else {
                    SourceMedia::original(a)
                }
            })
            .collect();
        let decoded: Cow<[Asset]> = if media.iter().any(|m| m.proxy) {
            Cow::Owned(assets.iter().zip(&media).map(|(a, m)| m.decoded(a)).collect())
        } else {
            Cow::Borrowed(assets)
        };
        let assets: &[Asset] = &decoded;
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
            .zip(&media)
            .map(|(a, m)| {
                let stream = a
                    .streams
                    .iter()
                    .find(|s| s.kind == StreamKind::Video)
                    .and_then(PlanStream::of);
                let facts = AssetFacts {
                    name: a.name.clone(),
                    path: a.path.clone(),
                    proxy: m.proxy,
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
                    zooms: clip.zoom_animated(),
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

    /// Whether a compositor with `caps` draws output frame `k` exactly at `size`. A frame that
    /// cannot be planned (a clip whose asset is gone) is not drawn.
    fn draws(&self, k: u64, size: (u32, u32), caps: &GpuCaps) -> bool {
        self.at_frame(k).is_ok_and(|plan| plan.reasons(caps, size).is_empty())
    }

    /// The output frames of `[a, b)`: from the frame on screen at `a` to the last whose slot
    /// starts before `b`.
    fn frames_in(&self, a: f64, b: f64) -> std::ops::Range<u64> {
        let fps = self.canvas.fps;
        let first = fps.frame_containing(a);
        let end = (b.max(0.0) * f64::from(fps.num) / f64::from(fps.den) - 1e-6).ceil().max(0.0) as u64;
        first..end.max(first)
    }

    /// Whether a compositor with `caps` draws every frame of `[a, b)` at `size`, frame by frame,
    /// run-length encoded ([`SpanPlan`]): where a playback loop hands a stretch of the cut to
    /// the compositor and where to FFmpeg's stream. Every frame of the grid is evaluated, not
    /// breakpoints: support flips *inside* a keyframed span (opacity through 1.0, a scale
    /// through 40:1).
    pub fn span(&self, a: f64, b: f64, size: (u32, u32), caps: &GpuCaps) -> SpanPlan {
        let frames = self.frames_in(a, b);
        let mut runs: Vec<SpanRun> = Vec::new();
        for k in frames.clone() {
            let supported = self.draws(k, size, caps);
            match runs.last_mut() {
                Some(run) if run.supported == supported => run.frames.end = k + 1,
                _ => runs.push(SpanRun {
                    frames: k..k + 1,
                    supported,
                }),
            }
        }
        SpanPlan {
            fps: self.canvas.fps,
            frame: (self.canvas.width, self.canvas.height),
            first: frames.start,
            runs,
            windows: self.open_windows(),
        }
    }

    /// The time of the first frame, from the one on screen at `a` to the one `limit` seconds
    /// later, that a compositor with `caps` does not draw at `size` — evaluated **lazily**,
    /// stopping at the first one, so asking again every few frames of a playback costs the
    /// frames up to the answer and not the whole stretch. `None` when all of them are drawn.
    pub fn first_unsupported(&self, a: f64, limit: f64, size: (u32, u32), caps: &GpuCaps) -> Option<f64> {
        let fps = self.canvas.fps;
        let first = fps.frame_containing(a);
        // Both ends are in: the frame on screen at `a`, and the one on screen `limit` later.
        (first..=fps.frame_containing(a + limit.max(0.0)).max(first))
            .find(|&k| !self.draws(k, size, caps))
            .map(|k| fps.exact_time(k))
    }

    /// The windows `(lo, hi)` of the timeline in which a stream that **starts inside** them
    /// would not be the cut. `Timeline::slice` cuts the front off a clip it starts in the middle
    /// of, and that changes a transition from either side of it:
    ///
    /// * the clip that is cut loses its fade-in and its `transition_in`, so a stream started inside
    ///   a fade-in, a dissolve, a dip's second half or a slide has none of it;
    /// * the clip *before* the transition is shortened (or, from its end on, dropped), and the
    ///   transition is clamped to what is left of it: a dissolve shortens, a dip's fade-out
    ///   restarts from the cut, and with the outgoing clip gone the incoming one fades up from
    ///   black instead of crossing it.
    ///
    /// So a clip that fades in is in the way from its start to the end of the fade, and one that
    /// transitions out is in the way from `lead` before its end (the transition's length on its
    /// side) to its end — closed at the end, since a stream starting *on* the cut has lost the
    /// outgoing clip altogether.
    fn open_windows(&self) -> Vec<(f64, f64)> {
        let mut windows = Vec::new();
        for clip in &self.clips {
            let (start, end) = (clip.window.0, clip.window.1 - clip.fx.tail);
            windows.extend(
                clip.fades
                    .iter()
                    .filter(|f| f.edge == FadeEdge::In)
                    .map(|f| (f.st, f.st + f.d)),
            );
            windows.extend(clip.fx.move_in.map(|(_, _, secs)| (start, start + secs)));
            let lead = clip.fx.tail.max(clip.fx.black_out).max(clip.fx.white_out);
            if lead > 0.0 {
                windows.push((end - lead, end + 1e-6));
            }
        }
        windows.retain(|w| w.1 > w.0);
        windows
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
                let source_time = clip_source_time(clip, asset.duration, slot);
                // A still plan asks for the frame `-ss` lands on (a still image has the one); the
                // export asks the `fps` filter, which needs the whole window and the rate — of a
                // still image too, whose `-loop` input is a run of frames like any other.
                let pick = if motion {
                    Pick::Fps(FpsPick {
                        speed: clip.speed_mag(),
                        reverse: clip.is_reversed(),
                        window: planned.source_window,
                        start: clip.timeline_start,
                        frame: u64::try_from(frame).unwrap_or(0),
                        fps: self.canvas.pick_fps,
                        drop_first: !asset.is_image && planned.fx.head_pad && clip_seek(planned.source_window.0) == 0.0,
                        image: asset.is_image.then_some(self.canvas.fps),
                    })
                } else if asset.is_image {
                    Pick::AtOrAfter(0.0)
                } else {
                    Pick::AtOrAfter(source_time)
                };
                layers.push(PlanLayer {
                    clip_id: clip.id,
                    asset_id: clip.asset_id,
                    track: planned.track,
                    path: asset.path.clone(),
                    source: PlanSource { proxy: asset.proxy },
                    pick,
                    is_image: asset.is_image,
                    source_time,
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

/// A run of consecutive output frames a compositor draws (or does not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanRun {
    pub frames: std::ops::Range<u64>,
    pub supported: bool,
}

/// Where a stream has to start so that it carries on from the compositor: see
/// [`SpanPlan::handover`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Handover {
    /// The time to start the stream at: no fade-in or transition is open here, so the
    /// stream's own `Timeline::slice` is the cut.
    pub stream_start: f64,
    /// The first frame to show from it, the time the compositor stopped at; the frames the
    /// stream produces before it are dropped.
    pub first_shown: f64,
    /// The delivery frame of the **whole** cut. A stream plays a slice, and in a project with no
    /// delivery frame set the frame derives from the footage on the timeline: a slice that drops
    /// the clip that defined it (the first video clip) would be cut for another frame. The
    /// caller pins it — `ExportOptions::resolution` for the stream — so the stream draws the
    /// frame the compositor did.
    pub frame: (u32, u32),
}

/// What a compositor draws over a stretch of the cut ([`Planner::span`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SpanPlan {
    fps: Rational,
    frame: (u32, u32),
    first: u64,
    runs: Vec<SpanRun>,
    windows: Vec<(f64, f64)>,
}

impl SpanPlan {
    /// The planned frames as runs, in order (the first frame of the first run is the frame
    /// on screen at the span's start).
    pub fn runs(&self) -> &[SpanRun] {
        &self.runs
    }

    /// The time of the first frame at or after `a` and within `limit` seconds of it that is
    /// not drawn, among the frames this span planned.
    pub fn first_unsupported(&self, a: f64, limit: f64) -> Option<f64> {
        let from = self.fps.frame_containing(a).max(self.first);
        let to = self.fps.frame_containing(a + limit.max(0.0));
        self.runs
            .iter()
            .filter(|r| !r.supported && r.frames.end > from && r.frames.start <= to)
            .map(|r| r.frames.start.max(from))
            .next()
            .map(|k| self.fps.exact_time(k))
    }

    /// Where a stream that takes over from the compositor at `a` has to start.
    ///
    /// `stream_preview` plays `Timeline::slice(start, end)`, which cuts the front off the clips it
    /// starts in the middle of: a fade-in is zeroed and a transition dropped or clamped, together
    /// with the tail the outgoing clip would play under it (see `open_windows`). A stream started
    /// inside a fade-in, a transition on either side of the cut or a slide would therefore cut
    /// where the cut fades. So it starts at the latest time at or before `a` that no such window
    /// is open at, and the caller drops the frames before `first_shown = a`.
    pub fn handover(&self, a: f64) -> Handover {
        let mut start = a;
        // A window is open strictly inside it: starting where a clip begins keeps its fade-in.
        while let Some(lo) = self
            .windows
            .iter()
            .filter(|w| w.0 < start && start < w.1)
            .map(|w| w.0)
            .reduce(f64::min)
        {
            start = lo;
        }
        Handover {
            stream_start: start,
            first_shown: a,
            frame: self.frame,
        }
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
    use crate::render_plan::RenderPlan;

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
        Planner::new(
            tl,
            assets,
            opts,
            PlanRequest {
                mode,
                ..PlanRequest::still(FIXED)
            },
        )
        .unwrap()
    }

    fn keyed(time: f64, scale: f64, pos_x: f64) -> Keyframe {
        Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
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
                PlanRequest {
                    mode,
                    ..PlanRequest::still(FIXED)
                },
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
        // A zoom that moves is the last stage of the export's chain (`keyed_zoom.rs`), which
        // the compositor does not order that way yet.
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
        // A still runs a moving zoom last too, so it is drawn when nothing is in front of the
        // zoom (the order is then the one the compositor has) and refused when a grade, a
        // rotation or a fade of opacity is: those act on the picture before it is zoomed.
        let still = |clip: crate::model::Clip, caps: &GpuCaps| {
            let plan = planner(&single(vec![clip]), &assets, PlanMode::Still, &opts(30.0))
                .at_frame(15)
                .unwrap();
            plan.reasons(caps, plan.size(u32::MAX))
        };
        let zoom = zoomed.tracks[0].clips[0].clone();
        let behind = |u: &Unsupported| matches!(u, Unsupported::ZoomBehind(_));
        assert!(!has(&still(zoom.clone(), &GpuCaps::A0), behind));
        let mut graded = zoom.clone();
        graded.color.contrast = 1.3;
        let mut turning = zoom.clone();
        turning.keyframes = vec![
            Keyframe {
                rotation: 10.0,
                ..keyed(0.0, 1.0, 0.0)
            },
            Keyframe {
                rotation: 10.0,
                ..keyed(2.0, 0.5, 0.0)
            },
        ];
        let mut fading = zoom;
        fading.keyframes = vec![
            Keyframe {
                opacity: 0.6,
                ..keyed(0.0, 1.0, 0.0)
            },
            Keyframe {
                opacity: 0.6,
                ..keyed(2.0, 0.5, 0.0)
            },
        ];
        for clip in [graded, turning, fading] {
            assert!(has(&still(clip.clone(), &GpuCaps::A0), behind));
            let ordered = GpuCaps {
                keyed_zoom: true,
                ..GpuCaps::A0
            };
            assert!(!has(&still(clip, &ordered), behind));
        }
        // A rotation or a grade with no zoom to move has nothing to be ordered against.
        let mut steady = tl.tracks[0].clips[0].clone();
        steady.color.contrast = 1.3;
        assert!(!has(&still(steady, &GpuCaps::A0), behind));
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

    // ---- picks, the media a plan describes, spans and the hand-over ----------------------

    #[test]
    fn a_still_asks_for_the_frame_ss_lands_on_and_an_export_frame_carries_the_fps_pick() {
        let (a, img) = (asset(), crate::engine::test_support::img_asset(Uuid::new_v4()));
        let mut back = make_clip(a.id, 2.0, 6.0, 1.0);
        back.speed = -2.0;
        let tl = timeline_of(vec![
            video_track(vec![back]),
            video_track(vec![make_clip(img.id, 0.0, 3.0, 0.0)]),
        ]);
        let assets = [a, img];
        let still = planner(&tl, &assets, PlanMode::Still, &opts(30.0)).at(1.5).unwrap();
        // The still decodes where `-ss` goes; a still image has its one frame.
        assert_eq!(still.layers[0].pick, Pick::AtOrAfter(still.layers[0].source_time));
        assert_eq!(still.layers[1].pick, Pick::AtOrAfter(0.0));
        let export = planner(&tl, &assets, PlanMode::Motion, &opts(30.0)).at_frame(45).unwrap();
        let want = FpsPick {
            speed: 2.0,
            reverse: true,
            window: (2.0, 6.0),
            start: 1.0,
            frame: 45,
            fps: Rational::new(30, 1).unwrap(),
            drop_first: false,
            image: None,
        };
        assert_eq!(export.layers[0].pick, Pick::Fps(want));
        // A still image is a run of frames made up from the delivery rate (`-loop 1 -framerate`).
        assert!(matches!(export.layers[1].pick, Pick::Fps(p) if p.image == Some(Rational::new(30, 1).unwrap()) && !p.drop_first));
        // A transition's tail is in the pick's window, as it is in the chain's trim.
        let (out, mut inc) = (
            make_clip(assets[0].id, 0.0, 2.0, 0.0),
            make_clip(assets[0].id, 10.0, 14.0, 2.0),
        );
        inc.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let tl = single(vec![out, inc]);
        let plan = planner(&tl, &assets, PlanMode::Motion, &opts(30.0)).at_frame(75).unwrap();
        assert!(matches!(plan.layers[0].pick, Pick::Fps(p) if p.window == (0.0, 3.0)));
    }

    #[test]
    fn a_padded_proxy_read_from_the_start_drops_the_clone_and_a_seek_skips_it() {
        let mut a = asset();
        a.path = "/home/u/.cache/kerf/proxies/0123456789abcdef.lead.mp4".into();
        let tl = timeline_of(vec![
            video_track(vec![make_clip(a.id, 0.0, 2.0, 0.0)]),
            video_track(vec![make_clip(a.id, 1.0, 3.0, 0.0)]),
            video_track(vec![make_clip(a.id, 0.0005, 2.0, 0.0)]),
        ]);
        let plan = planner(&tl, std::slice::from_ref(&a), PlanMode::Motion, &opts(30.0))
            .at_frame(3)
            .unwrap();
        let drops: Vec<_> = plan
            .layers
            .iter()
            .map(|l| matches!(l.pick, Pick::Fps(p) if p.drop_first))
            .collect();
        // From the very head (and within a millisecond of it, which the chain does not seek)
        // the clone goes; from a second in, `-ss` has skipped it.
        assert_eq!(drops, [true, false, true]);
        let plain = asset();
        let tl = single(vec![make_clip(plain.id, 0.0, 2.0, 0.0)]);
        let plan = planner(&tl, &[plain], PlanMode::Motion, &opts(30.0)).at_frame(3).unwrap();
        assert!(matches!(plan.layers[0].pick, Pick::Fps(p) if !p.drop_first));
    }

    /// Every asset decodes from a 1280x720 4:2:0 proxy of its own, as `ProxyMedia` finds one.
    #[derive(Debug)]
    struct Proxied;

    impl MediaResolver for Proxied {
        fn resolve(&self, asset: &Asset) -> SourceMedia {
            let mut video = asset.streams[0].clone();
            (video.width, video.height) = (Some(1280), Some(720));
            (video.pix_fmt, video.color_transfer, video.color_primaries) = (Some("yuv420p".into()), None, None);
            SourceMedia {
                path: "/home/u/.cache/kerf/proxies/0123456789abcdef.lead.mp4".into(),
                proxy: true,
                video: Some(video),
            }
        }
    }

    #[test]
    fn a_plan_over_a_proxy_describes_the_proxy_and_keeps_the_originals_canvas() {
        let mut a = asset();
        a.streams[0].pix_fmt = Some("yuv422p10le".into());
        a.streams[0].color_transfer = Some("arib-std-b67".into());
        let tl = single(vec![make_clip(a.id, 0.0, 4.0, 0.0)]);
        let assets = std::slice::from_ref(&a);
        let layer_of = |request: PlanRequest| Planner::new(&tl, assets, &opts(30.0), request).unwrap().at_frame(3).unwrap();
        let original = layer_of(PlanRequest::motion(FIXED));
        let (l, proxied) = (&original.layers[0], layer_of(PlanRequest::motion(FIXED).with_media(&Proxied)));
        // The file itself: its size and format, and an HDR flag the chain tone-maps.
        assert!(!l.source.proxy && l.path == a.path);
        assert_eq!((l.stream.width, l.stream.pix_fmt.as_deref()), (1920, Some("yuv422p10le")));
        assert!(l.hdr.is_some());
        // The proxy: the 4:2:0 picture it is (so nothing is refused for the format of the
        // original), already SDR, its path, and the pad's clone to drop.
        let p = &proxied.layers[0];
        assert!(p.source.proxy && p.path.ends_with("0123456789abcdef.lead.mp4"));
        assert_eq!(
            (p.stream.width, p.stream.height, p.stream.pix_fmt.as_deref()),
            (1280, 720, Some("yuv420p"))
        );
        assert!(p.hdr.is_none() && matches!(p.pick, Pick::Fps(f) if f.drop_first));
        // The delivery frame is the cut's, whatever is decoded; the geometry starts from the picture
        // that arrives.
        assert_eq!(
            (proxied.canvas.width, proxied.canvas.height),
            (original.canvas.width, original.canvas.height)
        );
        let size = (proxied.canvas.width, proxied.canvas.height);
        let stage = proxied.layer_geometry(p, size).unwrap().stages[0].src;
        assert_eq!((stage.w, stage.h), (1280, 720));
        // The default request is the original, and says so.
        assert_eq!(PlanRequest::still(FIXED).media.resolve(&a), SourceMedia::original(&a));
    }

    /// Counts how often it is asked.
    #[derive(Debug, Default)]
    struct Counting(std::sync::atomic::AtomicUsize);

    impl MediaResolver for Counting {
        fn resolve(&self, asset: &Asset) -> SourceMedia {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            SourceMedia::original(asset)
        }
    }

    #[test]
    fn only_the_assets_a_cut_shows_are_resolved() {
        // A resolver may read the disk, and a library holds assets the timeline never uses.
        let (shown, unused, audio) = (asset(), asset(), crate::engine::test_support::av_asset(Uuid::new_v4(), 5.0));
        let tl = timeline_of(vec![
            video_track(vec![make_clip(shown.id, 0.0, 2.0, 0.0)]),
            crate::engine::test_support::audio_track(vec![make_clip(audio.id, 0.0, 2.0, 0.0)]),
        ]);
        let counting = Counting::default();
        let request = PlanRequest::motion(FIXED).with_media(&counting);
        Planner::new(&tl, &[shown, unused, audio], &opts(30.0), request).unwrap();
        assert_eq!(counting.0.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn a_rate_the_canvas_and_the_clips_parse_differently_is_refused_in_an_export_frame() {
        let a = asset();
        let tl = single(vec![make_clip(a.id, 0.0, 4.0, 0.0)]);
        let refused = |fps: f64, mode| {
            let plan = planner(&tl, std::slice::from_ref(&a), mode, &opts(fps)).at_frame(5).unwrap();
            let caps = GpuCaps {
                motion: true,
                ..GpuCaps::A0
            };
            plan.reasons(&caps, plan.size(u32::MAX))
                .iter()
                .any(|r| matches!(r, Unsupported::PickRate { .. }))
        };
        // 29.970029 is 92997/3103 on the canvas and 29970029/1000000 on a clip: two frame grids.
        assert!(refused(29.970029, PlanMode::Motion));
        assert!(!refused(29.97, PlanMode::Motion) && !refused(30.0, PlanMode::Motion));
        assert!(!refused(29.970029, PlanMode::Still));
    }

    /// A 30 fps export of a cut with a fade-in over the first second and a dissolve into a second
    /// clip at 4 s (1 s long), the second also fading in.
    fn fading_cut() -> (Timeline, Vec<Asset>) {
        let a = asset();
        let mut first = make_clip(a.id, 0.0, 4.0, 0.0);
        first.fade_in = 1.0;
        let mut second = make_clip(a.id, 10.0, 16.0, 4.0);
        second.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        (single(vec![first, second]), vec![a])
    }

    #[test]
    fn a_span_says_where_the_compositor_draws_and_the_first_frame_it_does_not() {
        let (tl, assets) = fading_cut();
        let p = planner(&tl, &assets, PlanMode::Motion, &opts(30.0));
        let size = (1920, 1080);
        let motion = GpuCaps {
            motion: true,
            ..GpuCaps::A0
        };
        // Frames 0..30 are inside the fade-in, 120..151 inside the dissolve (the outgoing clip
        // on its tail, the incoming on its ramp): runs of refusals either side of what is drawn.
        let span = p.span(0.0, 6.0, size, &motion);
        let runs: Vec<_> = span.runs().iter().map(|r| (r.frames.clone(), r.supported)).collect();
        // (Frame 150 is the cut's closing frame: the outgoing clip is still drawn there, on its tail.)
        assert_eq!(runs, [(0..30, false), (30..120, true), (120..151, false), (151..180, true)]);
        // Asking for the next refusal is asking for the first frame of a run.
        let at = |k: u64| Rational::new(30, 1).unwrap().exact_time(k);
        assert_eq!(span.first_unsupported(0.0, 5.0), Some(0.0));
        assert_eq!(span.first_unsupported(1.5, 5.0), Some(at(120)));
        assert_eq!(span.first_unsupported(1.5, 2.0), None);
        assert_eq!(span.first_unsupported(4.5, 5.0), Some(4.5));
        assert_eq!(span.first_unsupported(5.0, 5.0), Some(5.0));
        assert_eq!(span.first_unsupported(5.1, 5.0), None);
        // The lazy scan stops at the first one and agrees with the span's answer everywhere.
        for a in [0.0, 0.5, 1.0, 1.5, 3.9, 4.2, 5.0, 5.9] {
            for limit in [0.0, 0.5, 2.0, 5.0] {
                let planned = span.first_unsupported(a, limit);
                let lazy = p.first_unsupported(a, limit, size, &motion);
                // The span planned frames up to 6 s: a scan that runs past it has more to say.
                assert!(
                    a + limit > 6.0 || planned == lazy,
                    "from {a}, {limit} s: {planned:?} vs {lazy:?}"
                );
            }
        }
        // The compositor that draws fades and transitions draws all of it.
        let all = GpuCaps {
            fades: true,
            transitions: true,
            ..motion
        };
        assert!(p.span(0.0, 6.0, size, &all).runs().iter().all(|r| r.supported));
        assert_eq!(p.first_unsupported(0.0, 6.0, size, &all), None);
        // An unplannable frame is not drawn: a clip whose asset is not there.
        let broken = planner(&tl, &[], PlanMode::Motion, &opts(30.0));
        assert_eq!(broken.first_unsupported(0.0, 1.0, size, &all), Some(0.0));
    }

    #[test]
    fn a_stream_that_takes_over_starts_where_no_fade_or_transition_is_open() {
        let (tl, assets) = fading_cut();
        let p = planner(&tl, &assets, PlanMode::Motion, &opts(30.0));
        let span = p.span(0.0, 6.0, (1920, 1080), &GpuCaps::A0);
        let start = |a: f64| {
            let h = span.handover(a);
            assert_eq!(h.first_shown, a);
            h.stream_start
        };
        // Inside the fade-in it starts where the clip does. A dissolve is in the way from the last
        // second of the outgoing clip to the end of the ramp: the slice would shorten it, or drop
        // the outgoing clip altogether. On the edges, or in the clear, it starts where asked.
        assert_eq!((start(0.5), start(1.0), start(2.0), start(3.0)), (0.0, 1.0, 2.0, 3.0));
        assert_eq!((start(3.5), start(4.0), start(4.5)), (3.0, 3.0, 3.0));
        assert_eq!((start(5.0), start(5.5)), (5.0, 5.5));
        // Windows chain: a second track's fade-in that starts before the dissolve pulls it back again.
        let a = assets[0].clone();
        let mut over = make_clip(a.id, 0.0, 3.0, 2.5);
        over.fade_in = 1.0;
        let mut tl = tl;
        tl.tracks.push(video_track(vec![over]));
        let span = planner(&tl, &assets, PlanMode::Motion, &opts(30.0)).span(0.0, 6.0, (1920, 1080), &GpuCaps::A0);
        assert_eq!(span.handover(4.2).stream_start, 2.5);
    }

    #[test]
    fn a_hand_over_carries_the_frame_of_the_whole_cut_for_a_slice_that_drops_the_clip_defining_it() {
        // No delivery frame is set, so the frame is the footage's: the first clip's. A stream
        // started past it plays a slice without that clip, and would be cut for the other's.
        let mut big = asset();
        big.streams[0] = video_stream(1920, 1080, 30.0);
        let mut small = asset();
        small.streams[0] = video_stream(1280, 720, 30.0);
        let tl = single(vec![make_clip(big.id, 0.0, 2.0, 0.0), make_clip(small.id, 0.0, 2.0, 2.0)]);
        let assets = [big, small];
        let full = planner(&tl, &assets, PlanMode::Motion, &opts(30.0));
        let handover = full.span(0.0, 4.0, (1920, 1080), &GpuCaps::A0).handover(2.5);
        assert_eq!(handover.frame, (1920, 1080));
        let frame_of = |tl: &Timeline, pin: Option<(u32, u32)>| {
            let opts = ExportOptions {
                resolution: pin,
                ..opts(30.0)
            };
            let p = Planner::new(tl, &assets, &opts, PlanRequest::motion(FIXED)).unwrap();
            (p.canvas().width, p.canvas().height)
        };
        let slice = tl.slice(handover.stream_start, 4.0);
        assert_eq!(frame_of(&slice, None), (1280, 720));
        assert_eq!(frame_of(&slice, Some(handover.frame)), (1920, 1080));
    }

    /// What a layer shows of its fades, travel and tail: the plan's per-frame state. A layer on
    /// the frame its window closes on is only a *candidate* (the pick decides, and an ulp of
    /// floating point in a shifted slice can add or drop it), so it is left out.
    fn fx_state(plan: &RenderPlan) -> Vec<(Vec<f64>, bool, (f64, f64))> {
        plan.layers
            .iter()
            .filter(|l| plan.time < l.timing.window.1 - 1e-6)
            .map(|l| {
                let fades = [FadeTint::Black, FadeTint::White, FadeTint::Alpha]
                    .map(|t| plan.strength(l, t))
                    .to_vec();
                (fades, l.fx.tail, l.fx.motion)
            })
            .collect()
    }

    /// The claim behind [`SpanPlan::handover`]: a slice started at `stream_start` is the cut from
    /// `first_shown` on, and one started inside a transition is not.
    #[test]
    fn a_slice_started_at_the_handover_is_the_cut_and_one_started_inside_a_dissolve_is_not() {
        let (tl, assets) = fading_cut();
        let fps = 30.0;
        let full = planner(&tl, &assets, PlanMode::Motion, &opts(fps));
        let span = full.span(0.0, 6.0, (1920, 1080), &GpuCaps::A0);
        let rate = full.canvas().fps;
        let mut compared = 0;
        for a in [0.5, 1.0, 3.5, 4.0, 4.5, 4.9, 5.2] {
            let stream_start = span.handover(a).stream_start;
            let sliced = planner(&tl.slice(stream_start, 6.0), &assets, PlanMode::Motion, &opts(fps));
            let offset = (stream_start * fps).round() as u64;
            for k in (a * fps).round() as u64..170 {
                let (whole, part) = (full.at_frame(k).unwrap(), sliced.at_frame(k - offset).unwrap());
                assert_eq!(
                    fx_state(&whole),
                    fx_state(&part),
                    "from {a} (stream {stream_start}), frame {k}"
                );
                compared += whole.layers.len();
            }
        }
        assert!(compared > 500);
        // Started inside the dissolve with no hand-over, the stream has no ramp and no tail.
        let naive = planner(&tl.slice(4.5, 6.0), &assets, PlanMode::Motion, &opts(fps));
        let (whole, part) = (full.at_frame(rate.frame_at(4.5)).unwrap(), naive.at_frame(0).unwrap());
        assert_eq!(whole.layers.len(), 2);
        assert_eq!(part.layers.len(), 1);
        assert_eq!(part.strength(&part.layers[0], FadeTint::Alpha), 1.0);
        assert!(whole.strength(&whole.layers[1], FadeTint::Alpha) < 1.0);
    }

    /// The same for the other families — a dip to black, a slide, a push, a dip to white — from
    /// every eighth of a second across a 15 s cut: the slice at the hand-over is the cut, and a
    /// slice started where the stream was asked for (inside a transition, or just before one,
    /// where the outgoing clip is cut short) is not, for some of them. (32 fps and eighths of a
    /// second: every time is exact in binary, so no ulp of a shifted clip start moves a frame.)
    #[test]
    fn a_slice_started_at_the_handover_keeps_dips_slides_and_pushes_too() {
        let a = asset();
        let clips: Vec<_> = [
            None,
            Some(TransitionKind::DipToBlack),
            Some(TransitionKind::SlideLeft),
            Some(TransitionKind::PushLeft),
            Some(TransitionKind::DipToWhite),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, kind)| {
            let mut c = make_clip(a.id, 10.0 * i as f64, 10.0 * i as f64 + 3.0, 3.0 * i as f64);
            c.transition_in = kind.map(|kind| Transition {
                kind,
                duration: if kind == TransitionKind::DipToWhite { 0.625 } else { 1.0 },
            });
            c
        })
        .collect();
        let tl = single(clips);
        let assets = [a];
        let fps = 32.0;
        let full = planner(&tl, &assets, PlanMode::Motion, &opts(fps));
        let span = full.span(0.0, 15.0, (1920, 1080), &GpuCaps::A0);
        let (mut compared, mut naive_wrong, mut moved) = (0, 0, 0);
        for i in 1..=112u64 {
            let at = i as f64 / 8.0;
            let first = i * 4;
            let stream_start = span.handover(at).stream_start;
            let from = |start: f64| {
                let sliced = planner(&tl.slice(start, 15.0), &assets, PlanMode::Motion, &opts(fps));
                let offset = (start * fps).round() as u64;
                move |k: u64| sliced.at_frame(k - offset).unwrap()
            };
            let (handed, naive) = (from(stream_start), from(at));
            for k in first..first + 64 {
                let whole = fx_state(&full.at_frame(k).unwrap());
                assert_eq!(whole, fx_state(&handed(k)), "from {at} (stream {stream_start}), frame {k}");
                naive_wrong += usize::from(whole != fx_state(&naive(k)));
                moved += usize::from(whole.iter().any(|l| l.2 != (0.0, 0.0)));
                compared += whole.len();
            }
        }
        assert!(
            compared > 4000 && moved > 100,
            "{compared} layers, {moved} frames with travel"
        );
        assert!(naive_wrong > 500, "a slice started in the way must differ ({naive_wrong})");
    }

    /// `KERF_BENCH=1 cargo test -p kerf-core --no-default-features --release -- --ignored --nocapture
    /// planner::tests::bench`: what planning a 500-clip cut costs. Print-only (a wall-clock limit
    /// on a shared machine measures the machine); `a_500_clip_cut_...` guards the shape instead.
    #[test]
    #[ignore = "benchmark: set KERF_BENCH=1"]
    #[allow(clippy::print_stderr)]
    fn bench_planning_a_500_clip_cut() {
        if std::env::var_os("KERF_BENCH").is_none() {
            eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
            return;
        }
        let (tl, assets) = five_hundred_clips();
        let time = |what: &str, n: usize, f: &mut dyn FnMut()| {
            let t = std::time::Instant::now();
            f();
            let dt = t.elapsed();
            eprintln!("{what}: {dt:?} ({:?} each over {n})", dt / n.max(1) as u32);
        };
        let mut p = None;
        time("Planner::new, 500 clips on 5 tracks", 1, &mut || {
            p = Some(planner(&tl, &assets, PlanMode::Motion, &opts(30.0)));
        });
        let p = p.unwrap();
        let caps = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            ..GpuCaps::A0
        };
        time("at_frame (5 layers on screen)", 3000, &mut || {
            for k in 0..3000u64 {
                std::hint::black_box(p.at_frame((k * 7) % 7000).unwrap());
            }
        });
        time("span, 10 s (300 frames)", 300, &mut || {
            std::hint::black_box(p.span(60.0, 70.0, (1920, 1080), &caps));
        });
        time("first_unsupported over 5 s, nothing unsupported", 150, &mut || {
            std::hint::black_box(p.first_unsupported(60.0, 5.0, (1920, 1080), &caps));
        });
    }

    /// 5 tracks of 100 clips of 2 to 3 s, every fifth dissolving in.
    fn five_hundred_clips() -> (Timeline, Vec<Asset>) {
        let a = asset();
        let mut rng = 0xC0FF_EE00_1234_5678u64;
        let tracks = (0..5)
            .map(|_| {
                let mut t = 0.0;
                video_track(
                    (0..100)
                        .map(|i| {
                            let len = 2.0 + (xorshift(&mut rng) % 100) as f64 / 100.0;
                            let mut c = make_clip(a.id, 10.0, 10.0 + len, t);
                            if i % 5 == 4 {
                                c.transition_in = Some(Transition {
                                    kind: TransitionKind::Crossfade,
                                    duration: 0.5,
                                });
                            }
                            t += len;
                            c
                        })
                        .collect(),
                )
            })
            .collect();
        (timeline_of(tracks), vec![a])
    }

    #[test]
    fn a_500_clip_cut_plans_and_spans_in_time_proportional_to_what_is_on_screen() {
        // Not a benchmark: a guard on the shape. Planning a frame walks the clips on screen, not
        // the 500; the limits are generous (a busy machine runs the suite in parallel) and sized
        // to catch an algorithm that scans every clip per frame *and* per plan.
        let (tl, assets) = five_hundred_clips();
        let p = planner(&tl, &assets, PlanMode::Motion, &opts(30.0));
        let started = std::time::Instant::now();
        let mut layers = 0;
        for k in 0..1500u64 {
            layers += p.at_frame(k * 5).unwrap().layers.len();
        }
        assert!(layers > 5000, "{layers}");
        let caps = GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            ..GpuCaps::A0
        };
        let span = p.span(100.0, 104.0, (1920, 1080), &caps);
        assert_eq!(span.runs().iter().map(|r| r.frames.end - r.frames.start).sum::<u64>(), 120);
        assert_eq!(p.first_unsupported(100.0, 4.0, (1920, 1080), &caps), None);
        assert!(started.elapsed().as_secs() < 8, "{:?}", started.elapsed());
    }
}
