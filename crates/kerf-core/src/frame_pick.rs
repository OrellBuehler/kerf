//! Which frame of its source a layer shows: [`Pick`], and [`fps_pick`], the export's own
//! choice reproduced.
//!
//! A still asks for the first frame at or after a time (`-ss`), and that is what
//! [`Pick::AtOrAfter`] says. The export asks nothing of the sort: it puts every source
//! frame on the timeline (`trim`, `setpts`) and lets the `fps` filter pick, per output
//! frame, **the last frame whose rounded slot is not after it**. That is not "the frame
//! containing the time" (wrong for reverse, speed != 1 and any phase of half a frame or
//! more), and it is decided by integer arithmetic, so [`fps_pick`] does the arithmetic
//! rather than a formula for it:
//!
//! 1. `-ss S` shifts the file's timestamps by `S` (plus the container's start), in
//!    microseconds rescaled to the stream's ticks, **rounded to nearest**; the frames left
//!    of the shift are dropped. A head-padded proxy opened without a seek then drops its
//!    clone of the first frame (`trim=start_frame=1`).
//! 2. `trim=start:end` keeps the window, both ends truncated to whole microseconds as
//!    `av_parse_time` reads the printed number, then rescaled to ticks and rounded.
//! 3. `reverse` outputs the kept frames last first **with the timestamps in forward order**
//!    (so a constant rate is mirrored over the window, and a variable one is not).
//! 4. `setpts=(PTS-STARTPTS)/speed+start/TB` evaluates in doubles over ticks of the stream's
//!    time base and **truncates** to a tick (`D2TS` is an `(int64_t)` cast, not a rounding).
//! 5. `fps` rescales that to its own ticks, `1/fps`, rounded half away from zero: the
//!    **slot** of a frame. Output frame `k` is the last frame whose slot is `<= k`. There is
//!    none before the first frame's slot, and none from the **end** on: the stream ends where
//!    the frame `trim` dropped (the first one past the window) would have landed, retimed and
//!    rounded the same way, or, for a window that runs to the end of the file, the last
//!    frame's own **duration** past the last frame (not the gap before it: a matroska
//!    clip's gaps alternate 33 and 34 ms and its last frame lasts 33). From there
//!    `overlay=eof_action=pass` shows nothing under it. That end is what holds a slowed clip's last frame for its whole share of the
//!    window, and what drops the last frame of a sped-up or reversed one whose slot lies past
//!    it: at a clip's closing edge **the clip is drawn or not by this rule**, not by the
//!    overlay's `enable`.
//!
//! The time base matters — `start/TB` truncates to a tick, which turns the exact tie of a
//! frame-aligned clip at speed 2 (a source frame precisely half a slot away) into one side
//! or the other — so a [`SourceFrames`] carries it. What the rule leaves out is the part of
//! the real time outside [`SourceFrames`]: the pick says which of the frames it was *given*,
//! and the caller (a decoder with an index) gives it the ones near the window. It was found
//! by rendering clips whose frames number themselves and is held to them on FFmpeg 6.1 and
//! 9.0 (`engine/cli/picked.rs`): speeds 0.5 to 4, forward and reverse, every phase of the
//! output grid, NTSC rates, a seek off the grid, other time bases, a variable rate, a window
//! to the end of the file and of a single frame.
//!
//! **A still image is a stream like any other**: the export reads it as `-loop 1 -framerate
//! <fps> -t <end>`, so it is a run of frames `0, 1, 2, ...` on a `1/<fps>` time base cut off
//! by `-t` (an automatic `trim` ahead of the chain's own, and a frame past the cut that is the
//! one that ended it), with no seek and the chain's own `trim` absolute — a still cut from
//! `source_in` 0.25 starts on the first frame at or after a quarter second. [`FpsPick::image`]
//! says so, and [`fps_pick`] makes that run up itself (it needs no [`SourceFrames`]), so a
//! still's closing edge is decided by the same rule as a video's: it is *not* always drawn on
//! the frame its window closes on.
//!
//! Pure: no I/O.

use crate::clip_timing::{clip_seek, parse_micros, parse_micros_text, Rational};
use crate::engine::seek_arg;

/// The frames of one decoded file as ffprobe or `showinfo` state them.
#[derive(Debug, Clone, Copy)]
pub struct SourceFrames<'a> {
    /// Presentation timestamps in stream ticks, ascending (presentation order).
    pub pts: &'a [i64],
    /// Seconds per tick, `num / den` (`1/90000`, `1001/30000`).
    pub time_base: Rational,
    /// The container's start time in microseconds (`0` for nearly every file): `-ss` is
    /// relative to it. Timestamps that are already relative to the start (`showinfo` under
    /// `-copyts -start_at_zero`) are given with `0`.
    pub start_us: i64,
    /// The last frame's own duration in ticks (ffprobe's frame `duration`, `showinfo`'s
    /// `duration:`): where a window that runs to the end of the file ends. `0` when unknown,
    /// and the last gap between frames stands in.
    pub last_duration: i64,
}

impl SourceFrames<'static> {
    /// No frames: what a still image's pick is given (it makes its own up).
    pub const NONE: Self = Self {
        pts: &[],
        time_base: Rational { num: 1, den: 1 },
        start_us: 0,
        last_duration: 0,
    };
}

/// Which frame of its decoded file a layer shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    /// The first frame at or after a source time: what `-ss` returns, and the still's pick.
    /// Also `0.0` for a still image in a still plan, which has the one frame (a Motion plan
    /// gives it an [`FpsPick`] with `image` set).
    AtOrAfter(f64),
    /// The last frame before a source time: the frame preceding [`Pick::AtOrAfter`]'s.
    Before(f64),
    /// What the export draws at an output frame: the `fps` filter's pick.
    Fps(FpsPick),
}

/// Everything the export's `fps` pick of one clip at one output frame depends on besides the
/// frames themselves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FpsPick {
    /// The clip's speed magnitude.
    pub speed: f64,
    /// Played backwards.
    pub reverse: bool,
    /// The source window the chain trims (`clip_source_window`, tail included): its start is
    /// also the input-side seek (`clip_seek`).
    pub window: (f64, f64),
    /// The clip's `timeline_start`.
    pub start: f64,
    /// The output frame.
    pub frame: u64,
    /// The rate as the clip's `fps=` filter parses it (`PlanCanvas::pick_fps`).
    pub fps: Rational,
    /// The input is a head-padded proxy read without a seek: its first frame is the pad's
    /// clone, which the chain drops.
    pub drop_first: bool,
    /// A still image, read as `-loop 1 -framerate <this>`: the frames are made up (see the
    /// [module](self)) and the chain does not seek. This is the `color=r=` parse of the
    /// delivery rate (`PlanCanvas::fps`), which `-framerate` shares.
    pub image: Option<Rational>,
}

impl Pick {
    /// The index in `src` of the frame this picks, or `None` for no frame (before the first,
    /// past the last, or — for [`Pick::Fps`] — a slot the clip's stream does not fill). A
    /// still image's [`FpsPick`] ignores `src` and answers `Some(0)` for every frame it draws:
    /// the one frame its loop shows.
    pub fn select(&self, src: &SourceFrames) -> Option<usize> {
        match *self {
            Pick::AtOrAfter(t) => {
                let at = src.pts.partition_point(|&p| p < still_shift(t, src));
                (at < src.pts.len()).then_some(at)
            }
            Pick::Before(t) => src.pts.partition_point(|&p| p < still_shift(t, src)).checked_sub(1),
            Pick::Fps(p) => fps_pick(&p, src),
        }
    }
}

/// `av_rescale_rnd(a, b, c, AV_ROUND_NEAR_INF)` for `c > 0`: `a * b / c`, halves away from zero.
fn rescale_near(a: i128, b: i128, c: i128) -> i128 {
    let n = a * b;
    if n >= 0 {
        (n + c / 2) / c
    } else {
        -((-n + c / 2) / c)
    }
}

/// Microseconds to ticks of a time base (`av_rescale_q` from `1/1000000`).
fn ticks(us: i64, tb: Rational) -> i64 {
    rescale_near(i128::from(us), i128::from(tb.den), 1_000_000 * i128::from(tb.num)) as i64
}

/// Where a seek to `micros` leaves the file's own timestamps: the tick that becomes zero.
fn seek_shift(micros: i64, tb: Rational, start_us: i64) -> i64 {
    ticks(micros + start_us, tb)
}

/// The shift of the still's `-ss {:.6}`.
fn still_shift(seconds: f64, src: &SourceFrames) -> i64 {
    seek_shift(parse_micros_text(&seek_arg(seconds)), src.time_base, src.start_us)
}

/// The frames a pick is made over: a file's, or the run a still image's loop makes up.
enum Frames<'a> {
    File(&'a SourceFrames<'a>),
    /// `len` frames, pts `0..len`, on `time_base` — the last of them is the one `-t` cut at.
    Still {
        len: usize,
        time_base: Rational,
    },
}

impl Frames<'_> {
    fn len(&self) -> usize {
        match self {
            Frames::File(f) => f.pts.len(),
            Frames::Still { len, .. } => *len,
        }
    }

    fn pts(&self, i: usize) -> i64 {
        match self {
            Frames::File(f) => f.pts[i],
            Frames::Still { .. } => i as i64,
        }
    }

    fn time_base(&self) -> Rational {
        match self {
            Frames::File(f) => f.time_base,
            Frames::Still { time_base, .. } => *time_base,
        }
    }

    fn start_us(&self) -> i64 {
        match self {
            Frames::File(f) => f.start_us,
            Frames::Still { .. } => 0,
        }
    }

    /// The first index whose pts is not `before`.
    fn partition(&self, before: impl Fn(i64) -> bool) -> usize {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if before(self.pts(mid)) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// How long the last frame lasts, in ticks: its own duration if known, else the gap
    /// before it, else a tick.
    fn last_duration(&self) -> i64 {
        match self {
            Frames::File(f) if f.last_duration > 0 => f.last_duration,
            Frames::File(f) if f.pts.len() >= 2 => f.pts[f.pts.len() - 1] - f.pts[f.pts.len() - 2],
            _ => 1,
        }
    }
}

/// What the chain does with one clip's frames, up to the `fps` filter's output.
struct Run {
    /// The frames the chain keeps, as a range of indices into the file's.
    kept: std::ops::Range<usize>,
    /// The tick the seek makes zero: a file timestamp less this is the chain's own.
    shift: i64,
    /// `setpts`'s `STARTPTS` (the first kept frame, relative to the seek) and `start / TB`.
    first: i64,
    offset: f64,
    speed: f64,
    /// The speed is 1: the chain divides by nothing.
    unit: bool,
    time_base: Rational,
    fps: Rational,
    /// The slot the end of the stream lands in, retimed: no output frame is at or past it.
    end: i64,
}

impl Run {
    /// The slot of the frame at `rel` ticks (relative to the seek): `setpts` in doubles,
    /// truncated (`D2TS` is an `(int64_t)` cast), then `fps`'s rounding to its own ticks.
    fn slot(&self, rel: i64) -> i64 {
        let d = (rel - self.first) as f64;
        let t = if self.unit {
            d + self.offset
        } else {
            d / self.speed + self.offset
        };
        let (tb, fps) = (self.time_base, self.fps);
        rescale_near(
            i128::from(t.trunc() as i64),
            i128::from(tb.num) * i128::from(fps.num),
            i128::from(tb.den) * i128::from(fps.den),
        ) as i64
    }
}

/// Which frames of the file the chain keeps (`from..to`, indices into the file's frames) and
/// the tick the seek makes zero.
struct Window {
    shift: i64,
    from: usize,
    to: usize,
}

fn window(pick: &FpsPick, frames: &Frames) -> Window {
    let tb = frames.time_base();
    // A still image is never seeked (its trim is absolute).
    let seek = if pick.image.is_some() { 0.0 } else { clip_seek(pick.window.0) };
    let shift = seek_shift(if seek > 0.0 { parse_micros(seek) } else { 0 }, tb, frames.start_us());
    // What the chain's `trim` keeps, in ticks relative to the seek: `[lo, hi)`.
    let lo = ticks(parse_micros(pick.window.0 - seek), tb);
    let hi = ticks(parse_micros(pick.window.1 - seek), tb);
    let len = frames.len();
    // The accurate seek drops what is left of it; the pad's clone goes next.
    let mut from = if seek > 0.0 { frames.partition(|p| p - shift < 0) } else { 0 };
    if pick.drop_first {
        from += 1;
    }
    let from = from.max(frames.partition(|p| p - shift < lo)).min(len);
    let to = frames.partition(|p| p - shift < hi);
    Window { shift, from, to }
}

fn run(pick: &FpsPick, frames: &Frames) -> Option<Run> {
    if !(pick.speed > 0.0 && pick.speed.is_finite()) {
        return None;
    }
    let tb = frames.time_base();
    let Window { shift, from, to } = window(pick, frames);
    if from >= to {
        return None;
    }
    let len = frames.len();
    // The stream ends where the frame that ended it would have been: the first frame past the
    // trim, or, when the file ends first, the last frame's own duration past the last frame.
    let beyond = if to < len {
        frames.pts(to) - shift
    } else {
        frames.pts(len - 1) - shift + frames.last_duration()
    };
    let mut run = Run {
        kept: from..to,
        shift,
        first: frames.pts(from) - shift,
        offset: pick.start / (f64::from(tb.num) / f64::from(tb.den)),
        speed: pick.speed,
        unit: (pick.speed - 1.0).abs() < 1e-9,
        time_base: tb,
        fps: pick.fps,
        end: 0,
    };
    run.end = run.slot(beyond);
    Some(run)
}

/// How many of `n` frames have a slot at or before `k` (slots never decrease): the last of
/// them is the one shown at output frame `k`.
fn slots_through(n: usize, k: i64, slot: impl Fn(usize) -> i64) -> usize {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if slot(mid) <= k {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The frame the export's `fps` filter shows at `pick.frame`, as an index into `src`
/// (see the [module](self) for the chain it reproduces); `None` where the clip's stream
/// has no frame for that slot, which is how a clip is **not drawn** on the frame at its
/// closing edge (an equal-rate source has no slot there; a slower one can).
///
/// Binary searches only: `O(log n)` in the frames of the file, whatever the window.
pub fn fps_pick(pick: &FpsPick, src: &SourceFrames) -> Option<usize> {
    // A still's loop: `-t` keeps the frames before `end` (at least one), and the next one is
    // the frame that ended the input.
    let still = pick.image.map(|rate| {
        let time_base = Rational {
            num: rate.den,
            den: rate.num,
        };
        let secs = pick.window.1.max(f64::from(rate.den) / f64::from(rate.num));
        Frames::Still {
            len: usize::try_from(ticks(parse_micros(secs), time_base)).unwrap_or(0) + 1,
            time_base,
        }
    });
    let frames = still.unwrap_or(Frames::File(src));
    let run = run(pick, &frames)?;
    match place(pick, &frames, &run) {
        // A still has one picture, whichever of its copies it is.
        Placed::At(i) => Some(if pick.image.is_some() { 0 } else { i }),
        Placed::Early | Placed::Late => None,
    }
}

/// Where an output frame falls in a run's stream.
enum Placed {
    /// Before the first frame's slot.
    Early,
    /// From the end of the stream on.
    Late,
    /// On this frame (an index into the file's).
    At(usize),
}

fn place(pick: &FpsPick, frames: &Frames, run: &Run) -> Placed {
    let Ok(k) = i64::try_from(pick.frame) else {
        return Placed::Late;
    };
    let n = run.kept.len();
    let slot = |j: usize| run.slot(frames.pts(run.kept.start + j) - run.shift);
    // Output order is file order, or the reverse of it carrying the forward timestamps.
    if k < slot(0) {
        return Placed::Early;
    }
    if k >= run.end {
        return Placed::Late;
    }
    let j = slots_through(n, k, slot) - 1;
    Placed::At(run.kept.start + if pick.reverse { n - 1 - j } else { j })
}

/// How far a streaming cursor has got with a pick: see [`Pick::progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickProgress {
    /// The frames read so far cannot decide it. Read at least one more frame (the
    /// lookahead that shows no later frame belongs to this output frame, or the first
    /// frame past the window), or, when the file has no more, say so (`eof`).
    NeedMore,
    /// Decided, and final: reading on cannot change it.
    Ready {
        /// The frame shown, an index into the frames read (`None`: nothing is drawn).
        shown: Option<usize>,
        /// The first frame whose **pixels** are still needed: everything before it can be
        /// dropped, and a cursor that keeps `keep_from..` holds the shown frame and the
        /// lookahead. A forward pick needs its own frame onwards; a reverse one its whole
        /// window (it is played backwards); a clip whose first slot has not come needs the
        /// first frame of its window; a clip that is over, nothing (the number of frames
        /// read). Only pixels are dropped: the **timestamps** stay, as indices are positions
        /// in the whole run.
        keep_from: usize,
    },
}

impl Pick {
    /// What a streaming cursor still needs to answer this pick, over the frames it has
    /// **read so far** (`read`: a run's `showinfo` timestamps in order, `start_us = 0` since
    /// they are already relative to the container's start). The reference is
    /// [`Pick::select`] over the whole file: whenever this says [`PickProgress::Ready`], the
    /// answer is `select`'s, and it says so as soon as it can — one frame past the shown one
    /// (the **lookahead**: slots never decrease, so the last frame at or before the output
    /// frame is known only when a later one is not) or at the window's end, whichever is first.
    ///
    /// What a cursor must have read, per kind of pick:
    /// * `AtOrAfter` / `Before`: the first frame at or after the time.
    /// * `Fps`, forward: `STARTPTS` (the window's first frame), the shown frame, one frame of
    ///   lookahead — and, where the window runs out first, the first frame past the window,
    ///   or the end of the file, whose `last_duration` (the last frame's own duration, valid
    ///   only when `eof`) fixes where the stream ends (finding 17 of the design note).
    /// * `Fps`, reverse: **every** frame of the window and the one past it (or the end of the
    ///   file): the forward timestamps are re-stamped onto the reversed frames, and `STARTPTS`
    ///   is the first of them.
    /// * a still image: its one frame — the rest is made up.
    ///
    /// `read` must start where the pick needs it: a run begun with the clip's own seek
    /// ([`FpsPick::seek`]) or earlier. A frame before the first one read is not there, so
    /// `Before` of a time the first frame is past answers `None`.
    pub fn progress(&self, read: &SourceFrames, eof: bool) -> PickProgress {
        let len = read.pts.len();
        let ready = |shown, keep_from| PickProgress::Ready { shown, keep_from };
        match *self {
            Pick::AtOrAfter(t) => {
                let at = read.pts.partition_point(|&p| p < still_shift(t, read));
                match (at < len, eof) {
                    (true, _) => ready(Some(at), at),
                    (false, true) => ready(None, len),
                    (false, false) => PickProgress::NeedMore,
                }
            }
            Pick::Before(t) => {
                let at = read.pts.partition_point(|&p| p < still_shift(t, read));
                if at < len || eof {
                    ready(at.checked_sub(1), at.saturating_sub(1))
                } else {
                    PickProgress::NeedMore
                }
            }
            Pick::Fps(p) => fps_progress(&p, read, eof),
        }
    }
}

fn fps_progress(pick: &FpsPick, read: &SourceFrames, eof: bool) -> PickProgress {
    let len = read.pts.len();
    let ready = |shown, keep_from| PickProgress::Ready { shown, keep_from };
    if pick.image.is_some() {
        return match fps_pick(pick, read) {
            None => ready(None, len),
            Some(_) if len > 0 => ready(Some(0), 0),
            Some(_) if eof => ready(None, 0),
            Some(_) => PickProgress::NeedMore,
        };
    }
    if !(pick.speed > 0.0 && pick.speed.is_finite()) {
        return ready(None, len);
    }
    let frames = Frames::File(read);
    let w = window(pick, &frames);
    // The window is complete once a frame at or past its end has been read, or the file ended.
    let complete = w.to < len || eof;
    if !complete && (w.from >= len || pick.reverse) {
        return PickProgress::NeedMore;
    }
    let Some(run) = run(pick, &frames) else {
        return ready(None, len);
    };
    let first = run.kept.start;
    match place(pick, &frames, &run) {
        Placed::Early => ready(None, first),
        Placed::Late if complete => ready(None, len),
        Placed::At(i) if complete => ready(Some(i), if pick.reverse { first } else { i }),
        // A forward stream still open: the shown frame is final only with a frame after it.
        Placed::At(i) if i + 1 < len => ready(Some(i), i),
        Placed::At(_) | Placed::Late => PickProgress::NeedMore,
    }
}

impl FpsPick {
    /// The `-ss` the clip's input is opened with, in seconds: where a run that serves this pick
    /// must begin (`None` for a still image, which is never seeked).
    pub fn seek(&self) -> Option<f64> {
        self.image.is_none().then(|| clip_seek(self.window.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TB10: Rational = Rational { num: 1, den: 10_240 };

    /// `n` frames of a constant `fps` clip in a 1/10240 time base: pts `1024 * i` at 10 fps.
    fn frames(n: usize, step: i64) -> Vec<i64> {
        (0..n as i64).map(|i| i * step).collect()
    }

    fn pick(window: (f64, f64), start: f64, speed: f64, reverse: bool, fps: (u32, u32), k: u64) -> FpsPick {
        FpsPick {
            speed,
            reverse,
            window,
            start,
            frame: k,
            fps: Rational::new(fps.0, fps.1).unwrap(),
            drop_first: false,
            image: None,
        }
    }

    fn shown(pts: &[i64], p: &FpsPick) -> Vec<Option<usize>> {
        let src = SourceFrames {
            pts,
            time_base: TB10,
            start_us: 0,
            last_duration: 0,
        };
        (0..16).map(|k| fps_pick(&FpsPick { frame: k, ..*p }, &src)).collect()
    }

    #[test]
    fn an_equal_rate_clip_fills_its_own_slots_and_not_the_one_after() {
        // Frames 2..7 of a 10 fps clip, placed at 0.4 s in a 10 fps export: slots 4..=9.
        let pts = frames(12, 1024);
        let shown = shown(&pts, &pick((0.2, 0.8), 0.4, 1.0, false, (10, 1), 0));
        let drawn: Vec<_> = shown.iter().map(|s| s.map(|i| i as i64)).collect();
        assert_eq!(&drawn[..4], &[None; 4]);
        assert_eq!(&drawn[4..10], &[Some(2), Some(3), Some(4), Some(5), Some(6), Some(7)]);
        assert_eq!(&drawn[10..], &[None; 6]);
    }

    #[test]
    fn a_slower_export_holds_frames_and_a_faster_one_drops_them() {
        let pts = frames(12, 1024);
        // Speed 0.5: each source frame lasts two slots, the last one through the end of the window.
        let slow = shown(&pts, &pick((0.2, 0.8), 0.0, 0.5, false, (10, 1), 0));
        let slow: Vec<_> = slow.into_iter().flatten().collect();
        assert_eq!(slow, [2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7]);
        // Speed 2, 10 fps in: a frame's slot is round(i / 2), halves up — frames 1, 3, 5 of the
        // window land in the slot after the one they are half way to, and the in-between ones
        // are dropped, as is the last, whose slot (3) is the end.
        let fast = shown(&pts, &pick((0.2, 0.8), 0.0, 2.0, false, (10, 1), 0));
        assert_eq!(fast.into_iter().flatten().collect::<Vec<_>>(), [2, 4, 6]);
    }

    #[test]
    fn the_stream_ends_where_the_dropped_frame_would_have_landed_so_the_edge_is_decided_by_rounding() {
        // 24 fps footage in a 30 fps export, a window of 8 export frames (0.2667 s) from a clip
        // placed at frame 44: seven source frames fit (pts 0..=6 of 1/24), slots 44, 45, 47, 48,
        // 49, 50, 52 — and the end is the slot of the *next* frame, 31 / 24 s on, which is 53.
        // The last source frame is drawn on the frame at the clip's end, 52 = 44 + 8.
        let tb = Rational { num: 1, den: 12_288 };
        let pts: Vec<i64> = (0..96).map(|i| i * 512).collect();
        let src = SourceFrames {
            pts: &pts,
            time_base: tb,
            start_us: 0,
            last_duration: 0,
        };
        let at = |k| fps_pick(&pick((1.0, 1.0 + 8.0 / 30.0), 44.0 / 30.0, 1.0, false, (30, 1), k), &src);
        assert_eq!(at(44), Some(24));
        assert_eq!(at(52), Some(30));
        assert_eq!(at(53), None);
        // The same window played backwards: the first frame to come out is the window's last, so
        // the stream is the same length — and the frame at slot 52 is now the *first* source frame.
        let back = |k| fps_pick(&pick((1.0, 1.0 + 8.0 / 30.0), 44.0 / 30.0, 1.0, true, (30, 1), k), &src);
        assert_eq!((back(44), back(52), back(53)), (Some(30), Some(24), None));
    }

    #[test]
    fn a_window_to_the_end_of_the_file_ends_a_last_frame_duration_after_its_last_frame() {
        // Ticks of a tenth of a second, a frame at 0, 4, 7 and 11: the last gap is 4 but the last
        // frame lasts 3 (a matroska file's gaps alternate 33 and 34 ms and its last frame is 33).
        let (tb, fps) = (Rational { num: 1, den: 10 }, (10, 1));
        let pts = [0, 4, 7, 11];
        let held = |last_duration, k| {
            let src = SourceFrames {
                pts: &pts,
                time_base: tb,
                start_us: 0,
                last_duration,
            };
            fps_pick(&pick((0.0, 100.0), 0.0, 1.0, false, fps, k), &src)
        };
        assert_eq!((held(3, 13), held(3, 14)), (Some(3), None));
        // Without a duration the last gap stands in.
        assert_eq!((held(0, 14), held(0, 15)), (Some(3), None));
    }

    #[test]
    fn a_still_image_is_a_run_of_frames_its_window_ends() {
        let rate = Rational::new(30, 1).unwrap();
        let still = |window: (f64, f64), start: f64, speed: f64, k: u64| {
            let p = FpsPick {
                image: Some(rate),
                ..pick(window, start, speed, false, (30, 1), k)
            };
            fps_pick(&p, &SourceFrames::NONE)
        };
        // A second of it is thirty frames, 0 to 29: the loop's next frame is the one that ended
        // it, so the frame at the window's end — slot 30 — is not drawn.
        let drawn = |window, start, speed| {
            (0..120)
                .filter(|&k| still(window, start, speed, k).is_some())
                .collect::<Vec<_>>()
        };
        assert_eq!(drawn((0.0, 1.0), 0.0, 1.0), (0..30).collect::<Vec<_>>());
        assert_eq!(drawn((0.0, 1.0), 5.0 / 30.0, 1.0), (5..35).collect::<Vec<_>>());
        // Cut from a quarter second the first frame at or after it is 8 (7.5 rounds up), and it
        // plays the same thirty frames.
        assert_eq!(drawn((0.25, 1.25), 0.0, 1.0), (0..30).collect::<Vec<_>>());
        // At twice the speed fifteen slots; at half, sixty.
        assert_eq!(drawn((0.0, 1.0), 0.0, 2.0), (0..15).collect::<Vec<_>>());
        assert_eq!(drawn((0.0, 1.0), 0.0, 0.5), (0..60).collect::<Vec<_>>());
        // Every drawn frame is the one picture.
        assert_eq!(still((0.0, 1.0), 0.0, 1.0, 7), Some(0));
        // A window shorter than a frame period keeps no frame.
        assert!(drawn((0.0, 0.01), 0.0, 1.0).is_empty());
    }

    #[test]
    fn a_window_to_the_end_of_the_file_ends_one_frame_interval_after_its_last_frame() {
        let pts = frames(12, 1024);
        let src = SourceFrames {
            pts: &pts,
            time_base: TB10,
            start_us: 0,
            last_duration: 0,
        };
        // Frames 9, 10 and 11 are the last; the window is longer than the file.
        let tail = |k| fps_pick(&pick((0.9, 5.0), 0.0, 1.0, false, (10, 1), k), &src);
        assert_eq!((tail(0), tail(2), tail(3)), (Some(9), Some(11), None));
        // A window of the very last frame is one interval too, the file's own.
        let one = |k| fps_pick(&pick((1.1, 5.0), 0.0, 1.0, false, (10, 1), k), &src);
        assert_eq!((one(0), one(1)), (Some(11), None));
    }

    #[test]
    fn reverse_plays_the_window_last_frame_first_on_the_forward_timestamps() {
        let pts = frames(12, 1024);
        let back = shown(&pts, &pick((0.2, 0.8), 0.0, 1.0, true, (10, 1), 0));
        assert_eq!(back.into_iter().flatten().collect::<Vec<_>>(), [7, 6, 5, 4, 3, 2]);
        // A variable rate keeps its timestamps in forward order: the gap does not mirror.
        let vfr = vec![0, 1024, 2048, 3072, 6144, 7168, 8192];
        let back = shown(&vfr, &pick((0.0, 0.9), 0.0, 1.0, true, (10, 1), 0));
        assert_eq!(back[..9].iter().map(|s| s.map(|i| i as i64)).collect::<Vec<_>>(), {
            // Output j is source 6 - j, on the pts of source j: slots 0, 1, 2, 3, 6, 7, 8.
            [
                Some(6),
                Some(5),
                Some(4),
                Some(3),
                Some(3),
                Some(3),
                Some(2),
                Some(1),
                Some(0),
            ]
            .to_vec()
        });
    }

    #[test]
    fn the_time_bases_rounding_decides_a_tie_the_exact_arithmetic_leaves_open() {
        // Speed 2 from 10 fps into 30 fps, a clip at 1/30: frame 1's slot is `1.5 i + 1` = 2.5,
        // a tie exact arithmetic breaks upward (slot 3). `start / TB` is 341.33 ticks, rounded to
        // 341, which puts frame 1 just *under* the tie, in slot 2.
        let pts = frames(12, 1024);
        let at = |tb_start: f64| {
            let src = SourceFrames {
                pts: &pts,
                time_base: TB10,
                start_us: 0,
                last_duration: 0,
            };
            (0..8)
                .map(|k| fps_pick(&pick((0.0, 0.8), tb_start, 2.0, false, (30, 1), k), &src).map(|i| i as i64))
                .collect::<Vec<_>>()
        };
        // On an exact tick (0.1 s = 1024 ticks) the slots are 3, 5, 6, ...: frame 1 is at the
        // tie rounded up.
        assert_eq!(at(0.1)[..6], [None, None, None, Some(0), Some(0), Some(1)]);
        // At 1/30 the rounded tick gives 1, 2, 4, 5, 7: frame 1 is one slot *earlier*.
        let rounded = at(1.0 / 30.0);
        assert_eq!(
            rounded[..6],
            [None, Some(0), Some(1), Some(1), Some(2), Some(3)],
            "{rounded:?}"
        );
    }

    #[test]
    fn a_seek_rounds_to_a_tick_and_a_padded_head_loses_its_clone() {
        // A frame every 1/24 s on a 1/24 time base: `-ss 1.02` is tick 24.48, which rounds to
        // the frame at 1.0 — the finding the pick has to share with `-ss`.
        let tb = Rational { num: 1, den: 24 };
        let pts: Vec<i64> = (0..60).collect();
        let src = SourceFrames {
            pts: &pts,
            time_base: tb,
            start_us: 0,
            last_duration: 0,
        };
        assert_eq!(Pick::AtOrAfter(1.02).select(&src), Some(24));
        assert_eq!(Pick::AtOrAfter(1.03).select(&src), Some(25));
        assert_eq!(Pick::Before(1.02).select(&src), Some(23));
        assert_eq!(Pick::AtOrAfter(99.0).select(&src), None);
        assert_eq!(Pick::Before(0.0).select(&src), None);
        // A padded proxy: the clone of frame 0 at pts 0, the footage from tick 3.
        let padded = [0i64, 3, 4, 5, 6];
        let src = SourceFrames {
            pts: &padded,
            time_base: tb,
            start_us: 0,
            last_duration: 0,
        };
        let p = |drop_first| FpsPick {
            drop_first,
            ..pick((0.0, 1.0), 0.0, 1.0, false, (24, 1), 0)
        };
        // Kept as a whole the clone is frame 0, which `trim=start_frame=1` removes.
        assert_eq!(fps_pick(&p(false), &src), Some(0));
        assert_eq!(fps_pick(&p(true), &src), Some(1));
    }

    /// What a cursor has read of `pts` after `m` frames: the file ends there only when `m` is all
    /// of them, and the last frame's duration is only known (and only trusted) then.
    fn read_of(pts: &[i64], m: usize, tb: Rational, last_duration: i64) -> (SourceFrames<'_>, bool) {
        let eof = m == pts.len();
        let src = SourceFrames {
            pts: &pts[..m],
            time_base: tb,
            start_us: 0,
            // Garbage while the file goes on: it must be ignored until then.
            last_duration: if eof { last_duration } else { 7777 },
        };
        (src, eof)
    }

    #[test]
    fn a_forward_pick_is_decided_one_frame_of_lookahead_after_the_frame_it_shows() {
        let pts = frames(12, 1024);
        // Output frame 3 of an equal-rate clip showing frames from 0: it is frame 3, and that is
        // only known once frame 4 is read (a frame 3 *alone* could be followed by another whose
        // slot is not after output frame 3).
        let p = Pick::Fps(pick((0.0, 5.0), 0.0, 1.0, false, (10, 1), 3));
        let at = |m| {
            let (src, eof) = read_of(&pts, m, TB10, 1024);
            p.progress(&src, eof)
        };
        assert_eq!(at(0), PickProgress::NeedMore);
        assert_eq!(at(3), PickProgress::NeedMore);
        assert_eq!(
            at(4),
            PickProgress::NeedMore,
            "frame 3 is the last read: nothing says it is the last at or before slot 3"
        );
        assert_eq!(
            at(5),
            PickProgress::Ready {
                shown: Some(3),
                keep_from: 3
            }
        );
        assert_eq!(
            at(12),
            PickProgress::Ready {
                shown: Some(3),
                keep_from: 3
            }
        );
        // Before the clip's first slot nothing is shown, and the first frame is the one to keep.
        let early = Pick::Fps(pick((0.2, 0.8), 0.5, 1.0, false, (10, 1), 1));
        let (src, eof) = read_of(&pts, 3, TB10, 1024);
        assert_eq!(
            early.progress(&src, eof),
            PickProgress::Ready {
                shown: None,
                keep_from: 2
            }
        );
        // ...and a window with nothing in it is decided as soon as a frame past it is read.
        let hole = Pick::Fps(pick((0.31, 0.39), 0.0, 1.0, false, (10, 1), 0));
        let (src, eof) = read_of(&pts, 5, TB10, 1024);
        assert_eq!(
            hole.progress(&src, eof),
            PickProgress::Ready {
                shown: None,
                keep_from: 5
            }
        );
    }

    #[test]
    fn the_end_of_the_window_and_of_the_file_are_what_close_a_clip() {
        let pts = frames(12, 1024);
        // The window ends at 0.6 s: frame 6 is the first past it, and a clip whose last frame is
        // 5 is over from slot 6 on. Reading frames 0..=6 is enough to say frame 6's slot draws nothing.
        let end = Pick::Fps(pick((0.0, 0.6), 0.0, 1.0, false, (10, 1), 6));
        let at = |m| {
            let (src, eof) = read_of(&pts, m, TB10, 1024);
            end.progress(&src, eof)
        };
        assert_eq!(at(6), PickProgress::NeedMore);
        assert_eq!(
            at(7),
            PickProgress::Ready {
                shown: None,
                keep_from: 7
            }
        );
        // To the end of the file: the slot after the last frame is empty only once the file is
        // known to be over, and then the last frame's own duration says how long it lasts.
        let tail = |k| Pick::Fps(pick((0.0, 100.0), 0.0, 1.0, false, (10, 1), k));
        let (open, ended) = read_of(&pts, 11, TB10, 1024);
        assert!(!ended);
        assert_eq!(tail(11).progress(&open, false), PickProgress::NeedMore);
        let (whole, ended) = read_of(&pts, 12, TB10, 1024);
        assert!(ended);
        assert_eq!(
            tail(11).progress(&whole, true),
            PickProgress::Ready {
                shown: Some(11),
                keep_from: 11
            }
        );
        assert_eq!(
            tail(12).progress(&whole, true),
            PickProgress::Ready {
                shown: None,
                keep_from: 12
            }
        );
        // A longer last frame holds the picture through the slot after it.
        let (long, _) = read_of(&pts, 12, TB10, 2048);
        assert_eq!(
            tail(12).progress(&long, true),
            PickProgress::Ready {
                shown: Some(11),
                keep_from: 11
            }
        );
    }

    #[test]
    fn a_reverse_pick_needs_every_frame_of_its_window() {
        let pts = frames(12, 1024);
        let back = Pick::Fps(pick((0.2, 0.8), 0.0, 1.0, true, (10, 1), 0));
        // Output frame 0 is the window's *last* frame, so nothing is decided until frame 8 (the
        // first past the window) has been read, and the whole window is to be kept.
        let at = |m| {
            let (src, eof) = read_of(&pts, m, TB10, 1024);
            back.progress(&src, eof)
        };
        assert_eq!(at(8), PickProgress::NeedMore);
        assert_eq!(
            at(9),
            PickProgress::Ready {
                shown: Some(7),
                keep_from: 2
            }
        );
    }

    #[test]
    fn a_still_image_needs_only_its_one_frame_and_a_still_image_pick_reads_stills() {
        let rate = Rational::new(30, 1).unwrap();
        let still = |k| {
            Pick::Fps(FpsPick {
                image: Some(rate),
                ..pick((0.0, 1.0), 0.0, 1.0, false, (30, 1), k)
            })
        };
        let pts = [0];
        let (one, _) = read_of(&pts, 1, Rational { num: 1, den: 30 }, 1);
        let (none, _) = read_of(&pts, 0, Rational { num: 1, den: 30 }, 1);
        assert_eq!(still(7).progress(&none, false), PickProgress::NeedMore);
        assert_eq!(
            still(7).progress(&none, true),
            PickProgress::Ready {
                shown: None,
                keep_from: 0
            }
        );
        assert_eq!(
            still(7).progress(&one, false),
            PickProgress::Ready {
                shown: Some(0),
                keep_from: 0
            }
        );
        // Past the loop the still is over, whatever was read.
        assert_eq!(
            still(30).progress(&one, false),
            PickProgress::Ready {
                shown: None,
                keep_from: 1
            }
        );
        assert_eq!(
            FpsPick {
                image: Some(rate),
                ..pick((0.0, 1.0), 0.0, 1.0, false, (30, 1), 0)
            }
            .seek(),
            None
        );
        assert_eq!(pick((2.0, 3.0), 0.0, 1.0, false, (30, 1), 0).seek(), Some(2.0));
        assert_eq!(pick((0.0, 3.0), 0.0, 1.0, false, (30, 1), 0).seek(), Some(0.0));
    }

    #[test]
    fn the_still_picks_are_decided_by_the_first_frame_at_or_after_the_time() {
        let tb = Rational { num: 1, den: 24 };
        let pts: Vec<i64> = (0..60).collect();
        let at = |p: Pick, m| {
            let (src, eof) = read_of(&pts, m, tb, 1);
            p.progress(&src, eof)
        };
        let ready = |shown, keep_from| PickProgress::Ready { shown, keep_from };
        assert_eq!(at(Pick::AtOrAfter(1.02), 24), PickProgress::NeedMore);
        assert_eq!(at(Pick::AtOrAfter(1.02), 25), ready(Some(24), 24));
        assert_eq!(at(Pick::Before(1.02), 24), PickProgress::NeedMore);
        assert_eq!(at(Pick::Before(1.02), 25), ready(Some(23), 23));
        // Past the end of the file there is no frame at or after, and the last one is before.
        assert_eq!(at(Pick::AtOrAfter(99.0), 60), ready(None, 60));
        assert_eq!(at(Pick::Before(99.0), 60), ready(Some(59), 59));
        assert_eq!(at(Pick::AtOrAfter(99.0), 59), PickProgress::NeedMore);
        // Nothing before the first frame read.
        assert_eq!(at(Pick::Before(0.0), 3), ready(None, 0));
    }

    /// The property that makes the helper safe to stream with: fed the file a frame at a time, it
    /// never answers differently from `fps_pick` over the whole file, it answers by the end, it
    /// keeps answering the same, and a forward pick is answered a frame after the one it shows.
    #[test]
    fn a_streaming_cursor_is_never_decided_differently_from_the_whole_file() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let (mut ready_early, mut decided) = (0, 0);
        for case in 0..4000 {
            let tb = [(1, 12_288), (1, 1000), (1, 90_000), (1, 24)][rnd(4) as usize];
            let tb = Rational { num: tb.0, den: tb.1 };
            let fps = [(24, 1), (25, 1), (30_000, 1001), (30, 1)][rnd(4) as usize];
            let step = (i64::from(tb.den) * i64::from(fps.1) / i64::from(fps.0)).max(1);
            let n = 4 + rnd(30) as usize;
            // A variable rate on a third of the cases, a late start on a few.
            let mut at = if rnd(5) == 0 { rnd(40) as i64 * step / 7 } else { 0 };
            let pts: Vec<i64> = (0..n)
                .map(|_| {
                    let here = at;
                    at += if case % 3 == 0 {
                        1 + rnd(2 * step as u64) as i64
                    } else {
                        step
                    };
                    here
                })
                .collect();
            let last_duration = if rnd(2) == 0 { 0 } else { step + rnd(3) as i64 - 1 };
            let dur = pts[n - 1] as f64 * f64::from(tb.num) / f64::from(tb.den);
            let a = if rnd(3) == 0 { 0.0 } else { dur * rnd(70) as f64 / 100.0 };
            let b = a + 0.02 + dur * rnd(100) as f64 / 100.0;
            let p = FpsPick {
                speed: [0.5, 1.0, 1.5, 2.0, 4.0][rnd(5) as usize],
                reverse: rnd(3) == 0,
                window: (a, b),
                start: rnd(100) as f64 / 100.0,
                frame: rnd(70),
                fps: Rational::new(fps.0, fps.1).unwrap(),
                drop_first: rnd(6) == 0,
                image: None,
            };
            let full = SourceFrames {
                pts: &pts,
                time_base: tb,
                start_us: 0,
                last_duration,
            };
            let want = fps_pick(&p, &full);
            let mut first = None;
            for m in 0..=n {
                let (read, eof) = read_of(&pts, m, tb, last_duration);
                match Pick::Fps(p).progress(&read, eof) {
                    PickProgress::NeedMore => assert!(first.is_none(), "case {case}: undecided again at {m} frames: {p:?}"),
                    PickProgress::Ready { shown, keep_from } => {
                        assert_eq!(
                            shown, want,
                            "case {case}: decided {shown:?} at {m} of {n} frames, the file says {want:?}: {p:?}"
                        );
                        assert!(
                            keep_from <= m && shown.is_none_or(|i| keep_from <= i),
                            "case {case}: keep {keep_from} of {m}, shown {shown:?}"
                        );
                        first = first.or(Some(m));
                    }
                }
            }
            let first = first.unwrap_or_else(|| panic!("case {case}: never decided: {p:?}"));
            if let (Some(i), false) = (want, p.reverse) {
                decided += 1;
                assert!(
                    first <= i + 2,
                    "case {case}: decided after {first} frames for frame {i}: {p:?}"
                );
                ready_early += usize::from(first < n);
            }
        }
        // The sweep has to have exercised the interesting case: most forward picks are decided
        // before the file's last frame.
        assert!(ready_early * 2 > decided, "{ready_early} of {decided}");
    }
}
