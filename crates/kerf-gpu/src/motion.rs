//! The frames of a [`PlanMode::Motion`](kerf_core::PlanMode) plan: one [`FrameCursor`] per clip.
//!
//! A still plan's layers are served by the [`FrameSource`]'s cache (`Pick::AtOrAfter`, the frame
//! `-ss` lands on). An export frame's layers pick with the `fps` filter ([`Pick::Fps`]), which
//! depends on the frames *around* the one shown and so on a clip's whole run: a [`FrameCursor`]
//! reads that run forward, once, and answers the pick of each output frame in order. This is the
//! per-clip bookkeeping around it — open a cursor the first time a clip is on screen, hand it the
//! pick of every later frame, drop it when the clip is gone — which is what an export (A7) and a
//! playback loop (A4) do, and what the Motion parity harness (`tests/motion_parity.rs`) runs.
//!
//! **Forward only**, like the cursors: frames must be asked for in ascending order per clip. A plan
//! earlier than the last one a clip was asked for (a seek) opens that clip's cursor afresh at the
//! clip's own start (a cursor decodes from there, as the export does) and reads forward to the frame.
//!
//! **Playing is a loop of these, so two things are off the frame's path.** The layers of a frame are
//! read side by side (each cursor blocks on its own `ffmpeg`, so three layers cost the slowest, not
//! the sum), and a clip that is about to come on screen is opened ahead of the frame that needs it by
//! a [`CursorFeed`], on a thread of the caller's choosing: the file's two probes (`ffprobe`) and the
//! process start are then paid before the cut and not on the frame after it. A feed that finds the
//! file untrusted says so then, when the caller can still decide what to play instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::{Pick, PlanLayer, RenderPlan};
use uuid::Uuid;

use crate::cursor::{CursorConfig, FrameCursor};
use crate::frame_source::FrameSource;
use crate::gpu::GpuError;
use crate::source::YuvFrame;

/// A cursor opened ahead of its clip is dropped (its `ffmpeg` killed) if the clip has not come on
/// screen within this long: a playhead that went elsewhere does not leave decoders parked.
const READY_TTL: Duration = Duration::from_secs(10);

/// The most cursors waiting for their clips at a time.
const READY_MAX: usize = 6;

struct Open {
    cursor: FrameCursor,
    /// The output frame of the last pick it answered.
    at: u64,
}

struct Ready {
    cursor: FrameCursor,
    since: Instant,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens cursors for clips that are not on screen yet; see the [module](self). Cheap to clone: every
/// copy feeds the [`MotionFrames`] it came from.
#[derive(Clone)]
pub struct CursorFeed {
    source: Arc<FrameSource>,
    config: CursorConfig,
    ready: Arc<Mutex<HashMap<Uuid, Ready>>>,
}

impl CursorFeed {
    /// Have a cursor for `layer`'s clip waiting: the file is checked as [`FrameSource::cursor`]
    /// checks it (so `Err(Unsupported)` says *now* that the clip cannot be played from runs) and
    /// its `ffmpeg` started, which decodes the first frames into the pipe and waits. Does nothing
    /// for a clip that already has one. **Blocks** for the file's probes the first time it is seen
    /// (tens of milliseconds a probe) and for the process start: call it from a thread that is not
    /// drawing a frame.
    pub fn open(&self, layer: &PlanLayer) -> Result<(), GpuError> {
        if !matches!(layer.pick, Pick::Fps(_)) {
            return Err(GpuError::Unsupported(format!(
                "{}: a still plan's pick is the still's `-ss`, not the export's `fps` filter",
                layer.name
            )));
        }
        if lock(&self.ready).contains_key(&layer.clip_id) {
            return Ok(());
        }
        let cursor = self.source.cursor(layer, self.config.clone())?;
        let mut ready = lock(&self.ready);
        let now = Instant::now();
        ready.retain(|_, r| now.duration_since(r.since) < READY_TTL);
        if ready.len() >= READY_MAX && !ready.contains_key(&layer.clip_id) {
            // The oldest is the one least likely to be wanted.
            if let Some(oldest) = ready.iter().min_by_key(|(_, r)| r.since).map(|(id, _)| *id) {
                ready.remove(&oldest);
            }
        }
        ready.entry(layer.clip_id).or_insert(Ready { cursor, since: now });
        Ok(())
    }

    /// Whether `layer`'s clip could have a cursor, without starting one ([`FrameSource::check_cursor`]).
    pub fn check(&self, layer: &PlanLayer) -> Result<(), GpuError> {
        self.source.check_cursor(layer)
    }

    /// Whether a cursor is waiting for the clip.
    pub fn has(&self, clip: Uuid) -> bool {
        lock(&self.ready).contains_key(&clip)
    }

    fn take(&self, clip: Uuid) -> Option<FrameCursor> {
        lock(&self.ready).remove(&clip).map(|r| r.cursor)
    }

    /// Cursors waiting for their clips.
    pub fn waiting(&self) -> usize {
        lock(&self.ready).len()
    }

    /// Drop (and kill) every waiting cursor.
    pub fn clear(&self) {
        lock(&self.ready).clear();
    }
}

/// See the [module](self).
pub struct MotionFrames {
    source: Arc<FrameSource>,
    config: CursorConfig,
    open: HashMap<Uuid, Open>,
    feed: CursorFeed,
}

impl MotionFrames {
    /// Cursors over what `source` trusts decode runs for, with the default limits.
    pub fn new(source: Arc<FrameSource>) -> Self {
        Self::with_config(source, CursorConfig::default())
    }

    pub fn with_config(source: Arc<FrameSource>, config: CursorConfig) -> Self {
        let feed = CursorFeed {
            source: Arc::clone(&source),
            config: config.clone(),
            ready: Arc::new(Mutex::new(HashMap::new())),
        };
        Self {
            source,
            config,
            open: HashMap::new(),
            feed,
        }
    }

    /// The handle that opens cursors ahead of the frames that need them.
    pub fn feed(&self) -> CursorFeed {
        self.feed.clone()
    }

    /// The picture of every layer of `plan`, in layer order: `None` where the pick names no frame
    /// (FFmpeg draws nothing there). An error is for that frame only — `Unsupported` for a file the
    /// runs are not trusted for, a still plan's layer (its pick is not the export's) — and the caller
    /// renders it through FFmpeg. A clip that is not on this plan has its cursor dropped (its
    /// `ffmpeg` killed). The layers are read side by side.
    pub fn frames(&mut self, plan: &RenderPlan) -> Result<Vec<Option<Arc<YuvFrame>>>, GpuError> {
        let live: Vec<Uuid> = plan.layers.iter().map(|l| l.clip_id).collect();
        self.open.retain(|id, _| live.contains(id));
        {
            // A cursor opened ahead for a clip that has one by now is a second decoder of it.
            let mut ready = lock(&self.feed.ready);
            ready.retain(|id, _| !self.open.contains_key(id));
        }
        let mut picks = Vec::with_capacity(plan.layers.len());
        for layer in &plan.layers {
            picks.push(self.prepare(layer)?);
        }
        if plan.layers.len() <= 1 {
            return plan
                .layers
                .iter()
                .zip(picks)
                .map(|(layer, frame)| {
                    let open = self
                        .open
                        .get_mut(&layer.clip_id)
                        .ok_or_else(|| GpuError::Gpu("a cursor that was just opened is gone".into()))?;
                    read(open, layer, frame)
                })
                .collect();
        }
        // Each cursor blocks on its own `ffmpeg`: asking them one after another would add the decodes.
        let mut cursors: HashMap<Uuid, &mut Open> = self.open.iter_mut().map(|(id, o)| (*id, o)).collect();
        let mut jobs = Vec::with_capacity(plan.layers.len());
        for (layer, frame) in plan.layers.iter().zip(picks) {
            let open = cursors
                .remove(&layer.clip_id)
                .ok_or_else(|| GpuError::Gpu("a clip is on the plan twice".into()))?;
            jobs.push((layer, frame, open));
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .into_iter()
                .map(|(layer, frame, open)| scope.spawn(move || read(open, layer, frame)))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(GpuError::Gpu("a cursor's reader panicked".into())))
                })
                .collect()
        })
    }

    /// Make sure `layer`'s clip has a cursor that can still answer its pick; the output frame of the
    /// pick.
    fn prepare(&mut self, layer: &PlanLayer) -> Result<u64, GpuError> {
        let Pick::Fps(pick) = layer.pick else {
            return Err(GpuError::Unsupported(format!(
                "{}: a still plan's pick is the still's `-ss`, not the export's `fps` filter",
                layer.name
            )));
        };
        // A frame earlier than one already answered: the cursor only reads forward.
        if self.open.get(&layer.clip_id).is_some_and(|o| pick.frame < o.at) {
            self.open.remove(&layer.clip_id);
        }
        if !self.open.contains_key(&layer.clip_id) {
            let cursor = match self.feed.take(layer.clip_id) {
                Some(cursor) => cursor,
                None => self.source.cursor(layer, self.config.clone())?,
            };
            self.open.insert(layer.clip_id, Open { cursor, at: 0 });
        }
        Ok(pick.frame)
    }

    /// Cursors alive (their `ffmpeg` runs), the ones waiting for their clips not counted.
    pub fn cursors(&self) -> usize {
        self.open.len()
    }
}

fn read(open: &mut Open, layer: &PlanLayer, frame: u64) -> Result<Option<Arc<YuvFrame>>, GpuError> {
    let shown = open.cursor.pick(&layer.pick)?;
    open.at = frame;
    Ok(shown)
}
