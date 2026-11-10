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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
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

/// How long an asker waits for the flight that is making the picture it wants before it gives up on it (a conversion is bounded by
/// [`RENDER`]'s 20 s; this is what a resvg draw and the queueing add).
const FLIGHT_WAIT: Duration = Duration::from_secs(30);

/// How long a picture a prefetch made is counted as due (a prefetch looks seconds ahead): past this, if no frame has shown it, it is
/// pushed out like any other.
const DUE_FOR: Duration = Duration::from_secs(10);
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

/// What a title's picture at `rect` is charged to the cache once it is made (its planes on the host and their copy on the device).
pub fn charge(rect: SvgRect) -> usize {
    2 * Picture::len(rect.w, rect.h)
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
    // (A drawing that panics is a title that cannot be drawn, remembered as any other failure is: a document that panics the rasteriser would
    // otherwise do it again on every frame that asks for it, and take the render thread with it.)
    let png = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| title.png(text, rect)))
        .map_err(|_| GpuError::Unsupported("the rasteriser panicked drawing a floating title".into()))?
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

/// What one key's flight has come to.
enum Fill<V> {
    /// Nothing: no one is making the picture (it was never asked for, or its maker panicked and the next asker makes it).
    Empty,
    /// Someone is making it, with **no lock held** (it spawns a child and waits for it): the others wait on the slot's condition for the
    /// result, each for no longer than [`FLIGHT_WAIT`] of its own.
    Making,
    Ready(Arc<V>),
    /// The maker failed: why. The askers that were waiting on this flight take it instead of making the picture again.
    Failed(String),
}

struct Slot<V> {
    fill: Mutex<Fill<V>>,
    /// Told when a flight ends, whatever it came to.
    ended: Condvar,
    used: AtomicU64,
    charged: AtomicUsize,
    /// When a prefetch made the picture, in nanoseconds on the cache's own clock plus one; 0: it was asked for by a frame (or never by a
    /// prefetch). A picture made ahead and not yet shown is due, and a prefetch does not push it out for another one that is due later.
    warmed: AtomicU64,
}

impl<V> Slot<V> {
    /// Whether the picture is held, without waiting on a flight under way (that one is not held yet).
    fn is_ready(&self) -> bool {
        matches!(*locked(&self.fill), Fill::Ready(_))
    }
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

/// What a failure says, without the "not rendered on the GPU: " every [`GpuError::Unsupported`] prints before it.
fn reason(e: &GpuError) -> String {
    match e {
        GpuError::Unsupported(why) => why.clone(),
        other => other.to_string(),
    }
}

/// How a picture is asked for.
#[derive(Clone, Copy)]
enum Ask {
    /// By a frame that is drawn: made if it is not held.
    Frame,
    /// By the prefetch, for a frame that comes due later: made only where it fits without pushing out a picture that is due sooner
    /// (a prefetch before it that nothing has shown yet). `charge` is what the picture will be charged.
    Ahead { charge: usize },
}

/// What a prefetch's ask came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warmed {
    /// The picture is held (it was, or it has been made).
    Held,
    /// It is not, and making it would push out one that is due sooner: nothing was made. Ask again when those have been shown.
    NoRoom,
}

/// The converted pictures of a compositor's titles, bounded by [`CACHE_MAX_ENTRIES`] and [`CACHE_MAX_BYTES`], least recently used out
/// first. A picture is made once even when several frames ask for it at the same moment: the others wait for the first, each no longer
/// than [`FLIGHT_WAIT`], and **take its result, a failure included**; a failure is remembered for [`FAILURE_MEMORY`] so that the frames
/// that follow do not each spawn the child that just failed. **No lock is held while a picture is made** (a maker spawns `ffmpeg` and
/// waits for it): a flight is a marker in its slot, and a waiter waits on the slot's condition with a deadline. The books (what is held,
/// what is charged) stay right through [`PictureCache::clear`]: a flight that began before it hands its picture to the asker and keeps
/// nothing.
pub struct PictureCache<V = Picture> {
    inner: Mutex<Inner<V>>,
    max_entries: usize,
    max_bytes: usize,
    runs: AtomicU64,
    epoch: Instant,
    flight_wait: Duration,
}

impl<V: Weighed> Default for PictureCache<V> {
    fn default() -> Self {
        Self::with_limits(CACHE_MAX_ENTRIES, CACHE_MAX_BYTES)
    }
}

/// Ends a flight that did not come to a result (the maker panicked): the slot is empty again and a waiter makes the picture.
struct Flight<'a, V> {
    slot: &'a Slot<V>,
    over: bool,
}

impl<V> Drop for Flight<'_, V> {
    fn drop(&mut self) {
        if !self.over {
            *locked(&self.slot.fill) = Fill::Empty;
            self.slot.ended.notify_all();
        }
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
            epoch: Instant::now(),
            flight_wait: FLIGHT_WAIT,
        }
    }

    /// The cache with an asker's wait for a flight under way bounded by `wait` instead of [`FLIGHT_WAIT`].
    #[must_use]
    pub fn with_flight_wait(mut self, wait: Duration) -> Self {
        self.flight_wait = wait;
        self
    }

    /// How many pictures were made (not served from the cache).
    pub fn runs(&self) -> u64 {
        self.runs.load(Ordering::Relaxed)
    }

    /// How many pictures are held and how many bytes they are charged.
    pub fn held(&self) -> (usize, usize) {
        let inner = locked(&self.inner);
        (inner.slots.values().filter(|s| s.is_ready()).count(), inner.bytes)
    }

    /// Nanoseconds on the cache's clock, plus one (0 is "not warmed").
    fn stamp(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX - 1) + 1
    }

    /// Whether a prefetch made `slot`'s picture lately and no frame has used it since.
    fn is_due(&self, slot: &Slot<V>) -> bool {
        let warmed = slot.warmed.load(Ordering::Relaxed);
        warmed != 0 && self.stamp().saturating_sub(warmed) < u64::try_from(DUE_FOR.as_nanos()).unwrap_or(u64::MAX)
    }

    /// The picture for `key`, made by `make` if it is not held. `make` runs with no lock held; an asker that arrives while it runs waits for
    /// it (for at most [`FLIGHT_WAIT`]) and takes what it made, or why it could not.
    pub fn get(&self, key: &str, make: impl FnOnce() -> Result<V, GpuError>) -> Result<Arc<V>, GpuError> {
        self.fetch(key, Ask::Frame, make)?
            .ok_or_else(|| GpuError::Gpu("a picture was asked for and not made".into()))
    }

    /// [`PictureCache::get`] for the prefetch: the picture of a frame that comes due later. It is made only if it fits without pushing out a
    /// picture another prefetch made that nothing has shown yet (those are due sooner: the prefetch goes in order), else [`Warmed::NoRoom`]
    /// and nothing is made. `charge` is what the picture will cost the books.
    pub fn warm(&self, key: &str, charge: usize, make: impl FnOnce() -> Result<V, GpuError>) -> Result<Warmed, GpuError> {
        Ok(match self.fetch(key, Ask::Ahead { charge }, make)? {
            Some(_) => Warmed::Held,
            None => Warmed::NoRoom,
        })
    }

    fn fetch(&self, key: &str, ask: Ask, make: impl FnOnce() -> Result<V, GpuError>) -> Result<Option<Arc<V>>, GpuError> {
        let slot = {
            let mut inner = locked(&self.inner);
            if let Some((when, why)) = inner.failed.get(key) {
                if when.elapsed() < FAILURE_MEMORY {
                    return Err(GpuError::Unsupported(format!("{why} (not tried again yet)")));
                }
                inner.failed.remove(key);
            }
            if let Ask::Ahead { charge } = ask {
                // A picture that is not here is made only into room that holds nothing due sooner.
                if !inner.slots.contains_key(key) && !self.make_room(&mut inner, charge) {
                    return Ok(None);
                }
            }
            inner.clock += 1;
            let clock = inner.clock;
            let slot = inner
                .slots
                .entry(key.to_string())
                .or_insert_with(|| {
                    Arc::new(Slot {
                        fill: Mutex::new(Fill::Empty),
                        ended: Condvar::new(),
                        used: AtomicU64::new(0),
                        charged: AtomicUsize::new(0),
                        warmed: AtomicU64::new(0),
                    })
                })
                .clone();
            slot.used.store(clock, Ordering::Relaxed);
            slot
        };
        // Either the picture is here, or this asker makes it, or it waits for the one who is.
        let deadline = Instant::now() + self.flight_wait;
        let mut fill = locked(&slot.fill);
        loop {
            match &*fill {
                Fill::Ready(picture) => {
                    let picture = Arc::clone(picture);
                    if matches!(ask, Ask::Frame) {
                        // A frame has it: it is no longer due.
                        slot.warmed.store(0, Ordering::Relaxed);
                    }
                    return Ok(Some(picture));
                }
                // The flight this asker waited on failed: its failure is the answer (the picture is not made again by each of them).
                Fill::Failed(why) => return Err(GpuError::Unsupported(why.clone())),
                Fill::Empty => break,
                Fill::Making => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(GpuError::Unsupported("a floating title's picture is still being made".into()));
                    }
                    fill = slot.ended.wait_timeout(fill, left).unwrap_or_else(PoisonError::into_inner).0;
                }
            }
        }
        *fill = Fill::Making;
        drop(fill);
        self.runs.fetch_add(1, Ordering::Relaxed);
        // (If `make` panics, the guard empties the slot and tells the waiters: the next one makes it.)
        let mut flight = Flight {
            slot: &slot,
            over: false,
        };
        let made = make();
        flight.over = true;
        let made = match made {
            Ok(picture) => Arc::new(picture),
            Err(e) => {
                let why = reason(&e);
                *locked(&slot.fill) = Fill::Failed(why.clone());
                slot.ended.notify_all();
                let mut inner = locked(&self.inner);
                // Remembered, and the slot dropped, only if the cache still holds this flight: a `clear` since it began has forgotten it.
                if inner.slots.get(key).is_some_and(|s| Arc::ptr_eq(s, &slot)) {
                    inner.slots.remove(key);
                    if inner.failed.len() >= FAILURES_KEPT {
                        let oldest = inner.failed.iter().min_by_key(|(_, (when, _))| *when).map(|(k, _)| k.clone());
                        if let Some(oldest) = oldest {
                            inner.failed.remove(&oldest);
                        }
                    }
                    inner.failed.insert(key.to_string(), (Instant::now(), why));
                }
                return Err(e);
            }
        };
        let charged = made.bytes();
        let mut inner = locked(&self.inner);
        // Charged only if the cache still holds this slot: a `clear` since the flight began has dropped it (and the books with it), and
        // what it made is the asker's alone.
        if inner.slots.get(key).is_some_and(|s| Arc::ptr_eq(s, &slot)) {
            slot.charged.store(charged, Ordering::Relaxed);
            if matches!(ask, Ask::Ahead { .. }) {
                slot.warmed.store(self.stamp(), Ordering::Relaxed);
            }
            inner.bytes += charged;
            // (Published last, under the books' lock: a frame that finds it ready finds it charged.)
            *locked(&slot.fill) = Fill::Ready(Arc::clone(&made));
            self.evict(&mut inner, key);
        } else {
            *locked(&slot.fill) = Fill::Ready(Arc::clone(&made));
        }
        drop(inner);
        slot.ended.notify_all();
        Ok(Some(made))
    }

    /// Drop the least recently used slots nobody is using until `charge` more bytes and one more entry fit, never one a prefetch made
    /// that nothing has shown yet; `false` when that cannot be done (nothing is dropped that need not be).
    fn make_room(&self, inner: &mut Inner<V>, charge: usize) -> bool {
        let fits =
            |inner: &Inner<V>| inner.slots.len() < self.max_entries && inner.bytes.saturating_add(charge) <= self.max_bytes;
        while !fits(inner) {
            let victim = inner
                .slots
                .iter()
                .filter(|(_, s)| Arc::strong_count(s) == 1 && !self.is_due(s))
                .min_by_key(|(_, s)| s.used.load(Ordering::Relaxed))
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else {
                return false;
            };
            if let Some(slot) = inner.slots.remove(&victim) {
                inner.bytes = inner.bytes.saturating_sub(slot.charged.load(Ordering::Relaxed));
            }
        }
        true
    }

    /// Drop the slots nobody is using until the limits hold — the ones no prefetch is waiting to show first, the least recently used first
    /// — never `keep`, never one somebody is filling or waiting on.
    fn evict(&self, inner: &mut Inner<V>, keep: &str) {
        loop {
            if inner.slots.len() <= self.max_entries && inner.bytes <= self.max_bytes {
                return;
            }
            let victim = inner
                .slots
                .iter()
                .filter(|(k, s)| k.as_str() != keep && Arc::strong_count(s) == 1)
                .min_by_key(|(_, s)| (self.is_due(s), s.used.load(Ordering::Relaxed)))
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else {
                return;
            };
            if let Some(slot) = inner.slots.remove(&victim) {
                inner.bytes = inner.bytes.saturating_sub(slot.charged.load(Ordering::Relaxed));
            }
        }
    }

    /// Forget everything. A flight under way finishes for the asker that began it and is not kept or charged.
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

    /// The frames that were waiting on a picture that could not be made take that failure: the child that failed is not spawned again by each of
    /// them (a stalled one would cost its timeout for every waiter), and the message names the cause once.
    #[test]
    fn the_askers_waiting_on_a_failing_picture_take_its_failure_and_the_message_is_not_doubled() {
        let cache = PictureCache::<Picture>::with_limits(4, 1 << 20);
        let made = AtomicUsize::new(0);
        let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..5 {
                scope.spawn(|| {
                    let err = cache
                        .get("k", || {
                            made.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(150));
                            Err(GpuError::Unsupported("the child failed".into()))
                        })
                        .unwrap_err();
                    locked(&failures).push(err.to_string());
                });
            }
        });
        assert_eq!(made.load(Ordering::Relaxed), 1);
        let failures = locked(&failures);
        assert_eq!(failures.len(), 5);
        assert!(
            failures
                .iter()
                .all(|f| f.starts_with("not rendered on the GPU: the child failed")),
            "{failures:?}"
        );
        // ... and a later ask is answered from the memory, with the prefix once as well.
        let later = cache.get("k", || panic!("made again")).unwrap_err().to_string();
        assert_eq!(later.matches("not rendered on the GPU").count(), 1, "{later}");
        assert!(later.contains("the child failed (not tried again yet)"), "{later}");
        // A failure that is not `Unsupported` is remembered by its own message.
        let other = cache.get("d", || Err(GpuError::Decode("a pipe broke".into()))).unwrap_err();
        assert!(matches!(other, GpuError::Decode(_)));
        let again = cache.get("d", || panic!("made again")).unwrap_err().to_string();
        assert!(
            again.contains("a pipe broke") && again.matches("not rendered on the GPU").count() == 1,
            "{again}"
        );
    }

    /// A `clear` while pictures are being made leaves the books right: what is charged is what is held, and a flight that began before it
    /// keeps nothing.
    #[test]
    fn a_clear_during_the_fills_leaves_what_is_charged_equal_to_what_is_held() {
        let cache = PictureCache::<Picture>::with_limits(50, 1 << 20);
        std::thread::scope(|scope| {
            for t in 0..4u32 {
                let cache = &cache;
                scope.spawn(move || {
                    for i in 0..200u32 {
                        let _ = cache.get(&format!("t{t}-{}", i % 7), || {
                            std::thread::sleep(Duration::from_millis(1));
                            Ok(tiny(1))
                        });
                    }
                });
            }
            let clearer = &cache;
            scope.spawn(move || {
                for _ in 0..100 {
                    std::thread::sleep(Duration::from_millis(2));
                    clearer.clear();
                }
            });
        });
        let (n, bytes) = cache.held();
        assert_eq!(bytes, n * 4, "{n} pictures held but {bytes} bytes charged");
        // A flight that is cleared away still hands its picture to the one who asked, and charges nothing.
        let started = std::sync::Barrier::new(2);
        let fresh = PictureCache::<Picture>::with_limits(4, 1 << 20);
        std::thread::scope(|scope| {
            let flight = scope.spawn(|| {
                fresh
                    .get("k", || {
                        started.wait();
                        std::thread::sleep(Duration::from_millis(100));
                        Ok(tiny(7))
                    })
                    .unwrap()
            });
            started.wait();
            fresh.clear();
            assert_eq!(flight.join().unwrap().y, vec![7]);
        });
        assert_eq!(fresh.held(), (0, 0), "nothing is kept or charged for it");
        // ... and a flight that fails after the clear is not remembered against a cache that has forgotten it.
        let started = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let flight = scope.spawn(|| {
                fresh.get("f", || {
                    started.wait();
                    std::thread::sleep(Duration::from_millis(100));
                    Err(GpuError::Unsupported("the child failed".into()))
                })
            });
            started.wait();
            fresh.clear();
            assert!(flight.join().unwrap().is_err());
        });
        assert!(
            fresh.get("f", || Ok(tiny(1))).is_ok(),
            "made again: the failure was not remembered"
        );
    }

    /// The prefetch goes in order, so what it made last is due latest: when the cache is full of pictures that are due sooner (made ahead,
    /// shown by no frame yet) the next one is not made, and nothing is pushed out for it. A picture a frame has shown is no longer due.
    #[test]
    fn a_prefetch_stops_where_it_would_push_out_a_picture_that_is_due_sooner() {
        let cache = PictureCache::<Picture>::with_limits(3, 1 << 20);
        let made = AtomicUsize::new(0);
        let warm = |key: &str| {
            cache
                .warm(key, 4, || {
                    made.fetch_add(1, Ordering::Relaxed);
                    Ok(tiny(1))
                })
                .unwrap()
        };
        for key in ["a", "b", "c"] {
            assert_eq!(warm(key), Warmed::Held, "{key}");
        }
        assert_eq!(made.load(Ordering::Relaxed), 3);
        // Full of pictures that are due: the fourth is not made, and nothing was dropped for it.
        assert_eq!(warm("d"), Warmed::NoRoom);
        assert_eq!(made.load(Ordering::Relaxed), 3);
        assert_eq!(cache.held(), (3, 12));
        // A held one is held, however full the cache is.
        assert_eq!(warm("b"), Warmed::Held);
        assert_eq!(made.load(Ordering::Relaxed), 3);
        // A frame shows `a`: it is past, and the next prefetch may push it out (the least recently used of what is not due).
        cache.get("a", || panic!("held")).unwrap();
        assert_eq!(warm("d"), Warmed::Held);
        assert_eq!(made.load(Ordering::Relaxed), 4);
        assert_eq!(cache.held(), (3, 12));
        cache.get("b", || panic!("b is still held")).unwrap();
        cache.get("c", || panic!("c is still held")).unwrap();
        cache.get("d", || panic!("d is held")).unwrap();
        // A frame that needs a picture takes room even from the ones that are due (a render never fails for the prefetch's sake).
        let fresh = PictureCache::<Picture>::with_limits(2, 1 << 20);
        for key in ["x", "y"] {
            fresh.warm(key, 4, || Ok(tiny(2))).unwrap();
        }
        fresh.get("z", || Ok(tiny(3))).unwrap();
        assert_eq!(fresh.held().0, 2);
        // By bytes as well: a picture that does not fit beside the ones that are due waits.
        let small = PictureCache::<Picture>::with_limits(10, 10);
        assert_eq!(small.warm("p", 4, || Ok(tiny(1))).unwrap(), Warmed::Held);
        assert_eq!(small.warm("q", 4, || Ok(tiny(1))).unwrap(), Warmed::Held);
        assert_eq!(small.warm("r", 4, || panic!("made")).unwrap(), Warmed::NoRoom);
        // ... and one larger than the whole cache never is.
        let tiny_cache = PictureCache::<Picture>::with_limits(10, 3);
        assert_eq!(tiny_cache.warm("big", 4, || panic!("made")).unwrap(), Warmed::NoRoom);
    }

    /// Nothing is held while a picture is made: another key is made, and the books are read, while one flight is under way, and an asker
    /// that waits for it waits no longer than its own deadline (it is not held in a lock a stalled child holds).
    #[test]
    fn no_lock_is_held_while_a_picture_is_made_and_a_waiter_is_bounded_by_its_own_deadline() {
        let cache = PictureCache::<Picture>::with_limits(4, 1 << 20).with_flight_wait(Duration::from_millis(100));
        let started = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let flight = scope.spawn(|| {
                cache
                    .get("slow", || {
                        started.wait();
                        std::thread::sleep(Duration::from_millis(500));
                        Ok(tiny(5))
                    })
                    .unwrap()
            });
            started.wait();
            let t0 = Instant::now();
            // The books and another key do not wait for the flight ...
            assert_eq!(cache.held(), (0, 0));
            assert!(cache.get("quick", || Ok(tiny(6))).is_ok());
            // ... and an asker for the same key gives up at its own deadline, with nothing remembered against the picture.
            let err = cache.get("slow", || panic!("made twice")).unwrap_err();
            assert!(
                matches!(&err, GpuError::Unsupported(why) if why.contains("still being made")),
                "{err}"
            );
            assert!(t0.elapsed() < Duration::from_millis(400), "{:?}", t0.elapsed());
            assert_eq!(flight.join().unwrap().y, vec![5]);
        });
        assert!(cache.get("slow", || panic!("held now")).is_ok());
    }

    /// A maker that panics ends its flight: the asker that was waiting makes the picture.
    #[test]
    fn a_waiter_makes_the_picture_when_the_flight_it_waited_on_panics() {
        let cache = PictureCache::<Picture>::with_limits(4, 1 << 20);
        let started = std::sync::Barrier::new(2);
        let made = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let flight = scope.spawn(|| {
                let _ = cache.get("k", || {
                    started.wait();
                    std::thread::sleep(Duration::from_millis(150));
                    panic!("the rasteriser panicked")
                });
            });
            started.wait();
            let got = cache
                .get("k", || {
                    made.fetch_add(1, Ordering::Relaxed);
                    Ok(tiny(8))
                })
                .unwrap();
            assert_eq!(got.y, vec![8]);
            assert!(flight.join().is_err());
        });
        assert_eq!(made.load(Ordering::Relaxed), 1);
        assert_eq!(cache.held(), (1, 4));
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
