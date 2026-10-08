//! `FrameCursor`: one clip's frames, in output order, from a run of its own (A1b-3; design
//! `.claude/plans/a1-design.md` §1).
//!
//! A [`FrameSource`](crate::FrameSource) answers a still's pick out of a shared cache; a cursor
//! answers the export's: `Pick::Fps`, output frame after output frame, which depends on frames
//! *around* the one shown (the lookahead that proves no later frame belongs to the slot, the
//! first frame past the window, the stream's first frame for `STARTPTS`). So a cursor is an
//! **exclusive run**: one `ffmpeg` decoding the clip's file forward from the clip's own seek
//! ([`FpsPick::seek`], the export's `-ss`), with the production flags
//! ([`run_args`]), read on the caller's thread, registered nowhere and cached nowhere.
//!
//! What it answers is [`Pick::progress`]'s, over the timestamps it has read so far: it reads
//! until the pick is decided and returns the shown frame, so a cursor's answer is
//! [`Pick::select`]'s over the whole file — and `select` is held to the export's rendered frames
//! by kerf-core's `picked.rs`. **That holds where `-ss` lands on the frame the timestamps say.**
//! A seek into the frames an open GOP's keyframe leads, and a long-GOP transport stream's, land on
//! a *later* keyframe (finding 5 of the design note): the run starts late, as the export's own
//! `-ss` does, and the cursor answers over the frames it was given — the export's, not `select`'s,
//! and not proven against it. [`FrameSource::cursor`](crate::FrameSource::cursor) refuses the
//! containers that are not MP4 / Matroska (a transport stream among them) and a file a router run
//! has marked one-shot, and cannot know of an open-GOP mp4 before one has: the same known limit
//! as the still's. A repeated timestamp is a file's own (a time base that rounds two frames onto one
//! tick) and is taken as it comes (the stderr is read by
//! [`ShowinfoParser::allowing_repeats`]; the frame cache's runs keep the strict parser).
//!
//! It keeps every **timestamp** it has read (indices are positions in the run) but only the
//! **pixels** a later pick can still show: after a decision, `keep_from`; while a forward pick is
//! undecided, the newest frame (the frames of output frames the caller skips are not held). A
//! forward clip holds a frame or two, a **reversed** one its whole window (it is played
//! backwards), which is bounded by [`CursorConfig::window_cap_bytes`] — past it the clip is
//! `Unsupported` and goes to FFmpeg.
//!
//! **Forward only.** Picks must come in output order (or repeat the last); a pick whose frame's
//! pixels were already dropped is an error, not a decode (the caller opens a new cursor).
//! `Pick::Before` asking for the frame before the run's first is "nothing" when the run began at
//! the start of the file and `Unsupported` otherwise (only an earlier run has it).
//!
//! **Nothing waits forever**: a watchdog kills the run when a read has waited
//! [`CursorConfig::first_frame_timeout`] for the first frame or [`CursorConfig::frame_timeout`]
//! for a later one, a run that closed its output and does not exit within `frame_timeout` is
//! killed, and `Drop` kills it.

use std::collections::VecDeque;
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kerf_core::{FpsPick, Pick, PickProgress, PlanLayer, Rational, SourceFrames};

use crate::frame_source::{kill, read_stderr, run_args, wait_until};
use crate::gpu::GpuError;
use crate::showinfo::{plain_log_env, ShowFrame, ShowinfoParser};
use crate::source::{yuv420p_len, YuvFrame, MAX_SIDE};
use crate::y4m::{Y4mError, Y4mReader};

/// Limits of one cursor.
#[derive(Debug, Clone)]
pub struct CursorConfig {
    /// The run is killed when the first frame has not come after this long.
    pub first_frame_timeout: Duration,
    /// The same between frames.
    pub frame_timeout: Duration,
    /// The most pixels a cursor holds at once (a reversed clip's window): past it the clip is
    /// `Unsupported`.
    pub window_cap_bytes: usize,
    /// `-hwaccel` for the run (`None`: software).
    pub hwaccel: Option<String>,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            first_frame_timeout: Duration::from_secs(30),
            frame_timeout: Duration::from_secs(15),
            window_cap_bytes: 512 << 20,
            hwaccel: kerf_core::decode_hwaccel(),
        }
    }
}

/// What the watchdog shares with the reader.
struct Watch {
    child: Mutex<Child>,
    /// A read is under way, and since when (`None`: not reading).
    reading: Mutex<Option<Instant>>,
    first_done: AtomicBool,
    done: AtomicBool,
    killed: AtomicBool,
}

/// See the [module](self).
pub struct FrameCursor {
    path: String,
    seek: f64,
    watch: Arc<Watch>,
    reader: Y4mReader<ChildStdout>,
    shown: Receiver<Result<ShowFrame, String>>,
    tail: Arc<Mutex<String>>,
    time_base: Option<Rational>,
    /// Every timestamp read, in order.
    pts: Vec<i64>,
    /// The last frame's own duration (valid at `eof`).
    last_duration: i64,
    /// The pixels of frames `base..base + pixels.len()`.
    pixels: VecDeque<Arc<YuvFrame>>,
    base: usize,
    frame_bytes: usize,
    eof: bool,
    config: CursorConfig,
}

impl FrameCursor {
    /// A cursor over `layer`'s file from where its pick needs it: an `Fps` pick's clip seek
    /// (a still image: the start), an `AtOrAfter` pick's time. A `Before` pick is refused: it
    /// needs the frame before the run's first, which only a run begun earlier has (open one
    /// with [`FrameCursor::open`] at the time one frame of the stream's rate earlier).
    pub fn for_layer(layer: &PlanLayer, config: CursorConfig) -> Result<Self, GpuError> {
        if matches!(layer.pick, Pick::Before(_)) {
            return Err(GpuError::Unsupported(format!(
                "{}: a `Before` pick needs the frame before a cursor's first",
                layer.path
            )));
        }
        Self::open(&layer.path, (layer.stream.width, layer.stream.height), &layer.pick, config)
    }

    /// A cursor over the file at `path` (probed `size`) that begins where `first` needs it, with
    /// the `-ss` spelled as `first`'s reference spells it (an `Fps` pick's as the export does,
    /// none at the head of the file; the still's `{:.6}` for the others). Later picks may be any
    /// in output order.
    pub fn open(path: &str, size: (u32, u32), first: &Pick, config: CursorConfig) -> Result<Self, GpuError> {
        let (w, h) = size;
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(GpuError::Unsupported(format!(
                "{path}: a {w}x{h} picture (the compositor takes 1 to {MAX_SIDE} px a side)"
            )));
        }
        let (seek, seek_text) = start_of(first);
        let mut args = run_args(path, seek_text.as_deref(), config.hwaccel.as_deref());
        kerf_core::limit_ffmpeg_args(&mut args, 1);
        let mut cmd = kerf_core::ffmpeg_command();
        cmd.args(&args);
        let mut child = plain_log_env(&mut cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| GpuError::Decode(format!("could not run ffmpeg: {e}")))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GpuError::Decode("ffmpeg's pipes were not captured".into()));
        };
        let (tx, rx) = mpsc::channel();
        let tail = Arc::new(Mutex::new(String::new()));
        let err_tail = Arc::clone(&tail);
        let reader = std::thread::Builder::new()
            .name("kerf-cursor-stderr".into())
            .spawn(move || read_stderr(stderr, &tx, &err_tail, ShowinfoParser::allowing_repeats()));
        if let Err(e) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GpuError::Decode(format!("could not start a reader: {e}")));
        }
        let watch = Arc::new(Watch {
            child: Mutex::new(child),
            reading: Mutex::new(None),
            first_done: AtomicBool::new(false),
            done: AtomicBool::new(false),
            killed: AtomicBool::new(false),
        });
        let watched = Arc::downgrade(&watch);
        let (first_frame, between) = (config.first_frame_timeout, config.frame_timeout);
        let watchdog = std::thread::Builder::new()
            .name("kerf-cursor-watch".into())
            .spawn(move || watchdog(&watched, first_frame, between));
        if let Err(e) = watchdog {
            kill(&watch.child);
            return Err(GpuError::Decode(format!("could not start a watchdog: {e}")));
        }
        Ok(Self {
            path: path.to_string(),
            seek,
            watch,
            reader: Y4mReader::new(stdout, size),
            shown: rx,
            tail,
            time_base: None,
            pts: Vec::new(),
            last_duration: 0,
            pixels: VecDeque::new(),
            base: 0,
            frame_bytes: yuv420p_len(w, h).unwrap_or(0),
            eof: false,
            config,
        })
    }

    /// Frames read so far.
    pub fn frames_read(&self) -> usize {
        self.pts.len()
    }

    /// Frames whose pixels are held.
    pub fn frames_held(&self) -> usize {
        self.pixels.len()
    }

    /// The frame `pick` shows (`None`: nothing is drawn). Picks come in output order.
    pub fn pick(&mut self, pick: &Pick) -> Result<Option<Arc<YuvFrame>>, GpuError> {
        loop {
            let progress = match self.time_base {
                Some(time_base) => pick.progress(
                    &SourceFrames {
                        pts: &self.pts,
                        time_base,
                        start_us: 0,
                        last_duration: if self.eof { self.last_duration } else { 0 },
                    },
                    self.eof,
                ),
                // Nothing read yet: nothing can be decided but a still image's emptiness.
                None if self.eof => pick.progress(&SourceFrames::NONE, true),
                None => PickProgress::NeedMore,
            };
            match progress {
                PickProgress::Ready { shown, keep_from } => {
                    let frame = match shown {
                        None => None,
                        Some(i) => Some(self.held(i)?),
                    };
                    self.drop_before(keep_from);
                    return Ok(frame);
                }
                PickProgress::NeedEarlier if self.seek <= 0.0 => return Ok(None),
                PickProgress::NeedEarlier => {
                    return Err(GpuError::Unsupported(format!(
                        "{}: the frame before {:.6} s is before this cursor's start",
                        self.path, self.seek
                    )))
                }
                PickProgress::NeedMore if self.eof => {
                    // `progress` decides everything at the end of the file; this is a guard.
                    return Err(GpuError::Decode(format!("{}: the pick is undecided at the end", self.path)));
                }
                PickProgress::NeedMore => {
                    // A forward pick still undecided shows the newest frame read or a later one
                    // (`Before`'s answer is the one before the first frame past its time: the
                    // newest now), so nothing older is wanted. A reversed pick plays its window
                    // backwards and needs all of it.
                    if !matches!(pick, Pick::Fps(p) if p.reverse) {
                        self.drop_before(self.pts.len().saturating_sub(1));
                    }
                    self.read_one()?;
                }
            }
        }
    }

    /// The pixels of frame `i`, if still held.
    fn held(&self, i: usize) -> Result<Arc<YuvFrame>, GpuError> {
        i.checked_sub(self.base)
            .and_then(|at| self.pixels.get(at))
            .cloned()
            .ok_or_else(|| {
                GpuError::Decode(format!(
                    "{}: frame {i} is no longer held (a cursor reads forward; picks must come in output order)",
                    self.path
                ))
            })
    }

    fn drop_before(&mut self, keep_from: usize) {
        while self.base < keep_from && !self.pixels.is_empty() {
            self.pixels.pop_front();
            self.base += 1;
        }
        if self.pixels.is_empty() {
            self.base = self.base.max(keep_from.min(self.pts.len()));
        }
    }

    /// Read the next frame and its timestamp, or reach the end.
    fn read_one(&mut self) -> Result<(), GpuError> {
        if (self.pixels.len() + 1).saturating_mul(self.frame_bytes) > self.config.window_cap_bytes {
            return Err(GpuError::Unsupported(format!(
                "{}: the pick needs more than {} MiB of frames held at once (a long reversed clip)",
                self.path,
                self.config.window_cap_bytes >> 20
            )));
        }
        *lock(&self.watch.reading) = Some(Instant::now());
        let next = self.reader.next_frame();
        *lock(&self.watch.reading) = None;
        let frame = match next {
            Ok(Some(f)) => f,
            Ok(None) => return self.finish(),
            Err(e @ (Y4mError::Size { .. } | Y4mError::Format(_))) => {
                self.stop();
                return Err(e.into());
            }
            Err(e) => return Err(self.failure(&e.to_string())),
        };
        self.watch.first_done.store(true, Ordering::Relaxed);
        let info = match self.shown.recv_timeout(self.config.frame_timeout) {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => return Err(self.failure(&format!("showinfo: {e}"))),
            Err(RecvTimeoutError::Timeout) => return Err(self.failure("no timestamp for a frame")),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(self.failure("stderr ended before a frame's timestamp"));
            }
        };
        if info.n + 1 != self.reader.frames_read() {
            return Err(self.failure(&format!(
                "frame {} was reported as frame {}",
                self.reader.frames_read() - 1,
                info.n
            )));
        }
        match self.time_base {
            None => self.time_base = Some(info.time_base),
            Some(tb) if tb != info.time_base => return Err(self.failure("the time base changed")),
            Some(_) => {}
        }
        // A repeated timestamp is a file's (the pick takes them as they come); one going back
        // is a stream that cannot be reasoned about.
        if self.pts.last().is_some_and(|&p| info.pts < p) {
            return Err(self.failure(&format!("timestamps went backwards at pts {}", info.pts)));
        }
        self.pts.push(info.pts);
        self.last_duration = info.duration;
        self.pixels.push_back(Arc::new(frame));
        Ok(())
    }

    /// The stream ended: a clean exit is the end of the file, anything else a failure. A run that
    /// closed its output and does not exit within `frame_timeout` is killed.
    fn finish(&mut self) -> Result<(), GpuError> {
        let status = wait_until(&self.watch.child, Instant::now() + self.config.frame_timeout);
        self.watch.done.store(true, Ordering::Relaxed);
        if self.watch.killed.load(Ordering::Relaxed) {
            return Err(self.failure("no frame in time (killed)"));
        }
        match status {
            Ok(s) if s.success() => {
                self.eof = true;
                Ok(())
            }
            Ok(s) => Err(self.failure(&format!("ffmpeg exited {s}"))),
            Err(e) => Err(self.failure(&format!("after its last frame: {e}"))),
        }
    }

    fn stop(&self) {
        kill(&self.watch.child);
        self.watch.done.store(true, Ordering::Relaxed);
    }

    fn failure(&self, why: &str) -> GpuError {
        self.stop();
        // Let the stderr reader finish so the error carries what ffmpeg said last.
        let until = Instant::now() + Duration::from_millis(500);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if self.shown.recv_timeout(left).is_err() {
                break;
            }
        }
        let tail = lock(&self.tail).trim().to_string();
        GpuError::Decode(if tail.is_empty() {
            format!("{}: {why}", self.path)
        } else {
            format!("{}: {why}: {tail}", self.path)
        })
    }
}

impl Drop for FrameCursor {
    fn drop(&mut self) {
        self.watch.done.store(true, Ordering::Relaxed);
        kill(&self.watch.child);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Kill the run when a read has waited too long (the first frame `first`, a later one `between`).
fn watchdog(watch: &std::sync::Weak<Watch>, first: Duration, between: Duration) {
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let Some(w) = watch.upgrade() else { return };
        if w.done.load(Ordering::Relaxed) {
            return;
        }
        let since = *lock(&w.reading);
        let limit = if w.first_done.load(Ordering::Relaxed) {
            between
        } else {
            first
        };
        if since.is_some_and(|s| s.elapsed() > limit) {
            w.killed.store(true, Ordering::Relaxed);
            kill(&w.child);
            return;
        }
    }
}

/// Where a run for `first` begins: the seek in seconds and the text of its `-ss` (`None`: no
/// `-ss`).
fn start_of(first: &Pick) -> (f64, Option<String>) {
    match first {
        Pick::Fps(p) => (p.seek().unwrap_or(0.0), p.seek_arg()),
        Pick::AtOrAfter(t) | Pick::Before(t) => {
            let t = t.max(0.0);
            (t, Some(kerf_core::seek_arg(t)))
        }
    }
}

/// The export's pick of one clip at consecutive output frames, through one cursor: what an
/// export (A7) does per clip. `frames` are the output frames, ascending; `each` takes every one
/// with the frame shown (`None`: nothing is drawn) as it is decided, so a long clip is never
/// held whole.
pub fn picks_through(
    cursor: &mut FrameCursor,
    pick: FpsPick,
    frames: impl IntoIterator<Item = u64>,
    mut each: impl FnMut(u64, Option<Arc<YuvFrame>>),
) -> Result<(), GpuError> {
    for frame in frames {
        let shown = cursor.pick(&Pick::Fps(FpsPick { frame, ..pick }))?;
        each(frame, shown);
    }
    Ok(())
}
