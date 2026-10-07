//! Waveform peak pyramids: what the timeline draws a clip's audio from.
//!
//! [`waveform`](super::cli::waveform) answers "give me N peaks for the whole
//! file", which is the wrong question for an editor: a clip shows a *window* of
//! its source at a zoom that changes constantly, and re-decoding the file for
//! every window (or every zoom step) is minutes of ffmpeg per scroll. So the
//! file is decoded **once** into a pyramid of min/max peak pairs at four
//! resolutions, cached on disk, and any window is then an array slice
//! ([`waveform_range`]) — the same trick every DAW's "overview file" plays.
//!
//! * **One decode, bounded memory.** ffmpeg writes interleaved f32 PCM to a
//!   pipe; [`PyramidBuilder`] folds it into 96-sample buckets as it arrives and
//!   the PCM is dropped. Memory is the pyramid itself (about 18 MB per hour of
//!   stereo, 9 MB mono), never the file.
//! * **48 kHz, f32, source channel count up to two.** 96 samples is exactly a
//!   2 ms bucket (500 per second), and the coarser levels (100 / 25 / 10 per
//!   second) are whole multiples of it, so every bucket is an integer number of
//!   samples and `bucket * 1/rate` is its exact start with no drift. 48 kHz is
//!   also what nearly all video audio already is, so ffmpeg inserts no
//!   resampler: the peaks are those of the real samples. The old 8 kHz
//!   waveform low-passes at 4 kHz, which smears hi-hats and clicks and — worse
//!   for a meter — rings around a clipped plateau, so a flat-topped `1.0` reads
//!   as `0.93` with overshoot beside it. Stereo is kept as two lanes; mono is
//!   mono; anything wider is downmixed to stereo.
//! * **Peaks are quantized to `i16`** (`±32767` is full scale): a quarter of an
//!   `f32` pyramid, far finer than a lane is tall, and ±1.0 stays exactly
//!   representable so a clipped sample is recognizable.
//! * **Ungated, but a background job.** Like the other reads the timeline draws
//!   from, it never takes [`cpu::lease`] — a waveform appearing is not worth
//!   queueing behind an export — but it runs thread-capped and niced, at most
//!   [`MAX_CONCURRENT_DECODES`] at a time (a freshly opened project asks for
//!   every clip's waveform at once), and concurrent requests for the *same*
//!   file share one decode ([`shared_pyramid`]).
//! * **Nothing waits on ffmpeg forever:** the pipe is read on a side thread and
//!   a decode that produces nothing for [`DECODE_STALL`] is killed.
//! * **Cached at `<cache>/kerf/waveforms/<hash>.bin`**, keyed by the source's
//!   path + size + mtime (so a replaced file recomputes) and a format version,
//!   written to a temp file and renamed so a crash never leaves a half-written
//!   pyramid. A file that fails validation — short, corrupt, wrong version — is
//!   recomputed, never trusted.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::cli::{bg_command, command, ffmpeg_bin, ffprobe_bin, fnv1a, launch_err, source_key};
use super::cpu;
use crate::error::{Error, Result};

/// The rate the audio is decoded at. See the module docs for why it is not 8 kHz.
pub const PYRAMID_SAMPLE_RATE: u32 = 48_000;

/// Buckets per second of every pyramid level, finest first. Each is a whole
/// divisor of the finest, so a coarse bucket is an exact merge of fine ones.
pub const LEVEL_RATES: [u32; 4] = [500, 100, 25, 10];

/// Samples per bucket at the finest level: 96, i.e. 2 ms.
const FINEST_BUCKET_FRAMES: usize = (PYRAMID_SAMPLE_RATE / LEVEL_RATES[0]) as usize;

/// The most buckets one [`waveform_range`] will return, whatever was asked for.
/// A request is a pixel count; nothing on screen needs more, and an agent
/// picking a number out of a schema description should not be able to ask for
/// a million.
pub const MAX_RANGE_BUCKETS: usize = 4096;

/// How long a decode may go without producing a byte of audio before it is
/// killed. A healthy decode is dozens of times faster than real time and writes
/// continuously; a minute of silence is a wedged ffmpeg, not a slow one.
const DECODE_STALL: Duration = Duration::from_secs(60);

/// How long the channel-count probe may take.
const PROBE_LIMIT: Duration = Duration::from_secs(20);

/// Whole-file decodes allowed to run at once. Ungated against exports (see the
/// module docs), but not unbounded: opening a project with thirty clips would
/// otherwise start thirty ffmpegs.
const MAX_CONCURRENT_DECODES: usize = 2;

/// Byte budget for the in-process pyramid memo.
const MEMO_MAX_BYTES: usize = 64 << 20;

/// Full scale in the quantized representation.
const FULL_SCALE: f32 = 32767.0;

fn quantize(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * FULL_SCALE).round() as i16
}

fn dequantize(peak: i16) -> f32 {
    f32::from(peak) / FULL_SCALE
}

// ---- the pyramid -----------------------------------------------------------

/// One resolution of a [`WaveformPyramid`]: `rate` buckets per second, each the
/// lowest and highest sample in its slice of the source.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformLevel {
    /// Buckets per second of media. Bucket `k` covers `[k / rate, (k + 1) / rate)`.
    pub rate: u32,
    /// Per channel, the lowest sample of each bucket (`±32767` is ±full scale).
    pub min: Vec<Vec<i16>>,
    /// Per channel, the highest sample of each bucket.
    pub max: Vec<Vec<i16>>,
}

impl WaveformLevel {
    /// Buckets in this level (identical for every channel).
    pub fn len(&self) -> usize {
        self.min.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A file's audio reduced to min/max peaks at four resolutions
/// ([`LEVEL_RATES`]: 500, 100, 25 and 10 buckets per second).
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformPyramid {
    /// 1 (mono) or 2 (stereo — also what anything wider is downmixed to).
    pub channels: u8,
    /// Sample frames decoded, at [`PYRAMID_SAMPLE_RATE`].
    pub frames: u64,
    /// Finest first, one per [`LEVEL_RATES`] entry.
    pub levels: Vec<WaveformLevel>,
}

/// Bucket counts of every level for `frames` decoded sample frames: the finest
/// is `frames` in 96-sample buckets (the last one partial), and each coarser
/// level merges a whole number of those. `None` for a count that overflows.
fn expected_lens(frames: u64) -> Option<[usize; 4]> {
    let finest = usize::try_from(frames.div_ceil(FINEST_BUCKET_FRAMES as u64)).ok()?;
    let mut lens = [finest; 4];
    for (len, rate) in lens.iter_mut().zip(LEVEL_RATES) {
        *len = finest.div_ceil((LEVEL_RATES[0] / rate) as usize);
    }
    Some(lens)
}

impl WaveformPyramid {
    /// Length of the decoded audio, in seconds.
    pub fn duration(&self) -> f64 {
        self.frames as f64 / f64::from(PYRAMID_SAMPLE_RATE)
    }

    /// The pyramid given its finest level; the coarser ones are merged from it.
    /// `min` / `max` hold one vector per channel, each `ceil(frames / 96)` long.
    pub fn from_finest(frames: u64, min: Vec<Vec<i16>>, max: Vec<Vec<i16>>) -> Self {
        let channels = min.len() as u8;
        let coarsen = |per_channel: &[Vec<i16>], ratio: usize, pick: fn(i16, i16) -> i16| -> Vec<Vec<i16>> {
            per_channel
                .iter()
                .map(|ch| {
                    ch.chunks(ratio)
                        .map(|c| c.iter().copied().reduce(pick).unwrap_or(0))
                        .collect()
                })
                .collect()
        };
        // The coarser levels first, merged out of the finest, which is then moved
        // in at the front rather than copied (it is nine tenths of the pyramid).
        let mut levels: Vec<WaveformLevel> = LEVEL_RATES[1..]
            .iter()
            .map(|&rate| {
                let ratio = (LEVEL_RATES[0] / rate) as usize;
                WaveformLevel {
                    rate,
                    min: coarsen(&min, ratio, i16::min),
                    max: coarsen(&max, ratio, i16::max),
                }
            })
            .collect();
        levels.insert(
            0,
            WaveformLevel {
                rate: LEVEL_RATES[0],
                min,
                max,
            },
        );
        Self {
            channels,
            frames,
            levels,
        }
    }

    /// Bytes held, for the memo's budget (a close estimate, not an exact one).
    pub fn approx_bytes(&self) -> usize {
        let buckets: usize = self.levels.iter().map(WaveformLevel::len).sum();
        buckets * usize::from(self.channels) * 2 * std::mem::size_of::<i16>() + 256
    }
}

// ---- building it from PCM --------------------------------------------------

/// Folds interleaved f32 PCM into the finest pyramid level as it streams past.
/// Holds one bucket's running min/max plus the output; the samples themselves
/// are never kept.
struct PyramidBuilder {
    channels: usize,
    cur_min: [f32; 2],
    cur_max: [f32; 2],
    /// Frames folded into the current (unfinished) bucket.
    in_bucket: usize,
    frames: u64,
    min: Vec<Vec<i16>>,
    max: Vec<Vec<i16>>,
}

impl PyramidBuilder {
    fn new(channels: usize) -> Self {
        let channels = channels.clamp(1, 2);
        Self {
            channels,
            cur_min: [f32::INFINITY; 2],
            cur_max: [f32::NEG_INFINITY; 2],
            in_bucket: 0,
            frames: 0,
            min: vec![Vec::new(); channels],
            max: vec![Vec::new(); channels],
        }
    }

    /// Fold `samples` (whole interleaved frames) into the pyramid. Where it is
    /// split between calls does not matter — a bucket carries across them.
    fn push(&mut self, samples: &[f32]) {
        let ch = self.channels;
        let mut rest = samples;
        while rest.len() >= ch {
            let take = (rest.len() / ch).min(FINEST_BUCKET_FRAMES - self.in_bucket);
            let (head, tail) = rest.split_at(take * ch);
            for c in 0..ch {
                let (mut lo, mut hi) = (self.cur_min[c], self.cur_max[c]);
                // `<` / `>` are false for NaN, so a corrupt sample is skipped
                // rather than poisoning its bucket.
                for &s in head.iter().skip(c).step_by(ch) {
                    if s < lo {
                        lo = s;
                    }
                    if s > hi {
                        hi = s;
                    }
                }
                self.cur_min[c] = lo;
                self.cur_max[c] = hi;
            }
            self.in_bucket += take;
            self.frames += take as u64;
            if self.in_bucket == FINEST_BUCKET_FRAMES {
                self.close_bucket();
            }
            rest = tail;
        }
    }

    fn close_bucket(&mut self) {
        for c in 0..self.channels {
            // A bucket with no finite sample at all reads as silence.
            let (lo, hi) = if self.cur_min[c] <= self.cur_max[c] {
                (self.cur_min[c], self.cur_max[c])
            } else {
                (0.0, 0.0)
            };
            self.min[c].push(quantize(lo));
            self.max[c].push(quantize(hi));
        }
        self.cur_min = [f32::INFINITY; 2];
        self.cur_max = [f32::NEG_INFINITY; 2];
        self.in_bucket = 0;
    }

    fn finish(mut self) -> WaveformPyramid {
        if self.in_bucket > 0 {
            self.close_bucket();
        }
        WaveformPyramid::from_finest(self.frames, self.min, self.max)
    }
}

// ---- reading a window ------------------------------------------------------

/// A window of a pyramid, resampled to exactly `buckets` buckets: what the
/// timeline draws one clip from.
///
/// JSON (snake_case; `min` / `max` hold one array per channel, each `buckets`
/// long), e.g. four buckets of a stereo file whose left lane clips in the third
/// and whose right lane is silent:
/// `{"channels":2,"buckets":4,"duration":3.0,"peaks_per_second":25,
///   "min":[[-0.5,-0.5,-1.0,-0.5],[0.0,0.0,0.0,0.0]],
///   "max":[[0.5,0.5,1.0,0.5],[0.0,0.0,0.0,0.0]]}`.
/// Peaks are `-1.0..=1.0`, rounded to four places to keep the payload small, and
/// a clipped sample reads exactly `1.0` / `-1.0`. A bucket that lies outside the
/// media is `0.0` / `0.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveformRange {
    /// Lanes: 1 (mono) or 2 (stereo).
    pub channels: u8,
    /// Buckets returned per channel — the request, capped at
    /// [`MAX_RANGE_BUCKETS`]. It is what the arrays hold, not what was asked.
    pub buckets: usize,
    /// Length of the decoded media in seconds, so a caller can tell where the
    /// audio ends without asking again.
    pub duration: f64,
    /// Resolution of the pyramid level the buckets were taken from (10, 25, 100
    /// or 500). When it is below `buckets / (end - start)` the request was
    /// finer than the source and neighbouring buckets repeat.
    pub peaks_per_second: u32,
    /// Lowest sample per bucket, `[channel][bucket]`.
    pub min: Vec<Vec<f32>>,
    /// Highest sample per bucket, `[channel][bucket]`.
    pub max: Vec<Vec<f32>>,
}

/// The level to read `buckets_per_second` from: the **coarsest** whose buckets
/// are at least that dense (every requested bucket then spans one source bucket
/// or more, and the least data is touched), or the finest when the request is
/// denser than anything stored.
fn pick_level(levels: &[WaveformLevel], buckets_per_second: f64) -> Option<&WaveformLevel> {
    levels
        .iter()
        .rev()
        .find(|l| f64::from(l.rate) >= buckets_per_second - 1e-9)
        .or_else(|| levels.first())
}

/// The source buckets requested bucket `j` of `buckets` (over `[start,
/// start + span)` seconds) reads from a level of `rate` buckets per second and
/// `len` buckets, or `None` when it lies outside the media.
///
/// Source buckets are *partitioned* among the requested ones by where each
/// begins — bucket `i` belongs to the one whose interval holds `i / rate` — so
/// a peak lands in exactly one column rather than smearing across two, and at a
/// 1:1 ratio the columns are the source buckets. When the request is finer than
/// the source an interval can hold no bucket start at all; it then reads the
/// one source bucket under its midpoint.
fn source_range(rate: f64, start: f64, span: f64, j: usize, buckets: usize, len: usize) -> Option<Range<usize>> {
    // Absorbs the float error of `start + span * j / buckets` landing a hair
    // past a bucket boundary it is meant to sit on.
    const EPS: f64 = 1e-6;
    let s = start + span * j as f64 / buckets as f64;
    let e = start + span * (j + 1) as f64 / buckets as f64;
    let (mut lo, mut hi) = ((s * rate - EPS).ceil(), (e * rate - EPS).ceil());
    if hi <= lo {
        lo = ((s + e) * 0.5 * rate).floor();
        hi = lo + 1.0;
    }
    let (lo, hi) = (lo.max(0.0), hi.min(len as f64));
    (lo < hi).then_some(lo as usize..hi as usize)
}

/// `buckets` min/max pairs over `[start, end)` **source seconds** of `pyramid`,
/// per channel.
///
/// Buckets are evenly spaced across the requested window — one per pixel, say —
/// *including* any part of it outside the media, which reads `0.0`/`0.0`; the
/// caller's mapping from time to column stays linear. The level is the coarsest
/// that still has a source bucket per requested one ([`pick_level`]).
/// `buckets` is capped at [`MAX_RANGE_BUCKETS`]; `0` yields empty lanes, and an
/// empty, inverted or non-finite window yields `buckets` silent ones.
pub fn waveform_range(pyramid: &WaveformPyramid, start: f64, end: f64, buckets: usize) -> WaveformRange {
    let buckets = buckets.min(MAX_RANGE_BUCKETS);
    let channels = usize::from(pyramid.channels);
    let span = end - start;
    let window = start.is_finite() && end.is_finite() && span > 0.0;
    let level = if window && buckets > 0 {
        pick_level(&pyramid.levels, buckets as f64 / span)
    } else {
        pyramid.levels.first()
    };
    let mut out = WaveformRange {
        channels: pyramid.channels,
        buckets,
        duration: pyramid.duration(),
        peaks_per_second: level.map_or(LEVEL_RATES[0], |l| l.rate),
        min: vec![vec![0.0; buckets]; channels],
        max: vec![vec![0.0; buckets]; channels],
    };
    let Some(level) = level.filter(|_| window && buckets > 0) else {
        return out;
    };
    let rate = f64::from(level.rate);
    for j in 0..buckets {
        let Some(range) = source_range(rate, start, span, j, buckets, level.len()) else {
            continue;
        };
        for c in 0..channels {
            let lo = level.min[c][range.clone()].iter().copied().min().unwrap_or(0);
            let hi = level.max[c][range.clone()].iter().copied().max().unwrap_or(0);
            out.min[c][j] = round4(dequantize(lo));
            out.max[c][j] = round4(dequantize(hi));
        }
    }
    out
}

/// Four decimal places: finer than `i16` resolution is worth, and the shortest
/// `f32` printing JSON can give (`0.4999`, not `0.49998474`).
fn round4(v: f32) -> f32 {
    (v * 10_000.0).round() / 10_000.0
}

// ---- the on-disk cache -----------------------------------------------------

const MAGIC: &[u8; 4] = b"KWVF";

/// Bumped whenever the layout above or the way peaks are computed changes; it is
/// part of both the file name and the header.
const CACHE_VERSION: u32 = 1;

/// magic, version, channels, level count, 2 reserved, sample rate, frames.
const HEADER_BYTES: usize = 4 + 4 + 1 + 1 + 2 + 4 + 8;

/// Per level, in the table after the header: rate, bucket count.
const LEVEL_ENTRY_BYTES: usize = 4 + 8;

/// Serialize a pyramid: the header and level table, then for each level and
/// channel its `min` array followed by its `max` array, all little-endian `i16`.
fn encode(pyramid: &WaveformPyramid) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES + LEVEL_ENTRY_BYTES * pyramid.levels.len() + pyramid.approx_bytes());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    out.push(pyramid.channels);
    out.push(pyramid.levels.len() as u8);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&PYRAMID_SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&pyramid.frames.to_le_bytes());
    for level in &pyramid.levels {
        out.extend_from_slice(&level.rate.to_le_bytes());
        out.extend_from_slice(&(level.len() as u64).to_le_bytes());
    }
    for level in &pyramid.levels {
        for c in 0..usize::from(pyramid.channels) {
            for v in level.min[c].iter().chain(&level.max[c]) {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    out
}

/// A cursor over the cache file's bytes; every read is bounds-checked.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes)
    }

    fn i16s(&mut self, n: usize) -> Option<Vec<i16>> {
        let raw = self.take(n.checked_mul(2)?)?;
        Some(raw.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect())
    }
}

/// Parse a cache file, or `None` if anything about it is off: wrong magic or
/// version, a shape that is not what this build produces, bucket counts that do
/// not follow from the frame count, a size that is not exactly what the header
/// promises (short *or* long), or a bucket whose min exceeds its max. The size
/// is checked before anything is allocated, so a corrupt header cannot ask for
/// gigabytes.
fn decode(bytes: &[u8]) -> Option<WaveformPyramid> {
    let mut r = Cursor { bytes, at: 0 };
    if r.take(4)? != MAGIC || r.u32()? != CACHE_VERSION {
        return None;
    }
    let channels = r.u8()?;
    if !(1..=2).contains(&channels) || usize::from(r.u8()?) != LEVEL_RATES.len() {
        return None;
    }
    r.take(2)?;
    if r.u32()? != PYRAMID_SAMPLE_RATE {
        return None;
    }
    let frames = r.u64()?;
    let lens = expected_lens(frames)?;
    for (rate, len) in LEVEL_RATES.iter().zip(lens) {
        if r.u32()? != *rate || r.u64()? != len as u64 {
            return None;
        }
    }
    let payload = lens.iter().map(|&l| l as u128).sum::<u128>() * u128::from(channels) * 4;
    if (bytes.len() - r.at) as u128 != payload {
        return None;
    }
    let mut levels = Vec::with_capacity(lens.len());
    for (rate, len) in LEVEL_RATES.iter().zip(lens) {
        let (mut min, mut max) = (Vec::new(), Vec::new());
        for _ in 0..channels {
            let lo = r.i16s(len)?;
            let hi = r.i16s(len)?;
            if lo.iter().zip(&hi).any(|(lo, hi)| lo > hi) {
                return None;
            }
            min.push(lo);
            max.push(hi);
        }
        levels.push(WaveformLevel { rate: *rate, min, max });
    }
    Some(WaveformPyramid {
        channels,
        frames,
        levels,
    })
}

/// Where `src`'s pyramid is cached (whether or not it exists yet):
/// `<cache>/kerf/waveforms/<hash>.bin`. `None` when the OS has no cache
/// directory, in which case the pyramid is only memoized in process.
pub fn cache_path(src: &Path) -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("kerf").join("waveforms");
    let key = format!("{}|waveform-v{CACHE_VERSION}", source_key(src));
    Some(dir.join(format!("{:016x}.bin", fnv1a(&key))))
}

fn read_cache(file: &Path) -> Option<WaveformPyramid> {
    decode(&std::fs::read(file).ok()?)
}

/// Write the pyramid to `file` through a uniquely named temp file in the same
/// directory and rename it into place, so a reader (or a crash) never sees half
/// of it and two writers never share a temp file.
fn write_cache(file: &Path, pyramid: &WaveformPyramid) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = file.with_extension(format!("{}.{seq}.part", std::process::id()));
    let written = std::fs::write(&tmp, encode(pyramid)).and_then(|()| std::fs::rename(&tmp, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

// ---- decoding --------------------------------------------------------------

/// The ffmpeg arguments that decode `path`'s first audio stream to interleaved
/// f32le at [`PYRAMID_SAMPLE_RATE`] on stdout, in `channels` channels. Pure —
/// the thread cap and priority are applied where it is spawned.
fn build_pyramid_args(path: &Path, channels: u8) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-i"]
        .iter()
        .map(OsString::from)
        .collect();
    args.push(path.as_os_str().to_owned());
    args.extend(
        [
            "-map",
            "0:a:0",
            "-ac",
            &channels.to_string(),
            "-ar",
            &PYRAMID_SAMPLE_RATE.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ]
        .iter()
        .map(OsString::from),
    );
    args
}

/// The lane count `ffprobe -show_entries stream=channels` output asks for:
/// stereo for two or more channels, mono for one, `None` when there was no audio
/// stream to report (ffprobe prints nothing) or the output is not a count.
fn parse_channels(ffprobe_stdout: &str) -> Option<u8> {
    let n: u16 = ffprobe_stdout.lines().next()?.trim().parse().ok()?;
    (n > 0).then_some(n.min(2) as u8)
}

/// Channel count of the first audio stream, capped at two: stereo when the
/// source has two or more, mono when it has one.
fn probe_channels(path: &Path) -> Result<u8> {
    let bin = ffprobe_bin();
    let mut child = command(&bin)
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=channels"])
        .args(["-of", "default=nw=1:nk=1"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    // The answer is a line or two, far inside a pipe's buffer, so it is read
    // after the exit and the wait needs no reader threads.
    let deadline = Instant::now() + PROBE_LIMIT;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Engine(format!("probing {} timed out", path.display())));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let (mut out, mut err) = (String::new(), String::new());
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    if !status.success() {
        return Err(Error::Engine(format!("could not read {}: {}", path.display(), err.trim())));
    }
    parse_channels(&out).ok_or_else(|| Error::Engine(format!("{} has no audio stream", path.display())))
}

/// Keep the last few KB of a child's stderr: all a failure message needs, and a
/// flood of warnings cannot grow it.
pub(super) fn drain_tail(mut stderr: impl Read) -> String {
    const KEEP: usize = 8 * 1024;
    let mut tail: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n @ 1..) = stderr.read(&mut buf) {
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > KEEP {
            tail.drain(..tail.len() - KEEP);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

/// Read a running ffmpeg's stdout as f32le frames of `frame_bytes` bytes each
/// and hand them, whole frames only, to `on_frames`, until it ends. A child that
/// produces nothing for `stall` is killed and the call fails; a non-zero exit
/// fails it too. Always reaps the child.
fn pump_pcm(mut child: Child, frame_bytes: usize, stall: Duration, on_frames: &mut dyn FnMut(&[f32])) -> Result<()> {
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || drain_tail(stderr));

    // stdout is read on its own thread so the loop below can give up on a
    // silent ffmpeg instead of blocking in `read`. The bounded channel is the
    // backpressure: a slow consumer fills the pipe and throttles the decode.
    let mut stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(4);
    // Detached: after a kill it ends on EOF, or on the dropped receiver.
    std::thread::spawn(move || {
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
    let mut floats: Vec<f32> = Vec::new();
    let failure = loop {
        match rx.recv_timeout(stall) {
            Ok(Ok(bytes)) => {
                pending.extend_from_slice(&bytes);
                let whole = pending.len() / frame_bytes * frame_bytes;
                if whole > 0 {
                    floats.clear();
                    floats.extend(pending[..whole].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
                    pending.drain(..whole);
                    on_frames(&floats);
                }
            }
            Ok(Err(e)) => break Some(format!("waveform read failed: {e}")),
            Err(RecvTimeoutError::Disconnected) => break None,
            Err(RecvTimeoutError::Timeout) => {
                break Some(format!(
                    "waveform decode stalled: ffmpeg produced no audio for {}s",
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
        return Err(Error::Engine(format!("could not decode audio: {}", tail())));
    }
    Ok(())
}

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

/// Decode `path` and build its pyramid — no cache involved.
fn compute_pyramid(path: &Path) -> Result<WaveformPyramid> {
    let channels = probe_channels(path)?;
    let _slot = DecodeSlot::acquire();
    let bin = ffmpeg_bin();
    // Ungated (see the module docs), but capped and niced like every other
    // background read.
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu::budget_threads());
    let child = cmd
        .args(build_pyramid_args(path, channels))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let mut builder = PyramidBuilder::new(usize::from(channels));
    pump_pcm(child, usize::from(channels) * 4, DECODE_STALL, &mut |frames| {
        builder.push(frames);
    })?;
    Ok(builder.finish())
}

/// `path`'s pyramid through `cache_file` when given: read and validated if it is
/// there, otherwise decoded and written (a failed write is logged, not fatal —
/// the pyramid is still good).
fn pyramid_with_cache(path: &Path, cache_file: Option<&Path>) -> Result<WaveformPyramid> {
    if let Some(hit) = cache_file.and_then(read_cache) {
        return Ok(hit);
    }
    let started = Instant::now();
    let pyramid = compute_pyramid(path)?;
    tracing::debug!(
        path = %path.display(),
        seconds = pyramid.duration(),
        took_ms = started.elapsed().as_millis() as u64,
        "built waveform pyramid"
    );
    if let Some(file) = cache_file {
        if let Err(e) = write_cache(file, &pyramid) {
            tracing::warn!(path = %file.display(), error = %e, "could not cache waveform pyramid");
        }
    }
    Ok(pyramid)
}

/// The first audio stream of `path` as a [`WaveformPyramid`]: from the disk
/// cache when a valid one exists, else one ffmpeg decode that is then cached.
/// Blocking and heavy on a miss — run it off the project lock. Callers that ask
/// repeatedly want [`shared_pyramid`], which also memoizes in process.
pub fn waveform_pyramid(path: &Path) -> Result<WaveformPyramid> {
    pyramid_with_cache(path, cache_path(path).as_deref())
}

// ---- the in-process memo ---------------------------------------------------

/// A byte-bounded LRU of loaded pyramids: scrolling a timeline asks for a new
/// window every frame, and re-reading a multi-megabyte cache file for each would
/// cost more than the slice it serves.
struct PyramidMemo {
    map: HashMap<String, (u64, Arc<WaveformPyramid>)>,
    tick: u64,
    bytes: usize,
    cap: usize,
}

impl PyramidMemo {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            bytes: 0,
            cap,
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<WaveformPyramid>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.0 = tick;
        Some(Arc::clone(&entry.1))
    }

    /// Insert, evicting least-recently-used pyramids until the budget holds. One
    /// pyramid larger than the whole budget is still kept (alone): scrolling a
    /// ten-hour file should not read it from disk on every frame.
    fn put(&mut self, key: String, pyramid: Arc<WaveformPyramid>) {
        self.tick += 1;
        if let Some((_, old)) = self.map.remove(&key) {
            self.bytes -= old.approx_bytes();
        }
        let size = pyramid.approx_bytes();
        while !self.map.is_empty() && self.bytes + size > self.cap {
            let oldest = self.map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone());
            if let Some((_, evicted)) = oldest.and_then(|k| self.map.remove(&k)) {
                self.bytes -= evicted.approx_bytes();
            }
        }
        self.bytes += size;
        self.map.insert(key, (self.tick, pyramid));
    }
}

fn memo() -> &'static Mutex<PyramidMemo> {
    static MEMO: OnceLock<Mutex<PyramidMemo>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(PyramidMemo::new(MEMO_MAX_BYTES)))
}

fn memo_get(key: &str) -> Option<Arc<WaveformPyramid>> {
    memo().lock().unwrap_or_else(|e| e.into_inner()).get(key)
}

/// [`waveform_pyramid`] behind an in-process memo, with concurrent requests for
/// one file collapsed into a single load: a timeline with ten clips of the same
/// asset asks for the same pyramid ten times in the same instant, and only the
/// first should decode.
pub fn shared_pyramid(path: &Path) -> Result<Arc<WaveformPyramid>> {
    static FLIGHTS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let flights = FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()));

    // The same key as the disk cache's: a file that changed is a different entry.
    let key = source_key(path);
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
        // Whoever held the turn before us has probably just loaded it.
        match memo_get(&key) {
            Some(hit) => Ok(hit),
            None => waveform_pyramid(path).map(|p| {
                let p = Arc::new(p);
                memo()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(key.clone(), Arc::clone(&p));
                p
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

/// `[start, end)` source seconds of `path`'s audio as `buckets` peaks per
/// channel — [`shared_pyramid`] then [`waveform_range`]. Blocking (a first call
/// decodes the whole file); run it off the project lock.
pub fn waveform_range_of(path: &Path, start: f64, end: f64, buckets: usize) -> Result<WaveformRange> {
    Ok(waveform_range(&*shared_pyramid(path)?, start, end, buckets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::StatusBounded;

    // ---- synthetic pyramids -------------------------------------------------

    /// A mono pyramid whose finest bucket `i` is exactly `value(i)` (min = max),
    /// `len` buckets long (the last one full).
    fn mono_pyramid(len: usize, value: impl Fn(usize) -> i16) -> WaveformPyramid {
        let finest: Vec<i16> = (0..len).map(value).collect();
        WaveformPyramid::from_finest((len * FINEST_BUCKET_FRAMES) as u64, vec![finest.clone()], vec![finest])
    }

    /// A pyramid whose finest bucket `i` spans `lo(i)..=hi(i)` per channel.
    fn pyramid_of(channels: &[(Vec<i16>, Vec<i16>)]) -> WaveformPyramid {
        let len = channels[0].0.len();
        WaveformPyramid::from_finest(
            (len * FINEST_BUCKET_FRAMES) as u64,
            channels.iter().map(|c| c.0.clone()).collect(),
            channels.iter().map(|c| c.1.clone()).collect(),
        )
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-4
    }

    // ---- building -----------------------------------------------------------

    #[test]
    fn levels_are_exact_merges_of_the_finest() {
        // 7 finest buckets: level 100/s merges by 5 (2 buckets), 25/s by 20 and
        // 10/s by 50 (1 bucket each, the tail partial).
        let min: Vec<i16> = vec![-10, -20, -5, -1, -30, -2, -3];
        let max: Vec<i16> = vec![10, 5, 7, 40, 8, 9, 100];
        let p = WaveformPyramid::from_finest(7 * 96 - 10, vec![min], vec![max]);
        let rates: Vec<u32> = p.levels.iter().map(|l| l.rate).collect();
        assert_eq!(rates, LEVEL_RATES);
        let lens: Vec<usize> = p.levels.iter().map(WaveformLevel::len).collect();
        assert_eq!(lens, [7, 2, 1, 1]);
        assert_eq!(p.levels[1].min[0], [-30, -3]);
        assert_eq!(p.levels[1].max[0], [40, 100]);
        assert_eq!((p.levels[3].min[0][0], p.levels[3].max[0][0]), (-30, 100));
        assert_eq!(p.channels, 1);
    }

    #[test]
    fn level_lengths_follow_from_the_frame_count() {
        // The invariant the cache validator leans on, checked against what the
        // builder really produces for awkward lengths.
        for frames in [
            0u64,
            1,
            95,
            96,
            97,
            96 * 5,
            96 * 5 + 1,
            96 * 50,
            96 * 50 + 95,
            48_000 * 3 + 17,
        ] {
            let mut b = PyramidBuilder::new(1);
            b.push(&vec![0.1; frames as usize]);
            let p = b.finish();
            let got: Vec<usize> = p.levels.iter().map(WaveformLevel::len).collect();
            assert_eq!(got, expected_lens(frames).unwrap(), "{frames} frames");
            assert_eq!(p.frames, frames);
        }
    }

    #[test]
    fn the_builder_buckets_every_96_samples_per_channel() {
        let mut b = PyramidBuilder::new(2);
        // Left rises, right is the negative of it; 2 full buckets + 10 frames.
        let frames = 96 * 2 + 10;
        let mut pcm = Vec::new();
        for i in 0..frames {
            let v = i as f32 / 1000.0;
            pcm.push(v);
            pcm.push(-v);
        }
        b.push(&pcm);
        let p = b.finish();
        assert_eq!((p.channels, p.frames), (2, frames as u64));
        let fine = &p.levels[0];
        assert_eq!(fine.len(), 3);
        // Bucket 0 holds frames 0..96: left 0..=95/1000, right the mirror image.
        assert_eq!(fine.min[0][0], 0);
        assert_eq!(fine.max[0][0], quantize(0.095));
        assert_eq!(fine.min[1][0], quantize(-0.095));
        assert_eq!(fine.max[1][0], 0);
        // The partial tail bucket still reports what it saw.
        assert_eq!(fine.min[0][2], quantize(0.192));
        assert_eq!(fine.max[0][2], quantize(0.201));
    }

    #[test]
    fn the_builder_does_not_care_where_the_stream_was_split() {
        let samples: Vec<f32> = (0..96 * 11 + 33).map(|i| ((i * 37 % 200) as f32 - 100.0) / 100.0).collect();
        let mut whole = PyramidBuilder::new(1);
        whole.push(&samples);
        let whole = whole.finish();
        for piece in [1usize, 2, 7, 95, 96, 97, 500] {
            let mut b = PyramidBuilder::new(1);
            for chunk in samples.chunks(piece) {
                b.push(chunk);
            }
            assert_eq!(b.finish(), whole, "split every {piece} samples");
        }
        // Stereo: pieces are whole frames (the pump guarantees it).
        let stereo: Vec<f32> = samples.iter().flat_map(|s| [*s, s * 0.5]).collect();
        let mut one = PyramidBuilder::new(2);
        one.push(&stereo);
        let one = one.finish();
        let mut many = PyramidBuilder::new(2);
        for chunk in stereo.chunks(2 * 13) {
            many.push(chunk);
        }
        assert_eq!(many.finish(), one);
    }

    #[test]
    fn the_builder_survives_empty_nan_and_out_of_range_input() {
        let p = PyramidBuilder::new(1).finish();
        assert_eq!((p.frames, p.levels[0].len()), (0, 0));
        assert!(p.levels.iter().all(WaveformLevel::is_empty));

        // NaN is skipped; a bucket of nothing but NaN reads as silence; values
        // beyond full scale (a float file, or resampler overshoot) saturate.
        let mut b = PyramidBuilder::new(1);
        let mut pcm = vec![f32::NAN; 96];
        pcm.extend(vec![f32::NAN, 0.25, 3.0, -7.0, f32::INFINITY]);
        b.push(&pcm);
        let p = b.finish();
        assert_eq!((p.levels[0].min[0][0], p.levels[0].max[0][0]), (0, 0));
        assert_eq!((p.levels[0].min[0][1], p.levels[0].max[0][1]), (-32767, 32767));
    }

    #[test]
    fn quantizing_keeps_full_scale_exact_and_round_trips_closely() {
        assert_eq!(quantize(1.0), 32767);
        assert_eq!(quantize(-1.0), -32767);
        assert_eq!(quantize(0.0), 0);
        assert_eq!(dequantize(32767), 1.0);
        assert_eq!(dequantize(-32767), -1.0);
        for v in [-0.9, -0.5, -0.123, 0.0, 0.333, 0.75] {
            assert!((dequantize(quantize(v)) - v).abs() <= 1.0 / 32767.0, "{v}");
        }
    }

    // ---- level choice -------------------------------------------------------

    #[test]
    fn the_coarsest_level_that_still_has_a_source_bucket_per_column_is_chosen() {
        let p = mono_pyramid(1000, |_| 0);
        let rate = |buckets_per_second: f64| pick_level(&p.levels, buckets_per_second).unwrap().rate;
        // Anything at or under 10 columns a second is served by the 10/s level.
        assert_eq!(rate(1.0), 10);
        assert_eq!(rate(10.0), 10);
        // Just over the next level's density steps up to it.
        assert_eq!(rate(10.1), 25);
        assert_eq!(rate(25.0), 25);
        assert_eq!(rate(26.0), 100);
        assert_eq!(rate(100.0), 100);
        assert_eq!(rate(101.0), 500);
        assert_eq!(rate(500.0), 500);
        // Denser than anything stored: the finest, not nothing.
        assert_eq!(rate(5000.0), 500);
        assert!(pick_level(&[], 10.0).is_none());
    }

    #[test]
    fn the_returned_range_says_which_level_served_it() {
        let p = mono_pyramid(5000, |_| 0); // 10 s
        assert_eq!(waveform_range(&p, 0.0, 10.0, 80).peaks_per_second, 10);
        assert_eq!(waveform_range(&p, 0.0, 10.0, 900).peaks_per_second, 100);
        assert_eq!(waveform_range(&p, 0.0, 10.0, 4000).peaks_per_second, 500);
        assert_eq!(waveform_range(&p, 0.0, 1.0, 4000).peaks_per_second, 500);
    }

    // ---- aggregation --------------------------------------------------------

    #[test]
    fn a_one_to_one_request_reproduces_the_source_buckets() {
        // 1000 finest buckets = 2 s, a distinct value in each.
        let p = mono_pyramid(1000, |i| (i * 30) as i16);
        let r = waveform_range(&p, 0.0, 2.0, 1000);
        assert_eq!((r.channels, r.buckets, r.peaks_per_second), (1, 1000, 500));
        for i in [0usize, 1, 499, 998, 999] {
            let want = (i * 30) as f32 / FULL_SCALE;
            assert!(approx(r.min[0][i], want) && approx(r.max[0][i], want), "bucket {i}");
        }
        assert!((r.duration - 1000.0 * 96.0 / 48_000.0).abs() < 1e-9);
    }

    #[test]
    fn each_source_bucket_lands_in_exactly_one_column() {
        // Source bucket i holds 30 * i (30 units apart, so a bucket read by the
        // wrong column is far outside the tolerance). 500 columns over 2 s read
        // the 500/s level two buckets a column: column j is exactly buckets 2j
        // and 2j + 1. Were boundary buckets shared, min and max would bleed into
        // the neighbours.
        let p = mono_pyramid(1000, |i| (i * 30) as i16);
        let r = waveform_range(&p, 0.0, 2.0, 500);
        assert_eq!(r.peaks_per_second, 500);
        for j in 0..500 {
            let (lo, hi) = (60 * j as i32, 60 * j as i32 + 30);
            assert!(
                approx(r.min[0][j], lo as f32 / FULL_SCALE),
                "min of column {j}: {}",
                r.min[0][j]
            );
            assert!(
                approx(r.max[0][j], hi as f32 / FULL_SCALE),
                "max of column {j}: {}",
                r.max[0][j]
            );
        }
        // An awkward ratio (3 columns per 7 buckets) still partitions: no
        // source bucket is read twice or skipped.
        let mut seen = vec![0u32; 700];
        for j in 0..300 {
            let range = source_range(500.0, 0.0, 1.4, j, 300, 700).expect("inside");
            for i in range {
                seen[i] += 1;
            }
        }
        assert!(seen.iter().all(|&n| n == 1), "every bucket read exactly once");
    }

    #[test]
    fn a_coarser_level_aggregates_min_and_max_across_its_buckets() {
        // 10 s of finest buckets. A loud spike at 3.52 s must survive being
        // served from the 10/s level (0.1 s buckets): the column holding it
        // shows it, its neighbours do not.
        let mut min = vec![-1000i16; 5000];
        let mut max = vec![1000i16; 5000];
        min[1760] = -30000;
        max[1760] = 30000; // 1760 / 500 = 3.52 s
        let p = pyramid_of(&[(min, max)]);
        let r = waveform_range(&p, 0.0, 10.0, 100); // 10 columns a second
        assert_eq!(r.peaks_per_second, 10);
        assert!(approx(r.max[0][35], 30000.0 / FULL_SCALE) && approx(r.min[0][35], -30000.0 / FULL_SCALE));
        assert!(approx(r.max[0][34], 1000.0 / FULL_SCALE) && approx(r.max[0][36], 1000.0 / FULL_SCALE));
    }

    #[test]
    fn channels_are_aggregated_independently() {
        let left = (vec![-16000i16; 500], vec![16000i16; 500]);
        let right = (vec![0i16; 500], vec![0i16; 500]);
        let p = pyramid_of(&[left, right]);
        let r = waveform_range(&p, 0.0, 1.0, 10);
        assert_eq!((r.channels, r.min.len(), r.max.len()), (2, 2, 2));
        assert!(r.min[0].iter().all(|v| approx(*v, -16000.0 / FULL_SCALE)));
        assert!(r.max[0].iter().all(|v| approx(*v, 16000.0 / FULL_SCALE)));
        assert!(r.min[1].iter().chain(&r.max[1]).all(|v| *v == 0.0));
    }

    #[test]
    fn a_window_inside_the_media_reads_only_that_window() {
        // Value 100 for the first second, 200 after it.
        let p = mono_pyramid(1000, |i| if i < 500 { 100 } else { 200 });
        let r = waveform_range(&p, 1.0, 2.0, 20);
        assert!(r.max[0].iter().all(|v| approx(*v, 200.0 / FULL_SCALE)));
        let r = waveform_range(&p, 0.0, 1.0, 20);
        assert!(r.max[0].iter().all(|v| approx(*v, 100.0 / FULL_SCALE)));
        // Straddling: the first half of the columns see the quiet part.
        let r = waveform_range(&p, 0.5, 1.5, 20);
        assert!(r.max[0][..10].iter().all(|v| approx(*v, 100.0 / FULL_SCALE)));
        assert!(r.max[0][10..].iter().all(|v| approx(*v, 200.0 / FULL_SCALE)));
    }

    // ---- edges --------------------------------------------------------------

    #[test]
    fn parts_of_a_window_outside_the_media_are_silent_not_stretched() {
        let p = mono_pyramid(500, |_| 16384); // 1 s, constant 0.5
                                              // Window -1..2 s: a third of the columns before the media, a third in
                                              // it, a third after — the mapping stays linear.
        let r = waveform_range(&p, -1.0, 2.0, 30);
        assert!(r.max[0][..10].iter().chain(&r.min[0][..10]).all(|v| *v == 0.0));
        assert!(r.max[0][10..20].iter().all(|v| approx(*v, 0.5)));
        assert!(r.max[0][20..].iter().chain(&r.min[0][20..]).all(|v| *v == 0.0));
        // Entirely before / after / far away: all zeros, still `buckets` long.
        for (s, e) in [(-5.0, -1.0), (1.5, 3.0), (1e9, 1e9 + 10.0)] {
            let r = waveform_range(&p, s, e, 16);
            assert_eq!(r.buckets, 16);
            assert!(r.max[0].iter().chain(&r.min[0]).all(|v| *v == 0.0), "{s}..{e}");
        }
    }

    #[test]
    fn degenerate_requests_return_empty_or_silent_lanes_never_panic() {
        let p = pyramid_of(&[
            (vec![-100i16; 500], vec![100i16; 500]),
            (vec![-100i16; 500], vec![100i16; 500]),
        ]);
        // No buckets: the lanes exist and are empty.
        let r = waveform_range(&p, 0.0, 1.0, 0);
        assert_eq!((r.channels, r.buckets), (2, 0));
        assert!(r.min.iter().chain(&r.max).all(Vec::is_empty));
        // Empty, inverted and non-finite windows: `buckets` silent buckets.
        for (s, e) in [
            (1.0, 1.0),
            (2.0, 1.0),
            (f64::NAN, 1.0),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 1.0),
        ] {
            let r = waveform_range(&p, s, e, 8);
            assert_eq!((r.buckets, r.min.len()), (8, 2), "{s}..{e}");
            assert!(r.min.iter().chain(&r.max).flatten().all(|v| *v == 0.0), "{s}..{e}");
        }
        // An empty pyramid (a stream with no samples) is all silence too.
        let empty = PyramidBuilder::new(2).finish();
        let r = waveform_range(&empty, 0.0, 1.0, 5);
        assert_eq!((r.buckets, r.duration), (5, 0.0));
        assert!(r.max.iter().flatten().all(|v| *v == 0.0));
    }

    #[test]
    fn an_absurd_bucket_count_is_capped() {
        let p = mono_pyramid(500, |_| 1000);
        let r = waveform_range(&p, 0.0, 1.0, 1_000_000);
        assert_eq!(r.buckets, MAX_RANGE_BUCKETS);
        assert_eq!((r.min[0].len(), r.max[0].len()), (MAX_RANGE_BUCKETS, MAX_RANGE_BUCKETS));
        let r = waveform_range(&p, 0.0, 1.0, usize::MAX);
        assert_eq!(r.buckets, MAX_RANGE_BUCKETS);
    }

    #[test]
    fn asking_for_more_columns_than_the_source_has_repeats_buckets_in_order() {
        // 4096 columns over 0.2 s of a 500/s source: 100 source buckets, so
        // roughly 41 columns each. The staircase must be monotonic — a column
        // never reads an earlier bucket than the one before it.
        let p = mono_pyramid(500, |i| (i * 10) as i16);
        let r = waveform_range(&p, 0.0, 0.2, 4096);
        assert_eq!(r.peaks_per_second, 500);
        assert!(r.max[0].windows(2).all(|w| w[0] <= w[1]), "monotone staircase");
        assert!(r.max[0].iter().all(|v| *v >= 0.0));
        let last = *r.max[0].last().unwrap();
        assert!(approx(last, 990.0 / FULL_SCALE), "reaches the last source bucket: {last}");
    }

    #[test]
    fn float_error_does_not_shift_a_column_off_its_bucket() {
        // `start + span * j / buckets` is not exact in binary. Every column must
        // still read the bucket whose start it sits on, from the first second of
        // the file to the seventeenth. A value that repeats every 1000 buckets
        // keeps them in `i16` range while each neighbour stays 30 units apart.
        let p = mono_pyramid(20_000, |i| ((i % 1000) * 30) as i16);
        for start in [0.1, 0.3, 7.7, 17.3] {
            // 100 columns over 0.2 s is 500 a second: one source bucket each.
            let r = waveform_range(&p, start, start + 0.2, 100);
            assert_eq!(r.peaks_per_second, 500);
            let first = (start * 500.0_f64).round() as usize;
            for j in 0..100 {
                let want = (((first + j) % 1000) * 30) as f32 / FULL_SCALE;
                assert!(
                    approx(r.min[0][j], want),
                    "start {start}, column {j}: {} vs {want}",
                    r.min[0][j]
                );
                assert!(
                    approx(r.max[0][j], want),
                    "start {start}, column {j}: {} vs {want}",
                    r.max[0][j]
                );
            }
        }
        // The same at the 100/s level, where a column is five finest buckets.
        let r = waveform_range(&p, 0.1, 1.1, 100);
        assert_eq!(r.peaks_per_second, 100);
        for j in 0..100 {
            let first = 50 + 5 * j;
            assert!(approx(r.min[0][j], ((first % 1000) * 30) as f32 / FULL_SCALE), "column {j}");
        }
    }

    #[test]
    fn range_json_is_flat_snake_case_with_short_numbers() {
        let p = pyramid_of(&[
            (vec![-16384i16; 500], vec![16384i16; 500]),
            (vec![0i16; 500], vec![0i16; 500]),
        ]);
        let r = waveform_range(&p, 0.0, 1.0, 2);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["channels"], 2);
        assert_eq!(json["buckets"], 2);
        assert_eq!(json["peaks_per_second"], 10);
        assert!(json["duration"].as_f64().unwrap() > 0.99);
        assert_eq!(json["min"].as_array().unwrap().len(), 2);
        assert_eq!(json["max"][0].as_array().unwrap().len(), 2);
        // Rounded to four places: 16384 / 32767 prints as 0.5, not 0.50001526.
        assert_eq!(serde_json::to_string(&json["max"][0]).unwrap(), "[0.5,0.5]");
        let back: WaveformRange = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    // ---- the cache file -----------------------------------------------------

    fn sample_pyramid() -> WaveformPyramid {
        let left: Vec<i16> = (0..700).map(|i| ((i * 31) % 20000) as i16 - 10000).collect();
        let right: Vec<i16> = (0..700).map(|i| ((i * 17) % 9000) as i16 - 4500).collect();
        let hi = |v: &[i16]| v.iter().map(|x| x.saturating_add(500)).collect::<Vec<i16>>();
        WaveformPyramid::from_finest(700 * 96 - 3, vec![left.clone(), right.clone()], vec![hi(&left), hi(&right)])
    }

    #[test]
    fn a_pyramid_survives_the_cache_file() {
        let p = sample_pyramid();
        let bytes = encode(&p);
        assert_eq!(&bytes[..4], b"KWVF");
        assert_eq!(decode(&bytes), Some(p));
        // Mono and empty round-trip too.
        let mono = mono_pyramid(123, |i| i as i16);
        assert_eq!(decode(&encode(&mono)), Some(mono));
        let empty = PyramidBuilder::new(1).finish();
        assert_eq!(decode(&encode(&empty)), Some(empty));
    }

    #[test]
    fn a_damaged_cache_file_is_rejected_not_trusted() {
        let good = encode(&sample_pyramid());
        assert!(decode(&good).is_some());

        // Short at every layer: empty, mid-header, mid-table, mid-payload.
        for cut in [0, 3, 10, HEADER_BYTES - 1, HEADER_BYTES + 5, good.len() / 2, good.len() - 1] {
            assert_eq!(decode(&good[..cut]), None, "truncated to {cut}");
        }
        // Too long (an appended byte is as wrong as a missing one).
        let mut long = good.clone();
        long.push(0);
        assert_eq!(decode(&long), None);

        // Header fields: magic, version, channels, level count, sample rate.
        let flip = |at: usize, to: u8| {
            let mut b = good.clone();
            b[at] = to;
            decode(&b)
        };
        assert_eq!(flip(0, b'X'), None, "magic");
        assert_eq!(flip(4, 2), None, "version");
        assert_eq!(flip(8, 0), None, "zero channels");
        assert_eq!(flip(8, 3), None, "three channels");
        assert_eq!(flip(9, 3), None, "level count");
        assert_eq!(flip(12, 0), None, "sample rate");
        // A frame count that no longer matches the stored bucket counts.
        let mut bad_frames = good.clone();
        bad_frames[16..24].copy_from_slice(&(u64::MAX).to_le_bytes());
        assert_eq!(decode(&bad_frames), None, "absurd frame count must not allocate");
        let mut off_by_a_bucket = good.clone();
        off_by_a_bucket[16..24].copy_from_slice(&(700u64 * 96 + 500).to_le_bytes());
        assert_eq!(decode(&off_by_a_bucket), None);
        // A level table entry that disagrees (rate, then length).
        let mut bad_rate = good.clone();
        bad_rate[HEADER_BYTES] ^= 0xFF;
        assert_eq!(decode(&bad_rate), None, "level rate");
        let mut bad_len = good.clone();
        bad_len[HEADER_BYTES + 4] ^= 0x01;
        assert_eq!(decode(&bad_len), None, "level length");

        // A bucket whose min is above its max cannot have come from us. (Left
        // channel, finest level: first min set to the top.)
        let payload = HEADER_BYTES + LEVEL_ENTRY_BYTES * 4;
        let mut inverted = good.clone();
        inverted[payload..payload + 2].copy_from_slice(&i16::MAX.to_le_bytes());
        assert_eq!(decode(&inverted), None, "min above max");

        // Random-looking garbage of the right magic.
        let mut junk = b"KWVF".to_vec();
        junk.extend((0..400).map(|i| (i * 7 + 3) as u8));
        assert_eq!(decode(&junk), None);
    }

    /// A scratch directory removed at the end of the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kerf-peaks-{tag}-{}", std::process::id()));
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

    #[test]
    fn the_cache_is_written_atomically_and_read_back() {
        let dir = Scratch::new("cache-io");
        let file = dir.join("nested").join("a.bin");
        let p = sample_pyramid();
        write_cache(&file, &p).expect("write creates the directory");
        assert_eq!(read_cache(&file), Some(p.clone()));
        // Overwriting replaces it; nothing is left behind but the file itself.
        let q = mono_pyramid(40, |i| i as i16);
        write_cache(&file, &q).expect("overwrite");
        assert_eq!(read_cache(&file), Some(q));
        let names: Vec<String> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.bin"], "no temp file survives");

        // Missing and corrupt files both read as a miss.
        assert_eq!(read_cache(&dir.join("absent.bin")), None);
        std::fs::write(&file, &encode(&p)[..100]).unwrap();
        assert_eq!(read_cache(&file), None);
    }

    #[test]
    fn the_cache_key_follows_the_file() {
        let dir = Scratch::new("cache-key");
        let a = dir.join("a.wav");
        std::fs::write(&a, b"one").unwrap();
        // A machine with no cache directory (no HOME) has nothing to key.
        let Some(first) = cache_path(&a) else {
            return;
        };
        assert_eq!(Some(&first), cache_path(&a).as_ref(), "stable for an unchanged file");
        assert!(first.to_string_lossy().contains("waveforms"), "{first:?}");
        assert_eq!(first.extension().and_then(|e| e.to_str()), Some("bin"));
        // A different size (and a different path) is a different entry.
        std::fs::write(&a, b"longer contents").unwrap();
        assert_ne!(Some(first), cache_path(&a), "a changed file must not reuse the old pyramid");
        let b = dir.join("b.wav");
        std::fs::write(&b, b"longer contents").unwrap();
        assert_ne!(cache_path(&a), cache_path(&b));
    }

    // ---- the memo -----------------------------------------------------------

    #[test]
    fn the_memo_evicts_least_recently_used_by_bytes() {
        let p = || Arc::new(mono_pyramid(500, |_| 0));
        let size = p().approx_bytes();
        let mut memo = PyramidMemo::new(size * 2 + size / 2); // room for two
        memo.put("a".into(), p());
        memo.put("b".into(), p());
        assert!(memo.get("a").is_some(), "touch a so b is the oldest");
        memo.put("c".into(), p());
        assert!(memo.get("b").is_none(), "b was least recently used");
        assert!(memo.get("a").is_some() && memo.get("c").is_some());
        assert!(memo.bytes <= memo.cap, "{} > {}", memo.bytes, memo.cap);
        // Re-putting a key replaces it without double-counting its bytes.
        memo.put("a".into(), p());
        memo.put("a".into(), p());
        assert_eq!(memo.bytes, size * 2);
    }

    #[test]
    fn an_oversized_pyramid_is_kept_alone_rather_than_refused() {
        let small = Arc::new(mono_pyramid(10, |_| 0));
        let big = Arc::new(mono_pyramid(2000, |_| 0));
        let mut memo = PyramidMemo::new(big.approx_bytes() / 2);
        memo.put("small".into(), small);
        memo.put("big".into(), Arc::clone(&big));
        assert!(memo.get("small").is_none());
        assert!(Arc::ptr_eq(&memo.get("big").unwrap(), &big));
        assert_eq!(memo.map.len(), 1);
    }

    // ---- ffmpeg plumbing ----------------------------------------------------

    #[test]
    fn the_decode_command_asks_for_f32_pcm_at_the_pyramid_rate() {
        let args: Vec<String> = build_pyramid_args(Path::new("/media/clip.mov"), 2)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("no {flag}: {args:?}"))
        };
        assert_eq!(args[at("-i") + 1], "/media/clip.mov");
        assert_eq!(args[at("-map") + 1], "0:a:0");
        assert_eq!(args[at("-ac") + 1], "2");
        assert_eq!(args[at("-ar") + 1], "48000");
        assert_eq!(args[at("-f") + 1], "f32le");
        assert_eq!(
            args.last().unwrap(),
            "pipe:1",
            "the sink is last (thread caps are spliced in before it)"
        );
        // Pure: no thread flags — those are written at spawn time.
        assert!(!args.iter().any(|a| a.contains("threads")), "{args:?}");
        let mono = build_pyramid_args(Path::new("a.wav"), 1);
        assert!(mono.windows(2).any(|w| w[0] == "-ac" && w[1] == "1"));
    }

    #[test]
    fn the_lane_count_is_stereo_at_most() {
        assert_eq!(parse_channels("1\n"), Some(1));
        assert_eq!(parse_channels("2\n"), Some(2));
        // Surround is downmixed to the two lanes a clip can show.
        assert_eq!(parse_channels("6\n"), Some(2));
        assert_eq!(parse_channels("  8  \r\n"), Some(2));
        // No audio stream prints nothing; nonsense is not a count.
        assert_eq!(parse_channels(""), None);
        assert_eq!(parse_channels("\n"), None);
        assert_eq!(parse_channels("0\n"), None);
        assert_eq!(parse_channels("N/A\n"), None);
        assert_eq!(parse_channels("-2\n"), None);
        // Only the first stream's line counts.
        assert_eq!(parse_channels("1\n6\n"), Some(1));
    }

    /// A stand-in for ffmpeg: `script` run by `sh` with piped stdout / stderr.
    /// `exec` in the script so a kill reaches the process holding the pipes.
    #[cfg(unix)]
    fn fake_decoder(script: &str) -> Child {
        command("sh")
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
    fn the_pump_delivers_whole_frames_even_when_the_pipe_splits_them() {
        // Three stereo frames, written as 5-byte pieces so every frame straddles
        // a read; the consumer must still see aligned samples in order.
        let samples = [0.5f32, -0.5, 0.25, -0.25, 1.0, -1.0];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let script: String = bytes
            .chunks(5)
            .map(|c| format!("printf '{}'; sleep 0.05; ", printf_escape(c)))
            .collect();
        let mut got = Vec::new();
        pump_pcm(fake_decoder(&script), 8, Duration::from_secs(5), &mut |f| {
            assert_eq!(f.len() % 2, 0, "only whole frames reach the consumer");
            got.extend_from_slice(f);
        })
        .expect("a clean stream");
        assert_eq!(got, samples);
    }

    #[cfg(unix)]
    #[test]
    fn a_decode_that_never_produces_audio_is_killed_not_awaited() {
        let started = Instant::now();
        let err = pump_pcm(
            fake_decoder("echo cannot open the stream >&2; exec sleep 60"),
            4,
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
        let bytes: Vec<u8> = [0.5f32, 0.25].iter().flat_map(|s| s.to_le_bytes()).collect();
        let script = format!("printf '{}'; exec sleep 60", printf_escape(&bytes));
        let mut got = Vec::new();
        let err = pump_pcm(fake_decoder(&script), 4, Duration::from_millis(400), &mut |f| {
            got.extend_from_slice(f);
        })
        .expect_err("stall")
        .to_string();
        assert!(err.contains("stalled"), "{err}");
        assert_eq!(got, [0.5, 0.25]);
    }

    #[cfg(unix)]
    #[test]
    fn a_decoder_that_fails_reports_why() {
        let err = pump_pcm(
            fake_decoder("echo no such stream >&2; exit 3"),
            4,
            Duration::from_secs(5),
            &mut |_| {},
        )
        .expect_err("non-zero exit")
        .to_string();
        assert!(
            err.contains("could not decode audio") && err.contains("no such stream"),
            "{err}"
        );
    }

    #[test]
    fn a_stderr_flood_keeps_only_its_tail() {
        let flood: Vec<u8> = (0..100_000).map(|i| b'a' + (i % 26) as u8).chain(*b"THE END").collect();
        let tail = drain_tail(&flood[..]);
        assert!(tail.len() <= 8 * 1024 + 4096);
        assert!(tail.ends_with("THE END"));
    }

    // ---- real ffmpeg ---------------------------------------------------------

    /// Synthesize `file` from lavfi `args` with the real ffmpeg.
    fn synth(file: &Path, args: &[&str]) {
        let status = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .arg(file)
            .stdin(Stdio::null())
            .status_bounded()
            .expect("run ffmpeg");
        assert!(status.success(), "ffmpeg {args:?} failed");
    }

    /// A 3 s stereo float WAV: the left lane is a 0.5-amplitude 440 Hz sine with a
    /// hard-clipped burst between 1.0 s and 1.1 s; the right lane is silent.
    fn stereo_tone(dir: &Scratch) -> PathBuf {
        let file = dir.join("tone.wav");
        let left = "if(between(t,1,1.1),clip(8*sin(2*PI*440*t),-1,1),0.5*sin(2*PI*440*t))";
        synth(
            &file,
            &[
                "-f",
                "lavfi",
                "-i",
                &format!("aevalsrc='{left}|0':s=48000:d=3:c=stereo"),
                "-c:a",
                "pcm_f32le",
            ],
        );
        file
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored synthesized_stereo_tone`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_synthesized_stereo_tone_has_the_peaks_it_was_made_with() {
        let dir = Scratch::new("tone");
        let wav = stereo_tone(&dir);
        let p = pyramid_with_cache(&wav, None).expect("pyramid");

        assert_eq!(p.channels, 2, "stereo is kept as two lanes");
        assert!((p.duration() - 3.0).abs() < 0.01, "duration {}", p.duration());
        assert_eq!(p.levels.iter().map(|l| l.rate).collect::<Vec<_>>(), LEVEL_RATES);
        // 3 s at 500 / 100 / 25 / 10 per second.
        assert_eq!(
            p.levels.iter().map(WaveformLevel::len).collect::<Vec<_>>(),
            [1500, 300, 75, 30]
        );

        // The quiet stretch: the left lane peaks at the sine's 0.5, the right is
        // dead silent.
        let quiet = waveform_range(&p, 0.2, 0.8, 12);
        let (lmax, lmin) = (
            quiet.max[0].iter().copied().fold(0.0, f32::max),
            quiet.min[0].iter().copied().fold(0.0, f32::min),
        );
        assert!((0.49..=0.501).contains(&lmax), "left max {lmax}");
        assert!((-0.501..=-0.49).contains(&lmin), "left min {lmin}");
        assert!(
            quiet.min[1].iter().chain(&quiet.max[1]).all(|v| v.abs() < 1e-4),
            "right lane is silent"
        );

        // The clipped burst reaches full scale, both ways, on the left only.
        let burst = waveform_range(&p, 1.0, 1.1, 5);
        assert!(burst.max[0].iter().any(|v| *v >= 0.9999), "{:?}", burst.max[0]);
        assert!(burst.min[0].iter().any(|v| *v <= -0.9999), "{:?}", burst.min[0]);
        assert!(burst.max[1].iter().chain(&burst.min[1]).all(|v| v.abs() < 1e-4));

        // ...and survives the coarsest level: 6 columns over 3 s is served from
        // the 10/s level, whose bucket for t = 1.0..1.1 is column 2.
        let coarse = waveform_range(&p, 0.0, 3.0, 6);
        assert_eq!(coarse.peaks_per_second, 10);
        assert!(coarse.max[0][2] >= 0.9999 && coarse.min[0][2] <= -0.9999, "{coarse:?}");
        assert!(coarse.max[0][5] < 0.51, "after the burst it is quiet again");

        // Past the end of the file there is nothing.
        let tail = waveform_range(&p, 2.5, 4.5, 8);
        assert!(tail.max[0][5..].iter().all(|v| *v == 0.0), "{:?}", tail.max[0]);
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored cache_round_trip`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn the_disk_cache_round_trips_and_recovers_from_damage() {
        let dir = Scratch::new("tone-cache");
        let wav = stereo_tone(&dir);
        let cache = dir.join("cache").join("tone.bin");

        let first = pyramid_with_cache(&wav, Some(&cache)).expect("decode");
        assert!(cache.is_file(), "the pyramid was written");
        // A hit must not decode: make the source unreadable-by-ffmpeg and ask again.
        let moved = dir.join("moved.wav");
        std::fs::rename(&wav, &moved).unwrap();
        let second = pyramid_with_cache(&wav, Some(&cache)).expect("served from cache without the source");
        assert_eq!(first, second);
        std::fs::rename(&moved, &wav).unwrap();

        // Truncate the cache: it is recomputed and rewritten whole.
        let bytes = std::fs::read(&cache).unwrap();
        std::fs::write(&cache, &bytes[..bytes.len() / 2]).unwrap();
        let third = pyramid_with_cache(&wav, Some(&cache)).expect("recomputed after damage");
        assert_eq!(first, third);
        assert_eq!(std::fs::read(&cache).unwrap(), bytes, "the damaged file was replaced");
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored channel_counts`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn channel_count_and_sample_rate_come_from_the_source() {
        let dir = Scratch::new("channels");

        // Mono at 44.1 kHz: one lane, and the resample to 48 kHz keeps the length.
        let mono = dir.join("mono.wav");
        synth(
            &mono,
            &["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100:duration=2"],
        );
        let p = pyramid_with_cache(&mono, None).expect("mono pyramid");
        assert_eq!(p.channels, 1);
        assert!((p.duration() - 2.0).abs() < 0.01, "duration {}", p.duration());
        let r = waveform_range(&p, 0.5, 1.5, 10);
        assert_eq!((r.min.len(), r.max.len()), (1, 1));
        assert!(
            r.max[0].iter().all(|v| *v > 0.1),
            "a sine is audible everywhere: {:?}",
            r.max[0]
        );

        // Six channels are downmixed to stereo, not rejected.
        let surround = dir.join("surround.wav");
        synth(
            &surround,
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-ac",
                "6",
                "-c:a",
                "pcm_s16le",
            ],
        );
        assert_eq!(pyramid_with_cache(&surround, None).expect("5.1 pyramid").channels, 2);

        // No audio stream is an error naming the problem, not an ffmpeg usage dump.
        let silent_video = dir.join("video.mp4");
        synth(
            &silent_video,
            &[
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:r=10:d=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ],
        );
        let err = pyramid_with_cache(&silent_video, None).expect_err("no audio").to_string();
        assert!(err.contains("no audio stream"), "{err}");

        // A file that does not exist fails the same way instead of hanging.
        assert!(pyramid_with_cache(&dir.join("absent.wav"), None).is_err());
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored concurrent_requests`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn concurrent_requests_for_one_file_share_one_pyramid() {
        let dir = Scratch::new("shared");
        let wav = stereo_tone(&dir);
        let pyramids: Vec<Arc<WaveformPyramid>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..6).map(|_| s.spawn(|| shared_pyramid(&wav).expect("pyramid"))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        // One load, handed to everyone: a second decode would be a second Arc.
        assert!(pyramids.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
        // And the memoized copy answers a range without touching the file again.
        let r = waveform_range_of(&wav, 0.0, 3.0, 30).expect("range");
        assert_eq!((r.channels, r.buckets), (2, 30));
    }
}
