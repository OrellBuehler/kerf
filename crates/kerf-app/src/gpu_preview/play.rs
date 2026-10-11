//! GPU playback (plan work package A4): the compositor draws the frames of a play session, and
//! the webview stays the clock.
//!
//! **Pull.** While the cut plays forward at 1x the page asks `get_preview_frame` for the time the
//! picture should show (the audio clock, less the output latency the sound has not got through yet,
//! plus what the request itself takes), once per animation frame, with `hint = forward` and a
//! session number. Requests go through the same latest-wins mailbox as a scrub's, so a frame that
//! takes longer than its slot costs the frames it overran and nothing else: **skip, never slow
//! down**. There is no clock here, no drift to correct and no audio plumbing.
//!
//! **A session** is a [`PlayCut`](kerf_core::PlayCut) — the Motion plan of the slice the stream
//! would play from the playhead, so what is drawn is what `stream_preview` would show — over one
//! forward-only cursor per clip ([`MotionFrames`]). The first request of a page's session decides
//! whether the GPU plays it at all, **for the whole session** (no flipping between renderers while
//! it plays): the next five seconds are scanned ([`PlayCut::ahead`](kerf_core::PlayCut::ahead)) and
//! a frame the compositor does not draw exactly, or would not draw in time, in that span, a surface
//! that cannot show what the page puts over the picture, a self-test that has not passed, a clip no
//! run is trusted for: the answer is `renderer: "stream"` with the reasons, and the page plays the
//! stream as it always did.
//!
//! **A refusal that comes into view later** (the scan goes on half a second at a time, five
//! seconds ahead, and a clip the prefetch finds untrusted counts too) is reported in the answers as
//! `play.ahead`: the time of the first frame the GPU will not draw and the time a stream that carries
//! on from the compositor has to start at ([`SpanPlan::handover`](kerf_core::SpanPlan::handover)),
//! so the page hands over before it gets there. A frame that fails mid-session ends the session the
//! same way (`stream`, with where to start).
//!
//! **Prefetch.** A thread of its own ([`Feeder`]) looks two seconds ahead of the playhead: it opens
//! the cursor of every clip that is about to come on screen ([`CursorFeed`]; the file's probes and the
//! process start are paid before the cut and not after it) and has the titles of the coming frames
//! rasterised ([`Compositor::warm_text`](kerf_gpu::Compositor::warm_text)). Neither is ever done on
//! the frame's own path.
//!
//! A session ends when the page's next request is not part of it (a paused frame, a scrub), when
//! another session replaces it, when the project changes under it, after [`IDLE`] without a
//! request, and with the backend.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kerf_core::{PlayAhead, PlayCut, ProxyMedia};
use kerf_gpu::{Compositor, CursorFeed, MotionFrames};
use serde::Serialize;
use uuid::Uuid;

use super::diagnostics::Reason;
use super::snapshot::PlanInputs;
use super::{lock, GpuPreview, PreviewFrameResult, Want};

/// How far ahead, seconds, a session looks for a frame the compositor does not draw.
pub const HORIZON: f64 = 5.0;

/// How much playback, seconds, passes between two scans.
const RESCAN: f64 = 0.5;

/// How far ahead, seconds, clips are opened and titles rasterised.
const PREFETCH: f64 = 2.0;

/// How far ahead of the playhead titles are rasterised (a rasterisation is a process; one that
/// comes due is not waited for on the frame, so it must have had time to run).
const WARM: f64 = 1.5;

/// How often the prefetch is told where the playhead is.
const FEED_EVERY: Duration = Duration::from_millis(250);

/// A session that is not asked for in this long is over.
pub const IDLE: Duration = Duration::from_secs(5);

/// A request this far (seconds) behind the one before it starts the session over: the page seeked
/// without saying so. A smaller step back is the clock's jitter, and shows the frame already shown.
const BACK: f64 = 1.0;

/// Which session a forward request belongs to: the page that asked (its pump's name) and its number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayKey {
    pub client: String,
    pub session: u64,
}

/// The first frame ahead the GPU will not draw, as the page is told (`AheadInfo` in `types.ts`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AheadInfo {
    /// The time of that frame, timeline seconds.
    pub at: f64,
    /// Where a stream that carries on from the compositor starts: at or before `at`, with no
    /// transition window open.
    pub handover: f64,
    /// A stable key for the reason (`Unsupported::kind` for the plan's, `cursor` for a clip no run
    /// is trusted for, `reversed`, `moving_text`, …).
    pub kind: &'static str,
    pub why: String,
}

/// What a forward answer says besides the renderer (`PlayInfo` in `types.ts`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlayInfo {
    pub session: u64,
    /// The timeline time of the session's first frame (a frame of the project's grid).
    pub start: f64,
    /// The frame drawn, counted from `start`.
    pub frame: u64,
    /// The request fell in the frame already on the screen: nothing was drawn.
    pub repeat: bool,
    /// The first frame ahead that the GPU will not draw, within the scan's reach, if any.
    pub ahead: Option<AheadInfo>,
}

/// What the sessions of this run did (`gpu_preview_diagnostics`).
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct PlayTally {
    /// Sessions the GPU played (a first request that was answered `gpu`).
    pub sessions: u64,
    /// First requests answered `stream`: the session is the stream's.
    pub declined: u64,
    /// Sessions that ended in `stream` after they began.
    pub handed_over: u64,
    /// Frames drawn.
    pub frames: u64,
    /// Frames of the cut that were never drawn because a newer request replaced them (counted
    /// from the gaps between the frames that were).
    pub skipped: u64,
    /// Requests that fell in the frame already on the screen.
    pub repeats: u64,
}

/// A thread that looks ahead of the playhead and opens what the frames to come need.
struct Feeder {
    tx: Sender<(f64, (u32, u32))>,
    /// Clips the prefetch found no run trusted for: (the time of the frame that needs them, why).
    failures: Arc<Mutex<Vec<(f64, String)>>>,
    told: Option<Instant>,
}

impl Feeder {
    fn start(cut: Arc<PlayCut>, feed: CursorFeed, compositor: Arc<Compositor>) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let failures = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&failures);
        std::thread::Builder::new()
            .name("kerf-gpu-play-prefetch".into())
            .spawn(move || feeder_loop(&rx, &cut, &feed, &compositor, &shared))
            .ok()
            .map(|_| Self {
                tx,
                failures,
                told: None,
            })
    }

    /// Tell the thread where the playhead is, at most every [`FEED_EVERY`].
    fn at(&mut self, t: f64, size: (u32, u32), now: Instant) {
        if self.told.is_some_and(|at| now.duration_since(at) < FEED_EVERY) {
            return;
        }
        self.told = Some(now);
        let _ = self.tx.send((t, size));
    }
}

/// The prefetch thread: it ends when the session drops its sender.
fn feeder_loop(
    rx: &Receiver<(f64, (u32, u32))>,
    cut: &PlayCut,
    feed: &CursorFeed,
    compositor: &Compositor,
    failures: &Mutex<Vec<(f64, String)>>,
) {
    // Clips a cursor was asked for (the ones on screen now are the frame's own to open).
    let mut asked: HashSet<Uuid> = HashSet::new();
    let mut warmed_to = 0u64;
    let mut warmed_size = (0, 0);
    while let Ok(mut told) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            told = newer;
        }
        let (t, size) = told;
        let from = cut.frame_at(t);
        if let Ok(now) = cut.at_frame(from) {
            asked.extend(now.layers.iter().map(|l| l.clip_id));
        }
        for k in from + 1..=cut.frame_at(t + PREFETCH) {
            let Ok(plan) = cut.at_frame(k) else { continue };
            for layer in &plan.layers {
                if !asked.insert(layer.clip_id) {
                    continue;
                }
                if let Err(error) = feed.open(layer) {
                    tracing::debug!(%error, clip = %layer.name, "a clip ahead cannot be opened for playback");
                    lock(failures).push((cut.time_of(k), error.to_string()));
                }
            }
        }
        if warmed_size != size {
            warmed_size = size;
            warmed_to = from;
        }
        let warm_to = cut.frame_at(t + WARM);
        for k in warmed_to.max(from) + 1..=warm_to {
            let Ok(plan) = cut.at_frame(k) else { continue };
            if plan.overlays.is_empty() {
                continue;
            }
            if let Err(error) = compositor.warm_text(&plan, size) {
                tracing::debug!(%error, "a title ahead could not be rasterised");
            }
        }
        warmed_to = warmed_to.max(warm_to);
    }
}

/// The last scan of the span ahead.
struct Scan {
    from: f64,
    size: (u32, u32),
    ahead: Option<PlayAhead>,
}

/// One play session; see the [module](self).
pub(super) struct PlaySession {
    key: PlayKey,
    epoch: u64,
    cut: Arc<PlayCut>,
    motion: MotionFrames,
    /// The frame source of the backend the session was made on: a rebuilt backend has another.
    source: Arc<kerf_gpu::FrameSource>,
    feeder: Option<Feeder>,
    scan: Option<Scan>,
    /// The slice frame on the screen, and the time that was asked for it.
    shown: Option<(u64, f64)>,
    last_used: Instant,
    began: Instant,
    stats: PlayTally,
    decode_ms: f64,
    composite_ms: f64,
    present_ms: f64,
    worst_ms: f64,
}

impl Drop for PlaySession {
    fn drop(&mut self) {
        // The cursors waiting for clips that did not come are the feed's; the thread holding the
        // other end of it would keep them alive until it notices that nobody is listening.
        self.motion.feed().clear();
        let s = self.stats;
        let frames = s.frames.max(1) as f64;
        tracing::info!(
            session = self.key.session,
            frames = s.frames,
            skipped = s.skipped,
            repeats = s.repeats,
            seconds = self.began.elapsed().as_secs_f64().round(),
            decode_ms = (self.decode_ms / frames * 10.0).round() / 10.0,
            composite_ms = (self.composite_ms / frames * 10.0).round() / 10.0,
            present_ms = (self.present_ms / frames * 10.0).round() / 10.0,
            worst_ms = self.worst_ms.round(),
            "GPU playback session ended"
        );
    }
}

#[cfg(test)]
impl PlaySession {
    /// Cursors opened ahead of their clips and not taken yet.
    pub(super) fn waiting(&self) -> usize {
        self.motion.feed().waiting()
    }
}

impl PlaySession {
    /// Whether this session still answers `key`'s request for time `t`.
    fn fits(&self, key: &PlayKey, epoch: u64, t: f64, now: Instant) -> bool {
        self.key == *key
            && self.epoch == epoch
            && now.duration_since(self.last_used) < IDLE
            && self.shown.is_none_or(|(_, last)| t >= last - BACK)
    }

    /// The first frame ahead the GPU will not draw: the scan's, or a clip the prefetch could not open.
    fn ahead(&mut self, t: f64, size: (u32, u32), caps: &kerf_core::GpuCaps) -> Option<AheadInfo> {
        let due = self
            .scan
            .as_ref()
            .is_none_or(|s| s.size != size || t < s.from || t - s.from >= RESCAN);
        if due {
            self.scan = Some(Scan {
                from: t,
                size,
                ahead: self.cut.ahead(t, HORIZON, size, caps),
            });
        }
        let scanned = self.scan.as_ref().and_then(|s| s.ahead.clone()).map(|a| AheadInfo {
            at: a.at,
            handover: a.handover,
            kind: a.why.kind(),
            why: a.why.to_string(),
        });
        let refused = self
            .feeder
            .as_ref()
            .and_then(|f| lock(&f.failures).iter().min_by(|a, b| a.0.total_cmp(&b.0)).cloned())
            .map(|(at, why)| AheadInfo {
                at,
                handover: self.cut.handover_at(at),
                kind: "cursor",
                why,
            });
        match (scanned, refused) {
            (Some(a), Some(b)) => Some(if b.at < a.at { b } else { a }),
            (a, b) => a.or(b),
        }
    }

    fn info(&self, frame: u64, repeat: bool, ahead: Option<AheadInfo>) -> PlayInfo {
        PlayInfo {
            session: self.key.session,
            start: self.cut.start(),
            frame,
            repeat,
            ahead,
        }
    }
}

impl GpuPreview {
    /// Serve a forward request: begin the page's session if it has none, then draw the frame at `t`
    /// (or tell that the stream plays it from here). The session is out of its lock while a frame
    /// is drawn, so turning the setting off or losing the device never waits for one.
    pub(super) fn play(&self, inputs: &PlanInputs, key: &PlayKey, t: f64, overlays: bool) -> Result<PreviewFrameResult, String> {
        let now = Instant::now();
        let mut held = lock(&self.play).take();
        if held.as_ref().is_some_and(|s| !s.fits(key, inputs.epoch(), t, now)) {
            tracing::debug!(
                session = key.session,
                "a play session is over: another request, a change or a seek"
            );
            self.retire(held.take());
        }
        if held.is_none() {
            // Under Proxy only the stream refuses a cut with clips whose proxy is not ready; so does this.
            if let Some(wait) = inputs.proxy_wait_from(t)? {
                return Err(wait);
            }
            match self.begin_play(inputs, key, t, overlays, now) {
                Ok(session) => {
                    lock(&self.play_tally).sessions += 1;
                    held = Some(session);
                }
                Err(reasons) => {
                    lock(&self.play_tally).declined += 1;
                    return Ok(self.stream_answer(reasons, None));
                }
            }
        }
        let Some(mut session) = held else {
            return Err("a play session that was just begun is gone".into());
        };
        let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.play_step(&mut session, t, overlays, now)
        }));
        match step {
            Ok(Step::Answer(result)) => {
                session.last_used = Instant::now();
                self.keep_play(session);
                Ok(result)
            }
            Ok(Step::Handover(reasons, ahead)) => {
                lock(&self.play_tally).handed_over += 1;
                let info = session.info(session.shown.map_or(0, |s| s.0), false, Some(ahead));
                self.retire(Some(session));
                Ok(self.stream_answer(reasons, Some(info)))
            }
            Err(payload) => {
                drop(session);
                let why = self.after_panic(payload.as_ref());
                lock(&self.play_tally).handed_over += 1;
                Ok(self.stream_answer(vec![Reason::new("panic", why)], None))
            }
        }
    }

    /// Hold the session for the next request, unless the setting went off or the backend was
    /// replaced while its frame was drawn.
    fn keep_play(&self, session: PlaySession) {
        let current = {
            let state = lock(&self.render);
            self.is_enabled()
                && state
                    .backend
                    .as_ref()
                    .is_some_and(|b| Arc::ptr_eq(&b.source, &session.source))
        };
        if current {
            *lock(&self.play) = Some(session);
        } else {
            self.retire(Some(session));
        }
    }

    /// A session is over: what it did joins the tally, and its cursors are killed.
    fn retire(&self, session: Option<PlaySession>) {
        if let Some(session) = session {
            let mut tally = lock(&self.play_tally);
            tally.frames += session.stats.frames;
            tally.skipped += session.stats.skipped;
            tally.repeats += session.stats.repeats;
        }
    }

    /// End the session, if there is one (the page asked for something that is not part of it, or the
    /// backend is going).
    pub(super) fn end_play(&self) {
        let session = lock(&self.play).take();
        self.retire(session);
    }

    /// What the sessions did so far.
    pub(super) fn play_diagnostics(&self) -> PlayTally {
        let mut tally = *lock(&self.play_tally);
        if let Some(live) = lock(&self.play).as_ref() {
            tally.frames += live.stats.frames;
            tally.skipped += live.stats.skipped;
            tally.repeats += live.stats.repeats;
        }
        tally
    }

    /// The answer that sends a session to the stream: the reasons (counted, and logged once per new
    /// kind, as a frame through FFmpeg is), and where it starts when the session was under way.
    fn stream_answer(&self, reasons: Vec<Reason>, play: Option<PlayInfo>) -> PreviewFrameResult {
        for kind in lock(&self.counters).refused(&reasons) {
            let example = reasons
                .iter()
                .find(|r| r.kind == kind)
                .map(|r| r.text.as_str())
                .unwrap_or_default();
            tracing::info!(
                kind,
                example,
                "a play session went to the stream for a reason not seen this session"
            );
        }
        let texts: Vec<String> = reasons.into_iter().map(|r| r.text).collect();
        tracing::debug!(reasons = ?texts, "a play session is the stream's");
        PreviewFrameResult {
            renderer: "stream",
            frame: None,
            reasons: texts,
            timings: None,
            play,
        }
    }

    /// Begin a session at `t`, or say why the stream plays it.
    fn begin_play(
        &self,
        inputs: &PlanInputs,
        key: &PlayKey,
        t: f64,
        overlays: bool,
        now: Instant,
    ) -> Result<PlaySession, Vec<Reason>> {
        let one = |kind: &'static str, why: String| vec![Reason::new(kind, why)];
        if !self.is_enabled() {
            return Err(one("preview_off", "the GPU preview is off".into()));
        }
        let Some(technique) = self.technique.technique else {
            return Err(one(
                "no_surface",
                self.technique
                    .note
                    .clone()
                    .unwrap_or_else(|| "no preview surface here".into()),
            ));
        };
        let Some(bounds) = lock(&self.bounds).current else {
            return Err(one("no_bounds", "the preview has not reported where it is".into()));
        };
        if !bounds.visible {
            return Err(one("hidden", "the preview surface is hidden".into()));
        }
        if overlays && !technique.overlays() {
            return Err(one(
                "overlays",
                "the page has something to draw over the picture, and this surface sits above the page".into(),
            ));
        }
        let cut = PlayCut::new(&inputs.timeline, &inputs.assets, t, (self.policy)(), &ProxyMedia)
            .map_err(|e| one("no_plan", format!("no plan to play from here: {e}")))?;
        self.ensure_backend(technique, now).map_err(|why| one("backend", why))?;
        let drawing = self.drawing().map_err(|r| vec![r])?;
        let first = cut
            .plan(t)
            .map_err(|e| one("no_plan", format!("no plan for the first frame: {e}")))?;
        let (_, size) = self.sized(technique, &bounds, &drawing.host, &first).map_err(|r| vec![r])?;
        let caps = drawing.compositor.caps();
        // All or nothing: a frame in the next few seconds that the GPU does not draw is the stream's
        // for the whole session, not a switch of renderer partway through it.
        if let Some(ahead) = cut.ahead(t, HORIZON, size, &caps) {
            return Err(one(ahead.why.kind(), format!("{} (at {:.2} s)", ahead.why, ahead.at)));
        }
        // Nothing the user looks at before the GPU has been checked against this FFmpeg.
        self.gate(&drawing.gpu, &drawing.adapter).map_err(|r| vec![r])?;
        let cut = Arc::new(cut);
        let motion = MotionFrames::new(Arc::clone(&drawing.source));
        let feeder = Feeder::start(Arc::clone(&cut), motion.feed(), Arc::clone(&drawing.compositor));
        tracing::info!(
            session = key.session,
            from = t,
            start = cut.start(),
            width = size.0,
            height = size.1,
            "GPU playback session begun"
        );
        Ok(PlaySession {
            key: key.clone(),
            epoch: inputs.epoch(),
            cut,
            motion,
            source: Arc::clone(&drawing.source),
            feeder,
            scan: None,
            shown: None,
            last_used: now,
            began: now,
            stats: PlayTally::default(),
            decode_ms: 0.0,
            composite_ms: 0.0,
            present_ms: 0.0,
            worst_ms: 0.0,
        })
    }

    /// One frame of a session: the frame at `t`, drawn and shown — or nothing new drawn when the
    /// frame on the screen is still the one — or the session's hand-over to the stream.
    fn play_step(&self, session: &mut PlaySession, t: f64, overlays: bool, now: Instant) -> Step {
        let stream = |reason: Reason, session: &PlaySession| {
            let at = session.cut.handover_at(t);
            Step::Handover(
                vec![reason],
                AheadInfo {
                    at: t,
                    handover: at,
                    kind: "session",
                    why: "the session ended here".into(),
                },
            )
        };
        let Some(technique) = self.technique.technique else {
            return stream(Reason::new("no_surface", "no preview surface here"), session);
        };
        let bounds = match lock(&self.bounds).current.filter(|b| b.visible) {
            Some(bounds) if self.is_enabled() => bounds,
            Some(_) => return stream(Reason::new("preview_off", "the GPU preview was turned off"), session),
            None => return stream(Reason::new("hidden", "the preview surface is hidden"), session),
        };
        if overlays && !technique.overlays() {
            return stream(
                Reason::new(
                    "overlays",
                    "the page has something to draw over the picture, and this surface sits above the page",
                ),
                session,
            );
        }
        let drawing = match self.drawing() {
            Ok(drawing) if Arc::ptr_eq(&drawing.source, &session.source) => drawing,
            Ok(_) => {
                return stream(
                    Reason::new("backend", "the GPU preview was restarted during playback"),
                    session,
                )
            }
            Err(reason) => return stream(reason, session),
        };
        let j = session.cut.frame_at(t);
        let plan = match session.cut.at_frame(j) {
            Ok(plan) => plan,
            Err(e) => return stream(Reason::new("no_plan", format!("no plan for this frame: {e}")), session),
        };
        let (canvas, size) = match self.sized(technique, &bounds, &drawing.host, &plan) {
            Ok(sized) => sized,
            Err(reason) => return stream(reason, session),
        };
        let caps = drawing.compositor.caps();
        let ahead = session.ahead(t, size, &caps);
        if let Some(feeder) = session.feeder.as_mut() {
            feeder.at(t, size, now);
        }
        // Still the frame on the screen: nothing to draw.
        if session.shown.is_some_and(|(frame, _)| frame >= j) {
            session.stats.repeats += 1;
            let frame = session.shown.map_or(j, |s| s.0);
            return Step::Answer(PreviewFrameResult {
                renderer: "gpu",
                frame: None,
                reasons: Vec::new(),
                timings: None,
                play: Some(session.info(frame, true, ahead)),
            });
        }
        let reasons = plan.reasons(&caps, size);
        if let Some(first) = reasons.first() {
            return stream(Reason::new(first.kind(), first.to_string()), session);
        }
        #[cfg(test)]
        if let Some(stall) = lock(&self.before_decode).clone() {
            stall();
        }
        let rendered = drawing.compositor.render_motion_texture(&plan, size, &mut session.motion);
        let want = Want {
            t,
            overlays,
            scrubbing: false,
        };
        match self.conclude(
            &drawing,
            technique,
            canvas,
            want,
            rendered,
            now,
            "the session goes to the stream",
        ) {
            super::Concluded::Shown(timings, _frame) => {
                lock(&self.counters).presented();
                let stats = &mut session.stats;
                stats.frames += 1;
                if let Some((last, _)) = session.shown {
                    stats.skipped += j.saturating_sub(last + 1);
                }
                session.decode_ms += timings.decode_ms;
                session.composite_ms += timings.composite_ms;
                session.present_ms += timings.present_ms;
                session.worst_ms = session
                    .worst_ms
                    .max(timings.decode_ms + timings.composite_ms + timings.present_ms);
                session.shown = Some((j, t));
                Step::Answer(PreviewFrameResult {
                    renderer: "gpu",
                    frame: None,
                    reasons: Vec::new(),
                    timings: Some(timings),
                    play: Some(session.info(j, false, ahead)),
                })
            }
            super::Concluded::Hidden => stream(
                Reason::new("hidden", "the preview surface was hidden while the frame was drawn"),
                session,
            ),
            super::Concluded::Restarted => stream(
                Reason::new("backend", "the GPU preview was restarted while the frame was drawn"),
                session,
            ),
            super::Concluded::Failed(error) => stream(Reason::new(super::error_kind(&error), error.to_string()), session),
        }
    }
}

/// How a session's frame ended.
enum Step {
    /// The GPU answered (a frame drawn, or the one on the screen).
    Answer(PreviewFrameResult),
    /// The stream takes over: the reasons, and where from.
    Handover(Vec<Reason>, AheadInfo),
}
