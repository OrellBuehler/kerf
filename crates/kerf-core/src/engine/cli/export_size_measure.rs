//! A measured size for an export: sample encodes of the real thing, extrapolated.
//!
//! The model in [`super::export_size`] knows the codec and the quality but not the footage, so for
//! constant quality (and ProRes, GIF, lossless, hardware) its band is wide. The footage is the one thing
//! that decides the size, and it can be read: [`measure_export_size`] encodes a few short windows spread
//! over the cut with the export's own graph and encoder settings, reads how many bytes each window
//! spent on the picture and on the sound, and extrapolates to the whole length with a band from how
//! much the windows differ.
//!
//! # What it promises, and what it does not
//!
//! The range holds **if the sampled seconds are typical of the cut**. A busy stretch the samples missed
//! (a short burst of noise in a calm shot, in the one asset the windows are all spread over) makes the
//! file larger than the high end; the band is asymmetric for that ([`HIGH_SKEW`]) and the note says
//! it, but the high end is not a guarantee, and neither is the low end: a platform's "it will not fit" is
//! never built on a sampled low end (`SizeBounds::floor_bytes`; only a whole encode, or the model's own floor
//! under it, may). It is an estimate of the cut as it is exported, not of the title clips, the
//! muxed captions, the chapters and the loudness pass, which the samples leave out (the note lists the
//! ones the cut has).
//!
//! # Design
//!
//! * **Windows.** [`planned_windows`] windows of [`WINDOW_SECS`] (1 s), evenly spaced over the span
//!   the export writes (the range, or the cut), each snapped inside the clip it lands on, and the
//!   footage that no window landed on is given one (the footage with the most windows gives one up).
//!   Each window speaks for the stretch of the span nearest to it (`weight`), so a clip that is a tenth
//!   of the cut is a tenth of the answer however the windows fell. The plan is pure and deterministic
//!   ([`plan_windows`]); a span of [`WHOLE_MAX_SECS`] or less, or one a few windows would cover half of, is
//!   encoded **whole** (nothing to extrapolate, band 5 %).
//! * **Real encode.** Each window is [`build_export_args`] over `range: window` of the original
//!   timeline (the builders stay untouched; the golden does not move), tuned for the process like an
//!   export ([`tune_export_args`]), written to a temp file in the export's own container and deleted.
//!   Burned captions are the real export's: `skip_burned_captions` is the options' own. What is left out
//!   because it needs a pass, a file or the heavy slot ([`sample_view`], listed in the note): title clips and SVG
//!   overlays (their pictures are drawn under the heavy slot), captions muxed in, chapters, loudness
//!   normalisation, `faststart`, a title tag. Originals are read, never a proxy.
//! * **Read.** One `ffprobe -show_packets` of the window file gives each stream's packets. The
//!   **first picture packet is the window's forced keyframe** and is left out of the rate (a 1 s window
//!   is a keyframe in 30 frames, a file one in 250): rate = the other packets' bytes over their duration,
//!   and the periodic keyframes of the whole file are added back as `K x (I - P)` with the window's own `I`
//!   (`K` = frames / the encoder's default keyframe interval + the cuts of the cut). The sound's first
//!   packet (priming) is left out the same way.
//! * **Band.** `half = max(15 %, t x se)`, `se` the successive-difference estimate of the standard
//!   error of the mean of an evenly spaced sample (Wolter), over the windows' combined rate in time order,
//!   `t` Student's two-sided 95 % for `done - 1` degrees of freedom ([`student_t95`]: 12.7 for two windows, 2.4 for
//!   seven), widened by `sqrt(planned / done)` when the time limit stopped it short, 50 % for a single window
//!   (nothing to compare it with), 5 % for a whole encode, doubled for a hardware encoder. The low end is `half`
//!   under the mean, the high end `half x` [`HIGH_SKEW`] over it (a burst costs more than a lull saves).
//! * **Time: what the budget bounds.** `budget` (clamped to [`MAX_MEASURE_BUDGET`]) bounds when a window may
//!   *begin*: one the average cost says will not finish is not begun. A window that has begun is killed at the
//!   budget plus [`HARD_GRACE`] (at least [`MIN_SPAWN`] from its start); the `ffprobe` of its file is bounded by what is left of
//!   that (at least 2 s, never more than [`PROBE_LIMIT`]); the retry of a failed hardware decode in software
//!   takes what is left and is not made when none is. Not under the budget: the asset time-base backfill (its probes
//!   have their own limits; made only while the budget is not spent, after the first cancel poll), the SVT-AV1 capability
//!   probe of a lossless SVT-AV1 request (once per binary, its own 20 s) and the wait for a VAAPI device (up to 10 s). The windows run
//!   in a spread order (the middle, then each half's middle), so any prefix of them covers the cut. With nothing measured
//!   the answer is the model's, with the reason in its note.
//! * **Lane.** Ungated and thread-capped, not a `cpu::lease`: a window is a moment read of a second, like a
//!   scrubbed frame or the preview stream, and the answer is wanted *now* by a dialog. A foreground lease
//!   would queue behind a running export or analysis (minutes) and the budget would end before it started;
//!   the cost is that it competes with a running render for the cores, so it runs niced like the real
//!   export (`cpu::background_always`) and held to half of the CPU budget's threads (`Budget::shared(2)`).
//!   **Hardware encoders** are not run for a sample while a foreground job (an export) is running: their
//!   sessions are limited, and a sample must not take one from the export; that file falls back to the model with the reason.
//! * **One at a time.** A measurement is the process's latest: starting one makes an older one still running
//!   return [`Error::Cancelled`] at its next poll, so changes of options in a dialog cannot pile up
//!   encodes. (A call that has nothing to measure, or is cancelled before it starts, does not supersede.)
//! * **Disk.** Each window is a temp file, removed at once. Before any is made, the model's high end for a window
//!   must fit under [`TEMP_CAP`] and half the free space of the temp folder (Unix), else the file falls back to the model
//!   with the reason; `-fs` is a hard stop on every encode, and a sample that reached it is a failure.
//! * **Several deliveries.** Shapes that share a fit and an aspect ([`measure_groups`]) are one group: its
//!   largest is measured and the others scaled by the model's ratio (the geometric middle of each band) with 30 % more
//!   room. A shape with another fit or aspect is measured on its own (two frames of the same pixels, one with
//!   bars and one cropped, differ ~5x), and the budget is split evenly across the groups: fewer windows each, wider
//!   bands; a group the budget cannot reach falls back to the model.
//!
//! `export_size_measure_calibration` holds the band to full exports of cuts whose complexity changes over
//! time, and to a burst the windows miss.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::export_size::{container_overhead, frame_count, picture_kind, to_bytes, total_of, Picture, Sampled};
use super::*;
use crate::model::MarkerKind;

/// How long [`measure_export_size`] may spend on its sample encodes in all unless the caller says: a few
/// seconds of a 1080p cut, a bounded wait for a dialog.
pub const DEFAULT_MEASURE_BUDGET: Duration = Duration::from_secs(8);
/// The longest a budget is taken at: a caller's `Duration::MAX` is this.
pub const MAX_MEASURE_BUDGET: Duration = Duration::from_secs(600);

/// How long a window is.
pub const WINDOW_SECS: f64 = 1.0;
/// A span this short is encoded whole.
const WHOLE_MAX_SECS: f64 = 5.0;
/// The narrowest half-width of a band built from windows, and the half-width of a whole encode.
const MIN_BAND: f64 = 0.15;
const WHOLE_BAND: f64 = 0.05;
/// What one window alone says: nothing about how the rest differs.
const ALONE_BAND: f64 = 0.5;
/// The most a band may open to, on the low side before the skew.
const MAX_BAND: f64 = 3.0;
/// How much further the high end opens than the low end: a burst costs more than a lull saves.
const HIGH_SKEW: f64 = 1.5;
/// The extra room of a delivery that is scaled from another's measurement.
const SCALED_BAND: f64 = 0.3;
/// How long past the budget a window that has begun may run before it is killed.
const HARD_GRACE: Duration = Duration::from_secs(2);
/// The least a window that has begun is given, whatever the budget has left.
const MIN_SPAWN: Duration = Duration::from_secs(3);
/// `ffprobe` over a one-second file: the most it is given.
const PROBE_LIMIT: Duration = Duration::from_secs(20);
/// The most the temp files of a measurement may reach (one window at a time, each removed at once).
pub const TEMP_CAP: u64 = 512 * 1024 * 1024;
/// A group of shapes needs at least this much budget left to be tried.
const MIN_GROUP_TIME: Duration = Duration::from_millis(500);

/// The latest measurement started in this process: a measurement whose number is not it has been superseded.
static LATEST: AtomicU64 = AtomicU64::new(0);

/// The number of the latest measurement (a test asks it whether a call took its place).
#[cfg(test)]
pub(super) fn latest_measurement() -> u64 {
    LATEST.load(Ordering::SeqCst)
}

/// How far a measurement has got, for a spinner or a bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct SizeMeasureProgress {
    pub windows_done: usize,
    pub windows_planned: usize,
}

// ---- the plan (pure) ---------------------------------------------------------------------------

/// A stretch of the cut a window may land on, and the footage it shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Span {
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) source: Uuid,
}

/// A window to encode: where it starts in the cut, how long it is, and how much of the span it speaks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Window {
    pub(super) start: f64,
    pub(super) len: f64,
    pub(super) weight: f64,
}

/// How many windows a span of `duration` seconds asks for: 3, and one more for every 15 s, up to 12.
pub(super) fn planned_windows(duration: f64) -> usize {
    (3 + (duration.max(0.0) / 15.0) as usize).min(12)
}

/// The order to encode `n` windows (indices in time order) in: the middle first, then the middle of
/// each half, and so on, so that the windows done when the time runs out are spread over the cut.
pub(super) fn spread_order(n: usize) -> Vec<usize> {
    let mut order = Vec::with_capacity(n);
    let mut queue = std::collections::VecDeque::from([(0, n)]);
    while let Some((lo, hi)) = queue.pop_front() {
        if lo < hi {
            let mid = lo + (hi - lo) / 2;
            order.push(mid);
            queue.push_back((lo, mid));
            queue.push_back((mid + 1, hi));
        }
    }
    order
}

/// The clip on top at `t`: the last of the spans that holds it (tracks are listed bottom to top).
fn topmost(spans: &[Span], t: f64) -> Option<Span> {
    spans.iter().rev().find(|s| s.start <= t && t < s.end).copied()
}

/// Where `n` windows of `w` seconds go in `start..end` (pure, deterministic): evenly spaced and snapped
/// inside the clip under them, then re-aimed so that every footage of `spans` has one while another has
/// two; each with the part of the span nearest to it as its `weight`. In time order. A span that is short,
/// or that the windows would cover half of, is one window of its whole length.
pub(super) fn plan_windows(start: f64, end: f64, spans: &[Span], n: usize, w: f64) -> Vec<Window> {
    let d = end - start;
    if d.is_nan() || d <= 0.0 {
        return Vec::new();
    }
    let n = n.max(1);
    if d <= WHOLE_MAX_SECS || n as f64 * w >= d * 0.5 {
        return vec![Window {
            start,
            len: d,
            weight: d,
        }];
    }
    let step = d / n as f64;
    let mut centres: Vec<(f64, Option<Span>)> = (0..n)
        .map(|i| {
            let c = start + (i as f64 + 0.5) * step;
            (c, topmost(spans, c))
        })
        .collect();
    // The footage of the span, each by its longest stretch there.
    let mut sources: Vec<(Uuid, f64, Span)> = Vec::new();
    for s in spans {
        let clipped = Span {
            start: s.start.max(start),
            end: s.end.min(end),
            source: s.source,
        };
        let len = clipped.end - clipped.start;
        if len <= 0.0 {
            continue;
        }
        match sources.iter_mut().find(|(id, ..)| *id == s.source) {
            Some(known) if len > known.1 => *known = (s.source, len, clipped),
            Some(_) => {}
            None => sources.push((s.source, len, clipped)),
        }
    }
    sources.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (id, _, longest) in &sources {
        if centres.iter().any(|(_, s)| s.is_some_and(|s| s.source == *id)) {
            continue;
        }
        let mid = (longest.start + longest.end) / 2.0;
        let windows_of = |source: Uuid| centres.iter().filter(|(_, s)| s.is_some_and(|s| s.source == source)).count();
        let victim = centres
            .iter()
            .enumerate()
            .filter(|(_, (_, s))| s.is_some_and(|s| windows_of(s.source) >= 2))
            .min_by(|a, b| (a.1 .0 - mid).abs().total_cmp(&(b.1 .0 - mid).abs()))
            .map(|(i, _)| i);
        let Some(i) = victim else { break };
        centres[i] = (mid, Some(*longest));
    }
    centres.sort_by(|a, b| a.0.total_cmp(&b.0));
    let starts: Vec<f64> = centres
        .iter()
        .map(|(c, span)| {
            let (lo, hi) = match span {
                Some(s) if s.end.min(end) - s.start.max(start) >= w => (s.start.max(start), s.end.min(end)),
                _ => (start, end),
            };
            (c - w / 2.0).clamp(lo, (hi - w).max(lo))
        })
        .collect();
    let mids: Vec<f64> = starts.iter().map(|s| s + w / 2.0).collect();
    (0..starts.len())
        .map(|i| {
            let from = if i == 0 { start } else { (mids[i - 1] + mids[i]) / 2.0 };
            let to = if i + 1 == starts.len() {
                end
            } else {
                (mids[i] + mids[i + 1]) / 2.0
            };
            Window {
                start: starts[i],
                len: w,
                weight: (to - from).max(0.0),
            }
        })
        .collect()
}

/// The clips that show the span on its picture (video tracks), or, with no picture, on its sound: where
/// each starts and ends on the cut and which asset it is. Tracks bottom to top.
fn clip_spans(rendered: &Timeline, picture: bool) -> Vec<Span> {
    let kind = if picture { StreamKind::Video } else { StreamKind::Audio };
    rendered
        .tracks
        .iter()
        .filter(|t| t.kind == kind)
        .flat_map(|t| t.clips.iter())
        .map(|c| Span {
            start: c.timeline_start,
            end: c.timeline_end(),
            source: c.asset_id,
        })
        .collect()
}

/// What the samples leave out of the cut, and so what the note must name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct LeftOut {
    /// Title clips and SVG overlays: their pictures are drawn under the heavy slot.
    pub(super) titles: bool,
    /// A caption track muxed into the file.
    pub(super) muxed_captions: bool,
    /// The chapters the container carries.
    pub(super) chapters: bool,
    /// A loudness pass (`loudnorm`, or a static gain).
    pub(super) loudness: bool,
}

impl LeftOut {
    /// The sentence naming what is left out; `None` when nothing is.
    pub(super) fn sentence(self) -> Option<String> {
        let named: Vec<&str> = [
            (self.titles, "title clips and SVG overlays"),
            (self.muxed_captions, "captions muxed in"),
            (self.chapters, "chapters"),
            (self.loudness, "loudness normalisation"),
        ]
        .into_iter()
        .filter_map(|(present, name)| present.then_some(name))
        .collect();
        let (last, rest) = named.split_last()?;
        Some(if rest.is_empty() {
            format!("Left out of the samples: {last}.")
        } else {
            format!("Left out of the samples: {} and {last}.", rest.join(", "))
        })
    }
}

/// The cut a sample is encoded from (pure): `cut` without the title clips and SVG overlays (their pictures would be drawn
/// under the heavy slot; every drawn thing in the graph itself, burned captions and typed titles among them, stays), and what
/// `opts` and the cut ask for that a sample does not carry.
pub(super) fn sample_view(cut: &Timeline, opts: &ExportOptions) -> (Timeline, LeftOut) {
    let left_out = LeftOut {
        titles: !cut.svg_overlays.is_empty() || cut.title_clips().next().is_some(),
        muxed_captions: opts.embed_captions && side_files::subtitle_codec(opts.container).is_some(),
        chapters: side_files::carries_chapters(opts.container) && cut.markers.iter().any(|m| m.kind == MarkerKind::Chapter),
        loudness: opts.loudnorm || opts.loudness.is_some(),
    };
    let mut view = cut.clone();
    if left_out.titles {
        view.svg_overlays.clear();
        drop_titles(&mut view);
    }
    (view, left_out)
}

/// The options a sample is encoded with: the export's own, without what needs a pass or a file, or is not the size.
pub(super) fn sample_options(opts: &ExportOptions) -> ExportOptions {
    ExportOptions {
        range: None,
        loudnorm: false,
        loudness: None,
        loudness_static: false,
        loudness_gain_db: None,
        faststart: false,
        embed_captions: false,
        caption_sidecars: Vec::new(),
        overwrite_sidecars: false,
        metadata_title: None,
        subtitle_input: None,
        chapters_input: None,
        // `auto` is dropped where the export drops it; a named accelerator never is.
        hwaccel: opts.hwaccel.as_deref().and_then(|_| {
            export_hwaccel_request(
                opts.hwaccel.as_deref(),
                hwaccel().is_some(),
                HWACCEL_PROVEN_BAD.load(std::sync::atomic::Ordering::Relaxed),
            )
        }),
        ..opts.clone()
    }
}

// ---- reading a window (pure) -------------------------------------------------------------------

#[derive(Deserialize)]
struct PacketProbe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    packets: Vec<ProbePacket>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: Option<String>,
}

#[derive(Deserialize)]
struct ProbePacket {
    stream_index: u32,
    size: Option<String>,
    duration_time: Option<String>,
}

/// What one stream of a window file spent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StreamStats {
    /// Bytes of the first packet: a picture's forced keyframe, a sound's priming.
    pub(super) first_bytes: f64,
    /// Bytes per second over the packets after the first.
    pub(super) rate: f64,
    /// Every packet's bytes, and how long they last: what a whole encode adds up to.
    pub(super) total_bytes: f64,
}

/// A window file's streams.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct WindowStats {
    pub(super) video: Option<StreamStats>,
    pub(super) audio: Option<StreamStats>,
}

/// One stream's `(bytes, seconds)` packets as [`StreamStats`]. `fallback_secs` lasts a packet that states
/// no duration. `None` for a stream with no packet.
pub(super) fn stream_stats(packets: &[(f64, Option<f64>)], fallback_secs: f64) -> Option<StreamStats> {
    let (first, rest) = packets.split_first()?;
    let secs = |p: &(f64, Option<f64>)| p.1.filter(|d| *d > 0.0).unwrap_or(fallback_secs);
    let total_bytes: f64 = packets.iter().map(|p| p.0).sum();
    let (bytes, seconds) = if rest.is_empty() {
        (first.0, secs(first))
    } else {
        (rest.iter().map(|p| p.0).sum::<f64>(), rest.iter().map(secs).sum::<f64>())
    };
    Some(StreamStats {
        first_bytes: first.0,
        rate: if seconds > 0.0 { bytes / seconds } else { 0.0 },
        total_bytes,
    })
}

/// A window file's `ffprobe -show_packets -of json` as its streams' stats (pure). `picture_secs` is how
/// long one picture lasts (a packet that states none) and `len` the window's, for the sound's.
pub(super) fn window_stats(json: &[u8], picture_secs: f64, len: f64) -> Result<WindowStats> {
    let probe: PacketProbe =
        serde_json::from_slice(json).map_err(|e| Error::Engine(format!("could not parse ffprobe's packets: {e}")))?;
    let packets_of = |kind: &str| -> Vec<(f64, Option<f64>)> {
        let Some(stream) = probe.streams.iter().find(|s| s.codec_type.as_deref() == Some(kind)) else {
            return Vec::new();
        };
        probe
            .packets
            .iter()
            .filter(|p| p.stream_index == stream.index)
            .filter_map(|p| {
                let size = p.size.as_deref()?.parse::<f64>().ok()?;
                Some((size, p.duration_time.as_deref().and_then(|d| d.parse::<f64>().ok())))
            })
            .collect()
    };
    let audio = packets_of("audio");
    let audio_fallback = if audio.is_empty() { len } else { len / audio.len() as f64 };
    Ok(WindowStats {
        video: stream_stats(&packets_of("video"), picture_secs),
        audio: stream_stats(&audio, audio_fallback),
    })
}

// ---- putting the windows together (pure) ------------------------------------------------------

/// A window that was encoded: where it is on the cut, what it speaks for, and what it read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Sample {
    pub(super) start: f64,
    pub(super) len: f64,
    pub(super) weight: f64,
    pub(super) stats: WindowStats,
}

/// What the windows add up to over `duration` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Extrapolated {
    /// The picture and sound together, bytes.
    pub(super) bytes: f64,
    /// The standard error of the mean rate, as a share of it.
    pub(super) relative_se: f64,
}

/// What the periodic keyframes add: a window leaves its own forced keyframe out of its rate, a file has `K`
/// of them (`frames / interval`, and one at each cut: a scene change forces one), each costing `I - P`
/// more than the picture it replaces.
pub(super) struct Keyframes {
    pub(super) frames: f64,
    pub(super) interval: Option<f64>,
    pub(super) cuts: usize,
    pub(super) fps: f64,
}

/// The windows' rates over `duration` seconds (pure). `samples` are any windows that were encoded, in any
/// order; weights are renormalised over them, so a run that stopped early still answers.
pub(super) fn extrapolate(samples: &[Sample], duration: f64, keys: &Keyframes) -> Option<Extrapolated> {
    let mut samples = samples.to_vec();
    samples.sort_by(|a, b| a.start.total_cmp(&b.start));
    let weight: f64 = samples.iter().map(|s| s.weight).sum();
    if samples.is_empty() || weight <= 0.0 {
        return None;
    }
    let rate = |s: &Sample| s.stats.video.map_or(0.0, |v| v.rate) + s.stats.audio.map_or(0.0, |a| a.rate);
    let mean = samples.iter().map(|s| s.weight * rate(s)).sum::<f64>() / weight;
    let n = samples.len() as f64;
    let relative_se = if samples.len() < 2 || mean <= 0.0 {
        0.0
    } else {
        let squares: f64 = samples.windows(2).map(|p| (rate(&p[1]) - rate(&p[0])).powi(2)).sum();
        (squares / (2.0 * n * (n - 1.0))).sqrt() / mean
    };
    let mut bytes = mean * duration;
    if let Some(interval) = keys.interval {
        let firsts: Vec<(f64, f64)> = samples
            .iter()
            .filter_map(|s| s.stats.video.map(|v| (s.weight, v.first_bytes)))
            .collect();
        let w: f64 = firsts.iter().map(|f| f.0).sum();
        let video_rate = samples
            .iter()
            .map(|s| s.weight * s.stats.video.map_or(0.0, |v| v.rate))
            .sum::<f64>()
            / weight;
        if w > 0.0 && keys.fps > 0.0 {
            let key = firsts.iter().map(|f| f.0 * f.1).sum::<f64>() / w;
            let per_picture = video_rate / keys.fps;
            let count = (keys.frames / interval.max(1.0) + keys.cuts as f64).min(keys.frames);
            bytes += count * (key - per_picture).max(0.0);
        }
    }
    Some(Extrapolated { bytes, relative_se })
}

/// Student's two-sided 95 % `t` for `df` degrees of freedom (pure; the normal 1.96 from 13 on): what turns the
/// successive-difference standard error of a few windows into a band that holds 19 times in 20 if the footage is typical.
pub(super) fn student_t95(df: usize) -> f64 {
    const T: [f64; 12] = [
        12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179,
    ];
    match df {
        0 => T[0],
        d => T.get(d - 1).copied().unwrap_or(1.96),
    }
}

/// The half-width of the band around an extrapolation, as a share of it (pure): at least 15 %, `t` times the
/// standard error when the windows differ more than that (`t` for the windows there are), opened by
/// `sqrt(planned / done)` when the time stopped it short, 50 % for a single window, 5 % for a whole encode,
/// twice each for a hardware encoder; never past 3.
pub(super) fn band(relative_se: f64, done: usize, planned: usize, whole: bool, hardware: bool) -> f64 {
    let scale = if hardware { 2.0 } else { 1.0 };
    if whole {
        return WHOLE_BAND * scale;
    }
    if done <= 1 {
        return ALONE_BAND.max(MIN_BAND * scale);
    }
    let open = (planned.max(done) as f64 / done as f64).sqrt();
    ((student_t95(done - 1) * relative_se).max(MIN_BAND * scale) * open).min(MAX_BAND)
}

/// `bytes` with `half` under it and `half x` [`HIGH_SKEW`] over it (the low side held above a tenth of it): a burst
/// the windows missed costs more than a lull they missed saves. A whole encode has nothing missed: symmetric.
pub(super) fn banded(bytes: f64, half: f64, whole: bool) -> (f64, f64) {
    let skew = if whole { 1.0 } else { HIGH_SKEW };
    (bytes * (1.0 - half).max(0.1), bytes * (1.0 + half * skew))
}

/// The frames between the periodic keyframes an encoder writes by default (measured on FFmpeg 9.0.2 at 30 fps:
/// x264 and x265 250, VP9 128, SVT-AV1 161 = 5 s rounded up to its mini-GOP of 16, plus one); `None` for an
/// encoder that codes every picture alone (ProRes, GIF). An encoder the model has not measured is read as 250.
pub(super) fn keyframe_interval(opts: &ExportOptions, fps: f64) -> Option<f64> {
    match picture_kind(opts) {
        Ok(Picture::Coded(family, hardware)) => Some(match (family, hardware) {
            (super::export_size::Family::Vp9, _) => 128.0,
            (super::export_size::Family::Av1, EncFamily::Software) => (fps.max(1.0) * 5.0 / 16.0).ceil() * 16.0 + 1.0,
            _ => 250.0,
        }),
        _ => None,
    }
}

/// What a measured note says: how it was measured, what it holds if, and what the samples leave out (pure).
pub(super) struct MeasuredFacts {
    pub(super) whole: bool,
    pub(super) done: usize,
    pub(super) planned: usize,
    pub(super) sampled_secs: f64,
    pub(super) total_secs: f64,
    pub(super) took_secs: f64,
    pub(super) half: f64,
    pub(super) left_out: LeftOut,
}

pub(super) fn measured_note(f: MeasuredFacts) -> String {
    let mut note = if f.whole {
        format!(
            "Measured by encoding the whole {:.0} s cut with these settings in {:.1} s; the file can differ by a few percent.",
            f.total_secs, f.took_secs
        )
    } else {
        format!(
            "Measured by encoding {} sample{} of 1 s ({:.0} of {:.0} s of the cut) with these settings in {:.1} s; \
             the range is -{:.0} % to +{:.0} % around that, and holds if the sampled seconds are typical of the cut: \
             a busy stretch the samples missed would make the file larger.",
            f.done,
            if f.done == 1 { "" } else { "s" },
            f.sampled_secs,
            f.total_secs,
            f.took_secs,
            f.half.min(0.9) * 100.0,
            f.half * HIGH_SKEW * 100.0,
        )
    };
    if !f.whole && f.done < f.planned {
        note += &format!(
            " The time limit stopped it after {} of {} samples, so the range is wider.",
            f.done, f.planned
        );
    }
    if let Some(left_out) = f.left_out.sentence() {
        note += " ";
        note += &left_out;
    }
    note
}

// ---- running it ---------------------------------------------------------------------------------

/// The directory a measurement's window files are in, removed when it goes out of scope: on every way out,
/// a cancel, a failure and a kill included.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kerf-size-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The bytes free on the filesystem of `dir` for an ordinary user; `None` where it cannot be read (not Unix).
#[cfg(unix)]
pub(super) fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` is a plain struct of integers, for which all-zero bytes are a valid value.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a NUL-terminated string that lives through the call and `stat` is a `statvfs` the call fills in.
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut stat) };
    (rc == 0).then(|| (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(not(unix))]
pub(super) fn free_space(_dir: &Path) -> Option<u64> {
    None
}

/// Why a window gave nothing.
enum Stopped {
    Cancelled,
    /// It did not finish in the time it had.
    Late,
    /// It could not be made: the reason, in a few words.
    Failed(String),
}

/// What every window of one file is encoded with.
struct Sampler<'a> {
    /// The cut, without what a window cannot draw (title clips, SVG overlays).
    cut: &'a Timeline,
    assets: &'a [Asset],
    /// The options of the real export, without what is not the size ([`sample_options`]).
    opts: ExportOptions,
    scratch: &'a Scratch,
    picture_secs: f64,
    /// The hard stop of a sample's file, bytes (`-fs`): what it may be written up to.
    fs_limit: u64,
}

impl Sampler<'_> {
    /// Encode `window` with the export's settings, read it, remove it. `hard` is when it must be over.
    fn sample(
        &self,
        k: usize,
        window: Window,
        hard: Instant,
        cancel: &dyn Fn() -> bool,
    ) -> std::result::Result<WindowStats, Stopped> {
        let out = self.scratch.0.join(format!("w{k}.{}", self.opts.container.ext()));
        let named = |h: &String| !h.is_empty() && !h.eq_ignore_ascii_case("none");
        let hw = self.opts.hwaccel.as_ref().is_some_and(named);
        let result = match self.run(&out, window, hard, cancel, self.opts.hwaccel.clone()) {
            // A hardware decode that failed is the export's own retry: once, in software, in what is left of the time.
            Err(Stopped::Failed(_)) if hw && hard.saturating_duration_since(Instant::now()) > Duration::from_secs(1) => {
                self.run(&out, window, hard, cancel, None)
            }
            other => other,
        };
        let _ = std::fs::remove_file(&out);
        result
    }

    fn run(
        &self,
        out: &Path,
        window: Window,
        hard: Instant,
        cancel: &dyn Fn() -> bool,
        hwaccel: Option<String>,
    ) -> std::result::Result<WindowStats, Stopped> {
        let opts = ExportOptions {
            range: Some(TimeRange {
                start: window.start,
                end: window.start + window.len,
            }),
            hwaccel,
            ..self.opts.clone()
        };
        let output = out.to_str().ok_or_else(|| Stopped::Failed("a non-UTF-8 temp path".into()))?;
        let fail = |e: Error| Stopped::Failed(e.to_string());
        let mut args = build_export_args(self.cut, self.assets, output, &opts).map_err(fail)?;
        tune_export_args(&mut args, self.assets, HostOs::CURRENT);
        // A graph too long for argv goes to a script (which probes the binary for its spelling of the flag).
        let _graph = if oversized_graph_index(&args).is_some() {
            externalize_filter_complex(&mut args, "size").map_err(fail)?
        } else {
            GraphScript(None)
        };
        args.splice(0..0, ["-loglevel".to_string(), "error".to_string()]);
        // A hard stop on the file: an output option, so before the output path, the argv's last.
        let at = args.len().saturating_sub(1);
        args.splice(at..at, ["-fs".to_string(), self.fs_limit.to_string()]);
        cpu::cap_args(&mut args, cpu::budget().shared(2));
        let bin = ffmpeg_bin();
        let mut cmd = command(&bin);
        cpu::background_always(&mut cmd);
        cmd.args(&args);
        let left = |hard: Instant| hard.saturating_duration_since(Instant::now());
        let run = match output_bounded_cancellable(cmd, left(hard).max(Duration::from_millis(200)), Some(cancel)) {
            Ok(Some(run)) => run,
            Ok(None) => return Err(Stopped::Cancelled),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(Stopped::Late),
            Err(e) => return Err(Stopped::Failed(launch_err(&bin, e).to_string())),
        };
        if !run.status.success() {
            let tail = String::from_utf8_lossy(&run.stderr);
            let line = tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it failed");
            return Err(Stopped::Failed(format!("ffmpeg: {}", line.trim())));
        }
        if std::fs::metadata(out).is_ok_and(|m| m.len() >= self.fs_limit) {
            return Err(Stopped::Failed("a sample grew past the size it is allowed".into()));
        }
        let probe_bin = ffprobe_bin();
        let mut probe = command(&probe_bin);
        probe
            .args(["-v", "error", "-show_entries"])
            .arg("stream=index,codec_type:packet=stream_index,duration_time,size")
            .args(["-of", "json"])
            .arg(out);
        let probe_limit = left(hard).clamp(Duration::from_secs(2), PROBE_LIMIT);
        let probed = match output_bounded_cancellable(probe, probe_limit, Some(cancel)) {
            Ok(Some(p)) if p.status.success() => p,
            Ok(Some(_)) => return Err(Stopped::Failed("ffprobe could not read the sample".into())),
            Ok(None) => return Err(Stopped::Cancelled),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(Stopped::Late),
            Err(e) => return Err(Stopped::Failed(launch_err(&probe_bin, e).to_string())),
        };
        window_stats(&probed.stdout, self.picture_secs, window.len).map_err(fail)
    }
}

/// How one file's measurement ended.
enum Measured {
    Done(SizeEstimate),
    /// Nothing was measured: why, for the model's note.
    Fell(String),
}

/// What a measurement of one file is given.
struct Job<'a> {
    cut: &'a Timeline,
    assets: &'a [Asset],
    opts: &'a ExportOptions,
    inputs: &'a SizeInputs,
    /// The model's own estimate of the file: its floor under a sampled low end, its high end for what a window may write.
    model: &'a SizeEstimate,
    /// When the sampling ends (a window that has begun may run [`HARD_GRACE`] past it).
    deadline: Instant,
}

/// Measure one file: the cut, as exported with the job's options.
fn measure_file(job: &Job<'_>, progress: &mut dyn FnMut(SizeMeasureProgress), cancel: &dyn Fn() -> bool) -> Result<Measured> {
    let started = Instant::now();
    let (opts, inputs) = (job.opts, job.inputs);
    let hardware = matches!(picture_kind(opts), Ok(Picture::Coded(_, hw)) if hw != EncFamily::Software);
    if hardware && cpu::foreground_busy() {
        return Ok(Measured::Fell(
            "a hardware encoder is not run for a sample while an export is running (its sessions are limited)".into(),
        ));
    }
    let (view, left_out) = sample_view(job.cut, opts);
    let rendered = view.for_render();
    let span = rendered
        .export_span(opts.range.map(|r| (r.start, r.end)))
        .unwrap_or((0.0, rendered.duration()));
    let duration = span.1 - span.0;
    let fps = inputs.picture.map_or(30.0, |p| p.fps);
    let picture_secs = 1.0 / fps.max(1.0);
    let spans = clip_spans(&rendered, inputs.picture.is_some());
    let windows = plan_windows(
        span.0,
        span.1,
        &spans,
        planned_windows(duration),
        WINDOW_SECS.max(8.0 * picture_secs),
    );
    if windows.is_empty() {
        return Ok(Measured::Fell("there is nothing to sample".into()));
    }
    let whole = windows.len() == 1 && windows[0].len >= duration - 1e-9;
    let sample_opts = sample_options(opts);
    if let Err(why) = export_preflight(&view, job.assets, &sample_opts) {
        return Ok(Measured::Fell(why.to_string()));
    }
    // The most a sample's file may reach: the model's high end for the longest window, bounded by the cap and the free space.
    let window_secs = windows.iter().map(|w| w.len).fold(0.0, f64::max);
    let peak = (job.model.high_bytes as f64 * window_secs / duration.max(1e-9)).ceil() as u64;
    let temp = std::env::temp_dir();
    if peak > TEMP_CAP || free_space(&temp).is_some_and(|free| peak.saturating_mul(2) > free) {
        return Ok(Measured::Fell(format!(
            "a sample could take up to {} MB of the temp folder, more than the measurement may use",
            peak / 1_000_000
        )));
    }
    // The first poll is before anything that takes time: the time base backfill below probes the footage.
    if cancel() {
        return Err(Error::Cancelled);
    }
    let backfilled_assets;
    let assets: &[Asset] = if Instant::now() < job.deadline {
        backfilled_assets = backfilled(&view, job.assets, &sample_opts);
        &backfilled_assets
    } else {
        job.assets
    };
    let scratch = match Scratch::new() {
        Ok(scratch) => scratch,
        Err(e) => return Ok(Measured::Fell(format!("a folder for the samples could not be made: {e}"))),
    };
    let sampler = Sampler {
        cut: &view,
        assets,
        opts: sample_opts,
        scratch: &scratch,
        picture_secs,
        fs_limit: (peak.saturating_mul(3)).clamp(1_000_000, TEMP_CAP),
    };
    let planned = windows.len();
    let mut done: Vec<Sample> = Vec::new();
    let mut spent = Duration::ZERO;
    for (nth, &k) in spread_order(planned).iter().enumerate() {
        if cancel() {
            return Err(Error::Cancelled);
        }
        if hardware && cpu::foreground_busy() {
            if done.is_empty() {
                return Ok(Measured::Fell(
                    "a hardware encoder is not run for a sample while an export is running".into(),
                ));
            }
            break;
        }
        let now = Instant::now();
        let left = job.deadline.saturating_duration_since(now);
        // A window the time cannot hold is not begun: it would run past the end and then be killed.
        if nth > 0 && left < spent.div_f64(nth as f64).mul_f64(1.2) {
            break;
        }
        let hard = job
            .deadline
            .checked_add(HARD_GRACE)
            .unwrap_or(job.deadline)
            .max(now.checked_add(MIN_SPAWN).unwrap_or(now));
        let window = windows[k];
        match sampler.sample(k, window, hard, cancel) {
            Ok(stats) => {
                spent += now.elapsed();
                done.push(Sample {
                    start: window.start,
                    len: window.len,
                    weight: window.weight,
                    stats,
                });
                progress(SizeMeasureProgress {
                    windows_done: done.len(),
                    windows_planned: planned,
                });
            }
            Err(Stopped::Cancelled) => return Err(Error::Cancelled),
            Err(Stopped::Late) if done.is_empty() => {
                return Ok(Measured::Fell("a sample did not finish within the time limit".into()));
            }
            Err(Stopped::Failed(why)) if done.is_empty() => return Ok(Measured::Fell(why)),
            Err(_) => break,
        }
    }
    let took = started.elapsed();
    let keys = Keyframes {
        frames: inputs.picture.map_or(0.0, |p| frame_count(p.fps, duration)),
        interval: if whole { None } else { keyframe_interval(opts, fps) },
        cuts: spans
            .iter()
            .filter(|s| s.end > span.0 && s.start < span.1)
            .count()
            .saturating_sub(1),
        fps,
    };
    let sound_secs = if inputs.sound.is_some() { duration } else { 0.0 };
    let (head_low, head_high) = container_overhead(opts, keys.frames, sound_secs);
    // A whole encode is the file's own streams, summed; windows are a rate, taken over the length.
    let (bytes, relative_se) = if whole {
        let stats = done[0].stats;
        (
            stats.video.map_or(0.0, |v| v.total_bytes) + stats.audio.map_or(0.0, |a| a.total_bytes),
            0.0,
        )
    } else {
        let Some(e) = extrapolate(&done, duration, &keys) else {
            return Ok(Measured::Fell("the samples read nothing".into()));
        };
        (e.bytes, e.relative_se)
    };
    let half = band(relative_se, done.len(), planned, whole, hardware);
    let (low, high) = banded(bytes, half, whole);
    let (low_bytes, high_bytes) = (to_bytes(low + head_low), to_bytes((high + head_high).max(low + head_low)));
    let sampled: f64 = done.iter().map(|s| s.len).sum::<f64>().min(duration);
    Ok(Measured::Done(SizeEstimate {
        basis: SizeBasis::Measured,
        low_bytes,
        high_bytes,
        // Sampled seconds give a statistic, not a floor: an error ("it will not fit") may stand on the model's floor or a whole encode.
        floor_bytes: if whole {
            low_bytes
        } else {
            job.model.low_bytes.min(low_bytes)
        },
        note: measured_note(MeasuredFacts {
            whole,
            done: done.len(),
            planned,
            sampled_secs: sampled,
            total_secs: duration,
            took_secs: took.as_secs_f64(),
            half,
            left_out,
        }),
        sampled: Some(Sampled {
            sampled_secs: sampled,
            total_secs: duration,
            whole,
        }),
    }))
}

// ---- the whole export ---------------------------------------------------------------------------

/// What an export of `timeline` with `opts` will weigh, **measured**: the model's estimate ([`estimate_export`]),
/// with every file the model could only bound ([`SizeBasis::Modelled`]: constant quality, ProRes, GIF, lossless,
/// hardware) replaced by a measurement from sample encodes of the cut (see the module docs: what it promises,
/// what the budget bounds, the lane). Bitrate-targeted, exact and unknown files are the model's as they are, and
/// nothing is spawned for them.
///
/// `budget` is the time to spend sampling in all (clamped to [`MAX_MEASURE_BUDGET`]; a window that has begun may run
/// a couple of seconds past it); `progress` hears each window; `cancel` stops it. **The only error is
/// [`Error::Cancelled`]**: by `cancel`, or because a later call started (one measurement runs at a time, the
/// latest). A measurement that could not be made (an encoder that failed, no time for one window, no room in the temp
/// folder, a hardware encoder beside a running export) is the model's estimate with the reason in its note.
/// With deliveries the shapes that share a fit and an aspect share one measurement, scaled by the model's ratio;
/// the others are measured on their own in an equal share of the budget each.
///
/// Reads the **originals** the assets name; touches the disk only for its temp files, removed on every path.
pub fn measure_export_size(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    deliveries: &[Delivery],
    budget: Duration,
    progress: &mut dyn FnMut(SizeMeasureProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<ExportSizeEstimate> {
    let started = Instant::now();
    let deadline = started.checked_add(budget.min(MAX_MEASURE_BUDGET)).unwrap_or(started);
    let mut estimate = estimate_export(timeline, assets, opts, deliveries);
    let groups = measure_groups(&estimate.files);
    if groups.is_empty() {
        return Ok(estimate);
    }
    if cancel() {
        return Err(Error::Cancelled);
    }
    // From here this is the process's latest measurement: an older one still running stops at its next poll.
    let mine = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
    let stop = || cancel() || LATEST.load(Ordering::SeqCst) != mine;
    for (g, group) in groups.iter().enumerate() {
        let reference = reference_of(&estimate.files, group);
        let now = Instant::now();
        let left = deadline.saturating_duration_since(now);
        let outcome = if left < MIN_GROUP_TIME && g > 0 {
            Measured::Fell("the time limit was spent on the other shapes".into())
        } else {
            let share = left / (groups.len() - g) as u32;
            let (cut, file_opts) = match estimate.files[reference].delivery {
                Some(d) => (
                    timeline.for_delivery(d),
                    ExportOptions {
                        resolution: Some((d.width, d.height)),
                        fit: d.fit,
                        ..opts.clone()
                    },
                ),
                None => (timeline.clone(), opts.clone()),
            };
            let inputs = export_size_inputs(&cut, assets, &file_opts);
            let job = Job {
                cut: &cut,
                assets,
                opts: &file_opts,
                inputs: &inputs,
                model: &estimate.files[reference].estimate,
                deadline: now.checked_add(share).unwrap_or(now),
            };
            measure_file(&job, progress, &stop)?
        };
        let reference_model = estimate.files[reference].estimate.clone();
        let reference_frame = estimate.files[reference].frame;
        match outcome {
            Measured::Fell(why) => {
                for &i in group {
                    let file = &mut estimate.files[i];
                    file.estimate.note = format!(
                        "{} A sample encode could not be made ({why}), so this is the model's range.",
                        file.estimate.note
                    );
                }
            }
            Measured::Done(measured) => {
                for &i in group {
                    let file = &mut estimate.files[i];
                    file.estimate = if i == reference {
                        measured.clone()
                    } else {
                        scale_from(&measured, &reference_model, &file.estimate, reference_frame)
                    };
                }
            }
        }
    }
    estimate.total = total_of(&estimate.files);
    Ok(estimate)
}

/// A delivery's fit, its aspect (reduced) and its frame rate (as bits, so it compares).
type ShapeClass = (Fit, (u32, u32), Option<u64>);

/// What shape class a delivery is: its fit, its aspect (reduced) and its frame rate. Two shapes of one class are the same
/// picture at two sizes, and scale by the model's ratio; two of different fit or aspect are different pictures (a contained
/// 16:9 with bars and a cropped one of the same pixels differ ~5x).
fn shape_class(file: &FileSizeEstimate) -> Option<ShapeClass> {
    file.delivery.map(|d| (d.fit, d.ratio(), d.rate().map(f64::to_bits)))
}

/// The files to measure, grouped by shape class in order of first appearance (pure): each group is the indices of files the
/// model could only bound, whose first measured and the rest scaled; a group of one is measured alone.
pub(super) fn measure_groups(files: &[FileSizeEstimate]) -> Vec<Vec<usize>> {
    let mut groups: Vec<(Option<ShapeClass>, Vec<usize>)> = Vec::new();
    for (i, file) in files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.estimate.basis == SizeBasis::Modelled)
    {
        let class = shape_class(file);
        match groups.iter_mut().find(|(c, _)| *c == class) {
            Some((_, members)) => members.push(i),
            None => groups.push((class, vec![i])),
        }
    }
    groups.into_iter().map(|(_, members)| members).collect()
}

/// The file of a group that is encoded: the one with the most pixels (the first of equals).
pub(super) fn reference_of(files: &[FileSizeEstimate], group: &[usize]) -> usize {
    let pixels = |i: usize| files[i].frame.map_or(0, |p| u64::from(p.width) * u64::from(p.height));
    group
        .iter()
        .copied()
        .fold(group[0], |best, i| if pixels(i) > pixels(best) { i } else { best })
}

/// `measured` (of the file the model read as `reference`, shown at `reference_frame`) carried to the file the
/// model reads as `this`, which shares its fit and shape: by the ratio of the middles of the two model bands, with
/// [`SCALED_BAND`] more room on each side (pure).
pub(super) fn scale_from(
    measured: &SizeEstimate,
    reference: &SizeEstimate,
    this: &SizeEstimate,
    reference_frame: Option<ExportedFrame>,
) -> SizeEstimate {
    let middle = |e: &SizeEstimate| (e.low_bytes as f64 * e.high_bytes as f64).sqrt();
    let ratio = if middle(reference) > 0.0 {
        middle(this) / middle(reference)
    } else {
        1.0
    };
    let shape = reference_frame.map_or(String::new(), |f| format!("{}x{} ", f.width, f.height));
    let low_bytes = to_bytes(measured.low_bytes as f64 * ratio / (1.0 + SCALED_BAND));
    SizeEstimate {
        basis: SizeBasis::Measured,
        low_bytes,
        high_bytes: to_bytes(measured.high_bytes as f64 * ratio * (1.0 + SCALED_BAND)),
        floor_bytes: this.low_bytes.min(low_bytes),
        note: format!(
            "{} Scaled from the {shape}file (the same fit and shape) by the size model's ratio, with more room.",
            measured.note
        ),
        sampled: measured.sampled.map(|s| Sampled { whole: false, ..s }),
    }
}
