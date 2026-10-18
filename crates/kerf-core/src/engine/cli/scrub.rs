//! Scrubbed frames: the composited still the Preview shows at the playhead, cached, and a
//! window of its neighbours drawn by one long-lived process.
//!
//! A still is one `ffmpeg` per frame: 55 to 150 ms, nearly all of it the process starting, its
//! inputs opening and its thread pools coming up, where a frame of an all-intra proxy takes 3 to
//! 10 ms. Scrubbing asks for frame after frame, so [`ScrubFrames`] pays that once per **window**:
//! a miss starts one process on the export's own graph over a short slice of the cut
//! (`Timeline::slice`, at most [`ScrubConfig::window_frames`] frames, `-frames:v`), and every frame
//! it writes goes into the cache. The next steps of the scrub are cache hits.
//!
//! * **What a frame is.** The playhead time is quantised to the cut's frame grid (the output frame
//!   whose slot holds it, `Rational::frame_containing`) and the frame is drawn at the slot's start,
//!   as the export draws it. A raw time off the grid used to be decoded with `-ss`, which lands on
//!   the first source frame *at or after* it: one frame later than the export shows at that moment.
//! * **The key is the still's own argv** (and the stamp of each file it reads): the same text a
//!   one-shot still of that frame would be handed. An edit that changes the frame changes its key,
//!   a proxy that lands changes the input path, a replaced file changes its stamp; nothing is
//!   invalidated and nothing stale can be served. An edit elsewhere in the cut leaves this
//!   frame's key alone, and the frame stays cached.
//! * **A window frame is the still's, or the export's where the still is the one that is off.** A
//!   window runs the export graph over a slice of the cut. For a frame that `Snapshot::plain`
//!   allows its JPEG is the one-shot still's byte for byte, or (a reversed clip, a grade, a blur,
//!   a clip over another track) the export's frame, which the still misses by a frame or a level or
//!   two. What it does not allow is drawn by the one-shot still: a frame inside a fade, a dissolve,
//!   a slide, a push or a tail (`Timeline::slice` drops a clip's fade-in and transition at the cut
//!   front and its fade-out at the back), a source that is not a whole number of times the cut's
//!   rate (the `fps` filter's pick then follows where the slice starts), the last frame of a title,
//!   360 and HDR. `scrub_rendered.rs` holds each of these to the real binary.
//! * **Latest request wins.** One window at a time. A request whose frame the running window will
//!   not draw (another cut after an edit, a jump outside it) kills it and starts its own; a request
//!   it will draw waits for that frame. A scrub that runs off the end of a window starts the next
//!   one before it is reached (read-ahead), and one going backwards starts a window that ends at the
//!   frame it is on.
//! * **Nothing waits forever, nothing is spawned under a lock.** The process is started with the
//!   state lock released; a reaper kills a window that has drawn nothing for
//!   [`ScrubConfig::first_frame_timeout`] or stalled between frames for
//!   [`ScrubConfig::frame_timeout`]; `stop`, `clear`, an edit that makes the window useless, a jump
//!   away from it and `Drop` kill it, and `stop` and `clear` also keep a request already waiting
//!   from starting another. A request that cannot be served from a window (or whose window died
//!   or stalled by itself) renders the one-shot still, bounded like every still.

use std::borrow::Cow;
use std::io::Read;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use super::still_cache::{file_stamp, StillCache, StillCacheStats, StillKey};
use super::{
    build_filter_complex, build_still_args_rendered, command, decode_hwaccel, export_format, externalize_filter_complex,
    facts_from_assets, ffmpeg_bin, launch_err, next_jpeg, push_inputs, run_still, still_layers, tune_inputs, ExportOptions,
    HostOs, Reading, StillOutput,
};
use crate::clip_timing::Rational;
use crate::engine::cpu;
use crate::error::{Error, Result};
use crate::model::{Asset, StreamKind, Timeline};
use crate::render_plan::still_size;

/// What a [`ScrubFrames`] holds and how it behaves. [`Default`] is the production numbers
/// (`KERF_SCRUB_CACHE_MB`, `KERF_SCRUB_WINDOW`).
#[derive(Debug, Clone)]
pub struct ScrubConfig {
    /// The cache's byte budget.
    pub cache_bytes: usize,
    /// The cache's entry budget.
    pub cache_entries: usize,
    /// Frames one window draws, at most. `0` turns windows off: every miss is a one-shot still
    /// (still cached, still on the frame grid).
    pub window_frames: u32,
    /// Frames before the requested one a window starts, going forward (the first frames back are
    /// then hits as well).
    pub lead_frames: u32,
    /// Frames from the end of a window at which a scrub moving that way starts the next window.
    /// `0` turns the read-ahead off.
    pub read_ahead_frames: u32,
    /// How far ahead of what a running window has drawn a request still waits for it, rather than
    /// starting a window of its own.
    pub wait_frames: u32,
    /// A window that has drawn nothing for this long is killed.
    pub first_frame_timeout: Duration,
    /// A window that stalls this long between two frames is killed.
    pub frame_timeout: Duration,
    /// The longest one request waits on a window before it renders the one-shot instead.
    pub request_timeout: Duration,
}

impl Default for ScrubConfig {
    fn default() -> Self {
        let mb = std::env::var("KERF_SCRUB_CACHE_MB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&mb| mb > 0)
            .unwrap_or(64);
        let window_frames = match std::env::var("KERF_SCRUB_WINDOW") {
            Ok(v) if v.trim().eq_ignore_ascii_case("off") => 0,
            Ok(v) => v.trim().parse::<u32>().unwrap_or(24).min(120),
            Err(_) => 24,
        };
        Self {
            cache_bytes: mb.saturating_mul(1 << 20),
            cache_entries: 2048,
            window_frames,
            lead_frames: 1,
            read_ahead_frames: 8,
            wait_frames: 12,
            first_frame_timeout: Duration::from_secs(10),
            frame_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(20),
        }
    }
}

/// What a [`ScrubFrames`] has done, for logs and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrubStats {
    pub cache: StillCacheStats,
    /// Frames answered from the cache without a process.
    pub hits: u64,
    /// Frames that were not in the cache when asked for.
    pub misses: u64,
    /// Windows started.
    pub windows: u64,
    /// Windows killed before they drew everything they were for (superseded, stopped, timed out).
    pub killed: u64,
    /// Frames a window drew into the cache.
    pub drawn: u64,
    /// Frames rendered by a one-shot still of their own.
    pub singles: u64,
}

/// Composited stills for scrubbing: see the [module](self). One per process, shared by `Arc`;
/// every call blocks, so make them from blocking-pool threads and never under the project lock.
pub struct ScrubFrames {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    config: ScrubConfig,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct State {
    cache: StillCache,
    /// The latest window, running or finished (a finished one is what read-ahead continues).
    window: Option<Window>,
    /// The frame the last request was for: which way a scrub is going.
    last: Option<u64>,
    next_id: u64,
    stats: ScrubStats,
    closed: bool,
    /// Counts the `stop`s and `clear`s: a request that began, or a read-ahead that was planned,
    /// before one does not start a window after it.
    epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Life {
    /// Entered in the table, the process not yet running.
    Starting,
    Running,
    /// Its output ended cleanly.
    Done,
    /// It died, was killed, or could not be started.
    Failed(String),
}

struct Window {
    id: u64,
    snap: Arc<Snapshot<'static>>,
    /// The frames it was started for, `[a, b)`; `head` is the next one it will draw.
    a: u64,
    b: u64,
    head: u64,
    life: Life,
    child: Option<Arc<Mutex<Child>>>,
    stop: Arc<AtomicBool>,
    started: Instant,
    last_progress: Instant,
}

impl Window {
    fn alive(&self) -> bool {
        matches!(self.life, Life::Starting | Life::Running)
    }
}

/// Everything about one request's cut that frames are drawn from and keyed on: the rendered
/// timeline, the preview assets and the shape of the picture.
pub(super) struct Snapshot<'a> {
    rendered: Timeline,
    assets: Cow<'a, [Asset]>,
    grid: Grid,
    /// The delivery frame (before the preview width caps it).
    frame: (u32, u32),
    width: u32,
    quality: u8,
}

/// The cut's frame grid: which output frame holds a time, and when a frame starts.
#[derive(Debug, Clone, Copy)]
pub(super) struct Grid {
    fps: f64,
    rate: Option<Rational>,
}

/// Frames past this are not frames of any cut (a time that is not a time).
const MAX_FRAME: u64 = 1 << 40;

impl Grid {
    pub(super) fn new(fps: f64) -> Self {
        let fps = if fps.is_finite() && fps > 0.0 { fps } else { 30.0 };
        Self {
            fps,
            rate: Rational::from_fps(fps),
        }
    }

    /// The output frame whose slot holds `t`; the first for a time before the start.
    pub(super) fn frame_of(&self, t: f64) -> u64 {
        let k = match self.rate {
            Some(r) => r.frame_containing(t),
            None => (t.max(0.0) * self.fps + 1e-6).floor() as u64,
        };
        k.min(MAX_FRAME)
    }

    /// The start of frame `k`'s slot, in seconds.
    pub(super) fn time_of(&self, k: u64) -> f64 {
        match self.rate {
            Some(r) => r.exact_time(k),
            None => k as f64 / self.fps,
        }
    }
}

/// Whether a clip of `asset` played at `speed` reaches the cut's rate by dropping a whole number
/// of source frames for each one it keeps (or none): a ratio of 1, 2, 3, ..., within half a
/// percent, so 29.97 in a 30 fps cut and 59.94 in 29.97 count. A still image has no rate of its
/// own and always does; a video whose rate was never recorded cannot be told, so it does not.
fn decimates_evenly(asset: &Asset, speed: f64, fps: f64) -> bool {
    if asset.is_image() {
        return true;
    }
    let source = asset
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .and_then(|s| s.fps)
        .filter(|f| f.is_finite() && *f > 0.0);
    let Some(source) = source else { return false };
    let ratio = source * speed / fps;
    let whole = ratio.round();
    whole >= 1.0 && (ratio - whole).abs() <= 0.005 * whole
}

impl<'a> Snapshot<'a> {
    pub(super) fn new(timeline: &Timeline, assets: &'a [Asset], width: u32, quality: u8) -> Self {
        let rendered = timeline.for_render();
        let fmt = export_format(&rendered, assets, &ExportOptions::default());
        Self {
            rendered,
            assets: Cow::Borrowed(assets),
            grid: Grid::new(fmt.fps),
            frame: (fmt.width, fmt.height),
            width,
            quality,
        }
    }

    /// The delivery rate the cut's frames are counted at.
    #[cfg(test)]
    pub(super) fn fps(&self) -> f64 {
        self.grid.fps
    }

    /// The timeline as it renders.
    #[cfg(test)]
    pub(super) fn rendered(&self) -> &Timeline {
        &self.rendered
    }

    fn to_static(&self) -> Snapshot<'static> {
        Snapshot {
            rendered: self.rendered.clone(),
            assets: Cow::Owned(self.assets.to_vec()),
            grid: self.grid,
            frame: self.frame,
            width: self.width,
            quality: self.quality,
        }
    }

    /// The argv of the one-shot still of frame `k`: what it is keyed on, and what draws it when no
    /// window can. Hardware decode is not in it (an option of the spawn, not of the picture).
    pub(super) fn still_args(&self, k: u64) -> Result<Vec<String>> {
        build_still_args_rendered(
            &self.rendered,
            &self.assets,
            &ExportOptions::default(),
            self.grid.time_of(k),
            self.width,
            None,
            &StillOutput::JpegPipe { quality: self.quality },
        )
    }

    /// The cache key of frame `k`.
    pub(super) fn key(&self, k: u64) -> Result<StillKey> {
        Ok(StillKey::of(&self.still_args(k)?, file_stamp))
    }

    /// Whether the export graph over a slice draws frame `k` as the export does, whichever frame
    /// the slice starts at (`scrub_rendered.rs` holds each rule to the real binary). Not so:
    ///
    /// * inside a fade, a dissolve, a slide, a push or a tail: `Timeline::slice` drops the
    ///   fade-in and the transition of a clip it cuts at the front and the fade-out of one it cuts
    ///   at the back, and the one-shot still draws these frames as the file does;
    /// * a source that is not decimated by a whole number to the cut's rate (24 or 25 fps in a
    ///   30 fps cut, a clip at 1.5x): the `fps` filter's pick of the source frame then depends on
    ///   where the slice starts, and differs from the still's `-ss` pick on some frames of each
    ///   cycle. (Rates a whole number of times the cut's, 60 in 30 and 2x, and 29.97 in 30, agree.);
    /// * the last frame of a title: the window's `drawtext` shows it through `end`, the still
    ///   stops before it;
    /// * 360 reframing and HDR originals, which the stream graph draws in another order.
    pub(super) fn plain(&self, k: u64) -> bool {
        let t = self.grid.time_of(k);
        if self.rendered.overlays.iter().any(|o| (o.end - t).abs() < 1e-6) {
            return false;
        }
        still_layers(&self.rendered, &self.assets, self.grid.fps, t)
            .iter()
            .all(|layer| {
                let clip = layer.active.clip;
                let Some(asset) = self.assets.iter().find(|a| a.id == clip.asset_id) else {
                    return false;
                };
                layer.fades.is_empty()
                    && layer.motion == (0.0, 0.0)
                    && clip.timeline_end() > t + 1e-9
                    && clip.reframe.is_none()
                    && asset.hdr().is_none()
                    && decimates_evenly(asset, clip.speed_mag(), self.grid.fps)
            })
    }

    /// The frames `[a, b)` of a window started for frame `k`, or `None` when `k` itself is not one
    /// the window graph draws. Going forward the window starts `lead_frames` before `k`, going
    /// back it ends two frames after it, so the step the other way is a hit. The lead is one frame
    /// over an original too: reaching `k` the process decodes every frame from the keyframe, so
    /// `k - 1` costs a filter and a JPEG, and without it a click followed by a step back is a miss
    /// that starts a window of its own (`tests/scrub_bench.rs`: 0.1 ms with the lead, 870 ms without
    /// on a 1080p long-GOP original, against 370 for the one-shot). Stops at the first frame that
    /// is not [`plain`](Self::plain).
    pub(super) fn window_range(&self, k: u64, reverse: bool, config: &ScrubConfig) -> Option<(u64, u64)> {
        let n = u64::from(config.window_frames);
        if n < 2 || !self.plain(k) {
            return None;
        }
        let lead = if reverse {
            n.saturating_sub(2)
        } else {
            u64::from(config.lead_frames)
        };
        let lead = lead.min(k);
        let lo = k - lead;
        let mut a = k;
        while a > lo && self.plain(a - 1) {
            a -= 1;
        }
        let hi = lo.saturating_add(n).max(k + 1);
        let mut b = k + 1;
        while b < hi && self.plain(b) {
            b += 1;
        }
        (b - a >= 2).then_some((a, b))
    }

    /// The frames of a window that carries on from `from` going forward (`forward`) or ends at
    /// `from` going back, if every one of them is plain.
    pub(super) fn window_beyond(&self, from: u64, forward: bool, config: &ScrubConfig) -> Option<(u64, u64)> {
        let n = u64::from(config.window_frames);
        if n < 2 {
            return None;
        }
        if forward {
            let mut b = from;
            while b < from.saturating_add(n) && self.plain(b) {
                b += 1;
            }
            (b - from >= 2).then_some((from, b))
        } else {
            let lo = from.saturating_sub(n);
            let mut a = from;
            while a > lo && self.plain(a - 1) {
                a -= 1;
            }
            (from - a >= 2).then_some((a, from))
        }
    }

    /// The `ffmpeg` argv drawing frames `[a, b)` as JPEGs on stdout: the export graph over a slice
    /// of the cut from frame `a`, at the still's size, grid and scaler, ending after `b - a`
    /// frames. Two frames more than asked for are in the slice so that a clip ending at the last
    /// frame is not cut at it.
    pub(super) fn window_args(&self, a: u64, b: u64, hwaccel: Option<String>) -> Result<Vec<String>> {
        let w0 = self.grid.time_of(a);
        let end = self.rendered.duration();
        if !w0.is_finite() || w0 >= end {
            return Err(Error::Engine("nothing to draw past the end of the cut".to_string()));
        }
        let w1 = self.grid.time_of(b.saturating_add(2)).min(end);
        let sliced = self.rendered.slice(w0, w1);
        if !sliced
            .tracks
            .iter()
            .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty())
        {
            return Err(Error::Engine("no video in this stretch of the cut".to_string()));
        }
        let (ow, oh) = still_size(self.frame.0, self.frame.1, self.width);
        let opts = ExportOptions {
            include_audio: false,
            fps: Some(self.grid.fps),
            resolution: Some((ow, oh)),
            hwaccel,
            ..ExportOptions::default()
        };
        let fmt = export_format(&sliced, &self.assets, &opts);
        let mut args: Vec<String> = ["-hide_banner", "-nostats", "-nostdin", "-loglevel", "error"]
            .map(String::from)
            .into();
        let plan = push_inputs(&sliced, &self.assets, &fmt, &opts, &mut args)?;
        let graph = build_filter_complex(&sliced, &self.assets, &fmt, sliced.duration(), &opts, true, false, &plan);
        args.extend(["-filter_complex".to_string(), graph.filter]);
        args.extend(["-map", "[outv]", "-an", "-c:v", "mjpeg", "-q:v"].map(String::from));
        args.push(self.quality.to_string());
        args.extend(["-frames:v".to_string(), (b - a).to_string()]);
        args.extend(["-f", "image2pipe", "pipe:1"].map(String::from));
        Ok(args)
    }
}

/// Which way the scrub is going, from the frame before to the frame now: `Some(true)` forward,
/// `Some(false)` back, `None` for a first request, the same frame again or a jump too far to be a
/// step (a jump is not a direction: nothing is read ahead from it).
fn direction(last: Option<u64>, k: u64, reach: u64) -> Option<bool> {
    let last = last?;
    if last == k || last.abs_diff(k) > reach {
        return None;
    }
    Some(k > last)
}

/// What the latest window offers a request, as the state knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Offer {
    id: u64,
    alive: bool,
    /// The window's own cut draws the same frame the request asks for (its keys agree on it).
    related: bool,
    b: u64,
    head: u64,
}

/// A read-ahead window a hit has asked for: where from, which way, and what to check before it
/// starts (the finished window's cut, and the epoch the request saw).
struct Ahead {
    from: u64,
    forward: bool,
    k: u64,
    key: StillKey,
    epoch: u64,
    theirs: Arc<Snapshot<'static>>,
}

/// What the latest window offers a request: nothing, an answer, or a question for its cut.
enum Offered {
    Nothing,
    Is(Offer),
    /// The window's own snapshot has to say whether it draws the requested frame; asked off the lock.
    Ask {
        id: u64,
        theirs: Arc<Snapshot<'static>>,
    },
}

/// Whether a request for frame `k` waits for the running window (pure, so each rule is a test),
/// rather than starting one of its own or drawing the frame alone. `mine` is the window this
/// request started itself (waited for whatever the distance: a window going back is started far
/// from the frame it is for); `wait_frames` is how far ahead of what another request's window has
/// drawn this one still waits instead of starting its own.
fn waits_for(offer: Option<Offer>, k: u64, wait_frames: u64, mine: Option<u64>) -> bool {
    // Heading for `k`: it has not drawn it, and it is not so far off that a window of its own
    // would be there first.
    offer
        .filter(|w| w.alive && w.related)
        .is_some_and(|w| k >= w.head && k < w.b && (mine == Some(w.id) || k - w.head <= wait_frames))
}

/// Whether frame `k` is within `reach` frames of the end of a window's frames `[a, head)`, on the
/// side a scrub going `forward` (or back) is heading for.
fn near_edge(a: u64, head: u64, k: u64, forward: bool, reach: u64) -> bool {
    if k < a || k >= head {
        return false;
    }
    if forward {
        head - k <= reach
    } else {
        a > 0 && k - a <= reach
    }
}

impl ScrubFrames {
    pub fn new(config: ScrubConfig) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                cache: StillCache::new(config.cache_bytes, config.cache_entries),
                window: None,
                last: None,
                next_id: 0,
                stats: ScrubStats::default(),
                closed: false,
                epoch: 0,
            }),
            changed: Condvar::new(),
            config,
        });
        let weak = Arc::downgrade(&shared);
        let _ = std::thread::Builder::new()
            .name("kerf-scrub-reaper".into())
            .spawn(move || reaper(&weak));
        Self { shared }
    }

    /// The composited frame under the playhead at `t`, as JPEG bytes (`quality` is ffmpeg's
    /// `-q:v`), at most `max_width` px wide. `assets` are the preview assets (proxies swapped in).
    ///
    /// `t` is quantised to the cut's frame grid. Served from the cache when the frame is held;
    /// otherwise from a window that draws it and its neighbours, or, where a window cannot (a frame
    /// inside a transition, 360, HDR, windows off, a window that failed), from the one-shot still.
    pub fn frame(&self, timeline: &Timeline, assets: &[Asset], t: f64, max_width: u32, quality: u8) -> Result<Arc<Vec<u8>>> {
        let shared = &self.shared;
        let config = &shared.config;
        let snap = Snapshot::new(timeline, assets, max_width, quality);
        let k = snap.grid.frame_of(t);
        let key = snap.key(k)?;
        let toward;
        let epoch;
        {
            let mut st = shared.lock();
            epoch = st.epoch;
            toward = direction(st.last, k, u64::from(config.window_frames).max(8));
            st.last = Some(k);
            if let Some(frame) = st.cache.get(key) {
                st.stats.hits += 1;
                let ahead = shared.ahead(&st, k, key, toward);
                drop(st);
                shared.read_ahead_later(&snap, ahead);
                return Ok(frame);
            }
            st.stats.misses += 1;
        }

        let deadline = Instant::now() + config.request_timeout;
        let mut starts = 0;
        let mut mine: Option<u64> = None;
        let mut related: Option<(u64, bool)> = None;
        loop {
            let mut st = shared.lock();
            if st.closed {
                break;
            }
            if let Some(frame) = st.cache.get(key) {
                let ahead = shared.ahead(&st, k, key, toward);
                drop(st);
                shared.read_ahead_later(&snap, ahead);
                return Ok(frame);
            }
            // Stopped since the request began (playback took the pane): no window is started for it.
            // Its own window ending by itself (it died, stalled, or finished without this frame) is
            // not tried again; only a window another request replaced is.
            let ended = |st: &State| mine.is_some_and(|id| st.window.as_ref().is_some_and(|w| w.id == id && !w.alive()));
            if st.epoch != epoch || ended(&st) {
                break;
            }
            let offer = match st.offer(k, related) {
                Offered::Nothing => None,
                Offered::Is(offer) => Some(offer),
                Offered::Ask { id, theirs } => {
                    // Whether that window draws this frame is asked of its cut off the lock.
                    drop(st);
                    related = Some((id, theirs.key(k).is_ok_and(|ours| ours == key)));
                    continue;
                }
            };
            if waits_for(offer, k, u64::from(config.wait_frames), mine) {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let wait = (deadline - now).min(Duration::from_millis(250));
                drop(shared.changed.wait_timeout(st, wait).unwrap_or_else(PoisonError::into_inner));
                continue;
            }
            // Nothing is coming for this frame. Which window to start is planned (and the cut
            // copied for it) without the state's lock: other requests and the window's reader file
            // frames under it.
            drop(st);
            let Some((a, b)) = (starts < 2)
                .then(|| snap.window_range(k, toward == Some(false), config))
                .flatten()
            else {
                break;
            };
            let fresh = Arc::new(snap.to_static());
            let st = shared.lock();
            if st.closed || st.epoch != epoch {
                break;
            }
            starts += 1;
            mine = Some(shared.start_window(st, fresh, a, b));
        }
        self.single(&snap, key, k)
    }

    /// The one-shot still of frame `k`, kept in the cache.
    fn single(&self, snap: &Snapshot<'_>, key: StillKey, k: u64) -> Result<Arc<Vec<u8>>> {
        let opts = ExportOptions {
            hwaccel: decode_hwaccel(),
            ..ExportOptions::default()
        };
        let out = StillOutput::JpegPipe { quality: snap.quality };
        let jpeg = run_still(
            &snap.rendered,
            &snap.assets,
            &opts,
            snap.grid.time_of(k),
            snap.width,
            None,
            &out,
            &|| false,
        )?;
        let mut st = self.shared.lock();
        st.stats.singles += 1;
        st.cache.insert(key, jpeg.clone());
        Ok(st.cache.get(key).unwrap_or_else(|| Arc::new(jpeg)))
    }

    /// Stop the window that is running (playback took over, the project was replaced): its process
    /// is killed and what it drew stays cached.
    pub fn stop(&self) {
        let victim = {
            let mut st = self.shared.lock();
            st.epoch += 1;
            st.retire_window("stopped")
        };
        self.shared.changed.notify_all();
        if let Some(child) = victim {
            reap_later(child);
        }
    }

    /// [`stop`](Self::stop) and forget every frame.
    pub fn clear(&self) {
        let victim = {
            let mut st = self.shared.lock();
            st.cache.clear();
            st.epoch += 1;
            let victim = st.retire_window("cleared");
            st.window = None;
            st.last = None;
            victim
        };
        self.shared.changed.notify_all();
        if let Some(child) = victim {
            reap_later(child);
        }
    }

    pub fn stats(&self) -> ScrubStats {
        let st = self.shared.lock();
        ScrubStats {
            cache: st.cache.stats(),
            ..st.stats
        }
    }
}

impl Shared {
    /// Where the next window should start for the request just served, if it should start one: a
    /// scrub moving along a finished window that has come within `read_ahead_frames` of its end
    /// (or, going back, of its start). Only what is cheap to know under the lock; the rest is
    /// asked in [`read_ahead_later`](Self::read_ahead_later).
    fn ahead(&self, st: &State, k: u64, key: StillKey, toward: Option<bool>) -> Option<Ahead> {
        let forward = toward?;
        if self.config.read_ahead_frames == 0 {
            return None;
        }
        let w = st.window.as_ref().filter(|w| w.life == Life::Done)?;
        if !near_edge(w.a, w.head, k, forward, u64::from(self.config.read_ahead_frames)) {
            return None;
        }
        Some(Ahead {
            from: if forward { w.head } else { w.a },
            forward,
            k,
            key,
            epoch: st.epoch,
            theirs: Arc::clone(&w.snap),
        })
    }

    /// Start the read-ahead window on a thread of its own: the request that wanted it has its
    /// frame and does not wait for a process to be spawned. The thread checks that the finished
    /// window is of this cut (an edit since may have moved its frames) and that nothing was
    /// stopped since.
    fn read_ahead_later(self: &Arc<Self>, snap: &Snapshot<'_>, ahead: Option<Ahead>) {
        let Some(ahead) = ahead else { return };
        let shared = Arc::clone(self);
        let snap = Arc::new(snap.to_static());
        let _ = std::thread::Builder::new().name("kerf-scrub-ahead".into()).spawn(move || {
            if !ahead.theirs.key(ahead.k).is_ok_and(|theirs| theirs == ahead.key) {
                return;
            }
            let Some((a, b)) = snap.window_beyond(ahead.from, ahead.forward, &shared.config) else {
                return;
            };
            let st = shared.lock();
            if st.closed || st.epoch != ahead.epoch || st.window.as_ref().is_some_and(Window::alive) {
                return;
            }
            shared.start_window(st, snap, a, b);
        });
    }

    /// Enter a window over `[a, b)` in the table (killing the one before it), start its process
    /// with the lock released, and give it its reader. Returns the window's id.
    fn start_window(self: &Arc<Self>, mut st: MutexGuard<'_, State>, snap: Arc<Snapshot<'static>>, a: u64, b: u64) -> u64 {
        let victim = st.retire_window("superseded");
        st.next_id += 1;
        let id = st.next_id;
        st.stats.windows += 1;
        let now = Instant::now();
        st.window = Some(Window {
            id,
            snap: Arc::clone(&snap),
            a,
            b,
            head: a,
            life: Life::Starting,
            child: None,
            stop: Arc::new(AtomicBool::new(false)),
            started: now,
            last_progress: now,
        });
        drop(st);
        self.changed.notify_all();
        if let Some(child) = victim {
            reap_later(child);
        }

        // The process, off the lock.
        let spawned = spawn_window(&snap, a, b);
        let mut st = self.lock();
        let current = st.window.as_ref().is_some_and(|w| w.id == id && w.alive()) && !st.closed;
        match spawned {
            Ok((child, stdout, stderr, script)) if current => {
                let child = Arc::new(Mutex::new(child));
                if let Some(w) = st.window.as_mut() {
                    w.child = Some(Arc::clone(&child));
                    w.life = Life::Running;
                    w.started = Instant::now();
                    w.last_progress = w.started;
                }
                drop(st);
                let shared = Arc::clone(self);
                let reading = Arc::clone(&child);
                let reader = std::thread::Builder::new()
                    .name("kerf-scrub-run".into())
                    .spawn(move || read_window(&shared, id, &snap, (a, b), (stdout, stderr), &reading, script));
                if reader.is_err() {
                    let victim = self.lock().fail_window(id, "could not start a reader thread");
                    if let Some(child) = victim {
                        reap_later(child);
                    }
                }
            }
            Ok((child, ..)) => {
                // Superseded or closed while the process was starting.
                drop(st);
                reap_later(Arc::new(Mutex::new(child)));
            }
            Err(why) => {
                if let Some(w) = st.window.as_mut().filter(|w| w.id == id && w.alive()) {
                    w.life = Life::Failed(why.to_string());
                }
                drop(st);
                tracing::debug!(error = %why, "a scrub window did not start");
            }
        }
        self.changed.notify_all();
        id
    }
}

impl Drop for ScrubFrames {
    fn drop(&mut self) {
        let victim = {
            let mut st = self.shared.lock();
            st.closed = true;
            let victim = st.retire_window("dropped");
            st.cache.clear();
            victim
        };
        self.shared.changed.notify_all();
        if let Some(child) = victim {
            kill(&child);
        }
    }
}

impl State {
    /// What the latest window offers the request for frame `k`. Whether the window draws the same
    /// frame is the question of its own cut (`related` remembers the answer for a window): the
    /// window is for this request only if its snapshot's frame `k` is the one asked for (an edit
    /// that moved it makes the window useless here, and one that did not leaves it useful).
    fn offer(&self, k: u64, related: Option<(u64, bool)>) -> Offered {
        let Some(w) = self.window.as_ref() else {
            return Offered::Nothing;
        };
        let same = match related {
            Some((id, same)) if id == w.id => same,
            _ if !(k >= w.a && k < w.b) => false,
            _ => {
                return Offered::Ask {
                    id: w.id,
                    theirs: Arc::clone(&w.snap),
                }
            }
        };
        Offered::Is(Offer {
            id: w.id,
            alive: w.alive(),
            related: same,
            b: w.b,
            head: w.head,
        })
    }

    /// End the latest window's life (if it is alive) and hand back its process to kill. Its
    /// reader sees the stop flag, and anything waiting on it wakes to find it gone.
    fn retire_window(&mut self, why: &str) -> Option<Arc<Mutex<Child>>> {
        let w = self.window.as_mut().filter(|w| w.alive())?;
        w.stop.store(true, Ordering::SeqCst);
        w.life = Life::Failed(why.to_string());
        self.stats.killed += 1;
        w.child.take()
    }

    /// The window `id` failed: stop it, and hand back its process.
    fn fail_window(&mut self, id: u64, why: &str) -> Option<Arc<Mutex<Child>>> {
        let w = self.window.as_mut().filter(|w| w.id == id && w.alive())?;
        w.stop.store(true, Ordering::SeqCst);
        w.life = Life::Failed(why.to_string());
        self.stats.killed += 1;
        w.child.take()
    }
}

/// The window's process, its pipes and the graph script it reads (kept until the process is done).
type Spawned = (
    Child,
    std::process::ChildStdout,
    std::process::ChildStderr,
    super::GraphScript,
);

/// Start the `ffmpeg` of a window: with the lock released, tuned as the playback stream is (a
/// proxy in software, the budget's threads).
fn spawn_window(snap: &Snapshot<'static>, a: u64, b: u64) -> Result<Spawned> {
    let mut args = snap.window_args(a, b, decode_hwaccel())?;
    let script = externalize_filter_complex(&mut args, "scrub")?;
    let tuning = tune_inputs(&mut args, HostOs::CURRENT, Reading::Window, &facts_from_assets(&snap.assets));
    if tuning.all_cheap {
        // Every input a proxy or an image: one thread each, for the graph and for the encoder. A
        // window of intra frames at preview size is done before a pool pays for its start-up (the
        // first frame of three proxy layers is a third sooner, the whole window no later).
        cpu::single_frame_args(&mut args);
    } else {
        cpu::limit_args(&mut args, cpu::budget_threads());
    }
    let bin = ffmpeg_bin();
    let mut child = command(&bin)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    match (child.stdout.take(), child.stderr.take()) {
        (Some(out), Some(err)) => Ok((child, out, err, script)),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            Err(Error::Engine("ffmpeg's pipes were not captured".to_string()))
        }
    }
}

/// The reader of window `id`: splits its stdout into JPEGs, keys each by the still it stands for
/// and files it in the cache, until the process ends, it is stopped or its output is not JPEGs.
fn read_window(
    shared: &Arc<Shared>,
    id: u64,
    snap: &Snapshot<'static>,
    (a, b): (u64, u64),
    (mut stdout, stderr): (std::process::ChildStdout, std::process::ChildStderr),
    child: &Arc<Mutex<Child>>,
    _script: super::GraphScript,
) {
    let tail = Arc::new(Mutex::new(String::new()));
    let err_tail = Arc::clone(&tail);
    let _ = std::thread::Builder::new().name("kerf-scrub-stderr".into()).spawn(move || {
        let mut text = String::new();
        let _ = std::io::BufReader::new(stderr).read_to_string(&mut text);
        let keep: Vec<&str> = text.lines().rev().take(8).collect();
        *err_tail.lock().unwrap_or_else(PoisonError::into_inner) = keep.into_iter().rev().collect::<Vec<_>>().join("\n");
    });

    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut drawn = 0u64;
    let outcome: std::result::Result<(), String> = 'read: loop {
        let n = match stdout.read(&mut chunk) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) => break Err(format!("reading the window failed: {e}")),
        };
        buf.extend_from_slice(&chunk[..n]);
        while let Some((s, e)) = next_jpeg(&buf) {
            let jpeg = buf[s..e].to_vec();
            buf.drain(..e);
            let frame = a + drawn;
            if frame >= b {
                break 'read Ok(());
            }
            drawn += 1;
            // Keyed off the lock, by the still this frame stands for.
            let key = snap.key(frame).ok();
            let mut st = shared.lock();
            let current = st.window.as_ref().is_some_and(|w| w.id == id && w.alive()) && !st.closed;
            if !current {
                break 'read Err("superseded".to_string());
            }
            if let Some(key) = key {
                st.cache.insert(key, jpeg);
                st.stats.drawn += 1;
            }
            if let Some(w) = st.window.as_mut() {
                w.head = frame + 1;
                w.last_progress = Instant::now();
            }
            drop(st);
            shared.changed.notify_all();
            // Everything it was started for: the process is about to exit by itself
            // (`-frames:v`), and is not waited on past that.
            if a + drawn >= b {
                break 'read Ok(());
            }
        }
        if buf.len() > MAX_PENDING {
            break Err("the window's output is not JPEG".to_string());
        }
    };

    // Reap the process, then say how it ended.
    let ended = match &outcome {
        Ok(()) => finish(child),
        Err(_) => {
            kill(child);
            Ok(())
        }
    };
    let mut st = shared.lock();
    if let Some(w) = st.window.as_mut().filter(|w| w.id == id && w.alive()) {
        let text = tail.lock().unwrap_or_else(PoisonError::into_inner).clone();
        w.life = match (outcome, ended) {
            (Ok(()), Ok(())) if drawn > 0 => Life::Done,
            (Ok(()), Ok(())) => Life::Failed("no frames".to_string()),
            (Ok(()), Err(why)) => Life::Failed(format!("{why} {text}").trim().to_string()),
            (Err(why), _) => Life::Failed(why),
        };
        if let Life::Failed(why) = &w.life {
            tracing::debug!(%why, drawn, "a scrub window ended");
        }
    }
    drop(st);
    shared.changed.notify_all();
}

/// The most a window's reader holds without finding a whole JPEG in it.
const MAX_PENDING: usize = 64 << 20;

/// How long a process that has closed its output is given to exit.
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// Wait for a process whose output has ended, killing it if it does not exit; its status as a
/// `Result`. The child's lock is held only for each poll.
fn finish(child: &Arc<Mutex<Child>>) -> std::result::Result<(), String> {
    let deadline = Instant::now() + EXIT_GRACE;
    loop {
        let mut c = child.lock().unwrap_or_else(PoisonError::into_inner);
        match c.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("ffmpeg exited {status}")),
            Ok(None) if Instant::now() >= deadline => {
                let _ = c.kill();
                let _ = c.wait();
                return Err("ffmpeg did not exit".to_string());
            }
            Ok(None) => {}
            Err(e) => return Err(format!("waiting for ffmpeg: {e}")),
        }
        drop(c);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Kill a process and collect it. Waits only for the kill to land.
fn kill(child: &Arc<Mutex<Child>>) {
    let mut c = child.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = c.kill();
    let _ = c.wait();
}

/// [`kill`] on a thread of its own, for callers that must not wait (a process stuck in a driver
/// call takes its time dying, and the callers hold the state's lock or answer a request).
fn reap_later(child: Arc<Mutex<Child>>) {
    let killer = Arc::clone(&child);
    if std::thread::Builder::new()
        .name("kerf-scrub-kill".into())
        .spawn(move || kill(&killer))
        .is_err()
    {
        kill(&child);
    }
}

/// How long the reaper sleeps when no window is alive: it is woken when one starts, and this only
/// bounds a wake-up that was missed.
const REAPER_IDLE: Duration = Duration::from_secs(5);

/// Kills a window that has stopped drawing: nothing for `first_frame_timeout`, or a gap of
/// `frame_timeout` between frames. Parked on the state's condvar while no window is alive and, with
/// one, until the moment it would be late (every frame it files wakes the reaper to move that
/// moment). Holds only a `Weak` between rounds: it ends with the last handle.
fn reaper(weak: &Weak<Shared>) {
    loop {
        let Some(shared) = weak.upgrade() else { return };
        let victim = {
            let mut st = shared.lock();
            if st.closed {
                return;
            }
            let due = st.window.as_ref().filter(|w| w.alive()).map(|w| {
                if w.head == w.a {
                    w.started + shared.config.first_frame_timeout
                } else {
                    w.last_progress + shared.config.frame_timeout
                }
            });
            let now = Instant::now();
            match due {
                Some(due) if due <= now => {
                    let id = st.window.as_ref().map(|w| w.id);
                    id.and_then(|id| st.fail_window(id, "the window stopped drawing"))
                }
                other => {
                    let wait = other.map_or(REAPER_IDLE, |due| due - now);
                    drop(shared.changed.wait_timeout(st, wait).unwrap_or_else(PoisonError::into_inner));
                    None
                }
            }
        };
        if let Some(child) = victim {
            tracing::warn!("a scrub window stopped drawing; killed");
            shared.changed.notify_all();
            reap_later(child);
        }
        // The strong handle is dropped here, so a parked reaper never keeps the cache alive for
        // longer than one wait.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_puts_a_time_in_the_frame_that_holds_it() {
        let g = Grid::new(30.0);
        assert_eq!(g.frame_of(0.0), 0);
        assert_eq!(g.frame_of(-3.0), 0, "before the start is the first frame");
        assert_eq!(g.frame_of(1.0), 30);
        assert_eq!(g.frame_of(1.0 + 0.5 / 30.0), 30, "half way into a frame is still that frame");
        assert_eq!(g.frame_of(1.0 + 0.999 / 30.0), 30);
        assert_eq!(g.frame_of(31.0 / 30.0), 31);
        assert_eq!(g.time_of(30), 1.0);
        for k in [0u64, 1, 29, 30, 1799, 54_000] {
            assert_eq!(g.frame_of(g.time_of(k)), k, "a frame's own start time is in it");
        }
    }

    #[test]
    fn a_fractional_rate_is_counted_on_its_rational() {
        let g = Grid::new(29.97);
        for k in [0u64, 1, 2, 100, 1000, 100_000] {
            assert_eq!(g.frame_of(g.time_of(k)), k, "frame {k}");
        }
        // 2997/100: a hundred seconds is 2997 frames, not the 3000 of 30 fps.
        assert!((g.time_of(2997) - 100.0).abs() < 1e-6);
    }

    #[test]
    fn a_rate_that_is_not_a_rate_is_thirty() {
        for bad in [0.0, -24.0, f64::NAN, f64::INFINITY] {
            assert_eq!(Grid::new(bad).fps, 30.0);
        }
    }

    #[test]
    fn a_time_that_is_not_a_time_is_a_frame_all_the_same() {
        let g = Grid::new(25.0);
        assert_eq!(g.frame_of(f64::NAN), 0);
        assert!(g.frame_of(f64::INFINITY) <= MAX_FRAME);
        assert!(g.time_of(g.frame_of(1e300)).is_finite());
    }

    #[test]
    fn a_direction_is_a_step_and_not_a_jump() {
        assert_eq!(direction(None, 10, 24), None, "the first request has none");
        assert_eq!(direction(Some(10), 10, 24), None, "the same frame again has none");
        assert_eq!(direction(Some(10), 11, 24), Some(true));
        assert_eq!(direction(Some(10), 9, 24), Some(false));
        assert_eq!(direction(Some(10), 34, 24), Some(true), "up to the reach is a step");
        assert_eq!(direction(Some(10), 35, 24), None, "past it is a jump");
        assert_eq!(direction(Some(500), 0, 24), None);
    }

    fn offer(alive: bool, related: bool, b: u64, head: u64) -> Option<Offer> {
        Some(Offer {
            id: 7,
            alive,
            related,
            b,
            head,
        })
    }

    #[test]
    fn a_frame_the_running_window_will_draw_is_waited_for() {
        let w = offer(true, true, 124, 105);
        assert!(waits_for(w, 105, 12, None), "the next one");
        assert!(waits_for(w, 117, 12, None), "twelve ahead");
        assert!(!waits_for(w, 118, 12, None), "further than a window of its own costs");
        assert!(waits_for(w, 123, 20, None), "the last frame of it");
        assert!(!waits_for(w, 124, 20, None), "one past its end");
    }

    #[test]
    fn a_window_this_request_started_is_waited_for_however_far_off_its_frame_is() {
        // A window going back starts 22 frames before the frame it is for.
        let w = offer(true, true, 127, 105);
        assert!(waits_for(w, 126, 12, Some(7)));
        assert!(!waits_for(w, 126, 12, Some(6)), "another request's is judged by the distance");
    }

    #[test]
    fn a_window_that_cannot_help_is_not_waited_for() {
        // Already drawn past: the request would have found it in the cache, so it was evicted.
        assert!(!waits_for(offer(true, true, 124, 110), 105, 12, None));
        // Another cut (an edit moved this frame): not for this request.
        assert!(!waits_for(offer(true, false, 124, 105), 106, 12, None));
        // Finished or failed: nothing to wait for.
        assert!(!waits_for(offer(false, true, 124, 124), 110, 12, None));
        assert!(!waits_for(None, 110, 12, None));
        assert!(!waits_for(offer(false, true, 124, 105), 110, 12, Some(7)));
    }

    #[test]
    fn a_scrub_nearing_the_end_of_a_window_is_near_its_edge() {
        // Frames 100..124 drawn.
        assert!(near_edge(100, 124, 116, true, 8), "eight from the end");
        assert!(near_edge(100, 124, 123, true, 8));
        assert!(!near_edge(100, 124, 115, true, 8), "not yet");
        assert!(!near_edge(100, 124, 124, true, 8), "past it is not in it");
        assert!(!near_edge(100, 124, 99, true, 8));
    }

    #[test]
    fn a_scrub_going_back_is_near_the_start_of_a_window() {
        assert!(near_edge(100, 124, 108, false, 8));
        assert!(!near_edge(100, 124, 109, false, 8));
        assert!(!near_edge(0, 24, 3, false, 8), "there is nothing before frame 0");
    }

    // ---- the cut: what a window may draw -------------------------------------------------

    use crate::engine::test_support::{image_stream, make_clip, test_asset, timeline_of, video_stream, video_track};
    use crate::model::{Projection, Reframe, TextOverlay, Transition, TransitionKind};

    fn video(fps: f64) -> Asset {
        let mut a = test_asset(vec![video_stream(1280, 720, fps)]);
        a.path = format!("/media/clip-{fps}.mp4");
        a
    }

    fn snap_of<'a>(timeline: &Timeline, assets: &'a [Asset]) -> Snapshot<'a> {
        Snapshot::new(timeline, assets, 960, 4)
    }

    fn one_clip(asset: &Asset, edit: impl FnOnce(&mut crate::model::Clip)) -> Timeline {
        let mut clip = make_clip(asset.id, 1.0, 11.0, 0.0);
        edit(&mut clip);
        timeline_of(vec![video_track(vec![clip])])
    }

    #[test]
    fn a_clip_at_the_cuts_rate_is_plain_from_its_first_frame_to_its_last() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        for k in [0, 1, 150, 299] {
            assert!(snap.plain(k), "frame {k}");
        }
    }

    #[test]
    fn a_source_a_whole_multiple_of_the_cuts_rate_is_plain_and_a_fraction_of_it_is_not() {
        // The cut's rate is the first clip's, so the second clip is the one that varies.
        let base = video(30.0);
        let plain = |other: f64, speed: f64| {
            let b = Asset {
                id: uuid::Uuid::new_v4(),
                ..video(other)
            };
            let mut clip = make_clip(b.id, 1.0, 11.0, 1.0);
            clip.speed = speed;
            let timeline = timeline_of(vec![video_track(vec![make_clip(base.id, 0.0, 1.0, 0.0), clip])]);
            let assets = vec![base.clone(), b];
            snap_of(&timeline, &assets).plain(40)
        };
        assert!(plain(30.0, 1.0), "the cut's own rate");
        assert!(plain(60.0, 1.0), "twice it");
        assert!(plain(120.0, 1.0));
        assert!(plain(29.97, 1.0), "29.97 in a 30 fps cut drifts a frame in half a minute");
        assert!(plain(30.0, 2.0), "a 2x clip drops every other frame");
        assert!(plain(60.0, 1.0));
        assert!(!plain(24.0, 1.0), "24 in 30: the pick follows the slice");
        assert!(!plain(25.0, 1.0));
        assert!(!plain(30.0, 1.5));
        assert!(!plain(30.0, 0.5), "half speed repeats frames");
        assert!(!plain(15.0, 1.0));
        assert!(!plain(50.0, 1.0));
    }

    #[test]
    fn a_still_image_has_no_rate_to_disagree_with() {
        let base = video(30.0);
        let mut image = test_asset(vec![image_stream(800, 600)]);
        image.path = "/media/title.png".into();
        let mut sticker = make_clip(image.id, 0.0, 5.0, 0.0);
        sticker.transform.scale = 0.4;
        let mut second = video_track(vec![sticker]);
        second.name = "V2".into();
        let timeline = timeline_of(vec![video_track(vec![make_clip(base.id, 0.0, 10.0, 0.0)]), second]);
        let assets = vec![base, image];
        assert!(snap_of(&timeline, &assets).plain(20));
    }

    #[test]
    fn a_video_with_no_recorded_rate_cannot_be_told_and_is_not_plain() {
        let mut a = video(30.0);
        a.streams[0].fps = None;
        let assets = vec![a.clone()];
        assert!(!snap_of(&one_clip(&a, |_| {}), &assets).plain(20));
    }

    #[test]
    fn frames_inside_a_fade_a_dissolve_and_a_tail_are_not_plain() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        // A fade-in of half a second: frames 0..15.
        let faded = snap_of(&one_clip(&a, |c| c.fade_in = 0.5), &assets);
        assert!((0..15).all(|k| !faded.plain(k)), "inside the fade-in");
        assert!((16..60).all(|k| faded.plain(k)), "and outside it");
        // A fade-out of half a second at the end of a 10 s clip: frames 285..300.
        let fading = snap_of(&one_clip(&a, |c| c.fade_out = 0.5), &assets);
        assert!((286..299).all(|k| !fading.plain(k)));
        assert!((0..280).all(|k| fading.plain(k)));
        // A dissolve between two clips at frame 30, half a second long.
        let mut second = make_clip(a.id, 20.0, 30.0, 1.0);
        second.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        let timeline = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0), second])]);
        let dissolve = snap_of(&timeline, &assets);
        let hazard: Vec<u64> = (0..90).filter(|k| !dissolve.plain(*k)).collect();
        assert_eq!(
            hazard.first(),
            Some(&30),
            "from the frame the incoming clip starts: {hazard:?}"
        );
        assert!(
            hazard.len() >= 15 && hazard.iter().all(|k| (30..46).contains(k)),
            "{hazard:?}"
        );
    }

    #[test]
    fn a_slide_is_not_plain_while_it_travels() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let mut second = make_clip(a.id, 20.0, 30.0, 1.0);
        second.transition_in = Some(Transition {
            kind: TransitionKind::SlideLeft,
            duration: 0.5,
        });
        let timeline = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0), second])]);
        let snap = snap_of(&timeline, &assets);
        assert!(!snap.plain(35), "mid-slide");
        assert!(snap.plain(10) && snap.plain(60));
    }

    #[test]
    fn the_last_frame_of_a_title_is_not_plain_because_the_window_draws_it_and_the_still_does_not() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let mut timeline = one_clip(&a, |_| {});
        timeline.overlays.push(TextOverlay::new("hello", 1.0, 2.0));
        let snap = snap_of(&timeline, &assets);
        assert!(snap.plain(29) && snap.plain(30), "its first frame is drawn by both");
        assert!(snap.plain(59), "the one before the last too");
        assert!(!snap.plain(60), "at its end");
        assert!(snap.plain(61));
    }

    #[test]
    fn a_360_reframe_and_an_hdr_original_are_never_plain() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let reframed = one_clip(&a, |c| c.reframe = Some(Reframe::new(Projection::Equirect)));
        assert!(!snap_of(&reframed, &assets).plain(10));

        let mut hdr = video(30.0);
        hdr.streams[0].color_transfer = Some("arib-std-b67".to_string());
        let assets = vec![hdr.clone()];
        assert!(!snap_of(&one_clip(&hdr, |_| {}), &assets).plain(10));
    }

    #[test]
    fn a_clip_whose_asset_is_missing_is_not_plain() {
        let a = video(30.0);
        let timeline = one_clip(&a, |_| {});
        assert!(!snap_of(&timeline, &[]).plain(10));
    }

    #[test]
    fn a_gap_is_plain_and_a_window_over_one_is_not_refused_for_it() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let timeline = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 5.0, 1.0)])]);
        let snap = snap_of(&timeline, &assets);
        assert!(snap.plain(0) && snap.plain(29), "black before the clip");
        assert!(snap.window_range(10, false, &cfg()).is_some());
    }

    fn cfg() -> ScrubConfig {
        ScrubConfig {
            window_frames: 24,
            lead_frames: 2,
            ..ScrubConfig::default()
        }
    }

    #[test]
    fn a_window_going_forward_starts_a_little_before_the_frame_and_runs_on_to_its_length() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        assert_eq!(
            snap.window_range(100, false, &cfg()),
            Some((98, 122)),
            "two before, 24 in all"
        );
        assert_eq!(snap.window_range(1, false, &cfg()), Some((0, 24)), "not before frame 0");
        assert_eq!(snap.window_range(0, false, &cfg()), Some((0, 24)));
    }

    #[test]
    fn a_window_going_back_ends_just_after_the_frame() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        assert_eq!(snap.window_range(100, true, &cfg()), Some((78, 102)));
        assert_eq!(
            snap.window_range(5, true, &cfg()),
            Some((0, 24)),
            "held to the start of the cut"
        );
    }

    #[test]
    fn a_window_starts_a_frame_early_over_an_original_too_so_a_click_then_a_step_back_is_a_hit() {
        // Not a proxy by its name: a camera file.
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        let c = ScrubConfig { lead_frames: 1, ..cfg() };
        assert_eq!(snap.window_range(100, false, &c), Some((99, 123)));
        assert_eq!(snap.window_range(0, false, &c), Some((0, 24)), "not before the first frame");
    }

    #[test]
    fn a_step_back_has_the_window_behind_it_over_an_original_as_over_a_proxy() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        assert_eq!(snap.window_range(100, true, &cfg()), Some((78, 102)));
    }

    #[test]
    fn a_window_stops_at_the_first_frame_it_may_not_draw() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let mut second = make_clip(a.id, 20.0, 30.0, 1.0);
        second.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        let timeline = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0), second])]);
        let snap = snap_of(&timeline, &assets);
        assert_eq!(
            snap.window_range(25, false, &cfg()),
            Some((23, 30)),
            "ends where the dissolve begins"
        );
        assert_eq!(snap.window_range(33, false, &cfg()), None, "a frame inside it is drawn alone");
        let (lo, hi) = snap.window_range(50, true, &cfg()).expect("a window after it");
        assert!(
            lo >= 45 && hi - lo >= 2,
            "starts after the dissolve (frames 30..45): {lo}..{hi}"
        );
        // The one frame before the dissolve has too few neighbours to be worth a process.
        assert_eq!(snap.window_range(29, false, &cfg()), Some((27, 30)));
    }

    #[test]
    fn a_window_of_one_frame_is_not_a_window() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let mut fade = one_clip(&a, |c| c.fade_in = 0.5);
        fade.tracks[0].clips[0].fade_out = 0.0;
        let snap = snap_of(&fade, &assets);
        // Frame 15 is the first plain frame; its neighbour 14 is not, and 16 is.
        assert!(snap
            .window_range(15, false, &cfg())
            .is_some_and(|(a, b)| a == 15 && b - a >= 2));
        let off = ScrubConfig {
            window_frames: 1,
            ..cfg()
        };
        assert_eq!(snap.window_range(100, false, &off), None, "windows off");
        let none = ScrubConfig {
            window_frames: 0,
            ..cfg()
        };
        assert_eq!(snap.window_range(100, false, &none), None);
    }

    #[test]
    fn the_window_that_carries_on_starts_where_the_last_ended() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        assert_eq!(snap.window_beyond(122, true, &cfg()), Some((122, 146)));
        assert_eq!(
            snap.window_beyond(98, false, &cfg()),
            Some((74, 98)),
            "going back ends where the last began"
        );
        assert_eq!(snap.window_beyond(10, false, &cfg()), Some((0, 10)));
        assert_eq!(snap.window_beyond(0, false, &cfg()), None, "nothing before the first frame");
    }

    #[test]
    fn a_window_is_the_export_graph_over_a_slice_for_the_frames_asked() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        let args = snap.window_args(98, 122, None).unwrap();
        let text = args.join(" ");
        assert!(text.starts_with("-hide_banner -nostats -nostdin -loglevel error"), "{text}");
        assert!(text.contains("-frames:v 24 -f image2pipe pipe:1"), "{text}");
        assert!(text.contains("-c:v mjpeg -q:v 4"), "the still's quality: {text}");
        assert!(text.contains(&format!("-i {}", a.path)));
        // 98 / 30 s into the cut is 1 + 3.2667 s into the source: one input, seeked to its whole second.
        assert_eq!(args.iter().filter(|x| *x == "-i").count(), 1);
        assert!(text.contains("scale=960:540"), "the still's size: {text}");
        assert!(!text.contains("-hwaccel"), "no hardware decode was asked for");
        let hw = snap.window_args(98, 122, Some("auto".to_string())).unwrap().join(" ");
        assert!(hw.contains("-hwaccel auto"), "the spawn can ask for it: {hw}");
    }

    #[test]
    fn a_window_reads_only_the_clips_in_its_slice() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        // Five clips of the same file one after another, each ten seconds from a different place.
        let clips: Vec<_> = (0..5)
            .map(|i| make_clip(a.id, f64::from(i) * 10.0, f64::from(i) * 10.0 + 10.0, f64::from(i) * 10.0))
            .collect();
        let snap = snap_of(&timeline_of(vec![video_track(clips)]), &assets);
        let args = snap.window_args(310, 334, None).unwrap();
        assert_eq!(
            args.iter().filter(|x| *x == "-i").count(),
            1,
            "frames 310..334 are in the second clip only"
        );
    }

    #[test]
    fn a_window_past_the_end_of_the_cut_or_over_no_video_is_an_error_not_a_process() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        assert!(
            snap.window_args(300, 324, None).is_err(),
            "the cut is 10 s: frame 300 is its end"
        );
        assert!(snap.window_args(5000, 5024, None).is_err());
        let empty = snap_of(&timeline_of(vec![video_track(vec![])]), &[]);
        assert!(empty.window_args(0, 24, None).is_err());
    }

    // ---- the key: what makes a cached frame the frame asked for --------------------------

    #[test]
    fn a_frame_has_one_key_and_a_neighbour_has_another() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        assert_eq!(snap.key(100).unwrap(), snap.key(100).unwrap());
        assert_ne!(snap.key(100).unwrap(), snap.key(101).unwrap());
    }

    #[test]
    fn an_edit_that_changes_the_frame_changes_its_key_and_one_that_does_not_leaves_it() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let before = snap_of(&one_clip(&a, |_| {}), &assets);
        let mut cut = one_clip(&a, |_| {});
        let key = before.key(100).unwrap();

        // Every kind of edit that reaches the picture.
        let edits: [fn(&mut crate::model::Clip); 7] = [
            |c| c.color.brightness = 0.2,
            |c| c.transform.scale = 0.5,
            |c| c.transform.pos_x = 0.1,
            |c| c.source_in += 0.5,
            |c| c.effects.push(crate::model::VideoEffect::Blur { sigma: 2.0 }),
            |c| c.speed = 2.0,
            |c| c.timeline_start += 0.5,
        ];
        for edit in edits {
            let mut edited = cut.clone();
            edit(&mut edited.tracks[0].clips[0]);
            assert_ne!(
                snap_of(&edited, &assets).key(100).unwrap(),
                key,
                "an edit did not move the key"
            );
        }

        // A title over the frame, a muted track and a disabled clip are edits too.
        let mut titled = cut.clone();
        titled.overlays.push(TextOverlay::new("on screen", 0.0, 20.0));
        assert_ne!(snap_of(&titled, &assets).key(100).unwrap(), key);
        let mut off = cut.clone();
        off.tracks[0].clips[0].enabled = false;
        assert_ne!(snap_of(&off, &assets).key(100).unwrap(), key);

        // Not edits to this frame: a clip elsewhere, the markers, the master bus, a track's name.
        cut.tracks[0].name = "Main".into();
        cut.master.volume = 0.5;
        assert_eq!(
            snap_of(&cut, &assets).key(100).unwrap(),
            key,
            "audio and names are not the picture"
        );
    }

    #[test]
    fn an_edit_to_another_part_of_the_cut_leaves_a_frame_cached() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let clips = vec![make_clip(a.id, 0.0, 5.0, 0.0), make_clip(a.id, 10.0, 15.0, 5.0)];
        let cut = timeline_of(vec![video_track(clips)]);
        let key = snap_of(&cut, &assets).key(30).unwrap();
        let mut graded_later = cut.clone();
        graded_later.tracks[0].clips[1].color.contrast = 1.4;
        assert_eq!(
            snap_of(&graded_later, &assets).key(30).unwrap(),
            key,
            "grading the second clip does not touch a frame of the first"
        );
        assert_ne!(
            snap_of(&graded_later, &assets).key(200).unwrap(),
            snap_of(&cut, &assets).key(200).unwrap(),
            "but it does touch its own"
        );
    }

    #[test]
    fn a_proxy_that_lands_a_size_and_a_quality_are_other_frames() {
        let a = video(30.0);
        let cut = one_clip(&a, |_| {});
        let key = snap_of(&cut, std::slice::from_ref(&a)).key(100).unwrap();
        let proxy = Asset {
            path: "/cache/kerf/proxies/abc.mp4".into(),
            ..a.clone()
        };
        assert_ne!(snap_of(&cut, &[proxy]).key(100).unwrap(), key, "the input path is in the key");
        assert_ne!(
            Snapshot::new(&cut, std::slice::from_ref(&a), 640, 4).key(100).unwrap(),
            key,
            "another width"
        );
        assert_ne!(
            Snapshot::new(&cut, std::slice::from_ref(&a), 960, 2).key(100).unwrap(),
            key,
            "another quality"
        );
    }

    #[test]
    fn a_file_replaced_under_the_same_name_is_another_frame() {
        let dir = std::env::temp_dir().join(format!("kerf-scrub-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        std::fs::write(&path, b"first take").unwrap();
        let mut a = video(30.0);
        a.path = path.to_string_lossy().into_owned();
        let assets = vec![a.clone()];
        let cut = one_clip(&a, |_| {});
        let before = snap_of(&cut, &assets).key(100).unwrap();
        std::fs::write(&path, b"second take, longer").unwrap();
        assert_ne!(snap_of(&cut, &assets).key(100).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hardware_decode_is_not_in_the_key_and_a_missing_asset_is_an_error() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let cut = one_clip(&a, |_| {});
        let snap = snap_of(&cut, &assets);
        assert!(!snap.still_args(100).unwrap().iter().any(|x| x == "-hwaccel"));
        assert!(snap_of(&cut, &[]).key(100).is_err(), "the same error the one-shot gives");
    }

    #[test]
    fn the_still_a_frame_is_keyed_on_is_the_still_at_the_start_of_its_slot() {
        let a = video(30.0);
        let assets = vec![a.clone()];
        let snap = snap_of(&one_clip(&a, |_| {}), &assets);
        let args = snap.still_args(100).unwrap();
        let ss = args.iter().position(|x| x == "-ss").unwrap();
        // Frame 100 starts at 3.333333 s of the cut; the clip's source_in is 1.
        assert_eq!(args[ss + 1], "4.333333");
    }

    #[test]
    fn the_defaults_are_bounded() {
        let c = ScrubConfig::default();
        assert!(c.cache_bytes >= 1 << 20 && c.cache_bytes <= 1 << 30);
        assert!(c.cache_entries >= 16);
        assert!(c.window_frames <= 120);
        assert!(c.first_frame_timeout > c.frame_timeout);
        assert!(c.first_frame_timeout <= Duration::from_secs(60), "nothing waits for ever");
        assert!(c.request_timeout <= Duration::from_secs(60));
    }
}
