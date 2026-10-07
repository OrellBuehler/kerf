//! Filmstrip thumbnails: what the timeline draws a video clip from.
//!
//! A clip on the timeline shows a *window* of its asset's picture, at a zoom
//! that changes constantly, so the picture is sampled **once per asset** into a
//! short row of small frames — a filmstrip — cached on disk, and any window is
//! then a handful of thumbnails picked out of it ([`Filmstrip::frame_at`]), the
//! same trick as the waveform pyramid in [`peaks`](super::peaks).
//!
//! # What a thumbnail is
//!
//! Thumbnail `k` is **the frame on screen at source time `k * interval`**: the
//! last frame whose presentation time is at or before that instant, so
//! thumbnail 0 is always the first frame. (ffmpeg's `fps` filter is told to round
//! *up* and start at 0 to say exactly that — its default `near` rounding picks
//! the frame a half interval *later*, so the first tile would show a frame from
//! the middle of the first window.) Tile `k` of a strip therefore nominally
//! covers `[k * interval, (k + 1) * interval)`, but is a sample *of its left
//! edge*, and a caller drawing source time `t` takes the sample nearest in time:
//! thumbnail `clamp(round(t / interval), 0, frames - 1)` — [`Filmstrip::frame_at`].
//! A video shorter than its container's duration (audio longer than picture)
//! simply has fewer thumbnails than `duration / interval`; `frames` is what was
//! really decoded, and a caller's clamp to the last thumbnail covers the rest.
//!
//! **One approximation.** From a sampling interval of 5 s up — assets of ten
//! minutes and more — an *original* (not a proxy) is sampled by keyframes only,
//! because decoding every frame of a long-GOP source to keep one in 150 is an
//! order of magnitude more work (90 s of 1080p H.264: 12.2 s against 0.8 s). A
//! thumbnail is then the last **keyframe** at or before `k * interval` instead of
//! the frame itself: up to a GOP earlier (2 s on a phone, often 8–10 s on a
//! camera), never later, and with a GOP longer than the interval some neighbours
//! repeat. An all-intra proxy has a keyframe on every frame, so a strip built
//! from one is exact; the two share a cache entry (below) and the difference is
//! far below what a 96 px thumbnail shows.
//!
//! # Shape
//!
//! * **Fixed height** [`FILMSTRIP_HEIGHT`] (96 px: a 48 px track at DPR 2 or
//!   the current 64 px one at DPR 1.5 are drawn without upscaling; a taller
//!   track preset scales it up a little rather than pay for a bigger strip — the
//!   bytes grow with the square of the height). The width follows the asset's
//!   *displayed* aspect (rotation applied — ffmpeg autorotates and the probe
//!   reports the displayed size), rounded to an even number: 170 px for 16:9,
//!   54 for a portrait phone clip. A 360 asset is its raw equirect frame, 2:1;
//!   per-clip reframes are not applied, the strip belongs to the asset.
//! * **Interval from the duration** ([`pick_interval`]): the finest rung of
//!   0.5 / 1 / 2 / 5 / 10 / 15 / 30 / 60 / … seconds that keeps the strip within
//!   [`MAX_FILMSTRIP_FRAMES`] (300) — so a 2 minute clip has a thumbnail every
//!   0.5 s, a 10 minute one every 2 s, an hour every 15 s. A still is one
//!   thumbnail.
//! * **Sheets, one row each.** Frames are `tile`d into JPEG sheets at most
//!   [`MAX_SHEET_WIDTH`] (8192 px) wide, balanced so no sheet is mostly padding
//!   ([`sheet_layout`]); sheet `s` holds frames `s * columns ..`, thumbnail `k`
//!   sits at `x = (k % columns) * frame_width`, `y = 0`. A strip is about 3-6 KB
//!   per frame — at most roughly 2 MB for 300.
//!
//! # How it is made
//!
//! One decode of the asset's **proxy when one is ready, else the original**
//! (never waiting for the proxy to finish): `fps` first, so only the wanted
//! frames are scaled, then `scale` to the thumbnail size, then the shared HDR
//! tone-map (`source_hdr` + `tonemap_chain`: the proxy answers "SDR" because it
//! was converted when it was encoded, the original is converted here). The
//! decode streams **raw thumbnails** over a pipe, which is what lets it be held
//! to the same no-hang rule as the waveform decode (a decode that produces no
//! thumbnail for [`stall_for`] is killed) and what makes the thumbnail count
//! exact rather than inferred from padding; a second, instant ffmpeg then tiles
//! and JPEG-encodes them. A proxy that fails to decode falls back to the original,
//! a hardware decode that fails to software.
//!
//! The frames look the same from the proxy and from the original, and a proxy
//! keeps frame times 1:1 with its source, so the **cache key is the original's**
//! (path + size + mtime, plus thumbnail height, width, interval and a format
//! version): a strip made from the original while the proxy was still encoding
//! is the same entry the proxy would have produced, and is never rebuilt when the
//! proxy lands.
//!
//! * **Ungated, but a background job held tighter than the budget** —
//!   [`cpu::lease`] is the gate for jobs whose point is the *result of the whole
//!   file*, wanted later. A filmstrip reads the whole file but is what the
//!   timeline is drawn from, like the waveform pyramid: gated, it would sit
//!   behind the proxy encode and the import's analysis (minutes) while the user
//!   watches empty clips. But unlike a waveform it is a *video* decode, and with
//!   no proxy yet it runs beside the encode of the very file it samples — so it
//!   is capped at [`decode_threads`] (a quarter of the cores, one or two) and
//!   niced **at every CPU budget, 100% included**, where `cpu::limit_args` /
//!   `cpu::background` stand down: "at 100% nothing changes" is for the job that
//!   owns the machine, not for the side job that must stay out of its way. At
//!   most [`MAX_CONCURRENT_DECODES`] run at a time, and concurrent requests for
//!   the *same* asset share one decode.
//! * **A short decode is not trusted.** ffmpeg exits 0 on a file that goes bad
//!   halfway, and that looks exactly like a video that is really short — and a
//!   strip cached from it would stay truncated for good. So when fewer
//!   thumbnails arrive than planned, the video stream's own duration (`ffprobe`,
//!   off the lock, only on a shortfall) says how many to expect
//!   ([`predicted_frames`]); more than [`SHORTFALL_TOLERANCE`] short and the strip
//!   is returned and memoized for the session but **never written to disk**
//!   ([`decode_checked`]). A keyframe pass that falls short is retried with every
//!   frame first.
//! * **Cached at `<cache>/kerf/filmstrips/<hash>/`**: the sheets as
//!   `sheet-000.jpg …` plus a `manifest.json`, built in a `.part` directory and
//!   renamed into place, so a crash never leaves half a strip. A directory that
//!   fails validation — wrong version or shape, a missing or resized sheet, a
//!   sheet that is not a JPEG of the size the manifest says — is rebuilt, never
//!   trusted. An in-process, byte-bounded LRU memo sits in front of the disk.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::cli::{
    command, decode_hwaccel, disable_hwaccel, ffmpeg_bin, ffprobe_bin, fnv1a, launch_err, source_hdr, source_key, tonemap_filter,
};
use super::cpu;
use super::peaks::drain_tail;
use crate::error::{Error, Result};
use crate::model::{Asset, StreamKind};

/// Height of every thumbnail, in pixels. See the module docs for why 96.
pub const FILMSTRIP_HEIGHT: u32 = 96;

/// The most thumbnails one strip holds, whatever the asset's length.
pub const MAX_FILMSTRIP_FRAMES: u32 = 300;

/// The widest a single JPEG sheet is allowed to be, in pixels: as wide as the
/// webview's canvas and GPU texture paths reliably take.
pub const MAX_SHEET_WIDTH: u32 = 8192;

/// The widest a single thumbnail may be: an extreme panorama is squeezed rather
/// than allowed to make a sheet hold a handful of frames.
const MAX_FRAME_WIDTH: u32 = 1024;

/// The sampling intervals, in seconds, a strip can have, finest first. Every
/// rung is a whole number of half-seconds, which is what lets [`fps_rate`] spell
/// the rate as an exact rational.
const INTERVAL_LADDER: [f64; 13] = [
    0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
];

/// `-q:v` of the sheets' JPEG encode: 2 is near-lossless, 31 the worst. 4 is
/// clean at this size and keeps a frame to a few KB.
const JPEG_QUALITY: &str = "4";

/// Bumped whenever the sampling, the geometry or the on-disk layout changes; it
/// is part of the cache key *and* the manifest.
const CACHE_VERSION: u32 = 1;

const MANIFEST_FILE: &str = "manifest.json";

/// Whole-file decodes allowed to run at once. Ungated against exports (see the
/// module docs), but not unbounded: opening a project with thirty clips would
/// otherwise start thirty ffmpegs.
const MAX_CONCURRENT_DECODES: usize = 2;

/// Byte budget for the in-process strip memo.
const MEMO_MAX_BYTES: usize = 48 << 20;

/// The shortest a decode may go without producing a thumbnail before it is
/// killed (see [`stall_for`]) — start-up, hardware initialization and the first
/// seek all live inside it.
const STALL_FLOOR: Duration = Duration::from_secs(60);

/// The longest [`stall_for`] ever allows.
const STALL_CEILING: Duration = Duration::from_secs(30 * 60);

/// How long tiling and encoding the already-decoded thumbnails may take. It is a
/// few hundred small frames; this is a bound on a wedge, not on work.
const SHEET_ENCODE_LIMIT: Duration = Duration::from_secs(120);

/// Threads one decode may use, whatever the CPU budget says — see
/// [`decode_threads`].
const MAX_DECODE_THREADS: usize = 2;

/// From this sampling interval up, an *original* is sampled by keyframes only
/// (see the module docs): below it the thumbnails are close enough together that
/// a keyframe a GOP earlier would visibly be the wrong picture.
const KEYFRAME_MIN_INTERVAL: f64 = 5.0;

/// How many thumbnails short of what the video's own length predicts a decode may
/// fall before the strip is called incomplete (see [`predicted_frames`]). Two:
/// the last window or two of a video can legitimately have no sample of their own
/// (the last keyframe, or the last frame's duration, ends before them).
const SHORTFALL_TOLERANCE: u32 = 2;

/// How long the video-duration probe may take.
const PROBE_LIMIT: Duration = Duration::from_secs(20);

/// A `.part` directory older than this is a crashed build's leftover.
const STALE_PART: Duration = Duration::from_secs(24 * 60 * 60);

/// The largest a cached sheet or manifest may be before it is distrusted.
const MAX_SHEET_BYTES: u64 = 64 << 20;
const MAX_MANIFEST_BYTES: u64 = 1 << 20;

// ---- the result ------------------------------------------------------------

/// One JPEG of a [`Filmstrip`]: `count` thumbnails side by side.
///
/// Serializes (snake_case) *without* the pixels, e.g.
/// `{"first_frame":64,"count":36,"width":3400,"height":96}`; a surface adds its
/// own transport for [`FilmstripSheet::jpeg`] (the app sends a base64 `data:` URL).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FilmstripSheet {
    /// Index of the first thumbnail on this sheet.
    pub first_frame: u32,
    /// Thumbnails on this sheet.
    pub count: u32,
    /// Pixel width of the JPEG. Always `Filmstrip::columns * frame_width`, so the
    /// last sheet of a strip may be wider than `count * frame_width` — the tail is
    /// padding; use `count`, not the width, to know how many thumbnails it holds.
    pub width: u32,
    /// Pixel height of the JPEG: [`Filmstrip::frame_height`].
    pub height: u32,
    /// The JPEG file's bytes.
    #[serde(skip)]
    pub jpeg: Arc<[u8]>,
}

/// An asset's thumbnails over time. See the module docs for what a thumbnail
/// is and where the sheets come from.
///
/// Serializes (snake_case, without the sheets' pixels), e.g. a 10 s clip:
/// `{"interval":0.5,"frame_width":170,"frame_height":96,"frames":20,"columns":20,
///   "sheets":[{"first_frame":0,"count":20,"width":3400,"height":96}]}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Filmstrip {
    /// Seconds of source between two thumbnails: thumbnail `k` is the frame at
    /// `k * interval` (from 5 s up, on a strip built from an original rather than
    /// a proxy: the last keyframe at or before it — see the module docs).
    pub interval: f64,
    /// Width of one thumbnail in pixels (even).
    pub frame_width: u32,
    /// Height of one thumbnail in pixels: [`FILMSTRIP_HEIGHT`].
    pub frame_height: u32,
    /// Thumbnails in the strip (at least 1, at most [`MAX_FILMSTRIP_FRAMES`]).
    pub frames: u32,
    /// Thumbnails per sheet (the last sheet may hold fewer): thumbnail `k` is on
    /// sheet `k / columns`, at `x = (k % columns) * frame_width`, `y = 0`.
    pub columns: u32,
    /// The sheets, in order; together they hold thumbnails `0..frames`.
    pub sheets: Vec<FilmstripSheet>,
}

impl Filmstrip {
    /// The source time thumbnail `frame` shows.
    pub fn time_of(&self, frame: u32) -> f64 {
        f64::from(frame) * self.interval
    }

    /// The thumbnail to draw for source time `t`: the sample nearest in time,
    /// clamped into the strip (a time before the start, or a NaN, is the first
    /// thumbnail; one past the end is the last).
    pub fn frame_at(&self, t: f64) -> u32 {
        let last = self.frames.saturating_sub(1);
        let k = (t / self.interval).round();
        if k.is_nan() || k <= 0.0 {
            0
        } else {
            (k.min(f64::from(last))) as u32
        }
    }

    /// The sheet holding thumbnail `frame` and the thumbnail's x offset on it, or
    /// `None` past the end of the strip.
    pub fn locate(&self, frame: u32) -> Option<(&FilmstripSheet, u32)> {
        let sheet = self
            .sheets
            .iter()
            .find(|s| frame >= s.first_frame && frame - s.first_frame < s.count)?;
        Some((sheet, (frame - sheet.first_frame) * self.frame_width))
    }

    /// Bytes held (the JPEGs plus a little overhead), for the memo's budget and
    /// for a surface that wants to bound what it sends.
    pub fn approx_bytes(&self) -> usize {
        self.sheets.iter().map(|s| s.jpeg.len()).sum::<usize>() + 256
    }
}

// ---- the plan: what to sample ---------------------------------------------

/// What a strip for one asset is made of — pure, derived from the asset alone.
#[derive(Debug, Clone, PartialEq)]
struct Plan {
    interval: f64,
    /// Thumbnails asked for. The decode can deliver fewer (a video shorter than
    /// its container), never more.
    frames: u32,
    frame_width: u32,
    frame_height: u32,
    /// A still image: no source timeline, so no `fps`, no seeking, one frame.
    still: bool,
}

impl Plan {
    fn for_asset(asset: &Asset) -> Result<Self> {
        // Like the waveform's audio check: say what is wrong before spawning an
        // ffmpeg to fail on it. (An asset with no stream info at all is tried.)
        let video = asset.streams.iter().find(|s| s.kind == StreamKind::Video);
        if video.is_none() && !asset.streams.is_empty() {
            return Err(Error::InvalidArgument(format!("asset {} has no video stream", asset.id)));
        }
        let still = asset.is_image();
        let (interval, frames) = if still {
            (asset.duration.max(1.0), 1)
        } else {
            let interval = pick_interval(asset.duration);
            (interval, frames_for(asset.duration, interval))
        };
        Ok(Self {
            interval,
            frames,
            frame_width: thumb_width(video.and_then(|v| v.width), video.and_then(|v| v.height)),
            frame_height: FILMSTRIP_HEIGHT,
            still,
        })
    }

    /// Bytes of one raw `yuv420p` thumbnail.
    fn frame_bytes(&self) -> usize {
        self.frame_width as usize * self.frame_height as usize * 3 / 2
    }
}

/// Thumbnails a `duration`-second asset has at `interval`: one per started
/// interval, and always at least one.
fn frames_for(duration: f64, interval: f64) -> u32 {
    if !duration.is_finite() || duration <= 0.0 {
        return 1;
    }
    // The small epsilon keeps an exact multiple (10 s at 0.5 s) from becoming one
    // frame more through float error.
    let n = (duration / interval - 1e-6).ceil().max(1.0);
    n.min(f64::from(u32::MAX)) as u32
}

/// The sampling interval for a `duration`-second asset: the finest rung of the
/// ladder (0.5 s … 1 h) that keeps the strip within [`MAX_FILMSTRIP_FRAMES`]. A
/// duration past the ladder's reach (hundreds of hours) gets whole seconds. An
/// unknown or non-positive duration is a one second interval — and one frame.
pub fn pick_interval(duration: f64) -> f64 {
    if !duration.is_finite() || duration <= 0.0 {
        return 1.0;
    }
    INTERVAL_LADDER
        .iter()
        .copied()
        .find(|&rung| frames_for(duration, rung) <= MAX_FILMSTRIP_FRAMES)
        .unwrap_or_else(|| (duration / f64::from(MAX_FILMSTRIP_FRAMES)).ceil())
}

/// Width of a thumbnail for a `width`x`height` picture: [`FILMSTRIP_HEIGHT`]'s
/// worth of the aspect, rounded to an even number (4:2:0 chroma needs one) and
/// kept in `2..=MAX_FRAME_WIDTH`. A picture of unknown size is taken as 16:9.
///
/// The aspect is the *coded* one: `StreamInfo` carries no sample aspect ratio, so
/// an anamorphic source (1440x1080 flagged 4:3 pixels) gets 4:3 thumbnails where
/// a player would show 16:9. That is the shape every other geometry here — the
/// project frame, fit and crop, the preview and the export — already works in, so
/// the strip matches what Kerf itself draws; if the probe ever records the SAR,
/// this is the one place to apply it (and `CACHE_VERSION` to bump).
fn thumb_width(width: Option<u32>, height: Option<u32>) -> u32 {
    let (w, h) = match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (f64::from(w), f64::from(h)),
        _ => (16.0, 9.0),
    };
    let even = (f64::from(FILMSTRIP_HEIGHT) * w / h / 2.0).round() * 2.0;
    (even as u32).clamp(2, MAX_FRAME_WIDTH)
}

/// `(columns, sheets)` for `frames` thumbnails `frame_width` px wide: as few
/// sheets as [`MAX_SHEET_WIDTH`] allows, with the frames spread evenly over them
/// so the last is not nearly all padding.
fn sheet_layout(frames: u32, frame_width: u32) -> (u32, u32) {
    let frames = frames.max(1);
    let widest = (MAX_SHEET_WIDTH / frame_width.max(1)).max(1);
    let sheets = frames.div_ceil(widest);
    let columns = frames.div_ceil(sheets);
    // Recounted from `columns`, so the last sheet always holds at least one.
    (columns, frames.div_ceil(columns))
}

/// `fps=`'s argument for one frame per `interval` seconds, as a reduced rational
/// (`2/1`, `1/2`, `1/15`). Every interval is a whole number of half-seconds, so
/// the rate is exactly `2 / half_seconds`.
fn fps_rate(interval: f64) -> String {
    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let half = ((interval * 2.0).round() as u64).max(1);
    let g = gcd(2, half);
    format!("{}/{}", 2 / g, half / g)
}

/// How long a decode of a strip sampled every `interval` seconds may go without
/// producing a thumbnail before it is killed: a thumbnail costs `interval`
/// seconds of decoding, so a decode slower than a sixth of real time is a wedge
/// and not a slow machine — within a floor that covers start-up and a ceiling
/// that keeps even a wedged hour-long sampling finite.
fn stall_for(interval: f64) -> Duration {
    // Clamped in seconds first: `from_secs_f64` panics on infinity, and a still
    // whose container reports an absurd duration has an absurd interval.
    let secs = (interval * 6.0).max(0.0).min(STALL_CEILING.as_secs_f64());
    Duration::from_secs_f64(secs).max(STALL_FLOOR)
}

/// Threads one filmstrip decode may use: a quarter of the cores, between one
/// and [`MAX_DECODE_THREADS`], and never above the CPU budget. **At any budget,
/// 100% included** (`cpu::cap_args`): a filmstrip is ungated, so it runs beside
/// whatever holds the heavy-job lease — usually the proxy encode of the very asset
/// it is sampling, or an export — and "a full budget leaves ffmpeg alone" is about
/// the job that has the machine, not the side job that must stay out of its way.
/// (Two decodes may run at once, so at most twice this in all.) The cap costs
/// little: decoding scales poorly across threads — 90 s of 1080p H.264 took 15.6 s
/// on one thread, 13.2 s on two, 9.5 s on four.
fn decode_threads(cores: usize, budget: usize) -> usize {
    budget.min((cores / 4).clamp(1, MAX_DECODE_THREADS)).max(1)
}

// ---- ffmpeg arguments (pure) -----------------------------------------------

/// The `-vf` chain that turns a decoded picture into raw thumbnails: `fps`
/// first (rounding up, from 0 — see the module docs for why that is "the frame on
/// screen at `k * interval`"), so only the wanted frames are scaled, then the
/// scale itself, then — for an HDR source — the tone-map, on frames that are by
/// now tiny. A still has one frame and needs no `fps`.
fn thumb_filter(plan: &Plan, tonemap: Option<&str>) -> String {
    let mut chain: Vec<String> = Vec::new();
    if !plan.still {
        chain.push(format!("fps=fps={}:start_time=0:round=up", fps_rate(plan.interval)));
    }
    chain.push(format!("scale={}:{}:flags=area", plan.frame_width, plan.frame_height));
    // The tone-map chain ends in `format=yuv420p` itself.
    chain.push(tonemap.map_or_else(|| "format=yuv420p".to_string(), str::to_string));
    chain.join(",")
}

/// Stage one: decode `src` to raw `yuv420p` thumbnails on stdout. Pure — thread
/// caps and priority are applied where it is spawned. `hwaccel` is the decode
/// acceleration (never for a still: there is nothing to accelerate);
/// `keyframes_only` is `-skip_frame nokey`, the decoder skipping every frame that
/// is not a keyframe so the `fps` filter downstream picks, for each sample time,
/// the last *keyframe* at or before it (see the module docs).
fn build_thumb_args(src: &str, plan: &Plan, tonemap: Option<&str>, hwaccel: Option<&str>, keyframes_only: bool) -> Vec<String> {
    let s = |v: &str| v.to_string();
    let mut args = vec![s("-hide_banner"), s("-loglevel"), s("error"), s("-nostdin")];
    if let Some(hw) = hwaccel.filter(|_| !plan.still) {
        args.extend([s("-hwaccel"), s(hw)]);
    }
    if keyframes_only && !plan.still {
        args.extend([s("-skip_frame"), s("nokey")]);
    }
    args.extend([
        s("-i"),
        s(src),
        s("-an"),
        s("-sn"),
        s("-dn"),
        s("-vf"),
        thumb_filter(plan, tonemap),
        s("-frames:v"),
        plan.frames.to_string(),
        s("-pix_fmt"),
        s("yuv420p"),
        s("-f"),
        s("rawvideo"),
        s("pipe:1"),
    ]);
    args
}

/// Stage two: raw thumbnails on stdin, `tile`d `columns` to a row and written as
/// `sheet-000.jpg …` in the working directory (a relative pattern, because the
/// pattern's `%` is syntax and a cache directory's name is not ours to choose).
fn build_sheet_args(plan: &Plan, columns: u32) -> Vec<String> {
    let s = |v: &str| v.to_string();
    vec![
        s("-hide_banner"),
        s("-loglevel"),
        s("error"),
        s("-y"),
        s("-f"),
        s("rawvideo"),
        s("-pixel_format"),
        s("yuv420p"),
        s("-video_size"),
        format!("{}x{}", plan.frame_width, plan.frame_height),
        s("-framerate"),
        s("25"),
        s("-i"),
        s("pipe:0"),
        s("-vf"),
        format!("tile={columns}x1"),
        s("-c:v"),
        s("mjpeg"),
        s("-q:v"),
        s(JPEG_QUALITY),
        s("-f"),
        s("image2"),
        s("-start_number"),
        s("0"),
        s("sheet-%03d.jpg"),
    ]
}

fn sheet_name(index: u32) -> String {
    format!("sheet-{index:03}.jpg")
}

// ---- reading a JPEG's size -------------------------------------------------

/// `(width, height)` from a JPEG's start-of-frame header, or `None` if the bytes
/// do not begin like one. Reads headers only; the entropy-coded data is never
/// touched, so it is a size check and not a validity proof.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2usize;
    loop {
        if *bytes.get(i)? != 0xFF {
            return None;
        }
        // Any number of 0xFF fill bytes may precede a marker code.
        while *bytes.get(i + 1)? == 0xFF {
            i += 1;
        }
        let marker = *bytes.get(i + 1)?;
        i += 2;
        match marker {
            // Standalone markers: no length follows.
            0x01 | 0xD0..=0xD8 => continue,
            // The end, or the scan, before any frame header.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*bytes.get(i)?, *bytes.get(i + 1)?]));
        if len < 2 {
            return None;
        }
        // SOF0..SOF15 except DHT (C4), JPG (C8) and DAC (CC).
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let height = u16::from_be_bytes([*bytes.get(i + 3)?, *bytes.get(i + 4)?]);
            let width = u16::from_be_bytes([*bytes.get(i + 5)?, *bytes.get(i + 6)?]);
            return (width > 0 && height > 0).then_some((u32::from(width), u32::from(height)));
        }
        i += len;
    }
}

// ---- the on-disk cache -----------------------------------------------------

/// `manifest.json`: everything about a cached strip except its pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    interval: f64,
    frame_width: u32,
    frame_height: u32,
    frames: u32,
    columns: u32,
    sheets: Vec<ManifestSheet>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ManifestSheet {
    file: String,
    first_frame: u32,
    count: u32,
    width: u32,
    height: u32,
    /// Size of the file, so a truncated sheet is caught before it is decoded.
    bytes: u64,
}

impl Manifest {
    fn of(strip: &Filmstrip) -> Self {
        Self {
            version: CACHE_VERSION,
            interval: strip.interval,
            frame_width: strip.frame_width,
            frame_height: strip.frame_height,
            frames: strip.frames,
            columns: strip.columns,
            sheets: strip
                .sheets
                .iter()
                .enumerate()
                .map(|(i, s)| ManifestSheet {
                    file: sheet_name(i as u32),
                    first_frame: s.first_frame,
                    count: s.count,
                    width: s.width,
                    height: s.height,
                    bytes: s.jpeg.len() as u64,
                })
                .collect(),
        }
    }

    /// Whether this is a manifest this build would have written for some asset:
    /// the right version, a geometry that follows from the frame count (the
    /// canonical [`sheet_layout`], sheets contiguous and in order, each file named
    /// as it is made) and no sizes out of range. A hand-damaged or foreign file
    /// fails here, before anything is read off disk on its say-so.
    fn is_canonical(&self) -> bool {
        if self.version != CACHE_VERSION
            || !self.interval.is_finite()
            || self.interval <= 0.0
            || self.frame_height != FILMSTRIP_HEIGHT
            || !(2..=MAX_FRAME_WIDTH).contains(&self.frame_width)
            || !self.frame_width.is_multiple_of(2)
            || !(1..=MAX_FILMSTRIP_FRAMES).contains(&self.frames)
        {
            return false;
        }
        let (columns, sheets) = sheet_layout(self.frames, self.frame_width);
        if self.columns != columns || self.sheets.len() != sheets as usize {
            return false;
        }
        self.sheets.iter().enumerate().all(|(i, s)| {
            let first = i as u32 * columns;
            s.file == sheet_name(i as u32)
                && s.first_frame == first
                && s.count == columns.min(self.frames - first)
                && s.width == columns * self.frame_width
                && s.height == self.frame_height
                && (1..=MAX_SHEET_BYTES).contains(&s.bytes)
        })
    }

    /// Whether the strip is the one `plan` asks for. `frames` may be *fewer* than
    /// planned (a video shorter than its container); the rest is exact.
    fn fits(&self, plan: &Plan) -> bool {
        self.interval == plan.interval
            && self.frame_width == plan.frame_width
            && self.frame_height == plan.frame_height
            && self.frames <= plan.frames
    }
}

/// The directory name of `asset`'s strip under the cache root, and the in-process
/// memo's key. The *original's* identity (see the module docs), so a strip made
/// from the proxy and one made from the source are one entry.
fn entry_key(asset: &Asset, plan: &Plan) -> String {
    let key = format!(
        "{}|filmstrip-v{CACHE_VERSION}|h{}|w{}|i{}|n{}",
        source_key(Path::new(&asset.path)),
        plan.frame_height,
        plan.frame_width,
        plan.interval,
        plan.frames,
    );
    format!("{:016x}", fnv1a(&key))
}

/// `<cache>/kerf/filmstrips`, or `None` when the OS has no cache directory (the
/// strip is then only memoized in process).
fn cache_root() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("kerf").join("filmstrips"))
}

/// Test-only: where `asset`'s strip is cached, so a test that built one in the
/// real cache can clean up after itself.
#[cfg(test)]
pub(crate) fn cached_entry_dir(asset: &Asset) -> Option<PathBuf> {
    let plan = Plan::for_asset(asset).ok()?;
    Some(cache_root()?.join(entry_key(asset, &plan)))
}

/// Load and fully validate the strip in `dir` against `plan`: a manifest that is
/// canonical and fits, and for every sheet a file of exactly the promised size
/// that is a JPEG of exactly the promised dimensions. `None` for anything else —
/// absent, short, resized, renamed, from another version.
fn load_entry(dir: &Path, plan: &Plan) -> Option<Filmstrip> {
    let manifest_path = dir.join(MANIFEST_FILE);
    if std::fs::metadata(&manifest_path).ok()?.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(manifest_path).ok()?).ok()?;
    if !manifest.is_canonical() || !manifest.fits(plan) {
        return None;
    }
    let mut sheets = Vec::with_capacity(manifest.sheets.len());
    for entry in &manifest.sheets {
        let path = dir.join(&entry.file);
        if std::fs::metadata(&path).ok()?.len() != entry.bytes {
            return None;
        }
        let jpeg = std::fs::read(&path).ok()?;
        if jpeg.len() as u64 != entry.bytes
            || !jpeg.ends_with(&[0xFF, 0xD9])
            || jpeg_dimensions(&jpeg) != Some((entry.width, entry.height))
        {
            return None;
        }
        sheets.push(FilmstripSheet {
            first_frame: entry.first_frame,
            count: entry.count,
            width: entry.width,
            height: entry.height,
            jpeg: jpeg.into(),
        });
    }
    Some(Filmstrip {
        interval: manifest.interval,
        frame_width: manifest.frame_width,
        frame_height: manifest.frame_height,
        frames: manifest.frames,
        columns: manifest.columns,
        sheets,
    })
}

/// Write `strip`'s manifest into `work` (whose sheets are already there) and
/// move the whole directory into place at `dest`. The sheets and manifest are
/// complete before the rename, so a reader — or a crash — never sees half a
/// strip. A directory already at `dest` is the one [`load_entry`] just rejected
/// (or lost a race to us), and is replaced.
fn publish(work: &Path, dest: &Path, strip: &Filmstrip) -> std::io::Result<()> {
    std::fs::write(work.join(MANIFEST_FILE), serde_json::to_vec(&Manifest::of(strip))?)?;
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::rename(work, dest)
}

/// Remove `.part` directories under `root` that a crashed build left behind.
/// Best effort: a failure here is only disk space.
fn sweep_stale(root: &Path, older_than: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().ends_with(".part")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > older_than);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

// ---- decoding: stage one ---------------------------------------------------

/// A turn at one of [`MAX_CONCURRENT_DECODES`] decode slots, held until dropped.
struct DecodeSlot;

struct Slots {
    busy: Mutex<usize>,
    free: Condvar,
}

fn slots() -> &'static Slots {
    static SLOTS: OnceLock<Slots> = OnceLock::new();
    SLOTS.get_or_init(|| Slots {
        busy: Mutex::new(0),
        free: Condvar::new(),
    })
}

impl DecodeSlot {
    fn acquire() -> Self {
        let slots = slots();
        let mut busy = slots.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy >= MAX_CONCURRENT_DECODES {
            busy = slots.free.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        *busy += 1;
        DecodeSlot
    }
}

impl Drop for DecodeSlot {
    fn drop(&mut self) {
        let slots = slots();
        *slots.busy.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        slots.free.notify_one();
    }
}

/// Read a running ffmpeg's stdout as raw frames of `frame_bytes` bytes each and
/// hand them, whole frames only, to `on_frames` until it ends. A child that
/// produces nothing for `stall` is killed and the call fails; a non-zero exit
/// fails it too. Always reaps the child.
fn pump_raw(mut child: Child, frame_bytes: usize, stall: Duration, on_frames: &mut dyn FnMut(&[u8])) -> Result<()> {
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || drain_tail(stderr));

    // stdout is read on its own thread so the loop below can give up on a
    // silent ffmpeg instead of blocking in `read`. The bounded channel is the
    // backpressure: a slow consumer fills the pipe and throttles the decode.
    let mut stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(4);
    // Detached: after a kill it ends on EOF, or on the dropped receiver.
    std::thread::spawn(move || {
        use std::io::Read;
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let msg = match stdout.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => Ok(chunk[..n].to_vec()),
                Err(e) => Err(e),
            };
            let failed = msg.is_err();
            if tx.send(msg).is_err() || failed {
                return;
            }
        }
    });

    // A pipe read need not land on a frame boundary: the straddling tail waits
    // here for the next one.
    let mut pending: Vec<u8> = Vec::new();
    let failure = loop {
        match rx.recv_timeout(stall) {
            Ok(Ok(bytes)) => {
                pending.extend_from_slice(&bytes);
                let whole = pending.len() / frame_bytes * frame_bytes;
                if whole > 0 {
                    on_frames(&pending[..whole]);
                    pending.drain(..whole);
                }
            }
            Ok(Err(e)) => break Some(format!("filmstrip read failed: {e}")),
            Err(RecvTimeoutError::Disconnected) => break None,
            Err(RecvTimeoutError::Timeout) => {
                break Some(format!(
                    "filmstrip decode stalled: ffmpeg produced no frame for {}s",
                    stall.as_secs_f32()
                ));
            }
        }
    };

    if failure.is_some() {
        let _ = child.kill();
    }
    drop(rx);
    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    let stderr_text = stderr_handle.join().unwrap_or_default();
    let tail = || {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(12).collect();
        tail.reverse();
        tail.join("\n").trim().to_string()
    };
    if let Some(reason) = failure {
        return Err(Error::Engine(format!("{reason}\n{}", tail()).trim().to_string()));
    }
    if !status.success() {
        return Err(Error::Engine(format!("could not decode frames: {}", tail())));
    }
    Ok(())
}

/// What one decode produced and where it came from.
struct Decoded {
    /// Raw `yuv420p` thumbnails, back to back.
    raw: Vec<u8>,
    /// The file that was decoded (the proxy, or the original).
    source: PathBuf,
    /// Whether it was sampled by keyframes only.
    keyframes_only: bool,
}

/// One decode of `path` into raw thumbnails, with `hwaccel` and / or keyframe
/// sampling if given.
fn decode_once(path: &Path, plan: &Plan, hwaccel: Option<&str>, keyframes_only: bool) -> Result<Vec<u8>> {
    let src = path
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    // The conversion an HDR original needs; a proxy answers `None` (it was
    // converted when it was encoded), so it is never done twice.
    let tonemap = source_hdr(path).map(tonemap_filter);
    let mut args = build_thumb_args(src, plan, tonemap.as_deref(), hwaccel, keyframes_only);
    cpu::cap_args(&mut args, decode_threads(cpu::cores(), cpu::budget_threads()));

    let _slot = DecodeSlot::acquire();
    let bin = ffmpeg_bin();
    // Ungated (see the module docs), but capped and niced at every budget: it
    // runs beside the heavy job, not behind it.
    let mut cmd = command(&bin);
    cpu::background_always(&mut cmd);
    let child = cmd
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let frame_bytes = plan.frame_bytes();
    let mut raw: Vec<u8> = Vec::with_capacity(frame_bytes * plan.frames as usize);
    pump_raw(child, frame_bytes, stall_for(plan.interval), &mut |frames| {
        raw.extend_from_slice(frames);
    })?;
    Ok(raw)
}

/// One way of decoding for a strip.
#[derive(Debug, Clone, PartialEq)]
struct Attempt<'a> {
    path: &'a Path,
    hwaccel: Option<String>,
    keyframes_only: bool,
}

/// The decodes to try, in order, for one strip.
///
/// * The `proxy`, when there is one (and it is not the original itself), in
///   software and every frame: it is all-intra, so a hardware decoder has nothing
///   to win and keyframe sampling is moot.
/// * The original **by keyframes** when `allow_keyframes` and the sampling
///   interval is at least [`KEYFRAME_MIN_INTERVAL`]: decoding only the keyframes of
///   a long-GOP original is an order of magnitude less work (90 s of 1080p H.264:
///   12.2 s against 0.8 s), at the price of each thumbnail being the last keyframe
///   at or before its time rather than the frame itself. Software, because only a
///   keyframe in 60 is decoded and a hardware decoder's start-up would be most of
///   the cost.
/// * The original with `hwaccel` if the machine has one, then in software.
///
/// A still has no use for any of that: the original, plainly.
fn decode_attempts<'a>(
    original: &'a Path,
    proxy: Option<&'a Path>,
    still: bool,
    interval: f64,
    hwaccel: Option<String>,
    allow_keyframes: bool,
) -> Vec<Attempt<'a>> {
    let attempt = |path, hwaccel, keyframes_only| Attempt {
        path,
        hwaccel,
        keyframes_only,
    };
    if still {
        return vec![attempt(original, None, false)];
    }
    let mut attempts = Vec::new();
    if let Some(proxy) = proxy.filter(|p| *p != original) {
        attempts.push(attempt(proxy, None, false));
    }
    if allow_keyframes && interval >= KEYFRAME_MIN_INTERVAL {
        attempts.push(attempt(original, None, true));
    }
    attempts.push(attempt(original, hwaccel.clone(), false));
    if hwaccel.is_some() {
        attempts.push(attempt(original, None, false));
    }
    attempts
}

/// Raw thumbnails of `asset`: from `proxy` when given, falling back to the
/// original if that fails, with hardware decode on the original falling back to
/// software ([`decode_attempts`]). The error is the last attempt's.
fn decode_thumbs(asset: &Asset, plan: &Plan, proxy: Option<&Path>, allow_keyframes: bool) -> Result<Decoded> {
    let attempts = decode_attempts(
        Path::new(&asset.path),
        proxy,
        plan.still,
        plan.interval,
        decode_hwaccel(),
        allow_keyframes,
    );

    let mut last_err = None;
    let mut hw_failed = false;
    for attempt in attempts {
        match decode_once(attempt.path, plan, attempt.hwaccel.as_deref(), attempt.keyframes_only) {
            Ok(raw) => {
                if hw_failed && attempt.hwaccel.is_none() {
                    // The accelerated decode of this very file failed and the
                    // software one did not: `-hwaccel` is the culprit here.
                    disable_hwaccel();
                    tracing::warn!("hardware decode failed for a filmstrip; using software decode from now on");
                }
                return Ok(Decoded {
                    raw,
                    source: attempt.path.to_path_buf(),
                    keyframes_only: attempt.keyframes_only,
                });
            }
            Err(e) => {
                tracing::debug!(path = %attempt.path.display(), error = %e, "filmstrip decode attempt failed");
                hw_failed = hw_failed || attempt.hwaccel.is_some();
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| Error::Engine("no way to decode the asset".to_string())))
}

// ---- did the decode get to the end? ---------------------------------------

/// `ffprobe`'s `duration` line for a video stream → seconds, or `None` for `N/A`
/// (a Matroska stream often has none), nonsense or a non-positive value.
fn parse_duration(ffprobe_stdout: &str) -> Option<f64> {
    let secs: f64 = ffprobe_stdout.lines().next()?.trim().parse().ok()?;
    (secs.is_finite() && secs > 0.0).then_some(secs)
}

/// The first video stream's own duration, in seconds — which can be shorter than
/// the container's (audio outlasting the picture). `None` when ffprobe cannot say.
fn probe_video_duration(path: &Path) -> Option<f64> {
    use std::io::Read;
    let bin = ffprobe_bin();
    let mut child = command(&bin)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=duration"])
        .args(["-of", "default=nw=1:nk=1"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // One line of output, far inside a pipe's buffer: read it after the exit.
    let deadline = Instant::now() + PROBE_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_duration(&out)
}

/// How many thumbnails a decode of this asset should have produced, judged from
/// how long the *video* runs — its own stream duration when known, else the
/// container's — and never more than the plan asked for.
fn predicted_frames(plan: &Plan, video_secs: Option<f64>, container_secs: f64) -> u32 {
    let usable = |s: f64| s.is_finite() && s > 0.0;
    let secs = match video_secs {
        Some(v) if usable(v) && usable(container_secs) => v.min(container_secs),
        Some(v) if usable(v) => v,
        _ => container_secs,
    };
    frames_for(secs, plan.interval).min(plan.frames)
}

/// Whether `got` thumbnails are too few for `predicted`: more than
/// [`SHORTFALL_TOLERANCE`] short.
fn is_short(got: u32, predicted: u32) -> bool {
    got.saturating_add(SHORTFALL_TOLERANCE) < predicted
}

/// A decode, and whether it can be trusted as the whole video.
struct Checked {
    raw: Vec<u8>,
    frames: u32,
    /// False when the decode fell short of what the video's length predicts.
    complete: bool,
}

/// Run `decode` (its argument: whether keyframe sampling is allowed) and judge
/// the result.
///
/// A decode that comes up short and still exits 0 — a file that goes bad halfway,
/// a decoder that gives up — is otherwise indistinguishable from a video that
/// really is that short, and caching it would pin the truncated strip for good. So
/// when fewer thumbnails arrive than were planned, the *video's own* duration
/// (`video_secs` of the decoded file) says how many to expect: more than
/// [`SHORTFALL_TOLERANCE`] short is `complete: false`, and the caller keeps the
/// strip for the session but never writes it to disk. A shortfall explained by the
/// video being shorter than its container is complete. When *keyframe sampling*
/// is what fell short (sparse or mis-flagged keyframes), it is retried
/// with every frame decoded before anything is judged.
fn decode_checked(
    plan: &Plan,
    container_secs: f64,
    what: &str,
    decode: &mut dyn FnMut(bool) -> Result<Decoded>,
    video_secs: &dyn Fn(&Path) -> Option<f64>,
) -> Result<Checked> {
    let count = |raw: &[u8]| (raw.len() / plan.frame_bytes()).min(plan.frames as usize) as u32;
    let mut decoded = decode(true)?;
    let mut frames = count(&decoded.raw);
    let mut complete = true;
    if frames < plan.frames {
        let predicted = predicted_frames(plan, video_secs(&decoded.source), container_secs);
        if decoded.keyframes_only && is_short(frames, predicted) {
            tracing::debug!(
                path = what,
                frames,
                predicted,
                "keyframe sampling fell short; decoding every frame"
            );
            decoded = decode(false)?;
            frames = count(&decoded.raw);
        }
        complete = !is_short(frames, predicted);
        if !complete {
            tracing::warn!(path = what, frames, predicted, "filmstrip decode ended early; not caching it");
        }
    }
    if frames == 0 {
        return Err(Error::Engine(format!(
            "no frames could be read from {what} for its filmstrip"
        )));
    }
    Ok(Checked {
        raw: decoded.raw,
        frames,
        complete,
    })
}

// ---- encoding: stage two ---------------------------------------------------

/// Tile `raw` (concatenated `yuv420p` thumbnails) `columns` to a row and write
/// the sheets into `dir` as JPEGs.
fn encode_sheets(raw: &[u8], plan: &Plan, columns: u32, dir: &Path) -> Result<()> {
    let bin = ffmpeg_bin();
    let mut args = build_sheet_args(plan, columns);
    // A few hundred tiny frames: one thread, niced, at every budget (see
    // `decode_threads`).
    cpu::cap_args(&mut args, 1);
    let mut cmd = command(&bin);
    cpu::background_always(&mut cmd);
    let mut child = cmd
        .args(&args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let mut stdin = child.stdin.take().expect("stdin piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let (outcome, stderr_text) = std::thread::scope(|scope| {
        // The thumbnails go in on a thread of their own so a wedged ffmpeg that
        // stops reading cannot also wedge this one; the kill below ends the write.
        scope.spawn(move || {
            let _ = stdin.write_all(raw);
        });
        let tail = scope.spawn(move || drain_tail(stderr));
        let deadline = Instant::now() + SHEET_ENCODE_LIMIT;
        let outcome = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("timed out".to_string());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!("wait failed: {e}"));
                }
            }
        };
        (outcome, tail.join().unwrap_or_default())
    });

    let tail = {
        let mut lines: Vec<&str> = stderr_text.lines().rev().take(12).collect();
        lines.reverse();
        lines.join("\n").trim().to_string()
    };
    match outcome {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(Error::Engine(format!("could not encode filmstrip sheets: {tail}"))),
        Err(why) => Err(Error::Engine(
            format!("could not encode filmstrip sheets: {why}\n{tail}").trim().to_string(),
        )),
    }
}

/// Read back the sheets [`encode_sheets`] wrote into `dir`, checking each is a
/// JPEG of the size the layout says.
fn read_sheets(dir: &Path, plan: &Plan, frames: u32, columns: u32, sheet_count: u32) -> Result<Vec<FilmstripSheet>> {
    let mut sheets = Vec::with_capacity(sheet_count as usize);
    for i in 0..sheet_count {
        let path = dir.join(sheet_name(i));
        let jpeg = std::fs::read(&path).map_err(|e| {
            Error::Engine(format!(
                "filmstrip sheet {i} was not written ({e}); expected {sheet_count} sheets"
            ))
        })?;
        let want = (columns * plan.frame_width, plan.frame_height);
        let got = jpeg_dimensions(&jpeg);
        if got != Some(want) {
            return Err(Error::Engine(format!("filmstrip sheet {i} is {got:?}, expected {want:?}")));
        }
        let first = i * columns;
        sheets.push(FilmstripSheet {
            first_frame: first,
            count: columns.min(frames - first),
            width: want.0,
            height: want.1,
            jpeg: jpeg.into(),
        });
    }
    // More sheets than the layout calls for would mean the decode and the tile
    // disagree about the frame count.
    if dir.join(sheet_name(sheet_count)).exists() {
        return Err(Error::Engine(format!(
            "filmstrip encode wrote more than the {sheet_count} sheets expected"
        )));
    }
    Ok(sheets)
}

// ---- building a strip ------------------------------------------------------

/// Encode a checked decode as sheets inside `work`: the strip, and whether it is
/// complete (see [`decode_checked`]).
fn assemble(
    plan: &Plan,
    container_secs: f64,
    what: &str,
    decode: &mut dyn FnMut(bool) -> Result<Decoded>,
    video_secs: &dyn Fn(&Path) -> Option<f64>,
    work: &Path,
) -> Result<(Filmstrip, bool)> {
    let checked = decode_checked(plan, container_secs, what, decode, video_secs)?;
    let (columns, sheet_count) = sheet_layout(checked.frames, plan.frame_width);
    std::fs::create_dir_all(work)?;
    encode_sheets(
        &checked.raw[..checked.frames as usize * plan.frame_bytes()],
        plan,
        columns,
        work,
    )?;
    let frames = checked.frames;
    drop(checked.raw);
    let strip = Filmstrip {
        interval: plan.interval,
        frame_width: plan.frame_width,
        frame_height: plan.frame_height,
        frames,
        columns,
        sheets: read_sheets(work, plan, frames, columns, sheet_count)?,
    };
    Ok((strip, checked.complete))
}

/// `asset`'s strip from the cache directory `root` when a valid one is there,
/// otherwise built by `build_into` (given a fresh working directory; returns the
/// strip and whether it is complete) and, if there is a root and the strip is
/// complete, published into it. An incomplete strip is returned but never written
/// to disk. With no root the strip is built in the system temp directory and only
/// returned.
fn load_or_build_with(
    root: Option<&Path>,
    key: &str,
    plan: &Plan,
    what: &str,
    build_into: impl FnOnce(&Path) -> Result<(Filmstrip, bool)>,
) -> Result<Filmstrip> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    if let Some(hit) = root.and_then(|r| load_entry(&r.join(key), plan)) {
        return Ok(hit);
    }
    // A cache directory that cannot be written (read-only profile, full disk) is
    // not a reason to show no thumbnails: build in the temp directory and just
    // do not keep the result.
    let root = root.filter(|r| match std::fs::create_dir_all(r) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(path = %r.display(), error = %e, "filmstrip cache directory is not writable");
            false
        }
    });
    let base = root.map_or_else(|| std::env::temp_dir().join("kerf-filmstrips"), Path::to_path_buf);
    sweep_stale(&base, STALE_PART);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let work = base.join(format!("{key}.{}.{seq}.part", std::process::id()));

    let started = Instant::now();
    let result = match (build_into(&work), root) {
        (Ok((strip, true)), Some(root)) => {
            if let Err(e) = publish(&work, &root.join(key), &strip) {
                tracing::warn!(path = %root.display(), error = %e, "could not cache a filmstrip");
            }
            Ok(strip)
        }
        (built, _) => built.map(|(strip, _)| strip),
    };
    // Whatever is left of the working directory (a failed build, a failed
    // publish, an incomplete strip, or the no-cache case); after a successful
    // rename it is gone.
    let _ = std::fs::remove_dir_all(&work);
    if let Ok(strip) = &result {
        tracing::debug!(
            path = what,
            frames = strip.frames,
            sheets = strip.sheets.len(),
            took_ms = started.elapsed().as_millis() as u64,
            "built filmstrip"
        );
    }
    result
}

/// [`load_or_build_with`] decoding `asset` for real. `proxy` is asked for only
/// once the cache has missed — resolving it can run an ffprobe, which a cache hit
/// must not pay for.
fn load_or_build(
    root: Option<&Path>,
    key: &str,
    asset: &Asset,
    plan: &Plan,
    proxy: impl FnOnce() -> Option<PathBuf>,
) -> Result<Filmstrip> {
    load_or_build_with(root, key, plan, &asset.path, |work| {
        let proxy = proxy();
        assemble(
            plan,
            asset.duration,
            &asset.path,
            &mut |keyframes| decode_thumbs(asset, plan, proxy.as_deref(), keyframes),
            &probe_video_duration,
            work,
        )
    })
}

// ---- the in-process memo ---------------------------------------------------

/// A byte-bounded LRU of loaded strips: the timeline asks for the same asset's
/// strip once per clip and on every project change, and re-reading and
/// re-validating a few megabytes of JPEG each time would cost more than it saves.
struct StripMemo {
    map: HashMap<String, (u64, Arc<Filmstrip>)>,
    tick: u64,
    bytes: usize,
    cap: usize,
}

impl StripMemo {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            bytes: 0,
            cap,
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<Filmstrip>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.0 = tick;
        Some(Arc::clone(&entry.1))
    }

    /// Insert, evicting least-recently-used strips until the budget holds. One
    /// strip larger than the whole budget is still kept (alone).
    fn put(&mut self, key: String, strip: Arc<Filmstrip>) {
        self.tick += 1;
        if let Some((_, old)) = self.map.remove(&key) {
            self.bytes -= old.approx_bytes();
        }
        let size = strip.approx_bytes();
        while !self.map.is_empty() && self.bytes + size > self.cap {
            let oldest = self.map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone());
            if let Some((_, evicted)) = oldest.and_then(|k| self.map.remove(&k)) {
                self.bytes -= evicted.approx_bytes();
            }
        }
        self.bytes += size;
        self.map.insert(key, (self.tick, strip));
    }
}

fn memo() -> &'static Mutex<StripMemo> {
    static MEMO: OnceLock<Mutex<StripMemo>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(StripMemo::new(MEMO_MAX_BYTES)))
}

fn memo_get(key: &str) -> Option<Arc<Filmstrip>> {
    memo().lock().unwrap_or_else(|e| e.into_inner()).get(key)
}

/// [`filmstrip_for`] against an explicit cache root (`None`: memory only).
fn shared_strip(root: Option<&Path>, asset: &Asset, proxy: impl FnOnce() -> Option<PathBuf>) -> Result<Arc<Filmstrip>> {
    static FLIGHTS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let flights = FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()));

    let plan = Plan::for_asset(asset)?;
    let key = entry_key(asset, &plan);
    if let Some(hit) = memo_get(&key) {
        return Ok(hit);
    }
    let flight = Arc::clone(
        flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.clone())
            .or_default(),
    );
    let result = {
        let _turn = flight.lock().unwrap_or_else(|e| e.into_inner());
        // Whoever held the turn before us has probably just built it.
        match memo_get(&key) {
            Some(hit) => Ok(hit),
            None => load_or_build(root, &key, asset, &plan, proxy).map(|strip| {
                let strip = Arc::new(strip);
                memo()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(key.clone(), Arc::clone(&strip));
                strip
            }),
        }
    };
    // Forget the flight once nobody else is queued on it (the map's reference
    // and ours are the two that remain).
    let mut map = flights.lock().unwrap_or_else(|e| e.into_inner());
    if Arc::strong_count(&flight) <= 2 {
        map.remove(&key);
    }
    result
}

/// The filmstrip of `asset`: from the memo or the disk cache when a valid one
/// exists, else one ffmpeg decode of the asset's ready proxy or of the original,
/// which is then cached. `proxy` resolves the asset's ready proxy (`None` when it
/// has none); it is called only on a cache miss, because resolving it may run an
/// ffprobe.
///
/// Blocking and heavy on a miss — run it off the project lock, and resolve nothing
/// that touches the media under it. Concurrent calls for the same asset share one
/// decode; a hit is cheap. An asset with no video stream is an `InvalidArgument`.
/// A strip whose decode ended early (see [`decode_checked`]) is returned and
/// memoized but not written to the disk cache.
pub fn filmstrip_for(asset: &Asset, proxy: impl FnOnce() -> Option<PathBuf>) -> Result<Arc<Filmstrip>> {
    shared_strip(cache_root().as_deref(), asset, proxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::StatusBounded;
    use crate::model::StreamInfo;
    use chrono::Utc;
    use uuid::Uuid;

    // ---- assets -------------------------------------------------------------

    fn video(width: Option<u32>, height: Option<u32>, image: bool) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: if image { "png".into() } else { "h264".into() },
            width,
            height,
            fps: Some(25.0),
            sample_rate: None,
            channels: None,
            image,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            color_space: None,
            pix_fmt: None,
        }
    }

    fn audio() -> StreamInfo {
        StreamInfo {
            kind: StreamKind::Audio,
            codec: "aac".into(),
            width: None,
            height: None,
            fps: None,
            sample_rate: Some(48_000),
            channels: Some(2),
            ..video(None, None, false)
        }
    }

    fn asset(path: &str, duration: f64, streams: Vec<StreamInfo>) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            path: path.into(),
            name: "x".into(),
            duration,
            streams,
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
        }
    }

    // ---- interval and geometry ----------------------------------------------

    #[test]
    fn the_interval_is_the_finest_rung_that_keeps_the_strip_short() {
        // Short clips get the finest sampling.
        assert_eq!(pick_interval(10.0), 0.5);
        assert_eq!(pick_interval(150.0), 0.5, "exactly 300 frames at 0.5 s is allowed");
        // One frame past the cap steps to the next rung.
        assert_eq!(pick_interval(150.5), 1.0);
        assert_eq!(pick_interval(300.0), 1.0);
        assert_eq!(pick_interval(600.0), 2.0);
        assert_eq!(pick_interval(3600.0), 15.0);
        assert_eq!(pick_interval(7200.0), 30.0);
        assert_eq!(pick_interval(86_400.0), 300.0);
        // Degenerate durations are one second — and one frame.
        for d in [0.0, -3.0, f64::NAN, f64::INFINITY] {
            assert_eq!(pick_interval(d), 1.0, "{d}");
            assert_eq!(frames_for(d, 1.0), 1, "{d}");
        }
    }

    #[test]
    fn the_strip_never_exceeds_the_frame_cap() {
        // Swept across every scale from a blink to a year-long recording.
        let mut d = 0.01;
        while d < 3.2e7 {
            let interval = pick_interval(d);
            let frames = frames_for(d, interval);
            assert!(
                (1..=MAX_FILMSTRIP_FRAMES).contains(&frames),
                "{d}s -> {frames} frames at {interval}s"
            );
            // ...and it is the *finest* such rung: the next one down would not fit.
            if let Some(finer) = INTERVAL_LADDER.iter().copied().rev().find(|r| *r < interval) {
                assert!(frames_for(d, finer) > MAX_FILMSTRIP_FRAMES, "{d}s could have used {finer}s");
            }
            d *= 1.37;
        }
        // Past the ladder's top rung, whole seconds still hold the cap.
        let d = 3600.0 * f64::from(MAX_FILMSTRIP_FRAMES) * 7.5;
        assert!(frames_for(d, pick_interval(d)) <= MAX_FILMSTRIP_FRAMES);
    }

    #[test]
    fn an_exact_multiple_of_the_interval_is_not_one_frame_too_many() {
        assert_eq!(frames_for(10.0, 0.5), 20);
        assert_eq!(frames_for(10.0, 2.0), 5);
        assert_eq!(frames_for(10.1, 2.0), 6);
        assert_eq!(frames_for(0.04, 0.5), 1);
        // Float noise on the boundary must not add a frame.
        assert_eq!(frames_for(10.000_000_01, 0.5), 20);
    }

    #[test]
    fn the_thumbnail_follows_the_displayed_aspect_in_even_pixels() {
        assert_eq!(thumb_width(Some(1920), Some(1080)), 170);
        assert_eq!(thumb_width(Some(1280), Some(720)), 170);
        // A portrait phone clip (displayed size, rotation already applied).
        assert_eq!(thumb_width(Some(1080), Some(1920)), 54);
        assert_eq!(thumb_width(Some(1080), Some(1080)), 96);
        // A 360 equirect frame.
        assert_eq!(thumb_width(Some(5760), Some(2880)), 192);
        // Unknown picture: 16:9.
        assert_eq!(thumb_width(None, None), 170);
        assert_eq!(thumb_width(Some(0), Some(0)), 170);
        // Extremes are squeezed into range rather than refused.
        assert_eq!(thumb_width(Some(100_000), Some(10)), MAX_FRAME_WIDTH);
        assert_eq!(thumb_width(Some(1), Some(100_000)), 2);
        for (w, h) in [(1920, 1080), (1000, 999), (7, 5), (4096, 1716), (720, 1280), (1, 1)] {
            let width = thumb_width(Some(w), Some(h));
            assert_eq!(width % 2, 0, "{w}x{h} -> {width}");
        }
    }

    #[test]
    fn sheets_are_as_few_and_as_even_as_the_width_cap_allows() {
        // 170 px thumbnails: 48 fit in 8192.
        assert_eq!(sheet_layout(20, 170), (20, 1));
        assert_eq!(sheet_layout(48, 170), (48, 1));
        // 49 would leave one lonely thumbnail on a second sheet; it is split evenly.
        assert_eq!(sheet_layout(49, 170), (25, 2));
        assert_eq!(sheet_layout(300, 170), (43, 7));
        assert_eq!(sheet_layout(1, 170), (1, 1));
        assert_eq!(sheet_layout(0, 170), (1, 1), "an empty strip is clamped, never zero columns");
        // Portrait: 54 px, so 151 per sheet.
        assert_eq!(sheet_layout(300, 54), (150, 2));
    }

    #[test]
    fn sheet_layouts_always_cover_the_frames_within_the_width_cap() {
        for width in [2, 54, 96, 170, 192, 700, 1024, 4000, 8192] {
            for frames in 1..=MAX_FILMSTRIP_FRAMES {
                let (columns, sheets) = sheet_layout(frames, width);
                assert!(columns >= 1 && sheets >= 1, "{frames} x {width}");
                assert!(
                    columns * width <= MAX_SHEET_WIDTH.max(width),
                    "{frames} x {width}: {columns} columns"
                );
                // Every sheet holds something, and together they hold exactly `frames`.
                let last = frames - (sheets - 1) * columns;
                assert!(
                    (1..=columns).contains(&last),
                    "{frames} x {width}: last sheet {last} of {columns}"
                );
                // As few sheets as the cap allows.
                let widest = (MAX_SHEET_WIDTH / width).max(1);
                assert_eq!(sheets, frames.div_ceil(widest), "{frames} x {width}");
            }
        }
    }

    #[test]
    fn the_rate_is_an_exact_reduced_rational() {
        assert_eq!(fps_rate(0.5), "2/1");
        assert_eq!(fps_rate(1.0), "1/1");
        assert_eq!(fps_rate(1.5), "2/3");
        assert_eq!(fps_rate(2.0), "1/2");
        assert_eq!(fps_rate(15.0), "1/15");
        assert_eq!(fps_rate(3600.0), "1/3600");
        // Every rung of the ladder parses back to exactly its interval.
        for rung in INTERVAL_LADDER {
            let rate = fps_rate(rung);
            let (n, d) = rate.split_once('/').unwrap();
            let (n, d): (f64, f64) = (n.parse().unwrap(), d.parse().unwrap());
            assert_eq!(d / n, rung, "{rate}");
        }
    }

    #[test]
    fn the_decode_is_declared_stalled_in_proportion_to_the_sampling() {
        assert_eq!(stall_for(0.5), STALL_FLOOR, "a fine strip gets the floor");
        assert_eq!(stall_for(10.0), STALL_FLOOR);
        assert_eq!(
            stall_for(30.0),
            Duration::from_secs(180),
            "six times the media a thumbnail spans"
        );
        assert_eq!(stall_for(3600.0), STALL_CEILING, "and never longer than the ceiling");
        // Nonsense never panics and lands inside the bounds.
        assert_eq!(stall_for(f64::INFINITY), STALL_CEILING);
        assert_eq!(stall_for(f64::NAN), STALL_FLOOR);
        assert_eq!(stall_for(-4.0), STALL_FLOOR);
    }

    // ---- the plan -----------------------------------------------------------

    #[test]
    fn the_plan_reads_the_asset() {
        let plan = Plan::for_asset(&asset("/a.mp4", 60.0, vec![video(Some(1920), Some(1080), false), audio()])).unwrap();
        assert_eq!(
            plan,
            Plan {
                interval: 0.5,
                frames: 120,
                frame_width: 170,
                frame_height: FILMSTRIP_HEIGHT,
                still: false,
            }
        );
        assert_eq!(plan.frame_bytes(), 170 * 96 * 3 / 2);

        // A long asset is coarser, and capped.
        let long = Plan::for_asset(&asset("/long.mp4", 7200.0, vec![video(Some(1080), Some(1920), false)])).unwrap();
        assert_eq!((long.interval, long.frames, long.frame_width), (30.0, 240, 54));

        // A still is one thumbnail with no sampling.
        let still = Plan::for_asset(&asset("/s.png", 5.0, vec![video(Some(800), Some(600), true)])).unwrap();
        assert_eq!((still.frames, still.still, still.frame_width), (1, true, 128));
        assert_eq!(still.interval, 5.0, "the one thumbnail stands for the whole asset");
        let tiny_still = Plan::for_asset(&asset("/s.png", 0.04, vec![video(Some(800), Some(600), true)])).unwrap();
        assert_eq!(tiny_still.interval, 1.0, "never a zero or sub-second interval to divide by");

        // No stream info: tried, as 16:9.
        let blind = Plan::for_asset(&asset("/b.mp4", 10.0, vec![])).unwrap();
        assert_eq!((blind.frame_width, blind.frames), (170, 20));
    }

    #[test]
    fn an_asset_without_video_has_no_filmstrip() {
        let err = Plan::for_asset(&asset("/voice.wav", 10.0, vec![audio()])).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
        assert!(err.to_string().contains("no video stream"), "{err}");
        // And the public entry says so before any ffmpeg is spawned (the file
        // does not exist).
        let missing = asset("/nowhere/voice.wav", 10.0, vec![audio()]);
        assert!(matches!(
            filmstrip_for(&missing, || panic!(
                "no proxy is looked up for an asset that cannot have a strip"
            )),
            Err(Error::InvalidArgument(_))
        ));
    }

    // ---- ffmpeg arguments ---------------------------------------------------

    fn plan_for(interval: f64, frames: u32, still: bool) -> Plan {
        Plan {
            interval,
            frames,
            frame_width: 170,
            frame_height: 96,
            still,
        }
    }

    fn arg_after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let at = args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("no {flag}: {args:?}"));
        &args[at + 1]
    }

    #[test]
    fn the_decode_samples_before_it_scales_and_rounds_up_from_zero() {
        let args = build_thumb_args("/media/clip.mov", &plan_for(2.0, 5, false), None, None, false);
        assert_eq!(arg_after(&args, "-i"), "/media/clip.mov");
        assert_eq!(
            arg_after(&args, "-vf"),
            "fps=fps=1/2:start_time=0:round=up,scale=170:96:flags=area,format=yuv420p"
        );
        assert_eq!(arg_after(&args, "-frames:v"), "5", "never more thumbnails than planned");
        assert_eq!(arg_after(&args, "-pix_fmt"), "yuv420p");
        assert_eq!(arg_after(&args, "-f"), "rawvideo");
        assert_eq!(
            args.last().unwrap(),
            "pipe:1",
            "the sink is last (thread caps are spliced in before it)"
        );
        // Pure: no thread flags and no accelerator unless asked for.
        assert!(!args.iter().any(|a| a.contains("threads") || a == "-hwaccel"), "{args:?}");
        // The filter value has no unquoted comma problem: it is one -vf argument.
        assert_eq!(args.iter().filter(|a| a.starts_with("fps=")).count(), 1);
    }

    #[test]
    fn the_proxy_is_tried_first_hardware_only_on_the_original_and_software_last() {
        let (original, proxy) = (Path::new("/m/clip.mov"), Path::new("/cache/proxy.mp4"));
        let auto = || Some("auto".to_string());
        let at = |path, hwaccel, keyframes_only| Attempt {
            path,
            hwaccel,
            keyframes_only,
        };
        // A fine sampling (under 5 s) is every frame. Everything available: proxy
        // (software), original with the accelerator, original in software.
        assert_eq!(
            decode_attempts(original, Some(proxy), false, 0.5, auto(), true),
            [at(proxy, None, false), at(original, auto(), false), at(original, None, false)]
        );
        // No proxy yet: the original is decoded rather than waited for.
        assert_eq!(
            decode_attempts(original, None, false, 2.0, auto(), true),
            [at(original, auto(), false), at(original, None, false)]
        );
        // No accelerator (or it was found broken): no pointless second attempt.
        assert_eq!(
            decode_attempts(original, None, false, 0.5, None, true),
            [at(original, None, false)]
        );
        assert_eq!(
            decode_attempts(original, Some(proxy), false, 0.5, None, true),
            [at(proxy, None, false), at(original, None, false)]
        );
        // A "proxy" that is the original is not tried twice.
        assert_eq!(
            decode_attempts(original, Some(original), false, 0.5, None, true),
            [at(original, None, false)]
        );
        // A still ignores all of it.
        assert_eq!(
            decode_attempts(original, Some(proxy), true, 5.0, auto(), true),
            [at(original, None, false)]
        );
    }

    #[test]
    fn a_coarse_original_is_sampled_by_keyframes_and_a_proxy_never_is() {
        let (original, proxy) = (Path::new("/m/clip.mov"), Path::new("/cache/proxy.mp4"));
        let auto = || Some("auto".to_string());
        let at = |path, hwaccel, keyframes_only| Attempt {
            path,
            hwaccel,
            keyframes_only,
        };
        // From 5 s up the original is decoded by keyframes, in software, and the
        // every-frame decodes stay behind it as the fallback.
        assert_eq!(
            decode_attempts(original, None, false, 5.0, auto(), true),
            [
                at(original, None, true),
                at(original, auto(), false),
                at(original, None, false)
            ]
        );
        assert_eq!(
            decode_attempts(original, None, false, 15.0, None, true),
            [at(original, None, true), at(original, None, false)]
        );
        // Just under the threshold it is not.
        assert!(decode_attempts(original, None, false, 2.0, None, true)
            .iter()
            .all(|a| !a.keyframes_only));
        // The proxy comes first, every frame — all-intra, so a keyframe-only decode
        // is moot there.
        assert_eq!(
            decode_attempts(original, Some(proxy), false, 10.0, None, true)[0],
            at(proxy, None, false)
        );
        // And the caller can forbid it (a retry after keyframe sampling fell short).
        assert!(decode_attempts(original, None, false, 30.0, auto(), false)
            .iter()
            .all(|a| !a.keyframes_only));
    }

    #[test]
    fn the_keyframe_flag_goes_before_the_input_and_never_on_a_still() {
        let args = build_thumb_args("/m/long.mov", &plan_for(10.0, 120, false), None, None, true);
        let skip = args.iter().position(|a| a == "-skip_frame").expect("-skip_frame");
        assert_eq!(args[skip + 1], "nokey");
        assert!(skip < args.iter().position(|a| a == "-i").unwrap(), "a decoder option");
        assert!(
            !build_thumb_args("/m/long.mov", &plan_for(10.0, 120, false), None, None, false).contains(&"-skip_frame".to_string())
        );
        // A still has one frame: nothing to skip.
        assert!(!build_thumb_args("a.png", &plan_for(5.0, 1, true), None, None, true).contains(&"-skip_frame".to_string()));
    }

    #[test]
    fn a_decode_is_capped_below_the_machine_whatever_the_budget() {
        // A quarter of the cores, one to two threads.
        assert_eq!(decode_threads(1, 1), 1);
        assert_eq!(decode_threads(2, 2), 1);
        assert_eq!(decode_threads(4, 4), 1);
        assert_eq!(decode_threads(8, 8), 2);
        assert_eq!(decode_threads(16, 16), 2);
        assert_eq!(decode_threads(64, 64), 2, "never above the ceiling");
        // And never above a budget that is itself smaller.
        assert_eq!(decode_threads(16, 1), 1);
        assert_eq!(decode_threads(16, 0), 1, "a zero budget still gets a thread");
        // Always strictly below a machine with room to spare.
        for cores in 2..=64 {
            assert!(decode_threads(cores, cores) < cores, "{cores} cores");
        }
    }

    #[test]
    fn the_accelerator_goes_before_the_input() {
        let args = build_thumb_args("a.mp4", &plan_for(0.5, 20, false), None, Some("auto"), false);
        let hw = args.iter().position(|a| a == "-hwaccel").expect("-hwaccel");
        assert_eq!(args[hw + 1], "auto");
        assert!(hw < args.iter().position(|a| a == "-i").unwrap(), "an input option");
    }

    #[test]
    fn hdr_is_tone_mapped_after_the_downscale_and_a_still_is_not_sampled() {
        let tonemap = "zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=yuv420p";
        let vf = thumb_filter(&plan_for(1.0, 10, false), Some(tonemap));
        let scale = vf.find("scale=170:96").expect("scale");
        let zscale = vf.find("zscale=tin").expect("tonemap");
        assert!(vf.starts_with("fps=") && scale < zscale, "{vf}");
        assert!(vf.ends_with("format=yuv420p"));
        assert!(
            !vf.replace(tonemap, "").contains("format=yuv420p,format"),
            "no second format when the tone-map already ends in one: {vf}"
        );

        let still = build_thumb_args("a.png", &plan_for(5.0, 1, true), None, Some("auto"), true);
        assert_eq!(arg_after(&still, "-vf"), "scale=170:96:flags=area,format=yuv420p");
        assert_eq!(arg_after(&still, "-frames:v"), "1");
        assert!(!still.contains(&"-hwaccel".to_string()), "nothing to accelerate in an image");
    }

    #[test]
    fn the_sheet_encode_reads_raw_frames_and_writes_numbered_jpegs_beside_itself() {
        let args = build_sheet_args(&plan_for(0.5, 20, false), 20);
        assert_eq!(arg_after(&args, "-video_size"), "170x96");
        assert_eq!(arg_after(&args, "-pixel_format"), "yuv420p");
        assert_eq!(arg_after(&args, "-i"), "pipe:0");
        assert_eq!(arg_after(&args, "-vf"), "tile=20x1");
        assert_eq!(arg_after(&args, "-c:v"), "mjpeg");
        // A *relative* pattern: the process runs in the sheets' directory.
        assert_eq!(args.last().unwrap(), "sheet-%03d.jpg");
        assert_eq!(sheet_name(7), "sheet-007.jpg");
    }

    // ---- reading JPEG headers -----------------------------------------------

    /// The smallest byte string `jpeg_dimensions` and the cache validator accept:
    /// SOI, an APP0, a one-component SOF0, an SOS and EOI.
    fn fake_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        v.extend([0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        v.extend(h.to_be_bytes());
        v.extend(w.to_be_bytes());
        v.extend([0x01, 0x01, 0x11, 0x00]);
        v.extend([0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        v
    }

    #[test]
    fn a_jpegs_size_is_read_from_its_frame_header() {
        assert_eq!(jpeg_dimensions(&fake_jpeg(3400, 96)), Some((3400, 96)));
        assert_eq!(jpeg_dimensions(&fake_jpeg(8192, 96)), Some((8192, 96)));
        // Fill bytes before a marker are allowed.
        let mut filled = fake_jpeg(170, 96);
        filled.splice(2..2, [0xFF, 0xFF]);
        assert_eq!(jpeg_dimensions(&filled), Some((170, 96)));
        // Not a JPEG, a truncated one, one that reaches the scan with no frame
        // header, and a zero-sized frame are all `None`.
        assert_eq!(jpeg_dimensions(b""), None);
        assert_eq!(jpeg_dimensions(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(jpeg_dimensions(&fake_jpeg(170, 96)[..12]), None);
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02]), None);
        assert_eq!(jpeg_dimensions(&fake_jpeg(0, 96)), None);
        // A segment length that runs past the data does not read out of bounds.
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0x00]), None);
    }

    // ---- the cache ----------------------------------------------------------

    /// A scratch directory removed at the end of the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("kerf-filmstrip-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            Scratch(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A strip of `frames` thumbnails, as the engine lays it out, whose sheets
    /// are well-formed JPEG headers (`marker` tells their bytes apart).
    fn sample_strip(plan: &Plan, frames: u32, marker: u8) -> Filmstrip {
        let (columns, sheet_count) = sheet_layout(frames, plan.frame_width);
        let sheets = (0..sheet_count)
            .map(|i| {
                let width = columns * plan.frame_width;
                let mut jpeg = fake_jpeg(width as u16, plan.frame_height as u16);
                jpeg.splice(2..2, [0xFF, 0xFE, 0x00, 0x03, marker]); // a COM segment
                FilmstripSheet {
                    first_frame: i * columns,
                    count: columns.min(frames - i * columns),
                    width,
                    height: plan.frame_height,
                    jpeg: jpeg.into(),
                }
            })
            .collect();
        Filmstrip {
            interval: plan.interval,
            frame_width: plan.frame_width,
            frame_height: plan.frame_height,
            frames,
            columns,
            sheets,
        }
    }

    /// Write `strip` as the build does: sheets into a work directory, then
    /// [`publish`].
    fn write_entry(root: &Path, key: &str, strip: &Filmstrip) -> PathBuf {
        let work = root.join(format!("{key}.work.part"));
        std::fs::create_dir_all(&work).unwrap();
        for (i, sheet) in strip.sheets.iter().enumerate() {
            std::fs::write(work.join(sheet_name(i as u32)), &sheet.jpeg).unwrap();
        }
        let dest = root.join(key);
        publish(&work, &dest, strip).expect("publish");
        dest
    }

    #[test]
    fn a_strip_survives_the_cache() {
        let dir = Scratch::new("roundtrip");
        // 120 thumbnails of 170 px: three sheets of 40.
        let plan = plan_for(0.5, 120, false);
        let strip = sample_strip(&plan, 120, 1);
        assert_eq!((strip.columns, strip.sheets.len()), (40, 3));
        let entry = write_entry(&dir.0, "abc", &strip);

        assert_eq!(load_entry(&entry, &plan), Some(strip));
        // The work directory moved, it was not copied.
        let names: Vec<String> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["abc"], "no .part directory survives");
        let mut files: Vec<String> = std::fs::read_dir(&entry)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, ["manifest.json", "sheet-000.jpg", "sheet-001.jpg", "sheet-002.jpg"]);

        // A shorter video than planned (audio outlasting the picture) is a valid
        // entry for the plan; a longer one, or different geometry, is not.
        let short = sample_strip(&plan, 100, 2);
        let short_entry = write_entry(&dir.0, "short", &short);
        assert_eq!(load_entry(&short_entry, &plan), Some(short));
        assert_eq!(
            load_entry(&entry, &plan_for(0.5, 100, false)),
            None,
            "more frames than planned"
        );
        assert_eq!(load_entry(&entry, &plan_for(1.0, 120, false)), None, "another interval");
        let mut wider = plan.clone();
        wider.frame_width = 172;
        assert_eq!(load_entry(&entry, &wider), None, "another thumbnail size");

        // Republishing over an existing entry replaces it.
        let replacement = sample_strip(&plan, 120, 9);
        let again = write_entry(&dir.0, "abc", &replacement);
        assert_eq!(load_entry(&again, &plan), Some(replacement));
        // An entry that is not there is a miss.
        assert_eq!(load_entry(&dir.join("absent"), &plan), None);
    }

    #[test]
    fn a_damaged_entry_is_rejected_not_trusted() {
        let dir = Scratch::new("damage");
        let plan = plan_for(0.5, 120, false);
        let strip = sample_strip(&plan, 120, 1);
        let entry = write_entry(&dir.0, "good", &strip);
        let fresh = |name: &str| -> PathBuf {
            let copy = dir.join(name);
            std::fs::create_dir_all(&copy).unwrap();
            for f in std::fs::read_dir(&entry).unwrap() {
                let f = f.unwrap();
                std::fs::copy(f.path(), copy.join(f.file_name())).unwrap();
            }
            assert!(load_entry(&copy, &plan).is_some(), "the copy starts out good");
            copy
        };

        // A sheet missing, truncated, grown, or swapped for something that is not
        // a JPEG of its size.
        let gone = fresh("gone");
        std::fs::remove_file(gone.join("sheet-001.jpg")).unwrap();
        assert_eq!(load_entry(&gone, &plan), None);

        let cut = fresh("cut");
        let bytes = std::fs::read(cut.join("sheet-002.jpg")).unwrap();
        std::fs::write(cut.join("sheet-002.jpg"), &bytes[..bytes.len() - 4]).unwrap();
        assert_eq!(load_entry(&cut, &plan), None, "truncated");

        let grown = fresh("grown");
        let mut bytes = std::fs::read(grown.join("sheet-000.jpg")).unwrap();
        bytes.extend([0, 0, 0]);
        std::fs::write(grown.join("sheet-000.jpg"), &bytes).unwrap();
        assert_eq!(load_entry(&grown, &plan), None, "size differs from the manifest");

        let resized = fresh("resized");
        let mut bytes = fake_jpeg(1000, 96);
        bytes.splice(2..2, [0xFF, 0xFE, 0x00, 0x03, 1]);
        std::fs::write(resized.join("sheet-000.jpg"), &bytes).unwrap();
        assert_eq!(load_entry(&resized, &plan), None, "a JPEG of another size");

        let junk = fresh("junk");
        let len = std::fs::read(junk.join("sheet-000.jpg")).unwrap().len();
        std::fs::write(junk.join("sheet-000.jpg"), vec![7u8; len]).unwrap();
        assert_eq!(load_entry(&junk, &plan), None, "same length, not a JPEG");

        // The manifest: gone, unparsable, another version, or inconsistent.
        let no_manifest = fresh("no-manifest");
        std::fs::remove_file(no_manifest.join("manifest.json")).unwrap();
        assert_eq!(load_entry(&no_manifest, &plan), None);

        let garbled = fresh("garbled");
        std::fs::write(garbled.join("manifest.json"), b"{\"version\": 1,").unwrap();
        assert_eq!(load_entry(&garbled, &plan), None);

        let edit = |name: &str, f: &dyn Fn(&mut Manifest)| {
            let copy = fresh(name);
            let mut m: Manifest = serde_json::from_slice(&std::fs::read(copy.join("manifest.json")).unwrap()).unwrap();
            f(&mut m);
            std::fs::write(copy.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
            load_entry(&copy, &plan)
        };
        assert_eq!(edit("v2", &|m| m.version += 1), None, "another format version");
        assert_eq!(
            edit("frames", &|m| m.frames = 119),
            None,
            "frame count disagrees with the layout"
        );
        assert_eq!(edit("columns", &|m| m.columns = 41), None);
        assert_eq!(edit("order", &|m| m.sheets.swap(0, 1)), None, "sheets out of order");
        assert_eq!(edit("first", &|m| m.sheets[1].first_frame = 41), None);
        assert_eq!(edit("count", &|m| m.sheets[2].count = 41), None);
        assert_eq!(
            edit("name", &|m| m.sheets[0].file = "../sheet-000.jpg".into()),
            None,
            "no path escapes"
        );
        assert_eq!(edit("bytes", &|m| m.sheets[0].bytes = 0), None);
        assert_eq!(edit("interval", &|m| m.interval = f64::NAN), None);
        assert_eq!(edit("interval0", &|m| m.interval = 0.0), None);
        assert_eq!(edit("none", &|m| m.sheets.clear()), None);
        // An honest edit that changes nothing still loads.
        assert!(edit("same", &|_| {}).is_some());

        // A huge manifest is not even read.
        let huge = fresh("huge");
        std::fs::write(huge.join("manifest.json"), vec![b' '; (MAX_MANIFEST_BYTES + 1) as usize]).unwrap();
        assert_eq!(load_entry(&huge, &plan), None);
    }

    #[test]
    fn the_manifest_is_canonical_only_when_it_follows_from_the_frame_count() {
        let plan = plan_for(0.5, 300, false);
        let m = Manifest::of(&sample_strip(&plan, 300, 1));
        assert!(m.is_canonical());
        assert_eq!(m.sheets.len(), 7);
        assert_eq!((m.sheets[6].first_frame, m.sheets[6].count), (258, 42));
        assert!(m.fits(&plan));
        // One frame, the other extreme.
        assert!(Manifest::of(&sample_strip(&plan_for(5.0, 1, true), 1, 1)).is_canonical());
        // Out-of-range geometry is never canonical.
        for tweak in [
            (|m: &mut Manifest| m.frames = 0) as fn(&mut Manifest),
            |m| m.frames = MAX_FILMSTRIP_FRAMES + 1,
            |m| m.frame_width = 169,
            |m| m.frame_height = 95,
            |m| m.frame_width = 0,
        ] {
            let mut bad = m.clone();
            tweak(&mut bad);
            assert!(!bad.is_canonical(), "{bad:?}");
        }
    }

    #[test]
    fn the_cache_key_follows_the_asset_and_the_geometry_not_the_proxy() {
        let dir = Scratch::new("key");
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"one").unwrap();
        let path = file.to_str().unwrap();
        let a = asset(path, 10.0, vec![video(Some(1920), Some(1080), false)]);
        let plan = Plan::for_asset(&a).unwrap();
        let key = entry_key(&a, &plan);
        assert_eq!(key.len(), 16, "a 64-bit hex name");
        assert_eq!(key, entry_key(&a, &plan), "stable for an unchanged file");

        // A different file, a changed file, another duration (interval), another
        // aspect: each is another entry.
        let other = dir.join("b.mp4");
        std::fs::write(&other, b"one").unwrap();
        let b = asset(other.to_str().unwrap(), 10.0, a.streams.clone());
        assert_ne!(key, entry_key(&b, &plan));
        std::fs::write(&file, b"longer contents").unwrap();
        assert_ne!(key, entry_key(&a, &plan), "a replaced source must not reuse the old strip");
        let key = entry_key(&a, &plan);
        let longer = asset(path, 400.0, a.streams.clone());
        assert_ne!(key, entry_key(&longer, &Plan::for_asset(&longer).unwrap()));
        let portrait = asset(path, 10.0, vec![video(Some(1080), Some(1920), false)]);
        assert_ne!(key, entry_key(&portrait, &Plan::for_asset(&portrait).unwrap()));
        // A thumbnail-size or format bump would change it too (they are in the key text).
        let taller = Plan {
            frame_height: plan.frame_height + 2,
            ..plan
        };
        assert_ne!(key, entry_key(&a, &taller));

        // The proxy is not an input: the same asset has the same key whichever
        // file it is decoded from (that is the point — see the module docs).
        // (`entry_key` does not even take it.)
        if let Some(root) = cache_root() {
            assert!(root.to_string_lossy().contains("filmstrips"), "{root:?}");
        }
    }

    #[test]
    fn stale_part_directories_are_swept_and_current_ones_are_not() {
        let dir = Scratch::new("sweep");
        let part = dir.join("abc.1.0.part");
        let entry = dir.join("0123456789abcdef");
        for d in [&part, &entry] {
            std::fs::create_dir_all(d).unwrap();
        }
        // Nothing here is a day old: nothing goes.
        sweep_stale(&dir.0, STALE_PART);
        assert!(part.exists() && entry.exists());
        // With the threshold at "any age at all", only the `.part` directory goes:
        // a finished entry is never touched, however old.
        std::thread::sleep(Duration::from_millis(30));
        sweep_stale(&dir.0, Duration::from_millis(1));
        assert!(!part.exists(), "an old part directory is a crashed build's leftover");
        assert!(entry.exists());
        sweep_stale(&dir.join("nonexistent"), Duration::ZERO); // no panic
    }

    // ---- the strip's own arithmetic -----------------------------------------

    #[test]
    fn a_time_maps_to_the_nearest_thumbnail_and_back() {
        let strip = sample_strip(&plan_for(2.0, 5, false), 5, 1); // t = 0, 2, 4, 6, 8
        assert_eq!(strip.time_of(0), 0.0);
        assert_eq!(strip.time_of(3), 6.0);
        // The nearest sample in time, ties going to the later one (round half up).
        assert_eq!(strip.frame_at(0.0), 0);
        assert_eq!(strip.frame_at(0.9), 0);
        assert_eq!(strip.frame_at(1.1), 1);
        assert_eq!(strip.frame_at(5.0), 3);
        assert_eq!(strip.frame_at(7.9), 4);
        // Outside the strip, or meaningless: the ends.
        assert_eq!(strip.frame_at(-5.0), 0);
        assert_eq!(strip.frame_at(f64::NAN), 0);
        assert_eq!(strip.frame_at(1e9), 4);
        assert_eq!(strip.frame_at(f64::INFINITY), 4);
        // Round trip: every thumbnail is the nearest to its own time.
        for k in 0..strip.frames {
            assert_eq!(strip.frame_at(strip.time_of(k)), k);
        }
    }

    #[test]
    fn a_thumbnail_is_found_on_its_sheet() {
        let plan = plan_for(0.5, 120, false);
        let strip = sample_strip(&plan, 120, 1); // three sheets of 40
        let (sheet, x) = strip.locate(0).unwrap();
        assert_eq!((sheet.first_frame, x), (0, 0));
        let (sheet, x) = strip.locate(39).unwrap();
        assert_eq!((sheet.first_frame, x), (0, 39 * 170));
        let (sheet, x) = strip.locate(40).unwrap();
        assert_eq!((sheet.first_frame, x), (40, 0));
        let (sheet, x) = strip.locate(119).unwrap();
        assert_eq!((sheet.first_frame, x), (80, 39 * 170));
        assert!(strip.locate(120).is_none(), "past the end");
        // The formula the module docs give a caller agrees.
        for k in 0..strip.frames {
            let (sheet, x) = strip.locate(k).unwrap();
            assert_eq!(sheet.first_frame, k / strip.columns * strip.columns);
            assert_eq!(x, (k % strip.columns) * strip.frame_width);
        }
        assert!(strip.approx_bytes() > strip.sheets.iter().map(|s| s.jpeg.len()).sum::<usize>());
    }

    #[test]
    fn the_json_carries_the_geometry_but_not_the_pixels() {
        let strip = sample_strip(&plan_for(0.5, 20, false), 20, 1);
        let json = serde_json::to_value(&strip).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "interval": 0.5,
                "frame_width": 170,
                "frame_height": 96,
                "frames": 20,
                "columns": 20,
                "sheets": [{"first_frame": 0, "count": 20, "width": 3400, "height": 96}],
            })
        );
    }

    // ---- the memo -----------------------------------------------------------

    #[test]
    fn the_memo_evicts_least_recently_used_by_bytes() {
        let plan = plan_for(0.5, 20, false);
        let strip = || Arc::new(sample_strip(&plan, 20, 1));
        let size = strip().approx_bytes();
        let mut memo = StripMemo::new(size * 2 + size / 2); // room for two
        memo.put("a".into(), strip());
        memo.put("b".into(), strip());
        assert!(memo.get("a").is_some(), "touch a so b is the oldest");
        memo.put("c".into(), strip());
        assert!(memo.get("b").is_none(), "b was least recently used");
        assert!(memo.get("a").is_some() && memo.get("c").is_some());
        assert!(memo.bytes <= memo.cap, "{} > {}", memo.bytes, memo.cap);
        // Re-putting a key replaces it without double-counting its bytes.
        memo.put("a".into(), strip());
        memo.put("a".into(), strip());
        assert_eq!(memo.bytes, size * 2);
    }

    #[test]
    fn an_oversized_strip_is_kept_alone_rather_than_refused() {
        let plan = plan_for(0.5, 300, false);
        let big = Arc::new(sample_strip(&plan, 300, 1));
        let small = Arc::new(sample_strip(&plan_for(0.5, 1, false), 1, 1));
        let mut memo = StripMemo::new(big.approx_bytes() / 2);
        memo.put("small".into(), small);
        memo.put("big".into(), Arc::clone(&big));
        assert!(memo.get("small").is_none());
        assert!(Arc::ptr_eq(&memo.get("big").unwrap(), &big));
        assert_eq!(memo.map.len(), 1);
    }

    // ---- the pump -----------------------------------------------------------

    /// A stand-in for ffmpeg: `script` run by `sh` with piped stdout / stderr.
    /// `exec` in the script so a kill reaches the process holding the pipes.
    #[cfg(unix)]
    fn fake_decoder(script: &str) -> Child {
        super::super::cli::command("sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh")
    }

    /// Bytes as an `sh` `printf` escape string.
    #[cfg(unix)]
    fn printf_escape(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("\\{b:03o}")).collect()
    }

    #[cfg(unix)]
    #[test]
    fn the_pump_delivers_whole_thumbnails_even_when_the_pipe_splits_them() {
        // Three 6-byte "thumbnails" written as 4-byte pieces, so every one
        // straddles a read; the consumer must see aligned frames in order.
        let bytes: Vec<u8> = (1..=18).collect();
        let script: String = bytes
            .chunks(4)
            .map(|c| format!("printf '{}'; sleep 0.05; ", printf_escape(c)))
            .collect();
        let mut got = Vec::new();
        pump_raw(fake_decoder(&script), 6, Duration::from_secs(5), &mut |f| {
            assert_eq!(f.len() % 6, 0, "only whole frames reach the consumer");
            got.extend_from_slice(f);
        })
        .expect("a clean stream");
        assert_eq!(got, bytes);
    }

    #[cfg(unix)]
    #[test]
    fn a_decode_that_never_produces_a_frame_is_killed_not_awaited() {
        let started = Instant::now();
        let err = pump_raw(
            fake_decoder("echo cannot open the stream >&2; exec sleep 60"),
            6,
            Duration::from_millis(400),
            &mut |_| {},
        )
        .expect_err("a silent decode must fail")
        .to_string();
        assert!(err.contains("stalled"), "{err}");
        assert!(err.contains("cannot open the stream"), "stderr tail is surfaced: {err}");
        assert!(started.elapsed().as_secs() < 10, "killed promptly: {:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn a_decode_that_stalls_midway_fails_after_delivering_what_it_had() {
        let script = format!("printf '{}'; exec sleep 60", printf_escape(&[1, 2, 3, 4, 5, 6, 7, 8]));
        let mut got = Vec::new();
        let err = pump_raw(fake_decoder(&script), 6, Duration::from_millis(400), &mut |f| {
            got.extend_from_slice(f);
        })
        .expect_err("stall")
        .to_string();
        assert!(err.contains("stalled"), "{err}");
        assert_eq!(got, [1, 2, 3, 4, 5, 6], "the whole frame, not the stray two bytes");
    }

    #[cfg(unix)]
    #[test]
    fn a_decoder_that_fails_reports_why() {
        let err = pump_raw(
            fake_decoder("echo no such stream >&2; exit 3"),
            6,
            Duration::from_secs(5),
            &mut |_| {},
        )
        .expect_err("non-zero exit")
        .to_string();
        assert!(
            err.contains("could not decode frames") && err.contains("no such stream"),
            "{err}"
        );
    }

    // ---- did the decode get to the end? ---------------------------------------

    #[test]
    fn a_video_duration_is_read_from_ffprobes_line() {
        assert_eq!(parse_duration("10.040000\n"), Some(10.04));
        assert_eq!(parse_duration("  3600  \r\n"), Some(3600.0));
        // Only the first line.
        assert_eq!(parse_duration("2.5\n99\n"), Some(2.5));
        // A stream with no duration of its own prints N/A (Matroska often has none).
        assert_eq!(parse_duration("N/A\n"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("\n"), None);
        assert_eq!(parse_duration("0\n"), None);
        assert_eq!(parse_duration("-1.5\n"), None);
        assert_eq!(parse_duration("inf\n"), None);
        assert_eq!(parse_duration("NaN\n"), None);
    }

    #[test]
    fn the_expected_thumbnails_follow_the_videos_own_length() {
        let plan = plan_for(0.5, 20, false); // a 10 s container
                                             // The stream's own duration wins when it is shorter than the container's...
        assert_eq!(predicted_frames(&plan, Some(2.5), 10.0), 5);
        // ...but never predicts more than was planned, or than the container has.
        assert_eq!(predicted_frames(&plan, Some(60.0), 10.0), 20);
        assert_eq!(predicted_frames(&plan, Some(10.0), 10.0), 20);
        // Unknown (no stream duration, or nonsense): the container's.
        assert_eq!(predicted_frames(&plan, None, 10.0), 20);
        assert_eq!(predicted_frames(&plan, Some(f64::NAN), 10.0), 20);
        assert_eq!(predicted_frames(&plan, Some(0.0), 4.0), 8);
        // A container with no usable duration of its own defers to the stream.
        assert_eq!(predicted_frames(&plan, Some(3.0), 0.0), 6);
        // Neither: one frame, never zero.
        assert_eq!(predicted_frames(&plan, None, 0.0), 1);
    }

    #[test]
    fn two_thumbnails_short_is_tolerated_three_is_not() {
        assert!(!is_short(20, 20));
        assert!(!is_short(19, 20));
        assert!(!is_short(18, 20), "the last windows may have no sample of their own");
        assert!(is_short(17, 20));
        assert!(is_short(0, 3));
        assert!(!is_short(0, 2));
        assert!(!is_short(30, 20), "more than predicted is never short");
    }

    /// `n` raw thumbnails of a plan, as one decode would deliver them.
    fn fake_decoded(plan: &Plan, n: u32, keyframes_only: bool) -> Decoded {
        Decoded {
            raw: vec![90u8; plan.frame_bytes() * n as usize],
            source: PathBuf::from("/fake/clip.mp4"),
            keyframes_only,
        }
    }

    #[test]
    fn a_decode_that_reached_the_end_is_complete_without_asking_ffprobe() {
        let plan = plan_for(0.5, 20, false);
        let checked = decode_checked(&plan, 10.0, "clip", &mut |_| Ok(fake_decoded(&plan, 20, false)), &|_| {
            panic!("a full strip needs no second opinion")
        })
        .unwrap();
        assert_eq!((checked.frames, checked.complete), (20, true));
        assert_eq!(checked.raw.len(), plan.frame_bytes() * 20);
    }

    #[test]
    fn a_truncated_decode_is_not_complete() {
        // A file that goes bad halfway: ffmpeg exits 0 with 5 of the 20 thumbnails,
        // and the stream says it runs the full 10 s. (The stream length is the
        // point — a video really only 2.5 s long is the next test.)
        let plan = plan_for(0.5, 20, false);
        let checked = decode_checked(&plan, 10.0, "clip", &mut |_| Ok(fake_decoded(&plan, 5, false)), &|_| {
            Some(10.0)
        })
        .unwrap();
        assert_eq!((checked.frames, checked.complete), (5, false));
        // Without a stream duration the container's stands in for it.
        let checked = decode_checked(&plan, 10.0, "clip", &mut |_| Ok(fake_decoded(&plan, 5, false)), &|_| None).unwrap();
        assert_eq!((checked.frames, checked.complete), (5, false));
        // Three short is the line.
        let checked = decode_checked(&plan, 10.0, "clip", &mut |_| Ok(fake_decoded(&plan, 17, false)), &|_| {
            Some(10.0)
        })
        .unwrap();
        assert!(!checked.complete);
        let checked = decode_checked(&plan, 10.0, "clip", &mut |_| Ok(fake_decoded(&plan, 18, false)), &|_| {
            Some(10.0)
        })
        .unwrap();
        assert!(checked.complete);
    }

    #[test]
    fn a_video_shorter_than_its_container_is_complete_at_its_own_length() {
        // The audio runs 10 s, the picture 2.5 s: five thumbnails are all there is.
        let plan = plan_for(0.5, 20, false);
        let checked = decode_checked(&plan, 10.0, "clip", &mut |_| Ok(fake_decoded(&plan, 5, false)), &|_| {
            Some(2.5)
        })
        .unwrap();
        assert_eq!((checked.frames, checked.complete), (5, true));
        // A stream with no duration of its own cannot make that case; the strip is
        // judged against the container and kept out of the disk cache.
        let checked = decode_checked(&plan, 10.0, "clip", &mut |_| Ok(fake_decoded(&plan, 5, false)), &|_| None).unwrap();
        assert!(!checked.complete);
    }

    #[test]
    fn keyframe_sampling_that_falls_short_is_retried_with_every_frame() {
        // A 60 s asset sampled every 5 s: 12 thumbnails. A file whose keyframes are
        // sparse or badly flagged yields 3 by keyframes; every frame yields 12.
        let plan = plan_for(5.0, 12, false);
        let mut asked = Vec::new();
        let checked = decode_checked(
            &plan,
            60.0,
            "clip",
            &mut |keyframes| {
                asked.push(keyframes);
                Ok(fake_decoded(&plan, if keyframes { 3 } else { 12 }, keyframes))
            },
            &|_| Some(60.0),
        )
        .unwrap();
        assert_eq!(asked, [true, false]);
        assert_eq!((checked.frames, checked.complete), (12, true));

        // A keyframe pass that is only the usual tail short (the last keyframe
        // ended before the last window) is accepted as it is.
        let mut asked = Vec::new();
        let checked = decode_checked(
            &plan,
            60.0,
            "clip",
            &mut |keyframes| {
                asked.push(keyframes);
                Ok(fake_decoded(&plan, 11, keyframes))
            },
            &|_| Some(60.0),
        )
        .unwrap();
        assert_eq!(asked, [true]);
        assert_eq!((checked.frames, checked.complete), (11, true));

        // If the retry is short too, that is a truncated file, not a keyframe problem.
        let checked = decode_checked(&plan, 60.0, "clip", &mut |k| Ok(fake_decoded(&plan, 3, k)), &|_| Some(60.0)).unwrap();
        assert_eq!((checked.frames, checked.complete), (3, false));
    }

    #[test]
    fn a_decode_that_produced_nothing_or_failed_is_an_error() {
        let plan = plan_for(0.5, 20, false);
        let err = decode_checked(
            &plan,
            10.0,
            "/m/clip.mp4",
            &mut |_| Ok(fake_decoded(&plan, 0, false)),
            &|_| Some(10.0),
        )
        .err()
        .expect("no frames")
        .to_string();
        assert!(err.contains("no frames could be read from /m/clip.mp4"), "{err}");
        // A partial thumbnail's bytes do not make a frame.
        let mut half = fake_decoded(&plan, 0, false);
        half.raw = vec![0; plan.frame_bytes() / 2];
        assert!(decode_checked(&plan, 10.0, "c", &mut |_| Ok(half_clone(&half)), &|_| Some(10.0)).is_err());
        let err = decode_checked(&plan, 10.0, "c", &mut |_| Err(Error::Engine("boom".into())), &|_| {
            panic!("a failed decode is not judged")
        })
        .err()
        .expect("failed")
        .to_string();
        assert!(err.contains("boom"), "{err}");
    }

    fn half_clone(d: &Decoded) -> Decoded {
        Decoded {
            raw: d.raw.clone(),
            source: d.source.clone(),
            keyframes_only: d.keyframes_only,
        }
    }

    #[test]
    fn a_cache_hit_never_looks_for_a_proxy() {
        // Resolving the proxy can run an ffprobe; a hit — the memo's or the disk's —
        // must not pay for one, so the resolver is only called to *build*.
        let dir = Scratch::new("hit");
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"media").unwrap();
        let a = asset(file.to_str().unwrap(), 10.0, vec![video(Some(1920), Some(1080), false)]);
        let plan = Plan::for_asset(&a).unwrap();
        let key = entry_key(&a, &plan);
        let strip = sample_strip(&plan, 20, 3);

        // The disk cache.
        write_entry(&dir.0, &key, &strip);
        let hit = load_or_build(Some(&dir.0), &key, &a, &plan, || {
            panic!("the proxy was looked up on a disk hit")
        })
        .unwrap();
        assert_eq!(hit, strip);

        // The memo.
        let memoed = Arc::new(sample_strip(&plan, 20, 4));
        memo().lock().unwrap_or_else(|e| e.into_inner()).put(key, Arc::clone(&memoed));
        let hit = shared_strip(None, &a, || panic!("the proxy was looked up on a memo hit")).unwrap();
        assert!(Arc::ptr_eq(&hit, &memoed));
    }

    // ---- real ffmpeg --------------------------------------------------------

    use crate::engine::cli::{command, probe};

    /// Say why an end-to-end test has nothing to check on this ffmpeg.
    #[allow(clippy::print_stderr)]
    fn skip(why: &str) {
        eprintln!("{why}");
    }

    /// Run the real ffmpeg with `args`, asserting success.
    fn run_ffmpeg(args: &[&str]) {
        assert!(try_ffmpeg(args), "ffmpeg {args:?} failed");
    }

    fn try_ffmpeg(args: &[&str]) -> bool {
        command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .stdin(Stdio::null())
            .status_bounded()
            .expect("run ffmpeg")
            .success()
    }

    /// `path` probed the way the import builds its asset.
    fn probed(path: &Path) -> Asset {
        let p = probe(path).expect("probe");
        let mut a = asset(path.to_str().unwrap(), p.duration, p.streams);
        a.name = "clip".into();
        a
    }

    /// A 10 s, 25 fps clip whose luma *is* the frame number: frame `n` is a flat
    /// field of value `n` (0..=249), so a decoded thumbnail says exactly which
    /// frame it is.
    fn coded_clip(dir: &Scratch, seconds: u32) -> PathBuf {
        let file = dir.join("coded.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=black:s=64x36:r=25:d={seconds},format=yuv420p,geq=lum='N':cb=128:cr=128"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            // Lossless, so the luma that says which frame this is comes back exact.
            "-qp",
            "0",
            "-g",
            "25",
            "-pix_fmt",
            "yuv420p",
            file.to_str().unwrap(),
        ]);
        file
    }

    /// The Y value at the centre of every raw `yuv420p` thumbnail of the plan.
    fn centre_lumas(raw: &[u8], plan: &Plan) -> Vec<u8> {
        let stride = plan.frame_bytes();
        let (w, h) = (plan.frame_width as usize, plan.frame_height as usize);
        raw.chunks(stride).map(|f| f[(h / 2) * w + w / 2]).collect()
    }

    /// The semantics the module docs promise: thumbnail `k` is the frame on
    /// screen at `k * interval` — the last frame at or before it — and
    /// thumbnail 0 is the first frame. Checked against a clip whose frames number
    /// themselves, at several intervals, so an off-by-half-an-interval (the `fps`
    /// filter's default rounding) or an off-by-one-frame fails exactly.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored thumbnails_are_the_frames_on_screen`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn thumbnails_are_the_frames_on_screen_at_k_times_the_interval() {
        let dir = Scratch::new("coded");
        let clip = coded_clip(&dir, 10);
        let a = probed(&clip);
        assert!((a.duration - 10.0).abs() < 0.05, "{}", a.duration);

        for interval in [0.5, 1.0, 2.0, 5.0] {
            let plan = Plan {
                interval,
                frames: frames_for(10.0, interval),
                frame_width: 64,
                frame_height: 36,
                still: false,
            };
            let raw = decode_thumbs(&a, &plan, None, false).expect("decode").raw;
            assert_eq!(
                raw.len() / plan.frame_bytes(),
                plan.frames as usize,
                "{interval}s: thumbnail count"
            );
            let lumas = centre_lumas(&raw, &plan);
            for (k, luma) in lumas.iter().enumerate() {
                // 25 fps: the frame on screen at t is floor(t * 25).
                let want = (k as f64 * interval * 25.0).floor() as u8;
                assert_eq!(*luma, want, "{interval}s: thumbnail {k} should be frame {want}");
            }
        }
    }

    /// A picture that starts after the sound (a phone clip's first frame lands
    /// 0.7 s into the file) still has a first thumbnail that is its first frame —
    /// the `start_time=0` in the `fps` filter, which without it would number the
    /// samples from the first frame's slot instead — and the later ones keep to the
    /// *container's* clock, the one the timeline and `-ss` use.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored picture_that_starts_late`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_picture_that_starts_after_the_sound_keeps_its_first_thumbnail() {
        let dir = Scratch::new("offset");
        let coded = coded_clip(&dir, 10);
        let late = dir.join("late.mkv");
        run_ffmpeg(&[
            "-itsoffset",
            "0.7",
            "-i",
            coded.to_str().unwrap(),
            "-f",
            "lavfi",
            "-t",
            "10",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            late.to_str().unwrap(),
        ]);
        let a = probed(&late);
        let plan = Plan {
            interval: 2.0,
            frames: frames_for(a.duration, 2.0),
            frame_width: 64,
            frame_height: 36,
            still: false,
        };
        assert_eq!(plan.frames, 6, "the container runs to {:.2}s", a.duration);
        let lumas = centre_lumas(&decode_thumbs(&a, &plan, None, false).expect("decode").raw, &plan);
        assert_eq!(lumas.len(), 6, "{lumas:?}");
        // Frame n appears at 0.7 s + n / 25 (give or take the audio's priming
        // offset): the first thumbnail holds frame 0 back to t = 0, thumbnail k >= 1 is
        // the frame on screen at 2k s, within a frame of that arithmetic.
        assert_eq!(lumas[0], 0, "{lumas:?}");
        for (k, luma) in lumas.iter().enumerate().skip(1) {
            let want = ((2.0 * k as f64 - 0.7) * 25.0).floor() as i32;
            assert!(
                (i32::from(*luma) - want).abs() <= 1,
                "thumbnail {k}: frame {luma}, expected about {want}: {lumas:?}"
            );
        }
    }

    /// Mean absolute difference of two equally-sized byte buffers.
    fn mad(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs())
            .sum::<f64>()
            / a.len() as f64
    }

    /// Gray pixels of the whole of `sheet`'s thumbnail cell `index`
    /// (`frame_width` x `frame_height`), decoded by the real ffmpeg.
    fn sheet_cell_gray(sheet: &Path, plan: &Plan, index: u32) -> Vec<u8> {
        let out = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(sheet)
            .args([
                "-vf",
                &format!(
                    "crop={}:{}:{}:0,format=gray",
                    plan.frame_width,
                    plan.frame_height,
                    index * plan.frame_width
                ),
            ])
            .args(["-f", "rawvideo", "-"])
            .stdin(Stdio::null())
            .output()
            .expect("decode sheet");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    }

    /// A direct decode of `clip` at `t` seconds, scaled the way a thumbnail is.
    fn direct_gray(clip: &Path, plan: &Plan, t: f64) -> Vec<u8> {
        let out = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{t:.3}"), "-i"])
            .arg(clip)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale={}:{}:flags=area,format=gray", plan.frame_width, plan.frame_height),
            ])
            .args(["-f", "rawvideo", "-"])
            .stdin(Stdio::null())
            .output()
            .expect("decode frame");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    }

    fn testsrc2(dir: &Scratch, name: &str, seconds: u32, extra: &[&str]) -> PathBuf {
        let file = dir.join(name);
        let mut args = vec![
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            format!("testsrc2=size=320x180:rate=25:duration={seconds},format=yuv420p"),
            "-c:v".to_string(),
            "libx264".to_string(),
            "-preset".to_string(),
            "ultrafast".to_string(),
            "-crf".to_string(),
            "14".to_string(),
            "-pix_fmt".to_string(),
            "yuv420p".to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        args.push(file.to_str().unwrap().to_string());
        run_ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>());
        file
    }

    /// A real strip, end to end: the manifest's geometry is the JPEG's, the k-th
    /// thumbnail looks like a direct decode at `k * interval` (and unlike a decode
    /// a second later), the disk cache serves the next ask without the source, a
    /// damaged sheet is rebuilt, and the proxy-or-original choice does not change
    /// the answer.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored a_real_clip_gets`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_real_clip_gets_a_strip_whose_thumbnails_match_direct_decodes() {
        let dir = Scratch::new("strip");
        let clip = testsrc2(&dir, "clip.mp4", 10, &[]);
        let a = probed(&clip);
        let root = dir.join("cache");
        std::fs::create_dir_all(&root).unwrap();

        let strip = shared_strip(Some(&root), &a, || None).expect("strip");
        // 10 s at 0.5 s; 320x180 is 16:9 -> 170x96, 20 of them fit one 3400 px sheet.
        assert_eq!(
            (
                strip.interval,
                strip.frame_width,
                strip.frame_height,
                strip.frames,
                strip.columns,
                strip.sheets.len()
            ),
            (0.5, 170, 96, 20, 20, 1)
        );
        let sheet = &strip.sheets[0];
        assert_eq!((sheet.first_frame, sheet.count, sheet.width, sheet.height), (0, 20, 3400, 96));
        assert_eq!(
            jpeg_dimensions(&sheet.jpeg),
            Some((3400, 96)),
            "the manifest says what the JPEG is"
        );
        assert!(
            sheet.jpeg.len() < 200_000,
            "a sheet of 20 thumbnails is {} bytes",
            sheet.jpeg.len()
        );

        // The k-th thumbnail looks like a direct decode at k * interval, and
        // looks *less* like the same shot a second later (testsrc2 animates, so
        // a wrong time shows).
        let plan = Plan::for_asset(&a).unwrap();
        let sheet_file = dir.join("sheet.jpg");
        std::fs::write(&sheet_file, &sheet.jpeg).unwrap();
        for k in [0u32, 1, 7, 13, 19] {
            let thumb = sheet_cell_gray(&sheet_file, &plan, k);
            let at = mad(&thumb, &direct_gray(&clip, &plan, strip.time_of(k)));
            let later = mad(&thumb, &direct_gray(&clip, &plan, (strip.time_of(k) + 1.0).min(9.9)));
            assert!(
                at < 6.0,
                "thumbnail {k} differs from the frame at {}s by {at:.1}",
                strip.time_of(k)
            );
            assert!(
                at < later,
                "thumbnail {k}: {at:.1} from its own time, {later:.1} from a second later"
            );
        }

        // It was cached with a manifest, and a second ask is served from disk
        // alone: move the source away first. (The key is the source's identity, so
        // it is taken while the source is still where it was.)
        let key = entry_key(&a, &plan);
        let entry = root.join(&key);
        assert!(entry.join("manifest.json").is_file() && entry.join("sheet-000.jpg").is_file());
        let moved = dir.join("moved.mp4");
        std::fs::rename(&clip, &moved).unwrap();
        let again = load_or_build(Some(&root), &key, &a, &plan, || None).expect("served from the cache");
        assert_eq!(again, *strip);
        // A truncated sheet is rebuilt, not trusted (put the source back first).
        std::fs::rename(&moved, &clip).unwrap();
        let bytes = std::fs::read(entry.join("sheet-000.jpg")).unwrap();
        std::fs::write(entry.join("sheet-000.jpg"), &bytes[..bytes.len() / 2]).unwrap();
        let rebuilt = load_or_build(Some(&root), &key, &a, &plan, || None).expect("rebuilt");
        assert_eq!(rebuilt.frames, 20);
        assert_eq!(jpeg_dimensions(&rebuilt.sheets[0].jpeg), Some((3400, 96)));
        assert_eq!(
            std::fs::read(entry.join("sheet-000.jpg")).unwrap().len(),
            rebuilt.sheets[0].jpeg.len(),
            "the damaged sheet was replaced on disk"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "no working directory is left behind: {leftovers:?}");

        // A proxy gives the same strip under the same key (it is not an input to
        // the key), and a proxy that is broken or gone falls back to the original.
        let proxy = dir.join("proxy.mp4");
        run_ffmpeg(&[
            "-i",
            clip.to_str().unwrap(),
            "-an",
            "-vf",
            "scale=160:90",
            "-c:v",
            "libx264",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
            proxy.to_str().unwrap(),
        ]);
        let root2 = dir.join("cache2");
        std::fs::create_dir_all(&root2).unwrap();
        let from_proxy = load_or_build(Some(&root2), "proxied", &a, &plan, || Some(proxy.clone())).expect("from the proxy");
        assert_eq!((from_proxy.frames, from_proxy.frame_width), (20, 170));
        let sheet_file = dir.join("proxied.jpg");
        std::fs::write(&sheet_file, &from_proxy.sheets[0].jpeg).unwrap();
        let thumb = sheet_cell_gray(&sheet_file, &plan, 9);
        assert!(
            mad(&thumb, &direct_gray(&clip, &plan, 4.5)) < 12.0,
            "a proxy-made thumbnail is the same picture"
        );
        // ...and the proxy really is what is decoded when it is given: a white
        // decoy thumbnails white, whatever the original looks like.
        let decoy = dir.join("decoy.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=white:s=160x90:r=25:d=10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            decoy.to_str().unwrap(),
        ]);
        let mean = |cell: &[u8]| cell.iter().map(|v| f64::from(*v)).sum::<f64>() / cell.len() as f64;
        let decoyed = load_or_build(Some(&root2), "decoyed", &a, &plan, || Some(decoy.clone())).expect("from the decoy");
        let decoy_sheet = dir.join("decoyed.jpg");
        std::fs::write(&decoy_sheet, &decoyed.sheets[0].jpeg).unwrap();
        let (white, original) = (
            mean(&sheet_cell_gray(&decoy_sheet, &plan, 4)),
            mean(&sheet_cell_gray(&sheet_file, &plan, 4)),
        );
        assert!(white > 240.0 && original < 200.0, "decoy {white:.0}, original {original:.0}");
        let root3 = dir.join("cache3");
        std::fs::create_dir_all(&root3).unwrap();
        let broken = dir.join("not-a-proxy.mp4");
        std::fs::write(&broken, b"garbage").unwrap();
        let fell_back =
            load_or_build(Some(&root3), "fellback", &a, &plan, || Some(broken.clone())).expect("falls back to the original");
        assert_eq!(fell_back.frames, 20);
        let gone =
            load_or_build(Some(&root3), "gone", &a, &plan, || Some(dir.join("vanished.mp4"))).expect("and when it is gone");
        assert_eq!(gone.frames, 20);

        // A cache root that cannot be created (something else is in the way) still
        // yields a strip: it is built in the temp directory and just not kept.
        let blocked = dir.join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let uncached = load_or_build(Some(&blocked), "blocked", &a, &plan, || None).expect("built without a cache");
        assert_eq!(uncached.frames, 20);
        assert!(blocked.is_file(), "and what was in the way is untouched");
    }

    /// A decode that comes up short with the stream still claiming its full length
    /// (ffmpeg exiting 0 on a file that goes bad halfway) is returned for the
    /// session but never written to the disk cache, where it would pin a truncated
    /// strip for good; a complete one is cached. The decode is faked (a gray frame
    /// per thumbnail), the sheet encode is the real ffmpeg.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored truncated_decode`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_truncated_decode_is_returned_but_never_written_to_the_cache() {
        let dir = Scratch::new("truncated");
        let root = dir.join("cache");
        let plan = plan_for(0.5, 20, false);
        let part_dirs = |root: &Path| -> Vec<String> {
            std::fs::read_dir(root)
                .map(|d| {
                    d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .filter(|n| n.ends_with(".part"))
                        .collect()
                })
                .unwrap_or_default()
        };
        // A fake decode of `frames` thumbnails from a stream that says it runs `video_secs`.
        let build = |key: &str, frames: u32, video_secs: f64| {
            load_or_build_with(Some(&root), key, &plan, "fake", |work| {
                assemble(
                    &plan,
                    10.0,
                    "fake",
                    &mut |_| Ok(fake_decoded(&plan, frames, false)),
                    &|_| Some(video_secs),
                    work,
                )
            })
            .expect("a strip")
        };

        // 5 of 20 thumbnails, the stream saying it runs the full 10 s.
        let short = build("short", 5, 10.0);
        assert_eq!(short.frames, 5);
        assert_eq!(
            jpeg_dimensions(&short.sheets[0].jpeg),
            Some((5 * 170, 96)),
            "a real strip, returned"
        );
        assert!(!root.join("short").exists(), "but it is not published");
        assert!(
            part_dirs(&root).is_empty(),
            "and no working directory is left: {:?}",
            part_dirs(&root)
        );
        // The next ask finds nothing cached, so a later, complete decode is what gets kept.
        let again = build("short", 20, 10.0);
        assert_eq!(again.frames, 20);
        assert!(
            root.join("short").join("manifest.json").is_file(),
            "the complete one is cached"
        );

        // The same 5 thumbnails of a video that really is 2.5 s long are complete.
        let genuine = build("genuine", 5, 2.5);
        assert_eq!(genuine.frames, 5);
        assert_eq!(load_entry(&root.join("genuine"), &plan).map(|s| s.frames), Some(5));
    }

    /// By keyframes an original's thumbnail is the last *keyframe* at or before its
    /// time; decoding every frame gives the frame itself. A 20 s clip with a
    /// keyframe every 2 s, each frame's luma its number mod 250 (so frame 250 is 0):
    /// at 5 s the frame is 125 and the keyframe 100 (4 s); at 10 s both are 250,
    /// which is a keyframe.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored keyframe_sampling`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn keyframe_sampling_takes_the_last_keyframe_at_or_before_each_time() {
        let dir = Scratch::new("keyframes");
        let clip = dir.join("gop.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x36:r=25:d=20,format=yuv420p,geq=lum='mod(N,250)':cb=128:cr=128",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "50",
            "-keyint_min",
            "50",
            "-sc_threshold",
            "0",
            "-pix_fmt",
            "yuv420p",
            clip.to_str().unwrap(),
        ]);
        let a = probed(&clip);
        let plan = Plan {
            interval: 5.0,
            frames: 4,
            frame_width: 64,
            frame_height: 36,
            still: false,
        };

        let exact = decode_thumbs(&a, &plan, None, false).expect("every frame");
        assert!(!exact.keyframes_only);
        assert_eq!(
            centre_lumas(&exact.raw, &plan),
            [0, 125, 0, 125],
            "the frames themselves, at 0 / 5 / 10 / 15 s"
        );

        let sampled = decode_thumbs(&a, &plan, None, true).expect("keyframes");
        assert!(
            sampled.keyframes_only,
            "a 5 s interval on an original is sampled by keyframes"
        );
        assert_eq!(
            centre_lumas(&sampled.raw, &plan),
            [0, 100, 0, 100],
            "the last keyframe (every 2 s) at or before 0 / 5 / 10 / 15 s"
        );

        // Under the threshold the same file is sampled frame by frame.
        let fine = Plan {
            interval: 2.5,
            frames: 8,
            ..plan
        };
        assert!(!decode_thumbs(&a, &fine, None, true).expect("fine").keyframes_only);

        // A proxy is never sampled by keyframes: it is all-intra, so every frame is one.
        let proxy = dir.join("proxy.mp4");
        run_ffmpeg(&[
            "-i",
            clip.to_str().unwrap(),
            "-c:v",
            "libx264",
            "-qp",
            "0",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
            proxy.to_str().unwrap(),
        ]);
        let via_proxy = decode_thumbs(&a, &plan, Some(&proxy), true).expect("proxy");
        assert_eq!(
            (via_proxy.source.as_path(), via_proxy.keyframes_only),
            (proxy.as_path(), false)
        );
        assert_eq!(centre_lumas(&via_proxy.raw, &plan), [0, 125, 0, 125]);
    }

    /// A still image is one thumbnail at the right shape.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored a_still_image`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_still_image_is_one_thumbnail() {
        let dir = Scratch::new("still");
        let png = dir.join("card.png");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=400x300:rate=1:duration=1",
            "-frames:v",
            "1",
            png.to_str().unwrap(),
        ]);
        let mut a = probed(&png);
        // Import gives a still its default length.
        a.duration = crate::model::DEFAULT_IMAGE_DURATION;
        assert!(a.is_image());
        let strip = shared_strip(Some(&dir.join("cache")), &a, || None).expect("still strip");
        assert_eq!(
            (
                strip.frames,
                strip.columns,
                strip.frame_width,
                strip.frame_height,
                strip.sheets.len()
            ),
            (1, 1, 128, 96, 1)
        );
        assert_eq!(strip.interval, 5.0);
        assert_eq!(jpeg_dimensions(&strip.sheets[0].jpeg), Some((128, 96)));
        // Any time maps to the one thumbnail.
        assert_eq!((strip.frame_at(0.0), strip.frame_at(3.0), strip.frame_at(1e6)), (0, 0, 0));
    }

    /// An hour of footage is a capped strip split over several sheets, and its
    /// thumbnails still sit where the interval says.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored an_hour_long_clip`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn an_hour_long_clip_is_a_capped_multi_sheet_strip() {
        let dir = Scratch::new("hour");
        // 3600 frames at 1 fps; the luma ramps with the frame number, 255 at the end.
        let clip = dir.join("hour.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x36:r=1:d=3600,format=yuv420p,geq=lum='N*255/3599':cb=128:cr=128",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "10",
            // A keyframe on every sample: at 15 s an original is sampled by keyframes,
            // and this way each thumbnail is exactly the frame at its time.
            "-g",
            "15",
            "-keyint_min",
            "15",
            "-sc_threshold",
            "0",
            "-pix_fmt",
            "yuv420p",
            clip.to_str().unwrap(),
        ]);
        let a = probed(&clip);
        assert!((a.duration - 3600.0).abs() < 1.5, "{}", a.duration);
        let strip = shared_strip(Some(&dir.join("cache")), &a, || None).expect("strip");
        // 3600 s -> a thumbnail every 15 s = 240, within the cap; 64x36 is 170 px wide
        // -> 48 per sheet -> five even sheets.
        assert_eq!(strip.interval, 15.0);
        assert_eq!(strip.frames, 240);
        assert!(strip.frames <= MAX_FILMSTRIP_FRAMES);
        assert_eq!((strip.columns, strip.sheets.len()), (48, 5));
        for (i, sheet) in strip.sheets.iter().enumerate() {
            assert_eq!((sheet.first_frame, sheet.count), (i as u32 * 48, 48));
            assert_eq!((sheet.width, sheet.height), (48 * 170, 96));
            assert_eq!(jpeg_dimensions(&sheet.jpeg), Some((sheet.width, sheet.height)));
            assert!(sheet.width <= MAX_SHEET_WIDTH);
        }
        // Thumbnail 200 sits on the fifth sheet at column 8 and shows second 3000:
        // frame 3000 of 3600, luma 3000 * 255 / 3599 = 212 (JPEG is full range, so
        // allow its rescale and the lossy encode).
        let (sheet, x) = strip.locate(200).unwrap();
        assert_eq!((sheet.first_frame, x), (192, 8 * 170));
        let sheet_file = dir.join("s.jpg");
        std::fs::write(&sheet_file, &sheet.jpeg).unwrap();
        let plan = Plan::for_asset(&a).unwrap();
        let cell = sheet_cell_gray(&sheet_file, &plan, 8);
        let mean = cell.iter().map(|v| f64::from(*v)).sum::<f64>() / cell.len() as f64;
        // limited -> full range: (212 - 16) * 255 / 219 = 228
        assert!(
            (mean - 228.0).abs() < 6.0,
            "thumbnail 200 has luma {mean:.1}; second 3000 should read ~228"
        );
        // And thumbnail 0 is the first frame (luma 0 -> black).
        let first = sheet_cell_gray(&sheet_file_of(&dir, &strip.sheets[0].jpeg), &plan, 0);
        assert!(first.iter().all(|v| *v < 6), "thumbnail 0 is the black first frame");
    }

    fn sheet_file_of(dir: &Scratch, jpeg: &[u8]) -> PathBuf {
        let file = dir.join("first.jpg");
        std::fs::write(&file, jpeg).unwrap();
        file
    }

    /// A portrait phone clip — a landscape frame plus a display matrix — gets a
    /// portrait thumbnail, upright: ffmpeg autorotates, the probe reports the
    /// displayed size, and the strip is sized to it.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored rotated_phone_clip`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_rotated_phone_clip_gets_an_upright_portrait_strip() {
        let dir = Scratch::new("rotated");
        let landscape = testsrc2(&dir, "landscape.mp4", 4, &[]);
        let portrait = dir.join("portrait.mp4");
        let (src, out) = (landscape.to_str().unwrap(), portrait.to_str().unwrap());
        // FFmpeg 6+ takes the angle as an input option; older builds write the
        // matrix from a `rotate` tag.
        let wrote = try_ffmpeg(&["-display_rotation", "-90", "-i", src, "-c", "copy", out])
            || try_ffmpeg(&["-i", src, "-c", "copy", "-metadata:s:v:0", "rotate=90", out]);
        let a = probed(&portrait);
        if !wrote || (a.streams[0].width, a.streams[0].height) != (Some(180), Some(320)) {
            skip("skipped: this ffmpeg cannot write a display matrix");
            return;
        }
        let strip = shared_strip(Some(&dir.join("cache")), &a, || None).expect("strip");
        // Displayed 180x320: 96 high is 54 wide.
        assert_eq!((strip.frame_width, strip.frame_height), (54, 96));
        assert_eq!(strip.frames, 8);
        // And the picture is upright: thumbnail 3 matches an autorotated direct decode
        // (an unrotated one would be nothing like it).
        let plan = Plan::for_asset(&a).unwrap();
        let sheet_file = dir.join("sheet.jpg");
        std::fs::write(&sheet_file, &strip.sheets[0].jpeg).unwrap();
        let thumb = sheet_cell_gray(&sheet_file, &plan, 3);
        let direct = direct_gray(&portrait, &plan, 1.5);
        assert!(
            mad(&thumb, &direct) < 6.0,
            "the thumbnail is the upright picture: {:.1}",
            mad(&thumb, &direct)
        );
    }

    /// An HDR (HLG) clip's thumbnails are tone-mapped: next to its SDR original's,
    /// colourful rather than washed out.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored hlg_clip`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn an_hlg_clips_thumbnails_are_tone_mapped() {
        let dir = Scratch::new("hlg");
        let sdr = dir.join("sdr.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25:duration=2,format=yuv420p",
            "-c:v",
            "libx264",
            "-crf",
            "12",
            "-pix_fmt",
            "yuv420p",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            sdr.to_str().unwrap(),
        ]);
        let hlg = dir.join("hlg.mp4");
        let convert = |input: &[&str]| {
            let mut args: Vec<&str> = input.to_vec();
            args.extend([
                "-vf",
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "arib-std-b67",
                "-colorspace",
                "bt2020nc",
                hlg.to_str().unwrap(),
            ]);
            try_ffmpeg(&args)
        };
        let made = convert(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25:duration=2,format=yuv420p,\
             setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv",
        ]) || convert(&["-i", sdr.to_str().unwrap()]);
        if !made {
            skip("skipped: this ffmpeg cannot make the HLG test clip");
            return;
        }
        let (sdr_asset, hlg_asset) = (probed(&sdr), probed(&hlg));
        assert!(hlg_asset.hdr().is_some() && sdr_asset.hdr().is_none());
        let plan = Plan::for_asset(&hlg_asset).unwrap();
        let colourfulness = |a: &Asset| -> f64 {
            let raw = decode_thumbs(a, &plan, None, false).expect("decode").raw;
            // Chroma spread of the first thumbnail's U and V planes about neutral.
            let (w, h) = (plan.frame_width as usize, plan.frame_height as usize);
            let chroma = &raw[w * h..plan.frame_bytes()];
            chroma.iter().map(|c| (f64::from(*c) - 128.0).abs()).sum::<f64>() / chroma.len() as f64
        };
        let (sdr_c, hlg_c) = (colourfulness(&sdr_asset), colourfulness(&hlg_asset));
        assert!(sdr_c > 1.0, "the reference has colour: {sdr_c:.2}");
        assert!(
            (0.7..1.4).contains(&(hlg_c / sdr_c)),
            "tone-mapped thumbnails are as colourful as the SDR original's: {hlg_c:.2} vs {sdr_c:.2}"
        );
    }

    /// Several asks for one asset at once decode it once and share the answer.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored concurrent_requests`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn concurrent_requests_for_one_asset_share_one_strip() {
        let dir = Scratch::new("shared");
        let clip = testsrc2(&dir, "clip.mp4", 6, &[]);
        let a = probed(&clip);
        let root = dir.join("cache");
        let strips: Vec<Arc<Filmstrip>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..6)
                .map(|_| s.spawn(|| shared_strip(Some(&root), &a, || None).expect("strip")))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        // One build, handed to everyone: a second would be a second Arc.
        assert!(strips.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
        assert_eq!(strips[0].frames, 12);
    }
}
