//! The cut a play session draws: what a compositor that plays forward from the playhead plans,
//! scans ahead of and hands back to `stream_preview`.
//!
//! `stream_preview` plays a [`Timeline::slice`] from the playhead through the export's own graph, so
//! what plays is the export of that slice. A compositor that takes over playback has to draw the
//! same thing, and [`PlayCut`] is that: a [`PlanMode::Motion`] [`Planner`] over the slice, with the
//! three things the stream gets from the graph made explicit.
//!
//! * **The start is a frame of the cut's grid with no transition window open.** The slice starts at
//!   the output frame whose slot holds the playhead — so slice frame `j` is frame `k0 + j` of the
//!   cut and a page that counts frames of the cut agrees with it — or, when a fade-in, a dissolve, a dip
//!   or a slide would be cut off by a slice there ([`SpanPlan::handover`]), at the latest grid frame
//!   before that window. The caller draws nothing before the time it was asked for; the clips are
//!   simply read through to it.
//! * **The frame and the rate are the whole cut's.** In a project with no delivery frame the
//!   canvas derives from the first clip with video, and the rate from the first clip with a rate: a slice
//!   that drops that clip would be cut for another frame and another rate (the stream pins both for
//!   the same reason, `Handover::frame`).
//! * **A refusal ahead is a place to hand back.** [`PlayCut::ahead`] scans the next few seconds for the
//!   first frame the compositor does not draw (or one it would not draw *in time*, see
//!   [`PlayRefusal`]) and says where a stream that carries on has to start, as [`SpanPlan::handover`]
//!   defines it for this slice.
//! * **A clean run ahead is a place to take back.** [`PlayCut::resume`] finds the first frame from
//!   which the compositor draws a whole horizon again, where a play the stream took over (or began
//!   with) can be handed back to the compositor.
//!
//! Pure: it plans, it decodes nothing, so it is tested without an FFmpeg.

use std::fmt;

use uuid::Uuid;

use crate::clip_timing::Rational;
use crate::engine::{render_geometry, ExportOptions, MAX_STREAM_FPS, MIN_STREAM_FPS};
use crate::error::{Error, Result};
use crate::media::MediaResolver;
use crate::model::{Asset, StreamKind, TextOverlay, Timeline};
use crate::plan_caps::{GpuCaps, Unsupported};
use crate::planner::{PlanRequest, Planner, SpanPlan};
use crate::render_plan::{CompositeColorPolicy, PlanCanvas, RenderPlan};

/// A nanosecond of bias on the slice's start (see [`PlayCut::new`]).
const SLICE_BIAS: f64 = 1e-9;

/// How many times the start is moved back to the grid before a transition window: the windows
/// chain, but a cut with sixteen of them overlapping is not a cut.
const SETTLE_STEPS: usize = 16;

/// Why a frame is not drawn by a compositor that is playing.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayRefusal {
    /// The plan: the compositor does not draw this frame exactly.
    Plan(Unsupported),
    /// A reversed clip: the cursor that plays it backwards has to read the clip's whole source window
    /// before it can show the first frame (and holds all of it), so it is not a real-time source.
    Reversed(String),
    /// A title whose position moves: `drawtext` rasterises a glyph at the position's quarter-pixel
    /// phase, so every frame of the move is a new rasterisation (one `ffmpeg`, tens of milliseconds)
    /// that a playing frame cannot wait for. Drawn exactly, but not in time.
    MovingText(String),
    /// The frame cannot be planned at all (a clip whose asset is gone).
    NoPlan(String),
}

impl PlayRefusal {
    /// A stable key to count refusals by (`Unsupported::kind` for the plan's).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Plan(u) => u.kind(),
            Self::Reversed(_) => "reversed",
            Self::MovingText(_) => "moving_text",
            Self::NoPlan(_) => "no_plan",
        }
    }
}

impl fmt::Display for PlayRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(u) => write!(f, "{u}"),
            Self::Reversed(name) => write!(f, "{name} plays backwards, which needs its whole source window read first"),
            Self::MovingText(text) => write!(
                f,
                "the title \"{text}\" moves, and each frame of its move is a rasterisation of its own"
            ),
            Self::NoPlan(why) => write!(f, "no plan for this frame: {why}"),
        }
    }
}

/// The first frame ahead that is not drawn, and where a stream that carries on from there starts.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayAhead {
    /// The time of that frame, timeline seconds.
    pub at: f64,
    /// The time to start a stream at so that it carries on from the compositor
    /// ([`SpanPlan::handover`], in the cut's time): at or before `at`, with no transition window open.
    pub handover: f64,
    pub why: PlayRefusal,
}

/// See the [module](self).
pub struct PlayCut {
    planner: Planner,
    fps: Rational,
    /// The rate as the graph prints it (what the slice's options pin).
    rate: f64,
    /// The timeline time of slice frame 0: a frame of the cut's grid.
    start: f64,
    frame: (u32, u32),
    /// The slice's windows, for [`SpanPlan::handover`] (a span of no frames).
    windows: SpanPlan,
    /// Titles whose position moves, in the slice.
    moving_text: Vec<Uuid>,
}

fn moves(overlay: &TextOverlay) -> bool {
    overlay
        .keyframes
        .windows(2)
        .any(|w| w[0].pos_x != w[1].pos_x || w[0].pos_y != w[1].pos_y)
}

impl PlayCut {
    /// The cut to play from `at` (timeline seconds): `timeline` as it renders, from the output frame
    /// that holds `at` — or from the grid frame before the transition `at` is inside of — to its end,
    /// planned as the export plans it, over what `media` decodes.
    ///
    /// The slice is cut a nanosecond before the frame: a clip whose start is a float's width under the
    /// frame's (a clip placed after another one sums its length) would otherwise be "cut" by that
    /// width, and a cut clip loses its fade-in and its transition.
    ///
    /// `Err` where nothing plays: at or past the end, no video from there, a rate outside what the
    /// stream plays (1 to 60 fps), or a plan that does not build.
    pub fn new(
        timeline: &Timeline,
        assets: &[Asset],
        at: f64,
        color: CompositeColorPolicy,
        media: &dyn MediaResolver,
    ) -> Result<Self> {
        let rendered = timeline.for_render();
        let end = rendered.duration();
        if !at.is_finite() || at >= end {
            return Err(Error::Engine("nothing to play past the playhead".to_string()));
        }
        let geom = render_geometry(&rendered, assets, &ExportOptions::default());
        if !(MIN_STREAM_FPS..=MAX_STREAM_FPS).contains(&geom.fps) {
            return Err(Error::Engine(format!(
                "the cut's {} fps is outside the {MIN_STREAM_FPS} to {MAX_STREAM_FPS} a playing preview draws",
                geom.fps
            )));
        }
        let request = PlanRequest::motion(color).with_media(media);
        let whole = Planner::new(timeline, assets, &ExportOptions::default(), request)?;
        let fps = whole.canvas().fps;
        let frame = (whole.canvas().width, whole.canvas().height);
        let windows = whole.span(0.0, 0.0, frame, &GpuCaps::A0);
        let grid = |t: f64| fps.exact_time(fps.frame_containing(t));
        let mut start = grid(at);
        for _ in 0..SETTLE_STEPS {
            let earlier = grid(windows.handover(start).stream_start);
            if earlier >= start - 1e-9 {
                break;
            }
            start = earlier;
        }

        let slice = rendered.slice((start - SLICE_BIAS).max(0.0), end);
        if !slice
            .tracks
            .iter()
            .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty())
        {
            return Err(Error::Engine("no video on the timeline from here".to_string()));
        }
        let moving_text = slice.overlays.iter().filter(|o| moves(o)).map(|o| o.id).collect();
        let opts = ExportOptions {
            fps: Some(geom.fps),
            resolution: Some(frame),
            ..ExportOptions::default()
        };
        let planner = Planner::new(&slice, assets, &opts, request)?;
        if (planner.canvas().width, planner.canvas().height) != frame || planner.canvas().fps != fps {
            return Err(Error::Engine(
                "the slice is cut for another frame or rate than the whole".to_string(),
            ));
        }
        let windows = planner.span(0.0, 0.0, frame, &GpuCaps::A0);
        Ok(Self {
            planner,
            fps,
            rate: geom.fps,
            start,
            frame,
            windows,
            moving_text,
        })
    }

    /// The timeline time of the first frame of the cut: a frame of the project's grid.
    pub fn start(&self) -> f64 {
        self.start
    }

    pub fn canvas(&self) -> &PlanCanvas {
        self.planner.canvas()
    }

    pub fn fps(&self) -> Rational {
        self.fps
    }

    /// The cut's rate in frames a second, as the graph prints it (`ExportOptions::fps`).
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The delivery frame of the whole cut.
    pub fn frame(&self) -> (u32, u32) {
        self.frame
    }

    /// The slice's frame on screen at timeline time `t` (`0` before the start).
    pub fn frame_at(&self, t: f64) -> u64 {
        self.fps.frame_containing(t - self.start)
    }

    /// The timeline time at which slice frame `k` starts.
    pub fn time_of(&self, k: u64) -> f64 {
        self.start + self.fps.exact_time(k)
    }

    /// The plan of slice frame `k`.
    pub fn at_frame(&self, k: u64) -> Result<RenderPlan> {
        self.planner.at_frame(k)
    }

    /// The plan of the frame on screen at timeline time `t`.
    pub fn plan(&self, t: f64) -> Result<RenderPlan> {
        self.at_frame(self.frame_at(t))
    }

    /// Why a compositor with `caps` does not draw slice frame `k` at `size` in time, or `None`.
    pub fn refusal(&self, k: u64, size: (u32, u32), caps: &GpuCaps) -> Option<PlayRefusal> {
        let plan = match self.planner.at_frame(k) {
            Ok(plan) => plan,
            Err(e) => return Some(PlayRefusal::NoPlan(e.to_string())),
        };
        if let Some(first) = plan.reasons(caps, size).into_iter().next() {
            return Some(PlayRefusal::Plan(first));
        }
        self.playing_refusal(&plan)
    }

    /// What a drawn plan still costs too much to play: see [`PlayRefusal`].
    pub fn playing_refusal(&self, plan: &RenderPlan) -> Option<PlayRefusal> {
        if let Some(layer) = plan.layers.iter().find(|l| l.timing.reversed && !l.is_image) {
            return Some(PlayRefusal::Reversed(layer.name.clone()));
        }
        // A title keyed to move, while it is on screen.
        plan.overlays
            .iter()
            .find(|t| self.moving_text.contains(&t.id))
            .map(|t| PlayRefusal::MovingText(t.text.clone()))
    }

    /// The first frame from the one on screen at `from` to the one `horizon` seconds later that is
    /// not drawn, and where a stream that carries on from it starts; `None` when all of them are.
    /// Evaluated lazily, in order, so a cut that is drawn throughout costs every frame of the span
    /// once and one that is not costs the frames up to the first.
    pub fn ahead(&self, from: f64, horizon: f64, size: (u32, u32), caps: &GpuCaps) -> Option<PlayAhead> {
        let first = self.frame_at(from);
        let last = self.frame_at(from + horizon.max(0.0)).max(first);
        (first..=last).find_map(|k| {
            let why = self.refusal(k, size, caps)?;
            Some(PlayAhead {
                at: self.time_of(k),
                handover: self.handover_in_slice(self.fps.exact_time(k)),
                why,
            })
        })
    }

    /// Where a compositor that gave the play to the stream can take it back: the time of the first
    /// frame, from the one on screen at `from` to the one `reach` seconds later, from which every
    /// frame for `horizon` seconds is drawn (what [`Self::ahead`] asks of a session begun there);
    /// `None` when no frame within `reach` starts such a run. Frames are planned in order, each once,
    /// and the scan stops at the first run long enough.
    pub fn resume(&self, from: f64, reach: f64, horizon: f64, size: (u32, u32), caps: &GpuCaps) -> Option<f64> {
        let last_start = self.frame_at(from + reach.max(0.0)).max(self.frame_at(from));
        let mut run: Option<u64> = None;
        let mut k = self.frame_at(from);
        loop {
            if self.refusal(k, size, caps).is_some() {
                run = None;
                if k >= last_start {
                    return None;
                }
            } else {
                let start = *run.get_or_insert(k);
                if k >= self.frame_at(self.time_of(start) + horizon.max(0.0)) {
                    return Some(self.time_of(start));
                }
            }
            k += 1;
        }
    }

    /// Where a stream that carries on from the compositor at timeline time `t` has to start:
    /// [`SpanPlan::handover`] of this slice's windows, at or before `t`.
    pub fn handover_at(&self, t: f64) -> f64 {
        self.handover_in_slice((t - self.start).max(0.0))
    }

    fn handover_in_slice(&self, slice_at: f64) -> f64 {
        self.start + self.windows.handover(slice_at).stream_start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track};
    use crate::media::OriginalMedia;
    use crate::model::{Transition, TransitionKind};

    const FIXED: CompositeColorPolicy = CompositeColorPolicy::FixedBt601;

    /// What a compositor that draws motion, fades and transitions asks of a plan.
    fn caps() -> GpuCaps {
        GpuCaps {
            motion: true,
            fades: true,
            transitions: true,
            text: true,
            ..GpuCaps::A0
        }
    }

    fn asset(w: u32, h: u32, fps: f64) -> Asset {
        let mut a = test_asset(vec![video_stream(w, h, fps)]);
        a.streams[0].pix_fmt = Some("yuv420p".into());
        a
    }

    fn cut(tl: &Timeline, assets: &[Asset], at: f64) -> PlayCut {
        PlayCut::new(tl, assets, at, FIXED, &OriginalMedia).expect("a cut")
    }

    /// Two one-second clips, the second dissolving in over `dissolve` seconds.
    fn pair(dissolve: f64) -> (Timeline, Vec<Asset>) {
        let a = asset(1280, 720, 30.0);
        let first = make_clip(a.id, 0.0, 1.0, 0.0);
        let mut second = make_clip(a.id, 2.0, 3.0, 1.0);
        if dissolve > 0.0 {
            second.transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: dissolve,
            });
        }
        (timeline_of(vec![video_track(vec![first, second])]), vec![a])
    }

    #[test]
    fn a_play_starts_at_the_frame_that_holds_the_playhead() {
        let (tl, assets) = pair(0.0);
        for (at, start) in [(0.0, 0.0), (0.51, 15.0 / 30.0), (0.5, 0.5), (1.0, 1.0), (1.2, 36.0 / 30.0)] {
            let c = cut(&tl, &assets, at);
            assert!((c.start() - start).abs() < 1e-9, "at {at}: {}", c.start());
            // Slice frame 0 is that frame, and the cut's frames count on from it.
            assert_eq!(c.frame_at(c.start()), 0);
            assert_eq!(c.frame_at(c.start() + 5.0 / 30.0 + 1e-4), 5);
            assert!((c.time_of(5) - (c.start() + 5.0 / 30.0)).abs() < 1e-9);
        }
    }

    #[test]
    fn nothing_plays_at_or_past_the_end_or_from_a_rate_the_stream_does_not_play() {
        let (tl, assets) = pair(0.0);
        assert!(PlayCut::new(&tl, &assets, 2.0, FIXED, &OriginalMedia).is_err(), "the end");
        assert!(PlayCut::new(&tl, &assets, 5.0, FIXED, &OriginalMedia).is_err(), "past it");
        assert!(PlayCut::new(&tl, &assets, f64::NAN, FIXED, &OriginalMedia).is_err());
        let a = asset(1280, 720, 120.0);
        let tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0)])]);
        let err = PlayCut::new(&tl, &[a], 0.0, FIXED, &OriginalMedia).err().expect("refused");
        assert!(err.to_string().contains("120"), "{err}");
    }

    #[test]
    fn a_start_inside_a_dissolve_goes_back_to_before_it() {
        // The second clip dissolves in over 0.6 s: the outgoing one plays on under it, on a tail
        // that opens 0.6 s before its end.
        let (tl, assets) = pair(0.6);
        let before = cut(&tl, &assets, 0.2);
        assert!((before.start() - 0.2).abs() < 1e-9, "no window is open at 0.2 s");
        let inside = cut(&tl, &assets, 1.1);
        assert!(inside.start() <= 0.4 + 1e-9, "inside the dissolve: {}", inside.start());
        assert!(
            inside.start() > 0.4 - 1.0 / 30.0,
            "and not further back than its window: {}",
            inside.start()
        );
        // The cut it draws still has the dissolve: the second clip fades in under the first.
        let plan = inside.plan(1.1).expect("a plan");
        assert_eq!(plan.layers.len(), 2, "both sides of the dissolve");
        // Past it the start is where it was asked.
        let after = cut(&tl, &assets, 1.9);
        assert!(after.start() > 1.8 && after.start() <= 1.9 + 1e-9, "{}", after.start());
    }

    #[test]
    fn a_clip_that_starts_a_float_under_the_frame_keeps_its_fade_in() {
        let a = asset(1280, 720, 30.0);
        // 0.1 + 0.2 + 0.7 summed as a timeline would place a clip after three others.
        let at = 0.1 + 0.2 + 0.7 - 1e-16;
        let mut c = make_clip(a.id, 0.0, 2.0, at);
        c.fade_in = 0.5;
        let tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0), c])]);
        let cut = cut(&tl, &[a], 1.0);
        let plan = cut.plan(1.0 + 0.2).expect("a plan");
        let fades: usize = plan.layers.iter().map(|l| l.fx.fades.len()).sum();
        assert_eq!(
            fades,
            1,
            "the second clip's fade-in survived the slice: {:?}",
            plan.layers.len()
        );
    }

    #[test]
    fn a_slice_that_drops_the_first_clip_keeps_the_frame_and_the_rate_of_the_whole() {
        let wide = asset(1920, 1080, 25.0);
        let tall = asset(720, 1280, 30.0);
        let tl = timeline_of(vec![video_track(vec![
            make_clip(wide.id, 0.0, 1.0, 0.0),
            make_clip(tall.id, 0.0, 2.0, 1.0),
        ])]);
        let assets = [wide, tall];
        let whole = cut(&tl, &assets, 0.0);
        assert_eq!(whole.frame(), (1920, 1080));
        // From the second clip on, the slice holds only the 720x1280 / 30 fps clip.
        let later = cut(&tl, &assets, 1.5);
        assert_eq!(later.frame(), (1920, 1080), "the frame the whole cut is made for");
        assert_eq!((later.canvas().width, later.canvas().height), (1920, 1080));
        assert_eq!(later.fps(), whole.fps(), "and its rate");
        assert_eq!((later.fps().num, later.fps().den), (25, 1));
    }

    #[test]
    fn what_is_ahead_is_the_first_frame_the_compositor_does_not_draw_and_where_a_stream_starts() {
        let (tl, assets) = pair(0.0);
        // A compositor that draws motion draws the whole cut.
        let c = cut(&tl, &assets, 0.0);
        assert_eq!(c.ahead(0.0, 5.0, (640, 360), &caps()), None);
        // One that does not is refused at the first frame, whose handover is the start.
        let ahead = c.ahead(0.0, 5.0, (640, 360), &GpuCaps::A0).expect("refused");
        assert_eq!(ahead.at, 0.0);
        assert!(matches!(ahead.why, PlayRefusal::Plan(Unsupported::Motion)), "{:?}", ahead.why);
        assert_eq!(ahead.handover, c.start());
        // Only the span asked for is looked at.
        let from_cut = c.ahead(0.2, 0.3, (640, 360), &caps());
        assert_eq!(from_cut, None);
    }

    #[test]
    fn a_refusal_inside_a_dissolve_hands_over_from_before_the_dissolve() {
        let (tl, assets) = pair(0.6);
        // No dissolve is drawn: the frames of it, and only they, are refused.
        let no_transitions = GpuCaps {
            transitions: false,
            ..caps()
        };
        let c = cut(&tl, &assets, 0.0);
        let ahead = c
            .ahead(0.0, 5.0, (640, 360), &no_transitions)
            .expect("refused inside the dissolve");
        assert!(ahead.at >= 0.7 - 1e-9 && ahead.at < 1.3, "{}", ahead.at);
        assert!(
            ahead.handover < ahead.at,
            "a stream started at the first refused frame would have no dissolve"
        );
        assert!(ahead.handover <= 0.7 + 1e-9, "{}", ahead.handover);
    }

    #[test]
    fn a_reversed_clip_and_a_moving_title_are_drawn_exactly_but_not_in_time() {
        let a = asset(1280, 720, 30.0);
        let mut back = make_clip(a.id, 0.0, 1.0, 0.0);
        back.speed = -1.0;
        let mut tl = timeline_of(vec![video_track(vec![back])]);
        let c = cut(&tl, std::slice::from_ref(&a), 0.0);
        let why = c.ahead(0.0, 5.0, (640, 360), &caps()).expect("refused").why;
        assert!(matches!(why, PlayRefusal::Reversed(_)), "{why:?}");
        assert_eq!(why.kind(), "reversed");

        let mut title = TextOverlay::new("Hello", 0.2, 0.8);
        title.keyframes = vec![
            crate::model::TextKeyframe {
                time: 0.0,
                pos_x: 0.2,
                pos_y: 0.8,
                opacity: 1.0,
            },
            crate::model::TextKeyframe {
                time: 0.5,
                pos_x: 0.5,
                pos_y: 0.8,
                opacity: 1.0,
            },
        ];
        let mut still = TextOverlay::new("Still", 0.0, 1.0);
        still.keyframes = vec![crate::model::TextKeyframe {
            time: 0.0,
            pos_x: 0.5,
            pos_y: 0.8,
            opacity: 0.5,
        }];
        tl.tracks[0].clips[0].speed = 1.0;
        tl.overlays = vec![still, title];
        let c = cut(&tl, &[a], 0.0);
        // Before the moving title, and a title that only fades, are drawn.
        assert_eq!(c.ahead(0.0, 0.1, (640, 360), &caps()), None);
        let ahead = c.ahead(0.0, 5.0, (640, 360), &caps()).expect("the moving title");
        assert!(
            matches!(ahead.why, PlayRefusal::MovingText(ref t) if t == "Hello"),
            "{:?}",
            ahead.why
        );
        assert!((ahead.at - 0.2).abs() < 1.0 / 30.0, "{}", ahead.at);
    }

    #[test]
    fn the_compositor_takes_a_play_back_where_a_run_as_long_as_the_horizon_begins() {
        // 0-2 s forward, 2-3 s a reversed clip, then forward to 10 s.
        let a = asset(1280, 720, 30.0);
        let mut back = make_clip(a.id, 0.0, 1.0, 2.0);
        back.speed = -1.0;
        let tl = timeline_of(vec![video_track(vec![
            make_clip(a.id, 0.0, 2.0, 0.0),
            back,
            make_clip(a.id, 10.0, 17.0, 3.0),
        ])]);
        let assets = [a];
        let c = cut(&tl, &assets, 0.0);
        let size = (640, 360);
        // The run from 0 s is cut by the reversed clip; the next one begins with the first frame after it
        // (the frame at 3 s is still the reversed clip's: its window holds its end, as `between` has it).
        let after = c.time_of(c.frame_at(3.0) + 1);
        assert!(c.ahead(3.0, 0.0, size, &caps()).is_some());
        assert_eq!(c.resume(0.0, 4.0, 5.0, size, &caps()), Some(after));
        assert_eq!(c.resume(2.5, 1.0, 5.0, size, &caps()), Some(after));
        // A short horizon fits before it.
        assert_eq!(c.resume(0.0, 4.0, 1.0, size, &caps()), Some(0.0));
        // Nothing begins within the reach: the reversed clip is all there is of it.
        assert_eq!(c.resume(2.0, 0.5, 5.0, size, &caps()), None);
        // What a session begun there scans is drawn.
        let from = c.resume(2.0, 2.0, 5.0, size, &caps()).expect("a run");
        assert_eq!(c.ahead(from, 5.0, size, &caps()), None);
        assert!(
            c.ahead(from - 1.0 / 30.0, 5.0, size, &caps()).is_some(),
            "the frame before is refused"
        );
        // The end of the cut ends the run: what is past it is not a refusal.
        assert_eq!(c.resume(2.5, 1.0, 30.0, size, &caps()), Some(after));
        // A compositor that draws none of it never takes it back.
        assert_eq!(c.resume(0.0, 4.0, 5.0, size, &GpuCaps::A0), None);
    }

    #[test]
    fn a_cut_with_no_video_from_there_does_not_play() {
        let a = asset(1280, 720, 30.0);
        let tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0)])]);
        // Past the only clip but inside the timeline's other content: audio alone.
        let mut with_audio = tl;
        with_audio.tracks.push(crate::model::Track {
            clips: vec![make_clip(a.id, 0.0, 3.0, 0.0)],
            ..crate::model::Track::new(StreamKind::Audio, "A1")
        });
        let err = PlayCut::new(&with_audio, &[a], 2.0, FIXED, &OriginalMedia)
            .err()
            .expect("no video");
        assert!(err.to_string().contains("no video"), "{err}");
    }
}
