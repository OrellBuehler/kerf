//! `FrameSource`: decoded frames for a plan's layers from **long-lived `ffmpeg` runs** and a
//! frame cache, instead of one spawn per frame (A1b-2; design `.claude/plans/a1-design.md` §1).
//!
//! A **run** is one `ffmpeg` decoding a file forward from a seek ([`run_args`]): every frame it
//! writes to its `yuv4mpegpipe` stdout is paired, by number, with the timestamp `showinfo` prints
//! for it on stderr, and goes into the [`FrameCache`] under `(file identity, pts in ticks)`. A
//! request is answered from the cache when the cache can *prove* the answer (`covers_from`), and
//! otherwise [`route`]d: read an existing run forward, start one, restart one, or decode a
//! one-shot frame — the pure rules are `router`'s, this module only does what they say:
//!
//! * **Picks.** `Pick::AtOrAfter(t)` is the first frame at or after [`seek_ticks`]`(t, tb)` —
//!   what the still's `-ss` returns, so a frame from a run is the frame [`decode_layer`] would
//!   have decoded, byte for byte (the parity harness checks every case both ways). `Before` and
//!   `Fps` are a cursor's (A1b-3) and come back `Unsupported`; a still image is decoded once,
//!   without a seek, and cached. The time base is learned from a file's first run; until then a
//!   request can only start one.
//! * **A run's first frame covers from its seek tick**, clamped to one frame interval before it
//!   (an `mpegts` seek can land a GOP late and must not claim what it skipped); every later frame
//!   covers from the tick after the one before it. A run that ends cleanly records the file's end
//!   ([`FrameCache::mark_end`]), so a time past the last frame is `Ok(None)` without a decode, as
//!   FFmpeg's still draws nothing there; a clean run that wrote **no** frame is a seek past the
//!   end (`mark_end(seek_tick - 1)`), and a run that exits non-zero without a frame is a failed
//!   start, never an end.
//! * **The file identity is captured at spawn** and carried by the run, so frames decoded from a
//!   file replaced mid-run are never filed under the new file's identity.
//! * **Nothing waits forever.** The reaper thread kills a run that is wanted but silent for
//!   [`FrameSourceConfig::first_frame_timeout`] (no frame yet) or
//!   [`FrameSourceConfig::frame_timeout`] (between frames) and one idle for
//!   [`FrameSourceConfig::idle_kill`]; a request gives up after
//!   [`FrameSourceConfig::request_timeout`] (`Exact`: the one-shot's own 30 s). A run reads
//!   ahead only as far as it was asked (plus a byte-bounded read-ahead for `Forward`): past that
//!   it blocks, the pipe fills and `ffmpeg` idles.
//! * **Hardware decode** is `decode_hwaccel()`'s; a run that dies before its first frame with it
//!   is retried once in software, and if that works the process stops asking
//!   (`disable_decode_hwaccel`).
//! * **The path can be off.** The first use runs a **self-test** ([`self_test`]): a tiny file on
//!   a fine time base made with `ffmpeg`'s own encoder, decoded with the production flags, every
//!   pts expected exactly. If it fails (an FFmpeg older than 5.1 rejects `-fps_mode` and
//!   `showinfo=checksum`, a build that prints `showinfo` differently) the whole process falls
//!   back to [`decode_layer`] — one spawn per frame, A0's path — rather than trust timestamps it
//!   cannot read. `KERF_FRAME_SOURCE=oneshot` does the same on purpose.
//! * **Errors are per frame**: `Unsupported`, `Decode`, a timeout or `Busy` (the thrash guard)
//!   mean the caller renders *that* frame through FFmpeg; `Ok(None)` is "no frame".
//!
//! Interactive role only (A2–A6): normal priority, no `cpu::lease`, the CPU budget's thread cap
//! divided among the runs alive at spawn. The export role (A7) is a `FrameCursor`'s.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use kerf_core::{seek_ticks, Pick, PlanLayer, Rational};

use crate::frame_cache::{CacheStats, FrameCache, FrameKey, Lookup, SourceId, DEFAULT_CAP_BYTES};
use crate::gpu::GpuError;
use crate::router::{route, Intent, Request, Route, RunId, RunState, StartOutcome, ThrashGuard, Ticket};
use crate::showinfo::{plain_log_env, ShowFrame, ShowinfoParser};
use crate::source::{decode_layer, yuv420p_len, YuvFrame, DECODE_TIMEOUT, MAX_SIDE};
use crate::y4m::Y4mReader;

/// How a frame is wanted (the router's [`Intent`], from the caller's point of view).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hint {
    /// One frame to look at (an agent's `preview_timeline`): never takes a run from anyone.
    Exact,
    /// The playhead is being dragged.
    Scrub,
    /// Playback at `fps`: frame after frame, read ahead.
    Forward { fps: f64 },
}

impl Hint {
    fn intent(self) -> Intent {
        match self {
            Hint::Exact => Intent::Exact,
            Hint::Scrub => Intent::Scrub,
            Hint::Forward { .. } => Intent::Forward,
        }
    }
}

/// Limits and deadlines. [`Default`] is the design's numbers (and `KERF_FRAME_CACHE_MB`).
#[derive(Debug, Clone)]
pub struct FrameSourceConfig {
    /// The frame cache's cap in bytes.
    pub cache_bytes: usize,
    /// A run nobody has used for this long is stopped.
    pub idle_kill: Duration,
    /// A run that is wanted and has produced no frame for this long is killed.
    pub first_frame_timeout: Duration,
    /// The same between frames.
    pub frame_timeout: Duration,
    /// How long a `Scrub` / `Forward` request waits before the caller falls back.
    pub request_timeout: Duration,
    /// How far ahead a `Forward` run may read, in bytes of frames.
    pub read_ahead_bytes: usize,
    /// One-shot decodes (`Exact` requests no run can serve) at once.
    pub max_oneshots: usize,
}

impl Default for FrameSourceConfig {
    fn default() -> Self {
        let cache_bytes = std::env::var("KERF_FRAME_CACHE_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&mb| mb > 0)
            .map_or(DEFAULT_CAP_BYTES, |mb| mb.saturating_mul(1 << 20));
        Self {
            cache_bytes,
            idle_kill: Duration::from_secs(20),
            first_frame_timeout: Duration::from_secs(30),
            frame_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(5),
            read_ahead_bytes: 48 << 20,
            max_oneshots: 2,
        }
    }
}

/// What the source has done, for logs and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    pub cache: CacheStats,
    /// Runs alive now.
    pub runs: usize,
    /// Runs started, in all.
    pub spawned: u64,
    /// Starts that stopped a live run to take its place.
    pub replaced: u64,
    /// Requests served by reading a run forward.
    pub reused: u64,
    /// One-shot decodes (an `Exact` no run could serve, a disabled path, an unprobed format).
    pub oneshots: u64,
    /// Requests that gave up on runs and decoded one-shot after repeated misses.
    pub fallbacks: u64,
}

/// What became of a run.
#[derive(Debug, Clone, PartialEq)]
enum Life {
    /// Spawned, no frame yet.
    Starting,
    Streaming,
    /// Its stdout ended and it exited 0.
    Ended,
    /// It died, was killed, or could not be read.
    Failed(String),
}

struct Run {
    id: RunId,
    /// The identity of the file when the run was spawned.
    file: SourceId,
    /// The seek, in seconds, and its tick once the time base is known.
    seek: f64,
    seek_tick: Option<i64>,
    /// The tick just past the last frame produced (the seek tick before the first).
    head: i64,
    /// Produce frames until one at or past this tick has been read.
    want: i64,
    /// Frames before this tick were only passed: they go in cold.
    warm_from: i64,
    life: Life,
    frames: u64,
    hw: bool,
    waiters: u32,
    last_used: Instant,
    last_progress: Instant,
    ticket: Option<Ticket>,
    child: Arc<Mutex<Child>>,
    ended_at: Option<Instant>,
}

impl Run {
    fn alive(&self) -> bool {
        matches!(self.life, Life::Starting | Life::Streaming)
    }
}

/// What a file's runs taught: its time base, the ticks in one frame, and whether its seeks
/// land late.
#[derive(Debug, Clone, Copy)]
struct FileInfo {
    time_base: Rational,
    frame_ticks: i64,
    /// A run's first frame came more than two frame intervals after its seek: the demuxer seeks
    /// to a later keyframe and the decode does not recover (a long-GOP transport stream, finding
    /// 5 of the design note). What `-ss t` returns is then not a function of the timestamps, so
    /// the file's frames come from A0's decode — the same seek the FFmpeg still makes.
    late_seek: bool,
}

struct State {
    cache: FrameCache,
    runs: Vec<Run>,
    files: HashMap<SourceId, FileInfo>,
    guard: ThrashGuard,
    next_id: u64,
    oneshots: usize,
    stats: SourceStats,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    config: FrameSourceConfig,
    epoch: Instant,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
}

/// Decoded frames for plan layers. See the [module](self). Create one per process with
/// [`FrameSource::new`] and share the `Arc`; every call blocks, so make them from blocking-pool
/// threads, never under the project lock.
pub struct FrameSource {
    shared: Arc<Shared>,
}

/// The `ffmpeg` argv of a run of `path` from `seek` seconds (before the CPU cap): the production
/// flags the self-test checks.
pub fn run_args(path: &str, seek: f64, hwaccel: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-nostdin", "-loglevel", "info"]
        .map(String::from)
        .into();
    if let Some(h) = hwaccel {
        args.extend(["-hwaccel".to_string(), h.to_string()]);
    }
    args.extend(["-copyts", "-start_at_zero", "-ss"].map(String::from));
    args.push(kerf_core::seek_arg(seek.max(0.0)));
    args.extend(["-i".to_string(), path.to_string()]);
    args.extend(
        [
            "-an",
            "-sn",
            "-dn",
            "-map",
            "0:v:0",
            "-vf",
            "showinfo=checksum=0,scale=out_range=tv",
            "-fps_mode",
            "passthrough",
            "-f",
            "yuv4mpegpipe",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ]
        .map(String::from),
    );
    args
}

/// Ticks in one frame at `fps` on `tb`, at least one (one tick when the rate is unknown).
fn frame_ticks(fps: Option<f64>, tb: Rational) -> i64 {
    fps.filter(|f| f.is_finite() && *f > 0.0)
        .map_or(1, |f| (f64::from(tb.den) / (f64::from(tb.num) * f)).round() as i64)
        .max(1)
}

/// Whether the path is on: `KERF_FRAME_SOURCE=oneshot` turns it off, and so does a failed
/// [`self_test`] (once per process).
fn path_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var("KERF_FRAME_SOURCE").is_ok_and(|v| v.eq_ignore_ascii_case("oneshot")) {
            tracing::info!("frame source: one-shot decodes (KERF_FRAME_SOURCE)");
            return false;
        }
        match self_test() {
            Ok(()) => true,
            Err(why) => {
                tracing::warn!("frame source: the run self-test failed, decoding one frame per spawn: {why}");
                false
            }
        }
    })
}

/// Make a tiny clip on a fine time base (`mpeg4` at 30000/1001 in mp4, 1/30000: frame `k` at
/// pts `1001 * k`), decode it with the production [`run_args`] from frame 5 and check that the
/// frames and the timestamps are exactly the ones expected. `Err` names what was wrong.
pub fn self_test() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("kerf-frame-source-{}-{}", std::process::id(), unique()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("a scratch directory: {e}"))?;
    // Every caller of the first frame waits behind this: an ffmpeg that hangs must not hang it.
    let deadline = Instant::now() + SELF_TEST_TIMEOUT;
    let result = (|| {
        let clip = dir.join("selftest.mp4");
        let status = kerf_core::ffmpeg_command()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=size=64x48:rate=30000/1001")
            .args([
                "-frames:v",
                "12",
                "-c:v",
                "mpeg4",
                "-video_track_timescale",
                "30000",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not run ffmpeg: {e}"))
            .and_then(|c| wait_until(&Mutex::new(c), deadline))?;
        if !status.success() {
            return Err(format!("making the test clip: {status}"));
        }
        let mut cmd = kerf_core::ffmpeg_command();
        cmd.args(run_args(&clip.to_string_lossy(), 5.0 * 1001.0 / 30000.0, None));
        let mut child = plain_log_env(&mut cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run ffmpeg: {e}"))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the pipes were not captured".into());
        };
        let child = Arc::new(Mutex::new(child));
        // Reading blocks until the pipes close: the watchdog closes them at the deadline.
        let watched = Arc::clone(&child);
        let watchdog = std::thread::spawn(move || wait_until(&watched, deadline));
        let err = std::thread::spawn(move || {
            let mut parser = ShowinfoParser::new();
            let mut shown = Vec::new();
            for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
                match parser.line_lossy(&line) {
                    Ok(Some(f)) => shown.push(f),
                    Ok(None) => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(shown)
        });
        let mut reader = Y4mReader::new(stdout, (64, 48));
        let mut frames = 0;
        let read = loop {
            match reader.next_frame() {
                Ok(Some(_)) => frames += 1,
                Ok(None) => break Ok(()),
                Err(e) => break Err(e.to_string()),
            }
        };
        let status = watchdog.join().map_err(|_| "the self-test watchdog panicked".to_string())??;
        let shown = err.join().map_err(|_| "the stderr reader panicked".to_string())??;
        read?;
        if !status.success() {
            return Err(format!("the run exited {status}"));
        }
        let pts: Vec<i64> = shown.iter().map(|f| f.pts).collect();
        let expected: Vec<i64> = (5..12).map(|k| 1001 * k).collect();
        let tb = shown.first().map(|f| f.time_base);
        if frames != 7 || pts != expected || tb != Some(Rational { num: 1, den: 30000 }) {
            return Err(format!(
                "{frames} frames with pts {pts:?} on {tb:?}, expected 7 with {expected:?} on 1/30000"
            ));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// How long the self-test may take, both of its ffmpegs together.
const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait for `child` to exit, killing it at `deadline`.
fn wait_until(child: &Mutex<Child>, deadline: Instant) -> Result<std::process::ExitStatus, String> {
    loop {
        let mut c = child.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match c.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = c.kill();
                let _ = c.wait();
                return Err(format!("ffmpeg did not finish within {} s", SELF_TEST_TIMEOUT.as_secs()));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("waiting for ffmpeg: {e}")),
        }
        drop(c);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

impl FrameSource {
    pub fn new(config: FrameSourceConfig) -> Arc<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                cache: FrameCache::new(config.cache_bytes),
                runs: Vec::new(),
                files: HashMap::new(),
                guard: ThrashGuard::new(),
                next_id: 0,
                oneshots: 0,
                stats: SourceStats::default(),
                closed: false,
            }),
            changed: Condvar::new(),
            config,
            epoch: Instant::now(),
        });
        let weak = Arc::downgrade(&shared);
        let _ = std::thread::Builder::new()
            .name("kerf-frame-reaper".into())
            .spawn(move || reaper(&weak));
        Arc::new(Self { shared })
    }

    /// Whether frames come from runs (`false`: one spawn per frame, see [`self_test`]).
    pub fn runs_enabled(&self) -> bool {
        path_enabled()
    }

    /// The frame each layer shows, decoded side by side; `None` where the source has no frame.
    /// Layers asking for the same frame of the same file share one decode.
    pub fn frames(&self, layers: &[PlanLayer], hint: Hint) -> Result<Vec<Option<Arc<YuvFrame>>>, GpuError> {
        // One request per distinct frame: two layers of one shot are one decode.
        let mut distinct: Vec<usize> = Vec::new();
        let mut of: Vec<usize> = Vec::with_capacity(layers.len());
        for l in layers {
            let same = distinct
                .iter()
                .position(|&d| layers[d].path == l.path && layers[d].pick == l.pick && layers[d].is_image == l.is_image);
            of.push(same.unwrap_or_else(|| {
                distinct.push(of.len());
                distinct.len() - 1
            }));
        }
        let decoded: Vec<Result<Option<Arc<YuvFrame>>, GpuError>> = if let [only] = distinct[..] {
            vec![self.frame(&layers[only], hint)]
        } else {
            std::thread::scope(|scope| {
                let handles: Vec<_> = distinct
                    .iter()
                    .map(|&d| scope.spawn(move || self.frame(&layers[d], hint)))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err(GpuError::Decode("a decode thread panicked".into())))
                    })
                    .collect()
            })
        };
        let decoded: Vec<Option<Arc<YuvFrame>>> = decoded.into_iter().collect::<Result<_, _>>()?;
        Ok(of.into_iter().map(|i| decoded[i].clone()).collect())
    }

    /// The frame one layer shows.
    pub fn frame(&self, layer: &PlanLayer, hint: Hint) -> Result<Option<Arc<YuvFrame>>, GpuError> {
        let (w, h) = (layer.stream.width, layer.stream.height);
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(GpuError::Unsupported(format!(
                "{}: a {w}x{h} picture (the compositor takes 1 to {MAX_SIDE} px a side)",
                layer.path
            )));
        }
        let Pick::AtOrAfter(t) = layer.pick else {
            return Err(GpuError::Unsupported(format!(
                "{}: a {:?} pick (a cursor's; the frame source answers AtOrAfter)",
                layer.path, layer.pick
            )));
        };
        // A disabled path, and an asset that never recorded its pixel format (whose alpha only
        // A0's second decode can rule out), decode one-shot.
        if !path_enabled() || layer.stream.pix_fmt.is_none() {
            return self.oneshot(layer);
        }
        if let Some(fmt) = layer.stream.pix_fmt.as_deref() {
            if kerf_core::model::pix_fmt_layout(fmt).is_none() {
                return Err(GpuError::Unsupported(format!(
                    "{}: the pixel format {fmt} is not one known to be opaque (it may carry alpha)",
                    layer.path
                )));
            }
        }
        let source = SourceId::of(Path::new(&layer.path));
        if layer.is_image {
            return self.image(layer, source);
        }
        let frame_bytes = yuv420p_len(w, h).unwrap_or(0);
        let deadline = Instant::now()
            + if hint == Hint::Exact {
                DECODE_TIMEOUT
            } else {
                self.shared.config.request_timeout
            };
        let mut st = self.shared.lock();
        let mut starts = 0;
        // The run this request started or is reading, to read its outcome.
        let mut mine: Option<RunId> = None;
        // A software run started because the hardware one died before its first frame: if it
        // delivers, hardware decode is broken here.
        let mut sw_retry: Option<RunId> = None;
        // Waiting on another request's starting run before the time base was known: once it is,
        // route this target properly instead of reading that run forward however far it is.
        let mut joined_blind = false;
        loop {
            if st.closed {
                return Err(GpuError::Decode("the frame source was released".into()));
            }
            let info = st.files.get(&source).copied();
            if info.is_some_and(|i| i.late_seek) {
                drop(st);
                return self.oneshot(layer);
            }
            let target = info.map(|i| seek_ticks(t, i.time_base));
            if joined_blind && target.is_some() {
                joined_blind = false;
                mine = None;
            }
            if let Some(target) = target {
                match st.cache.at_or_after(source, target) {
                    Lookup::Hit(hit) => {
                        touch(&mut st, mine);
                        if sw_retry.is_some() && sw_retry == mine {
                            kerf_core::disable_decode_hwaccel();
                        }
                        return Ok(Some(hit.frame));
                    }
                    Lookup::PastEnd => return Ok(None),
                    Lookup::Miss => {}
                }
            }
            // What became of the run this request is waiting on.
            if let Some(id) = mine {
                match st.runs.iter().find(|r| r.id == id).map(|r| (r.life.clone(), r.frames, r.hw)) {
                    Some((Life::Ended, 0, _)) => return Ok(None),
                    Some((Life::Failed(why), 0, true)) if starts < 3 => {
                        // Died before its first frame with a hardware decoder: once in software.
                        tracing::debug!(
                            "frame source: {} failed with -hwaccel ({why}); retrying in software",
                            layer.path
                        );
                        let seek = st.runs.iter().find(|r| r.id == id).map_or(t, |r| r.seek);
                        let warm = target;
                        mine = Some(self.spawn(&mut st, layer, source, seek, false, None, info, warm)?);
                        sw_retry = mine;
                        starts += 1;
                        continue;
                    }
                    Some((Life::Failed(why), _, _)) => {
                        return Err(GpuError::Decode(format!("{}: {why}", layer.path)));
                    }
                    Some((Life::Ended, _, _)) | None => mine = None,
                    Some((Life::Starting | Life::Streaming, _, _)) => {
                        // It read past the target and the cache still cannot prove the frame
                        // (a seek that landed late, or a passed frame not kept): this run will
                        // not answer, so route again — an earlier start, or after two, A0's
                        // decode.
                        let passed = target.is_some_and(|tg| st.runs.iter().any(|r| r.id == id && r.frames > 0 && r.head > tg));
                        if passed {
                            mine = None;
                        }
                    }
                }
            }
            if mine.is_none() {
                // A run of this file already heading for the frame (another request's): wait on it.
                let heading = st
                    .runs
                    .iter()
                    .find(|r| {
                        r.file == source
                            && r.alive()
                            && match target {
                                Some(tg) => r.head <= tg && r.want >= tg,
                                None => r.life == Life::Starting,
                            }
                    })
                    .map(|r| r.id);
                if let Some(id) = heading {
                    mine = Some(id);
                    joined_blind = target.is_none();
                } else if starts >= 2 {
                    // Two runs and still no proof (a seek that lands late on every try):
                    // A0's decode is the reference anyway.
                    st.stats.fallbacks += 1;
                    drop(st);
                    return self.oneshot(layer);
                } else {
                    let req = Request {
                        file: source,
                        target: target.unwrap_or(0),
                        frame_ticks: info.map_or(1, |i| i.frame_ticks),
                        proxy: layer.source.proxy,
                        intent: hint.intent(),
                        frame_bytes,
                        cache_cap: self.shared.config.cache_bytes,
                    };
                    let states = run_states(&st);
                    let route = route(&states, &req);
                    let now = self.shared.now();
                    let ticket = st.guard.check(req.intent, &route, now)?;
                    let fps = layer.stream.fps.filter(|f| f.is_finite() && *f > 0.0);
                    let lead_secs = |lead: u32| fps.map_or(0.0, |f| f64::from(lead) / f);
                    let ahead = read_ahead(hint, &self.shared.config, frame_bytes, req.frame_ticks);
                    match route {
                        Route::Reuse(id) => {
                            st.stats.reused += 1;
                            if let (Some(run), Some(tg)) = (st.runs.iter_mut().find(|r| r.id == id), target) {
                                run.want = run.want.max(tg.saturating_add(ahead));
                                run.warm_from = run.warm_from.min(tg);
                            }
                            mine = Some(id);
                            self.shared.changed.notify_all();
                        }
                        Route::Start { evict, lead } => {
                            if let Some(e) = evict {
                                st.stats.replaced += 1;
                                stop(&mut st, e.run);
                            }
                            let warm = target.map(|tg| req.warm_from(&route).min(tg));
                            let hw = kerf_core::decode_hwaccel().is_some();
                            mine = Some(self.spawn(&mut st, layer, source, t - lead_secs(lead), hw, ticket, info, warm)?);
                            want_ahead(&mut st, mine, target, ahead);
                            starts += 1;
                        }
                        Route::Restart { run, lead } => {
                            st.stats.replaced += 1;
                            stop(&mut st, run);
                            let warm = target.map(|tg| req.warm_from(&route).min(tg));
                            let hw = kerf_core::decode_hwaccel().is_some();
                            mine = Some(self.spawn(&mut st, layer, source, t - lead_secs(lead), hw, ticket, info, warm)?);
                            want_ahead(&mut st, mine, target, ahead);
                            starts += 1;
                        }
                        Route::OneShot => {
                            drop(st);
                            return self.oneshot(layer);
                        }
                        Route::Skip | Route::Busy => {
                            return Err(GpuError::Busy(format!("{}: every decode run is in use", layer.path)))
                        }
                    }
                }
            }
            // Wait for the run to move, holding it against eviction meanwhile.
            let now = Instant::now();
            if now >= deadline {
                return Err(GpuError::Decode(format!(
                    "{}: no frame at {t:.6} s within {:.1} s",
                    layer.path,
                    self.shared.config.request_timeout.as_secs_f64()
                )));
            }
            set_waiting(&mut st, mine, 1);
            if let (Some(id), Some(tg)) = (mine, target) {
                if let Some(run) = st.runs.iter_mut().find(|r| r.id == id) {
                    run.want = run.want.max(tg);
                }
            }
            let (back, _) = self
                .shared
                .changed
                .wait_timeout(st, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            st = back;
            set_waiting(&mut st, mine, -1);
        }
    }

    /// Start a run of `layer`'s file at `seek` seconds and register it. `info` is the file's
    /// time base when known; `warm` the tick from which frames are kept warm.
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &self,
        st: &mut State,
        layer: &PlanLayer,
        source: SourceId,
        seek: f64,
        hw: bool,
        ticket: Option<Ticket>,
        info: Option<FileInfo>,
        warm: Option<i64>,
    ) -> Result<RunId, GpuError> {
        let seek = seek.max(0.0);
        let hwaccel = if hw { kerf_core::decode_hwaccel() } else { None };
        let mut args = run_args(&layer.path, seek, hwaccel.as_deref());
        let live = st.runs.iter().filter(|r| r.alive()).count();
        kerf_core::limit_ffmpeg_args(&mut args, live + 1);
        let mut cmd = kerf_core::ffmpeg_command();
        cmd.args(&args);
        let spawned = plain_log_env(&mut cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                st.guard.finished(ticket, self.shared.now(), StartOutcome::Failed);
                return Err(GpuError::Decode(format!("could not run ffmpeg: {e}")));
            }
        };
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            st.guard.finished(ticket, self.shared.now(), StartOutcome::Failed);
            return Err(GpuError::Decode("ffmpeg's pipes were not captured".into()));
        };
        st.next_id += 1;
        let id = RunId(st.next_id);
        st.stats.spawned += 1;
        let seek_tick = info.map(|i| seek_ticks(seek, i.time_base));
        let now = Instant::now();
        let child = Arc::new(Mutex::new(child));
        st.runs.push(Run {
            id,
            file: source,
            seek,
            seek_tick,
            head: seek_tick.unwrap_or(i64::MIN),
            want: warm.unwrap_or(i64::MIN),
            warm_from: warm.unwrap_or(i64::MIN),
            life: Life::Starting,
            frames: 0,
            hw: hwaccel.is_some(),
            waiters: 0,
            last_used: now,
            last_progress: now,
            ticket,
            child: Arc::clone(&child),
            ended_at: None,
        });
        let (tx, rx) = mpsc::channel();
        let tail = Arc::new(Mutex::new(String::new()));
        let err_tail = Arc::clone(&tail);
        let _ = std::thread::Builder::new()
            .name("kerf-frame-stderr".into())
            .spawn(move || read_stderr(stderr, &tx, &err_tail));
        let weak = Arc::downgrade(&self.shared);
        let expected = (layer.stream.width, layer.stream.height);
        let fps = layer.stream.fps;
        let spawned = std::thread::Builder::new()
            .name("kerf-frame-run".into())
            .spawn(move || run_reader(&weak, id, source, stdout, &rx, &tail, expected, fps, &child));
        if spawned.is_err() {
            stop(st, id);
            return Err(GpuError::Decode("could not start a decode thread".into()));
        }
        Ok(id)
    }

    /// A still image: decoded once without a seek, kept under its file.
    fn image(&self, layer: &PlanLayer, source: SourceId) -> Result<Option<Arc<YuvFrame>>, GpuError> {
        let key = FrameKey { source, pts: i64::MIN };
        if let Some(frame) = self.shared.lock().cache.get(key) {
            return Ok(Some(frame));
        }
        let frame = self.oneshot(layer)?;
        if let Some(f) = &frame {
            self.shared.lock().cache.insert(key, Arc::clone(f), i64::MIN);
        }
        Ok(frame)
    }

    /// A0's decode of the one frame, bounded in how many run at once.
    fn oneshot(&self, layer: &PlanLayer) -> Result<Option<Arc<YuvFrame>>, GpuError> {
        {
            let mut st = self.shared.lock();
            while st.oneshots >= self.shared.config.max_oneshots.max(1) {
                st = self
                    .shared
                    .changed
                    .wait(st)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            st.oneshots += 1;
            st.stats.oneshots += 1;
        }
        let decoded = decode_layer(layer);
        self.shared.lock().oneshots -= 1;
        self.shared.changed.notify_all();
        Ok(decoded?.map(Arc::new))
    }

    /// Stop every run of `source` and forget its frames.
    pub fn release(&self, source: SourceId) {
        let mut st = self.shared.lock();
        let ids: Vec<RunId> = st.runs.iter().filter(|r| r.file == source).map(|r| r.id).collect();
        for id in ids {
            stop(&mut st, id);
        }
        st.cache.purge(source);
        st.files.remove(&source);
        self.shared.changed.notify_all();
    }

    /// Stop every run and forget every frame.
    pub fn release_all(&self) {
        let children = {
            let mut st = self.shared.lock();
            st.closed = true;
            let children: Vec<_> = st.runs.drain(..).map(|r| r.child).collect();
            st.cache.clear();
            st.files.clear();
            children
        };
        self.shared.changed.notify_all();
        for child in children {
            kill(&child);
        }
    }

    pub fn stats(&self) -> SourceStats {
        let st = self.shared.lock();
        SourceStats {
            cache: st.cache.stats(),
            runs: st.runs.iter().filter(|r| r.alive()).count(),
            ..st.stats
        }
    }
}

impl Drop for FrameSource {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// The frames a `Forward` run reads beyond the target: `read_ahead_bytes` worth, at most 24.
fn read_ahead(hint: Hint, config: &FrameSourceConfig, frame_bytes: usize, frame_ticks: i64) -> i64 {
    match hint {
        Hint::Forward { .. } if frame_bytes > 0 => {
            let frames = (config.read_ahead_bytes / frame_bytes).clamp(1, 24) as i64;
            frames.saturating_mul(frame_ticks.max(1))
        }
        _ => 0,
    }
}

fn run_states(st: &State) -> Vec<RunState> {
    st.runs
        .iter()
        .filter(|r| r.alive())
        .map(|r| RunState {
            id: r.id,
            file: r.file,
            head: r.head,
            idle: r.last_used.elapsed().as_secs_f64(),
            busy: r.waiters > 0,
        })
        .collect()
}

/// A fresh run reads `ahead` ticks past the target (a `Forward` request's read-ahead).
fn want_ahead(st: &mut State, run: Option<RunId>, target: Option<i64>, ahead: i64) {
    if let (Some(r), Some(tg)) = (run.and_then(|id| st.runs.iter_mut().find(|r| r.id == id)), target) {
        r.want = r.want.max(tg.saturating_add(ahead));
    }
}

fn touch(st: &mut State, run: Option<RunId>) {
    if let Some(r) = run.and_then(|id| st.runs.iter_mut().find(|r| r.id == id)) {
        r.last_used = Instant::now();
    }
}

fn set_waiting(st: &mut State, run: Option<RunId>, by: i32) {
    if let Some(r) = run.and_then(|id| st.runs.iter_mut().find(|r| r.id == id)) {
        r.waiters = r.waiters.saturating_add_signed(by);
        r.last_used = Instant::now();
    }
}

/// Kill a run and drop it from the table (its reader sees the pipe close and exits).
fn stop(st: &mut State, id: RunId) {
    if let Some(at) = st.runs.iter().position(|r| r.id == id) {
        let run = st.runs.remove(at);
        kill(&run.child);
    }
}

fn kill(child: &Mutex<Child>) {
    let mut c = child.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = c.kill();
    let _ = c.wait();
}

/// How much of a run's stderr is kept for its error message.
const STDERR_TAIL: usize = 4096;

/// Parse a run's stderr into `showinfo` frames, keeping a tail of the rest for errors. A line
/// the parser rejects ends the stream of frames with the error; the pipe is drained regardless,
/// so `ffmpeg` never blocks on it.
fn read_stderr(stderr: ChildStderr, tx: &Sender<Result<ShowFrame, String>>, tail: &Mutex<String>) {
    let mut parser = ShowinfoParser::new();
    let mut broken = false;
    for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
        if !broken {
            match parser.line_lossy(&line) {
                Ok(Some(f)) => {
                    if tx.send(Ok(f)).is_err() {
                        broken = true;
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    broken = true;
                }
            }
        }
        let mut t = tail.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        t.push_str(&String::from_utf8_lossy(&line));
        t.push('\n');
        if t.len() > STDERR_TAIL {
            let cut = t.len() - STDERR_TAIL;
            let cut = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(t.len());
            t.drain(..cut);
        }
    }
}

/// The body of a run's reader thread: frames off stdout, timestamps off the stderr channel,
/// both into the cache; then the run's end.
#[allow(clippy::too_many_arguments)]
fn run_reader(
    shared: &Weak<Shared>,
    id: RunId,
    source: SourceId,
    stdout: ChildStdout,
    shown: &Receiver<Result<ShowFrame, String>>,
    tail: &Mutex<String>,
    expected: (u32, u32),
    fps: Option<f64>,
    child: &Mutex<Child>,
) {
    let mut reader = Y4mReader::new(stdout, expected);
    let mut prev_pts: Option<i64> = None;
    let mut last: Option<ShowFrame> = None;
    let outcome: Result<(), String> = loop {
        // Block while nobody wants more of this run (the pipe fills and ffmpeg idles).
        {
            let Some(sh) = shared.upgrade() else { return };
            let mut st = sh.lock();
            loop {
                let Some(run) = st.runs.iter().find(|r| r.id == id) else {
                    return;
                };
                if !run.alive() {
                    return;
                }
                if run.head <= run.want || run.frames == 0 {
                    break;
                }
                st = sh
                    .changed
                    .wait_timeout(st, Duration::from_millis(500))
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        }
        let frame = match reader.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break Ok(()),
            Err(e) => break Err(e.to_string()),
        };
        // Its timestamp: printed before the frame reached the pipe, so it is there or coming.
        let info = match shown.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => break Err(format!("showinfo: {e}")),
            Err(RecvTimeoutError::Timeout) => break Err("no timestamp for a frame within 15 s".into()),
            Err(RecvTimeoutError::Disconnected) => break Err("stderr ended before a frame's timestamp".into()),
        };
        if info.n + 1 != reader.frames_read() {
            break Err(format!(
                "frame {} of the stream was reported as frame {}",
                reader.frames_read() - 1,
                info.n
            ));
        }
        let Some(sh) = shared.upgrade() else { return };
        let mut st = sh.lock();
        let tb = info.time_base;
        let ft = frame_ticks(fps, tb);
        let known = st.files.entry(source).or_insert(FileInfo {
            time_base: tb,
            frame_ticks: ft,
            late_seek: false,
        });
        if known.time_base != tb {
            break Err(format!(
                "the time base is {}/{}, a run of the same file said {}/{}",
                tb.num, tb.den, known.time_base.num, known.time_base.den
            ));
        }
        let now = sh.now();
        let Some(at) = st.runs.iter().position(|r| r.id == id) else {
            return;
        };
        let (covers_from, warm, ticket, late) = {
            let run = &mut st.runs[at];
            let seek = run.seek;
            let seek_tick = *run.seek_tick.get_or_insert_with(|| seek_ticks(seek, tb));
            let covers_from = match prev_pts {
                Some(p) => p.saturating_add(1),
                // A run from the start of the file: nothing comes before its first frame, however
                // late the picture starts.
                None if seek <= 0.0 => i64::MIN,
                // The first frame: from the seek tick, but never more than one frame before it.
                None => seek_tick.max(info.pts.saturating_sub(ft)),
            };
            let late = prev_pts.is_none() && seek > 0.0 && info.pts.saturating_sub(seek_tick) > ft.saturating_mul(2);
            if run.warm_from == i64::MIN {
                run.warm_from = seek_tick;
            }
            if run.want == i64::MIN {
                run.want = seek_tick;
            }
            run.frames += 1;
            run.head = info.pts.saturating_add(1);
            run.life = Life::Streaming;
            run.last_progress = Instant::now();
            (covers_from, info.pts >= run.warm_from, run.ticket.take(), late)
        };
        st.guard.finished(ticket, now, StartOutcome::Frame);
        if late {
            if let Some(f) = st.files.get_mut(&source) {
                f.late_seek = true;
            }
        }
        let key = FrameKey { source, pts: info.pts };
        let frame = Arc::new(frame);
        if warm {
            st.cache.insert(key, frame, covers_from);
        } else {
            st.cache.insert_cold(key, frame, covers_from);
        }
        prev_pts = Some(info.pts);
        last = Some(info);
        drop(st);
        sh.changed.notify_all();
    };
    // The end: how the child exited decides what the run's last frame means.
    let status = {
        let mut c = child.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if outcome.is_err() {
            let _ = c.kill();
        }
        c.wait()
    };
    // Let the stderr reader finish (it drops its sender at the end of the pipe), so the error
    // carries what ffmpeg said last.
    let until = Instant::now() + Duration::from_secs(1);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match shown.recv_timeout(left) {
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let Some(sh) = shared.upgrade() else { return };
    let mut st = sh.lock();
    let Some(at) = st.runs.iter().position(|r| r.id == id) else {
        return;
    };
    let clean = outcome.is_ok() && status.as_ref().is_ok_and(std::process::ExitStatus::success);
    let frames = st.runs[at].frames;
    let ticket = st.runs[at].ticket.take();
    if clean {
        // A clean end: no frame after the last one (or, with none, after the seek).
        let end = match last {
            Some(f) => Some(f.pts),
            None => st.runs[at].seek_tick.map(|s| s - 1),
        };
        if let Some(end) = end {
            st.cache.mark_end(source, end);
        }
        st.runs[at].life = Life::Ended;
    } else {
        let why = match (&outcome, &status) {
            (Err(e), _) => e.clone(),
            (Ok(()), Ok(s)) => format!(
                "ffmpeg exited {s}: {}",
                tail.lock().map(|t| t.trim().to_string()).unwrap_or_default()
            ),
            (Ok(()), Err(e)) => format!("waiting for ffmpeg: {e}"),
        };
        st.runs[at].life = Life::Failed(why);
    }
    if frames == 0 {
        let outcome = if clean { StartOutcome::Frame } else { StartOutcome::Failed };
        st.guard.finished(ticket, sh.now(), outcome);
    }
    st.runs[at].ended_at = Some(Instant::now());
    drop(st);
    sh.changed.notify_all();
}

/// Kill runs that are wanted but silent past their deadline and runs idle too long; drop the
/// records of runs that ended a while ago; meet the cache's cap once pins have gone.
fn reaper(shared: &Weak<Shared>) {
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let Some(sh) = shared.upgrade() else { return };
        let mut doomed = Vec::new();
        {
            let mut st = sh.lock();
            if st.closed {
                return;
            }
            let cfg = &sh.config;
            for run in &mut st.runs {
                if run.alive() {
                    let wanted = run.head <= run.want || run.frames == 0;
                    let limit = if run.frames == 0 {
                        cfg.first_frame_timeout
                    } else {
                        cfg.frame_timeout
                    };
                    if wanted && run.last_progress.elapsed() > limit {
                        run.life = Life::Failed(format!(
                            "no frame for {:.0} s (killed)",
                            run.last_progress.elapsed().as_secs_f64()
                        ));
                        run.ended_at = Some(Instant::now());
                        doomed.push(Arc::clone(&run.child));
                    } else if !wanted && run.waiters == 0 && run.last_used.elapsed() > cfg.idle_kill {
                        run.life = Life::Ended;
                        run.ended_at = Some(Instant::now());
                        doomed.push(Arc::clone(&run.child));
                    }
                }
            }
            // A finished run's record is kept a moment for the requests reading its outcome.
            st.runs
                .retain(|r| r.alive() || r.waiters > 0 || r.ended_at.is_none_or(|e| e.elapsed() < Duration::from_secs(5)));
            st.cache.trim();
        }
        if !doomed.is_empty() {
            sh.changed.notify_all();
        }
        drop(sh);
        for child in doomed {
            kill(&child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_flags_are_the_designs() {
        let args = run_args("/m/a.mp4", 1.5, Some("auto"));
        assert_eq!(
            args.join(" "),
            "-hide_banner -nostats -nostdin -loglevel info -hwaccel auto -copyts -start_at_zero \
             -ss 1.500000 -i /m/a.mp4 -an -sn -dn -map 0:v:0 -vf showinfo=checksum=0,scale=out_range=tv \
             -fps_mode passthrough -f yuv4mpegpipe -pix_fmt yuv420p pipe:1"
        );
        // A negative seek (a lead before the start) is the start.
        assert!(run_args("/m/a.mp4", -0.2, None).join(" ").contains("-ss 0.000000 -i"));
        assert!(!run_args("/m/a.mp4", 0.0, None).iter().any(|a| a == "-hwaccel"));
    }

    #[test]
    fn a_frame_is_the_rate_in_ticks_and_never_less_than_one() {
        let tb = Rational { num: 1, den: 30000 };
        assert_eq!(frame_ticks(Some(30000.0 / 1001.0), tb), 1001);
        assert_eq!(frame_ticks(Some(25.0), Rational { num: 1, den: 1000 }), 40);
        assert_eq!(frame_ticks(None, tb), 1);
        assert_eq!(frame_ticks(Some(0.0), tb), 1);
        assert_eq!(frame_ticks(Some(1e9), tb), 1);
    }

    #[test]
    fn forward_reads_ahead_a_bounded_number_of_frames_and_the_rest_do_not() {
        let cfg = FrameSourceConfig::default();
        let fwd = Hint::Forward { fps: 30.0 };
        // 720p: 1.3 MB a frame, 48 MiB is 36 frames, held to 24.
        assert_eq!(read_ahead(fwd, &cfg, 1280 * 720 * 3 / 2, 100), 2400);
        // 4K: 12 MB a frame, four of them.
        assert_eq!(read_ahead(fwd, &cfg, 3840 * 2160 * 3 / 2, 100), 400);
        assert_eq!(read_ahead(Hint::Scrub, &cfg, 1280 * 720 * 3 / 2, 100), 0);
        assert_eq!(read_ahead(Hint::Exact, &cfg, 1280 * 720 * 3 / 2, 100), 0);
    }
}
