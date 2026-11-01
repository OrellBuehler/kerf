//! Text as the sequence of bitmaps `drawtext` blends, recovered from FFmpeg itself.
//!
//! `drawtext` does not blend a picture of the text: it blends the **bitmap of each glyph in turn**
//! (a synthetic-bold border first, every glyph's, and then every glyph's fill), and the blend is
//! rounding arithmetic on the destination, so where two bitmaps share a pixel — or a 2x2 block of a
//! 4:2:0 chroma plane, where each bitmap's own average is blended — the result is not a function of
//! the sum of their coverage. One mask for the whole text (what an earlier version of the compositor
//! blended, and still does where this module does not know the build's layout) is exact only while no
//! two bitmaps meet: never along the letter gaps of a thickened bold, never in 3 to 11 px text.
//!
//! The bitmaps are FFmpeg's, and so are their places; this module only finds which is which. For a
//! piece of text it asks FFmpeg for
//!
//! * **its measure** (cached per text, font and size): the pen position of every glyph, to 1/64 of a
//!   pixel, and the box `drawtext` gives the whole ([`measure`]);
//! * **its dictionary** (cached per glyph, font and size): the bitmap of each glyph unit and of its
//!   border, drawn alone at each of the four quarter-pixel phases a position can land on, and where
//!   each lands against its pen ([`dictionary`]);
//! * **the picture of the text** as `drawtext` leaves it on black ([`super::rasterize`]), which is
//!   what the assembled sequence has to reproduce **exactly** or the text is refused.
//!
//! and lays the dictionary's bitmaps at the pen positions in `drawtext`'s order. Nothing in the layout
//! is trusted: a sequence is returned only if blending it onto black gives, sample for sample, the
//! picture FFmpeg drew for the same overlay at the same place.
//!
//! **Two layouts**, told apart by what the build says about itself ([`build`]):
//!
//! * [`Model::Shaped`] (HarfBuzz; measured on 6.1.1 and 9.0.2, which the Windows and macOS bundles ship): glyphs are
//!   shaped per line, positions are 26.6 fixed point and a glyph's bitmap is rendered at the quarter pixel its
//!   position falls on. The debug log of a multi-line `drawtext` gives the shaped width of every prefix of the text
//!   and the advance of its last glyph, which is each glyph's pen; the baseline is `(int)(y * 64 + ascender)`, and
//!   the ascender's fraction is not told, so the (at most five) quarter-pixel phases it can be are tried
//!   against the picture. Ligatures are units of several characters.
//! * [`Model::Legacy`] (no shaper; measured on 4.4.2): one glyph per character, whole pixels, kerning from the `kern`
//!   table; the box of every prefix of the text gives each glyph's pen.
//!
//! and one of two **colour conversions** ([`ColourModel`]; 4.4.2's is fixed point), told by the same probe.
//!
//! A build that is none of these (or a text this does not understand: tabs, glyphs that shape into something
//! other than one glyph per unit, a glyph whose placement is not its pen (a GPOS offset), a font whose bitmaps touch
//! the edge of their cell, a run that timed out) is not handled: the caller refuses the frame, and FFmpeg draws it.
//! **Nothing is returned that was not proven.**

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::{PlanText, TextPiece};

use super::{Bitmap, ColourModel, RASTER_TIMEOUT};
use crate::gpu::GpuError;
use crate::source::{run_child_logged, RunError};
use kerf_core::YuvMatrix;

/// How this build of FFmpeg lays `drawtext` text out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// HarfBuzz shaping, 26.6 positions, glyph bitmaps rendered at quarter-pixel phases.
    Shaped,
    /// Whole-pixel pens, a bitmap per character.
    Legacy,
}

/// The most debug log read from a measuring run: a few dozen lines of ours among FFmpeg's own.
const LOG_CAP: usize = 4 << 20;

/// What a graph that must stay in argv may be (one with a line feed in a quoted value: the shaped measure;
/// the rest go through a script file when they are long, [`kerf_core::spill_long_graph`]): the 32 767
/// characters a Windows command line allows, less the rest of the arguments.
const MAX_GRAPH: usize = 24 << 10;

/// The most instances of `drawtext` one run holds (two per character of the longest text, and the box).
const MAX_INSTANCES: usize = 2 * kerf_core::MAX_SEQUENCE_CHARS + 8;

/// The most bytes one dictionary run may read back.
const MAX_DICTIONARY_BYTES: usize = 48 << 20;

/// How long a run of `ffmpeg` may take besides the [`RASTER_TIMEOUT`] it always has: per instance of `drawtext`
/// in its graph (each opens the font), up to [`MAX_RUN`]. A run that is still going then is a refusal.
const PER_INSTANCE: Duration = Duration::from_millis(25);

/// The longest a run of `ffmpeg` is waited for.
const MAX_RUN: Duration = Duration::from_secs(20);

/// The margin of the invisible box a dictionary unit is drawn in, in pixels: the clip window of a
/// shaped `drawtext` is the text's box grown by it.
const WINDOW_MARGIN: u32 = 24;

/// The most glyph units one dictionary run draws.
const UNITS_PER_RUN: usize = 400;

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unsupported(why: impl Into<String>) -> GpuError {
    GpuError::Unsupported(why.into())
}

// ---- which build -------------------------------------------------------------------

/// What this process's FFmpeg does with text that the compositor has to match: how `drawtext` lays it out
/// and how it converts its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Build {
    pub layout: Model,
    pub colour: ColourModel,
}

/// How long a probe that could not tell is believed.
const RETRY_AFTER: Duration = Duration::from_secs(30);

static ANSWER: Mutex<Option<(Instant, Option<Build>)>> = Mutex::new(None);

/// The layout and colour conversion of this process's FFmpeg, probed once (a few runs of a few
/// milliseconds: the debug log of `drawtext`, and boxes of colours that the two conversions give different
/// planes for); `None` when it is neither layout, or converts colours in neither way, or the probe could not
/// run (retried after [`RETRY_AFTER`]): no text is drawn on such a build. Blocks on the first call: callers
/// are on blocking threads, off the render lock ([`crate::prepare`] asks ahead of the frame that needs it).
pub fn build() -> Option<Build> {
    {
        let held = locked(&ANSWER);
        if let Some((at, answer)) = *held {
            if answer.is_some() || at.elapsed() < RETRY_AFTER {
                return answer;
            }
        }
    }
    let answer = probe_build();
    *locked(&ANSWER) = Some((Instant::now(), answer));
    answer
}

/// [`build`]'s layout.
pub fn model() -> Option<Model> {
    build().map(|b| b.layout)
}

/// What is known without running anything: the probed build, else `None` (not probed yet, or a probe that
/// could not tell). What a plan's caps are made from ([`crate::Compositor::caps`]): a plan does not wait for a probe.
pub fn measured_build() -> Option<Build> {
    locked(&ANSWER).and_then(|(_, answer)| answer)
}

/// [`measured_build`]'s layout.
pub fn measured_model() -> Option<Model> {
    measured_build().map(|b| b.layout)
}

/// A text for the probes to hang their `drawtext`s on.
fn sample_text(font: std::path::PathBuf) -> PlanText {
    PlanText {
        id: uuid::Uuid::nil(),
        text: "ab".into(),
        size: 0.3,
        pos: (0.0, 0.0),
        alpha: 1.0,
        color: SHOWN.into(),
        bg: None,
        font_file: Some(font),
        synthetic_bold: false,
    }
}

fn probe_build() -> Option<Build> {
    let font = kerf_core::fonts::default_font_file(false)?;
    let sample = sample_text(font);
    Some(Build {
        layout: probe_layout(&sample)?,
        colour: probe_colour(&sample)?,
    })
}

fn probe_layout(sample: &PlanText) -> Option<Model> {
    let filter = kerf_core::text_piece_filter(
        sample,
        64,
        &TextPiece {
            text: "ab",
            lines: false,
            font: SHOWN,
            border: None,
            boxed: None,
        },
        &["x=0", "y=0"],
    );
    let graph = format!("color=c=black:s=64x64:r=1:d=1,format=rgb24,{filter}[o]");
    let (_, log) = run_graph(&graph, &[("o", None)], true, 1, 1 << 10, 1, false).ok()?;
    if log
        .lines()
        .any(|l| l.contains("Line:") && l.contains("width64:") && l.contains("glyphs count:"))
    {
        Some(Model::Shaped)
    } else if log.lines().any(|l| l.contains("text_w:") && l.contains("text_h:")) {
        Some(Model::Legacy)
    } else {
        None
    }
}

/// The colours the probe draws: the named colours the two conversions give different planes for, then
/// colours from a fixed pseudo-random walk of RGB where they differ, fourteen in all.
pub(super) fn probe_colours() -> Vec<[u8; 3]> {
    let differs = |rgb: &[u8; 3]| super::draw_color(*rgb, YuvMatrix::Bt601) != super::draw_color_ccir(*rgb);
    let mut out: Vec<[u8; 3]> = super::COLOR_NAMES.iter().map(|(_, rgb)| *rgb).filter(differs).collect();
    let mut state = 0x2545_f491_u32;
    while out.len() < PROBE_COLOURS {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let rgb = [(state >> 24) as u8, (state >> 16) as u8, (state >> 8) as u8];
        if differs(&rgb) && !out.contains(&rgb) {
            out.push(rgb);
        }
    }
    out.truncate(PROBE_COLOURS);
    out
}

/// How many colours the colour probe draws.
const PROBE_COLOURS: usize = 14;

/// Which conversion `drawtext` makes of a colour, told by drawing boxes in colours the two disagree on
/// ([`draw_color`] against [`draw_color_ccir`]: a level off in Y for a few of the named colours and one in
/// twenty of the rest) and comparing the planes with what each conversion predicts for the blend of the box over the canvas. A build
/// that matches neither is not one text is drawn on.
fn probe_colour(sample: &PlanText) -> Option<ColourModel> {
    const TILE: usize = 96;
    let asked: Vec<([u8; 3], String)> = probe_colours()
        .into_iter()
        .map(|rgb| (rgb, format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])))
        .collect();
    let n = asked.len();
    let outs: String = (0..n).map(|i| format!("[c{i}]")).collect();
    let branches: Vec<String> = asked
        .iter()
        .enumerate()
        .map(|(i, (_, hex))| {
            let filter = kerf_core::text_piece_filter(
                sample,
                TILE as u32,
                &TextPiece {
                    text: "x",
                    lines: false,
                    font: HIDDEN,
                    border: None,
                    boxed: Some((hex, 20)),
                },
                &["x=22", "y=22"],
            );
            format!("[c{i}]{filter}[t{i}]")
        })
        .collect();
    let stacked: String = (0..n).map(|i| format!("[t{i}]")).collect();
    let graph = format!(
        "color=c=black:s={TILE}x{TILE}:r=1:d=1,format=yuv420p,split={n}{outs};{};{stacked}vstack=inputs={n}[o]",
        branches.join(";")
    );
    let (planes, _) = run_graph_pix(&graph, "yuv420p", n * TILE * TILE * 3 / 2, n).ok()?;
    let (ys, cs) = (n * TILE * TILE, n * TILE * TILE / 4);
    if planes.len() != ys + 2 * cs {
        return None;
    }
    // A tile's untouched corner is the canvas the box was blended over, a point inside the box the result.
    let at = |plane: usize, i: usize, x: usize, y: usize| match plane {
        0 => planes[(i * TILE + y) * TILE + x],
        p => planes[ys + (p - 1) * cs + (i * TILE / 2 + y / 2) * (TILE / 2) + x / 2],
    };
    // The box is blended in with `ff_blend_rectangle`'s alpha for an opaque colour.
    let alpha = 0x10203 * 255 + 2;
    let matches = |model: ColourModel| {
        asked.iter().enumerate().all(|(i, (rgb, _))| {
            let Some(want) = super::draw_color_with(model, *rgb, YuvMatrix::Bt601) else {
                return false;
            };
            (0..3).all(|p| {
                let before = at(p, i, TILE - 2, TILE - 2);
                at(p, i, 24, 24) == super::mix(before, alpha, want[p])
            })
        })
    };
    // Float first: it is the one the builds that draw text for most users have.
    if matches(ColourModel::Float) {
        Some(ColourModel::Float)
    } else if matches(ColourModel::Ccir) {
        Some(ColourModel::Ccir)
    } else {
        None
    }
}

// ---- small helpers -----------------------------------------------------------------

/// Characters that have no bitmap of their own: only their advance counts.
fn blank(c: char) -> bool {
    c.is_whitespace() || c.is_control()
}

/// The bounding box `(x0, y0, x1, y1)` (end exclusive) of the non-zero samples of a `w` x `h` plane.
fn bbox(plane: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut found: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        let row = &plane[y * w..(y + 1) * w];
        let (Some(first), Some(last)) = (row.iter().position(|v| *v != 0), row.iter().rposition(|v| *v != 0)) else {
            continue;
        };
        found = Some(match found {
            None => (first, y, last + 1, y + 1),
            Some((x0, y0, x1, _)) => (x0.min(first), y0, x1.max(last + 1), y + 1),
        });
    }
    found
}

/// What a measuring or dictionary run is keyed by, besides what it draws: the build and the font.
fn run_key(t: &PlanText) -> String {
    let font = t.font_file.as_deref();
    let id = font
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| {
            let modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{modified}", m.len())
        })
        .unwrap_or_default();
    format!(
        "{}|{}|{id}|{}",
        kerf_core::ffmpeg_path(),
        font.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        u8::from(t.synthetic_bold)
    )
}

/// A cache of `V`s, least recently used out first.
struct Lru<V> {
    map: HashMap<String, (u64, usize, Arc<V>)>,
    clock: u64,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl<V> Lru<V> {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            map: HashMap::new(),
            clock: 0,
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<V>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(key).map(|(used, _, v)| {
            *used = clock;
            Arc::clone(v)
        })
    }

    fn put(&mut self, key: String, cost: usize, v: Arc<V>) {
        self.clock += 1;
        if let Some((_, old, _)) = self.map.insert(key, (self.clock, cost, v)) {
            self.bytes = self.bytes.saturating_sub(old);
        }
        self.bytes += cost;
        while self.map.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some(victim) = self.map.iter().min_by_key(|(_, (used, _, _))| *used).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some((_, cost, _)) = self.map.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(cost);
            }
        }
    }
}

// ---- the measure -------------------------------------------------------------------

/// One glyph unit of a text: the characters that shape to one glyph (a ligature is several), and where
/// its pen is, in 1/64 pixels from the start of the line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    text: String,
    pen64: i32,
}

/// What FFmpeg says about laying a text out.
#[derive(Debug)]
struct Measure {
    /// `drawtext`'s `text_w` and `text_h` for the whole text.
    text_w: i32,
    text_h: i32,
    /// `drawtext`'s `ascent`: the baseline's distance below the top of the text, whole pixels.
    ascent: i32,
    units: Vec<Unit>,
}

fn measures() -> &'static Mutex<Lru<Measure>> {
    static M: OnceLock<Mutex<Lru<Measure>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Lru::new(512, 4 << 20)))
}

/// The pixel size of the font an overlay is drawn in on a frame `frame_h` high.
pub(super) fn font_px(t: &PlanText, frame_h: u32) -> u32 {
    (f64::from(frame_h) * t.size).round().max(1.0) as u32
}

/// A canvas a text of `n` characters at `px` fits on, for measuring ([`kerf_core::text_sequence_limit`] has
/// refused the text that does not fit [`kerf_core::MAX_MEASURE_SIDE`]).
fn probe_canvas(px: u32, n: usize) -> Result<(u32, u32), GpuError> {
    let w = (f64::from(px) * 1.8 * n as f64).ceil() + 64.0;
    let h = f64::from(px) * 3.0 + 64.0;
    if w > kerf_core::MAX_MEASURE_SIDE || h > kerf_core::MAX_MEASURE_SIDE {
        return Err(unsupported(format!(
            "a {px} px text of {n} characters is too large to measure"
        )));
    }
    Ok((w as u32, h as u32))
}

const SHOWN: &str = "white";
const HIDDEN: &str = "white@0";

/// Run `graph` and read its outputs, `(label, pix_fmt)`: a raw picture on the pipe in that format, or
/// (`None`) nothing. `debug` asks for the debug log, which a measure reads; `frames` is how many frames each
/// output takes; `cap` how many bytes may come back; `work` how many instances of `drawtext` the graph
/// holds, which sets how long it is waited for; `spill` moves a graph too long for argv into a script
/// file (not one with a line feed in a quoted value). A run that does not finish, or writes more than
/// `cap`, is `GpuError::Unsupported`: FFmpeg draws that frame.
fn run_graph(
    graph: &str,
    outputs: &[(&str, Option<&str>)],
    debug: bool,
    frames: usize,
    cap: usize,
    work: usize,
    spill: bool,
) -> Result<(Vec<u8>, String), GpuError> {
    let timeout = (RASTER_TIMEOUT + PER_INSTANCE * work.min(MAX_INSTANCES) as u32).min(MAX_RUN);
    run_graph_within(timeout, graph, outputs, debug, frames, cap, spill)
}

fn run_graph_within(
    timeout: Duration,
    graph: &str,
    outputs: &[(&str, Option<&str>)],
    debug: bool,
    frames: usize,
    cap: usize,
    spill: bool,
) -> Result<(Vec<u8>, String), GpuError> {
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-nostdin", "-loglevel"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    args.push(if debug { "debug" } else { "error" }.into());
    args.push("-filter_complex".into());
    args.push(graph.to_string());
    for (label, pix_fmt) in outputs {
        args.push("-map".into());
        args.push(format!("[{label}]"));
        args.push("-frames:v".into());
        args.push(frames.to_string());
        match pix_fmt {
            Some(fmt) => args.extend(["-f", "rawvideo", "-pix_fmt", fmt, "pipe:1"].map(String::from)),
            None => args.extend(["-f", "null", "-"].map(String::from)),
        }
    }
    kerf_core::limit_frame_args(&mut args, 1);
    let _script = if spill {
        Some(
            kerf_core::spill_long_graph(&mut args, "text")
                .map_err(|e| unsupported(format!("could not write the text's graph: {e}")))?,
        )
    } else {
        None
    };
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(args.iter());
    let (out, log) = run_child_logged(cmd, cap, LOG_CAP, timeout).map_err(|e| match e {
        RunError::Overflow => unsupported("a text run wrote more than its canvas"),
        RunError::Failed(why) => unsupported(format!("FFmpeg could not measure the text: {why}")),
    })?;
    Ok((out, String::from_utf8_lossy(&log).into_owned()))
}

/// [`run_graph`] for one picture out, `[o]`, in `pix_fmt`, over `frames` frames.
fn run_graph_pix(graph: &str, pix_fmt: &str, cap: usize, frames: usize) -> Result<(Vec<u8>, String), GpuError> {
    let _ = frames;
    run_graph(graph, &[("o", Some(pix_fmt))], false, 1, cap, 16, false)
}

/// The number after `key` in `line`.
fn field(line: &str, key: &str) -> Option<i32> {
    let rest = line.split(key).nth(1)?.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn measure(t: &PlanText, frame_h: u32, chars: &[char], model: Model) -> Result<Arc<Measure>, GpuError> {
    let drawn: String = chars.iter().collect();
    let key = format!("{}|{}|{drawn}", run_key(t), font_px(t, frame_h));
    if let Some(m) = locked(measures()).get(&key) {
        return Ok(m);
    }
    let failed = format!("measure|{key}");
    if let Some(e) = remembered(&failed) {
        return Err(e);
    }
    let made = match model {
        Model::Shaped => measure_shaped(t, frame_h, chars),
        Model::Legacy => measure_legacy(t, frame_h, chars),
    };
    let made = match made {
        Ok(m) => Arc::new(m),
        Err(e) => {
            remember(failed, &e);
            return Err(e);
        }
    };
    locked(measures()).put(key, std::mem::size_of::<Measure>() + made.units.len() * 48, Arc::clone(&made));
    Ok(made)
}

/// How long a run that failed is not tried again: a font on a slow disk, a machine under load, a text no
/// measure can take apart. Every frame of a title would otherwise wait out the same timeout, under the
/// render lock.
const FAILURE_MEMORY: Duration = Duration::from_secs(30);

fn failures() -> &'static Mutex<HashMap<String, (Instant, String)>> {
    static F: OnceLock<Mutex<HashMap<String, (Instant, String)>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// The refusal a run earned less than `memory` ago, if it did.
fn remembered_within(key: &str, memory: Duration) -> Option<GpuError> {
    let held = locked(failures());
    let (at, why) = held.get(key)?;
    (at.elapsed() < memory).then(|| unsupported(format!("{why} (not tried again yet)")))
}

/// Whether a refusal was earned lately for `key` (anything a caller would otherwise run FFmpeg to find out).
pub(super) fn remembered(key: &str) -> Option<GpuError> {
    remembered_within(key, FAILURE_MEMORY)
}

/// Remember the refusal `e` for `key`, so that it is not run again for a while. Only refusals are remembered: an
/// error that is not one (a decode failing, a lost device) is not a verdict on the text.
pub(super) fn remember(key: String, e: &GpuError) {
    let why = match e {
        GpuError::Unsupported(why) => why.clone(),
        other => other.to_string(),
    };
    let mut held = locked(failures());
    if held.len() >= 256 {
        held.retain(|_, (at, _)| at.elapsed() < FAILURE_MEMORY);
        if held.len() >= 256 {
            held.clear();
        }
    }
    held.insert(key, (Instant::now(), why));
}

/// The box `drawtext` draws behind the whole text with its top left at `(0, ascent)`: the text's
/// width and height, and `ascent`, read off the picture `plane` (`w` x `h`).
fn read_box(plane: &[u8], w: usize, h: usize) -> Result<(i32, i32, i32), GpuError> {
    let (x0, y0, x1, y1) = bbox(plane, w, h).ok_or_else(|| unsupported("the text has no extent to measure"))?;
    if x0 != 0 || x1 >= w || y1 >= h {
        return Err(unsupported("the text's box did not fit its measuring canvas"));
    }
    Ok(((x1 - x0) as i32, (y1 - y0) as i32, y0 as i32))
}

/// `y=ascent`, the top of a measuring box.
const AT_ASCENT: &str = "x=0:y=ascent";

fn measure_shaped(t: &PlanText, frame_h: u32, chars: &[char]) -> Result<Measure, GpuError> {
    let n = chars.len();
    let (pw, ph) = probe_canvas(font_px(t, frame_h), n)?;
    let prefixes: Vec<String> = (1..=n).map(|k| chars[..k].iter().collect()).collect();
    let lines = prefixes.join("\n");
    let by_lines = kerf_core::text_piece_filter(
        t,
        frame_h,
        &TextPiece {
            text: &lines,
            lines: true,
            font: SHOWN,
            border: None,
            boxed: None,
        },
        &["x=0", "y=0"],
    );
    let boxed = kerf_core::text_piece_filter(
        t,
        frame_h,
        &TextPiece {
            text: &chars.iter().collect::<String>(),
            lines: false,
            font: HIDDEN,
            border: None,
            boxed: Some((SHOWN, 0)),
        },
        &[AT_ASCENT],
    );
    let graph = format!(
        "color=c=black:s={pw}x{ph}:r=1:d=1,format=rgb24,split=2[m0][m1];[m0]{by_lines}[l];[m1]{boxed},extractplanes=r[b]"
    );
    if graph.len() > MAX_GRAPH {
        return Err(unsupported("a text too long to measure"));
    }
    let (plane, log) = run_graph(
        &graph,
        &[("l", None), ("b", Some("gray"))],
        true,
        1,
        (pw * ph) as usize,
        2,
        false,
    )?;
    if plane.len() != (pw * ph) as usize {
        return Err(unsupported("the text's box came back at the wrong size"));
    }
    let (text_w, text_h, ascent) = read_box(&plane, pw as usize, ph as usize)?;
    let rows = shaped_rows(&log, n)?;
    let units = units_of(chars, &rows)?;
    if (rows[n - 1].1 + 63) / 64 != text_w {
        return Err(unsupported("the text's width and its box disagree"));
    }
    Ok(Measure {
        text_w,
        text_h,
        ascent,
        units,
    })
}

/// The log lines of the shaper for a text of `n` prefixes, `(glyph count, width64, last advance64)` each:
/// `Line: k -- glyphs count: g - width64: w - offset_left64: l - offset_right64: r)`. The lines are told
/// apart by what they say and not by whose they are (a build prints `[drawtext @ 0x…]` whatever the
/// instance is called, another `[drawtext@name @ 0x…]`): each `drawtext` that draws logs its lines, from
/// `Line: 0` on, together, so the log is cut at every `Line: 0`, and the group of `n` lines numbered
/// `0..n` is the multi-line instance's. (The box probe logs a group of its own: one line, the whole
/// text, which for `n = 1` is the same line.) No such group, or two that differ, is a refusal.
fn shaped_rows(log: &str, n: usize) -> Result<Vec<(i32, i32, i32)>, GpuError> {
    let mut groups: Vec<Vec<(i32, i32, i32)>> = Vec::new();
    let mut next = 0;
    for l in log.lines().filter(|l| l.contains("width64:")) {
        let (Some(k), Some(g), Some(w), Some(r)) = (
            field(l, "Line:"),
            field(l, "glyphs count:"),
            field(l, "width64:"),
            field(l, "offset_right64:"),
        ) else {
            continue;
        };
        if k == 0 {
            groups.push(Vec::new());
            next = 0;
        }
        if k != next {
            // A line out of order: that group is not one `drawtext` drew.
            next = -1;
        }
        if let Some(group) = groups.last_mut() {
            if next >= 0 {
                group.push((g, w, r));
                next += 1;
            }
        }
    }
    let mut whole = groups.into_iter().filter(|g| g.len() == n);
    let rows = whole.next().ok_or_else(|| {
        unsupported(format!(
            "FFmpeg logged no group of {n} lines for the {n} prefixes of the text"
        ))
    })?;
    if whole.any(|other| other != rows) {
        return Err(unsupported("FFmpeg logged two different shapings of the text"));
    }
    Ok(rows)
}

/// The glyph units of `chars` from the shaper's log, one `(glyph count, width64, last advance64)` per
/// prefix of the text. A unit starts at each character the glyph count grows at (by one); the others (the
/// second `f` of a ligature) join the unit before. The pen of a unit is where its last glyph starts in the
/// line that ends with it: that line's width less that glyph's advance.
fn units_of(chars: &[char], rows: &[(i32, i32, i32)]) -> Result<Vec<Unit>, GpuError> {
    let n = chars.len();
    let mut starts: Vec<usize> = Vec::new();
    let mut previous = 0;
    for (k, (glyphs, _, _)) in rows.iter().enumerate() {
        match glyphs - previous {
            1 => starts.push(k),
            0 if k > 0 => {}
            _ => return Err(unsupported("the text shapes into glyphs that are not one per unit")),
        }
        previous = *glyphs;
    }
    Ok(starts
        .iter()
        .enumerate()
        .map(|(i, start)| {
            let end = starts.get(i + 1).copied().unwrap_or(n);
            let (_, width64, last) = rows[end - 1];
            Unit {
                text: chars[*start..end].iter().collect(),
                pen64: width64 - last,
            }
        })
        .collect())
}

fn measure_legacy(t: &PlanText, frame_h: u32, chars: &[char]) -> Result<Measure, GpuError> {
    let n = chars.len();
    let (pw, ph) = probe_canvas(font_px(t, frame_h), n)?;
    let full: String = chars.iter().collect();
    let box_of = |text: &str, at: &str| {
        kerf_core::text_piece_filter(
            t,
            frame_h,
            &TextPiece {
                text,
                lines: false,
                font: HIDDEN,
                border: None,
                boxed: Some((SHOWN, 0)),
            },
            &[at],
        )
    };
    // The widths of the prefix ending at each visible character and of that character alone, in
    // one-row boxes: the pen of a character is the first less the second.
    let mut asked: Vec<String> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut ask = |s: String| -> usize {
        *index.entry(s.clone()).or_insert_with(|| {
            asked.push(s);
            asked.len() - 1
        })
    };
    let wanted: Vec<(usize, usize, usize)> = (0..n)
        .filter(|k| !blank(chars[*k]))
        .map(|k| (k, ask(chars[..=k].iter().collect()), ask(chars[k].to_string())))
        .collect();
    // One run holds every row and the whole text's box at `ascent` (its width and height, and the baseline):
    // a graph of an instance of `drawtext` per row, which goes through a script file when it is long.
    if asked.is_empty() {
        return Err(unsupported("a text with nothing to measure"));
    }
    let rows = asked.len().max(2);
    let inputs = rows + 1;
    let outs: String = (0..inputs).map(|i| format!("[m{i}]")).collect();
    let mut branches: Vec<String> = (0..rows)
        .map(|i| {
            let text = &asked[i.min(asked.len().saturating_sub(1))];
            format!("[m{i}]{},crop={pw}:1:0:0,extractplanes=r[r{i}]", box_of(text, "x=0:y=0"))
        })
        .collect();
    branches.push(format!("[m{rows}]{},extractplanes=r[r{rows}]", box_of(&full, AT_ASCENT)));
    let stacked: String = (0..inputs).map(|i| format!("[r{i}]")).collect();
    let graph = format!(
        "color=c=black:s={pw}x{ph}:r=1:d=1,format=rgb24,split={inputs}{outs};{};{stacked}vstack=inputs={inputs}[o]",
        branches.join(";")
    );
    let want = pw as usize * (rows + ph as usize);
    let (out, _) = run_graph(&graph, &[("o", Some("gray"))], false, 1, want, inputs, true)?;
    if out.len() != want {
        return Err(unsupported("the text's widths came back short"));
    }
    let mut widths: Vec<i32> = Vec::with_capacity(asked.len());
    for row in out.chunks_exact(pw as usize).take(asked.len()) {
        let w = row.iter().take_while(|v| **v != 0).count();
        if row[w..].iter().any(|v| *v != 0) {
            return Err(unsupported("a text's box was not one piece"));
        }
        widths.push(w as i32);
    }
    let (text_w, text_h, ascent) = read_box(&out[pw as usize * rows..], pw as usize, ph as usize)?;
    let mut pens: HashMap<usize, i32> = HashMap::new();
    for (k, prefix, own) in &wanted {
        let (w, a) = (widths[*prefix], widths[*own]);
        if w == 0 || a == 0 || w < a {
            return Err(unsupported("a character of the text has no width to measure"));
        }
        pens.insert(*k, (w - a) * 64);
    }
    let units = chars
        .iter()
        .enumerate()
        .map(|(k, c)| Unit {
            text: c.to_string(),
            pen64: pens.get(&k).copied().unwrap_or(0),
        })
        .collect();
    Ok(Measure {
        text_w,
        text_h,
        ascent,
        units,
    })
}

// ---- the dictionary -------------------------------------------------------------------

/// One bitmap of the dictionary: its non-zero part and where that lies against the pen (`dx`, to the
/// right of the whole-pixel pen position) and the baseline (`dy`, below the whole-pixel baseline the
/// phase gives), in pixels.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Shape {
    dx: i32,
    dy: i32,
    w: u32,
    h: u32,
    data: Vec<u8>,
}

/// A glyph unit's bitmaps at each of the four quarter-pixel phases of its pen, for the fill and (with a
/// synthetic-bold border) the border.
#[derive(Debug, Clone, Default)]
struct Entry {
    fill: [Shape; 4],
    border: Option<[Shape; 4]>,
}

impl Entry {
    fn bytes(&self) -> usize {
        let one = |s: &[Shape; 4]| s.iter().map(|s| s.data.len() + 64).sum::<usize>();
        one(&self.fill) + self.border.as_ref().map_or(0, one) + 128
    }
}

fn dictionaries() -> &'static Mutex<Lru<Entry>> {
    static D: OnceLock<Mutex<Lru<Entry>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(Lru::new(4096, 24 << 20)))
}

/// Where a dictionary run draws: the pen's whole pixel and the baseline's, in a cell.
struct Cell {
    w: u32,
    h: u32,
    x0: i32,
    y0: i32,
}

fn cell(px: u32, chars: usize) -> Cell {
    // Room for what a face puts outside its em box: a script's swashes reach a whole em to the left of the
    // pen and a half above the ascent, its descenders a half below; a bitmap that still touches the edge of
    // its cell is refused ([`shape_of`]).
    let s = f64::from(px);
    let x0 = s.ceil() as i32 + 8;
    let y0 = (s * 1.8).ceil() as i32 + 8;
    Cell {
        w: (f64::from(x0) + s * 1.6 * chars as f64).ceil() as u32 + 8,
        h: (f64::from(y0) + s).ceil() as u32 + 8,
        x0,
        y0,
    }
}

/// The dictionary entries for `units` (distinct, not blank) at the vertical phase `qy`, drawn by FFmpeg
/// or served from the cache.
fn dictionary(
    t: &PlanText,
    frame_h: u32,
    model: Model,
    qy: i32,
    units: &[String],
) -> Result<HashMap<String, Arc<Entry>>, GpuError> {
    let px = font_px(t, frame_h);
    let base = format!("{}|{px}|{}", run_key(t), if model == Model::Shaped { qy } else { 0 });
    let mut found: HashMap<String, Arc<Entry>> = HashMap::new();
    let mut missing: Vec<&String> = Vec::new();
    for u in units {
        match locked(dictionaries()).get(&format!("{base}|{u}")) {
            Some(e) => {
                found.insert(u.clone(), e);
            }
            None => missing.push(u),
        }
    }
    // As many units to a run as the bytes it reads back allow: a cell is `cell(px, 1)` big (a ligature's is wider).
    let c = cell(px, 1);
    let per_unit =
        c.w as usize * c.h as usize * 2 * (1 + usize::from(t.synthetic_bold)) * if model == Model::Shaped { 4 } else { 1 };
    let per_run = (MAX_DICTIONARY_BYTES / per_unit.max(1)).clamp(1, UNITS_PER_RUN);
    for chunk in missing.chunks(per_run) {
        let failed = format!(
            "dictionary|{base}|{}",
            chunk.iter().map(|u| u.as_str()).collect::<Vec<_>>().join("\u{1}")
        );
        if let Some(e) = remembered(&failed) {
            return Err(e);
        }
        let drawn = match draw_dictionary(t, frame_h, model, qy, chunk) {
            Ok(d) => d,
            Err(e) => {
                remember(failed, &e);
                return Err(e);
            }
        };
        for (u, entry) in chunk.iter().zip(drawn) {
            let entry = Arc::new(entry);
            locked(dictionaries()).put(format!("{base}|{u}"), entry.bytes(), Arc::clone(&entry));
            found.insert((*u).clone(), entry);
        }
    }
    Ok(found)
}

fn draw_dictionary(t: &PlanText, frame_h: u32, model: Model, qy: i32, units: &[&String]) -> Result<Vec<Entry>, GpuError> {
    let px = font_px(t, frame_h);
    let longest = units.iter().map(|u| u.chars().count()).max().unwrap_or(1);
    let c = cell(px, longest);
    let border = t.synthetic_bold;
    let parts: usize = units.len() * if border { 2 } else { 1 };
    let frames: usize = if model == Model::Shaped { 4 } else { 1 };
    // `vstack` wants two inputs at least: a lone part is drawn twice.
    let rows = parts.max(2);
    let frame_bytes = c.w as usize * c.h as usize * rows;
    if frame_bytes * frames > MAX_DICTIONARY_BYTES {
        return Err(unsupported(format!(
            "a {px} px dictionary of {} units is too large",
            units.len()
        )));
    }
    // Shaped: the pen is the same on every line of every cell, `x0` plus a quarter pixel per frame, and
    // the baseline `y0` plus the phase. Legacy: the integer pen `x0`; the baseline `y0` is reached by
    // taking the glyph's own ascent off (`ascent` is the tallest glyph of the text drawn).
    let (x, y, extra): (String, String, &[&str]) = match model {
        Model::Shaped => (
            format!("x='{}+mod(n,4)/4'", c.x0),
            format!("y={}", f64::from(c.y0) + f64::from(qy) / 4.0),
            &["y_align=baseline"],
        ),
        Model::Legacy => (format!("x={}", c.x0), format!("y={}-ascent", c.y0), &[]),
    };
    let mut branches: Vec<String> = Vec::new();
    let mut stacked = String::new();
    let mut k = 0;
    let mut last = String::new();
    for u in units {
        let mut draws = vec![(SHOWN, None)];
        if border {
            draws.push((HIDDEN, Some(SHOWN)));
        }
        for (font, edge) in draws {
            let mut opts = vec![x.as_str(), y.as_str()];
            opts.extend_from_slice(extra);
            let filter = kerf_core::text_piece_filter(
                t,
                frame_h,
                &TextPiece {
                    text: u,
                    lines: false,
                    font,
                    border: edge,
                    // `drawtext` cuts every bitmap to the text's box grown by a pixel or two: a unit's own
                    // bitmap overhangs its advance (an `f`'s hook), and in the middle of a text nothing cuts
                    // it. An invisible box with a wide margin widens the window instead.
                    boxed: (model == Model::Shaped).then_some((HIDDEN, WINDOW_MARGIN)),
                },
                &opts,
            );
            branches.push(format!("[d{k}]{filter},extractplanes=r[e{k}]"));
            stacked.push_str(&format!("[e{k}]"));
            last = filter;
            k += 1;
        }
    }
    if k < rows {
        branches.push(format!("[d{k}]{last},extractplanes=r[e{k}]"));
        stacked.push_str(&format!("[e{k}]"));
        k += 1;
    }
    let outs: String = (0..k).map(|i| format!("[d{i}]")).collect();
    let rate = frames;
    let graph = format!(
        "color=c=black:s={}x{}:r={rate}:d=1,format=rgb24,split={k}{outs};{};{stacked}vstack=inputs={k}[o]",
        c.w,
        c.h,
        branches.join(";")
    );
    let (out, _) = run_graph(&graph, &[("o", Some("gray"))], false, frames, frame_bytes * frames, k, true)?;
    if out.len() != frame_bytes * frames {
        return Err(unsupported(format!(
            "{} bytes of dictionary for {} expected",
            out.len(),
            frame_bytes * frames
        )));
    }
    let cell_len = c.w as usize * c.h as usize;
    let mut entries = Vec::with_capacity(units.len());
    for (i, _) in units.iter().enumerate() {
        let mut entry = Entry::default();
        for part in 0..(if border { 2 } else { 1 }) {
            let mut shapes: [Shape; 4] = Default::default();
            for (q, shape) in shapes.iter_mut().enumerate() {
                let frame = if model == Model::Shaped { q } else { 0 };
                let at = frame * frame_bytes + (i * (if border { 2 } else { 1 }) + part) * cell_len;
                let plane = &out[at..at + cell_len];
                *shape = shape_of(plane, &c, model, qy)?;
            }
            if part == 0 {
                entry.fill = shapes;
            } else {
                entry.border = Some(shapes);
            }
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// The shape of the bitmap in one cell, against the whole-pixel pen and baseline the draw put it at.
fn shape_of(plane: &[u8], c: &Cell, model: Model, qy: i32) -> Result<Shape, GpuError> {
    let (w, h) = (c.w as usize, c.h as usize);
    let Some((x0, y0, x1, y1)) = bbox(plane, w, h) else {
        return Ok(Shape::default());
    };
    if x0 == 0 || y0 == 0 || x1 == w || y1 == h {
        return Err(unsupported("a glyph's bitmap touches the edge of its cell"));
    }
    let mut data = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for row in y0..y1 {
        data.extend_from_slice(&plane[row * w + x0..row * w + x1]);
    }
    // Shaped: the baseline of a phase `qy` is drawn on row `y0 + 1` (rows start at 0 for the whole
    // pixel, and a fraction rounds the bitmap up a row); legacy has no phase.
    let baseline = c.y0 + i32::from(model == Model::Shaped && qy != 0);
    Ok(Shape {
        dx: x0 as i32 - c.x0,
        dy: y0 as i32 - baseline,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
        data,
    })
}

// ---- assembling the sequence ----------------------------------------------------------------

/// Cut `b` to the window `(x0, y0, x1, y1)` (end exclusive); `None` if nothing of it is inside.
fn clip(b: Bitmap, win: (i32, i32, i32, i32)) -> Option<Bitmap> {
    let (x0, y0) = (b.x.max(win.0), b.y.max(win.1));
    let (x1, y1) = ((b.x + b.w as i32).min(win.2), (b.y + b.h as i32).min(win.3));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    if (x0, y0, x1, y1) == (b.x, b.y, b.x + b.w as i32, b.y + b.h as i32) {
        return Some(b);
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut data = Vec::with_capacity((w * h) as usize);
    for row in y0..y1 {
        let from = ((row - b.y) as usize) * b.w as usize + (x0 - b.x) as usize;
        data.extend_from_slice(&b.data[from..from + w as usize]);
    }
    Some(Bitmap {
        x: x0,
        y: y0,
        w,
        h,
        data,
    })
}

/// The picture a sequence leaves on black when each bitmap is blended in turn in white at full
/// opacity (`mask * 0x10203` over `0x1010101`): the red plane `drawtext` leaves on an `rgb24` canvas.
pub(super) fn on_black(sequence: &[Bitmap], w: usize, h: usize) -> Vec<u8> {
    let mut plane = vec![0u8; w * h];
    for b in sequence {
        for row in 0..b.h as usize {
            let y = b.y as usize + row;
            if y >= h {
                break;
            }
            for col in 0..b.w as usize {
                let x = b.x as usize + col;
                if x >= w {
                    break;
                }
                let cov = u64::from(b.data[row * b.w as usize + col]);
                let d = &mut plane[y * w + x];
                *d = super::mix(*d, (cov * 0x10203) as u32, 255);
            }
        }
    }
    plane
}

/// What `drawtext` is given besides the text, as the placement needs it.
struct Where {
    w: i32,
    h: i32,
    x: f64,
    y: f64,
    boxed: bool,
}

/// The ordered bitmaps `drawtext` blends for `t` drawn on a `size` canvas, proven to leave `ink`
/// (the picture FFmpeg drew, `size.0 * size.1` red samples) when blended onto black.
pub(super) fn sequence(t: &PlanText, size: (u32, u32), ink: &[u8]) -> Result<Vec<Bitmap>, GpuError> {
    let model = model().ok_or_else(|| unsupported("this FFmpeg's text layout is not one the compositor knows"))?;
    let drawn = kerf_core::text_as_drawn(&t.text);
    let chars: Vec<char> = drawn.chars().collect();
    if let Some(why) = kerf_core::text_sequence_limit(&t.text, Some(font_px(t, size.1))) {
        return Err(unsupported(why));
    }
    let (w, h) = (size.0 as i32, size.1 as i32);
    if chars.iter().all(|c| blank(*c)) {
        return if ink.iter().all(|v| *v == 0) {
            Ok(Vec::new())
        } else {
            Err(unsupported("blank text drew something"))
        };
    }
    let m = measure(t, size.1, &chars, model)?;
    let at = Where {
        w,
        h,
        // The expressions the graph gives `drawtext`, evaluated in doubles as `av_expr` does.
        x: f64::from(w) * t.pos.0 - f64::from(m.text_w) / 2.0,
        y: f64::from(h) * t.pos.1 - f64::from(m.text_h) / 2.0,
        boxed: t.bg.is_some(),
    };
    let visible: Vec<&Unit> = m.units.iter().filter(|u| !u.text.chars().all(blank)).collect();
    let mut distinct: Vec<String> = Vec::new();
    for u in &visible {
        if !distinct.contains(&u.text) {
            distinct.push(u.text.clone());
        }
    }
    let (wu, hu) = (w as usize, h as usize);
    if ink.len() != wu * hu {
        return Err(unsupported("the text's picture is not the canvas"));
    }
    match model {
        Model::Legacy => {
            let dict = dictionary(t, size.1, model, 0, &distinct)?;
            let seq = assemble_legacy(t, &m, &at, &visible, &dict);
            let got = on_black(&seq, wu, hu);
            if got == ink {
                return Ok(seq);
            }
            tracing::debug!("text: {} samples differ from the picture", differing(&got, ink));
        }
        Model::Shaped => {
            let x64 = (at.x * 64.0) as i32;
            // The baseline is `(int)(y * 64 + offset_top64)`; the top's fraction is not told, only that it
            // lies in the 64 below `ascent` pixels: the candidates are those phases, likeliest first.
            let mut tried: Vec<i32> = Vec::new();
            for back in 0..64 {
                let y64 = (at.y * 64.0 + f64::from(m.ascent * 64 - back)) as i32;
                let q = y64 >> 4;
                if tried.contains(&q) {
                    continue;
                }
                tried.push(q);
                let dict = dictionary(t, size.1, model, q & 3, &distinct)?;
                let seq = assemble_shaped(t, &m, &at, &visible, &dict, x64, y64);
                let got = on_black(&seq, wu, hu);
                if got == ink {
                    return Ok(seq);
                }
                tracing::debug!(
                    "text: baseline {y64}: {} samples differ from the picture",
                    differing(&got, ink)
                );
            }
        }
    }
    Err(unsupported("the text's glyph bitmaps were not reproduced exactly"))
}

fn assemble_legacy(t: &PlanText, m: &Measure, at: &Where, visible: &[&Unit], dict: &HashMap<String, Arc<Entry>>) -> Vec<Bitmap> {
    // `s->x` and `s->y` are ints: the doubles truncated. The baseline is `s->y` plus the ascent.
    let (sx, sy) = (at.x as i32, at.y as i32);
    let baseline = sy + m.ascent;
    let mut out = Vec::new();
    let passes: &[bool] = if t.synthetic_bold { &[true, false] } else { &[false] };
    let canvas = (0, 0, at.w, at.h);
    for border in passes {
        for unit in visible {
            let Some(entry) = dict.get(&unit.text) else {
                continue;
            };
            let shape = if *border {
                match entry.border.as_ref() {
                    Some(s) => &s[0],
                    None => continue,
                }
            } else {
                &entry.fill[0]
            };
            if shape.w == 0 {
                continue;
            }
            let b = Bitmap {
                x: sx + unit.pen64 / 64 + shape.dx,
                y: baseline + shape.dy,
                w: shape.w,
                h: shape.h,
                data: shape.data.clone(),
            };
            if let Some(b) = clip(b, canvas) {
                out.push(b);
            }
        }
    }
    out
}

fn assemble_shaped(
    t: &PlanText,
    m: &Measure,
    at: &Where,
    visible: &[&Unit],
    dict: &HashMap<String, Arc<Entry>>,
    x64: i32,
    y64: i32,
) -> Vec<Bitmap> {
    // `drawtext` clips every bitmap to the text's box grown by `bb`: the box padding, or with no box a
    // pixel more than the border.
    let (rect_x, rect_y) = (at.x as i32, at.y as i32);
    let bb = if at.boxed {
        kerf_core::TEXT_BOX_PX as i32
    } else {
        i32::from(t.synthetic_bold) * kerf_core::TEXT_BORDER_PX as i32 + 1
    };
    let left = rect_x - bb;
    let top = rect_y - bb;
    let right = (rect_x + m.text_w + bb).min(at.w);
    let bottom = (rect_y + m.text_h + bb).min(at.h);
    if left >= at.w || top >= at.h || rect_x + m.text_w + bb <= 0 || rect_y + m.text_h + bb <= 0 {
        return Vec::new();
    }
    let window = (left.max(0), top.max(0), right, bottom);
    let qy = (y64 >> 4) & 3;
    let info_y = (y64 >> 6) + i32::from(qy != 0);
    let mut out = Vec::new();
    let passes: &[bool] = if t.synthetic_bold { &[true, false] } else { &[false] };
    for border in passes {
        for unit in visible {
            let Some(entry) = dict.get(&unit.text) else {
                continue;
            };
            let pen = x64 + unit.pen64;
            let q = ((pen >> 4) & 3) as usize;
            let shape = if *border {
                match entry.border.as_ref() {
                    Some(s) => &s[q],
                    None => continue,
                }
            } else {
                &entry.fill[q]
            };
            if shape.w == 0 {
                continue;
            }
            let b = Bitmap {
                x: (pen >> 6) + shape.dx,
                y: info_y + shape.dy,
                w: shape.w,
                h: shape.h,
                data: shape.data.clone(),
            };
            if let Some(b) = clip(b, window) {
                out.push(b);
            }
        }
    }
    out
}

/// How many samples of `got` are not `want`'s.
fn differing(got: &[u8], want: &[u8]) -> usize {
    got.iter().zip(want).filter(|(a, b)| a != b).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn bitmap(x: i32, y: i32, w: u32, h: u32, v: u8) -> Bitmap {
        Bitmap {
            x,
            y,
            w,
            h,
            data: vec![v; (w * h) as usize],
        }
    }

    #[test]
    fn the_log_lines_of_a_shaper_are_read_by_name() {
        let line = "[drawtext@kerfmeasure @ 0x1]   Line: 3 -- glyphs count: 4 - width64: 6847 - offset_left64: 353 - offset_right64: 1025)";
        assert_eq!(field(line, "glyphs count:"), Some(4));
        assert_eq!(field(line, "width64:"), Some(6847));
        assert_eq!(field(line, "offset_right64:"), Some(1025));
        assert_eq!(field(line, "offset_top64:"), None);
        assert_eq!(field("width64: -12 ", "width64:"), Some(-12));
    }

    #[test]
    fn the_shapers_lines_are_found_whoever_logs_them() {
        let line = |who: &str, k: i32, g: i32, w: i32| {
            format!("[{who} @ 0x55]   Line: {k} -- glyphs count: {g} - width64: {w} - offset_left64: 9 - offset_right64: {w})")
        };
        let prefixes = |who: &str| -> Vec<String> { (0..3).map(|k| line(who, k, k + 1, 100 * (k + 1))).collect() };
        let want = vec![(1, 100, 100), (2, 200, 200), (3, 300, 300)];
        // 9.0.2 names the instance, 6.1.1 does not; the box probe logs a line of its own, before or after.
        for who in ["drawtext@kerfmeasure", "drawtext", "Parsed_drawtext_4"] {
            let own = prefixes(who);
            let boxed = line("Parsed_drawtext_5", 0, 3, 300);
            for log in [
                [own.clone(), vec![boxed.clone()]].concat().join("\n"),
                [vec![boxed.clone()], own.clone()].concat().join("\n"),
                format!("noise\n{}\nmore noise\n{}", own.join("\nxx\n"), boxed),
            ] {
                assert_eq!(shaped_rows(&log, 3).unwrap(), want, "{who}");
            }
        }
        // One prefix: the instance and the box log the same line, and either will do.
        assert_eq!(
            shaped_rows(&[line("drawtext", 0, 1, 77), line("drawtext", 0, 1, 77)].join("\n"), 1).unwrap(),
            vec![(1, 77, 77)]
        );
        // The wrong number of lines, lines out of order, or two shapings that differ: refused, never guessed.
        assert!(shaped_rows(&prefixes("drawtext").join("\n"), 4).is_err());
        assert!(shaped_rows("nothing logged", 1).is_err());
        let out_of_order = [
            line("drawtext", 0, 1, 1),
            line("drawtext", 2, 3, 3),
            line("drawtext", 1, 2, 2),
        ]
        .join("\n");
        assert!(shaped_rows(&out_of_order, 3).is_err());
        let differ = [line("drawtext", 0, 1, 77), line("drawtext", 0, 2, 99)].join("\n");
        assert!(shaped_rows(&differ, 1).is_err());
    }

    #[test]
    fn a_unit_starts_where_the_glyph_count_grows_and_its_pen_is_its_last_glyphs_start() {
        // "off": the second `f` joins the first (a ligature, as Geist shapes it), then an `i`.
        let rows = [(1, 2200, 2200), (2, 3602, 1517), (2, 4708, 2623), (3, 5491, 937)];
        let units = units_of(&chars("offi"), &rows).unwrap();
        assert_eq!(
            units,
            vec![
                Unit {
                    text: "o".into(),
                    pen64: 0
                },
                Unit {
                    text: "ff".into(),
                    pen64: 4708 - 2623
                },
                Unit {
                    text: "i".into(),
                    pen64: 5491 - 937
                },
            ]
        );
        // A character that makes two glyphs, or takes one away, is not a unit this reads.
        assert!(units_of(&chars("ab"), &[(1, 10, 10), (3, 30, 10)]).is_err());
        assert!(units_of(&chars("ab"), &[(2, 10, 10)]).is_err());
        assert!(units_of(&chars("ab"), &[(1, 10, 10), (0, 30, 10)]).is_err());
    }

    #[test]
    fn a_bitmap_is_cut_to_a_window_and_nothing_of_it_inside_is_none() {
        let b = Bitmap {
            x: 2,
            y: 3,
            w: 3,
            h: 2,
            data: vec![1, 2, 3, 4, 5, 6],
        };
        assert_eq!(clip(b.clone(), (0, 0, 10, 10)), Some(b.clone()), "untouched when it fits");
        let cut = clip(b.clone(), (3, 0, 5, 4)).unwrap();
        assert_eq!((cut.x, cut.y, cut.w, cut.h), (3, 3, 2, 1));
        assert_eq!(cut.data, vec![2, 3]);
        assert_eq!(clip(b.clone(), (5, 0, 9, 9)), None);
        assert_eq!(clip(b, (0, 0, 4, 3)), None, "the window ends where its first row starts");
    }

    #[test]
    fn a_picture_on_black_is_the_blend_of_each_bitmap_in_turn() {
        // Two bitmaps of half coverage on the same sample: one blend at 127 each, not one at 254.
        let seq = vec![bitmap(1, 1, 1, 1, 127), bitmap(1, 1, 1, 1, 127), bitmap(0, 0, 1, 1, 255)];
        let plane = on_black(&seq, 3, 3);
        let one = super::super::mix(0, 127 * 0x10203, 255);
        let two = super::super::mix(one, 127 * 0x10203, 255);
        assert_eq!(plane[4], two);
        assert_eq!(plane[0], 255, "full coverage over black is its own value");
        assert!(two > one && two < 254, "{one} {two}");
        // A bitmap off the canvas is not drawn.
        assert_eq!(on_black(&[bitmap(5, 5, 2, 2, 255)], 3, 3), vec![0; 9]);
    }

    #[test]
    #[ignore = "needs ffmpeg"]
    fn a_run_that_does_not_finish_or_writes_too_much_is_a_refusal() {
        // A source that never ends, written to a null output: the deadline kills it, and the text is FFmpeg's.
        let endless = "testsrc=s=64x64:r=1000000:d=100000[o]";
        let started = Instant::now();
        let err = run_graph_within(
            Duration::from_millis(300),
            endless,
            &[("o", None)],
            false,
            100_000_000,
            1 << 10,
            false,
        )
        .unwrap_err();
        assert!(
            matches!(&err, GpuError::Unsupported(w) if w.contains("did not finish")),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        // More bytes than the canvas holds: killed, and refused as well.
        let err = run_graph_within(
            RASTER_TIMEOUT,
            "testsrc=s=64x64:r=30:d=10[o]",
            &[("o", Some("gray"))],
            false,
            300,
            100,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
        // How long a run is waited for grows with the instances it holds, and stops.
        assert_eq!(RASTER_TIMEOUT + PER_INSTANCE * 40, Duration::from_millis(6000));
        assert!(RASTER_TIMEOUT + PER_INSTANCE * MAX_INSTANCES as u32 <= MAX_RUN * 2);
    }

    #[test]
    fn a_failed_run_is_not_tried_again_until_it_is_forgotten() {
        let key = "test|failure memory";
        assert!(remembered_within(key, Duration::from_secs(60)).is_none());
        remember(key.to_string(), &unsupported("FFmpeg could not measure the text: timed out"));
        let e = remembered_within(key, Duration::from_secs(60)).expect("remembered");
        assert!(
            matches!(&e, GpuError::Unsupported(w) if w.contains("timed out") && w.contains("not tried again")),
            "{e}"
        );
        assert!(remembered_within(key, Duration::ZERO).is_none(), "and forgotten");
    }

    #[test]
    fn a_cache_keeps_the_latest_and_drops_the_least_recently_used() {
        let mut lru: Lru<u32> = Lru::new(2, 100);
        lru.put("a".into(), 10, Arc::new(1));
        lru.put("b".into(), 10, Arc::new(2));
        assert_eq!(lru.get("a").as_deref(), Some(&1));
        lru.put("c".into(), 10, Arc::new(3));
        assert!(lru.get("b").is_none(), "b was the least recently used");
        assert!(lru.get("a").is_some() && lru.get("c").is_some());
        lru.put("d".into(), 95, Arc::new(4));
        assert_eq!(lru.map.len(), 1, "the bytes bound drops the rest: {:?}", lru.map.keys());
    }
}
