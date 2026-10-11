//! A play session started before Play is pressed.
//!
//! Starting a session is mostly waiting for `ffmpeg`: the process start, the file's open and its
//! first decoded frames, 50 to 70 ms for a clip and more for several, which is the whole of the
//! "start latency" there is once the compositor itself costs a few milliseconds. The playhead that is
//! about to be played from is known long before Play: it is where the paused frame was just drawn. So
//! when a paused frame has been the last request for a moment ([`PRIME_AFTER`]) the cut that would
//! play from there is **primed**: its [`PlayCut`] is made, and the cursors of the frame on screen are
//! opened — each `ffmpeg` decodes its first frames into the pipe and waits. Play then adopts that
//! session ([`GpuPreview::take_primed`]) and its first frame is a read of what is already there.
//!
//! Nothing here decides anything the session does not decide again: the five seconds ahead are
//! scanned, the surface, the self-test and the plan are asked at adoption as at any start. What
//! priming refuses (the check has not passed, a refusal within the horizon, a clip no run is trusted
//! for) is the stream's session anyway, and nothing is opened for it. A primed session is dropped (its
//! `ffmpeg`s killed) by anything that is not a play from the playhead it was made for: another paused
//! frame, a change to the project, the backend going, the setting off, and [`PRIME_TTL`] unplayed.
//! One thread (`kerf-gpu-prime`) does the work, off every lock the frames use, and ends with the
//! preview.

use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use kerf_core::{PlayCut, ProxyMedia};
use kerf_gpu::{FrameSource, MotionFrames};

use super::play::{play_config, HORIZON};
use super::snapshot::PlanInputs;
use super::{lock, GpuPreview};

/// How long the playhead rests on a paused frame before the cut from it is primed: a drag, a click
/// and another click open nothing between them.
pub const PRIME_AFTER: Duration = Duration::from_millis(300);

/// A primed session nobody played is dropped after this long.
pub const PRIME_TTL: Duration = Duration::from_secs(10);

/// A play whose first request is up to this long (seconds) before the primed start is adopted: the
/// picture is asked for the audible time, which is a little before the playhead, and what is before the
/// start is its first frame.
const BEFORE: f64 = 0.3;

/// ...and up to this long after it: the cursors read forward to the frame, 4 ms a frame.
const AFTER: f64 = 0.15;

/// A cut made ready, and the cursors of its first frame opened.
pub(super) struct Primed {
    epoch: u64,
    pub(super) cut: Arc<PlayCut>,
    pub(super) motion: MotionFrames,
    source: Arc<FrameSource>,
    created: Instant,
}

impl Primed {
    /// Whether a play from `t` on this backend and these inputs is the one this was made for.
    fn fits(&self, epoch: u64, t: f64, source: &Arc<FrameSource>, now: Instant) -> bool {
        let start = self.cut.start();
        self.epoch == epoch
            && Arc::ptr_eq(&self.source, source)
            && now.duration_since(self.created) < PRIME_TTL
            && t >= start - BEFORE
            && t <= start + AFTER
    }
}

struct Pending {
    due: Instant,
    inputs: Arc<PlanInputs>,
    t: f64,
}

#[derive(Default)]
struct State {
    next: Option<Pending>,
    running: bool,
    /// When the session primed last is dropped if it was not played.
    expire: Option<Instant>,
}

/// The thread that primes, and what it is waiting for.
pub(super) struct Primer {
    state: Mutex<State>,
    wake: Condvar,
}

impl Primer {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        })
    }

    /// Prime the cut from `t` once the playhead has rested [`PRIME_AFTER`]; a later call replaces this one.
    fn schedule(self: &Arc<Self>, preview: &Weak<GpuPreview>, inputs: Arc<PlanInputs>, t: f64) {
        let mut state = lock(&self.state);
        state.next = Some(Pending {
            due: Instant::now() + PRIME_AFTER,
            inputs,
            t,
        });
        if !state.running {
            let (primer, weak) = (Arc::clone(self), preview.clone());
            match std::thread::Builder::new()
                .name("kerf-gpu-prime".into())
                .spawn(move || primer_loop(&primer, &weak))
            {
                Ok(_) => state.running = true,
                Err(e) => tracing::debug!(error = %e, "could not start the thread that primes a play"),
            }
        }
        drop(state);
        self.wake.notify_all();
    }

    /// Nothing is primed after all (a play began, the setting went off).
    pub(super) fn cancel(&self) {
        lock(&self.state).next = None;
    }

    fn pending(&self) -> bool {
        lock(&self.state).next.is_some()
    }
}

enum Work {
    Prime(Pending),
    Expire,
}

fn primer_loop(primer: &Primer, preview: &Weak<GpuPreview>) {
    loop {
        let work = {
            let mut state = lock(&primer.state);
            loop {
                let now = Instant::now();
                if state.next.as_ref().is_some_and(|p| p.due <= now) {
                    match state.next.take() {
                        Some(pending) => break Work::Prime(pending),
                        None => continue,
                    }
                }
                if state.expire.is_some_and(|at| at <= now) {
                    state.expire = None;
                    break Work::Expire;
                }
                if preview.strong_count() == 0 {
                    state.running = false;
                    return;
                }
                // The nearest thing waited for, and at most a second so that a preview that is gone is noticed.
                let soonest = [state.next.as_ref().map(|p| p.due), state.expire]
                    .into_iter()
                    .flatten()
                    .min()
                    .map_or(Duration::from_secs(1), |at| at.saturating_duration_since(now));
                let wait = soonest.min(Duration::from_secs(1)).max(Duration::from_millis(1));
                state = primer
                    .wake
                    .wait_timeout(state, wait)
                    .map(|(guard, _)| guard)
                    .unwrap_or_else(|poisoned| poisoned.into_inner().0);
            }
        };
        let Some(preview) = preview.upgrade() else {
            lock(&primer.state).running = false;
            return;
        };
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match work {
            Work::Prime(pending) => preview.prime(&pending.inputs, pending.t),
            Work::Expire => {
                preview.expire_primed();
                false
            }
        }));
        match done {
            Ok(true) => lock(&primer.state).expire = Some(Instant::now() + PRIME_TTL),
            Ok(false) => {}
            Err(_) => tracing::warn!("priming a play panicked"),
        }
    }
}

impl GpuPreview {
    /// A paused frame was drawn at `t`: prime the cut from it if the playhead rests there.
    pub(super) fn prime_soon(&self, inputs: &Arc<PlanInputs>, t: f64) {
        if !self.is_enabled() {
            return;
        }
        if let Some(me) = self.me.get() {
            self.primer.schedule(me, Arc::clone(inputs), t);
        }
    }

    /// Make the session from `t` ready; whether one was.
    fn prime(&self, inputs: &PlanInputs, t: f64) -> bool {
        if !self.is_enabled() || lock(&self.play).is_some() {
            return false;
        }
        let Some(technique) = self.technique.technique else {
            return false;
        };
        let Some(bounds) = lock(&self.bounds).current.filter(|b| b.visible) else {
            return false;
        };
        // No backend yet is the paused frame's to build; a check that has not passed is not asked to begin.
        let Ok(drawing) = self.drawing() else {
            return false;
        };
        if !self.check.is_open(&drawing.adapter) {
            return false;
        }
        let Ok(cut) = PlayCut::new(&inputs.timeline, &inputs.assets, t, (self.policy)(), &ProxyMedia) else {
            return false;
        };
        let Ok(first) = cut.plan(t) else {
            return false;
        };
        let Ok((_, size)) = self.sized(technique, &bounds, &drawing.host, &first) else {
            return false;
        };
        // A session the stream will play is not primed.
        if cut.ahead(t, HORIZON, size, &drawing.compositor.caps()).is_some() {
            return false;
        }
        let motion = MotionFrames::with_config(Arc::clone(&drawing.source), play_config());
        let feed = motion.feed();
        for layer in &first.layers {
            // Opens the file's probes and starts its `ffmpeg`, here and not on a frame; a clip that cannot be
            // opened ends the priming (what was opened is killed with `motion`).
            if feed.open(layer).is_err() {
                return false;
            }
        }
        // A newer paused frame arrived while the probes ran, or a play began: this is not the playhead any more.
        if self.primer.pending() || lock(&self.play).is_some() || !self.is_enabled() {
            return false;
        }
        let primed = Primed {
            epoch: inputs.epoch(),
            cut: Arc::new(cut),
            motion,
            source: Arc::clone(&drawing.source),
            created: Instant::now(),
        };
        let still_there = self.drawing().is_ok_and(|now| Arc::ptr_eq(&now.source, &primed.source));
        if !still_there {
            return false;
        }
        tracing::debug!(start = primed.cut.start(), layers = first.layers.len(), "a play is primed");
        *lock(&self.primed) = Some(primed);
        true
    }

    /// The primed session, if a play from `t` is the one it was made for. Anything else primed is dropped.
    pub(super) fn take_primed(&self, epoch: u64, t: f64, source: &Arc<FrameSource>, now: Instant) -> Option<Primed> {
        self.primer.cancel();
        lock(&self.primed).take().filter(|p| p.fits(epoch, t, source, now))
    }

    /// Drop the primed session and the work waiting to make one (its `ffmpeg`s are killed).
    pub(super) fn drop_primed(&self) {
        self.primer.cancel();
        self.expire_primed();
    }

    /// Drop the primed session: it was not played in time.
    fn expire_primed(&self) {
        let primed = lock(&self.primed).take();
        drop(primed);
    }
}
