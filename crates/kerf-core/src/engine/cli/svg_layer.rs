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
//! * **A still reads one PNG** ([`still_svgs`], [`ensure_stills`]): the document at the still's
//!   own time, at the still's size.
//!
//! Nothing here changes a graph without an SVG overlay: every function answers "nothing" for an
//! empty `svg_overlays`, and the builders add nothing for that.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{cpu, export_format, export_timeline, ExportFormat, ExportOptions, ExportProgress};
use crate::clip_timing::Rational;
use crate::error::{Error, Result};
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

/// A digest of what decides a picture, as 32 hex digits.
fn digest(parts: impl FnOnce(&mut Sha256)) -> String {
    let mut hash = Sha256::new();
    hash.update(RASTER_VERSION.to_le_bytes());
    parts(&mut hash);
    hash.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_svg(hash: &mut Sha256, svg: &str) {
    hash.update((svg.len() as u64).to_le_bytes());
    hash.update(svg.as_bytes());
}

// ---- a sequence for an export or the stream ------------------------------------------------------

/// One overlay as a numbered PNG sequence on disk.
#[derive(Debug, Clone)]
pub(super) struct SvgSeq<'a> {
    /// The sanitised source.
    pub svg: &'a str,
    /// The directory the frames are in (named by a digest of everything that decides them).
    pub dir: PathBuf,
    /// `dir/%06d.png`, for `-i` (a `%` in the path is `%%`).
    pub pattern: String,
    /// The output frame the first picture lands on, and how many pictures there are.
    pub first_frame: u64,
    pub frames: u64,
    /// The document's own clock at the first picture; the next is one frame later.
    pub doc_t0: f64,
    pub fps: Rational,
    pub rect: Rect,
    pub opacity: f64,
    /// The overlay's window, for `enable`.
    pub start: f64,
    pub end: f64,
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
    timeline
        .svg_overlays
        .iter()
        .filter(|o| o.start.is_finite() && o.end.is_finite() && o.end > o.start)
        .filter_map(|o| {
            let rect = rect_px(o, fmt.width, fmt.height)?;
            let first_frame = first_frame_from(fps, o.start);
            let last = last_frame_to(fps, o.end, false)?.min(last_output);
            if last < first_frame {
                return None;
            }
            let frames = last - first_frame + 1;
            let doc_t0 = (o.offset + fps.exact_time(first_frame) - o.start).max(0.0);
            let opacity = if o.opacity.is_finite() {
                o.opacity.clamp(0.0, 1.0)
            } else {
                1.0
            };
            // The rectangle's position is where the picture is laid, not what it shows, so a
            // title that is only moved keeps its frames.
            let key = digest(|h| {
                h.update(b"sequence");
                hash_svg(h, &o.svg);
                h.update(doc_t0.to_bits().to_le_bytes());
                h.update([fps.num.to_le_bytes(), fps.den.to_le_bytes()].concat());
                h.update(frames.to_le_bytes());
                h.update([rect.w.to_le_bytes(), rect.h.to_le_bytes()].concat());
                h.update(opacity.to_bits().to_le_bytes());
            });
            let dir = root.join(key);
            let pattern = format!("{}/%06d.png", dir.to_string_lossy().replace('%', "%%"));
            Some(SvgSeq {
                svg: &o.svg,
                dir,
                pattern,
                first_frame,
                frames,
                doc_t0,
                fps,
                rect,
                opacity,
                start: o.start,
                end: o.end,
            })
        })
        .collect()
}

/// The `-i` of each sequence, after the clips' inputs.
pub(super) fn push_inputs(seqs: &[SvgSeq<'_>], fmt: &ExportFormat, args: &mut Vec<String>) {
    for s in seqs {
        args.extend(["-f", "image2", "-framerate"].map(String::from));
        args.push(format!("{}", fmt.fps));
        args.push("-i".to_string());
        args.push(s.pattern.clone());
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

/// One stretch of consecutive frames that are the same picture.
struct Run {
    first: u64,
    len: u64,
    doc_t: f64,
}

/// `seq`'s frames grouped into runs of identical documents: a title that holds still between its
/// moves is drawn once per hold, and the frames after the first are links to it.
fn runs(seq: &SvgSeq<'_>, doc: &SvgDoc) -> Vec<Run> {
    if !doc.is_animated() {
        return vec![Run {
            first: 0,
            len: seq.frames,
            doc_t: seq.doc_t0,
        }];
    }
    let mut out: Vec<Run> = Vec::new();
    let mut previous: Option<String> = None;
    for j in 0..seq.frames {
        let doc_t = seq.doc_t0 + seq.fps.exact_time(j);
        let now = doc.at(doc_t);
        match (&previous, out.last_mut()) {
            (Some(before), Some(run)) if *before == now => run.len += 1,
            _ => out.push(Run { first: j, len: 1, doc_t }),
        }
        previous = Some(now);
    }
    out
}

/// Draw one run: its first frame is rendered (unless a previous, interrupted attempt left it —
/// a frame is renamed into place whole, so one that is there is right), the rest linked to it.
fn draw_run(seq: &SvgSeq<'_>, doc: &SvgDoc, run: &Run, stop: &AtomicBool) -> Result<()> {
    let first = raster::frame_path(&seq.dir, run.first);
    if !first.exists() {
        let picture = raster::render(&doc.at(run.doc_t), seq.rect.w, seq.rect.h, seq.opacity)?;
        raster::write_atomic(&first, &raster::encode_png(&picture)?)?;
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

/// Have every sequence in `seqs` on disk, drawing what is missing on `threads` threads, and return
/// a [`Pin`] on each so the cache keeps them until the caller (ffmpeg) is done.
///
/// `cancel` is polled about forty times a second and ends the work with [`Error::Cancelled`];
/// `deadline` ends it with an error (the stream's "too heavy to preview"). A sequence is usable
/// only once its marker is written, which is last, so a stopped run leaves nothing that looks
/// finished — and what it did draw is kept, because a frame is only ever renamed into place whole
/// and its content is fixed by the directory's name, so the next attempt carries on from it.
pub(super) fn prepare(
    seqs: &[SvgSeq<'_>],
    threads: usize,
    deadline: Option<Instant>,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<Pin>> {
    // Pinned first: a sequence found finished must not be evicted before ffmpeg reads it.
    let pins: Vec<Pin> = seqs.iter().map(|s| Pin::new(&s.dir)).collect();
    let mut todo: Vec<&SvgSeq<'_>> = Vec::new();
    for s in seqs {
        if raster::is_complete(&s.dir) {
            raster::touch(&s.dir);
        } else {
            todo.push(s);
        }
    }
    if todo.is_empty() {
        return Ok(pins);
    }
    let docs = todo
        .iter()
        .map(|s| SvgDoc::parse(s.svg).map_err(|e| Error::Engine(format!("an animated title cannot be drawn: {e}"))))
        .collect::<Result<Vec<_>>>()?;
    let units: Vec<(usize, Run)> = todo
        .iter()
        .zip(&docs)
        .enumerate()
        .flat_map(|(i, (s, doc))| runs(s, doc).into_iter().map(move |r| (i, r)))
        .collect();
    tracing::debug!(
        sequences = todo.len(),
        pictures = units.len(),
        frames = todo.iter().map(|s| s.frames).sum::<u64>(),
        threads,
        "drawing animated titles"
    );

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let failure: Mutex<Option<Error>> = Mutex::new(None);
    let drawn = AtomicU64::new(0);
    let (mut cancelled, mut timed_out) = (false, false);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.clamp(1, units.len().max(1)))
            .map(|_| {
                scope.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        let Some((i, run)) = units.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        match draw_run(todo[*i], &docs[*i], run, &stop) {
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
        while !workers.iter().all(|w| w.is_finished()) {
            std::thread::sleep(POLL);
            if stop.load(Ordering::Relaxed) {
                continue;
            }
            if cancel() {
                cancelled = true;
                stop.store(true, Ordering::Relaxed);
            } else if deadline.is_some_and(|d| Instant::now() >= d) {
                timed_out = true;
                stop.store(true, Ordering::Relaxed);
            }
        }
    });
    if let Some(e) = failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(e);
    }
    if cancelled {
        return Err(Error::Cancelled);
    }
    if timed_out {
        return Err(Error::Engine(TOO_HEAVY.to_string()));
    }
    for s in &todo {
        raster::mark_complete(&s.dir)?;
    }
    raster::trim();
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
    if timeline.svg_overlays.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let cut = export_timeline(timeline, opts);
    let wants_picture =
        !opts.container.is_audio_only() && cut.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    if !wants_picture {
        return Ok(Some(Vec::new()));
    }
    let fmt = export_format(&cut, assets, opts);
    let seqs = svg_sequences(&cut, &fmt);
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
    if seqs.iter().all(|s| raster::is_complete(&s.dir)) {
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

/// Have the titles of the playback stream's cut (`sliced` at the preview's `fmt`) on disk: on the
/// budget's threads, no queue — what the user is waiting to see is never gated — and for at most
/// [`PREVIEW_PREPARE_LIMIT`].
pub(super) fn prepare_preview(sliced: &Timeline, fmt: &ExportFormat) -> Result<Vec<Pin>> {
    let seqs = svg_sequences(sliced, fmt);
    if seqs.is_empty() {
        return Ok(Vec::new());
    }
    prepare(
        &seqs,
        cpu::budget_threads(),
        Some(Instant::now() + PREVIEW_PREPARE_LIMIT),
        &|| false,
    )
}

// ---- a picture for a still ------------------------------------------------------------------------

/// One overlay as a single PNG, for a still.
#[derive(Debug, Clone)]
pub(super) struct StillSvg<'a> {
    pub svg: &'a str,
    pub path: PathBuf,
    pub rect: Rect,
    /// The document's clock at the still's time.
    pub doc_t: f64,
    pub opacity: f64,
}

/// The overlays on screen in a still of `timeline` at `t` (`start <= t < end`, as a text overlay)
/// on a `w` x `h` canvas, in the order they are laid over the picture. Pure.
pub(super) fn still_svgs<'a>(timeline: &'a Timeline, w: u32, h: u32, t: f64) -> Vec<StillSvg<'a>> {
    if timeline.svg_overlays.is_empty() {
        return Vec::new();
    }
    let root = raster::root();
    timeline
        .svg_overlays
        .iter()
        .filter(|o| t >= o.start && t < o.end)
        .filter_map(|o| {
            let rect = rect_px(o, w, h)?;
            let doc_t = o.doc_time(t);
            let opacity = if o.opacity.is_finite() {
                o.opacity.clamp(0.0, 1.0)
            } else {
                1.0
            };
            let key = digest(|hash| {
                hash.update(b"still");
                hash_svg(hash, &o.svg);
                hash.update(doc_t.to_bits().to_le_bytes());
                hash.update([rect.w.to_le_bytes(), rect.h.to_le_bytes()].concat());
                hash.update(opacity.to_bits().to_le_bytes());
            });
            Some(StillSvg {
                svg: &o.svg,
                path: root.join(format!("still-{key}.png")),
                rect,
                doc_t,
                opacity,
            })
        })
        .collect()
}

/// The `-i` of each picture, after the clips' inputs.
pub(super) fn push_still_inputs(live: &[StillSvg<'_>], args: &mut Vec<String>) {
    for s in live {
        args.push("-i".to_string());
        args.push(s.path.to_string_lossy().into_owned());
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
/// and pinned until the returned pins are dropped.
pub(super) fn ensure_stills(live: &[StillSvg<'_>]) -> Result<Vec<Pin>> {
    let pins: Vec<Pin> = live.iter().map(|s| Pin::new(&s.path)).collect();
    for s in live {
        if s.path.is_file() {
            raster::touch(&s.path);
            continue;
        }
        let doc = SvgDoc::parse(s.svg).map_err(|e| Error::Engine(format!("an animated title cannot be drawn: {e}")))?;
        let picture = raster::render(&doc.at(s.doc_t), s.rect.w, s.rect.h, s.opacity)?;
        raster::write_atomic(&s.path, &raster::encode_png(&picture)?)?;
    }
    if !live.is_empty() {
        raster::trim();
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
                s.pattern.starts_with("/kerf-test/svg/") && s.pattern.ends_with("/%06d.png"),
                "{}",
                s.pattern
            );

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

    #[test]
    fn a_percent_in_the_cache_path_is_escaped_for_the_image_demuxer() {
        crate::svg::raster::with_pinned_dir("/home/a%b/svg", || {
            let (tl, _) = cut(vec![overlay(sliding(), 0.0, 1.0)]);
            let s = &svg_sequences(&tl, &fmt(1920, 1080, 30.0))[0];
            assert!(s.pattern.starts_with("/home/a%%b/svg/"), "{}", s.pattern);
            assert!(s.pattern.ends_with("/%06d.png"));
            assert_eq!(s.pattern.matches('%').count(), 3, "one escaped pair and one frame number");
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
        // Static: one picture, nine links.
        let (tl, _) = titled(&doc("<rect width=\"192\" height=\"108\" fill=\"#123456\"/>"), 0.0, 0.9);
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
            let r = runs(&seqs[0], &doc);
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
            raster::trim_to(&root, 0, &|p| pins.iter().any(|_| p == seqs[0].dir));
            assert!(raster::is_complete(&seqs[0].dir));
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
}
