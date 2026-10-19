//! Animated SVG titles in the ffmpeg graphs.
//!
//! ffmpeg never sees an SVG. Each title is rasterised by resvg ([`crate::svg::raster`]) from the
//! static document [`SvgDoc::at`] gives for a moment, written as PNGs, and `overlay`ed like any
//! other picture — so an export, the scrubbed still and the playback stream draw the same title
//! from the same pixels, and the filter graph has nothing animated in it.
//!
//! * **An export or a stream reads a numbered sequence** (`-f image2 -framerate fps -i dir/%06d.png`,
//!   [`svg_sequences`]): frame `j` is the document at `doc_t0 + j / fps`, for exactly the output
//!   frames the overlay's `between(t,start,end)` is true on. The PNGs are written *before* ffmpeg
//!   is spawned ([`prepare`]) — no spawn here has a stdin to stream them down, an export runs
//!   ffmpeg twice for two-pass and again for the software retry, and a sequence on disk serves all
//!   of those. The directory is named by a digest of everything that decides the pixels, so a
//!   cut that did not change finds its titles already drawn.
//! * **A document that cannot move is one picture** (`Kind::Held`: `-loop 1 -framerate fps -i file`),
//!   not a file per frame.
//! * **The stream's sequences do not depend on the playhead** ([`stream_sequences`]): a title is drawn
//!   once on the output's own frame grid, keyed by the title, and a stream from any frame of it starts
//!   reading at the right picture (`-start_number`), so a replay, or a seek back, finds it drawn.
//! * **A still reads one PNG** ([`still_svgs`], [`ensure_stills`]): the document at the still's
//!   own time, at the still's size.
//!
//! Nothing here changes a graph without an SVG overlay: every function answers "nothing" for an
//! empty `svg_overlays`, and the builders add nothing for that.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{cpu, export_format, export_timeline, ExportFormat, ExportOptions, ExportProgress};
use crate::clip_timing::{ClipFx, ClipTiming, Rational};
use crate::error::{Error, Result};
use crate::fonts::{self, FontSet};
use crate::model::{Asset, StreamKind, SvgOverlay, Timeline};
use crate::svg::raster::{self, Pin, MAX_PIXELS, RASTER_VERSION};
use crate::svg::SvgDoc;

/// What an export says while its titles are being drawn (`ExportProgress.waiting`).
pub(super) const RENDERING: &str = "rendering animated titles";

/// How long the playback stream waits for its titles to be drawn before it gives up.
const PREVIEW_PREPARE_LIMIT: Duration = Duration::from_secs(10);

/// What the stream says when it gave up.
const TOO_HEAVY: &str = "animated title too heavy to preview: drawing its frames took more than 10 s";

/// How often a preparing thread looks at its cancel and its deadline.
const POLL: Duration = Duration::from_millis(25);

/// The most memory the drawing threads of one preparation may hold between them, estimated from
/// the picture's size and the widest image the document decodes (a thread draws into a picture,
/// a second one when the document is fit inside the rectangle, and encodes the PNG).
const RENDER_MEMORY: u64 = 1 << 30;

/// Frames between two looks at the cancel while the frames are being compared.
const SCAN_STRIDE: u64 = 16;

/// Documents kept parsed (reading one takes up to a second and a half).
const DOC_CACHE: usize = 6;

/// A time past which nothing is looked for: a position this far out is a mistake, and the search
/// for its frame must not run away with it.
const MAX_SECONDS: f64 = 1.0e9;

/// Where an overlay lands on a frame, in pixels: its rectangle's top-left corner (which may lie
/// off the frame) and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
}

/// The rectangle `o` covers on a `frame_w` x `frame_h` frame, or `None` when none of it is on the
/// frame or it would be too large to draw (the overlay is then left out).
fn rect_px(o: &SvgOverlay, frame_w: u32, frame_h: u32) -> Option<Rect> {
    let (fw, fh) = (f64::from(frame_w), f64::from(frame_h));
    let px = |v: f64, of: f64| {
        let p = (v * of).round();
        p.is_finite().then_some(p)
    };
    let (x, y) = (px(o.x, fw)?, px(o.y, fh)?);
    let (w, h) = (px(o.width, fw)?.max(1.0), px(o.height, fh)?.max(1.0));
    if w * h > MAX_PIXELS as f64 {
        tracing::warn!(overlay = %o.id, "an animated title is too large to draw; leaving it out");
        return None;
    }
    if x >= fw || y >= fh || x + w <= 0.0 || y + h <= 0.0 {
        return None;
    }
    Some(Rect {
        x: x as i64,
        y: y as i64,
        w: w as u32,
        h: h as u32,
    })
}

/// The first output frame whose graph time is at or after `t`.
fn first_frame_from(fps: Rational, t: f64) -> u64 {
    let t = if t.is_finite() { t.clamp(0.0, MAX_SECONDS) } else { 0.0 };
    let mut k = fps.frame_containing(t).saturating_sub(1);
    while fps.frame_time(k) < t {
        k += 1;
    }
    while k > 0 && fps.frame_time(k - 1) >= t {
        k -= 1;
    }
    k
}

/// The last output frame whose graph time is at or before `t` (before it when `strict`); `None`
/// when not even frame 0 is.
fn last_frame_to(fps: Rational, t: f64, strict: bool) -> Option<u64> {
    if t.is_nan() {
        return None;
    }
    let t = t.min(MAX_SECONDS);
    let before = |k: u64| {
        let ft = fps.frame_time(k);
        if strict {
            ft < t
        } else {
            ft <= t
        }
    };
    let mut k = fps.frame_containing(t) + 1;
    while !before(k) {
        k = k.checked_sub(1)?;
    }
    Some(k)
}

/// The output frames `o` is on screen for on a cut `total` seconds long whose last frame is
/// `last_output`: the first frame at or after its start to the last at or before its end, closed at
/// both ends like the graph's `between`. `None` when it is on none of them. A title that starts at
/// or after the end of the cut is left out before anything is searched for (a start of 1e300 is a
/// mistake, and no frame is at it).
fn window(o: &SvgOverlay, fps: Rational, total: f64, last_output: u64) -> Option<(u64, u64)> {
    if !(o.start.is_finite() && o.end.is_finite() && o.end > o.start) || o.start >= total {
        return None;
    }
    let first = first_frame_from(fps, o.start);
    let last = last_frame_to(fps, o.end.min(total), false)?.min(last_output);
    (last >= first).then_some((first, last))
}

fn clamped_opacity(o: &SvgOverlay) -> f64 {
    if o.opacity.is_finite() {
        o.opacity.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// A digest of what decides a picture, as 32 hex digits. **The fonts a project embeds are part of it**
/// (the digest of the [`FontSet`] the title is planned with, which is also the set it is drawn with):
/// another set, another picture. With none embedded nothing is added, so a project without them keeps
/// the keys it always had.
fn digest(fonts: &FontSet, parts: impl FnOnce(&mut Sha256)) -> String {
    digest_with(fonts.digest(), parts)
}

/// [`digest`] for a given embedded font set (`None` for none).
fn digest_with(embedded_fonts: Option<&str>, parts: impl FnOnce(&mut Sha256)) -> String {
    let mut hash = Sha256::new();
    hash.update(RASTER_VERSION.to_le_bytes());
    if let Some(fonts) = embedded_fonts {
        hash.update(b"embedded fonts");
        hash.update(fonts.as_bytes());
    }
    parts(&mut hash);
    hash.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_svg(hash: &mut Sha256, svg: &str) {
    hash.update((svg.len() as u64).to_le_bytes());
    hash.update(svg.as_bytes());
}

// ---- a sequence for an export or the stream ------------------------------------------------------

/// How a title's pictures are laid out on disk and read by ffmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// One PNG a frame (`dir/%06d.png`), hard-linked where consecutive frames are the same.
    Frames,
    /// A document with nothing in it that moves: one PNG (`dir/000000.png`), looped by ffmpeg.
    Held,
}

/// One overlay as pictures on disk.
#[derive(Debug, Clone)]
pub(super) struct SvgSeq<'a> {
    /// The sanitised source.
    pub svg: &'a str,
    /// The directory the pictures are in (named by a digest of everything that decides them).
    pub dir: PathBuf,
    /// What follows `-i`: `dir/%06d.png` (a `%` in the path is `%%`), or the one file, joined with
    /// `/` on every OS (see [`argv_root`]).
    pub input: String,
    pub kind: Kind,
    /// The output frame the first picture read lands on.
    pub first_frame: u64,
    /// The pictures in the sequence `dir` holds (named by the key) ...
    pub frames: u64,
    /// ... and the ones this run reads: `first_picture..first_picture + pictures`. An export reads
    /// all of them; a stream from the middle of a title reads the rest of it.
    pub first_picture: u64,
    pub pictures: u64,
    /// The document's own clock at picture 0; picture `j` is one frame after picture `j - 1`.
    pub doc_t0: f64,
    pub fps: Rational,
    pub rect: Rect,
    pub opacity: f64,
    /// The overlay's window, for `enable`.
    pub start: f64,
    pub end: f64,
    /// The embedded fonts the title is drawn with, taken when it was planned: the key above names this
    /// set, and the drawing uses it, whatever the registry holds by then.
    pub fonts: Arc<FontSet>,
}

impl SvgSeq<'_> {
    /// The pictures this run needs on disk.
    fn needed(&self) -> Range<u64> {
        self.first_picture..self.first_picture + self.pictures
    }

    /// Whether the run needs every picture of the sequence.
    fn needs_all(&self) -> bool {
        self.first_picture == 0 && self.pictures == self.frames
    }
}

/// A sequence the document's own frames make: `layout.frames` pictures from the clock at
/// `layout.doc_t0`, at `fps`, drawn `rect` big, for the overlay `o` (the title the directory is
/// named for) — or, when the sanitised source has nothing that could move, the one picture
/// [`Kind::Held`] is.
struct Layout {
    /// The output frame the first picture read lands on, and the first frame the title is on at all
    /// (the one a held picture starts at).
    lands_on: u64,
    window_start: u64,
    /// The pictures this run reads: `first_picture..first_picture + pictures`.
    first_picture: u64,
    pictures: u64,
    /// The sequence the directory holds: the clock at picture 0 and how many pictures.
    doc_t0: f64,
    frames: u64,
}

/// `root` as the text of an argv path, with no separator at its end. A path in the argv is the
/// cache directory's text, then `/` and the names below it, never `Path::join`: that would put a
/// `\` before them on Windows (which ffmpeg reads as `/`), and the argv would differ by OS, as
/// would the golden digests. The directories and files themselves are still [`PathBuf`]s.
fn argv_root(root: &std::path::Path) -> String {
    root.to_string_lossy().trim_end_matches(std::path::is_separator).to_string()
}

/// What a sequence draws and when it is on screen: a floating title's artwork and window, or a title
/// clip's (whole frame, opaque; its window is the clip's).
struct Art<'a> {
    svg: &'a str,
    opacity: f64,
    start: f64,
    end: f64,
}

impl<'a> Art<'a> {
    fn of(o: &'a SvgOverlay) -> Self {
        Art {
            svg: &o.svg,
            opacity: clamped_opacity(o),
            start: o.start,
            end: o.end,
        }
    }
}

fn seq_of<'a>(
    o: Art<'a>,
    root: &std::path::Path,
    fps: Rational,
    rect: Rect,
    layout: Layout,
    registered: &Arc<FontSet>,
) -> SvgSeq<'a> {
    let fonts = registered.named_by([o.svg]);
    let Layout {
        lands_on,
        window_start,
        first_picture,
        pictures,
        doc_t0,
        frames,
    } = layout;
    let opacity = o.opacity;
    if !crate::svg::may_animate(o.svg) {
        // Nothing moves, so the picture is the same at every moment, and what decides it is the
        // artwork, its size and its opacity: not when the title starts, how long it runs, or the
        // rate, so trimming a held title keeps its picture.
        let key = digest(&fonts, |h| {
            h.update(b"held");
            hash_svg(h, o.svg);
            h.update([rect.w.to_le_bytes(), rect.h.to_le_bytes()].concat());
            h.update(opacity.to_bits().to_le_bytes());
        });
        let dir = root.join(&key);
        let input = format!("{}/{key}/{}", argv_root(root), raster::frame_name(0));
        return SvgSeq {
            svg: o.svg,
            dir,
            input,
            kind: Kind::Held,
            first_frame: window_start,
            frames: 1,
            first_picture: 0,
            pictures: 1,
            doc_t0: 0.0,
            fps,
            rect,
            opacity,
            start: o.start,
            end: o.end,
            fonts,
        };
    }
    // The rectangle's position is where the picture is laid, not what it shows, so a title that is
    // only moved keeps its frames.
    let key = digest(&fonts, |h| {
        h.update(b"sequence");
        hash_svg(h, o.svg);
        h.update(doc_t0.to_bits().to_le_bytes());
        h.update([fps.num.to_le_bytes(), fps.den.to_le_bytes()].concat());
        h.update(frames.to_le_bytes());
        h.update([rect.w.to_le_bytes(), rect.h.to_le_bytes()].concat());
        h.update(opacity.to_bits().to_le_bytes());
    });
    let dir = root.join(&key);
    let input = format!("{}/{key}/%06d.png", argv_root(root).replace('%', "%%"));
    SvgSeq {
        svg: o.svg,
        dir,
        input,
        kind: Kind::Frames,
        first_frame: lands_on,
        frames,
        first_picture,
        pictures,
        doc_t0,
        fps,
        rect,
        opacity,
        start: o.start,
        end: o.end,
        fonts,
    }
}

/// The sequences `timeline` (already `for_render`ed and sliced, as the builders have it) draws on a
/// canvas of `fmt`, in the order they are laid over the picture. Pure: it names the files, it
/// writes none.
///
/// Frame `k` carries the overlay when the graph's `between(t,start,end)` holds at
/// `ffmpeg_frame_time(k)` — closed at both ends, like a text overlay — and the output has that
/// frame at all (`k / fps` before the cut ends). The document is read at the frame's exact slot
/// start, from the overlay's own clock (`SvgOverlay::offset` is what a slice cut off the front).
pub(super) fn svg_sequences<'a>(timeline: &'a Timeline, fmt: &ExportFormat) -> Vec<SvgSeq<'a>> {
    if timeline.svg_overlays.is_empty() {
        return Vec::new();
    }
    let Some(fps) = Rational::from_fps(fmt.fps) else {
        return Vec::new();
    };
    let total = timeline.duration();
    let Some(last_output) = last_frame_to(fps, total, true) else {
        return Vec::new();
    };
    let root = raster::root();
    let registered = fonts::snapshot();
    timeline
        .svg_overlays
        .iter()
        .filter_map(|o| {
            let rect = rect_px(o, fmt.width, fmt.height)?;
            let (first, last) = window(o, fps, total, last_output)?;
            let frames = last - first + 1;
            let doc_t0 = (o.offset + fps.exact_time(first) - o.start).max(0.0);
            let layout = Layout {
                lands_on: first,
                window_start: first,
                first_picture: 0,
                pictures: frames,
                doc_t0,
                frames,
            };
            Some(seq_of(Art::of(o), &root, fps, rect, layout, &registered))
        })
        .collect()
}

/// The sequences the playback stream reads: `sliced` is the cut from the playhead `start`, and
/// `full` the titles of the cut before it was sliced (`full_total` seconds long).
///
/// A title is drawn **once on the output's own frame grid and named for the title**, not for where
/// the playhead is: the stream from any frame of it reads the same directory, starting at the
/// picture that frame needs (`-start_number`), so playing it again, or from just before, finds it
/// drawn. That holds when the playhead is on a frame of the stream (it is, to a millionth of a
/// frame, wherever the page quantises it); one between frames keeps the sequence of its own — the
/// cut's frames are then half a slot from the title's, and the title would be a fraction of a frame
/// out of step with the picture under it.
pub(super) fn stream_sequences<'a>(
    full: &[SvgOverlay],
    full_total: f64,
    sliced: &'a Timeline,
    start: f64,
    fmt: &ExportFormat,
) -> Vec<SvgSeq<'a>> {
    if sliced.svg_overlays.is_empty() {
        return Vec::new();
    }
    let Some(fps) = Rational::from_fps(fmt.fps) else {
        return Vec::new();
    };
    let ahead = start * f64::from(fps.num) / f64::from(fps.den);
    let on_grid = ahead.is_finite() && ahead >= 0.0 && (ahead - ahead.round()).abs() < 1e-6;
    if !on_grid {
        return svg_sequences(sliced, fmt);
    }
    let shift = fps.frame_at(start);
    let (Some(last_sliced), Some(last_full)) = (
        last_frame_to(fps, sliced.duration(), true),
        last_frame_to(fps, full_total, true),
    ) else {
        return Vec::new();
    };
    let root = raster::root();
    let registered = fonts::snapshot();
    sliced
        .svg_overlays
        .iter()
        .filter_map(|o| {
            let rect = rect_px(o, fmt.width, fmt.height)?;
            let whole = full.iter().find(|f| f.id == o.id)?;
            // Frames of the stream the title is on, and frames of the whole cut it is drawn on.
            let (on_first, on_last) = window(o, fps, sliced.duration(), last_sliced)?;
            let (first, last) = window(whole, fps, full_total, last_full)?;
            let frames = last - first + 1;
            // Picture j is on frame `first + j` of the cut, and on frame `first + j - shift` of the stream.
            let lo = (i128::from(on_first) + i128::from(shift) - i128::from(first)).max(0);
            let hi = (i128::from(on_last) + i128::from(shift) - i128::from(first)).min(i128::from(frames) - 1);
            if hi < lo {
                return None;
            }
            let (from, to) = (u64::try_from(lo).ok()?, u64::try_from(hi).ok()?);
            let lands_on = u64::try_from(i128::from(first) + lo - i128::from(shift)).ok()?;
            let doc_t0 = (whole.offset + fps.exact_time(first) - whole.start).max(0.0);
            let layout = Layout {
                lands_on,
                window_start: on_first,
                first_picture: from,
                pictures: to - from + 1,
                doc_t0,
                frames,
            };
            Some(seq_of(Art::of(o), &root, fps, rect, layout, &registered))
        })
        .collect()
}

/// The `-i` of each sequence, after the clips' inputs.
pub(super) fn push_inputs(seqs: &[SvgSeq<'_>], fmt: &ExportFormat, args: &mut Vec<String>) {
    for s in seqs {
        match s.kind {
            Kind::Frames => {
                args.extend(["-f", "image2", "-framerate"].map(String::from));
                args.push(format!("{}", fmt.fps));
                if s.first_picture > 0 {
                    args.push("-start_number".to_string());
                    args.push(s.first_picture.to_string());
                }
            }
            // One file, read again and again at the output's rate: `-pattern_type none` makes a `%`
            // or a `[` in the path a character of it, not a pattern.
            Kind::Held => {
                args.extend(["-f", "image2", "-pattern_type", "none", "-loop", "1", "-framerate"].map(String::from));
                args.push(format!("{}", fmt.fps));
            }
        }
        args.push("-i".to_string());
        args.push(s.input.clone());
    }
}

/// The chains that lay `seqs` over the pad `from`, the last landing on `to`. `first_input` is the
/// ffmpeg input index of the first sequence (the clips' inputs come before it).
///
/// Each sequence is restamped onto the canvas' grid before it is overlaid: `settb` puts it on the
/// canvas' own time base (`1/fps`) and `setpts=N+k` makes picture `N` frame `k` of the output —
/// counted in frames, not computed in seconds, so no rounding can put a picture a frame off.
pub(super) fn push_chains(seqs: &[SvgSeq<'_>], first_input: usize, from: &str, to: &str, chains: &mut Vec<String>) {
    let mut cur = from.to_string();
    for (i, s) in seqs.iter().enumerate() {
        chains.push(format!(
            "[{input}:v]settb=expr={den}/{num},setpts=N+{k}[svg{i}]",
            input = first_input + i,
            den = s.fps.den,
            num = s.fps.num,
            k = s.first_frame,
        ));
        let out = if i + 1 == seqs.len() {
            to.to_string()
        } else {
            format!("vsv{i}")
        };
        chains.push(format!(
            "[{cur}][svg{i}]overlay=x={x}:y={y}:eof_action=pass:enable='between(t,{start},{end})'[{out}]",
            x = s.rect.x,
            y = s.rect.y,
            start = s.start,
            end = s.end,
        ));
        cur = out;
    }
}

// ---- a title on a track --------------------------------------------------------------------------

/// A title clip's pictures as the input of its clip chain: the sequence, and how many output frames
/// the clip is drawn on (a held picture, which ffmpeg loops for ever, is cut after that many).
#[derive(Debug, Clone)]
pub(super) struct ClipSeq<'a> {
    pub seq: SvgSeq<'a>,
    pub count: u64,
}

/// How a title clip's chain restamps its pictures onto the canvas' grid: the chain's first filters,
/// in place of a decoded clip's `trim` and `setpts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClipStamp {
    pub num: u32,
    pub den: u32,
    pub first_frame: u64,
    pub held: Option<u64>,
}

impl ClipSeq<'_> {
    pub(super) fn stamp(&self) -> ClipStamp {
        ClipStamp {
            num: self.seq.fps.num,
            den: self.seq.fps.den,
            first_frame: self.seq.first_frame,
            held: (self.seq.kind == Kind::Held).then_some(self.count),
        }
    }
}

impl ClipStamp {
    /// `settb=expr=den/num,setpts=N+k`, counted in frames as a floating title is (see [`push_chains`]),
    /// then for a held picture `trim=end_frame=n`.
    pub(super) fn filters(&self) -> String {
        let stamp = format!("settb=expr={}/{},setpts=N+{}", self.den, self.num, self.first_frame);
        match self.held {
            Some(n) => format!("{stamp},trim=end_frame={n}"),
            None => stamp,
        }
    }
}

/// The pictures of every title clip of `timeline` (already `for_render`ed and sliced, as the builders
/// have it) on a canvas of `fmt`, indexed like `fx` (flat storage order); `None` for a clip of footage.
/// Pure: it names the files, it writes none.
///
/// A title is drawn over the whole frame (its clip chain fits and places it from there), on exactly the
/// output frames its clip is on: from the first at or after its start to the last **before** its end,
/// tail included (a decoded clip has no frame at its end either), and not past the cut's last frame.
/// Picture `j` is the document at the slot start of frame `first + j`, on the clip's own clock
/// (`source_in` is what a head trim or a slice cut off the front). A clip on no frame at all still has
/// one picture, so that every clip has its input; its overlay's `enable` never shows it.
pub(super) fn clip_sequences<'a>(timeline: &'a Timeline, fmt: &ExportFormat, fx: &[ClipFx]) -> Vec<Option<ClipSeq<'a>>> {
    let clips = timeline.tracks.iter().flat_map(|t| t.clips.iter().map(move |c| (t.kind, c)));
    let mut out: Vec<Option<ClipSeq<'a>>> = clips.clone().map(|_| None).collect();
    if !clips.clone().any(|(_, c)| c.is_generated()) {
        return out;
    }
    let Some(fps) = Rational::from_fps(fmt.fps) else {
        return out;
    };
    let last_output = last_frame_to(fps, timeline.duration(), true).unwrap_or(0);
    let root = raster::root();
    let registered = fonts::snapshot();
    let rect = Rect {
        x: 0,
        y: 0,
        w: fmt.width.max(1),
        h: fmt.height.max(1),
    };
    for (flat, (kind, clip)) in clips.enumerate() {
        let Some(generator) = clip.generator.as_ref().filter(|_| kind == StreamKind::Video) else {
            continue;
        };
        let (start, end) = ClipTiming::new(clip, fx.get(flat).unwrap_or(&ClipFx::default())).window();
        let first = first_frame_from(fps, start);
        let last = last_frame_to(fps, end.min(MAX_SECONDS), true)
            .map(|l| l.min(last_output))
            .filter(|l| *l >= first);
        let count = last.map_or(1, |l| l - first + 1);
        let doc_t0 = clip.generator_time(fps.exact_time(first) - clip.timeline_start);
        let layout = Layout {
            lands_on: first,
            window_start: first,
            first_picture: 0,
            pictures: count,
            doc_t0,
            frames: count,
        };
        let art = Art {
            svg: generator.svg(),
            opacity: 1.0,
            start,
            end,
        };
        out[flat] = Some(ClipSeq {
            seq: seq_of(art, &root, fps, rect, layout, &registered),
            count,
        });
    }
    out
}

/// The sequences of `clips` (from [`clip_sequences`]) a preparation draws.
pub(super) fn clip_seqs<'a>(clips: &[Option<ClipSeq<'a>>]) -> Vec<SvgSeq<'a>> {
    clips.iter().flatten().map(|c| c.seq.clone()).collect()
}

/// A title clip's picture in a still: the document at `doc_t`, over the whole `w` x `h` canvas.
pub(super) fn still_clip_picture<'a>(svg: &'a str, w: u32, h: u32, doc_t: f64) -> StillSvg<'a> {
    let rect = Rect {
        x: 0,
        y: 0,
        w: w.max(1),
        h: h.max(1),
    };
    still_of(svg, rect, doc_t, 1.0, &raster::root(), &fonts::snapshot())
}

// ---- drawing -------------------------------------------------------------------------------------

/// The cancel and the deadline of a preparation, looked at between every step that can take a while.
struct Control<'a> {
    cancel: &'a dyn Fn() -> bool,
    deadline: Option<Instant>,
}

impl Control<'_> {
    fn check(&self) -> Result<()> {
        if (self.cancel)() {
            return Err(Error::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Error::Engine(TOO_HEAVY.to_string()));
        }
        Ok(())
    }
}

/// The last few documents read, most recent first, by a digest of their source.
struct DocCache(Mutex<VecDeque<(String, Arc<SvgDoc>)>>);

impl DocCache {
    const fn new() -> Self {
        DocCache(Mutex::new(VecDeque::new()))
    }

    /// `svg` read, or the read kept from before. The lock is not held while it is read.
    fn get(&self, svg: &str) -> Result<Arc<SvgDoc>> {
        // Reading a document does not depend on the fonts it will be drawn with.
        let key = digest_with(None, |h| hash_svg(h, svg));
        {
            let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(at) = cache.iter().position(|(k, _)| *k == key) {
                let hit = cache.remove(at).expect("just found");
                let doc = hit.1.clone();
                cache.push_front(hit);
                return Ok(doc);
            }
        }
        let doc = Arc::new(SvgDoc::parse(svg).map_err(|e| Error::Engine(format!("an animated title cannot be drawn: {e}")))?);
        let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cache.retain(|(k, _)| *k != key);
        cache.push_front((key, doc.clone()));
        cache.truncate(DOC_CACHE);
        Ok(doc)
    }
}

/// `svg` read, from the few documents kept (a scrub draws one title again and again, and reading
/// a heavy one takes up to a second and a half).
fn parsed(svg: &str) -> Result<Arc<SvgDoc>> {
    static CACHE: DocCache = DocCache::new();
    CACHE.get(svg)
}

/// One stretch of consecutive frames that are the same picture.
#[derive(Debug)]
struct Run {
    first: u64,
    len: u64,
    doc_t: f64,
}

/// The pictures of `seq` that are not on disk, grouped into runs of identical documents: a title
/// that holds still between its moves is drawn once per hold, and the frames after the first are
/// links to it. A picture that is there already (a run cut short earlier, or another preparation of
/// the same title) breaks a run and is not looked at again.
fn runs(seq: &SvgSeq<'_>, doc: &SvgDoc, ctl: &Control<'_>) -> Result<Vec<Run>> {
    if seq.kind == Kind::Held {
        return Ok(if raster::frame_path(&seq.dir, 0).is_file() {
            Vec::new()
        } else {
            vec![Run {
                first: 0,
                len: 1,
                doc_t: 0.0,
            }]
        });
    }
    let animated = doc.is_animated();
    let mut out: Vec<Run> = Vec::new();
    let mut previous: Option<(u64, String)> = None;
    let needed = seq.needed();
    for j in needed.clone() {
        if (j - needed.start).is_multiple_of(SCAN_STRIDE) {
            ctl.check()?;
        }
        if raster::frame_path(&seq.dir, j).is_file() {
            previous = None;
            continue;
        }
        let doc_t = seq.doc_t0 + seq.fps.exact_time(j);
        let text = if animated { doc.at(doc_t) } else { String::new() };
        match (&previous, out.last_mut()) {
            (Some((before_j, before)), Some(run)) if *before_j + 1 == j && *before == text => run.len += 1,
            _ => out.push(Run { first: j, len: 1, doc_t }),
        }
        previous = Some((j, text));
    }
    Ok(out)
}

/// What a worker needs to draw a run of one sequence.
struct Job<'a> {
    seq: &'a SvgSeq<'a>,
    doc: Arc<SvgDoc>,
}

/// Draw one run: its first frame is rendered (unless a previous, interrupted attempt left it —
/// a frame is renamed into place whole, so one that is there is right), the rest linked to it.
fn draw_run(job: &Job<'_>, run: &Run, stop: &AtomicBool) -> Result<()> {
    let (seq, doc) = (job.seq, &job.doc);
    let first = raster::frame_path(&seq.dir, run.first);
    if !first.exists() {
        let picture = raster::render_with(&doc.at(run.doc_t), seq.rect.w, seq.rect.h, seq.opacity, seq.fonts.db())?;
        raster::write_atomic(&first, &raster::encode_frame_png(&picture)?)?;
    }
    for j in run.first + 1..run.first + run.len {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let path = raster::frame_path(&seq.dir, j);
        if !path.exists() {
            raster::link_or_copy(&first, &path)?;
        }
    }
    Ok(())
}

/// How many drawing threads the memory allows: each holds a picture of its sequence's size three
/// times over and, per draw, the widest image of its document decoded twice.
fn workers_for(threads: usize, jobs: &[Job<'_>], units: usize) -> usize {
    let per_worker = jobs
        .iter()
        .map(|j| 12 * u64::from(j.seq.rect.w) * u64::from(j.seq.rect.h) + 8 * j.doc.widest_image_pixels())
        .max()
        .unwrap_or(0)
        .max(1);
    let by_memory = usize::try_from(RENDER_MEMORY / per_worker).unwrap_or(usize::MAX).max(1);
    threads.min(by_memory).clamp(1, units.max(1))
}

/// Whether `seq` is on disk and finished: the marker, and the first and last picture it needs (a
/// marker is a promise made once, and the cache can lose a file under it).
fn is_ready(seq: &SvgSeq<'_>) -> bool {
    let needed = seq.needed();
    raster::is_complete(&seq.dir)
        && raster::frame_path(&seq.dir, needed.start).is_file()
        && raster::frame_path(&seq.dir, needed.end - 1).is_file()
}

/// Have every sequence in `seqs` on disk, drawing what is missing on `threads` threads, and return
/// a [`Pin`] on each so the cache keeps them until the caller (ffmpeg) is done.
///
/// `cancel` is looked at about forty times a second, and between the steps that come before the
/// drawing (reading each document, comparing its frames) as well: it ends the work with
/// [`Error::Cancelled`]. A picture being drawn when it fires is finished first. `deadline` ends it with
/// an error (the stream's "too heavy to preview"). A sequence is marked finished only when the
/// pictures it needs are all there, last, so a stopped run leaves nothing that looks complete — and
/// what it did draw is kept, because a frame is only ever renamed into place whole and its content is
/// fixed by the directory's name, so the next attempt carries on from it. The drawing threads run
/// at the given [`Priority`], and no more of them than memory allows.
pub(super) fn prepare(
    seqs: &[SvgSeq<'_>],
    threads: usize,
    deadline: Option<Instant>,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<Pin>> {
    prepare_as(seqs, threads, deadline, cancel, Priority::Behind)
}

/// Whether the drawing threads give way to the rest of the machine's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Priority {
    /// Below normal while the budget is under 100% ([`cpu::background_thread`]): an export's titles,
    /// drawn before ffmpeg starts, step aside for the interface and for what the user is doing.
    Behind,
    /// As any thread of the process: the playback stream's titles, which Play is waiting for and
    /// which have 10 s to be on disk. The budget still caps how many threads draw.
    Normal,
}

impl Priority {
    /// Call first thing in a drawing thread.
    fn apply_to_this_thread(self) {
        if self == Priority::Behind {
            cpu::background_thread();
        }
    }
}

/// [`prepare`] at the given `priority`.
fn prepare_as(
    seqs: &[SvgSeq<'_>],
    threads: usize,
    deadline: Option<Instant>,
    cancel: &dyn Fn() -> bool,
    priority: Priority,
) -> Result<Vec<Pin>> {
    // Pinned first: a sequence found finished must not be evicted before ffmpeg reads it.
    let pins: Vec<Pin> = seqs.iter().map(|s| Pin::new(&s.dir)).collect();
    let mut todo: Vec<&SvgSeq<'_>> = Vec::new();
    for s in seqs {
        if is_ready(s) {
            raster::touch(&s.dir);
        } else if !raster::is_complete(&s.dir) && raster::has_frames(&s.dir, s.needed()) {
            // Drawn by an earlier stream (from the middle of the title) and never marked: it is all
            // there, and it is the whole of it, or the marker waits for the rest.
            if s.needs_all() {
                raster::mark_complete(&s.dir)?;
            }
            raster::touch(&s.dir);
        } else {
            todo.push(s);
        }
    }
    if todo.is_empty() {
        return Ok(pins);
    }
    // The first picture of the process reads every font the machine has (a macOS install has some
    // hundreds): once, and not the drawing the deadline is for, so it is not counted in it.
    let warming = Instant::now();
    fonts::warm();
    let ctl = Control {
        cancel,
        deadline: deadline.map(|d| d + warming.elapsed()),
    };
    let mut jobs: Vec<Job<'_>> = Vec::new();
    for s in &todo {
        ctl.check()?;
        jobs.push(Job {
            seq: s,
            doc: parsed(s.svg)?,
        });
    }
    let mut units: Vec<(usize, Run)> = Vec::new();
    for (i, job) in jobs.iter().enumerate() {
        units.extend(runs(job.seq, &job.doc, &ctl)?.into_iter().map(|r| (i, r)));
    }
    let workers = workers_for(threads, &jobs, units.len());
    tracing::debug!(
        sequences = todo.len(),
        pictures = units.len(),
        frames = todo.iter().map(|s| s.pictures).sum::<u64>(),
        threads,
        workers,
        "drawing animated titles"
    );

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let failure: Mutex<Option<Error>> = Mutex::new(None);
    let drawn = AtomicU64::new(0);
    let mut halted: Option<Error> = None;
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    priority.apply_to_this_thread();
                    while !stop.load(Ordering::Relaxed) {
                        let Some((i, run)) = units.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        match draw_run(&jobs[*i], run, &stop) {
                            Ok(()) => {
                                drawn.fetch_add(run.len, Ordering::Relaxed);
                            }
                            Err(e) => {
                                failure.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(e);
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                })
            })
            .collect();
        while !handles.iter().all(|w| w.is_finished()) {
            std::thread::sleep(POLL);
            if halted.is_none() && !stop.load(Ordering::Relaxed) {
                if let Err(e) = ctl.check() {
                    halted = Some(e);
                    stop.store(true, Ordering::Relaxed);
                }
            }
        }
    });
    if let Some(e) = failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(e);
    }
    if let Some(e) = halted {
        return Err(e);
    }
    for s in &todo {
        let needed = s.needed();
        let there = raster::has_frames(&s.dir, needed.clone());
        if !there {
            return Err(Error::Engine(
                "an animated title's pictures were removed while they were being drawn".to_string(),
            ));
        }
        // A stream from the middle of a title draws the rest of it; the marker is the whole of it.
        if s.needs_all() || raster::has_frames(&s.dir, 0..s.frames) {
            raster::mark_complete(&s.dir)?;
        }
    }
    if drawn.load(Ordering::Relaxed) > 0 {
        raster::trim();
    }
    Ok(pins)
}

/// Have the titles of an export on disk before its ffmpeg is spawned (once for the lot: a
/// two-pass export and the software retry reuse them). `Ok(None)` when the export was cancelled
/// meanwhile. Waits for the machine's one heavy slot like any foreground job — saying so through
/// `progress` — and gives it back before ffmpeg asks for its own; a cut whose titles are all
/// drawn already never asks.
pub(super) fn prepare_export(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<Option<Vec<Pin>>> {
    if timeline.svg_overlays.is_empty() && timeline.title_clips().next().is_none() {
        return Ok(Some(Vec::new()));
    }
    let cut = export_timeline(timeline, opts);
    let wants_picture =
        !opts.container.is_audio_only() && cut.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    if !wants_picture {
        return Ok(Some(Vec::new()));
    }
    let fmt = export_format(&cut, assets, opts);
    let mut seqs = svg_sequences(&cut, &fmt);
    seqs.extend(clip_seqs(&clip_sequences(&cut, &fmt, &super::transition_fx(&cut, assets))));
    if seqs.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let started = Instant::now();
    let say = |progress: &mut dyn FnMut(ExportProgress), waiting| {
        progress(ExportProgress {
            fraction: 0.0,
            elapsed_secs: started.elapsed().as_secs_f64(),
            eta_secs: None,
            waiting: Some(waiting),
        });
    };
    if seqs.iter().all(is_ready) {
        return prepare(&seqs, 1, None, cancel).map(Some);
    }
    let lease = {
        let mut on_wait = |wait: cpu::Wait| say(progress, wait.message());
        match cpu::lease_waiting(&mut on_wait, cancel) {
            Ok(lease) => lease,
            Err(Error::Cancelled) => return Ok(None),
            Err(e) => return Err(e),
        }
    };
    say(progress, RENDERING);
    match prepare(&seqs, lease.threads(), None, cancel) {
        Ok(pins) => Ok(Some(pins)),
        Err(Error::Cancelled) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The one preparation of the playback stream that may be drawing at a time. A seek, or a second
/// Play, supersedes the stream before it, and that one's `cancel` fires; until it has noticed, the
/// new one waits here instead of starting a second set of threads beside it.
///
/// A value rather than a bare static so a test can have a gate of its own: the process has one
/// ([`preview_gate`]), and every test that went through it would wait for, or be cancelled by, the
/// others running beside it.
pub(super) struct PreviewGate(AtomicBool);

impl PreviewGate {
    pub(super) const fn new() -> Self {
        PreviewGate(AtomicBool::new(false))
    }
}

/// The gate a stream's preparation goes through: the process's, and in a test the calling thread's own
/// (a test that wants two threads on one gate names it: [`prepare_preview_in`]).
fn preview_gate() -> &'static PreviewGate {
    #[cfg(test)]
    {
        thread_local! {
            static OWN: &'static PreviewGate = Box::leak(Box::new(PreviewGate::new()));
        }
        OWN.with(|gate| *gate)
    }
    #[cfg(not(test))]
    {
        static GATE: PreviewGate = PreviewGate::new();
        &GATE
    }
}

struct PreviewSlot<'a>(&'a PreviewGate);

impl<'a> PreviewSlot<'a> {
    fn take(gate: &'a PreviewGate, ctl: &Control<'_>) -> Result<Self> {
        loop {
            if gate
                .0
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(PreviewSlot(gate));
            }
            ctl.check()?;
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for PreviewSlot<'_> {
    fn drop(&mut self) {
        self.0 .0.store(false, Ordering::Release);
    }
}

/// Have the titles of the playback stream (`seqs`, from [`stream_sequences`]) on disk: on the
/// budget's threads at normal priority, no queue — what the user is waiting to see is never gated
/// or stepped aside for — for at most [`PREVIEW_PREPARE_LIMIT`] from now (waiting for an earlier
/// preparation counts; reading the machine's fonts for the first time does not), one preparation at
/// a time, and ended at once by `cancel` (the stream was stopped or superseded).
pub(super) fn prepare_preview(seqs: &[SvgSeq<'_>], cancel: &dyn Fn() -> bool) -> Result<Vec<Pin>> {
    prepare_preview_in(preview_gate(), seqs, cancel)
}

/// [`prepare_preview`] through the given gate.
pub(super) fn prepare_preview_in(gate: &PreviewGate, seqs: &[SvgSeq<'_>], cancel: &dyn Fn() -> bool) -> Result<Vec<Pin>> {
    if seqs.is_empty() {
        return Ok(Vec::new());
    }
    let deadline = Instant::now() + PREVIEW_PREPARE_LIMIT;
    let ctl = Control {
        cancel,
        deadline: Some(deadline),
    };
    let _slot = PreviewSlot::take(gate, &ctl)?;
    prepare_as(seqs, cpu::budget_threads(), Some(deadline), cancel, Priority::Normal)
}

// ---- a picture for a still ------------------------------------------------------------------------

/// One overlay as a single PNG, for a still.
#[derive(Debug, Clone)]
pub(super) struct StillSvg<'a> {
    pub svg: &'a str,
    pub path: PathBuf,
    /// What follows `-i`: `path` as the argv spells it (see [`argv_root`]).
    pub input: String,
    pub rect: Rect,
    /// The document's clock at the still's time.
    pub doc_t: f64,
    pub opacity: f64,
    /// The embedded fonts the picture is drawn with, taken when it was planned (see [`SvgSeq::fonts`]).
    pub fonts: Arc<FontSet>,
}

/// The overlays on screen in a still of `timeline` at `t` (`start <= t < end`, as a text overlay)
/// on a `w` x `h` canvas, in the order they are laid over the picture. Pure.
pub(super) fn still_svgs<'a>(timeline: &'a Timeline, w: u32, h: u32, t: f64) -> Vec<StillSvg<'a>> {
    if timeline.svg_overlays.is_empty() {
        return Vec::new();
    }
    let root = raster::root();
    let registered = fonts::snapshot();
    timeline
        .svg_overlays
        .iter()
        .filter(|o| t >= o.start && t < o.end)
        .filter_map(|o| {
            let rect = rect_px(o, w, h)?;
            Some(still_of(&o.svg, rect, o.doc_time(t), clamped_opacity(o), &root, &registered))
        })
        .collect()
}

/// One picture of `svg` at its clock `doc_t`, `rect` big, named by what decides it.
fn still_of<'a>(
    svg: &'a str,
    rect: Rect,
    doc_t: f64,
    opacity: f64,
    root: &std::path::Path,
    registered: &Arc<FontSet>,
) -> StillSvg<'a> {
    let fonts = registered.named_by([svg]);
    let key = digest(&fonts, |hash| {
        hash.update(b"still");
        hash_svg(hash, svg);
        hash.update(doc_t.to_bits().to_le_bytes());
        hash.update([rect.w.to_le_bytes(), rect.h.to_le_bytes()].concat());
        hash.update(opacity.to_bits().to_le_bytes());
    });
    StillSvg {
        svg,
        path: root.join(format!("still-{key}.png")),
        input: format!("{}/still-{key}.png", argv_root(root)),
        rect,
        doc_t,
        opacity,
        fonts,
    }
}

/// The `-i` of each picture, after the clips' inputs. `-pattern_type none` makes the path one file
/// whatever it holds: a `%d`, or a `[` or `*` that the image demuxer would take for a pattern.
pub(super) fn push_still_inputs(live: &[StillSvg<'_>], args: &mut Vec<String>) {
    for s in live {
        args.extend(["-f", "image2", "-pattern_type", "none", "-i"].map(String::from));
        args.push(s.input.clone());
    }
}

/// The chains that lay `live` over the pad `from`; the pad of the last. `first_input` is the
/// ffmpeg input index of the first picture.
pub(super) fn push_still_chains(live: &[StillSvg<'_>], first_input: usize, from: &str, chains: &mut Vec<String>) -> String {
    let mut cur = from.to_string();
    for (i, s) in live.iter().enumerate() {
        let out = format!("svgov{i}");
        chains.push(format!(
            "[{cur}][{input}:v]overlay=x={x}:y={y}[{out}]",
            input = first_input + i,
            x = s.rect.x,
            y = s.rect.y,
        ));
        cur = out;
    }
    cur
}

/// Have the pictures of a still on disk (drawn here, on this thread: a moment read is never gated)
/// and pinned until the returned pins are dropped. The cache is trimmed only when a picture was
/// written (a scrub that finds everything drawn does not walk the directory), and after it.
pub(super) fn ensure_stills(live: &[StillSvg<'_>]) -> Result<Vec<Pin>> {
    ensure_stills_with(live, &raster::trim)
}

/// [`ensure_stills`] with the cache's trim given (a test counts the calls).
fn ensure_stills_with(live: &[StillSvg<'_>], trim: &dyn Fn()) -> Result<Vec<Pin>> {
    let pins: Vec<Pin> = live.iter().map(|s| Pin::new(&s.path)).collect();
    let mut wrote = false;
    for s in live {
        if s.path.is_file() {
            raster::touch(&s.path);
            continue;
        }
        let doc = parsed(s.svg)?;
        let picture = raster::render_with(&doc.at(s.doc_t), s.rect.w, s.rect.h, s.opacity, s.fonts.db())?;
        raster::write_atomic(&s.path, &raster::encode_frame_png(&picture)?)?;
        wrote = true;
    }
    if wrote {
        trim();
    }
    Ok(pins)
}

/// Whether `path` is a file of the cache (a title's picture). Its name is its content, so a cache
/// key built from an argv needs no `stat` of it.
pub(super) fn in_cache(path: &str) -> bool {
    std::path::Path::new(path).starts_with(raster::root())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cli::{build_filter_complex, build_still_args, push_inputs as push_clip_inputs, StillOutput};
    use crate::engine::test_support::{av_asset, make_clip, single};
    use crate::model::SvgOverlay;
    use uuid::Uuid;

    const PIN: &str = "/kerf-test/svg";

    fn doc(inner: &str) -> String {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 192 108\">{inner}</svg>")
    }

    /// A box that slides across the frame in two seconds.
    fn sliding() -> String {
        doc(
            "<rect id=\"b\" y=\"40\" width=\"40\" height=\"28\" fill=\"#ff3355\"><animate attributeName=\"x\" from=\"0\" to=\"152\" dur=\"2s\" fill=\"freeze\"/></rect>",
        )
    }

    fn overlay(svg: String, start: f64, end: f64) -> SvgOverlay {
        SvgOverlay {
            id: Uuid::new_v4(),
            name: String::new(),
            svg,
            start,
            end,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            opacity: 1.0,
            offset: 0.0,
            preset: None,
            template: None,
        }
    }

    fn fmt(w: u32, h: u32, fps: f64) -> ExportFormat {
        ExportFormat {
            width: w,
            height: h,
            fps,
            ..ExportFormat::default()
        }
    }

    /// A 10 s cut of one clip with the given titles.
    fn cut(titles: Vec<SvgOverlay>) -> (Timeline, Vec<Asset>) {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let mut tl = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        tl.svg_overlays = titles;
        (tl, vec![asset])
    }

    #[test]
    fn a_title_is_the_frames_the_graphs_between_holds_on() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (mut tl, _) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            let seqs = svg_sequences(&tl, &f);
            let s = &seqs[0];
            // 30..=90 inclusive: the overlay is on at both of its ends.
            assert_eq!((s.first_frame, s.frames), (30, 61));
            assert_eq!(s.doc_t0, 0.0);
            assert_eq!((s.rect.x, s.rect.y, s.rect.w, s.rect.h), (0, 0, 1920, 1080));
            assert!(
                s.input.starts_with("/kerf-test/svg/") && s.input.ends_with("/%06d.png"),
                "{}",
                s.input
            );
            assert_eq!((s.kind, s.first_picture, s.pictures, s.frames), (Kind::Frames, 0, 61, 61));

            // Between two frames: the first frame at or after the start, the last at or before the end.
            tl.svg_overlays[0].start = 1.01;
            tl.svg_overlays[0].end = 2.99;
            let s = &svg_sequences(&tl, &f)[0];
            assert_eq!((s.first_frame, s.frames), (31, 59));
            assert!((s.doc_t0 - (31.0 / 30.0 - 1.01)).abs() < 1e-12, "{}", s.doc_t0);

            // What a slice cut off the front has already run.
            tl.svg_overlays[0].offset = 0.5;
            let s = &svg_sequences(&tl, &f)[0];
            assert!((s.doc_t0 - (0.5 + 31.0 / 30.0 - 1.01)).abs() < 1e-12);

            // The output ends at the cut: nothing is drawn for frames that are not made.
            tl.svg_overlays[0].end = 40.0;
            let s = &svg_sequences(&tl, &f)[0];
            assert_eq!(s.first_frame + s.frames, 300, "frames 31..=299 of a ten-second cut");
        });
    }

    #[test]
    fn a_frame_rate_that_is_not_whole_keeps_its_own_grid() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (tl, _) = cut(vec![overlay(sliding(), 1.0, 2.0)]);
            let s = &svg_sequences(&tl, &fmt(1920, 1080, 29.97))[0];
            assert_eq!((s.fps.num, s.fps.den), (2997, 100));
            // frame_time(k) = k * 100/2997 >= 1.0 first at k = 30 (1.001 s), and <= 2.0 up to k = 59 (1.968 s) -- 60 is 2.002.
            assert_eq!((s.first_frame, s.frames), (30, 30));
            let s = &svg_sequences(&tl, &fmt(1920, 1080, 24000.0 / 1001.0))[0];
            assert_eq!((s.fps.num, s.fps.den), (24000, 1001));
        });
    }

    #[test]
    fn a_key_names_what_decides_the_picture_and_not_where_it_is_laid() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (mut tl, _) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            let key = |tl: &Timeline, f: &ExportFormat| svg_sequences(tl, f)[0].dir.clone();
            let base = key(&tl, &f);
            assert_eq!(key(&tl, &f), base, "stable");
            // Moving it changes the argv's overlay position, not its frames.
            tl.svg_overlays[0].x = 0.25;
            tl.svg_overlays[0].y = 0.1;
            assert_eq!(key(&tl, &f), base);
            assert_eq!(svg_sequences(&tl, &f)[0].rect.x, 480);
            // Anything that changes a pixel is another sequence.
            let mut other = tl.clone();
            other.svg_overlays[0].width = 0.5;
            assert_ne!(key(&other, &f), base, "size");
            let mut other = tl.clone();
            other.svg_overlays[0].opacity = 0.5;
            assert_ne!(key(&other, &f), base, "opacity");
            let mut other = tl.clone();
            other.svg_overlays[0].svg = sliding().replace("#ff3355", "#ff3356");
            assert_ne!(key(&other, &f), base, "artwork");
            let mut other = tl.clone();
            other.svg_overlays[0].offset = 0.5;
            assert_ne!(key(&other, &f), base, "where in the animation it starts");
            assert_ne!(key(&tl, &fmt(1920, 1080, 25.0)), base, "rate");
            assert_ne!(key(&tl, &fmt(960, 540, 30.0)), base, "frame size");
            // Starting a frame later with the same clock at the first picture is the same pictures
            // (a rate whose frame is exact in binary, so the start is a frame boundary to the bit).
            let exact = fmt(1920, 1080, 32.0);
            tl.svg_overlays[0].x = 0.0;
            tl.svg_overlays[0].y = 0.0;
            let mut shifted = tl.clone();
            shifted.svg_overlays[0].start += 1.0 / 32.0;
            shifted.svg_overlays[0].end += 1.0 / 32.0;
            assert_eq!(svg_sequences(&shifted, &exact)[0].first_frame, 33);
            assert_eq!(key(&shifted, &exact), key(&tl, &exact));
        });
    }

    /// `sliding()` with text in the family `family`.
    fn typed(family: &str) -> String {
        doc(&format!(
            "<rect id=\"b\" y=\"40\" width=\"40\" height=\"28\" fill=\"#ff3355\"><animate attributeName=\"x\" from=\"0\" to=\"152\" dur=\"2s\" fill=\"freeze\"/></rect><text x=\"2\" y=\"20\" font-family=\"{family}\">a</text>"
        ))
    }

    #[test]
    fn the_fonts_a_title_names_are_part_of_its_picture_keys_and_none_leaves_them_alone() {
        use crate::fonts::{self, EmbeddedFont};
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (tl, _) = cut(vec![overlay(typed("Zeist"), 1.0, 3.0)]);
            let held = {
                let mut still = tl.clone();
                still.svg_overlays[0].svg = doc("<text font-family=\"Zeist\">a</text>");
                still
            };
            let f = fmt(1920, 1080, 30.0);
            let keys = |tl: &Timeline| {
                (
                    svg_sequences(tl, &f)[0].dir.clone(),
                    still_svgs(tl, 960, 540, 2.0).first().map(|s| s.path.clone()),
                )
            };
            assert_eq!(fonts::embedded_digest(), None);
            let (bare, bare_held) = (keys(&tl), keys(&held));
            // With nothing embedded a key is what it always was.
            assert_eq!(
                svg_sequences(&tl, &f)[0].dir.file_name().unwrap().to_str().unwrap().len(),
                32,
                "32 hex digits"
            );
            let a = EmbeddedFont::new("Zeist", fonts::shipped_font_renamed(true, "Zeist"));
            assert!(fonts::register_embedded(std::slice::from_ref(&a)));
            let (with_a, held_a) = (keys(&tl), keys(&held));
            assert_ne!(with_a.0, bare.0, "a sequence drawn without the font is not served");
            assert_ne!(with_a.1, bare.1, "nor a still");
            assert_ne!(held_a.0, bare_held.0, "nor a held picture");
            // The same set is the same key (a project reopened finds its frames), another set another.
            assert!(!fonts::register_embedded(std::slice::from_ref(&a)));
            assert_eq!(keys(&tl), with_a);
            // A font the title does not name is not in its key, however many the project holds.
            let b = EmbeddedFont::new("Zeyst", fonts::shipped_font_renamed(false, "Zeyst"));
            assert!(fonts::register_embedded(std::slice::from_ref(&b)));
            assert_eq!(keys(&tl), with_a, "the project's other font changes nothing it draws");
            let both = {
                let mut both = tl.clone();
                both.svg_overlays[0].svg = typed("Zeist, Zeyst");
                both
            };
            assert_ne!(keys(&both).0, with_a.0, "a title that names both is keyed by both");
            // Back to nothing: the original keys, byte for byte.
            assert!(fonts::replace_embedded(Vec::new()));
            assert_eq!(keys(&tl), bare);
            assert_eq!(keys(&held), bare_held);
        });
    }

    #[test]
    fn a_font_held_for_a_proposal_does_not_move_a_title_that_does_not_name_it() {
        use crate::fonts::{self, EmbeddedFont};
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (live, _) = cut(vec![
                overlay(typed("Geist"), 1.0, 3.0),
                overlay(typed("Sans Serif"), 1.0, 3.0),
            ]);
            let f = fmt(1920, 1080, 30.0);
            let dirs = |tl: &Timeline| svg_sequences(tl, &f).iter().map(|s| s.dir.clone()).collect::<Vec<_>>();
            let before = dirs(&live);
            let stills_before = still_svgs(&live, 960, 540, 2.0)
                .iter()
                .map(|s| s.path.clone())
                .collect::<Vec<_>>();
            assert!(fonts::register_embedded(&[EmbeddedFont::new(
                "Zeist",
                fonts::shipped_font_renamed(true, "Zeist")
            )]));
            assert_eq!(dirs(&live), before, "no live title names it: every picture is where it was");
            let stills = still_svgs(&live, 960, 540, 2.0);
            assert_eq!(stills.iter().map(|s| s.path.clone()).collect::<Vec<_>>(), stills_before);
            assert!(stills.iter().all(|s| s.fonts.digest().is_none()));
            // The proposal's title names it, and is drawn with it.
            let (proposal, _) = cut(vec![overlay(typed("Geist"), 1.0, 3.0), overlay(typed("Zeist"), 1.0, 3.0)]);
            let seqs = svg_sequences(&proposal, &f);
            assert_eq!(seqs[0].dir, before[0]);
            assert_ne!(seqs[1].dir, before[1]);
            assert!(seqs[0].fonts.digest().is_none() && seqs[1].fonts.digest().is_some());
            fonts::replace_embedded(Vec::new());
        });
    }

    #[test]
    fn a_plan_draws_with_the_fonts_its_keys_name_whatever_the_registry_does_next() {
        use crate::fonts::{self, EmbeddedFont};
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (tl, _) = cut(vec![overlay(typed("Zeist"), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            assert!(fonts::register_embedded(&[EmbeddedFont::new(
                "Zeist",
                fonts::shipped_font_renamed(true, "Zeist")
            )]));
            let seqs = svg_sequences(&tl, &f);
            let stills = still_svgs(&tl, 960, 540, 2.0);
            let digest = seqs[0].fonts.digest().map(str::to_string);
            assert!(digest.is_some());
            // Another project is opened (or the font dropped) between planning and drawing.
            assert!(fonts::replace_embedded(Vec::new()));
            assert_eq!(fonts::embedded_digest(), None);
            let has_zeist = |db: &fontdb::Database| db.faces().any(|face| face.families.iter().any(|(n, _)| n == "Zeist"));
            assert!(
                has_zeist(&seqs[0].fonts.db()),
                "the sequence is drawn with the set its key names"
            );
            assert!(has_zeist(&stills[0].fonts.db()), "and so is the still");
            assert_eq!(seqs[0].fonts.digest().map(str::to_string), digest);
            assert!(!has_zeist(&fonts::svg_fontdb()));
        });
    }

    #[test]
    fn a_percent_in_the_cache_path_is_escaped_for_the_image_demuxer() {
        crate::svg::raster::with_pinned_dir("/home/a%b/svg", || {
            let (tl, _) = cut(vec![overlay(sliding(), 0.0, 1.0)]);
            let s = &svg_sequences(&tl, &fmt(1920, 1080, 30.0))[0];
            assert!(s.input.starts_with("/home/a%%b/svg/"), "{}", s.input);
            assert!(s.input.ends_with("/%06d.png"));
            assert_eq!(s.input.matches('%').count(), 3, "one escaped pair and one frame number");
        });
    }

    #[test]
    fn a_title_that_cannot_be_seen_is_left_out() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let f = fmt(1920, 1080, 30.0);
            let mut off = overlay(sliding(), 1.0, 3.0);
            off.x = 1.5;
            let mut backwards = overlay(sliding(), 3.0, 1.0);
            backwards.x = 0.0;
            let mut after = overlay(sliding(), 20.0, 22.0);
            after.x = 0.0;
            let mut huge = overlay(sliding(), 1.0, 3.0);
            huge.width = 40.0;
            huge.height = 40.0;
            let mut nan = overlay(sliding(), 1.0, 3.0);
            nan.x = f64::NAN;
            let (tl, _) = cut(vec![off, backwards, after, huge, nan, overlay(sliding(), 1.0, 3.0)]);
            let seqs = svg_sequences(&tl, &f);
            assert_eq!(
                seqs.len(),
                1,
                "off the frame, backwards, after the cut, too large, not a number"
            );
            // Partly off the frame is still drawn, at its negative corner.
            let mut partly = overlay(sliding(), 1.0, 3.0);
            partly.x = -0.25;
            partly.width = 0.5;
            let (tl, _) = cut(vec![partly]);
            let s = &svg_sequences(&tl, &f)[0];
            assert_eq!((s.rect.x, s.rect.w), (-480, 960));
        });
    }

    #[test]
    fn an_empty_list_gives_no_sequences_and_no_stills() {
        let (tl, _) = cut(vec![]);
        assert!(svg_sequences(&tl, &fmt(1920, 1080, 30.0)).is_empty());
        assert!(still_svgs(&tl, 960, 540, 1.0).is_empty());
    }

    #[test]
    fn the_graph_lays_the_titles_over_the_clips_and_under_the_text() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (mut tl, assets) = cut(vec![
                overlay(sliding(), 1.0, 3.0),
                overlay(sliding().replace("40", "41"), 4.0, 5.0),
            ]);
            tl.overlays.push(crate::model::TextOverlay::new("Hi", 0.0, 5.0));
            let f = fmt(1920, 1080, 30.0);
            let opts = ExportOptions::default();
            let mut args = Vec::new();
            let plan = push_clip_inputs(&tl, &assets, &f, &opts, &mut args).unwrap();
            let seqs = svg_sequences(&tl, &f);
            push_inputs(&seqs, &f, &mut args);
            let joined = args.join(" ");
            assert_eq!(
                joined.matches("-f image2 -framerate 30 -i /kerf-test/svg/").count(),
                2,
                "{joined}"
            );
            let graph = build_filter_complex(&tl, &assets, &f, tl.duration(), &opts, true, false, &plan).filter;
            let chains: Vec<&str> = graph.split(';').collect();
            let at = |needle: &str| {
                chains
                    .iter()
                    .position(|c| c.contains(needle))
                    .unwrap_or_else(|| panic!("{needle}: {graph}"))
            };
            // The clip's overlay ends on the pad the titles start from, and they end where the text starts.
            assert!(chains[at("[v0]overlay=")].ends_with("[vsvg]"), "{graph}");
            assert_eq!(chains[at("[1:v]settb")], "[1:v]settb=expr=1/30,setpts=N+30[svg0]");
            assert_eq!(
                chains[at("[vsvg][svg0]")],
                "[vsvg][svg0]overlay=x=0:y=0:eof_action=pass:enable='between(t,1,3)'[vsv0]"
            );
            assert_eq!(chains[at("[2:v]settb")], "[2:v]settb=expr=1/30,setpts=N+120[svg1]");
            assert_eq!(
                chains[at("[vsv0][svg1]")],
                "[vsv0][svg1]overlay=x=0:y=0:eof_action=pass:enable='between(t,4,5)'[vtext]"
            );
            assert!(chains[at("[vtext]drawtext")].ends_with("[outv]"), "{graph}");
            assert!(at("[vsvg][svg0]") > at("[v0]overlay=") && at("[vtext]drawtext") > at("[vsv0][svg1]"));
        });
    }

    #[test]
    fn the_pad_routing_follows_what_comes_after_the_titles() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (tl, assets) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            let build = |opts: &ExportOptions| {
                let mut args = Vec::new();
                let plan = push_clip_inputs(&tl, &assets, &f, opts, &mut args).unwrap();
                build_filter_complex(&tl, &assets, &f, tl.duration(), opts, true, false, &plan).filter
            };
            let plain = build(&ExportOptions::default());
            assert!(
                plain.contains("[vsv") || plain.contains("overlay=x=0:y=0:eof_action=pass:enable='between(t,1,3)'[outv]"),
                "{plain}"
            );
            assert!(!plain.contains("[vtext]") && !plain.contains("[vcomp]"));
            let gif = build(&ExportOptions {
                container: crate::engine::Container::Gif,
                ..ExportOptions::default()
            });
            assert!(gif.contains("between(t,1,3)'[vcomp]"), "{gif}");
            let vaapi = build(&ExportOptions {
                video_codec: Some("h264_vaapi".into()),
                ..ExportOptions::default()
            });
            assert!(vaapi.contains("between(t,1,3)'[vhw]"), "{vaapi}");
        });
    }

    #[test]
    fn a_graph_without_titles_has_not_a_byte_more() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (mut tl, assets) = cut(vec![]);
            tl.overlays.push(crate::model::TextOverlay::new("Hi", 0.0, 5.0));
            let f = fmt(1920, 1080, 30.0);
            let opts = ExportOptions::default();
            let mut args = Vec::new();
            let plan = push_clip_inputs(&tl, &assets, &f, &opts, &mut args).unwrap();
            let seqs = svg_sequences(&tl, &f);
            let before = args.clone();
            push_inputs(&seqs, &f, &mut args);
            assert_eq!(args, before);
            let graph = build_filter_complex(&tl, &assets, &f, tl.duration(), &opts, true, false, &plan).filter;
            assert!(!graph.contains("svg") && !graph.contains("settb"), "{graph}");
            assert!(graph.contains("[vov") || graph.contains("[vtext]"));
        });
    }

    #[test]
    fn a_still_has_the_titles_live_at_its_time_as_one_picture_each() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (mut tl, assets) = cut(vec![overlay(sliding(), 1.0, 3.0), overlay(sliding(), 5.0, 6.0)]);
            tl.overlays.push(crate::model::TextOverlay::new("Hi", 0.0, 9.0));
            let out = StillOutput::JpegPipe { quality: 4 };
            let build = |t: f64| build_still_args(&tl, &assets, &ExportOptions::default(), t, 960, None, &out).unwrap();
            assert_eq!(
                build(0.5).iter().filter(|a| a.starts_with("/kerf-test/svg/still-")).count(),
                0
            );
            let live = build(2.0);
            let pics: Vec<&String> = live.iter().filter(|a| a.starts_with("/kerf-test/svg/still-")).collect();
            assert_eq!(pics.len(), 1);
            let graph = &live[live.iter().position(|a| a == "-filter_complex").unwrap() + 1];
            assert!(graph.contains("[ov0][1:v]overlay=x=0:y=0[svgov0]"), "{graph}");
            assert!(graph.contains("[svgov0]drawtext="), "titles are under the text: {graph}");
            // The end is not on screen in a still (the export draws it; a text overlay is the same).
            assert_eq!(build(3.0).iter().filter(|a| a.contains("still-")).count(), 0);
            // Another moment of the animation is another picture.
            let later = build(2.5);
            assert_ne!(
                later.iter().find(|a| a.contains("still-")),
                live.iter().find(|a| a.contains("still-"))
            );
            // The same moment is the same file.
            assert_eq!(build(2.0), live);
        });
    }

    #[test]
    fn a_still_is_drawn_at_the_overlays_clock_and_the_stills_size() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let mut o = overlay(sliding(), 1.0, 3.0);
            o.offset = 0.5;
            let (mut tl, _) = cut(vec![o]);
            let live = still_svgs(&tl, 960, 540, 1.5);
            assert_eq!(live[0].doc_t, 1.0, "0.5 s in, and half a second had elapsed before the start");
            assert_eq!((live[0].rect.w, live[0].rect.h), (960, 540));
            tl.svg_overlays[0].x = 0.5;
            tl.svg_overlays[0].width = 0.25;
            tl.svg_overlays[0].height = 0.25;
            let live = still_svgs(&tl, 960, 540, 1.5);
            assert_eq!((live[0].rect.x, live[0].rect.w, live[0].rect.h), (480, 240, 135));
        });
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-svg-layer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A cut with one title that covers a tenth of the frame (small pictures: these tests draw them).
    fn titled(svg: &str, start: f64, end: f64) -> (Timeline, Vec<Asset>) {
        let mut o = overlay(svg.to_string(), start, end);
        o.width = 0.1;
        o.height = 0.1;
        cut(vec![o])
    }

    #[test]
    fn prepare_draws_every_frame_once_and_marks_the_sequence_finished() {
        let root = scratch("prepare");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled(&sliding(), 1.0, 2.0);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let s = &seqs[0];
            assert_eq!(s.frames, 11);
            assert!(!raster::is_complete(&s.dir));
            let pins = prepare(&seqs, 3, None, &|| false).unwrap();
            assert!(raster::is_complete(&s.dir));
            for j in 0..s.frames {
                let png = std::fs::read(raster::frame_path(&s.dir, j)).unwrap();
                let p = tiny_skia::Pixmap::decode_png(&png).unwrap();
                assert_eq!((p.width(), p.height()), (32, 18));
            }
            assert!(!raster::frame_path(&s.dir, s.frames).exists());
            // Frames of different moments differ, and the box moves right.
            let x_of = |j: u64| {
                let p = tiny_skia::Pixmap::decode_png(&std::fs::read(raster::frame_path(&s.dir, j)).unwrap()).unwrap();
                (0..32)
                    .find(|&x| p.pixel(x, 9).is_some_and(|c| c.alpha() > 128))
                    .unwrap_or(99)
            };
            assert!(
                x_of(0) < x_of(5) && x_of(5) < x_of(10),
                "{} {} {}",
                x_of(0),
                x_of(5),
                x_of(10)
            );
            drop(pins);
            // A second call finds it finished and draws nothing: remove a frame the marker vouches for.
            let gone = raster::frame_path(&s.dir, 3);
            std::fs::remove_file(&gone).unwrap();
            let _pins = prepare(&seqs, 3, None, &|| false).unwrap();
            assert!(!gone.exists(), "a finished sequence is trusted");
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_title_that_holds_still_is_drawn_once_and_linked() {
        let root = scratch("links");
        let f = fmt(320, 180, 10.0);
        // Text that mentions an animation the reader dropped (animateMotion is not supported) cannot be
        // told from one that moves without reading it, so it is a sequence: one picture, nine links.
        let (tl, _) = titled(
            &doc("<rect width=\"192\" height=\"108\" fill=\"#123456\"><animateMotion dur=\"1s\" path=\"M0,0 L1,1\"/></rect>"),
            0.0,
            0.9,
        );
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let _pins = prepare(&seqs, 4, None, &|| false).unwrap();
            let first = raster::frame_path(&seqs[0].dir, 0);
            let bytes = std::fs::read(&first).unwrap();
            for j in 0..seqs[0].frames {
                assert_eq!(std::fs::read(raster::frame_path(&seqs[0].dir, j)).unwrap(), bytes);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert_eq!(
                    std::fs::metadata(&first).unwrap().nlink(),
                    seqs[0].frames,
                    "hard links to one file"
                );
            }
        });
        // A fade that finishes in the first half second and then holds: six pictures for ten frames.
        let held = doc(
            "<rect width=\"192\" height=\"108\" fill=\"#fff\" opacity=\"0\"><animate attributeName=\"opacity\" from=\"0\" to=\"1\" dur=\"0.5s\" fill=\"freeze\"/></rect>",
        );
        let (tl, _) = titled(&held, 0.0, 0.9);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let doc = SvgDoc::parse(seqs[0].svg).unwrap();
            let ctl = Control {
                cancel: &|| false,
                deadline: None,
            };
            let r = runs(&seqs[0], &doc, &ctl).unwrap();
            assert_eq!(r.len(), 6, "frames 0..=4 differ, 5..=9 are the same");
            assert_eq!((r[5].first, r[5].len), (5, 5));
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_stops_when_asked_and_carries_on_where_it_stopped() {
        let root = scratch("resume");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled(&sliding(), 0.0, 1.5);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let s = &seqs[0];
            let err = prepare(&seqs, 2, None, &|| true).unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{err}");
            assert!(!raster::is_complete(&s.dir), "a stopped run is not finished");
            let drawn = (0..s.frames).filter(|j| raster::frame_path(&s.dir, *j).exists()).count();
            // Whatever was drawn is whole, and the next run keeps it.
            let before: Vec<_> = (0..s.frames)
                .filter(|j| raster::frame_path(&s.dir, *j).exists())
                .map(|j| {
                    (
                        j,
                        std::fs::metadata(raster::frame_path(&s.dir, j)).unwrap().modified().unwrap(),
                    )
                })
                .collect();
            assert_eq!(before.len(), drawn);
            let _pins = prepare(&seqs, 2, None, &|| false).unwrap();
            assert!(raster::is_complete(&s.dir));
            for (j, modified) in before {
                assert_eq!(
                    std::fs::metadata(raster::frame_path(&s.dir, j)).unwrap().modified().unwrap(),
                    modified,
                    "frame {j} was kept"
                );
            }
            assert_eq!(
                (0..s.frames).filter(|j| raster::frame_path(&s.dir, *j).exists()).count() as u64,
                s.frames
            );
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_deadline_ends_the_preparing_with_the_streams_words() {
        let root = scratch("deadline");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled(&sliding(), 0.0, 1.5);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let err = prepare(&seqs, 1, Some(Instant::now()), &|| false).unwrap_err();
            assert!(err.to_string().contains("too heavy to preview"), "{err}");
            assert!(!raster::is_complete(&seqs[0].dir));
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_pinned_sequence_survives_the_cache_budget() {
        let root = scratch("pin");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled(&sliding(), 0.0, 0.5);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let pins = prepare(&seqs, 2, None, &|| false).unwrap();
            raster::trim_to(&root, 0, &|_| false);
            assert!(raster::is_complete(&seqs[0].dir), "the pins the preparation returned hold it");
            drop(pins);
            raster::trim_to(&root, 0, &|_| false);
            assert!(!seqs[0].dir.exists());
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_still_is_drawn_to_a_file_the_argv_names() {
        let root = scratch("still");
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let (tl, assets) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let out = StillOutput::JpegPipe { quality: 4 };
            let args = build_still_args(&tl, &assets, &ExportOptions::default(), 2.0, 192, None, &out).unwrap();
            let path = args.iter().find(|a| a.contains("still-")).unwrap().clone();
            let live = still_svgs(&tl, 192, 108, 2.0);
            assert_eq!(live[0].input, path);
            assert_eq!(live[0].path, PathBuf::from(&path));
            assert!(!live[0].path.exists());
            let pins = ensure_stills(&live).unwrap();
            let p = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!((p.width(), p.height()), (192, 108));
            assert!(p.pixels().iter().any(|c| c.alpha() == 255), "the box is there");
            let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
            drop(ensure_stills(&live).unwrap());
            assert!(std::fs::metadata(&path).unwrap().modified().unwrap() >= stamp);
            drop(pins);
            assert!(in_cache(&path));
            assert!(!in_cache("/media/clip.mp4"));
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_title_that_does_not_draw_is_an_error_not_a_hang() {
        let root = scratch("broken");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled("<svg", 0.0, 1.0);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let err = prepare(&seqs, 2, None, &|| false).unwrap_err();
            assert!(err.to_string().contains("cannot be drawn"), "{err}");
        });
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- a document that cannot move ------------------------------------------------------------

    /// Artwork with nothing in it that moves.
    fn still_art() -> String {
        doc("<rect width=\"192\" height=\"108\" fill=\"#123456\"/>")
    }

    #[test]
    fn a_document_that_cannot_move_is_one_picture_ffmpeg_loops() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (mut tl, _) = cut(vec![overlay(still_art(), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            let s = svg_sequences(&tl, &f).remove(0);
            assert_eq!(
                (s.kind, s.frames, s.first_picture, s.pictures, s.first_frame),
                (Kind::Held, 1, 0, 1, 30)
            );
            assert!(
                s.input.starts_with("/kerf-test/svg/") && s.input.ends_with("/000000.png"),
                "{}",
                s.input
            );
            let mut args = Vec::new();
            push_inputs(std::slice::from_ref(&s), &f, &mut args);
            assert_eq!(
                args.join(" "),
                format!("-f image2 -pattern_type none -loop 1 -framerate 30 -i {}", s.input)
            );
            // Counted into the output the same way as a sequence's first picture.
            let mut chains = Vec::new();
            push_chains(std::slice::from_ref(&s), 1, "vsvg", "outv", &mut chains);
            assert_eq!(chains[0], "[1:v]settb=expr=1/30,setpts=N+30[svg0]");
            assert!(chains[1].ends_with("enable='between(t,1,3)'[outv]"), "{chains:?}");

            // What decides the picture is the artwork, its size and its opacity: not when the title
            // starts, how long it runs, where in its clock it starts, or the rate.
            let key = |tl: &Timeline, f: &ExportFormat| svg_sequences(tl, f)[0].dir.clone();
            let base = key(&tl, &f);
            tl.svg_overlays[0].start = 2.0;
            tl.svg_overlays[0].end = 9.0;
            tl.svg_overlays[0].offset = 0.7;
            tl.svg_overlays[0].x = 0.3;
            assert_eq!(key(&tl, &f), base);
            assert_eq!(key(&tl, &fmt(1920, 1080, 25.0)), base);
            assert_ne!(key(&tl, &fmt(960, 540, 30.0)), base, "size");
            let mut other = tl.clone();
            other.svg_overlays[0].opacity = 0.5;
            assert_ne!(key(&other, &f), base, "opacity");
            other.svg_overlays[0].opacity = 1.0;
            other.svg_overlays[0].svg = still_art().replace("#123456", "#123457");
            assert_ne!(key(&other, &f), base, "artwork");
            // A title that moves is a sequence, as ever.
            let (moving, _) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            assert_eq!(svg_sequences(&moving, &f)[0].kind, Kind::Frames);
        });
    }

    #[test]
    fn a_percent_in_the_path_of_a_held_picture_is_not_doubled() {
        // `-pattern_type none` reads the path as it is: it is the sequence's `%06d` that needs `%%`.
        crate::svg::raster::with_pinned_dir("/home/a%b/svg", || {
            let (tl, _) = cut(vec![overlay(still_art(), 0.0, 1.0)]);
            let s = &svg_sequences(&tl, &fmt(1920, 1080, 30.0))[0];
            assert!(
                s.input.starts_with("/home/a%b/svg/") && !s.input.contains("%%"),
                "{}",
                s.input
            );
        });
    }

    #[test]
    fn a_held_title_is_one_file_and_found_drawn_the_next_time() {
        let root = scratch("held");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled(&still_art(), 0.0, 0.9);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let dir = seqs[0].dir.clone();
            let pins = prepare(&seqs, 3, None, &|| false).unwrap();
            let mut files: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            files.sort();
            assert_eq!(files, ["000000.png", "complete"], "one picture, not one a frame");
            let p = tiny_skia::Pixmap::decode_png(&std::fs::read(raster::frame_path(&dir, 0)).unwrap()).unwrap();
            assert_eq!((p.width(), p.height()), (32, 18));
            assert!(p.pixels().iter().all(|c| c.alpha() == 255));
            drop(pins);
            // Trimmed to half a second later and shifted: the same picture, found drawn.
            let (later, _) = titled(&still_art(), 0.4, 0.5);
            let again = svg_sequences(&later, &f);
            assert_eq!(again[0].dir, dir);
            assert!(is_ready(&again[0]));
        });
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- positions that are not on the timeline ------------------------------------------------

    #[test]
    fn a_title_at_a_position_nowhere_near_the_cut_is_left_out_without_a_search_for_its_frame() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let f = fmt(1920, 1080, 30.0);
            for (start, end, drawn) in [
                (1e300, 1e301, false),
                (f64::MAX, f64::MAX, false),
                (1e18, 1e19, false),
                (20.0, 1e300, false),
                (10.0, 11.0, false),
                (-1e300, 3.0, true),
                (1.0, 1e300, true),
                (1.0, f64::MAX, true),
                (f64::NEG_INFINITY, 3.0, false),
                (1.0, f64::INFINITY, false),
                (f64::NAN, 3.0, false),
            ] {
                let (tl, _) = cut(vec![overlay(sliding(), start, end)]);
                let seqs = svg_sequences(&tl, &f);
                assert_eq!(!seqs.is_empty(), drawn, "{start} .. {end}");
            }
            // A title that runs on past the cut is drawn up to the cut's last frame.
            let (tl, _) = cut(vec![overlay(sliding(), 9.0, 1e300)]);
            let s = &svg_sequences(&tl, &f)[0];
            assert_eq!(s.first_frame + s.frames, 300);
            // The helpers hold on their own too: nothing loops, nothing overflows.
            let fps = Rational::from_fps(30.0).unwrap();
            assert_eq!(first_frame_from(fps, f64::NAN), 0);
            assert_eq!(first_frame_from(fps, -5.0), 0);
            assert!(first_frame_from(fps, f64::MAX) > 0);
            assert_eq!(last_frame_to(fps, f64::NAN, false), None);
            assert_eq!(last_frame_to(fps, -1.0, false), None);
            assert!(last_frame_to(fps, f64::INFINITY, true).is_some());
            assert!(last_frame_to(fps, 1e300, false).is_some());
        });
    }

    // ---- the playback stream -----------------------------------------------------------------------

    /// What the stream from `start` reads of `tl`'s first title: the directory, the output frame the
    /// first picture lands on, the pictures read and the sequence's length.
    fn streamed(tl: &Timeline, start: f64, f: &ExportFormat) -> Vec<(PathBuf, u64, Range<u64>, u64)> {
        let sliced = tl.slice(start, tl.duration());
        stream_sequences(&tl.svg_overlays, tl.duration(), &sliced, start, f)
            .into_iter()
            .map(|s| (s.dir.clone(), s.first_frame, s.needed(), s.frames))
            .collect()
    }

    #[test]
    fn the_stream_reads_the_titles_own_sequence_from_wherever_the_playhead_is() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (tl, _) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            // Frames 30..=90 of the cut are the title: 61 pictures, the export's.
            let whole = svg_sequences(&tl, &f).remove(0);
            assert_eq!((whole.first_frame, whole.frames), (30, 61));

            // From the start the stream is the export.
            assert_eq!(streamed(&tl, 0.0, &f), [(whole.dir.clone(), 30, 0..61, 61)]);
            // From the middle (2 s is frame 60): the same directory, the rest of it, from the first frame.
            assert_eq!(streamed(&tl, 2.0, &f), [(whole.dir.clone(), 0, 30..61, 61)]);
            // From before the title: all of it, landing 15 frames in.
            assert_eq!(streamed(&tl, 0.5, &f), [(whole.dir.clone(), 15, 0..61, 61)]);
            // From a tenth of a second before it ends: the last four pictures (the end is closed).
            assert_eq!(streamed(&tl, 2.9, &f), [(whole.dir, 0, 57..61, 61)]);
            // Once it is over there is nothing to read.
            assert!(streamed(&tl, 3.5, &f).is_empty());
            assert!(streamed(&tl, 9.5, &f).is_empty());

            // The graph: `-start_number` names the first picture, and the first one is counted onto
            // the output's frame it lands on.
            let sliced = tl.slice(2.0, tl.duration());
            let seqs = stream_sequences(&tl.svg_overlays, tl.duration(), &sliced, 2.0, &f);
            let mut args = Vec::new();
            push_inputs(&seqs, &f, &mut args);
            assert_eq!(
                args.join(" "),
                format!("-f image2 -framerate 30 -start_number 30 -i {}", seqs[0].input)
            );
            let mut chains = Vec::new();
            push_chains(&seqs, 1, "vsvg", "outv", &mut chains);
            assert_eq!(chains[0], "[1:v]settb=expr=1/30,setpts=N+0[svg0]");
            assert!(chains[1].contains("between(t,0,1)"), "the sliced window: {chains:?}");
            // The start of the title has no `-start_number` (neutral values are left out).
            let sliced = tl.slice(0.0, tl.duration());
            let seqs = stream_sequences(&tl.svg_overlays, tl.duration(), &sliced, 0.0, &f);
            let mut args = Vec::new();
            push_inputs(&seqs, &f, &mut args);
            assert!(!args.contains(&"-start_number".to_string()), "{args:?}");
        });
    }

    #[test]
    fn a_playhead_between_frames_keeps_the_exact_sequence_of_its_own() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (tl, _) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            let whole = svg_sequences(&tl, &f).remove(0);
            // 2.01 s is a third of a frame past frame 60: the cut's frames are not the title's.
            let off = streamed(&tl, 2.01, &f);
            assert_eq!(off.len(), 1);
            assert_ne!(off[0].0, whole.dir);
            assert_eq!(off[0].2.start, 0, "all of its own pictures, read from the first");
            // And it is exactly what the cut's own sequence is.
            let sliced = tl.slice(2.01, tl.duration());
            let exact = svg_sequences(&sliced, &f);
            assert_eq!(off[0].0, exact[0].dir);
            // A playhead a millionth of a frame off a frame is on it.
            let near = streamed(&tl, 2.0 + 1e-9, &f);
            assert_eq!(near[0].0, whole.dir);
            // A frame rate that is not whole: playheads on its frames share a directory too.
            let f = fmt(1920, 1080, 29.97);
            let whole = svg_sequences(&tl, &f).remove(0);
            for k in [0u64, 31, 45, 60] {
                let start = k as f64 * 100.0 / 2997.0;
                assert_eq!(streamed(&tl, start, &f)[0].0, whole.dir, "frame {k}");
            }
        });
    }

    #[test]
    fn a_held_title_in_the_stream_covers_the_window_it_is_on() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let (tl, _) = cut(vec![overlay(still_art(), 1.0, 3.0)]);
            let f = fmt(1920, 1080, 30.0);
            let sliced = tl.slice(2.0, tl.duration());
            let seqs = stream_sequences(&tl.svg_overlays, tl.duration(), &sliced, 2.0, &f);
            assert_eq!(seqs[0].kind, Kind::Held);
            assert_eq!(seqs[0].first_frame, 0, "from the first frame of the window");
            let mut args = Vec::new();
            push_inputs(&seqs, &f, &mut args);
            assert!(
                !args.contains(&"-start_number".to_string()) && args.contains(&"-loop".to_string()),
                "{args:?}"
            );
        });
    }

    #[test]
    fn a_stream_from_the_middle_draws_the_rest_and_one_from_earlier_draws_only_the_difference() {
        let root = scratch("suffix");
        let f = fmt(320, 180, 10.0);
        // Frames 10..=30 of the cut: 21 pictures.
        let (tl, _) = titled(&sliding(), 1.0, 3.0);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let sliced = tl.slice(2.0, tl.duration());
            let seqs = stream_sequences(&tl.svg_overlays, tl.duration(), &sliced, 2.0, &f);
            assert_eq!((seqs[0].needed(), seqs[0].frames), (10..21, 21));
            let dir = seqs[0].dir.clone();
            let pins = prepare(&seqs, 2, None, &|| false).unwrap();
            assert!(!raster::is_complete(&dir), "a part is not the whole");
            assert!(raster::has_frames(&dir, 10..21) && !raster::frame_path(&dir, 9).exists());
            // Played again: nothing to do (and nothing is drawn).
            let stamps: Vec<_> = (10..21)
                .map(|j| std::fs::metadata(raster::frame_path(&dir, j)).unwrap().modified().unwrap())
                .collect();
            drop(prepare(&seqs, 2, None, &|| false).unwrap());
            drop(pins);

            // From the start of the title: the first half is drawn, the rest kept, and it is whole.
            let sliced = tl.slice(0.0, tl.duration());
            let all = stream_sequences(&tl.svg_overlays, tl.duration(), &sliced, 0.0, &f);
            assert_eq!(all[0].dir, dir);
            let pins = prepare(&all, 2, None, &|| false).unwrap();
            assert!(raster::is_complete(&dir) && raster::has_frames(&dir, 0..21));
            for (n, j) in (10..21).enumerate() {
                let kept = std::fs::metadata(raster::frame_path(&dir, j)).unwrap().modified().unwrap();
                assert_eq!(kept, stamps[n], "frame {j} was not drawn again");
            }
            drop(pins);

            // And the middle again: finished, found at once.
            let sliced = tl.slice(2.0, tl.duration());
            let again = stream_sequences(&tl.svg_overlays, tl.duration(), &sliced, 2.0, &f);
            assert!(is_ready(&again[0]));
        });
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- stopping, waiting and not stacking ------------------------------------------------------

    #[test]
    fn cancelling_stops_a_preparation_between_its_steps_not_only_once_it_is_drawing() {
        let root = scratch("cancel-scan");
        let f = fmt(320, 180, 60.0);
        // 541 frames to compare before the first one is drawn.
        let (tl, _) = titled(&sliding(), 0.0, 9.0);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let polls = AtomicUsize::new(0);
            // Fires on the fourth look: before the document is read, then in the scan of its frames.
            let err = prepare(&seqs, 2, None, &|| polls.fetch_add(1, Ordering::Relaxed) >= 3).unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{err}");
            assert_eq!(
                polls.load(Ordering::Relaxed),
                4,
                "the scan looks every few frames and stops at once"
            );
            let drawn = std::fs::read_dir(&seqs[0].dir).map_or(0, |d| d.count());
            assert_eq!(drawn, 0, "nothing was drawn");
            // A deadline stops the scan the same way.
            let late = prepare(&seqs, 2, Some(Instant::now()), &|| false).unwrap_err();
            assert!(late.to_string().contains("too heavy to preview"), "{late}");
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn one_stream_preparation_draws_at_a_time_and_a_cancelled_one_does_not_wait_for_it() {
        let root = scratch("slot");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled(&sliding(), 0.0, 1.0);
        let root_str = root.to_str().unwrap().to_string();
        // A gate of this test's own: the process's is the one every stream test goes through.
        let gate = PreviewGate::new();
        crate::svg::raster::with_pinned_dir(&root_str, || {
            let seqs = svg_sequences(&tl, &f);
            let never = Control {
                cancel: &|| false,
                deadline: None,
            };
            let held = PreviewSlot::take(&gate, &never).unwrap();
            // Stopped while it waits: it gives up at once, without having drawn anything.
            let err = prepare_preview_in(&gate, &seqs, &|| true).unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{err}");
            assert!(!seqs[0].dir.exists());
            // One that is not stopped waits, and goes when the first lets go.
            std::thread::scope(|scope| {
                let waiter = scope.spawn(|| {
                    crate::svg::raster::with_pinned_dir(&root_str, || {
                        let seqs = svg_sequences(&tl, &f);
                        prepare_preview_in(&gate, &seqs, &|| false).map(|pins| pins.len())
                    })
                });
                std::thread::sleep(Duration::from_millis(250));
                assert!(!waiter.is_finished(), "it waits for the slot");
                drop(held);
                assert_eq!(waiter.join().unwrap().unwrap(), 1);
            });
            // And the slot is free again afterwards.
            drop(PreviewSlot::take(&gate, &never).unwrap());
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn each_test_thread_has_a_gate_of_its_own_so_the_stream_tests_do_not_wait_for_each_other() {
        let a = preview_gate() as *const PreviewGate;
        assert_eq!(a, preview_gate() as *const PreviewGate, "one per thread");
        let other = std::thread::spawn(|| preview_gate() as *const PreviewGate as usize)
            .join()
            .unwrap();
        assert_ne!(a as usize, other);
    }

    #[test]
    fn a_stream_told_to_stop_while_its_titles_are_drawn_ends_before_ffmpeg_is_asked_for_anything() {
        let root = scratch("stream-cancel");
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            // The clip's file does not exist: reaching ffmpeg would be an error of another kind.
            let (tl, assets) = titled(&sliding(), 0.0, 9.0);
            let request = super::super::PlaybackRequest {
                fps: Some(30.0),
                max_width: Some(640),
            };
            let mut frames = 0;
            let polls = AtomicUsize::new(0);
            let err = super::super::stream_preview_until(
                &tl,
                &assets,
                0.0,
                request,
                &|| polls.fetch_add(1, Ordering::Relaxed) >= 2,
                &mut |_| {
                    frames += 1;
                    true
                },
            )
            .unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{err}");
            assert_eq!(frames, 0);
            assert!(polls.load(Ordering::Relaxed) >= 3, "it was asked again and again");
            // Never asked, it goes on to draw the pictures (and then fails on the missing file).
            let err = super::super::stream_preview_with(&tl, &assets, 0.0, request, &mut |_| true).unwrap_err();
            assert!(!matches!(err, Error::Cancelled), "{err}");
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_marker_does_not_vouch_for_a_sequence_that_lost_a_frame_at_either_end() {
        let root = scratch("edges");
        let f = fmt(320, 180, 10.0);
        let (tl, _) = titled(&sliding(), 0.0, 1.0);
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let seqs = svg_sequences(&tl, &f);
            let s = &seqs[0];
            drop(prepare(&seqs, 2, None, &|| false).unwrap());
            assert!(is_ready(s));
            for j in [0, s.frames - 1] {
                std::fs::remove_file(raster::frame_path(&s.dir, j)).unwrap();
                assert!(raster::is_complete(&s.dir) && !is_ready(s), "frame {j}");
                drop(prepare(&seqs, 2, None, &|| false).unwrap());
                assert!(
                    is_ready(s) && raster::has_frames(&s.dir, 0..s.frames),
                    "frame {j} was drawn again"
                );
            }
            // The whole directory gone (the cache was cleared): drawn again, and finished.
            std::fs::remove_dir_all(&s.dir).unwrap();
            assert!(!is_ready(s));
            drop(prepare(&seqs, 2, None, &|| false).unwrap());
            assert!(is_ready(s));
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn memory_caps_the_drawing_threads() {
        crate::svg::raster::with_pinned_dir(PIN, || {
            let f = fmt(1920, 1080, 30.0);
            let big = |w: f64| {
                let mut o = overlay(sliding(), 0.0, 1.0);
                o.width = w;
                o.height = w;
                cut(vec![o]).0
            };
            fn jobs_of<'a>(tl: &'a Timeline, f: &ExportFormat) -> Vec<(SvgSeq<'a>, Arc<SvgDoc>)> {
                svg_sequences(tl, f)
                    .into_iter()
                    .map(|s| {
                        let doc = Arc::new(SvgDoc::parse(s.svg).unwrap());
                        (s, doc)
                    })
                    .collect()
            }
            // 7680 x 4320 is 33 million pixels: 400 MB a thread, so two.
            let tl = big(4.0);
            let owned = jobs_of(&tl, &f);
            let jobs: Vec<Job<'_>> = owned.iter().map(|(s, d)| Job { seq: s, doc: d.clone() }).collect();
            assert_eq!(workers_for(8, &jobs, 100), 2);
            assert_eq!(workers_for(1, &jobs, 100), 1);
            // A title the size of a frame, and a handful of pictures to draw.
            let tl = big(1.0);
            let owned = jobs_of(&tl, &f);
            let jobs: Vec<Job<'_>> = owned.iter().map(|(s, d)| Job { seq: s, doc: d.clone() }).collect();
            assert_eq!(workers_for(8, &jobs, 100), 8);
            assert_eq!(workers_for(8, &jobs, 3), 3, "no more threads than pictures");
            assert_eq!(workers_for(0, &jobs, 3), 1);
        });
    }

    // ---- a request that is refused ---------------------------------------------------------------

    #[test]
    fn a_request_that_would_be_refused_is_refused_before_a_title_is_drawn() {
        let root = scratch("preflight");
        let cache = root.join("cache");
        crate::svg::raster::with_pinned_dir(cache.to_str().unwrap(), || {
            let (tl, assets) = titled(&sliding(), 0.0, 1.0);
            let out = root.join("never.mp4");
            let refused = |tl: &Timeline, assets: &[Asset], opts: &ExportOptions| {
                let mut said = false;
                let result = super::super::render_with_progress(tl, assets, &out, opts, &mut |_| said = true, &|| false);
                assert!(!said, "nothing was reported: no wait for the slot, no drawing");
                result.unwrap_err()
            };
            // Options that would export nothing: a sound-only file of a picture's worth, with no sound asked for.
            let silent = ExportOptions {
                container: crate::engine::Container::Mp3,
                include_audio: false,
                ..ExportOptions::default()
            };
            assert!(matches!(refused(&tl, &assets, &silent), Error::InvalidArgument(_)));
            // A container that cannot hold the codec.
            let wrong = ExportOptions {
                container: crate::engine::Container::Webm,
                video_codec: Some("libx264".into()),
                ..ExportOptions::default()
            };
            assert!(matches!(refused(&tl, &assets, &wrong), Error::InvalidArgument(_)));
            // No clips at all (a title alone is not a cut).
            let mut empty = tl;
            empty.tracks.iter_mut().for_each(|t| t.clips.clear());
            let e = refused(&empty, &assets, &ExportOptions::default());
            assert!(e.to_string().contains("no clips"), "{e}");
            assert!(!cache.exists(), "no picture was drawn for any of them");
        });
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- stills --------------------------------------------------------------------------------------

    #[test]
    fn documents_are_read_once_and_the_last_few_are_kept() {
        let cache = DocCache::new();
        let a = cache.get(&sliding()).unwrap();
        assert!(Arc::ptr_eq(&a, &cache.get(&sliding()).unwrap()), "the same read");
        for i in 0..DOC_CACHE {
            cache.get(&doc(&format!("<rect width=\"{}\" height=\"5\"/>", i + 1))).unwrap();
        }
        assert!(!Arc::ptr_eq(&a, &cache.get(&sliding()).unwrap()), "the oldest went");
        assert!(cache.0.lock().unwrap().len() <= DOC_CACHE);
        let err = cache.get("<svg").unwrap_err();
        assert!(err.to_string().contains("cannot be drawn"), "{err}");
    }

    #[test]
    fn a_scrub_over_titles_that_are_drawn_does_not_walk_the_cache() {
        let root = scratch("stills-trim");
        crate::svg::raster::with_pinned_dir(root.to_str().unwrap(), || {
            let (tl, _) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let trims = std::cell::Cell::new(0);
            let count = || trims.set(trims.get() + 1);
            let live = still_svgs(&tl, 192, 108, 1.5);
            drop(ensure_stills_with(&live, &count).unwrap());
            assert_eq!(trims.get(), 1, "a picture was written");
            drop(ensure_stills_with(&live, &count).unwrap());
            drop(ensure_stills_with(&live, &count).unwrap());
            assert_eq!(trims.get(), 1, "everything was drawn already: the directory is not walked");
            drop(ensure_stills_with(&still_svgs(&tl, 192, 108, 2.0), &count).unwrap());
            assert_eq!(trims.get(), 2);
            drop(ensure_stills_with(&still_svgs(&tl, 192, 108, 0.5), &count).unwrap());
            assert_eq!(trims.get(), 2, "nothing is live at 0.5 s");
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_argv_path_is_the_roots_text_then_slashes_on_every_os() {
        assert_eq!(argv_root(std::path::Path::new("/kerf-test/svg")), "/kerf-test/svg");
        assert_eq!(argv_root(std::path::Path::new("/kerf-test/svg/")), "/kerf-test/svg");
        // The root's own separators are the OS's, and stay: only what is added below it is `/`.
        assert_eq!(argv_root(std::path::Path::new("C:\\Users\\a\\svg")), "C:\\Users\\a\\svg");
        crate::svg::raster::with_pinned_dir("/kerf-test/svg/", || {
            let (tl, _) = cut(vec![overlay(sliding(), 0.0, 1.0), overlay(still_art(), 0.0, 1.0)]);
            let seqs = svg_sequences(&tl, &fmt(1920, 1080, 30.0));
            let dir = |s: &SvgSeq<'_>| s.dir.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(seqs[0].input, format!("/kerf-test/svg/{}/%06d.png", dir(&seqs[0])));
            assert_eq!(seqs[1].input, format!("/kerf-test/svg/{}/000000.png", dir(&seqs[1])));
            let live = still_svgs(&tl, 192, 108, 0.5);
            assert!(live[0].input.starts_with("/kerf-test/svg/still-") && !live[0].input.contains('\\'));
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_stream_draws_at_normal_priority_and_an_export_behind_the_rest() {
        let _serial = cpu::test_lock();
        let restore = cpu::cpu_percent();
        cpu::set_cpu_percent(cpu::MIN_CPU_PERCENT);
        let nice_after = |priority: Option<Priority>| {
            std::thread::spawn(move || {
                if let Some(priority) = priority {
                    priority.apply_to_this_thread();
                }
                // SAFETY: reads the scheduling priority of our own task id.
                unsafe { libc::getpriority(libc::PRIO_PROCESS as _, libc::gettid() as libc::id_t) }
            })
            .join()
            .unwrap()
        };
        let (start, normal, behind) = (
            nice_after(None),
            nice_after(Some(Priority::Normal)),
            nice_after(Some(Priority::Behind)),
        );
        cpu::set_cpu_percent(restore);
        assert_eq!(normal, start, "the stream's threads are not lowered");
        assert_eq!(behind, (start + 10).min(19), "an export's are, from {start}");
    }

    #[test]
    fn a_still_names_its_picture_as_one_file_whatever_the_path_holds() {
        crate::svg::raster::with_pinned_dir("/home/a%d[1]*/svg", || {
            let (tl, _) = cut(vec![overlay(sliding(), 1.0, 3.0)]);
            let live = still_svgs(&tl, 192, 108, 1.5);
            let mut args = Vec::new();
            push_still_inputs(&live, &mut args);
            assert_eq!(&args[..5], ["-f", "image2", "-pattern_type", "none", "-i"]);
            assert_eq!(args[5], live[0].input);
            assert!(args[5].starts_with("/home/a%d[1]*/svg/still-"), "{}", args[5]);
        });
    }
}
