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
//!    rounded the same way, or, for a window that runs to the end of the file, one frame
//!    interval past the last frame. From there `overlay=eof_action=pass` shows nothing
//!    under it. That end is what holds a slowed clip's last frame for its whole share of the
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
    /// relative to it.
    pub start_us: i64,
}

/// Which frame of its decoded file a layer shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    /// The first frame at or after a source time: what `-ss` returns, and the still's pick.
    /// Also `0.0` for a still image, which has the one frame.
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
}

impl Pick {
    /// The index in `src` of the frame this picks, or `None` for no frame (before the first,
    /// past the last, or — for [`Pick::Fps`] — a slot the clip's stream does not fill).
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

/// Microseconds to ticks of `src`'s time base (`av_rescale_q` from `1/1000000`).
fn ticks(us: i64, src: &SourceFrames) -> i64 {
    let tb = src.time_base;
    rescale_near(i128::from(us), i128::from(tb.den), 1_000_000 * i128::from(tb.num)) as i64
}

/// Where a seek to `micros` leaves the file's own timestamps: the tick that becomes zero.
fn seek_shift(micros: i64, src: &SourceFrames) -> i64 {
    ticks(micros + src.start_us, src)
}

/// The shift of the still's `-ss {:.6}`.
fn still_shift(seconds: f64, src: &SourceFrames) -> i64 {
    seek_shift(parse_micros_text(&seek_arg(seconds)), src)
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

fn run(pick: &FpsPick, src: &SourceFrames) -> Option<Run> {
    if !(pick.speed > 0.0 && pick.speed.is_finite()) {
        return None;
    }
    let tb = src.time_base;
    let seek = clip_seek(pick.window.0);
    let shift = seek_shift(if seek > 0.0 { parse_micros(seek) } else { 0 }, src);
    // What the chain's `trim` keeps, in ticks relative to the seek: `[lo, hi)`.
    let lo = ticks(parse_micros(pick.window.0 - seek), src);
    let hi = ticks(parse_micros(pick.window.1 - seek), src);
    let len = src.pts.len();
    // The accurate seek drops what is left of it; the pad's clone goes next.
    let mut from = if seek > 0.0 {
        src.pts.partition_point(|&p| p - shift < 0)
    } else {
        0
    };
    if pick.drop_first {
        from += 1;
    }
    let from = from.max(src.pts.partition_point(|&p| p - shift < lo)).min(len);
    let to = src.pts.partition_point(|&p| p - shift < hi);
    if from >= to {
        return None;
    }
    // The stream ends where the frame that ended it would have been: the first frame past the
    // trim, or one more frame interval past the last when the file ends first (the interval
    // the file's last two frames are apart, a tick when it has one frame).
    let beyond = if to < len {
        src.pts[to] - shift
    } else {
        let step = if len >= 2 { src.pts[len - 1] - src.pts[len - 2] } else { 1 };
        src.pts[len - 1] - shift + step
    };
    let mut run = Run {
        kept: from..to,
        shift,
        first: src.pts[from] - shift,
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

/// The frame the export's `fps` filter shows at `pick.frame`, as an index into `src`
/// (see the [module](self) for the chain it reproduces); `None` where the clip's stream
/// has no frame for that slot, which is how a clip is **not drawn** on the frame at its
/// closing edge (an equal-rate source has no slot there; a slower one can).
///
/// Binary searches only: `O(log n)` in the frames of the file, whatever the window.
pub fn fps_pick(pick: &FpsPick, src: &SourceFrames) -> Option<usize> {
    let run = run(pick, src)?;
    let n = run.kept.len();
    let k = i64::try_from(pick.frame).ok()?;
    let slot = |j: usize| run.slot(src.pts[run.kept.start + j] - run.shift);
    // Output order is file order, or the reverse of it carrying the forward timestamps.
    if k < slot(0) || k >= run.end {
        return None;
    }
    // Slots never decrease: the last output frame at or before `k`.
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if slot(mid) <= k {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let j = lo - 1;
    Some(run.kept.start + if pick.reverse { n - 1 - j } else { j })
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
        }
    }

    fn shown(pts: &[i64], p: &FpsPick) -> Vec<Option<usize>> {
        let src = SourceFrames {
            pts,
            time_base: TB10,
            start_us: 0,
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
    fn a_window_to_the_end_of_the_file_ends_one_frame_interval_after_its_last_frame() {
        let pts = frames(12, 1024);
        let src = SourceFrames {
            pts: &pts,
            time_base: TB10,
            start_us: 0,
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
        };
        let p = |drop_first| FpsPick {
            drop_first,
            ..pick((0.0, 1.0), 0.0, 1.0, false, (24, 1), 0)
        };
        // Kept as a whole the clone is frame 0, which `trim=start_frame=1` removes.
        assert_eq!(fps_pick(&p(false), &src), Some(0));
        assert_eq!(fps_pick(&p(true), &src), Some(1));
    }
}
