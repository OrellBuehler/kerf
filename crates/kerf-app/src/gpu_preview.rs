//! The GPU preview (plan work package A2): the Preview panel's frame drawn by the wgpu
//! compositor in a native surface, instead of FFmpeg's JPEG, **for the frames the compositor
//! draws exactly**.
//!
//! It is opt-in (*Settings › Preview › GPU preview*, off by default) and strictly additive: with
//! the setting off nothing here runs, and with it on every failure — no adapter, no surface, a
//! lost device, a frame the plan refuses, a decode that is busy — ends in the JPEG that was
//! always there, for that frame, with the reason in the result. The webview asks for a frame
//! ([`GpuPreview::post`]) and is told which renderer made it.
//!
//! # Pieces
//!
//! * **The decision** is the plan's: [`kerf_core::RenderPlan::reasons`] with the compositor's
//!   caps at the size it would render. Nothing is decided here that the plan does not know,
//!   except what the *surface* adds: the panel has no place yet, it is hidden, or the page has
//!   to draw over the picture on a surface that sits above the page ([`Technique::overlays`]).
//! * **Lock-free**: the plan's inputs are an `Arc` snapshot ([`snapshot`]) taken under the project
//!   lock only when the plan epoch says the project or its proxies changed, and the lock is released
//!   before anything is decoded, planned from files or rendered; a scrubbed frame takes none.
//! * **Latest wins** ([`mailbox`]): a request replaces the one waiting, so a drag draws the frames
//!   it ends on and not every one it passed.
//! * **Gestures and forward scrubs (A3)**: a request may carry what a gesture under way would write
//!   ([`draft`]); the frame is drawn as the cut will be once it is let go, and nothing is written. A
//!   drag that walks forward is decoded reading ahead ([`pace::Trend`]).
//! * **Checked, counted, paced**: the first frame the GPU would draw waits for the parity self-test
//!   ([`check`]), every refusal is counted by kind ([`diagnostics`]), and frames of a drag are
//!   presented without waiting for the refresh ([`pace`]).
//! * **Lazy and rebuildable**: the device, compositor, frame source and surface are made on the
//!   first frame that wants them ([`Backend`]); a failure is remembered with a backoff
//!   ([`Backoff`]) and a lost device is dropped and rebuilt on the next use, as `kerf-gpu`
//!   documents — the owner builds a new `Gpu`, never the same one revived.
//! * **Nothing may freeze it**: the build runs off the render lock with a deadline, a panic in the
//!   GPU path is caught (the frame is FFmpeg's), the setting flips synchronously and is looked at
//!   again before a build and before a picture is shown, and a hide that lands while a frame is
//!   being drawn is not undone by it ([`GpuPreview::under_bounds`]).
//! * **Two surface techniques**, picked per platform ([`resolve_technique`]):
//!   [`Technique::Window`] draws to the main window's own surface *under* a transparent webview
//!   (the preferred one: the page keeps drawing titles, guides and timecode over the picture) and
//!   [`Technique::Child`] puts a borderless child X11 window over the panel (Linux; WebKitGTK
//!   paints into the toplevel's window, so a swapchain there loses to GTK's repaints). The
//!   evidence for each is in `CLAUDE.md` and the progress log. macOS has neither until it has been
//!   run on a Mac (`.claude/docs/app.md`, *macOS presenter*).
//!
//! This is a GUI-only surface: it has no MCP tool, because the agent already has
//! `preview_timeline` (a JPEG of the cut) and nothing here is an edit.

#[cfg(test)]
mod busy;
mod check;
mod diagnostics;
mod draft;
mod mailbox;
mod pace;
mod play;
mod prime;
mod snapshot;
#[cfg(target_os = "linux")]
mod x11;

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

use base64::Engine as _;
use kerf_core::{
    composite_color_policy, CompositeColorPolicy, ExportOptions, GpuCaps, PlanLayer, PlanRequest, Planner, Project, ProxyMedia,
    RenderPlan, ScrubConfig, ScrubFrames, StreamKind, Timeline,
};
use kerf_gpu::{
    Compositor, FrameSource, FrameSourceConfig, Gpu, GpuError, GpuOptions, Hint, PixelRect, Presenter, RenderTimings,
    RenderedFrame, SelfTestError, SelfTestReport, Surround,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use check::{AdapterKey, Check, Gate, Verdict};
use diagnostics::{Counters, Reason};
pub use draft::PreviewDraft;
pub use mailbox::Delivery;
use mailbox::{Mailbox, Order, Worker};
use pace::{Pace, Trend};
use play::PlaySession;
pub use play::{PlayInfo, PlayKey, PlayTally};
use prime::{Primed, Primer};
pub use snapshot::Taken;
use snapshot::{PlanInputs, PlanSnapshots};

/// The widest frame the GPU preview renders; a bigger panel is drawn from this, scaled up.
pub const MAX_RENDER_WIDTH: u32 = 1920;

/// What the surface shows around the picture when the page does not say: the app background.
const DEFAULT_MATTE: [u8; 3] = [0x0f, 0x13, 0x18];

/// How a surface gets into the Preview panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Technique {
    /// The main window's surface, under a transparent webview.
    Window,
    /// A borderless child window of the main window, placed over the panel (X11).
    Child,
}

impl Technique {
    pub fn name(self) -> &'static str {
        match self {
            Technique::Window => "window",
            Technique::Child => "child",
        }
    }

    /// Whether the page can draw over the picture: under a transparent webview it can, over a
    /// child window it cannot.
    pub fn overlays(self) -> bool {
        self == Technique::Window
    }
}

/// What a platform gets: its technique, or why it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub technique: Option<Technique>,
    pub note: Option<String>,
}

/// The technique for `os` (`std::env::consts::OS`), unless `choice` (`KERF_GPU_SURFACE`) says
/// `off`, `window` or `child`.
///
/// * **Windows** — the window's own surface. The webview's background is made transparent at
///   runtime (`WebviewWindow::set_background_color`, alpha 0); tao does not clip children, so
///   the swapchain on the parent HWND shows through. Unconfirmed: needs a Windows machine.
/// * **Linux** — a child X11 window. Confirmed under WSLg/Xwayland with Mesa lavapipe.
/// * **macOS** — none until it has been run on a Mac. Tauri's `macos-private-api` is *not* the
///   obstacle (it gates only the `transparent` option; the webview is already see-through wherever
///   the page paints nothing). What is unproven is where wgpu's Metal layer ends up relative to
///   the WKWebView and how it is removed again. `KERF_GPU_SURFACE=window` forces the attempt for
///   whoever can run it; it may cover the window (`.claude/docs/app.md`, *macOS presenter*). The
///   note is shown to the user as it stands, so it does not name the variable.
pub fn resolve_technique(os: &str, choice: Option<&str>) -> Resolution {
    let some = |technique| Resolution {
        technique: Some(technique),
        note: None,
    };
    let none = |why: &str| Resolution {
        technique: None,
        note: Some(why.to_string()),
    };
    match choice.map(str::trim).filter(|c| !c.is_empty()) {
        Some("off") => return none("turned off by KERF_GPU_SURFACE=off"),
        Some("window") => return some(Technique::Window),
        Some("child") if os == "linux" => return some(Technique::Child),
        Some("child") => return none("a child window is only implemented for X11 (KERF_GPU_SURFACE=child)"),
        Some(other) => tracing::warn!(
            value = other,
            "KERF_GPU_SURFACE is not off, window or child; using the platform default"
        ),
        None => {}
    }
    match os {
        "windows" => some(Technique::Window),
        "linux" => some(Technique::Child),
        "macos" => none("the macOS preview surface has not been verified on a Mac yet, so it is not used"),
        other => none(&format!("no native preview surface on {other}")),
    }
}

/// The Preview frame's place in the window, as the webview reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct BoundsReport {
    /// The frame's rectangle in **device pixels**, relative to the webview's top-left corner.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// The webview's own size in device pixels, so the rectangle can be mapped onto a surface
    /// whose size differs by a rounding (or a zoom the page did not account for).
    pub viewport_width: f64,
    pub viewport_height: f64,
    /// Whether the surface should be showing: false while a stream plays, the panel is hidden or
    /// folded, or there is nothing to show.
    pub visible: bool,
    /// The colour around the picture, `#rrggbb` (the theme's `--frame-matte`).
    pub matte: Option<String>,
    /// The colour of the rest of the surface, `#rrggbb` (the theme's `--surface-app`): what a
    /// transparent webview shows where no element of the page paints.
    #[serde(default)]
    pub backdrop: Option<String>,
    /// Which report this is, counting up across every Preview the page has had: reports from a
    /// panel that was just replaced (a workspace switch) can arrive after the new one's, and the
    /// older one is ignored. `0` (no number) is always taken.
    #[serde(default)]
    pub seq: u64,
}

/// [`BoundsReport`], sanitised: whole pixels, finite, clamped to a sane range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub rect: PixelRect,
    pub viewport: (u32, u32),
    pub visible: bool,
    pub matte: [u8; 3],
    pub backdrop: [u8; 3],
}

/// The largest coordinate taken as real: bigger is a bug in the caller, not a monitor.
const MAX_COORD: f64 = 32_768.0;

fn whole(v: f64) -> Option<u32> {
    (v.is_finite() && (0.0..=MAX_COORD).contains(&v)).then(|| v.round() as u32)
}

/// `#rrggbb` (or `rrggbb`) as bytes.
pub fn parse_matte(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().trim_start_matches('#');
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| {
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
        [byte(0), byte(2), byte(4)]
    })
}

impl Bounds {
    /// `None` for a report that cannot be a rectangle (not finite, negative, absurd). An empty
    /// rectangle is a valid report of a surface that is not showing.
    pub fn from_report(r: &BoundsReport) -> Option<Bounds> {
        let rect = PixelRect {
            x: whole(r.x)?,
            y: whole(r.y)?,
            width: whole(r.width)?,
            height: whole(r.height)?,
        };
        let viewport = (whole(r.viewport_width)?, whole(r.viewport_height)?);
        Some(Bounds {
            rect,
            viewport,
            visible: r.visible && rect.width > 0 && rect.height > 0,
            matte: r.matte.as_deref().and_then(parse_matte).unwrap_or(DEFAULT_MATTE),
            backdrop: r.backdrop.as_deref().and_then(parse_matte).unwrap_or(DEFAULT_MATTE),
        })
    }
}

/// `rect`, given in a `from`-sized space, in a `to`-sized one (edges rounded, so neighbours
/// never gap). The identity when the sizes agree or `from` is unknown.
pub fn scaled(rect: PixelRect, from: (u32, u32), to: (u32, u32)) -> PixelRect {
    if from == to || from.0 == 0 || from.1 == 0 {
        return rect;
    }
    let edge = |v: u32, from: u32, to: u32| ((u64::from(v) * u64::from(to) + u64::from(from) / 2) / u64::from(from)) as u32;
    let (x0, x1) = (
        edge(rect.x, from.0, to.0),
        edge(rect.x.saturating_add(rect.width), from.0, to.0),
    );
    let (y0, y1) = (
        edge(rect.y, from.1, to.1),
        edge(rect.y.saturating_add(rect.height), from.1, to.1),
    );
    PixelRect {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

/// The largest rectangle of `inner`'s aspect inside `area`, centred: how a letterboxed picture
/// sits in its frame (`object-fit: contain`).
pub fn fit_contain(inner: (u32, u32), area: PixelRect) -> PixelRect {
    if inner.0 == 0 || inner.1 == 0 || area.width == 0 || area.height == 0 {
        return area;
    }
    let (iw, ih) = (u64::from(inner.0), u64::from(inner.1));
    let (aw, ah) = (u64::from(area.width), u64::from(area.height));
    // Width-bound when the picture is relatively wider than the area.
    let (w, h) = if iw * ah >= ih * aw {
        (aw, (aw * ih + iw / 2) / iw)
    } else {
        ((ah * iw + ih / 2) / ih, ah)
    };
    let (w, h) = (w.clamp(1, aw) as u32, h.clamp(1, ah) as u32);
    PixelRect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Where a `canvas`-sized frame goes in `area`, and the width to render it at: about the width
/// it is shown at (so the presenter draws it 1:1 whenever the panel is not wider than the cap),
/// and one whose render size keeps the canvas's shape (see [`exact_width`]). `size_for` is the
/// size a render at a width comes out (`RenderPlan::size`).
pub fn place_frame(canvas: (u32, u32), area: PixelRect, size_for: impl Fn(u32) -> (u32, u32)) -> (u32, PixelRect) {
    let dest = fit_contain(canvas, area);
    (exact_width(canvas, dest.width, size_for), dest)
}

/// How far a render at width `w` is from the canvas's shape, in rows: the ideal height minus the
/// one `size_for` gives. Zero when the sizes are exact.
fn shape_error(canvas: (u32, u32), w: u32, size_for: &impl Fn(u32) -> (u32, u32)) -> f64 {
    let (rw, rh) = size_for(w);
    (f64::from(rw) * f64::from(canvas.1) / f64::from(canvas.0.max(1)) - f64::from(rh)).abs()
}

/// The render width nearest `target` (even, at most [`MAX_RENDER_WIDTH`], within a few dozen
/// pixels of it) whose size has the canvas's shape to the row. A preview size is the delivery
/// aspect at an even height rounded down, which at 430 px is 240 rows for a 241.9 ideal: the
/// compositor then letterboxes the footage into a canvas a hair taller than its shape, and a
/// pillar a couple of pixels wide shows. Another width of the same size class has none.
pub fn exact_width(canvas: (u32, u32), target: u32, size_for: impl Fn(u32) -> (u32, u32)) -> u32 {
    let cap = MAX_RENDER_WIDTH.min(canvas.0.max(2)) & !1;
    let target = target.clamp(2, cap.max(2)) & !1;
    let mut best = (shape_error(canvas, target, &size_for), target);
    for step in (2..=48u32).step_by(2) {
        if best.0 < 1e-9 {
            break;
        }
        for w in [target.checked_sub(step), target.checked_add(step)].into_iter().flatten() {
            if !(2..=cap).contains(&w) {
                continue;
            }
            let err = shape_error(canvas, w, &size_for);
            // Strictly better only: the nearer width wins a tie.
            if err < best.0 - 1e-9 {
                best = (err, w);
            }
        }
    }
    best.1
}

/// Where the surface is and what part of it the picture goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The surface's size in device pixels.
    pub surface: (u32, u32),
    /// The part of it the Preview frame occupies.
    pub area: PixelRect,
    /// The child window's rectangle in the webview (a [`Technique::Child`] only).
    pub window: Option<PixelRect>,
}

/// The layout for `bounds`: the whole window's surface with the frame's rectangle inside it
/// ([`Technique::Window`], `window_size` its inner size), or a surface exactly the frame
/// ([`Technique::Child`]). `None` for a rectangle that is not on the webview at all.
pub fn layout_for(technique: Technique, bounds: &Bounds, window_size: (u32, u32)) -> Option<Layout> {
    match technique {
        Technique::Window => {
            let surface = (window_size.0.max(1), window_size.1.max(1));
            let area = scaled(bounds.rect, bounds.viewport, surface).clamped(surface)?;
            Some(Layout {
                surface,
                area,
                window: None,
            })
        }
        Technique::Child => {
            let on_page = bounds.rect.clamped(if bounds.viewport.0 > 0 && bounds.viewport.1 > 0 {
                bounds.viewport
            } else {
                (u32::MAX, u32::MAX)
            })?;
            Some(Layout {
                surface: (on_page.width, on_page.height),
                area: PixelRect {
                    x: 0,
                    y: 0,
                    width: on_page.width,
                    height: on_page.height,
                },
                window: Some(on_page),
            })
        }
    }
}

/// Retry timing for a backend that failed to build or was lost: 5 s, doubling to 5 min. It is
/// forgiven only by a run of frames that reached the screen ([`Backoff::FORGIVEN_AFTER`]), not by
/// one: a device that is lost after every present would otherwise be rebuilt every other frame
/// with no wait at all. The first loss of a device that worked is rebuilt at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Backoff {
    strikes: u32,
    streak: u32,
}

impl Backoff {
    const BASE: Duration = Duration::from_secs(5);
    const MAX: Duration = Duration::from_secs(300);
    /// Consecutive frames shown before the strikes are wiped.
    pub const FORGIVEN_AFTER: u32 = 20;

    /// How long to wait after the `n`th failure in a row (the first loss of a working device
    /// is free: `n == 0` after it was reset).
    pub fn delay(strikes: u32) -> Duration {
        if strikes == 0 {
            return Duration::ZERO;
        }
        Self::BASE.saturating_mul(1u32 << (strikes - 1).min(10)).min(Self::MAX)
    }

    /// A failure: returns how long to wait before building again.
    pub fn fail(&mut self, immediate: bool) -> Duration {
        let wait = if immediate && self.strikes == 0 {
            Duration::ZERO
        } else {
            Self::delay(self.strikes + 1)
        };
        self.strikes = self.strikes.saturating_add(1);
        self.streak = 0;
        wait
    }

    /// A frame was shown.
    pub fn succeeded(&mut self) {
        self.streak = self.streak.saturating_add(1);
        if self.streak >= Self::FORGIVEN_AFTER {
            self.strikes = 0;
        }
    }

    #[cfg(test)]
    pub fn strikes(&self) -> u32 {
        self.strikes
    }
}

/// What to do about a GPU error on the frame path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// This frame only: the next one may work (a plan the compositor refuses, a busy decode).
    Frame,
    /// Drop the backend and rebuild it on the next use (the device or the surface is gone).
    Rebuild,
}

pub fn react(error: &GpuError) -> Reaction {
    match error {
        GpuError::DeviceLost(_) | GpuError::OutOfMemory | GpuError::Gpu(_) | GpuError::Device(_) | GpuError::NoAdapter(_) => {
            Reaction::Rebuild
        }
        // A surface that cannot be drawn to is rebuilt, except for the cases that mean "not
        // now": occluded and timed out are the window system saying skip this frame.
        GpuError::Surface(why) if why.contains("occluded") || why.contains("timed out") => Reaction::Frame,
        GpuError::Surface(_) => Reaction::Rebuild,
        GpuError::Unsupported(_) | GpuError::Decode(_) | GpuError::Busy(_) | GpuError::Readback(_) => Reaction::Frame,
    }
}

/// A fixed word for a GPU error, to count failures by.
fn error_kind(error: &GpuError) -> &'static str {
    match error {
        GpuError::NoAdapter(_) => "gpu_no_adapter",
        GpuError::Device(_) => "gpu_device",
        GpuError::DeviceLost(_) => "gpu_device_lost",
        GpuError::OutOfMemory => "gpu_out_of_memory",
        GpuError::Gpu(_) => "gpu_rejected",
        GpuError::Decode(_) => "decode",
        GpuError::Unsupported(_) => "gpu_unsupported",
        GpuError::Busy(_) => "decode_busy",
        GpuError::Readback(_) => "gpu_readback",
        GpuError::Surface(_) => "surface",
    }
}

/// Puts something back when it is dropped: the webview's opaque background, whichever way the
/// backend's build or life ends (an error after the webview was made transparent, a panic, the
/// setting going off).
struct RestoreOnDrop(Option<Box<dyn FnOnce() + Send>>);

impl RestoreOnDrop {
    fn new(restore: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(restore)))
    }
}

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        if let Some(restore) = self.0.take() {
            restore();
        }
    }
}

/// The surface's host: whatever has to stay alive, be placed and be shown or hidden with it.
enum Host {
    /// The main window; its webview background is transparent from [`Host::make_transparent`]
    /// until this is dropped.
    Window {
        window: Box<tauri::WebviewWindow>,
        restore: Option<RestoreOnDrop>,
    },
    #[cfg(target_os = "linux")]
    Child(Box<x11::Child>),
    /// Nothing at all, for tests that draw without a window.
    #[cfg(test)]
    Null,
}

impl Host {
    fn window(window: tauri::WebviewWindow) -> Self {
        Host::Window {
            window: Box::new(window),
            restore: None,
        }
    }

    fn technique(&self) -> Technique {
        match self {
            Host::Window { .. } => Technique::Window,
            #[cfg(target_os = "linux")]
            Host::Child(_) => Technique::Child,
            #[cfg(test)]
            Host::Null => Technique::Child,
        }
    }

    /// Make the webview show what is under it (the swapchain): its background alpha 0. Undone when
    /// the host is dropped, by any path. The last step of a build, so no later failure leaves the
    /// page transparent over nothing.
    fn make_transparent(&mut self) -> Result<(), String> {
        match self {
            Host::Window { window, restore } => {
                let [r, g, b] = DEFAULT_MATTE;
                window
                    .set_background_color(Some(tauri::window::Color(r, g, b, 0)))
                    .map_err(|err| format!("making the webview transparent: {err}"))?;
                let window = (**window).clone();
                *restore = Some(RestoreOnDrop::new(move || {
                    // The config's own `backgroundColor` (`#0f1318`), opaque again.
                    if let Err(e) = window.set_background_color(Some(tauri::window::Color(r, g, b, 255))) {
                        tracing::debug!(error = %e, "could not restore the webview background");
                    }
                }));
                Ok(())
            }
            #[cfg(target_os = "linux")]
            Host::Child(_) => Ok(()),
            #[cfg(test)]
            Host::Null => Ok(()),
        }
    }

    /// The size of the window a [`Technique::Window`] surface covers, in device pixels.
    fn window_size(&self) -> Option<(u32, u32)> {
        match self {
            Host::Window { window, .. } => window.inner_size().ok().map(|s| (s.width, s.height)),
            #[cfg(target_os = "linux")]
            Host::Child(_) => None,
            #[cfg(test)]
            Host::Null => None,
        }
    }

    /// Put the surface where `layout` says and show it.
    fn apply(&mut self, layout: &Layout) -> Result<(), String> {
        // Only a child window has to be put anywhere.
        let _ = layout;
        match self {
            Host::Window { .. } => Ok(()),
            #[cfg(target_os = "linux")]
            Host::Child(child) => {
                let r = layout.window.ok_or("a child window layout has a rectangle")?;
                child.place(r.x as i32, r.y as i32, r.width, r.height)
            }
            #[cfg(test)]
            Host::Null => Ok(()),
        }
    }

    /// The page's frame moved or resized: a child window that is on show follows it now.
    fn follow(&mut self, layout: &Layout) {
        let _ = layout;
        match self {
            Host::Window { .. } => {}
            #[cfg(test)]
            Host::Null => {}
            #[cfg(target_os = "linux")]
            Host::Child(child) => {
                if let Some(r) = layout.window {
                    if let Err(e) = child.follow(r.x as i32, r.y as i32, r.width, r.height) {
                        tracing::debug!(error = %e, "could not move the GPU preview window");
                    }
                }
            }
        }
    }

    /// Stop showing the surface, so what the page draws there is what is seen.
    fn hide(&mut self) {
        match self {
            Host::Window { .. } => {}
            #[cfg(test)]
            Host::Null => {
                HIDES.fetch_add(1, Ordering::SeqCst);
            }
            #[cfg(target_os = "linux")]
            Host::Child(child) => child.hide(),
        }
    }

    fn release(&mut self) {
        match self {
            // Dropping the guard restores the background.
            Host::Window { restore, .. } => drop(restore.take()),
            #[cfg(test)]
            Host::Null => {}
            #[cfg(target_os = "linux")]
            Host::Child(child) => child.hide(),
        }
    }
}

/// How many times a test host was asked to hide the surface.
#[cfg(test)]
static HIDES: AtomicUsize = AtomicUsize::new(0);

/// What a frame is shown with: the presenter and its surface. A trait so the frame path can be
/// driven without a window (the tests' screen records what it was asked to do).
trait Screen: Send {
    /// Follow the surface's new size.
    fn resize(&mut self, size: (u32, u32)) -> Result<(), GpuError>;
    /// Present without waiting for the refresh (`on`: frames of a drag) or with vsync; a surface that
    /// cannot skip the wait stays as it is. Done before the frame is presented.
    fn skip_vsync(&mut self, on: bool) -> Result<(), GpuError>;
    fn present(&mut self, frame: &RenderedFrame, dest: PixelRect, surround: Surround) -> Result<(), GpuError>;
}

impl Screen for Presenter {
    fn resize(&mut self, size: (u32, u32)) -> Result<(), GpuError> {
        Presenter::resize(self, size)
    }

    fn skip_vsync(&mut self, on: bool) -> Result<(), GpuError> {
        Presenter::skip_vsync(self, on).map(|_| ())
    }

    fn present(&mut self, frame: &RenderedFrame, dest: PixelRect, surround: Surround) -> Result<(), GpuError> {
        Presenter::present(self, frame, dest, surround)
    }
}

/// The plan the preview draws for time `t`: the output frame whose slot holds it, at the slot's
/// start. The JPEG route counts the same grid (`ScrubFrames`), so the two routes show the same
/// frame for a time between two frames, not the source frame at `t` itself.
fn plan_at_grid(planner: &Planner, t: f64) -> kerf_core::Result<RenderPlan> {
    planner.at_frame(planner.canvas().fps.frame_containing(t))
}

/// How a still plan's layers are decoded: a drag that walks forward (`forward`, `pace::Trend`) reads
/// ahead like playback, unless a layer plays its footage backward. Its frames go the other way through
/// the file, so a run that reads ahead would restart at every frame and the frame source's thrash guard
/// would refuse it (`Busy`): that drag is decoded as a scrub, as it was before reading ahead.
fn decode_hint(plan: &RenderPlan, forward: bool) -> Hint {
    if forward && !plan.layers.iter().any(|l| l.timing.reversed && !l.is_image) {
        Hint::Forward {
            fps: f64::from(plan.canvas.fps.num) / f64::from(plan.canvas.fps.den.max(1)),
        }
    } else {
        Hint::Scrub
    }
}

/// Clears a flag when dropped, a panic included.
struct ClearOnDrop(Arc<AtomicBool>);

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// How far past the playhead, in seconds, the clips whose first frames are prefetched start.
const PREFETCH_HORIZON: f64 = 2.0;

/// How many upcoming clips are prefetched at a time.
const PREFETCH_CLIPS: usize = 3;

/// The same clips are not asked for again within this long.
const PREFETCH_REPEAT: Duration = Duration::from_secs(1);

/// The times, in `(t, t + horizon]`, at which a clip of the cut starts on a video track that
/// renders: the moments a cut brings a frame the decoders have not been reading, nearest first, at
/// most `limit`, each once (two tracks starting together are one moment). Muted and solo-shadowed
/// tracks and disabled clips are not the picture.
pub fn upcoming_clip_starts(timeline: &Timeline, t: f64, horizon: f64, limit: usize) -> Vec<f64> {
    if !t.is_finite() || !horizon.is_finite() || horizon <= 0.0 {
        return Vec::new();
    }
    let mut starts: Vec<f64> = timeline
        .tracks
        .iter()
        .filter(|track| track.kind == StreamKind::Video && timeline.track_renders(track))
        .flat_map(|track| track.clips.iter())
        .filter(|clip| clip.enabled)
        .map(|clip| clip.timeline_start)
        // Strictly past `t`, by a thousandth of a frame at 30 fps: the clip already on screen is
        // being read, and the frame at its first instant is not "upcoming".
        .filter(|&start| start > t + 1e-5 && start <= t + horizon)
        .collect();
    starts.sort_by(f64::total_cmp);
    starts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    starts.truncate(limit);
    starts
}

/// The times, in `[t - horizon, t)`, at which a clip of the cut has just ended on a video track that
/// renders: the moment is the clip's **last frame** (half a frame before its end), the picture a
/// playhead dragged back across the cut brings next. Nearest first, at most `limit`, each once.
/// A clip the playhead is inside (its end is ahead) or sits exactly at the end of is not behind it.
pub fn passed_clip_ends(timeline: &Timeline, t: f64, horizon: f64, frame: f64, limit: usize) -> Vec<f64> {
    if !t.is_finite() || !horizon.is_finite() || horizon <= 0.0 || !frame.is_finite() || frame <= 0.0 {
        return Vec::new();
    }
    let mut ends: Vec<f64> = timeline
        .tracks
        .iter()
        .filter(|track| track.kind == StreamKind::Video && timeline.track_renders(track))
        .flat_map(|track| track.clips.iter())
        .filter(|clip| clip.enabled)
        .map(|clip| clip.timeline_end())
        .filter(|&end| end < t - 1e-5 && end >= t - horizon)
        .collect();
    ends.sort_by(|a, b| b.total_cmp(a));
    ends.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    ends.truncate(limit);
    ends.into_iter().map(|end| end - frame * 0.5).collect()
}

/// A moment a playhead near `t` is about to reach a cut: the first frame of a clip ahead of it, or the
/// last frame of one just behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutMoment {
    pub t: f64,
    pub behind: bool,
}

/// The cuts around the playhead, nearest first and at most `limit` in all: a clip that starts
/// within `horizon` ahead ([`upcoming_clip_starts`]) and one that ended within `horizon` behind
/// ([`passed_clip_ends`]). A drag goes either way.
pub fn cut_moments(timeline: &Timeline, t: f64, horizon: f64, frame: f64, limit: usize) -> Vec<CutMoment> {
    let mut moments: Vec<CutMoment> = upcoming_clip_starts(timeline, t, horizon, limit)
        .into_iter()
        .map(|t| CutMoment { t, behind: false })
        .chain(
            passed_clip_ends(timeline, t, horizon, frame, limit)
                .into_iter()
                .map(|t| CutMoment { t, behind: true }),
        )
        .collect();
    moments.sort_by(|a, b| (a.t - t).abs().total_cmp(&(b.t - t).abs()));
    moments.truncate(limit);
    moments
}

/// Everything a GPU frame needs, built together and dropped together.
struct Backend {
    // Field order is drop order: the presenter (and its surface) go before the window host.
    screen: Box<dyn Screen>,
    /// Shared: a frame draws with it after the render lock is let go of.
    compositor: Arc<Compositor>,
    source: Arc<FrameSource>,
    gpu: Arc<Gpu>,
    /// What the self-test's verdict is about, made when the backend is.
    adapter: AdapterKey,
    host: Arc<Mutex<Host>>,
}

/// What a frame draws with, held by the frame itself while the backend stays under the render lock.
/// The backend it came from is told by `source` (a new backend has a new frame source).
struct Drawing {
    compositor: Arc<Compositor>,
    source: Arc<FrameSource>,
    gpu: Arc<Gpu>,
    adapter: AdapterKey,
    host: Arc<Mutex<Host>>,
}

impl Drop for Backend {
    fn drop(&mut self) {
        // The frame source kills its decodes on drop; the host gives the window back.
        self.source.release_all();
        lock(&self.host).release();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the status bar and the Settings dialog read.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct GpuPreviewStatus {
    /// The setting.
    pub enabled: bool,
    /// The platform has a technique (it may still fail to build).
    pub supported: bool,
    pub technique: Option<&'static str>,
    /// A device and surface are up.
    pub ready: bool,
    /// The page can draw over the picture (titles, guides, the trim monitor).
    pub overlays: bool,
    pub adapter: Option<String>,
    pub software: bool,
    /// Why the GPU preview is not in use: no technique here, the last failure, or that it was
    /// turned off because the last run ended while it was starting.
    pub reason: Option<String>,
}

#[derive(Default)]
struct Info {
    ready: bool,
    adapter: Option<String>,
    software: bool,
    overlays: bool,
    reason: Option<String>,
}

#[derive(Default)]
struct RenderState {
    backend: Option<Backend>,
    backoff: Backoff,
    retry_at: Option<Instant>,
    /// The backend has not yet put a frame on the screen: the crash marker is still down.
    unproven: bool,
}

/// Where the page says its frame is, and which report that came from.
#[derive(Default)]
struct BoundsState {
    current: Option<Bounds>,
    seq: u64,
}

/// Builds a backend (the app's: a window, a device and a surface on it). Injected so the
/// failure paths are testable without a window.
type Factory = dyn Fn(Technique) -> Result<Backend, String> + Send + Sync;

/// How long a backend may take to come up before the preview gives up on it for now and shows the
/// JPEG. Device and surface creation is a few hundred milliseconds on a working machine; this is
/// for the one where something stalls (a driver, an X server that does not answer).
const BUILD_DEADLINE: Duration = Duration::from_secs(10);

/// The file whose presence says the last run ended while the GPU preview was starting.
const CRASH_MARKER: &str = "gpu-preview-attempt";

/// What the status says after the marker was found.
const CRASH_NOTE: &str = "turned off: the last run ended while the GPU preview was starting (a driver crash?); \
     switching it on again tries once more";

/// Whether the previous run left its marker (and remove it). Called once at launch, before the
/// settings are read: the run that died during the first GPU frame must not repeat it on every launch.
pub fn take_crash_marker(dir: &Path) -> bool {
    let path = dir.join(CRASH_MARKER);
    let found = path.exists();
    if found {
        let _ = std::fs::remove_file(&path);
    }
    found
}

/// [`take_crash_marker`] in the app's config directory.
pub fn take_crash_marker_in(app: &tauri::AppHandle) -> bool {
    use tauri::Manager as _;
    app.path().app_config_dir().is_ok_and(|dir| take_crash_marker(&dir))
}

/// What one GPU frame took.
#[derive(Debug, Clone, Copy, Serialize, Default, PartialEq)]
pub struct GpuTimings {
    pub width: u32,
    pub height: u32,
    pub decode_ms: f64,
    pub composite_ms: f64,
    pub present_ms: f64,
}

/// What `get_preview_frame` answers: which renderer made the frame, and the JPEG when it was
/// FFmpeg.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreviewFrameResult {
    /// `"gpu"` (drawn in the native surface; the webview shows it by showing nothing over it),
    /// `"ffmpeg"` (the `frame` data URL), `"superseded"` (a newer request replaced this one before
    /// it was drawn: nothing was, and the page ignores the answer) or, to a forward request, `"stream"`
    /// (the GPU does not play this session, or no longer: the page plays the stream from
    /// `play.ahead.handover`, or from the playhead when there is none).
    pub renderer: &'static str,
    pub frame: Option<String>,
    /// Why the GPU did not draw it (empty when it did).
    pub reasons: Vec<String>,
    pub timings: Option<GpuTimings>,
    /// To a forward request: the session's state (`None` for every other answer).
    pub play: Option<PlayInfo>,
}

impl PreviewFrameResult {
    /// The answer of a request that a newer one replaced.
    pub fn superseded() -> Self {
        Self {
            renderer: "superseded",
            frame: None,
            reasons: Vec::new(),
            timings: None,
            play: None,
        }
    }
}

/// Take the inputs of a frame: what needs the project lock, in one pass. Call under the lock;
/// nothing slow happens here, and [`Taken::resolve`] does the rest without it.
pub fn take_inputs(project: &Project) -> Result<Taken, String> {
    Taken::from(project)
}

/// Takes the inputs of a frame from the project (the app's: under `lock_user`), called only when
/// the held snapshot is stale.
pub type TakeInputs = Arc<dyn Fn() -> Result<Taken, String> + Send + Sync>;

/// One `get_preview_frame`.
pub struct FrameRequest {
    take: TakeInputs,
    t: f64,
    ffmpeg_width: u32,
    overlays: bool,
    arrived: Instant,
    /// Frames are being dragged (how fast requests arrive): set by [`GpuPreview::post`].
    scrubbing: bool,
    /// The drag walks forward through the cut: set by [`GpuPreview::post`].
    forward: bool,
    /// What the gesture under way would write: the frame is drawn as the cut would be ([`draft`]).
    draft: Option<PreviewDraft>,
    /// Which of the page's requests this is, when it says.
    order: Option<Order>,
    /// The play session this is a frame of (`hint = forward`), when it is.
    play: Option<PlayKey>,
}

impl FrameRequest {
    pub fn new(take: TakeInputs, t: f64, ffmpeg_width: u32, overlays: bool) -> Self {
        Self {
            take,
            t,
            ffmpeg_width,
            overlays,
            arrived: Instant::now(),
            scrubbing: false,
            forward: false,
            draft: None,
            order: None,
            play: None,
        }
    }

    /// This frame is drawn as the cut would be with `draft` written ([`draft`]): the frame of a gesture
    /// under way. An empty draft is no draft.
    pub fn drafted(mut self, draft: PreviewDraft) -> Self {
        self.draft = (!draft.is_empty()).then_some(draft);
        self
    }

    /// This is a frame of play session `session` of the page `client`: the GPU plays the cut forward
    /// from the first of them, or says that the stream does ([`play`]).
    pub fn forward(mut self, client: impl Into<String>, session: u64) -> Self {
        self.play = Some(PlayKey {
            client: client.into(),
            session,
        });
        self
    }

    /// This is request `seq` of the page `client`: one that arrives after a newer request of the
    /// same page is not drawn.
    pub fn ordered(mut self, client: impl Into<String>, seq: u64) -> Self {
        self.order = Some(Order {
            client: client.into(),
            seq,
        });
        self
    }
}

/// The job that runs the worker. Dropped without having run (the executor refused it, or is gone),
/// the mailbox is handed back, or no later request would ever start a worker.
struct Starter(Option<Arc<GpuPreview>>);

impl Starter {
    fn run(mut self) {
        if let Some(preview) = self.0.take() {
            preview.drain();
        }
    }
}

impl Drop for Starter {
    fn drop(&mut self) {
        if let Some(preview) = self.0.take() {
            preview.mail.abandon();
        }
    }
}

/// What one frame is asked to be.
#[derive(Debug, Clone, Copy)]
struct Want {
    t: f64,
    /// The page must draw over the picture (a title box, the trim monitor, safe-area guides).
    overlays: bool,
    scrubbing: bool,
    /// Decode it like playback, reading ahead ([`Trend`]).
    forward: bool,
}

#[cfg(test)]
impl Want {
    fn at(t: f64) -> Self {
        Self {
            t,
            overlays: false,
            scrubbing: false,
            forward: false,
        }
    }
}

/// The cut a frame is drawn from.
enum Cut {
    /// The snapshot's.
    Held,
    /// The snapshot's with a gesture's draft in it.
    Drafted(Timeline),
    /// A draft that could not be drawn, and why: the frame is the snapshot's, through FFmpeg.
    Refused(String),
}

/// The last reason of a drag's frame that FFmpeg drew.
const DRAFT_NOTE: &str = "the picture is the cut as it was before the drag; the drag shows once it is let go";

/// How a `get_preview_frame` ended, for the command: the frame, or "a newer request took its place".
type Answer = Delivery<Result<PreviewFrameResult, String>>;

/// What the diagnostics answer (read by tests; no command serves them): what this session's frames did and why the others went
/// through FFmpeg, and what the machinery around them did.
#[cfg(test)]
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GpuDiagnostics {
    pub tally: diagnostics::Tally,
    /// How often the frames' inputs were served from the held snapshot and how often taken again.
    pub snapshots: snapshot::SnapshotStats,
    /// Requests posted, and how many a newer one replaced.
    pub requests: mailbox::MailboxStats,
    /// The parity self-test: running, passed (with its figures) or closed (and why); `None` before the
    /// first frame that wanted the GPU.
    pub self_test: Option<String>,
    pub adapter: Option<String>,
    pub software: bool,
    /// What the play sessions did: how many the GPU played and how many the stream did.
    pub play: PlayTally,
}

enum Attempt {
    Presented(GpuTimings),
    Fallback(Vec<Reason>),
}

/// A width and a height, pixels.
type Dims = (u32, u32);

/// How a drawn frame ended ([`GpuPreview::conclude`]).
enum Concluded {
    /// On the screen, drawn in `frame` (still on the GPU).
    Shown(GpuTimings, RenderedFrame),
    /// The surface was hidden (or the setting went off) while the frame was drawn.
    Hidden,
    /// The backend was replaced while the frame was drawn.
    Restarted,
    /// The frame was not drawn or not shown; a device or surface that is gone has been dropped.
    Failed(GpuError),
}

/// Why a backend did not come up.
enum BuildFailure {
    /// It said no (or panicked): the marker is not needed.
    Failed(String),
    /// It did not answer in time: it may still be running, and the marker stays.
    TimedOut(String),
}

/// The text of a panic payload.
fn panic_text(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".into())
}

/// The GPU preview: the setting, the surface's place, and the backend that draws into it.
///
/// **Locks**, outermost first (a thread holding one may take those after it, never before):
/// `render` (a frame in flight; the backend) → `bounds` (where the page says the frame is; held
/// while a picture is shown, so a hide cannot land in the middle) → `host` (the option) → the
/// host's own mutex. `building` (one backend build at a time) is taken alone, never under `render`.
/// `render` → `info` (the build's note) → `play_tally` / `play` (the play session's slot: `keep_play` stores
/// under `render`, `diagnostics` reads `play` under `info`), and `primed` → `play` (priming stores under the
/// lock a play takes its primed session from, after looking at the slot). `play` is never held while
/// taking any other of these, and the primer's own state is a leaf.
pub struct GpuPreview {
    enabled: AtomicBool,
    /// Turning it on is a retry: the next frame forgets the backoff.
    reset_backoff: AtomicBool,
    technique: Resolution,
    factory: Arc<Factory>,
    /// The FFmpeg's composite colour policy (a measured fact; a field so a test can pin it).
    policy: fn() -> CompositeColorPolicy,
    build_deadline: Duration,
    /// The crash marker's path (the app's config directory), when there is one.
    marker: Option<PathBuf>,
    /// Where the JPEG route gets its frames: the cache the Preview's other route reads.
    scrub: Arc<ScrubFrames>,
    /// What the frame source was last asked to have ready for the clips past a cut, and when.
    prefetched: Mutex<(Vec<u64>, Option<Instant>)>,
    /// A prefetch is under way: the next is not started on top of it.
    prefetching: Arc<AtomicBool>,
    /// The inputs of the frames being drawn, held between frames (see [`snapshot`]).
    snapshots: PlanSnapshots,
    /// The play session, between its frames (taken out while one is drawn): see [`play`].
    play: Mutex<Option<PlaySession>>,
    /// Forward requests being served: with a held session, a play that priming leaves the playhead to.
    serving: AtomicUsize,
    /// How long a session may go unasked for, milliseconds ([`play::IDLE`]; a field so a test can shorten it).
    idle_ms: AtomicU64,
    /// What the sessions that ended did.
    play_tally: Mutex<PlayTally>,
    /// The session from the playhead the paused frame is at, made ready (see [`prime`]).
    primed: Mutex<Option<Primed>>,
    primer: Arc<Primer>,
    /// This preview, for the thread that primes (set by the first [`GpuPreview::post`]).
    me: OnceLock<Weak<GpuPreview>>,
    /// Frame requests: only the newest is drawn.
    mail: Mailbox<FrameRequest, Result<PreviewFrameResult, String>>,
    /// Whether requests are arriving like a drag, and which way it goes.
    pace: Mutex<(Pace, Trend)>,
    counters: Mutex<Counters>,
    /// The parity self-test's standing, and what runs it.
    check: Arc<Check>,
    checker: Arc<Checker>,
    /// Told when the self-test has a verdict, so the page asks for the frame again.
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
    bounds: Mutex<BoundsState>,
    /// The live host, outside the render lock so hiding the surface never waits for a render.
    host: Mutex<Option<Arc<Mutex<Host>>>>,
    /// The backend. **Not held while a frame decodes or renders**: a frame takes what it draws with
    /// (`Drawing`), lets go, and takes the lock again to show the result.
    render: Mutex<RenderState>,
    building: Mutex<()>,
    info: Arc<Mutex<Info>>,
    /// Test seam: runs just before a frame decodes, with the render lock free.
    #[cfg(test)]
    before_decode: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

/// Runs the parity self-test on a device (injected so the gate is testable without an FFmpeg).
type Checker = dyn Fn(Arc<Gpu>) -> Result<SelfTestReport, SelfTestError> + Send + Sync;

impl Drop for GpuPreview {
    fn drop(&mut self) {
        self.primer.stop();
    }
}

impl GpuPreview {
    /// The app's: `app`'s main window is the host. `crashed` is [`take_crash_marker_in`]'s answer.
    pub fn for_app(app: tauri::AppHandle, crashed: bool, scrub: Arc<ScrubFrames>) -> Self {
        use tauri::Emitter as _;
        use tauri::Manager as _;

        let choice = std::env::var("KERF_GPU_SURFACE").ok();
        let marker = app.path().app_config_dir().ok().map(|dir| dir.join(CRASH_MARKER));
        let notify = app.clone();
        let mut preview = Self::with_factory(
            resolve_technique(std::env::consts::OS, choice.as_deref()),
            Arc::new(move |technique| build_backend(&app, technique)),
        );
        preview.scrub = scrub;
        preview.marker = marker;
        // The page asks for the frame under the playhead again (the one that waited for the check
        // went through FFmpeg), and reads the status the verdict changed.
        preview.notify = Some(Arc::new(move || {
            if let Err(e) = notify.emit("gpu-preview-checked", ()) {
                tracing::warn!(error = %e, "failed to emit gpu-preview-checked");
            }
        }));
        if crashed {
            lock(&preview.info).reason = Some(CRASH_NOTE.into());
        }
        preview
    }

    fn with_factory(technique: Resolution, factory: Arc<Factory>) -> Self {
        Self {
            policy: composite_color_policy,
            build_deadline: BUILD_DEADLINE,
            marker: None,
            scrub: Arc::new(ScrubFrames::new(ScrubConfig::default())),
            prefetched: Mutex::new((Vec::new(), None)),
            prefetching: Arc::new(AtomicBool::new(false)),
            snapshots: PlanSnapshots::default(),
            play: Mutex::new(None),
            serving: AtomicUsize::new(0),
            idle_ms: AtomicU64::new(play::IDLE.as_millis() as u64),
            play_tally: Mutex::new(PlayTally::default()),
            primed: Mutex::new(None),
            primer: Primer::new(),
            me: OnceLock::new(),
            mail: Mailbox::default(),
            pace: Mutex::new((Pace::default(), Trend::default())),
            counters: Mutex::new(Counters::default()),
            check: Arc::new(Check::default()),
            checker: Arc::new(|gpu| kerf_gpu::parity_self_test(&gpu)),
            notify: None,
            enabled: AtomicBool::new(false),
            reset_backoff: AtomicBool::new(false),
            info: Arc::new(Mutex::new(Info {
                reason: technique.note.clone(),
                ..Info::default()
            })),
            #[cfg(test)]
            before_decode: Mutex::new(None),
            technique,
            factory,
            bounds: Mutex::new(BoundsState::default()),
            host: Mutex::new(None),
            render: Mutex::new(RenderState::default()),
            building: Mutex::new(()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// The setting changed. The flag flips **now**, so the very next frame (and a frame already in
    /// flight, which looks again before it builds and before it shows anything) sees it; returns
    /// whether the surface and the device are now to be torn down ([`GpuPreview::teardown_if_off`],
    /// which may wait for that frame and so belongs off the caller's thread). Turning it on is a
    /// retry: the backoff of an earlier failure is forgotten.
    pub fn set_enabled(&self, on: bool) -> bool {
        let was = self.enabled.swap(on, Ordering::SeqCst);
        if on {
            self.reset_backoff.store(true, Ordering::SeqCst);
            if !was {
                lock(&self.info).reason = self.technique.note.clone();
                // A self-test that failed, whatever way, is run again.
                self.check.retry();
            }
        }
        was && !on
    }

    /// Hide the surface and free the device, unless the setting came back on meanwhile.
    pub fn teardown_if_off(&self) {
        if self.is_enabled() {
            return;
        }
        self.hide();
        self.teardown();
    }

    pub fn status(&self) -> GpuPreviewStatus {
        let info = lock(&self.info);
        GpuPreviewStatus {
            enabled: self.is_enabled(),
            supported: self.technique.technique.is_some(),
            technique: self.technique.technique.map(Technique::name),
            ready: info.ready,
            overlays: self.technique.technique.map_or(info.overlays, Technique::overlays),
            adapter: info.adapter.clone(),
            software: info.software,
            reason: info.reason.clone(),
        }
    }

    /// The Preview frame moved (or stopped being shown). Cheap, and it never waits for a *render*
    /// (only for a picture being put on the screen, a frame at most): a hidden frame hides the
    /// surface, a moved one moves it, and both are serialized with a frame in flight — a hide that
    /// arrives while a picture is being shown is not undone by it. The report of an older panel
    /// that arrives late is ignored.
    pub fn set_bounds(&self, report: &BoundsReport) -> Result<(), String> {
        let bounds = Bounds::from_report(report).ok_or("the preview bounds are not a rectangle on a screen")?;
        let mut state = lock(&self.bounds);
        if report.seq != 0 && report.seq < state.seq {
            tracing::debug!(
                seq = report.seq,
                newest = state.seq,
                "a stale preview bounds report was ignored"
            );
            return Ok(());
        }
        state.seq = state.seq.max(report.seq);
        let before = state.current.replace(bounds);
        if before.map(|b| b.visible) != Some(bounds.visible) {
            tracing::debug!(visible = bounds.visible, rect = ?bounds.rect, "preview surface visibility changed");
        }
        if bounds.visible {
            self.follow(&bounds);
        } else {
            self.hide();
        }
        Ok(())
    }

    /// Run `show` against the bounds as they are **now**, with `set_bounds` held off until it is
    /// done: `None` when the surface is no longer to be shown (hidden, or the setting went off), in
    /// which case `show` did not run. This is what makes a hide that lands during a long render
    /// stick, instead of being undone by the frame that was already on its way.
    fn under_bounds<T>(&self, show: impl FnOnce(&Bounds) -> T) -> Option<T> {
        let state = lock(&self.bounds);
        let current = state.current.filter(|b| b.visible)?;
        if !self.is_enabled() {
            return None;
        }
        Some(show(&current))
    }

    fn hide(&self) {
        let host = lock(&self.host).clone();
        if let Some(host) = host {
            lock(&host).hide();
        }
    }

    /// A child window that is on show follows the frame the page just moved.
    fn follow(&self, bounds: &Bounds) {
        let Some(technique @ Technique::Child) = self.technique.technique else {
            return;
        };
        let host = lock(&self.host).clone();
        let Some(host) = host else {
            return;
        };
        match layout_for(technique, bounds, (0, 0)) {
            Some(layout) => lock(&host).follow(&layout),
            None => lock(&host).hide(),
        }
    }

    /// Drop the backend (device, surface, decodes). The window host goes back to normal.
    fn teardown(&self) {
        tracing::info!(session = %lock(&self.counters).tally(self.mail.stats().superseded).line(), "the GPU preview's frames so far");
        self.snapshots.forget();
        self.end_play();
        self.drop_primed();
        *lock(&self.host) = None;
        let backend = lock(&self.render).backend.take();
        // A panic while dropping a broken backend must not take the caller with it.
        let _ = catch_unwind(AssertUnwindSafe(move || drop(backend)));
        self.clear_marker();
        let mut info = lock(&self.info);
        info.ready = false;
        info.adapter = None;
    }

    fn write_marker(&self) {
        if let Some(path) = &self.marker {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, std::process::id().to_string());
        }
    }

    fn clear_marker(&self) {
        if let Some(path) = &self.marker {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Ask for a frame. **Latest wins**: the request replaces the one waiting (which is answered
    /// [`Delivery::Superseded`] at once), the frame being drawn is finished, and the newest waiting
    /// request is drawn next. `run` starts the worker on a thread that may block (the app's: the
    /// blocking pool) when none is running; the answer comes on the receiver.
    pub fn post(
        self: &Arc<Self>,
        mut request: FrameRequest,
        run: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) -> oneshot::Receiver<Answer> {
        let _ = self.me.set(Arc::downgrade(self));
        // Frames of a play session are not a drag: they are shown with the refresh, and do not
        // teach the pace what a click after them is.
        if request.play.is_none() {
            let mut pace = lock(&self.pace);
            request.scrubbing = pace.0.observe(request.arrived);
            request.forward = pace.1.observe(request.t, request.scrubbing);
        }
        let order = request.order.take();
        let (answer, start) = self.mail.post(request, order);
        if start {
            let starter = Starter(Some(Arc::clone(self)));
            run(Box::new(move || starter.run()));
        }
        answer
    }

    /// The worker: draw what is posted, newest first, until nothing waits.
    fn drain(&self) {
        let mut worker = Worker::new(&self.mail);
        while let Some((request, reply)) = worker.next() {
            let answer = catch_unwind(AssertUnwindSafe(|| self.serve(&request))).unwrap_or_else(|payload| {
                Err(format!(
                    "the GPU preview's frame worker panicked: {}",
                    panic_text(payload.as_ref())
                ))
            });
            reply.send(answer);
        }
    }

    /// One request: its inputs (the held snapshot, or taken from the project once it is stale),
    /// the Proxy-only refusal, then the frame.
    fn serve(&self, request: &FrameRequest) -> Result<PreviewFrameResult, String> {
        let inputs = self.inputs(&request.take)?;
        if let Some(key) = &request.play {
            return self.play(&inputs, key, request.t, request.overlays);
        }
        self.end_play();
        // A primed play was made for the playhead of an earlier paused frame; this is another.
        self.drop_primed();
        if let Some(wait) = inputs.proxy_wait(request.t)? {
            return Err(wait);
        }
        let cut = match &request.draft {
            None => Cut::Held,
            Some(draft) => match draft.apply(&inputs.timeline, &inputs.assets) {
                Ok(timeline) => match inputs.proxy_wait_in(&timeline, request.t)? {
                    Some(wait) => Cut::Refused(wait),
                    None => Cut::Drafted(timeline),
                },
                Err(why) => Cut::Refused(why),
            },
        };
        let drafted = !matches!(cut, Cut::Held);
        let result = self.frame_in(
            &inputs,
            cut,
            Want {
                t: request.t,
                overlays: request.overlays,
                scrubbing: request.scrubbing,
                forward: request.forward,
            },
            request.ffmpeg_width,
        )?;
        // The playhead rests on a frame the GPU drew: the cut from here is made ready for Play. Not a
        // drag's frame: that cut is not the one Play would play.
        if result.renderer == "gpu" && !request.overlays && !drafted {
            self.prime_soon(&inputs, request.t);
        }
        Ok(result)
    }

    /// The inputs frames are planned from: the held snapshot while it is fresh, else taken from the
    /// project again (`take` is where the project lock is, and it is gone by the time this
    /// returns).
    fn inputs(&self, take: &TakeInputs) -> Result<Arc<PlanInputs>, String> {
        if let Some(held) = self.snapshots.fresh(Instant::now()) {
            return Ok(held);
        }
        let taken = take()?;
        Ok(self.snapshots.keep(taken.resolve()))
    }

    /// A frame for `want.t`: drawn by the GPU when the plan allows it and everything works, else
    /// FFmpeg's JPEG of `ffmpeg_width`. `want.overlays` is the page saying it must draw over the
    /// picture (a title box, the trim monitor, safe-area guides). A drag's frame (`cut` drafted) the
    /// GPU does not draw is FFmpeg's JPEG of the cut as it is: the drag shows once it is written, as it
    /// does with the GPU preview off.
    ///
    /// The project lock is not taken here at all. A panic in the GPU path (wgpu, the X connection) is
    /// caught here: the backend is dropped and rebuilt behind the backoff, the surface hidden, and the
    /// frame is FFmpeg's like any other fallback.
    fn frame_in(&self, inputs: &PlanInputs, cut: Cut, want: Want, ffmpeg_width: u32) -> Result<PreviewFrameResult, String> {
        // Whatever else is asked for (the paused frame, a scrub) is not part of a play session.
        self.end_play();
        let drafted = !matches!(cut, Cut::Held);
        let attempt = match &cut {
            Cut::Held => catch_unwind(AssertUnwindSafe(|| {
                self.attempt_in(inputs, &inputs.timeline, false, want, Instant::now())
            })),
            Cut::Drafted(timeline) => catch_unwind(AssertUnwindSafe(|| {
                self.attempt_in(inputs, timeline, true, want, Instant::now())
            })),
            Cut::Refused(why) => {
                self.hide();
                Ok(Attempt::Fallback(vec![Reason::new("draft", why.clone())]))
            }
        };
        let reasons = match attempt {
            Ok(Attempt::Presented(timings)) => {
                lock(&self.counters).presented();
                return Ok(PreviewFrameResult {
                    renderer: "gpu",
                    frame: None,
                    reasons: Vec::new(),
                    timings: Some(timings),
                    play: None,
                });
            }
            Ok(Attempt::Fallback(reasons)) => reasons,
            Err(payload) => vec![Reason::new("panic", self.after_panic(payload.as_ref()))],
        };
        for kind in lock(&self.counters).refused(&reasons) {
            let example = reasons
                .iter()
                .find(|r| r.kind == kind)
                .map(|r| r.text.as_str())
                .unwrap_or_default();
            tracing::info!(
                kind,
                example,
                "a preview frame went through FFmpeg for a reason not seen this session"
            );
        }
        let mut texts: Vec<String> = reasons.into_iter().map(|r| r.text).collect();
        if drafted {
            texts.push(DRAFT_NOTE.into());
        }
        tracing::debug!(reasons = ?texts, t = want.t, "preview frame through FFmpeg");
        let jpeg = self
            .scrub
            .frame(&inputs.timeline, &inputs.preview_assets, want.t, ffmpeg_width, 4)
            .map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg.as_slice());
        Ok(PreviewFrameResult {
            renderer: "ffmpeg",
            frame: Some(format!("data:image/jpeg;base64,{b64}")),
            reasons: texts,
            timings: None,
            play: None,
        })
    }

    /// What this session's frames did and why the others went through FFmpeg.
    #[cfg(test)]
    pub fn diagnostics(&self) -> GpuDiagnostics {
        let mail = self.mail.stats();
        let info = lock(&self.info);
        GpuDiagnostics {
            tally: lock(&self.counters).tally(mail.superseded),
            snapshots: self.snapshots.stats(),
            requests: mail,
            self_test: self.check.standing(),
            adapter: info.adapter.clone(),
            software: info.software,
            play: self.play_diagnostics(),
        }
    }

    /// The GPU path panicked: say so, hide the surface and drop the backend (it may be in any
    /// state), behind the backoff. Every step is itself guarded: this must not panic in turn.
    fn after_panic(&self, payload: &(dyn Any + Send)) -> String {
        let message = panic_text(payload);
        tracing::error!(%message, "the GPU preview panicked; dropping it, this frame goes through FFmpeg");
        let why = format!("the GPU preview panicked: {message}");
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.hide();
            self.drop_backend(&why, false, Instant::now(), None);
        }));
        why
    }

    /// Have the frame source ready for the cuts around the playhead: the first frame of a clip that
    /// starts within [`PREFETCH_HORIZON`] seconds ahead of `t` and the last of one that ended within
    /// that behind it ([`cut_moments`]; a drag goes either way) are asked for with
    /// [`Hint::Prefetch`], so a run is waiting (and the file's probes are done) by the time the
    /// playhead crosses the cut instead of the first frame there costing a process start. Called
    /// after a frame was shown, with the render lock released; the asking is on a thread of its own
    /// (the probes block), at most one at a time, and not again for the same moments within a second.
    /// It never takes a run another layer is reading (a spare slot, `Hint::Prefetch`), and a failure to
    /// prefetch is nothing: the frame is decoded when asked. A moment the GPU will refuse at this size
    /// (`caps`) is not prefetched: its frame goes through FFmpeg, and a run waiting for it would be a
    /// decode nobody reads.
    fn prefetch_ahead(
        &self,
        planner: &Planner,
        timeline: &Timeline,
        t: f64,
        size: (u32, u32),
        caps: &GpuCaps,
        source: Arc<FrameSource>,
    ) {
        let fps = planner.canvas().fps;
        let frame = f64::from(fps.den) / f64::from(fps.num.max(1));
        let moments = cut_moments(timeline, t, PREFETCH_HORIZON, frame, PREFETCH_CLIPS);
        if moments.is_empty() {
            return;
        }
        {
            let key: Vec<u64> = moments.iter().map(|m| m.t.to_bits()).collect();
            let mut last = lock(&self.prefetched);
            if last.0 == key && last.1.is_some_and(|at| at.elapsed() < PREFETCH_REPEAT) {
                return;
            }
            *last = (key, Some(Instant::now()));
        }
        let layers: Vec<PlanLayer> = moments
            .iter()
            .filter_map(|m| {
                if m.behind {
                    plan_at_grid(planner, m.t)
                } else {
                    planner.at(m.t)
                }
                .ok()
            })
            .filter(|plan| plan.reasons(caps, size).is_empty())
            .flat_map(|plan| plan.layers)
            .collect();
        if layers.is_empty() || self.prefetching.swap(true, Ordering::SeqCst) {
            return;
        }
        // Moved into the thread, so that whatever ends it, a panic or a thread that never started,
        // clears the flag.
        let clear = ClearOnDrop(Arc::clone(&self.prefetching));
        let _ = std::thread::Builder::new().name("kerf-gpu-prefetch".into()).spawn(move || {
            let _clear = clear;
            // `Hint::Prefetch` returns nothing for every layer; an error is only a layer not ready.
            if let Err(error) = source.frames(&layers, Hint::Prefetch) {
                tracing::debug!(%error, "prefetching the next clip");
            }
        });
    }

    /// Drop the backend, remember why, and wait before building another (`immediate`: a device that
    /// worked and was lost is rebuilt at once, the first time). With `only`, the backend is dropped
    /// only if it is still the one that frame source belongs to: a frame that failed on a backend
    /// that was replaced meanwhile must not take the new one down.
    fn drop_backend(&self, why: &str, immediate: bool, now: Instant, only: Option<&Arc<FrameSource>>) {
        let backend = {
            let mut state = lock(&self.render);
            if let Some(only) = only {
                if !state.backend.as_ref().is_some_and(|b| Arc::ptr_eq(&b.source, only)) {
                    return;
                }
            }
            let wait = state.backoff.fail(immediate);
            state.retry_at = Some(now + wait);
            state.unproven = false;
            state.backend.take()
        };
        *lock(&self.host) = None;
        self.end_play();
        self.drop_primed();
        let _ = catch_unwind(AssertUnwindSafe(move || drop(backend)));
        self.clear_marker();
        let mut info = lock(&self.info);
        info.ready = false;
        info.reason = Some(why.to_string());
    }

    /// May the GPU draw on `gpu`'s adapter? The first frame that asks starts the parity self-test on
    /// a thread of its own and is FFmpeg's, as is every frame until the verdict.
    fn gate(&self, gpu: &Arc<Gpu>, key: &AdapterKey) -> Result<(), Reason> {
        match self.check.gate(key) {
            Gate::Open => Ok(()),
            Gate::Wait(why) => Err(Reason::new("self_test_pending", why)),
            Gate::Closed(why) => Err(Reason::new("self_test_failed", why)),
            Gate::Begin(why) => {
                self.begin_check(key.clone(), Arc::clone(gpu));
                Err(Reason::new("self_test_pending", why))
            }
        }
    }

    /// Run the self-test off every lock and thread the page waits on. It runs FFmpeg the way the
    /// interactive paths do (thread-capped, no machine-budget slot) and takes about a second.
    fn begin_check(&self, key: AdapterKey, gpu: Arc<Gpu>) {
        let (check, checker, info, notify) = (
            Arc::clone(&self.check),
            Arc::clone(&self.checker),
            Arc::clone(&self.info),
            self.notify.clone(),
        );
        let adapter = gpu.adapter_info().name.clone();
        let started = {
            let (check, info, notify, key) = (Arc::clone(&check), Arc::clone(&info), notify.clone(), key.clone());
            std::thread::Builder::new().name("kerf-gpu-selftest".into()).spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| checker(gpu))).unwrap_or_else(|payload| {
                    Err(SelfTestError::Inconclusive(format!(
                        "the self-test panicked: {}",
                        panic_text(payload.as_ref())
                    )))
                });
                finish_check(&check, &info, notify.as_deref(), &key, &adapter, result);
            })
        };
        if let Err(e) = started {
            let verdict = check.failed_to_start(&key, &format!("could not start the self-test's thread: {e}"));
            if let Verdict::Failed(reason) = verdict {
                lock(&info).reason = Some(reason);
            }
        }
    }

    #[cfg(test)]
    fn frame(&self, inputs: &PlanInputs, want: Want, ffmpeg_width: u32) -> Result<PreviewFrameResult, String> {
        self.frame_in(inputs, Cut::Held, want, ffmpeg_width)
    }

    #[cfg(test)]
    fn attempt(&self, inputs: &PlanInputs, want: Want, now: Instant) -> Attempt {
        self.attempt_in(inputs, &inputs.timeline, false, want, now)
    }

    /// Draw `timeline` at `want.t` (the snapshot's, or a drag's draft of it: `drafted`, which is not
    /// prefetched around, since the next frame of a drag is the same time in another cut).
    fn attempt_in(&self, inputs: &PlanInputs, timeline: &Timeline, drafted: bool, want: Want, now: Instant) -> Attempt {
        let t = want.t;
        let fallback = |kind: &'static str, why: String| {
            self.hide();
            Attempt::Fallback(vec![Reason::new(kind, why)])
        };
        if !self.is_enabled() {
            return Attempt::Fallback(vec![Reason::new("preview_off", "the GPU preview is off")]);
        }
        let Some(technique) = self.technique.technique else {
            return Attempt::Fallback(vec![Reason::new(
                "no_surface",
                self.technique
                    .note
                    .clone()
                    .unwrap_or_else(|| "no preview surface here".into()),
            )]);
        };
        let Some(bounds) = lock(&self.bounds).current else {
            return Attempt::Fallback(vec![Reason::new("no_bounds", "the preview has not reported where it is")]);
        };
        if !bounds.visible {
            return fallback("hidden", "the preview surface is hidden".into());
        }
        if want.overlays && !technique.overlays() {
            return fallback(
                "overlays",
                "the page has something to draw over the picture, and this surface sits above the page".into(),
            );
        }

        // The plan, off the project lock.
        let planner = match Planner::new(
            timeline,
            &inputs.assets,
            &ExportOptions::default(),
            PlanRequest::still((self.policy)()).with_media(&ProxyMedia),
        ) {
            Ok(planner) => planner,
            Err(e) => return fallback("no_plan", format!("no plan for this frame: {e}")),
        };
        let plan = match plan_at_grid(&planner, t) {
            Ok(plan) => plan,
            Err(e) => return fallback("no_plan", format!("no plan for this frame: {e}")),
        };

        // What the plan needs measured (FFmpeg's `geq`, for a mask: one child process, once) is measured
        // here, off the render lock, not by the first masked frame under it.
        kerf_gpu::prepare(&plan);

        // A backend is built off the render lock, with a deadline: a slow or stuck build is a frame
        // that goes through FFmpeg, not a preview that waits for it.
        if let Err(why) = self.ensure_backend(technique, now) {
            return fallback("backend", why);
        }
        // What the frame draws with is taken under the render lock and kept without it: the lock is
        // not held while the frame decodes (which waits for FFmpeg) or renders, so turning the setting
        // off, a lost device or a build never waits for a frame.
        let drawing = match self.drawing() {
            Ok(drawing) => drawing,
            Err(reason) => return fallback(reason.kind, reason.text),
        };
        let host = Arc::clone(&drawing.host);

        let (canvas, size) = match self.sized(technique, &bounds, &host, &plan) {
            Ok(sized) => sized,
            Err(reason) => return fallback(reason.kind, reason.text),
        };
        let caps = drawing.compositor.caps();
        let reasons = plan.reasons(&caps, size);
        if !reasons.is_empty() {
            self.hide();
            return Attempt::Fallback(reasons.iter().map(|r| Reason::new(r.kind(), r.to_string())).collect());
        }
        // The GPU draws nothing the user looks at before it has been checked against this FFmpeg.
        if let Err(reason) = self.gate(&drawing.gpu, &drawing.adapter) {
            self.hide();
            return Attempt::Fallback(vec![reason]);
        }

        #[cfg(test)]
        if let Some(stall) = lock(&self.before_decode).clone() {
            stall();
        }
        let hint = decode_hint(&plan, want.forward);
        // A drag's frame keeps the poses of its titles apart from the cut's.
        let rendered = if drafted {
            drawing
                .compositor
                .render_draft_texture_with(&plan, size, &drawing.source, hint)
        } else {
            drawing
                .compositor
                .render_plan_texture_with(&plan, size, &drawing.source, hint)
        };

        match self.conclude(
            &drawing,
            technique,
            canvas,
            want,
            rendered,
            now,
            "this frame goes through FFmpeg",
            true,
        ) {
            Concluded::Shown(timings, _) => {
                tracing::debug!(?timings, t, drafted, "GPU preview frame presented");
                if !drafted {
                    self.prefetch_ahead(&planner, timeline, t, size, &caps, Arc::clone(&drawing.source));
                }
                Attempt::Presented(timings)
            }
            Concluded::Restarted => fallback("backend", "the GPU preview was restarted while the frame was drawn".into()),
            Concluded::Hidden => fallback("hidden", "the preview surface was hidden while the frame was drawn".into()),
            Concluded::Failed(error) => Attempt::Fallback(vec![Reason::new(error_kind(&error), error.to_string())]),
        }
    }

    /// What a frame draws with, taken under the render lock: `Err` when the setting went off since the
    /// frame was planned, or no backend is up.
    fn drawing(&self) -> Result<Drawing, Reason> {
        let state = lock(&self.render);
        // Looked at again under the lock: the setting may have gone off while this frame was planned.
        if !self.is_enabled() {
            return Err(Reason::new("preview_off", "the GPU preview was turned off"));
        }
        match state.backend.as_ref() {
            Some(b) => Ok(Drawing {
                compositor: Arc::clone(&b.compositor),
                source: Arc::clone(&b.source),
                gpu: Arc::clone(&b.gpu),
                adapter: b.adapter.clone(),
                host: Arc::clone(&b.host),
            }),
            None => Err(Reason::new("backend", "the GPU preview is not up")),
        }
    }

    /// Where a frame goes and how big it is drawn: the frame the plan's canvas is, the width
    /// nearest the panel's ([`place_frame`]) and the size that comes out at it. `Err` for a window
    /// with no size or a frame outside it.
    fn sized(
        &self,
        technique: Technique,
        bounds: &Bounds,
        host: &Arc<Mutex<Host>>,
        plan: &RenderPlan,
    ) -> Result<(Dims, Dims), Reason> {
        // A child window's surface is the frame itself; the window's covers the window.
        let window_size = match technique {
            Technique::Window => lock(host).window_size(),
            Technique::Child => Some((0, 0)),
        };
        let Some(window_size) = window_size else {
            return Err(Reason::new("no_window", "the window has no size"));
        };
        let Some(layout) = layout_for(technique, bounds, window_size) else {
            return Err(Reason::new("outside_window", "the preview frame is outside the window"));
        };
        let canvas = (plan.canvas.width, plan.canvas.height);
        let (width, _) = place_frame(canvas, layout.area, |w| plan.size(w));
        Ok((canvas, plan.size(width)))
    }

    /// A frame was drawn (or failed to be): back under the render lock, show it if the backend is
    /// still the one that drew it, and settle what the backend makes of the outcome — the backoff
    /// forgives a shown frame, the crash marker goes once one has reached the screen, a device or
    /// surface that is gone is dropped to be rebuilt, and a surface that was hidden meanwhile stays
    /// hidden. `declined` ends the log line of a frame the GPU did not draw; a failure hides the surface
    /// (the page shows FFmpeg's picture instead) unless `hide_on_failure` is off, as a play session's is.
    #[allow(clippy::too_many_arguments)]
    fn conclude(
        &self,
        drawing: &Drawing,
        technique: Technique,
        canvas: (u32, u32),
        want: Want,
        rendered: Result<(RenderedFrame, RenderTimings), GpuError>,
        now: Instant,
        declined: &str,
        hide_on_failure: bool,
    ) -> Concluded {
        let mut state = lock(&self.render);
        let same = state
            .backend
            .as_ref()
            .is_some_and(|b| Arc::ptr_eq(&b.source, &drawing.source));
        let outcome = match (rendered, state.backend.as_mut().filter(|_| same)) {
            (Err(error), _) => Err(error),
            (Ok(_), None) => {
                drop(state);
                return Concluded::Restarted;
            }
            // `None`: the surface was hidden (or the setting turned off) while the frame was drawn.
            (Ok((frame, timings)), Some(backend)) => self
                .show(backend, &drawing.host, technique, canvas, want, &frame, timings)
                .map(|shown| shown.map(|timings| (timings, frame))),
        };
        match outcome {
            Ok(Some((timings, frame))) => {
                state.backoff.succeeded();
                if std::mem::take(&mut state.unproven) {
                    // A frame reached the screen: whatever the first start risked, it survived.
                    self.clear_marker();
                }
                drop(state);
                Concluded::Shown(timings, frame)
            }
            Ok(None) => {
                drop(state);
                Concluded::Hidden
            }
            Err(error) => {
                let reaction = react(&error);
                // A frame the GPU declines (a busy decode, a plan it refuses at this size) is the
                // ordinary fallback; a device or surface that is gone is news.
                if reaction == Reaction::Frame {
                    tracing::debug!(%error, "GPU preview frame declined; {declined}");
                } else {
                    tracing::warn!(%error, "GPU preview failed; rebuilding it, and {declined}");
                }
                drop(state);
                if reaction == Reaction::Rebuild {
                    self.drop_backend(
                        &error.to_string(),
                        matches!(error, GpuError::DeviceLost(_)),
                        now,
                        Some(&drawing.source),
                    );
                }
                // A frame of a play session that fails leaves the surface as it is: the stream takes the
                // session over and the GPU's last picture stays until the stream's first frame is shown.
                if hide_on_failure || reaction == Reaction::Rebuild {
                    self.hide();
                }
                Concluded::Failed(error)
            }
        }
    }

    /// Put a drawn frame on the screen: the surface is sized and placed for the bounds as they are
    /// **now** (the frame may have moved during the render; the picture is scaled into where it is),
    /// with `set_bounds` held off until it is shown. `None`: the surface was hidden (or the setting
    /// turned off) while the frame was drawn.
    #[allow(clippy::too_many_arguments)]
    fn show(
        &self,
        backend: &mut Backend,
        host: &Arc<Mutex<Host>>,
        technique: Technique,
        canvas: (u32, u32),
        want: Want,
        frame: &RenderedFrame,
        timings: RenderTimings,
    ) -> Result<Option<GpuTimings>, GpuError> {
        let t0 = Instant::now();
        self.under_bounds(|now_bounds| -> Result<GpuTimings, GpuError> {
            let window_size = match technique {
                Technique::Window => lock(host).window_size(),
                Technique::Child => Some((0, 0)),
            }
            .ok_or_else(|| GpuError::Surface("the window has no size".into()))?;
            let layout = layout_for(technique, now_bounds, window_size)
                .ok_or_else(|| GpuError::Surface("the preview frame is outside the window".into()))?;
            let dest = fit_contain(canvas, layout.area);
            backend.screen.resize(layout.surface)?;
            // Frames of a drag are shown as soon as they are drawn, the settled one with vsync; a change
            // reconfigures the surface, which is why it is decided here, before the frame that is
            // presented into it.
            backend.screen.skip_vsync(want.scrubbing)?;
            // The surface is placed after the frame is drawn, just before it is shown: a child window
            // moved earlier would flash its previous contents.
            lock(host).apply(&layout).map_err(GpuError::Surface)?;
            backend.screen.present(
                frame,
                dest,
                Surround {
                    area: layout.area,
                    matte: now_bounds.matte,
                    backdrop: now_bounds.backdrop,
                },
            )?;
            Ok(GpuTimings {
                width: frame.width,
                height: frame.height,
                decode_ms: timings.decode.as_secs_f64() * 1e3,
                composite_ms: timings.composite.as_secs_f64() * 1e3,
                present_ms: t0.elapsed().as_secs_f64() * 1e3,
            })
        })
        .transpose()
    }

    /// Make sure a backend is up, building one if the backoff allows. A device that was lost is
    /// dropped here and rebuilt. The build runs **without the render lock** and with a deadline;
    /// a frame that finds one under way does not wait for it.
    fn ensure_backend(&self, technique: Technique, now: Instant) -> Result<(), String> {
        {
            let mut state = lock(&self.render);
            if self.reset_backoff.swap(false, Ordering::SeqCst) {
                state.backoff = Backoff::default();
                state.retry_at = None;
            }
            if state.backend.as_ref().is_some_and(|b| b.gpu.lost().is_some()) {
                let why = state.backend.as_ref().and_then(|b| b.gpu.lost()).unwrap_or_default();
                tracing::warn!(%why, "the GPU preview's device was lost; rebuilding it");
                let wait = state.backoff.fail(true);
                state.retry_at = Some(now + wait);
                *lock(&self.host) = None;
                let lost = state.backend.take();
                state.unproven = false;
                lock(&self.info).ready = false;
                let _ = catch_unwind(AssertUnwindSafe(move || drop(lost)));
            }
            if state.backend.is_some() {
                return Ok(());
            }
            if let Some(at) = state.retry_at.filter(|at| *at > now) {
                let reason = lock(&self.info)
                    .reason
                    .clone()
                    .unwrap_or_else(|| "the GPU preview failed".into());
                return Err(format!("{reason} (trying again in {} s)", (at - now).as_secs() + 1));
            }
        }
        let _building = match self.building.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Err("the GPU preview is starting".into()),
        };
        self.write_marker();
        let built = self.build_with_deadline(technique);
        let mut state = lock(&self.render);
        match built {
            Ok(backend) => {
                // The setting may have gone off while it was built, or another build got there first.
                if !self.is_enabled() || state.backend.is_some() {
                    drop(state);
                    let _ = catch_unwind(AssertUnwindSafe(move || drop(backend)));
                    self.clear_marker();
                    return Err("the GPU preview was turned off".into());
                }
                tracing::info!(
                    adapter = %backend.gpu.adapter_info().name,
                    software = backend.gpu.is_software(),
                    technique = technique.name(),
                    "GPU preview is up"
                );
                *lock(&self.host) = Some(Arc::clone(&backend.host));
                let mut info = lock(&self.info);
                info.ready = true;
                info.adapter = Some(backend.gpu.adapter_info().name.clone());
                info.software = backend.gpu.is_software();
                info.overlays = technique.overlays();
                // A verdict that stands for this adapter (a device lost and rebuilt) is still the reason.
                info.reason = self.check.closed(&backend.adapter);
                state.retry_at = None;
                // The marker stays down until a frame has reached the screen.
                state.unproven = true;
                state.backend = Some(backend);
                Ok(())
            }
            Err(failure) => {
                let (why, timed_out) = match failure {
                    BuildFailure::Failed(why) => (why, false),
                    BuildFailure::TimedOut(why) => (why, true),
                };
                if !timed_out {
                    self.clear_marker();
                }
                let wait = state.backoff.fail(false);
                state.retry_at = Some(now + wait);
                tracing::warn!(%why, retry_in_s = wait.as_secs(), "the GPU preview could not start; the JPEG preview is used");
                let mut info = lock(&self.info);
                info.ready = false;
                info.reason = Some(why.clone());
                Err(why)
            }
        }
    }

    /// Run the factory on a thread of its own and wait for it at most [`GpuPreview::build_deadline`].
    /// A build that outlives the deadline is left to finish (and is dropped, window and device
    /// with it, when it does); a panic in it is an error like any other.
    fn build_with_deadline(&self, technique: Technique) -> Result<Backend, BuildFailure> {
        let (tx, rx) = mpsc::channel();
        let factory = Arc::clone(&self.factory);
        std::thread::Builder::new()
            .name("kerf-gpu-build".into())
            .spawn(move || {
                let built = catch_unwind(AssertUnwindSafe(|| factory(technique)));
                // A receiver that gave up drops the backend here, on this thread.
                let _ = tx.send(built);
            })
            .map_err(|e| BuildFailure::Failed(format!("could not start the GPU preview's build thread: {e}")))?;
        match rx.recv_timeout(self.build_deadline) {
            Ok(Ok(built)) => built.map_err(BuildFailure::Failed),
            Ok(Err(payload)) => Err(BuildFailure::Failed(format!(
                "starting the GPU preview panicked: {}",
                panic_text(payload.as_ref())
            ))),
            Err(RecvTimeoutError::Timeout) => Err(BuildFailure::TimedOut(format!(
                "starting the GPU preview took longer than {} s",
                self.build_deadline.as_secs_f64().ceil()
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(BuildFailure::Failed(
                "the GPU preview's build thread ended without an answer".into(),
            )),
        }
    }
}

/// A self-test ended: record the verdict, log it, put a failure where Settings shows it, and tell the
/// page to ask for the frame again.
fn finish_check(
    check: &Check,
    info: &Mutex<Info>,
    notify: Option<&(dyn Fn() + Send + Sync)>,
    key: &AdapterKey,
    adapter: &str,
    result: Result<SelfTestReport, SelfTestError>,
) {
    match check.finish(key, result) {
        Verdict::Passed(summary) => {
            tracing::info!(adapter, %summary, "the GPU passed its self-test against this FFmpeg; the GPU preview is open");
        }
        Verdict::Failed(reason) => {
            tracing::warn!(adapter, %reason, "the GPU preview is off: its self-test against this FFmpeg did not pass");
            lock(info).reason = Some(reason);
        }
        Verdict::Stale => return,
    }
    if let Some(notify) = notify {
        notify();
    }
}

/// Whether `os` makes a [`Technique::Window`] surface on the main thread only. AppKit views belong
/// to it, and wgpu's Metal backend touches the content view (it makes it layer-backed and adds a
/// `CAMetalLayer` to it) the moment the surface is created, panicking anywhere else.
fn surface_on_main_thread(os: &str) -> bool {
    os == "macos"
}

/// How long the main thread gets to make a surface before the build gives up on it.
const MAIN_THREAD_DEADLINE: Duration = Duration::from_secs(5);

/// Where a task posted by [`on_main_thread`] stands: the task claims `RUNNING`, or the waiter
/// claims `ABANDONED`, and whoever is second backs off.
const PENDING: u8 = 0;
const RUNNING: u8 = 1;
const ABANDONED: u8 = 2;

/// Run `work` on the main thread and wait for its result at most `deadline`. `post` hands the task
/// to the main thread (the app's: `AppHandle::run_on_main_thread`, which runs it inline when the
/// caller already is the main thread). A panic in `work` comes back as an error and never unwinds
/// into the event loop that ran it.
///
/// A task that reaches the main thread after the deadline does **not** run `work`: what it makes
/// (a layer on the content view) would outlive everything that could remove it, and every retry
/// would add another. The same goes for a task `post` queued and then reported as failed. A task
/// that has already started when the deadline passes is waited for for half a deadline more, since
/// it cannot be called back and its result is the only handle on what it made; the two waits add up
/// to one and a half deadlines, which is why the app's 5 s fits inside [`BUILD_DEADLINE`].
fn on_main_thread<T: Send + 'static>(
    post: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), String>,
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    on_main_thread_with_grace(post, deadline, deadline / 2, work)
}

/// [`on_main_thread`] with the extra wait for a task that has started given, so a test can make
/// it generous without making the deadline long.
fn on_main_thread_with_grace<T: Send + 'static>(
    post: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), String>,
    deadline: Duration,
    grace: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    let state = Arc::new(AtomicU8::new(PENDING));
    let claim = Arc::clone(&state);
    let posted = post(Box::new(move || {
        if claim
            .compare_exchange(PENDING, RUNNING, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = tx.send(catch_unwind(AssertUnwindSafe(work)));
        }
    }));
    if let Err(refused) = posted {
        state.store(ABANDONED, Ordering::SeqCst);
        return Err(refused);
    }
    let mut waited = deadline;
    let mut answer = rx.recv_timeout(deadline);
    if matches!(answer, Err(RecvTimeoutError::Timeout))
        && state
            .compare_exchange(PENDING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        waited += grace;
        answer = rx.recv_timeout(grace);
    }
    match answer {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(payload)) => Err(format!("panicked on the main thread: {}", panic_text(payload.as_ref()))),
        Err(RecvTimeoutError::Timeout) => Err(format!("the main thread did not answer within {} s", waited.as_secs_f64())),
        Err(RecvTimeoutError::Disconnected) => Err("the main thread dropped the task".into()),
    }
}

/// Build the app's backend: the device on an adapter that can present to the host, the
/// compositor, a frame source and the presenter.
fn build_backend(app: &tauri::AppHandle, technique: Technique) -> Result<Backend, String> {
    use tauri::Manager as _;

    let window = app.get_webview_window("main").ok_or("the main window does not exist")?;
    // The machine's GPU; a CPU rasteriser (lavapipe, WARP) only when `KERF_GPU_ADAPTER` asks for it.
    let options = GpuOptions::for_preview(std::env::var("KERF_GPU_ADAPTER").ok().as_deref());
    let e = |what: &str| {
        let what = what.to_string();
        move |err: GpuError| format!("{what}: {err}")
    };
    let (gpu, presenter, mut host) = match technique {
        Technique::Window => {
            let (gpu, surface) = if surface_on_main_thread(std::env::consts::OS) {
                // The device is opened here, off the main thread; only the surface is made there.
                let gpu = Gpu::new(options).map_err(e("the window surface"))?;
                let surface = on_main_thread(
                    |task| app.run_on_main_thread(task).map_err(|err| err.to_string()),
                    MAIN_THREAD_DEADLINE,
                    {
                        let (gpu, window) = (Arc::clone(&gpu), window.clone());
                        move || gpu.create_surface(window)
                    },
                )
                .map_err(|why| format!("the window surface: {why}"))?
                .map_err(e("the window surface"))?;
                (gpu, surface)
            } else {
                Gpu::new_for_surface(options, window.clone()).map_err(e("the window surface"))?
            };
            let size = window.inner_size().map_err(|err| err.to_string())?;
            let presenter =
                Presenter::new(Arc::clone(&gpu), surface, (size.width, size.height)).map_err(e("the window surface"))?;
            (gpu, presenter, Host::window(window))
        }
        #[cfg(target_os = "linux")]
        Technique::Child => {
            let child = x11::Child::create(&window)?;
            let (gpu, surface) = Gpu::new_for_surface(options, child.target()).map_err(e("the child window surface"))?;
            let presenter = Presenter::new(Arc::clone(&gpu), surface, (1, 1)).map_err(e("the child window surface"))?;
            (gpu, presenter, Host::Child(Box::new(child)))
        }
        #[cfg(not(target_os = "linux"))]
        Technique::Child => return Err("a child window is only implemented for X11".into()),
    };
    debug_assert_eq!(host.technique(), technique);
    let compositor = Compositor::new(Arc::clone(&gpu)).map_err(e("the compositor"))?;
    let source = FrameSource::new(FrameSourceConfig::default());
    // Last, so nothing above can fail with the webview already transparent; and if this fails, the
    // host (and so the background) goes back to normal as it is dropped.
    host.make_transparent()?;
    Ok(Backend {
        screen: Box::new(presenter),
        compositor: Arc::new(compositor),
        source,
        adapter: AdapterKey::of(&gpu),
        gpu,
        host: Arc::new(Mutex::new(host)),
    })
}

#[cfg(test)]
mod bench_tests;
#[cfg(test)]
mod drive_tests;
#[cfg(test)]
mod play_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_gpu::{Gpu, GpuOptions};

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect { x, y, width, height }
    }

    fn cut(tracks: Vec<(StreamKind, &str, Vec<f64>)>) -> Timeline {
        Timeline {
            tracks: tracks
                .into_iter()
                .map(|(kind, name, starts)| kerf_core::Track {
                    clips: starts
                        .into_iter()
                        .map(|s| kerf_core::Clip::new(uuid::Uuid::nil(), 0.0, 1.0, s))
                        .collect(),
                    ..kerf_core::Track::new(kind, name.to_string())
                })
                .collect(),
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        }
    }

    #[test]
    fn a_time_between_two_frames_is_planned_at_the_start_of_its_frame() {
        let tl = cut(vec![(StreamKind::Video, "V1", vec![])]);
        let planner = Planner::new(
            &tl,
            &[],
            &ExportOptions::default(),
            PlanRequest::still(CompositeColorPolicy::FixedBt601),
        )
        .unwrap();
        let fps = planner.canvas().fps;
        let start = fps.exact_time(37);
        for t in [start, start + 0.25 / 30.0, start + 0.99 / 30.0] {
            let plan = plan_at_grid(&planner, t).unwrap();
            assert_eq!(plan.time, start, "t = {t}");
            assert_eq!(plan.frame, 37);
        }
        assert_eq!(plan_at_grid(&planner, fps.exact_time(38)).unwrap().frame, 38);
        assert_eq!(
            plan_at_grid(&planner, -1.0).unwrap().frame,
            0,
            "before the start is its first frame"
        );
    }

    #[test]
    fn a_forward_drag_reads_ahead_unless_a_layer_plays_backward() {
        let asset: kerf_core::Asset = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "path": "/x.mp4", "name": "x.mp4", "duration": 100.0,
            "streams": [{
                "index": 0, "kind": StreamKind::Video, "codec": "h264",
                "width": 1920, "height": 1080, "fps": 30.0
            }],
            "imported_at": "2026-01-01T00:00:00Z", "source_paths": [], "voiceover": null, "music_score": null
        }))
        .expect("an asset");
        let hint_at = |speed: f64| {
            let mut tl = cut(vec![(StreamKind::Video, "V1", vec![0.0])]);
            let clip = &mut tl.tracks[0].clips[0];
            clip.asset_id = asset.id;
            clip.source_out = 10.0;
            clip.speed = speed;
            let planner = Planner::new(
                &tl,
                std::slice::from_ref(&asset),
                &ExportOptions::default(),
                PlanRequest::still(CompositeColorPolicy::FixedBt601),
            )
            .unwrap();
            let plan = plan_at_grid(&planner, 1.0).unwrap();
            assert_eq!(plan.layers.len(), 1);
            (decode_hint(&plan, true), decode_hint(&plan, false))
        };
        let (forward, still) = hint_at(1.0);
        assert!(
            matches!(forward, Hint::Forward { fps } if (fps - 30.0).abs() < 1e-9),
            "{forward:?}"
        );
        assert_eq!(still, Hint::Scrub, "a drag that does not walk forward is a scrub");
        assert_eq!(
            hint_at(-1.0),
            (Hint::Scrub, Hint::Scrub),
            "a reversed clip under the playhead is scrubbed, walking forward or not"
        );
    }

    #[test]
    fn a_flag_is_cleared_when_what_held_it_panics() {
        let flag = Arc::new(AtomicBool::new(true));
        let held = ClearOnDrop(Arc::clone(&flag));
        let ended = std::thread::spawn(move || {
            let _held = held;
            panic!("the prefetch panicked");
        })
        .join();
        assert!(ended.is_err());
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn the_clips_that_start_within_the_horizon_are_the_upcoming_ones_nearest_first() {
        let tl = cut(vec![(StreamKind::Video, "V1", vec![0.0, 1.0, 2.0, 3.5, 6.0])]);
        assert_eq!(upcoming_clip_starts(&tl, 0.5, 2.0, 3), vec![1.0, 2.0]);
        assert_eq!(upcoming_clip_starts(&tl, 0.5, 4.0, 3), vec![1.0, 2.0, 3.5]);
        assert_eq!(
            upcoming_clip_starts(&tl, 0.5, 6.0, 2),
            vec![1.0, 2.0],
            "at most the limit, the nearest"
        );
        assert_eq!(
            upcoming_clip_starts(&tl, 6.5, 2.0, 3),
            Vec::<f64>::new(),
            "none past the last"
        );
    }

    #[test]
    fn the_clip_on_screen_and_the_cut_under_the_playhead_are_not_upcoming() {
        let tl = cut(vec![(StreamKind::Video, "V1", vec![0.0, 1.0, 2.0])]);
        // At the very frame a clip starts, it is the one being read.
        assert_eq!(upcoming_clip_starts(&tl, 1.0, 2.0, 3), vec![2.0]);
        assert_eq!(
            upcoming_clip_starts(&tl, 1.0 - 1e-7, 2.0, 3),
            vec![2.0],
            "a hair before is the same frame"
        );
        assert_eq!(
            upcoming_clip_starts(&tl, 0.9, 0.05, 3),
            Vec::<f64>::new(),
            "and the horizon is a horizon"
        );
    }

    #[test]
    fn tracks_that_do_not_render_and_clips_that_are_off_are_not_upcoming() {
        let mut tl = cut(vec![
            (StreamKind::Video, "V1", vec![1.0, 3.0]),
            (StreamKind::Audio, "A1", vec![1.5]),
            (StreamKind::Video, "V2", vec![2.0]),
        ]);
        assert_eq!(
            upcoming_clip_starts(&tl, 0.0, 4.0, 5),
            vec![1.0, 2.0, 3.0],
            "audio is not a picture"
        );
        tl.tracks[2].muted = true;
        assert_eq!(upcoming_clip_starts(&tl, 0.0, 4.0, 5), vec![1.0, 3.0]);
        tl.tracks[0].clips[1].enabled = false;
        assert_eq!(upcoming_clip_starts(&tl, 0.0, 4.0, 5), vec![1.0]);
    }

    #[test]
    fn clips_on_two_tracks_that_start_together_are_one_moment() {
        let tl = cut(vec![
            (StreamKind::Video, "V1", vec![1.0]),
            (StreamKind::Video, "V2", vec![1.0]),
        ]);
        assert_eq!(upcoming_clip_starts(&tl, 0.0, 2.0, 5), vec![1.0]);
    }

    #[test]
    fn the_clips_that_ended_within_the_horizon_behind_the_playhead_are_passed_ones_with_their_last_frame() {
        // Clips of a second at 0, 1, 2, 3.5 and 6: they end at 1, 2, 3, 4.5 and 7.
        let tl = cut(vec![(StreamKind::Video, "V1", vec![0.0, 1.0, 2.0, 3.5, 6.0])]);
        let frame = 1.0 / 30.0;
        let last = |end: f64| end - frame / 2.0;
        // From 3.2 s, two seconds back: the clips that ended at 2 and 3 (nearest first); the one
        // ending at 1 is further than the horizon, and the one ending at 4.5 is ahead.
        assert_eq!(passed_clip_ends(&tl, 3.2, 2.0, frame, 5), vec![last(3.0), last(2.0)]);
        assert_eq!(
            passed_clip_ends(&tl, 3.2, 2.0, frame, 1),
            vec![last(3.0)],
            "at most the limit, the nearest"
        );
        // A clip the playhead is inside, or sits exactly at the end of, is not behind it.
        assert_eq!(passed_clip_ends(&tl, 3.0, 2.0, frame, 5), vec![last(2.0), last(1.0)]);
        assert!(
            passed_clip_ends(&tl, 0.5, 2.0, frame, 5).is_empty(),
            "nothing behind the first clip"
        );
        assert!(passed_clip_ends(&tl, 3.2, 2.0, 0.0, 5).is_empty() && passed_clip_ends(&tl, f64::NAN, 2.0, frame, 5).is_empty());
        // Tracks that do not render and clips that are off are not the picture.
        let mut two = cut(vec![
            (StreamKind::Video, "V1", vec![0.0]),
            (StreamKind::Audio, "A1", vec![0.0]),
            (StreamKind::Video, "V2", vec![0.0]),
        ]);
        assert_eq!(
            passed_clip_ends(&two, 1.5, 2.0, frame, 5),
            vec![last(1.0)],
            "two tracks ending together are one moment"
        );
        two.tracks[2].muted = true;
        two.tracks[0].clips[0].enabled = false;
        assert!(passed_clip_ends(&two, 1.5, 2.0, frame, 5).is_empty());
    }

    #[test]
    fn a_drag_prefetches_the_cuts_on_both_sides_of_the_playhead_nearest_first() {
        let tl = cut(vec![(StreamKind::Video, "V1", vec![0.0, 1.0, 2.0, 3.5, 6.0])]);
        let frame = 1.0 / 30.0;
        // At 2.2 s: clips end at 2.0 (0.2 s back) and 1.0 (1.2 s back), and the next starts at 3.5
        // (1.3 s ahead).
        let moments = cut_moments(&tl, 2.2, 2.0, frame, 3);
        assert_eq!(
            moments,
            vec![
                CutMoment {
                    t: 2.0 - frame / 2.0,
                    behind: true
                },
                CutMoment {
                    t: 1.0 - frame / 2.0,
                    behind: true
                },
                CutMoment { t: 3.5, behind: false },
            ]
        );
        assert_eq!(
            cut_moments(&tl, 2.2, 2.0, frame, 1),
            vec![CutMoment {
                t: 2.0 - frame / 2.0,
                behind: true
            }]
        );
        // Nothing near: nothing.
        assert!(cut_moments(&tl, 5.0, 0.2, frame, 3).is_empty());
        // The old forward-only answer is still what is ahead.
        let ahead: Vec<f64> = cut_moments(&tl, 0.5, 2.0, frame, 5)
            .iter()
            .filter(|m| !m.behind)
            .map(|m| m.t)
            .collect();
        assert_eq!(ahead, upcoming_clip_starts(&tl, 0.5, 2.0, 5));
    }

    #[test]
    fn a_time_that_is_not_a_time_has_nothing_upcoming() {
        let tl = cut(vec![(StreamKind::Video, "V1", vec![1.0])]);
        assert!(upcoming_clip_starts(&tl, f64::NAN, 2.0, 3).is_empty());
        assert!(upcoming_clip_starts(&tl, 0.0, f64::NAN, 3).is_empty());
        assert!(upcoming_clip_starts(&tl, 0.0, 0.0, 3).is_empty());
        assert!(upcoming_clip_starts(&tl, 0.0, -1.0, 3).is_empty());
    }

    pub(super) fn report() -> BoundsReport {
        BoundsReport {
            x: 10.4,
            y: 20.6,
            width: 800.0,
            height: 450.0,
            viewport_width: 1440.0,
            viewport_height: 900.0,
            visible: true,
            matte: Some("#102030".into()),
            backdrop: Some("#0a0b0c".into()),
            seq: 0,
        }
    }

    #[test]
    fn every_platform_has_its_technique_or_says_why_not() {
        let t = |os, choice| resolve_technique(os, choice);
        assert_eq!(t("windows", None).technique, Some(Technique::Window));
        assert_eq!(t("linux", None).technique, Some(Technique::Child));
        let mac = t("macos", None);
        assert_eq!(mac.technique, None);
        let note = mac.note.unwrap();
        assert!(note.contains("not been verified on a Mac"), "{note}");
        assert!(
            !note.contains("KERF_GPU_SURFACE"),
            "the user is not invited to force it: {note}"
        );
        assert!(!note.contains("private-api"), "the private API is not the obstacle: {note}");
        assert!(t("freebsd", None).technique.is_none());
        // The environment overrides, in both directions.
        assert_eq!(t("linux", Some("window")).technique, Some(Technique::Window));
        assert_eq!(t("windows", Some("off")).technique, None);
        assert_eq!(t("macos", Some("window")).technique, Some(Technique::Window));
        assert_eq!(t("linux", Some("child")).technique, Some(Technique::Child));
        assert!(t("windows", Some("child")).technique.is_none(), "no child window outside X11");
        assert!(t("macos", Some("child")).technique.is_none(), "no child window outside X11");
        assert_eq!(t("macos", Some("off")).technique, None);
        // A value nobody defined is not obeyed.
        assert_eq!(t("windows", Some("nonsense")).technique, Some(Technique::Window));
        assert_eq!(t("linux", Some("")).technique, Some(Technique::Child));
        // Only the technique under a transparent webview lets the page draw over the picture.
        assert!(Technique::Window.overlays());
        assert!(!Technique::Child.overlays());
    }

    #[test]
    fn only_macos_makes_its_window_surface_on_the_main_thread() {
        assert!(surface_on_main_thread("macos"));
        for os in ["windows", "linux", "freebsd", "ios", ""] {
            assert!(!surface_on_main_thread(os), "{os}");
        }
    }

    /// A stand-in for the event loop: runs each posted task on a thread of its own.
    fn loop_thread(task: Box<dyn FnOnce() + Send>) -> Result<(), String> {
        std::thread::Builder::new()
            .name("stand-in-main".into())
            .spawn(task)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn work_posted_to_the_main_thread_runs_there_and_its_answer_comes_back() {
        let caller = std::thread::current().id();
        let (ran_on, name) = on_main_thread(loop_thread, Duration::from_secs(5), move || {
            (std::thread::current().id(), std::thread::current().name().map(str::to_string))
        })
        .unwrap();
        assert_ne!(ran_on, caller);
        assert_eq!(name.as_deref(), Some("stand-in-main"));
        // Tauri runs a task inline when the caller already is the main thread.
        let inline = on_main_thread(
            |task| {
                task();
                Ok(())
            },
            Duration::from_secs(5),
            || 7,
        );
        assert_eq!(inline, Ok(7));
    }

    #[test]
    fn a_main_thread_that_cannot_run_the_task_is_an_error_not_a_hang() {
        let t = Duration::from_millis(100);
        // The loop refuses the task.
        assert_eq!(
            on_main_thread(|_| Err("the event loop is closed".to_string()), t, || 1),
            Err("the event loop is closed".to_string())
        );
        // The loop drops it unrun (the app is closing): the answer can never come.
        let dropped = on_main_thread(
            |task| {
                drop(task);
                Ok(())
            },
            t,
            || 1,
        )
        .unwrap_err();
        assert!(dropped.contains("dropped"), "{dropped}");
    }

    #[test]
    fn a_task_that_reaches_the_main_thread_late_does_not_run_its_work() {
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let (finished_tx, finished_rx) = mpsc::channel();
        let started = Instant::now();
        let late = on_main_thread(
            move |task| {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    task();
                    let _ = finished_tx.send(());
                });
                Ok(())
            },
            Duration::from_millis(300),
            move || flag.store(true, Ordering::SeqCst),
        )
        .unwrap_err();
        assert!(late.contains("did not answer within 0.3 s"), "{late}");
        assert!(started.elapsed() < Duration::from_millis(1000), "waited past the deadline");
        finished_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the late task never ran at all");
        assert!(!ran.load(Ordering::SeqCst), "a layer made now would never be removed");
    }

    #[test]
    fn a_task_that_was_queued_and_then_reported_as_failed_does_not_run_its_work() {
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let queued = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&queued);
        let err = on_main_thread(
            move |task| {
                *lock(&slot) = Some(task);
                Err("the loop refused it after queueing".to_string())
            },
            Duration::from_secs(1),
            move || flag.store(true, Ordering::SeqCst),
        )
        .unwrap_err();
        assert_eq!(err, "the loop refused it after queueing");
        let task = lock(&queued).take().expect("the task was queued");
        task();
        assert!(!ran.load(Ordering::SeqCst), "nobody waits for what this would make");
    }

    #[test]
    fn a_task_already_running_at_the_deadline_is_waited_for_once_more() {
        let got = on_main_thread_with_grace(loop_thread, Duration::from_secs(1), Duration::from_secs(20), || {
            std::thread::sleep(Duration::from_millis(1500));
            "made"
        });
        assert_eq!(got, Ok("made"));
    }

    #[test]
    fn the_wait_for_a_running_task_is_bounded_and_the_message_gives_the_total() {
        let started = Instant::now();
        let stuck = on_main_thread_with_grace(loop_thread, Duration::from_millis(300), Duration::from_millis(200), || {
            std::thread::sleep(Duration::from_secs(4));
        })
        .unwrap_err();
        assert!(stuck.contains("did not answer within 0.5 s"), "{stuck}");
        assert!(started.elapsed() < Duration::from_secs(3), "waited for the whole task");
        // The default grace is half a deadline: 1.5 deadlines in all.
        let stuck = on_main_thread(loop_thread, Duration::from_millis(400), || {
            std::thread::sleep(Duration::from_secs(4));
        })
        .unwrap_err();
        assert!(stuck.contains("did not answer within 0.6 s"), "{stuck}");
    }

    #[test]
    fn a_panic_on_the_main_thread_does_not_unwind_into_its_loop() {
        // The task itself must return normally: a panic that escaped it would take the event loop down.
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let got = on_main_thread::<()>(
            move |task| {
                task();
                flag.store(true, Ordering::SeqCst);
                Ok(())
            },
            Duration::from_secs(5),
            || panic!("a backend that asserts on a handle it cannot use"),
        )
        .unwrap_err();
        assert!(ran.load(Ordering::SeqCst), "the posted task returned");
        assert!(got.contains("a backend that asserts on a handle it cannot use"), "{got}");
    }

    #[test]
    fn a_report_becomes_whole_pixels_and_a_bad_one_becomes_nothing() {
        let b = Bounds::from_report(&report()).unwrap();
        assert_eq!(b.rect, rect(10, 21, 800, 450));
        assert_eq!(b.viewport, (1440, 900));
        assert_eq!(b.matte, [0x10, 0x20, 0x30]);
        assert_eq!(b.backdrop, [0x0a, 0x0b, 0x0c]);
        assert!(b.visible);

        let mut r = report();
        r.matte = Some("not a colour".into());
        assert_eq!(Bounds::from_report(&r).unwrap().matte, DEFAULT_MATTE);
        for bad in [f64::NAN, f64::INFINITY, -1.0, 1e12] {
            let mut r = report();
            r.x = bad;
            assert!(Bounds::from_report(&r).is_none(), "{bad}");
            let mut r = report();
            r.viewport_height = bad;
            assert!(Bounds::from_report(&r).is_none(), "{bad}");
        }
        // An empty rectangle is a surface that is not showing, not an error.
        let mut r = report();
        r.width = 0.0;
        assert!(!Bounds::from_report(&r).unwrap().visible);
        let mut r = report();
        r.visible = false;
        assert!(!Bounds::from_report(&r).unwrap().visible);
    }

    #[test]
    fn matte_colours_parse_with_or_without_the_hash_and_nothing_else() {
        assert_eq!(parse_matte("#0f1318"), Some([0x0f, 0x13, 0x18]));
        assert_eq!(parse_matte("FFFFFF"), Some([255, 255, 255]));
        assert_eq!(parse_matte(" #abcdef "), Some([0xab, 0xcd, 0xef]));
        for bad in ["", "#fff", "#12345g", "rgb(1,2,3)", "#1234567"] {
            assert_eq!(parse_matte(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_rectangle_maps_between_spaces_without_gaps() {
        let r = rect(100, 50, 600, 300);
        assert_eq!(scaled(r, (1000, 500), (1000, 500)), r);
        assert_eq!(scaled(r, (0, 0), (10, 10)), r, "an unknown source space changes nothing");
        assert_eq!(scaled(r, (1000, 500), (2000, 1000)), rect(200, 100, 1200, 600));
        // Neighbours that touch before still touch after, at a fractional scale.
        let left = scaled(rect(0, 0, 333, 10), (1000, 10), (1500, 10));
        let right = scaled(rect(333, 0, 667, 10), (1000, 10), (1500, 10));
        assert_eq!(left.x + left.width, right.x);
        assert_eq!(right.x + right.width, 1500);
    }

    #[test]
    fn a_picture_sits_in_its_frame_the_way_object_fit_contain_puts_it() {
        let area = rect(10, 20, 800, 450);
        assert_eq!(fit_contain((1920, 1080), area), area);
        // 4:3 in 16:9: pillarboxed, centred.
        assert_eq!(fit_contain((1440, 1080), area), rect(110, 20, 600, 450));
        // 9:16 in 16:9.
        let tall = fit_contain((1080, 1920), area);
        assert_eq!((tall.height, tall.y), (450, 20));
        assert!(tall.width < 300 && tall.x > 10);
        // Wider than the area: letterboxed.
        assert_eq!(fit_contain((2000, 500), area), rect(10, 20 + (450 - 200) / 2, 800, 200));
        // Degenerate inputs leave the area alone rather than dividing by zero.
        assert_eq!(fit_contain((0, 0), area), area);
        assert_eq!(fit_contain((10, 10), rect(0, 0, 0, 5)), rect(0, 0, 0, 5));
    }

    fn still(canvas: (u32, u32)) -> impl Fn(u32) -> (u32, u32) {
        move |w| kerf_core::render_plan::still_size(canvas.0, canvas.1, w)
    }

    #[test]
    fn the_render_width_is_the_shown_width_even_and_capped() {
        let canvas = (1920, 1080);
        let (w, dest) = place_frame(canvas, rect(0, 0, 801, 451), still(canvas));
        assert_eq!(w % 2, 0);
        assert!((w as i64 - 801).abs() <= 48, "{w}");
        assert!(dest.width <= 801 && dest.height <= 451);
        // A panel bigger than the cap renders at the cap, and the presenter scales it up.
        let canvas = (3840, 2160);
        let (w, dest) = place_frame(canvas, rect(0, 0, 3000, 1688), still(canvas));
        assert_eq!(w, MAX_RENDER_WIDTH);
        assert!(dest.width > MAX_RENDER_WIDTH);
        // A sliver still renders at a legal size.
        let (w, _) = place_frame((1920, 1080), rect(0, 0, 1, 1), still((1920, 1080)));
        assert!(w >= 2 && w % 2 == 0 && w <= 50, "{w}");
        // A canvas smaller than the panel is rendered at its own size, never above it.
        assert_eq!(place_frame((640, 360), rect(0, 0, 1600, 900), still((640, 360))).0, 640);
    }

    #[test]
    fn a_render_keeps_the_canvas_shape_to_the_row_where_a_nearby_width_can() {
        // 430 px is 240 rows of an ideal 241.9: the footage would be letterboxed into a canvas a
        // hair too tall. 416 px is 234 rows exactly.
        let canvas = (1280, 720);
        assert_eq!(kerf_core::render_plan::still_size(1280, 720, 430), (430, 240));
        let w = exact_width(canvas, 430, still(canvas));
        let (rw, rh) = kerf_core::render_plan::still_size(1280, 720, w);
        assert_eq!(u64::from(rw) * 720, u64::from(rh) * 1280, "{w} -> {rw}x{rh}");
        assert!((w as i64 - 430).abs() <= 16, "{w}");
        // A width that is already exact stays.
        assert_eq!(exact_width(canvas, 960, still(canvas)), 960);
        // Every common shape finds an exact width within the search at a typical panel size.
        for canvas in [
            (1920u32, 1080u32),
            (1080, 1920),
            (1080, 1080),
            (1080, 1350),
            (1440, 1080),
            (2560, 1080),
        ] {
            for target in [300u32, 431, 640, 801, 1100] {
                let w = exact_width(canvas, target, still(canvas));
                let (rw, rh) = kerf_core::render_plan::still_size(canvas.0, canvas.1, w);
                let err = (f64::from(rw) * f64::from(canvas.1) / f64::from(canvas.0) - f64::from(rh)).abs();
                assert!(err < 1.0, "{canvas:?} at {target}: {w} -> {rw}x{rh} is {err} rows off");
                assert_eq!(w % 2, 0);
            }
        }
        // Odd shapes with no exact width nearby take the least wrong one, never anything silly.
        let canvas = (1998, 1080);
        let w = exact_width(canvas, 700, still(canvas));
        assert!((w as i64 - 700).abs() <= 48);
    }

    #[test]
    fn the_window_surface_covers_the_window_and_the_child_one_the_frame() {
        let b = Bounds::from_report(&report()).unwrap();
        // The window's surface is 1440x900 px, as the page measured it.
        let l = layout_for(Technique::Window, &b, (1440, 900)).unwrap();
        assert_eq!((l.surface, l.area, l.window), ((1440, 900), rect(10, 21, 800, 450), None));
        // The window is a pixel narrower than the page believed: the frame is mapped, not cut.
        let l = layout_for(Technique::Window, &b, (1439, 900)).unwrap();
        assert_eq!(l.surface, (1439, 900));
        assert!(l.area.x + l.area.width <= 1439);
        // A frame past the window's edge is clipped to it; one entirely outside is nothing.
        let mut far = b;
        far.rect = rect(1400, 0, 200, 100);
        assert_eq!(
            layout_for(Technique::Window, &far, (1440, 900)).unwrap().area,
            rect(1400, 0, 40, 100)
        );
        far.rect = rect(1500, 0, 200, 100);
        assert!(layout_for(Technique::Window, &far, (1440, 900)).is_none());

        let l = layout_for(Technique::Child, &b, (0, 0)).unwrap();
        assert_eq!(l.surface, (800, 450));
        assert_eq!(l.area, rect(0, 0, 800, 450));
        assert_eq!(l.window, Some(rect(10, 21, 800, 450)));
        // The child never extends past the page it lives on.
        far.rect = rect(1400, 0, 200, 100);
        assert_eq!(
            layout_for(Technique::Child, &far, (0, 0)).unwrap().window,
            Some(rect(1400, 0, 40, 100))
        );
    }

    #[test]
    fn a_failing_backend_backs_off_and_a_shown_frame_forgives() {
        assert_eq!(Backoff::delay(0), Duration::ZERO);
        assert_eq!(Backoff::delay(1), Duration::from_secs(5));
        assert_eq!(Backoff::delay(2), Duration::from_secs(10));
        assert_eq!(Backoff::delay(40), Duration::from_secs(300), "capped, and no overflow");
        let mut b = Backoff::default();
        // The first loss of a device that worked is rebuilt at once…
        assert_eq!(b.fail(true), Duration::ZERO);
        // …the second in a row is not.
        assert_eq!(b.fail(true), Duration::from_secs(10));
        assert_eq!(b.fail(false), Duration::from_secs(20));
        for _ in 0..Backoff::FORGIVEN_AFTER {
            b.succeeded();
        }
        assert_eq!(b.strikes(), 0);
        // A failed build waits from the start.
        assert_eq!(b.fail(false), Duration::from_secs(5));
    }

    #[test]
    fn a_device_that_dies_after_every_frame_is_not_forgiven_by_the_frames_it_shows() {
        let mut b = Backoff::default();
        assert_eq!(b.fail(true), Duration::ZERO, "the first loss is rebuilt at once");
        let mut waits = Vec::new();
        for _ in 0..4 {
            // One frame reaches the screen, and then the device is lost again.
            b.succeeded();
            waits.push(b.fail(true));
        }
        assert_eq!(
            waits,
            [10, 20, 40, 80].map(Duration::from_secs),
            "the waits grow instead of resetting"
        );
        // A run one short of the forgiving one changes nothing; the run itself does.
        for _ in 0..Backoff::FORGIVEN_AFTER - 1 {
            b.succeeded();
        }
        assert_eq!(b.strikes(), 5);
        b.succeeded();
        assert_eq!(b.strikes(), 0);
    }

    #[test]
    fn the_device_or_surface_being_gone_rebuilds_and_a_refused_frame_does_not() {
        use GpuError as E;
        for rebuilt in [
            E::DeviceLost("x".into()),
            E::OutOfMemory,
            E::Gpu("x".into()),
            E::Surface("the surface was lost".into()),
        ] {
            assert_eq!(react(&rebuilt), Reaction::Rebuild, "{rebuilt}");
        }
        for frame in [
            E::Unsupported("x".into()),
            E::Decode("x".into()),
            E::Busy("x".into()),
            E::Readback("x".into()),
            E::Surface("the window is occluded".into()),
            E::Surface("acquiring the next surface texture timed out".into()),
        ] {
            assert_eq!(react(&frame), Reaction::Frame, "{frame}");
        }
    }

    /// A preview whose backend never starts, with the colour policy pinned (the real probe
    /// would run an FFmpeg).
    pub(super) fn with(technique: Resolution, factory: Box<Factory>) -> GpuPreview {
        let mut preview = GpuPreview::with_factory(technique, Arc::from(factory));
        preview.policy = || CompositeColorPolicy::FixedBt601;
        preview
    }

    pub(super) fn failing(counter: Arc<std::sync::atomic::AtomicUsize>, why: &'static str) -> GpuPreview {
        with(
            Resolution {
                technique: Some(Technique::Child),
                note: None,
            },
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(why.to_string())
            }),
        )
    }

    /// A project with a clip on it, as the plan sees it (the file never has to exist: planning
    /// does not decode).
    pub(super) fn inputs() -> PlanInputs {
        let project = Project::sample().expect("the sample project");
        PlanInputs::of(&project).expect("inputs")
    }

    pub(super) fn visible(preview: &GpuPreview) {
        preview.set_bounds(&report()).expect("bounds");
    }

    fn no_inputs() -> TakeInputs {
        Arc::new(|| Err("no project in this test".to_string()))
    }

    #[test]
    fn a_worker_job_dropped_without_running_hands_the_mailbox_back() {
        let preview = Arc::new(failing(Arc::new(std::sync::atomic::AtomicUsize::new(0)), "no adapter"));
        let mut lost = preview.post(FrameRequest::new(no_inputs(), 0.5, 320, false), drop);
        assert!(
            matches!(lost.try_recv(), Ok(Delivery::Abandoned)),
            "told its worker never ran, not left waiting"
        );
        // The mailbox is idle again: the next request starts a worker, which answers it.
        let mut next = preview.post(FrameRequest::new(no_inputs(), 0.6, 320, false), |job| job());
        let Ok(Delivery::Done(answer)) = next.try_recv() else {
            panic!("the worker did not run");
        };
        assert_eq!(answer.unwrap_err(), "no project in this test");
    }

    #[test]
    fn a_request_older_than_one_the_preview_has_seen_is_superseded_without_a_worker() {
        let preview = Arc::new(failing(Arc::new(std::sync::atomic::AtomicUsize::new(0)), "no adapter"));
        let newer = FrameRequest::new(no_inputs(), 2.0, 320, false).ordered("page", 7);
        assert!(matches!(preview.post(newer, |job| job()).try_recv(), Ok(Delivery::Done(_))));
        // The older request of the same page, arriving second over the IPC.
        let older = FrameRequest::new(no_inputs(), 1.0, 320, false).ordered("page", 6);
        let started = std::cell::Cell::new(false);
        let mut late = preview.post(older, |_job| started.set(true));
        assert!(matches!(late.try_recv(), Ok(Delivery::Superseded)));
        assert!(!started.get(), "no worker for a frame nobody wants");
        assert_eq!(preview.diagnostics().requests.superseded, 1);
    }

    #[test]
    fn with_the_setting_off_nothing_is_built_and_the_reason_says_so() {
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let preview = failing(built.clone(), "should not be called");
        visible(&preview);
        let Attempt::Fallback(reasons) = preview.attempt(&inputs(), Want::at(0.5), Instant::now()) else {
            panic!("a disabled preview drew a frame");
        };
        assert!(reasons[0].text.contains("off"), "{reasons:?}");
        assert_eq!(built.load(Ordering::SeqCst), 0);
        assert!(!preview.status().enabled);
    }

    #[test]
    fn a_backend_that_cannot_start_is_a_fallback_with_its_reason_and_is_not_retried_at_once() {
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let preview = failing(built.clone(), "no usable GPU adapter: nothing here");
        preview.set_enabled(true);
        visible(&preview);
        let inputs = inputs();
        let t0 = Instant::now();
        for n in 0..3 {
            let Attempt::Fallback(reasons) = preview.attempt(&inputs, Want::at(0.5), t0 + Duration::from_millis(n)) else {
                panic!("a failing backend drew a frame");
            };
            assert!(reasons[0].text.contains("no usable GPU adapter"), "{reasons:?}");
        }
        assert_eq!(
            built.load(Ordering::SeqCst),
            1,
            "the failure is remembered, not retried per frame"
        );
        let status = preview.status();
        assert!(status.enabled && status.supported && !status.ready);
        assert!(status.reason.unwrap().contains("no usable GPU adapter"));
        // After the backoff it tries again.
        let _ = preview.attempt(&inputs, Want::at(0.5), t0 + Duration::from_secs(6));
        assert_eq!(built.load(Ordering::SeqCst), 2);
        // Turning the setting off and on again forgets nothing it should not, and draws nothing.
        preview.set_enabled(false);
        assert!(!preview.status().enabled);
    }

    #[test]
    fn a_surface_that_sits_above_the_page_leaves_frames_with_overlays_to_ffmpeg() {
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let preview = failing(built.clone(), "unused");
        preview.set_enabled(true);
        visible(&preview);
        let Attempt::Fallback(reasons) = preview.attempt(
            &inputs(),
            Want {
                overlays: true,
                ..Want::at(0.5)
            },
            Instant::now(),
        ) else {
            panic!("drew over the page");
        };
        assert!(reasons[0].text.contains("draw over the picture"), "{reasons:?}");
        assert_eq!(built.load(Ordering::SeqCst), 0, "decided before a device is made");
    }

    #[test]
    fn no_bounds_or_hidden_bounds_or_no_technique_is_a_fallback_before_any_work() {
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let preview = failing(built.clone(), "unused");
        preview.set_enabled(true);
        let Attempt::Fallback(r) = preview.attempt(&inputs(), Want::at(0.5), Instant::now()) else {
            panic!()
        };
        assert!(r[0].text.contains("not reported"), "{r:?}");
        let mut hidden = report();
        hidden.visible = false;
        preview.set_bounds(&hidden).unwrap();
        let Attempt::Fallback(r) = preview.attempt(&inputs(), Want::at(0.5), Instant::now()) else {
            panic!()
        };
        assert!(r[0].text.contains("hidden"), "{r:?}");
        assert_eq!(built.load(Ordering::SeqCst), 0);

        let macos = with(resolve_technique("macos", None), Box::new(|_| Err("unreachable".into())));
        macos.set_enabled(true);
        assert!(!macos.status().supported);
        assert!(macos.status().reason.unwrap().contains("not been verified on a Mac"));
        visible(&macos);
        let Attempt::Fallback(r) = macos.attempt(&inputs(), Want::at(0.5), Instant::now()) else {
            panic!()
        };
        assert!(r[0].text.contains("not been verified on a Mac"), "{r:?}");
    }

    /// The app's first step on a machine with no Vulkan driver at all, run in a child process with
    /// the loader pointed at nothing (a variable is process-wide, and the other tests share this
    /// process): the device request is an error value, the frame is FFmpeg's with the reason, and
    /// the status says why — nothing panics, and nothing is retried per frame.
    #[test]
    fn a_machine_with_no_adapter_gets_the_jpeg_and_a_reason_and_not_a_crash() {
        const MARK: &str = "KERF_TEST_NO_ADAPTER";
        if std::env::var_os(MARK).is_none() {
            let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
                .args([
                    "--exact",
                    "gpu_preview::tests::a_machine_with_no_adapter_gets_the_jpeg_and_a_reason_and_not_a_crash",
                    "--nocapture",
                ])
                .env(MARK, "1")
                .env("VK_ICD_FILENAMES", "/nonexistent/kerf-no-icd.json")
                .env("VK_DRIVER_FILES", "/nonexistent/kerf-no-icd.json")
                .output()
                .expect("run the child");
            assert!(
                out.status.success(),
                "the child failed:\n{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        // The child. A machine that has an adapter whatever the loader is told (Metal, WARP) has
        // nothing to prove here.
        let Err(error) = Gpu::new(GpuOptions::default()) else {
            return;
        };
        assert!(matches!(error, GpuError::NoAdapter(_)), "{error:?}");
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = built.clone();
        let preview = with(
            Resolution {
                technique: Some(Technique::Child),
                note: None,
            },
            // The same first step `build_backend` takes, and its error mapped the same way.
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Gpu::new(GpuOptions::default())
                    .map(|_| unreachable!("an adapter appeared"))
                    .map_err(|e| format!("the window surface: {e}"))
            }),
        );
        preview.set_enabled(true);
        visible(&preview);
        let inputs = inputs();
        for n in 0..3 {
            let Attempt::Fallback(reasons) = preview.attempt(&inputs, Want::at(0.5), Instant::now() + Duration::from_millis(n))
            else {
                panic!("drew a frame with no adapter");
            };
            assert!(reasons[0].text.contains("no usable GPU adapter"), "{reasons:?}");
        }
        assert_eq!(
            built.load(Ordering::SeqCst),
            1,
            "a missing driver is not asked about again every frame"
        );
        let status = preview.status();
        assert!(status.enabled && !status.ready);
        assert!(status.reason.unwrap().contains("no usable GPU adapter"));
    }

    #[test]
    fn a_report_that_is_not_a_rectangle_is_refused_and_keeps_the_last_good_one() {
        let preview = failing(Arc::default(), "unused");
        visible(&preview);
        let mut bad = report();
        bad.width = f64::NAN;
        assert!(preview.set_bounds(&bad).is_err());
        assert_eq!(lock(&preview.bounds).current.unwrap().rect.width, 800);
    }

    #[test]
    fn a_hide_that_arrives_while_a_picture_is_being_shown_waits_for_it_and_is_what_stays() {
        let preview = Arc::new(failing(Arc::default(), "unused"));
        preview.set_enabled(true);
        visible(&preview);
        let (entered_tx, entered_rx) = mpsc::channel();
        let shower = Arc::clone(&preview);
        let shown = std::thread::spawn(move || {
            shower.under_bounds(|_| {
                entered_tx.send(()).unwrap();
                // A slow present: the surface is being put on the screen.
                std::thread::sleep(Duration::from_millis(300));
                Instant::now()
            })
        });
        entered_rx.recv().unwrap();
        let mut hidden = report();
        hidden.visible = false;
        hidden.seq = 2;
        // The page hides the surface (playback started, a dialog opened) in the middle of it.
        preview.set_bounds(&hidden).unwrap();
        let hidden_at = Instant::now();
        let shown_until = shown.join().unwrap().expect("it was visible when it started");
        assert!(hidden_at >= shown_until, "the hide landed in the middle of the present");
        // What stays is the hide: a frame that finishes drawing after it is dropped, not shown.
        let mut ran = false;
        assert!(preview
            .under_bounds(|_| {
                ran = true;
            })
            .is_none());
        assert!(!ran);
    }

    #[test]
    fn nothing_is_shown_to_a_surface_that_is_hidden_or_whose_setting_went_off() {
        let preview = failing(Arc::default(), "unused");
        preview.set_enabled(true);
        // No report yet.
        assert!(preview.under_bounds(|_| ()).is_none());
        visible(&preview);
        assert_eq!(preview.under_bounds(|b| b.rect.width), Some(800));
        // The bounds it is given are the ones as they are now.
        let mut moved = report();
        moved.x = 100.0;
        moved.seq = 3;
        preview.set_bounds(&moved).unwrap();
        assert_eq!(preview.under_bounds(|b| b.rect.x), Some(100));
        // The setting went off in the meantime: nothing is shown after it.
        assert!(preview.set_enabled(false));
        assert!(preview.under_bounds(|_| ()).is_none());
    }

    #[test]
    fn a_report_from_an_older_panel_that_arrives_late_is_ignored() {
        let preview = failing(Arc::default(), "unused");
        let mut newer = report();
        newer.seq = 5;
        preview.set_bounds(&newer).unwrap();
        let mut older = report();
        older.seq = 3;
        older.visible = false;
        preview.set_bounds(&older).unwrap();
        assert!(
            lock(&preview.bounds).current.unwrap().visible,
            "the old panel's hide did not land"
        );
        assert_eq!(lock(&preview.bounds).seq, 5);
        // The same number again is the same panel; no number at all is always taken.
        let mut same = report();
        same.seq = 5;
        same.visible = false;
        preview.set_bounds(&same).unwrap();
        assert!(!lock(&preview.bounds).current.unwrap().visible);
        let mut legacy = report();
        legacy.seq = 0;
        preview.set_bounds(&legacy).unwrap();
        assert!(lock(&preview.bounds).current.unwrap().visible);
        assert_eq!(lock(&preview.bounds).seq, 5, "it does not wind the counter back");
    }

    #[test]
    fn turning_it_off_flips_at_once_and_only_the_teardown_is_left_for_later() {
        let preview = failing(Arc::default(), "unused");
        assert!(!preview.set_enabled(true), "nothing to tear down when it was not on");
        assert!(preview.is_enabled());
        assert!(preview.set_enabled(false), "going off asks for a teardown");
        assert!(!preview.is_enabled(), "the flag is already down when it returns");
        // A teardown that finds the setting back on does nothing.
        preview.set_enabled(true);
        preview.teardown_if_off();
        assert!(preview.is_enabled());
    }

    #[test]
    fn turning_it_on_again_is_a_retry_that_forgets_the_backoff() {
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let preview = failing(built.clone(), "no usable GPU adapter: nothing here");
        preview.set_enabled(true);
        visible(&preview);
        let inputs = inputs();
        let t0 = Instant::now();
        let _ = preview.attempt(&inputs, Want::at(0.5), t0);
        let _ = preview.attempt(&inputs, Want::at(0.5), t0 + Duration::from_secs(1));
        assert_eq!(built.load(Ordering::SeqCst), 1, "backing off");
        // Off and on: the next frame builds again without waiting out the 5 seconds.
        preview.set_enabled(false);
        preview.set_enabled(true);
        let _ = preview.attempt(&inputs, Want::at(0.5), t0 + Duration::from_secs(2));
        assert_eq!(built.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_build_that_stalls_is_a_jpeg_after_its_deadline_and_blocks_nobody_meanwhile() {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = started.clone();
        let mut preview = with(
            Resolution {
                technique: Some(Technique::Child),
                note: None,
            },
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                // A connection that never answers: it waits until the test lets it go.
                let _ = lock(&release_rx).recv_timeout(Duration::from_secs(20));
                Err("finally gave up".to_string())
            }),
        );
        preview.build_deadline = Duration::from_millis(600);
        let preview = Arc::new(preview);
        preview.set_enabled(true);
        visible(&preview);
        let inputs = Arc::new(inputs());

        let slow = {
            let (preview, inputs) = (Arc::clone(&preview), Arc::clone(&inputs));
            std::thread::spawn(move || (Instant::now(), preview.attempt(&inputs, Want::at(0.5), Instant::now())))
        };
        // While it builds, the render lock is free and another frame does not wait for the build.
        std::thread::sleep(Duration::from_millis(150));
        assert!(preview.render.try_lock().is_ok(), "the build holds the render lock");
        let t = Instant::now();
        let Attempt::Fallback(reasons) = preview.attempt(&inputs, Want::at(0.6), Instant::now()) else {
            panic!("drew a frame while the backend was building");
        };
        assert!(t.elapsed() < Duration::from_millis(300), "waited {:?}", t.elapsed());
        assert!(reasons[0].text.contains("starting"), "{reasons:?}");

        let (began, outcome) = slow.join().unwrap();
        let Attempt::Fallback(reasons) = outcome else {
            panic!("drew a frame with a stalled backend");
        };
        assert!(reasons[0].text.contains("took longer"), "{reasons:?}");
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "the preview waited for the stalled build"
        );
        assert!(preview.status().reason.unwrap().contains("took longer"));
        assert_eq!(started.load(Ordering::SeqCst), 1);
        let _ = release_tx.send(());
    }

    #[test]
    fn a_build_that_panics_is_an_error_and_not_a_crash() {
        let preview = with(
            Resolution {
                technique: Some(Technique::Child),
                note: None,
            },
            Box::new(|_| panic!("x11rb blew up")),
        );
        preview.set_enabled(true);
        visible(&preview);
        let Attempt::Fallback(reasons) = preview.attempt(&inputs(), Want::at(0.5), Instant::now()) else {
            panic!("drew a frame");
        };
        assert!(
            reasons[0].text.contains("panicked") && reasons[0].text.contains("x11rb blew up"),
            "{reasons:?}"
        );
        assert!(!preview.status().ready);
    }

    #[test]
    fn a_panic_in_the_gpu_path_is_caught_the_surface_hidden_and_the_frame_is_ffmpegs() {
        let mut preview = failing(Arc::default(), "unused");
        // The colour policy is read after the visibility checks, inside the guarded path.
        preview.policy = || panic!("wgpu panicked");
        preview.set_enabled(true);
        visible(&preview);
        let result = catch_unwind(AssertUnwindSafe(|| preview.frame(&inputs(), Want::at(0.5), 320)));
        let result = result.expect("the panic reached the caller");
        // The FFmpeg half has nothing to read here, so it is its own error: what matters is that it was
        // reached, instead of the command dying with the panic.
        assert!(result.is_err() || result.as_ref().is_ok_and(|r| r.renderer == "ffmpeg"));
        let status = preview.status();
        assert!(!status.ready);
        assert!(status.reason.unwrap().contains("panicked: wgpu panicked"));
        assert_eq!(lock(&preview.render).backoff.strikes(), 1, "it is rebuilt behind the backoff");
    }

    #[test]
    fn a_run_that_ended_while_the_gpu_preview_was_starting_turns_it_off_next_time() {
        let dir = std::env::temp_dir().join(format!("kerf-gpu-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!take_crash_marker(&dir), "no marker, no crash");
        std::fs::write(dir.join(CRASH_MARKER), "123").unwrap();
        assert!(take_crash_marker(&dir), "the marker says the last run died");
        assert!(!dir.join(CRASH_MARKER).exists(), "and is taken, so it is not found twice");
        assert!(!take_crash_marker(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_marker_is_down_while_a_backend_is_built_and_gone_when_the_build_fails() {
        let dir = std::env::temp_dir().join(format!("kerf-gpu-build-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let marker = dir.join(CRASH_MARKER);
        let seen = Arc::new(Mutex::new(None));
        let (probe, path) = (seen.clone(), marker.clone());
        let mut preview = with(
            Resolution {
                technique: Some(Technique::Child),
                note: None,
            },
            Box::new(move |_| {
                *lock(&probe) = Some(path.exists());
                Err("no adapter".to_string())
            }),
        );
        preview.marker = Some(marker.clone());
        preview.set_enabled(true);
        visible(&preview);
        let _ = preview.attempt(&inputs(), Want::at(0.5), Instant::now());
        assert_eq!(*lock(&seen), Some(true), "the marker was down during the build");
        assert!(!marker.exists(), "a build that failed cleanly is not a crash");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_build_that_never_answers_leaves_the_marker_down() {
        let dir = std::env::temp_dir().join(format!("kerf-gpu-stall-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let marker = dir.join(CRASH_MARKER);
        let mut preview = with(
            Resolution {
                technique: Some(Technique::Child),
                note: None,
            },
            Box::new(|_| {
                std::thread::sleep(Duration::from_millis(800));
                Err("late".to_string())
            }),
        );
        preview.marker = Some(marker.clone());
        preview.build_deadline = Duration::from_millis(100);
        preview.set_enabled(true);
        visible(&preview);
        let _ = preview.attempt(&inputs(), Want::at(0.5), Instant::now());
        // If the app is killed now (a hung driver) the next launch starts with the preview off.
        assert!(marker.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_webview_is_made_opaque_again_when_the_guard_goes_by_any_path() {
        let restored = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = restored.clone();
        let guard = RestoreOnDrop::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(restored.load(Ordering::SeqCst), 0);
        // A build that fails after the webview was made transparent drops the host, and with it this.
        let failed_build = || -> Result<(), String> {
            let _held = guard;
            Err("the compositor could not be built".into())
        };
        assert!(failed_build().is_err());
        assert_eq!(restored.load(Ordering::SeqCst), 1);
    }
}
