//! Floating animated SVG titles on the GPU: FFmpeg's `overlay` of a picture, with FFmpeg's own planes.
//!
//! ffmpeg never sees an SVG (`kerf-core`'s `svg_layer.rs`): resvg draws the document at a moment into a straight-alpha RGBA PNG and
//! the export's graph `overlay`s it on the composite. The same resvg draws the same pixels here (the plan carries the artwork and
//! the moment: [`kerf_core::SvgTitle`]), so nothing of the artwork is a parity question. What is left is `overlay`'s, and it is two
//! things:
//!
//! 1. **The conversion** of the RGBA picture to the `yuva420p` the filter takes. That is swscale (RGB to YCbCr coefficients, the 2:1
//!    chroma filter, the x86 kernels), and nothing here models it: **FFmpeg converts the PNG**, in a `-filter_complex` graph of the
//!    build in use ([`CONVERT`]; a *simple* graph converts differently on some builds, `tests/svg_overlay.rs`), once per picture, and
//!    the planes are kept ([`PictureCache`], bounded, one flight per picture, a failure remembered for a while).
//! 2. **The blend**, `vf_overlay.c`'s `blend_plane` in integers (`shaders/svg.wgsl`, and [`blend_onto`] as the reference):
//!    `FAST_DIV255(d * (255 - a) + s * a)` per sample, the alpha of a chroma sample the `>> 2` average of its 2x2 block (the last chroma
//!    column and row halve what they have), and the position **rounded down to even** (`s->x &= ~((1 << hsub) - 1)`: an odd offset lands
//!    where the even one below it does, negative ones too).
//!
//! **Which FFmpeg**: the model is held to the real filter on this machine, once per process, by [`svg`]: the export's own chain text
//! ([`kerf_core::svg_overlay_chains`]) over a test composite, a picture with every alpha, odd and even sizes, offsets odd, negative and
//! past the edge; every sample of the planes it writes against the converted planes blended by the model. A build whose `overlay`
//! (its SIMD rows included) or whose conversion is not that is [`Verdict::Differs`], one that could not be asked is
//! [`Verdict::Unknown`]; neither sets [`kerf_core::GpuCaps::svg`], and the plan refuses every title.
//!
//! **The colour**: the picture is converted as the composite's matrix says, and FFmpeg 9 converts it with the composite's (its
//! negotiation ties the overlay's second input to the first's colour space), so only a BT.601 composite is drawn (the plan refuses the
//! others: [`kerf_core::SvgWhy::Matrix`]); on FFmpeg 6 the composite is read as BT.601 whatever the layers are tagged.
//!
//! **Cost**: a picture the cache does not hold costs a resvg draw, a PNG, and one `ffmpeg` child (the conversion), capped to the CPU
//! budget's threads like every read the compositor makes. A title that holds still between its moves is one picture ([`PictureCache`] is
//! keyed by what decides the pixels, not by the moment), and a title whose document cannot move is one picture for the whole cut.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::{Rational, SvgRect, SvgTitle};

use crate::gpu::GpuError;
use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::source::YuvFrame;

/// What the probe found: [`Verdict::Match`] (the model is `overlay`'s on every sample of every case), [`Verdict::Differs`] (it is not:
/// the detail names the case) or [`Verdict::Unknown`].
pub type Overlay = Verdict;

/// The graph FFmpeg converts a title's PNG with: a complex graph, whose auto-inserted scaler is the one `overlay`'s second input
/// gets in the export (a simple `-vf format=` graph converts differently on some builds).
pub const CONVERT: &str = "[0:v]format=yuva420p[o]";

/// Titles larger than this many pixels are FFmpeg's (a 4K frame twice over): the planes are kept in the cache.
pub const MAX_PICTURE_PIXELS: u64 = 3840 * 2160 * 2;

/// How many pictures the cache holds, and how many bytes of planes (a 1920x1080 picture is 5 MB).
pub const CACHE_MAX_ENTRIES: usize = 64;
pub const CACHE_MAX_BYTES: usize = 160 << 20;

/// How long a picture that could not be made is not tried again: a child that failed or timed out would otherwise be spawned again by
/// every frame that asks for it.
const FAILURE_MEMORY: Duration = Duration::from_secs(20);
const FAILURES_KEPT: usize = 64;

/// One conversion of a picture: its own child, bounded.
const RENDER: Timing = Timing {
    backoff: FAILURE_MEMORY,
    overall: Duration::from_secs(20),
    per_run: Duration::from_secs(20),
};

/// The probe: about twenty short `ffmpeg` children.
const PROBE: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(45),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// What is known without running anything: the measured answer, else [`Overlay::Unknown`]. What the caps read.
pub fn measured() -> Overlay {
    measure::known(&STATE)
}

/// The answer, measured on the first call (bounded children, no lock held: a blocking thread's, ahead of the frame that needs it,
/// [`crate::prepare`]). A caller that arrives while it runs waits for it and takes its answer. `Unknown` is not remembered but for a
/// backoff.
pub fn svg() -> Overlay {
    measure::cached(&STATE, Instant::now(), PROBE, probe)
}

/// `chroma_size` of a `w` x `h` picture.
fn chroma(w: u32, h: u32) -> (u32, u32) {
    (w.div_ceil(2), h.div_ceil(2))
}

/// A title's picture as `overlay` takes it: the `yuva420p` planes FFmpeg made of the PNG, tightly packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// The alpha plane, at luma size.
    pub a: Vec<u8>,
}

impl Picture {
    /// The chroma planes' size.
    pub fn chroma_size(&self) -> (u32, u32) {
        chroma(self.w, self.h)
    }

    /// Bytes of planes.
    pub fn bytes(&self) -> usize {
        self.y.len() + self.u.len() + self.v.len() + self.a.len()
    }

    /// Bytes of a `w` x `h` `yuva420p` picture.
    fn len(w: u32, h: u32) -> usize {
        let (cw, ch) = chroma(w, h);
        2 * w as usize * h as usize + 2 * cw as usize * ch as usize
    }

    /// The planes of `raw` (`yuva420p`, tightly packed), or `None` when it is not a `w` x `h` picture.
    fn of_planar(raw: &[u8], w: u32, h: u32) -> Option<Self> {
        if w == 0 || h == 0 || raw.len() != Self::len(w, h) {
            return None;
        }
        let (cw, ch) = chroma(w, h);
        let (luma, c) = (w as usize * h as usize, cw as usize * ch as usize);
        Some(Self {
            w,
            h,
            y: raw[..luma].to_vec(),
            u: raw[luma..luma + c].to_vec(),
            v: raw[luma + c..luma + 2 * c].to_vec(),
            a: raw[luma + 2 * c..].to_vec(),
        })
    }
}

/// What a [`PictureCache`] is charged for a value it holds.
pub trait Weighed {
    fn bytes(&self) -> usize;
}

impl Weighed for Picture {
    fn bytes(&self) -> usize {
        Picture::bytes(self)
    }
}

/// Have FFmpeg convert `pngs` (each `w` x `h`) to the `yuva420p` planes `overlay` reads, in one child: [`CONVERT`] over a PNG stream.
/// `None` when the child would not run, failed, outlasted the budget, or wrote anything but `pngs.len()` pictures.
pub(crate) fn convert(budget: &Budget, pngs: &[Vec<u8>], w: u32, h: u32) -> Option<Vec<Picture>> {
    if pngs.is_empty() {
        return Some(Vec::new());
    }
    let frames = pngs.len().to_string();
    let args = [
        "-f",
        "image2pipe",
        "-c:v",
        "png",
        "-i",
        "pipe:0",
        "-filter_complex",
        CONVERT,
        "-map",
        "[o]",
        "-frames:v",
        &frames,
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuva420p",
        "pipe:1",
    ];
    let out = budget.ffmpeg(&args, pngs.concat())?;
    let one = Picture::len(w, h);
    if out.len() != one * pngs.len() {
        return None;
    }
    out.chunks_exact(one).map(|raw| Picture::of_planar(raw, w, h)).collect()
}

/// The picture of `text` (the document at this frame, [`SvgTitle::text`]) at `rect`: resvg draws it, FFmpeg converts it.
pub fn make(title: &SvgTitle, text: &str, rect: SvgRect) -> Result<Picture, GpuError> {
    let pixels = u64::from(rect.w) * u64::from(rect.h);
    if rect.w == 0 || rect.h == 0 || pixels > MAX_PICTURE_PIXELS {
        return Err(GpuError::Unsupported(format!(
            "a floating title of {}x{} pixels is over the {MAX_PICTURE_PIXELS} the compositor holds",
            rect.w, rect.h
        )));
    }
    let png = title
        .png(text, rect)
        .map_err(|e| GpuError::Unsupported(format!("a floating title cannot be drawn: {e}")))?;
    convert(&Budget::new(&RENDER), &[png], rect.w, rect.h)
        .and_then(|mut pictures| pictures.pop())
        .ok_or_else(|| {
            GpuError::Unsupported(
                "FFmpeg could not convert a floating title's picture (it would not run, failed or timed out)".into(),
            )
        })
}

// ---- the model ---------------------------------------------------------------------------------------------------------------

fn fast_div255(v: u32) -> u32 {
    ((v + 128) * 257) >> 16
}

/// The alpha `overlay` takes for chroma sample `(cj, ck)` (row, column) of `pic`: the `>> 2` average of its 2x2 block of luma alphas
/// where it has a neighbour to the right and below, else `(alpha_v + alpha_h) >> 1` of what is there (`vf_overlay.c`, `blend_plane`; a last
/// column that has a second luma column ignores it).
pub fn chroma_alpha(pic: &Picture, cj: usize, ck: usize) -> u32 {
    let (cw, ch) = pic.chroma_size();
    let (cw, ch) = (cw as usize, ch as usize);
    let w = pic.w as usize;
    let a = |r: usize, c: usize| u32::from(pic.a[r * w + c]);
    let (right, below) = (ck + 1 < cw, cj + 1 < ch);
    let (r, c) = (2 * cj, 2 * ck);
    if right && below {
        return (a(r, c) + a(r + 1, c) + a(r, c + 1) + a(r + 1, c + 1)) >> 2;
    }
    let ah = if right { (a(r, c) + a(r, c + 1)) >> 1 } else { a(r, c) };
    let av = if below { (a(r, c) + a(r + 1, c)) >> 1 } else { a(r, c) };
    (av + ah) >> 1
}

/// `overlay`'s blend of `pic` on `base` at `(x, y)` (any sign; the part off the frame is clipped): the reference the shader and the
/// probe are held to. The position is rounded down to even, as the filter does for 4:2:0.
pub fn blend_onto(base: &YuvFrame, pic: &Picture, x: i64, y: i64) -> YuvFrame {
    let (x, y) = (x & !1, y & !1);
    let mut out = base.clone();
    let (bw, bh) = (i64::from(base.width), i64::from(base.height));
    let (w, h) = (i64::from(pic.w), i64::from(pic.h));
    for j in 0..h {
        for k in 0..w {
            let (cy, cx) = (y + j, x + k);
            if cy < 0 || cy >= bh || cx < 0 || cx >= bw {
                continue;
            }
            let (at, from) = ((cy * bw + cx) as usize, (j * w + k) as usize);
            out.y[at] = fast_div255(
                u32::from(base.y[at]) * (255 - u32::from(pic.a[from])) + u32::from(pic.y[from]) * u32::from(pic.a[from]),
            ) as u8;
        }
    }
    let (cw, ch) = pic.chroma_size();
    let (bcw, bch) = (i64::from(base.width.div_ceil(2)), i64::from(base.height.div_ceil(2)));
    for cj in 0..i64::from(ch) {
        for ck in 0..i64::from(cw) {
            let (cy, cx) = ((y >> 1) + cj, (x >> 1) + ck);
            if cy < 0 || cy >= bch || cx < 0 || cx >= bcw {
                continue;
            }
            let (at, from) = ((cy * bcw + cx) as usize, (cj * i64::from(cw) + ck) as usize);
            let a = chroma_alpha(pic, cj as usize, ck as usize);
            for (plane, src, dst) in [(&base.u, &pic.u, &mut out.u), (&base.v, &pic.v, &mut out.v)] {
                dst[at] = fast_div255(u32::from(plane[at]) * (255 - a) + u32::from(src[from]) * a) as u8;
            }
        }
    }
    out
}

// ---- the cache ---------------------------------------------------------------------------------------------------------------

struct Slot<V> {
    picture: Mutex<Option<Arc<V>>>,
    used: AtomicU64,
    charged: AtomicUsize,
}

struct Inner<V> {
    slots: HashMap<String, Arc<Slot<V>>>,
    bytes: usize,
    clock: u64,
    /// What could not be made and when (and why), for [`FAILURE_MEMORY`].
    failed: HashMap<String, (Instant, String)>,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The converted pictures of a compositor's titles, bounded by [`CACHE_MAX_ENTRIES`] and [`CACHE_MAX_BYTES`], least recently used out
/// first. A picture is made once even when several frames ask for it at the same moment (the others wait for the first); a failure is
/// remembered for [`FAILURE_MEMORY`] so that the frames that follow do not each spawn the child that just failed.
pub struct PictureCache<V = Picture> {
    inner: Mutex<Inner<V>>,
    max_entries: usize,
    max_bytes: usize,
    runs: AtomicU64,
}

impl<V: Weighed> Default for PictureCache<V> {
    fn default() -> Self {
        Self::with_limits(CACHE_MAX_ENTRIES, CACHE_MAX_BYTES)
    }
}

impl<V: Weighed> PictureCache<V> {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                slots: HashMap::new(),
                bytes: 0,
                clock: 0,
                failed: HashMap::new(),
            }),
            max_entries,
            max_bytes,
            runs: AtomicU64::new(0),
        }
    }

    /// How many pictures were made (not served from the cache).
    pub fn runs(&self) -> u64 {
        self.runs.load(Ordering::Relaxed)
    }

    /// How many pictures are held and how many bytes they are charged.
    pub fn held(&self) -> (usize, usize) {
        let inner = locked(&self.inner);
        (
            inner.slots.values().filter(|s| locked(&s.picture).is_some()).count(),
            inner.bytes,
        )
    }

    /// The picture for `key`, made by `make` if it is not held. `make` runs with no lock but its own key's.
    pub fn get(&self, key: &str, make: impl FnOnce() -> Result<V, GpuError>) -> Result<Arc<V>, GpuError> {
        let slot = {
            let mut inner = locked(&self.inner);
            if let Some((when, why)) = inner.failed.get(key) {
                if when.elapsed() < FAILURE_MEMORY {
                    return Err(GpuError::Unsupported(format!("{why} (not tried again yet)")));
                }
                inner.failed.remove(key);
            }
            inner.clock += 1;
            let clock = inner.clock;
            let slot = inner
                .slots
                .entry(key.to_string())
                .or_insert_with(|| {
                    Arc::new(Slot {
                        picture: Mutex::new(None),
                        used: AtomicU64::new(0),
                        charged: AtomicUsize::new(0),
                    })
                })
                .clone();
            slot.used.store(clock, Ordering::Relaxed);
            slot
        };
        let mut held = locked(&slot.picture);
        if let Some(picture) = held.as_ref() {
            return Ok(Arc::clone(picture));
        }
        self.runs.fetch_add(1, Ordering::Relaxed);
        let made = match make() {
            Ok(picture) => Arc::new(picture),
            Err(e) => {
                drop(held);
                let mut inner = locked(&self.inner);
                // Nobody else is waiting on a slot that never filled: forget it.
                if inner.slots.get(key).is_some_and(|s| Arc::ptr_eq(s, &slot)) && Arc::strong_count(&slot) <= 2 {
                    inner.slots.remove(key);
                }
                if inner.failed.len() >= FAILURES_KEPT {
                    let oldest = inner.failed.iter().min_by_key(|(_, (when, _))| *when).map(|(k, _)| k.clone());
                    if let Some(oldest) = oldest {
                        inner.failed.remove(&oldest);
                    }
                }
                inner.failed.insert(key.to_string(), (Instant::now(), e.to_string()));
                return Err(e);
            }
        };
        *held = Some(Arc::clone(&made));
        drop(held);
        let charged = made.bytes();
        slot.charged.store(charged, Ordering::Relaxed);
        let mut inner = locked(&self.inner);
        inner.bytes += charged;
        self.evict(&mut inner, key);
        Ok(made)
    }

    /// Drop the least recently used filled slots until the limits hold (never `keep`, never one somebody is still filling).
    fn evict(&self, inner: &mut Inner<V>, keep: &str) {
        loop {
            if inner.slots.len() <= self.max_entries && inner.bytes <= self.max_bytes {
                return;
            }
            let victim = inner
                .slots
                .iter()
                .filter(|(k, s)| k.as_str() != keep && Arc::strong_count(s) == 1 && locked(&s.picture).is_some())
                .min_by_key(|(_, s)| s.used.load(Ordering::Relaxed))
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else {
                return;
            };
            if let Some(slot) = inner.slots.remove(&victim) {
                inner.bytes = inner.bytes.saturating_sub(slot.charged.load(Ordering::Relaxed));
            }
        }
    }

    /// Forget everything.
    pub fn clear(&self) {
        let mut inner = locked(&self.inner);
        inner.slots.clear();
        inner.failed.clear();
        inner.bytes = 0;
    }
}

/// What names a title's picture in the cache: the FFmpeg that converted it (two builds convert differently) and the key of what decides
/// its pixels ([`SvgTitle::key`]).
pub fn cache_key(title: &SvgTitle, text: &str, rect: SvgRect) -> String {
    format!("{}|{}", kerf_core::ffmpeg_path(), title.key(text, rect))
}

// ---- the probe ---------------------------------------------------------------------------------------------------------------

/// The test composite, and the frame rate its graph and the title's chain share.
const BASE: &str = "testsrc2=s=96x64:d=1:r=25,format=yuv420p";
const CANVAS: (u32, u32) = (96, 64);

/// The cases: a picture's size and the offsets it is laid at. Even and odd sizes (the odd one ends in a half chroma block), offsets odd
/// and negative (the filter rounds them down to even), a picture reaching past the right and bottom edges and one larger than the
/// composite, off its top-left.
/// A picture's size, and where it is laid.
type Case = ((u32, u32), &'static [(i64, i64)]);

const CASES: &[Case] = &[
    ((40, 30), &[(0, 0), (2, 4), (3, 5), (-4, -2), (-3, -5), (70, 50)]),
    ((41, 31), &[(0, 0), (1, 1), (-5, -7), (80, 40)]),
    ((96, 64), &[(0, 0)]),
    ((130, 90), &[(-10, -6)]),
];

/// A picture like a title card's: colour that varies, clear and opaque areas, soft edges and every alpha. Straight alpha, as the PNG has it.
fn picture_rgba(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    for y in 0..h {
        for x in 0..w {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let alpha = match (state >> 24) % 6 {
                0 => 0,
                1..=3 => 255,
                4 => (state >> 8) as u8,
                _ => (x * 4) as u8,
            };
            out.extend_from_slice(&[(x * 3 + y) as u8, (y * 5) as u8, (x * y) as u8, alpha]);
        }
    }
    out
}

fn base_frame(raw: &[u8]) -> Option<YuvFrame> {
    let (w, h) = CANVAS;
    let (cw, ch) = chroma(w, h);
    let (luma, c) = (w as usize * h as usize, cw as usize * ch as usize);
    (raw.len() == luma + 2 * c).then(|| YuvFrame {
        width: w,
        height: h,
        y: raw[..luma].to_vec(),
        u: raw[luma..luma + c].to_vec(),
        v: raw[luma + c..].to_vec(),
    })
}

fn probe(budget: &Budget) -> Overlay {
    probe_with(budget, |budget, args, input| budget.ffmpeg(args, input))
}

/// [`probe`] with the way FFmpeg is run given (a test hides the real filter's answer behind a change to see the probe notice).
fn probe_with(budget: &Budget, run: impl Fn(&Budget, &[&str], Vec<u8>) -> Option<Vec<u8>>) -> Overlay {
    let Some(fps) = Rational::from_fps(25.0) else {
        return Overlay::Unknown;
    };
    let args = [
        "-f",
        "lavfi",
        "-i",
        BASE,
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ];
    let Some(base) = budget.ffmpeg(&args, Vec::new()).and_then(|raw| base_frame(&raw)) else {
        return Overlay::Unknown;
    };
    for (n, &((w, h), offsets)) in CASES.iter().enumerate() {
        let Ok(png) = kerf_core::straight_rgba_png(w, h, &picture_rgba(w, h, n as u32)) else {
            return Overlay::Unknown;
        };
        let Some(pic) = convert(budget, std::slice::from_ref(&png), w, h).and_then(|mut p| p.pop()) else {
            return Overlay::Unknown;
        };
        for &(x, y) in offsets {
            let graph = kerf_core::svg_overlay_chains(fps, 0, 1, "0:v", "o", (x, y), (0.0, 1.0));
            let args = [
                "-f",
                "lavfi",
                "-i",
                BASE,
                "-f",
                "image2pipe",
                "-c:v",
                "png",
                "-i",
                "pipe:0",
                "-filter_complex",
                &graph,
                "-map",
                "[o]",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ];
            let Some(real) = run(budget, &args, png.clone()) else {
                return Overlay::Unknown;
            };
            let model = blend_onto(&base, &pic, x, y);
            let want: Vec<u8> = model.y.iter().chain(&model.u).chain(&model.v).copied().collect();
            if real.len() != want.len() {
                return Overlay::Unknown;
            }
            let apart = real.iter().zip(&want).filter(|(a, b)| a != b).count();
            if apart != 0 {
                return Overlay::Differs(format!(
                    "overlay of a {w}x{h} picture at ({x}, {y}) on a {}x{} composite: {apart} samples differ from the converted planes blended",
                    CANVAS.0, CANVAS.1
                ));
            }
        }
    }
    Overlay::Match
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: u32, h: u32, alpha: impl Fn(u32, u32) -> u8) -> Picture {
        let (cw, ch) = chroma(w, h);
        Picture {
            w,
            h,
            y: vec![200; (w * h) as usize],
            u: vec![90; (cw * ch) as usize],
            v: vec![160; (cw * ch) as usize],
            a: (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| alpha(x, y))
                .collect(),
        }
    }

    fn flat(w: u32, h: u32) -> YuvFrame {
        let (cw, ch) = chroma(w, h);
        YuvFrame {
            width: w,
            height: h,
            y: vec![16; (w * h) as usize],
            u: vec![128; (cw * ch) as usize],
            v: vec![128; (cw * ch) as usize],
        }
    }

    #[test]
    fn the_division_is_ffmpegs_fast_div255() {
        for d in [0u32, 1, 16, 127, 128, 200, 255] {
            for s in [0u32, 1, 16, 127, 128, 200, 255] {
                for a in 0..=255 {
                    let v = d * (255 - a) + s * a;
                    let exact = (v as f64 / 255.0).round() as u32;
                    assert_eq!(fast_div255(v), exact, "{d} {s} {a}");
                }
            }
        }
    }

    #[test]
    fn an_opaque_picture_replaces_what_it_covers_and_a_clear_one_changes_nothing() {
        let base = flat(8, 8);
        let opaque = blend_onto(&base, &picture(4, 4, |_, _| 255), 2, 2);
        assert_eq!(opaque.y[2 * 8 + 2], 200);
        assert_eq!(opaque.y[0], 16);
        assert_eq!((opaque.u[4 + 1], opaque.v[4 + 1]), (90, 160));
        assert_eq!(blend_onto(&base, &picture(4, 4, |_, _| 0), 2, 2), base);
    }

    #[test]
    fn an_odd_offset_lands_where_the_even_one_below_it_does() {
        let base = flat(16, 16);
        let pic = picture(5, 5, |x, y| (x * 40 + y * 9) as u8);
        for (odd, even) in [
            ((1, 0), (0, 0)),
            ((3, 5), (2, 4)),
            ((-1, -1), (-2, -2)),
            ((-3, 0), (-4, 0)),
            ((7, 9), (6, 8)),
        ] {
            assert_eq!(
                blend_onto(&base, &pic, odd.0, odd.1),
                blend_onto(&base, &pic, even.0, even.1),
                "{odd:?}"
            );
        }
        assert_ne!(blend_onto(&base, &pic, 2, 0), blend_onto(&base, &pic, 0, 0));
    }

    #[test]
    fn a_picture_off_the_frame_is_clipped_and_one_wholly_off_it_changes_nothing() {
        let base = flat(8, 8);
        let pic = picture(6, 6, |_, _| 255);
        let clipped = blend_onto(&base, &pic, -4, -2);
        assert_eq!(clipped.y[0], 200, "the picture's bottom-right corner is on the frame");
        assert_eq!(
            (clipped.y[3 * 8 + 1], clipped.y[4 * 8], clipped.y[2]),
            (200, 16, 16),
            "four rows and two columns of it, and nothing beyond"
        );
        assert_eq!(blend_onto(&base, &pic, 8, 0), base);
        assert_eq!(blend_onto(&base, &pic, 0, 8), base);
        assert_eq!(blend_onto(&base, &pic, -6, 0), base);
        assert_eq!(blend_onto(&base, &pic, 100, 100), base);
    }

    #[test]
    fn the_last_chroma_column_and_row_ignore_their_second_luma_sample_and_halve_what_they_have() {
        // 4x4 picture, chroma 2x2. Only the right-most luma column is opaque.
        let pic = picture(4, 4, |x, _| if x >= 2 { 255 } else { 0 });
        // Chroma (0, 0) has a right and a below neighbour and reads luma columns 0 and 1, which are clear.
        assert_eq!(chroma_alpha(&pic, 0, 0), 0);
        // Chroma (0, 1) is the last column: it reads its first luma column (x = 2, opaque) and never the second;
        // the row below it exists, so alpha_v = (255 + 255) >> 1; alpha_h = 255: (255 + 255) >> 1.
        assert_eq!(chroma_alpha(&pic, 0, 1), 255);
        // 5x5 picture: its last chroma column is column 2 (luma 4 only).
        let odd = picture(5, 5, |x, y| (x * 10 + y) as u8);
        let a = |x: usize, y: usize| u32::from(odd.a[y * 5 + x]);
        assert_eq!(chroma_alpha(&odd, 0, 0), (a(0, 0) + a(1, 0) + a(0, 1) + a(1, 1)) >> 2);
        assert_eq!(chroma_alpha(&odd, 2, 2), a(4, 4), "the corner has only itself");
        assert_eq!(chroma_alpha(&odd, 0, 2), (((a(4, 0) + a(4, 1)) >> 1) + a(4, 0)) >> 1);
    }

    #[test]
    fn a_planar_picture_is_checked_for_its_length() {
        let raw = vec![7u8; Picture::len(5, 3)];
        let pic = Picture::of_planar(&raw, 5, 3).expect("a 5x3 picture");
        assert_eq!((pic.y.len(), pic.u.len(), pic.v.len(), pic.a.len()), (15, 6, 6, 15));
        assert!(Picture::of_planar(&raw[1..], 5, 3).is_none());
        assert!(Picture::of_planar(&raw, 0, 3).is_none());
    }

    fn tiny(n: usize) -> Picture {
        Picture {
            w: 1,
            h: 1,
            y: vec![n as u8; 1],
            u: vec![0],
            v: vec![0],
            a: vec![255],
        }
    }

    #[test]
    fn the_cache_makes_a_picture_once_and_serves_it_again() {
        let cache = PictureCache::with_limits(4, 1 << 20);
        let a = cache.get("a", || Ok(tiny(1))).unwrap();
        let again = cache.get("a", || panic!("made twice")).unwrap();
        assert!(Arc::ptr_eq(&a, &again));
        assert_eq!(cache.runs(), 1);
        assert_eq!(cache.held(), (1, 4));
    }

    #[test]
    fn the_cache_is_bounded_and_drops_the_least_recently_used() {
        let cache = PictureCache::with_limits(3, 1 << 20);
        for n in 0..3 {
            cache.get(&n.to_string(), || Ok(tiny(n))).unwrap();
        }
        // Use "0" again, then add two more: "1" and "2" are the ones to go.
        cache.get("0", || panic!("held")).unwrap();
        cache.get("3", || Ok(tiny(3))).unwrap();
        cache.get("4", || Ok(tiny(4))).unwrap();
        assert_eq!(cache.held().0, 3);
        cache.get("0", || panic!("kept: it was used last but one")).unwrap();
        let made = std::cell::Cell::new(false);
        cache
            .get("1", || {
                made.set(true);
                Ok(tiny(1))
            })
            .unwrap();
        assert!(made.get(), "1 was dropped");
        // By bytes too.
        let small = PictureCache::with_limits(100, 10);
        for n in 0..5 {
            small.get(&n.to_string(), || Ok(tiny(n))).unwrap();
        }
        assert!(small.held().1 <= 10 && small.held().0 == 2, "{:?}", small.held());
    }

    #[test]
    fn a_failure_is_remembered_so_that_the_next_frame_does_not_spawn_the_child_again() {
        let cache = PictureCache::with_limits(4, 1 << 20);
        let tried = AtomicUsize::new(0);
        for _ in 0..3 {
            let err = cache
                .get("x", || {
                    tried.fetch_add(1, Ordering::Relaxed);
                    Err(GpuError::Unsupported("the child failed".into()))
                })
                .unwrap_err();
            assert!(matches!(err, GpuError::Unsupported(_)));
        }
        assert_eq!(tried.load(Ordering::Relaxed), 1);
        assert_eq!(cache.held(), (0, 0), "nothing is kept for a picture that was not made");
        // Another picture is its own.
        assert!(cache.get("y", || Ok(tiny(1))).is_ok());
    }

    #[test]
    fn several_frames_asking_for_one_picture_make_it_once() {
        let cache = PictureCache::with_limits(4, 1 << 20);
        let made = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..6 {
                scope.spawn(|| {
                    cache
                        .get("same", || {
                            made.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(30));
                            Ok(tiny(9))
                        })
                        .unwrap();
                });
            }
        });
        assert_eq!(made.load(Ordering::Relaxed), 1);
    }

    /// The probe's memory is the shared one: a probe that could not tell is asked again after the backoff, a match is kept.
    #[test]
    fn a_probe_that_could_not_tell_is_asked_again_later() {
        let state = Probe::new();
        assert_eq!(measure::known(&state), Overlay::Unknown);
        let t0 = Instant::now();
        assert_eq!(measure::cached(&state, t0, PROBE, |_| Overlay::Unknown), Overlay::Unknown);
        let after = Instant::now() + PROBE.backoff + Duration::from_secs(1);
        assert_eq!(measure::cached(&state, after, PROBE, |_| Overlay::Match), Overlay::Match);
        assert_eq!(measure::known(&state), Overlay::Match);
    }

    /// The probe on the FFmpeg in use (`#[ignore]`d: it needs one): it must be able to tell, and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_overlays_a_title_as_the_model_blends_it() {
        let first = probe(&Budget::new(&PROBE));
        eprintln!("the overlay of a title: {first:?}");
        assert_eq!(first, Overlay::Match, "{first:?}");
        assert_eq!(first, probe(&Budget::new(&PROBE)));
    }

    /// FFmpeg converts a stream of pictures in one child as it converts each in a child of its own (the conversion takes any number of pictures;
    /// the compositor asks for one at a time).
    #[test]
    #[ignore = "needs ffmpeg"]
    fn converting_several_pictures_in_one_child_is_converting_each_in_its_own() {
        let budget = Budget::new(&PROBE);
        let (w, h) = (41, 31);
        let pngs: Vec<Vec<u8>> = (0..3)
            .map(|n| kerf_core::straight_rgba_png(w, h, &picture_rgba(w, h, 10 + n)).expect("a PNG"))
            .collect();
        let together = convert(&budget, &pngs, w, h).expect("one child, three pictures");
        assert_eq!(together.len(), 3);
        for (png, got) in pngs.iter().zip(&together) {
            let alone = convert(&budget, std::slice::from_ref(png), w, h).expect("one picture");
            assert_eq!(&alone[0], got);
        }
        assert_ne!(together[0], together[1], "they are different pictures");
    }

    /// The probe has teeth: shown a blend that is one level off in one sample, it says `Differs`.
    #[test]
    #[ignore = "needs ffmpeg"]
    fn a_blend_one_level_off_is_a_difference_and_not_a_match() {
        let found = probe_with(&Budget::new(&PROBE), |budget, args, input| {
            let mut out = budget.ffmpeg(args, input)?;
            if args.contains(&"-filter_complex") && args.iter().any(|a| a.contains("overlay")) {
                out[3 * 96 + 40] ^= 1;
            }
            Some(out)
        });
        assert!(matches!(found, Overlay::Differs(_)), "{found:?}");
    }
}
