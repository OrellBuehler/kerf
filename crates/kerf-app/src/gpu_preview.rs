//! The GPU preview (plan work package A2): the Preview panel's frame drawn by the wgpu
//! compositor in a native surface, instead of FFmpeg's JPEG, **for the frames the compositor
//! draws exactly**.
//!
//! It is opt-in (*Settings › Preview › GPU preview*, off by default) and strictly additive: with
//! the setting off nothing here runs, and with it on every failure — no adapter, no surface, a
//! lost device, a frame the plan refuses, a decode that is busy — ends in the JPEG that was
//! always there, for that frame, with the reason in the result. The webview asks for a frame
//! ([`GpuPreview::frame`]) and is told which renderer made it.
//!
//! # Pieces
//!
//! * **The decision** is the plan's: [`kerf_core::RenderPlan::reasons`] with the compositor's
//!   caps at the size it would render. Nothing is decided here that the plan does not know,
//!   except what the *surface* adds: the panel has no place yet, it is hidden, or the page has
//!   to draw over the picture on a surface that sits above the page ([`Technique::overlays`]).
//! * **Lock-free**: the plan's inputs are taken under the project lock, which is released before
//!   anything is decoded, planned from files or rendered ([`GpuPreview::frame`]).
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
//!   evidence for each is in `CLAUDE.md` and the progress log.
//!
//! This is a GUI-only surface: it has no MCP tool, because the agent already has
//! `preview_timeline` (a JPEG of the cut) and nothing here is an edit.

#[cfg(target_os = "linux")]
mod x11;

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use kerf_core::{
    composite_color_policy, Asset, CompositeColorPolicy, ExportOptions, PlanRequest, Planner, Project, ProxyMedia, Timeline,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuError, GpuOptions, Hint, PixelRect, Presenter, Surround};
use serde::{Deserialize, Serialize};

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
/// * **macOS** — none: a surface under the webview needs a transparent window, which Tauri
///   gates behind its `macos-private-api` feature (a private WKWebView key). That changes the
///   build of every macOS bundle, so it is left for a session that can run one.
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
        "macos" => none(
            "a native surface under the webview needs a transparent window, which on macOS is Tauri's \
             macos-private-api feature; it is not enabled in this build (KERF_GPU_SURFACE=window forces it)",
        ),
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
        }
    }

    /// The size of the window a [`Technique::Window`] surface covers, in device pixels.
    fn window_size(&self) -> Option<(u32, u32)> {
        match self {
            Host::Window { window, .. } => window.inner_size().ok().map(|s| (s.width, s.height)),
            #[cfg(target_os = "linux")]
            Host::Child(_) => None,
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
        }
    }

    /// The page's frame moved or resized: a child window that is on show follows it now.
    fn follow(&mut self, layout: &Layout) {
        let _ = layout;
        match self {
            Host::Window { .. } => {}
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
            #[cfg(target_os = "linux")]
            Host::Child(child) => child.hide(),
        }
    }

    fn release(&mut self) {
        match self {
            // Dropping the guard restores the background.
            Host::Window { restore, .. } => drop(restore.take()),
            #[cfg(target_os = "linux")]
            Host::Child(child) => child.hide(),
        }
    }
}

/// Everything a GPU frame needs, built together and dropped together.
struct Backend {
    // Field order is drop order: the presenter (and its surface) go before the window host.
    presenter: Presenter,
    compositor: Compositor,
    source: Arc<FrameSource>,
    gpu: Arc<Gpu>,
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
    /// `"gpu"` (drawn in the native surface; the webview shows it by showing nothing over it) or
    /// `"ffmpeg"` (the `frame` data URL).
    pub renderer: &'static str,
    pub frame: Option<String>,
    /// Why the GPU did not draw it (empty when it did).
    pub reasons: Vec<String>,
    pub timings: Option<GpuTimings>,
}

/// The inputs of one frame, taken under the project lock.
pub struct PlanInputs {
    timeline: Timeline,
    /// The assets as imported: the plan resolves the proxies itself.
    assets: Vec<Asset>,
    /// The proxy-swapped assets the FFmpeg preview graph reads.
    preview_assets: Vec<Asset>,
}

/// Take the inputs of a frame. Call under the project lock; nothing slow happens here.
pub fn plan_inputs(project: &Project) -> Result<PlanInputs, String> {
    let (timeline, preview_assets) = project.timeline_frame_inputs().map_err(|e| e.to_string())?;
    let assets = project.list_assets().map_err(|e| e.to_string())?;
    Ok(PlanInputs {
        timeline,
        assets,
        preview_assets,
    })
}

enum Attempt {
    Presented(GpuTimings),
    Fallback(Vec<String>),
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
    bounds: Mutex<BoundsState>,
    /// The live host, outside the render lock so hiding the surface never waits for a render.
    host: Mutex<Option<Arc<Mutex<Host>>>>,
    render: Mutex<RenderState>,
    building: Mutex<()>,
    info: Mutex<Info>,
}

impl GpuPreview {
    /// The app's: `app`'s main window is the host. `crashed` is [`take_crash_marker_in`]'s answer.
    pub fn for_app(app: tauri::AppHandle, crashed: bool) -> Self {
        use tauri::Manager as _;

        let choice = std::env::var("KERF_GPU_SURFACE").ok();
        let marker = app.path().app_config_dir().ok().map(|dir| dir.join(CRASH_MARKER));
        let mut preview = Self::with_factory(
            resolve_technique(std::env::consts::OS, choice.as_deref()),
            Arc::new(move |technique| build_backend(&app, technique)),
        );
        preview.marker = marker;
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
            enabled: AtomicBool::new(false),
            reset_backoff: AtomicBool::new(false),
            info: Mutex::new(Info {
                reason: technique.note.clone(),
                ..Info::default()
            }),
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

    /// A frame for `t`: drawn by the GPU when the plan allows it and everything works, else
    /// FFmpeg's JPEG of `ffmpeg_width`. `overlays_needed` is the page saying it must draw over
    /// the picture (a title box, the trim monitor, safe-area guides).
    ///
    /// The project lock is taken only inside `inputs`, and not held after it returns. A panic in
    /// the GPU path (wgpu, the X connection) is caught here: the backend is dropped and rebuilt
    /// behind the backoff, the surface hidden, and the frame is FFmpeg's like any other fallback.
    pub fn frame(
        &self,
        inputs: impl FnOnce() -> Result<PlanInputs, String>,
        t: f64,
        ffmpeg_width: u32,
        overlays_needed: bool,
    ) -> Result<PreviewFrameResult, String> {
        let inputs = inputs()?;
        let attempt = catch_unwind(AssertUnwindSafe(|| self.attempt(&inputs, t, overlays_needed, Instant::now())));
        let reasons = match attempt {
            Ok(Attempt::Presented(timings)) => {
                return Ok(PreviewFrameResult {
                    renderer: "gpu",
                    frame: None,
                    reasons: Vec::new(),
                    timings: Some(timings),
                })
            }
            Ok(Attempt::Fallback(reasons)) => reasons,
            Err(payload) => vec![self.after_panic(payload.as_ref())],
        };
        tracing::debug!(?reasons, t, "preview frame through FFmpeg");
        let jpeg = Project::composite_timeline_frame(&inputs.timeline, &inputs.preview_assets, t, ffmpeg_width, 4)
            .map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
        Ok(PreviewFrameResult {
            renderer: "ffmpeg",
            frame: Some(format!("data:image/jpeg;base64,{b64}")),
            reasons,
            timings: None,
        })
    }

    /// The GPU path panicked: say so, hide the surface and drop the backend (it may be in any
    /// state), behind the backoff. Every step is itself guarded: this must not panic in turn.
    fn after_panic(&self, payload: &(dyn Any + Send)) -> String {
        let message = panic_text(payload);
        tracing::error!(%message, "the GPU preview panicked; dropping it, this frame goes through FFmpeg");
        let why = format!("the GPU preview panicked: {message}");
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.hide();
            self.drop_backend(&why, false, Instant::now());
        }));
        why
    }

    /// Drop the backend, remember why, and wait before building another (`immediate`: a device that
    /// worked and was lost is rebuilt at once, the first time).
    fn drop_backend(&self, why: &str, immediate: bool, now: Instant) {
        let backend = {
            let mut state = lock(&self.render);
            let wait = state.backoff.fail(immediate);
            state.retry_at = Some(now + wait);
            state.unproven = false;
            state.backend.take()
        };
        *lock(&self.host) = None;
        let _ = catch_unwind(AssertUnwindSafe(move || drop(backend)));
        self.clear_marker();
        let mut info = lock(&self.info);
        info.ready = false;
        info.reason = Some(why.to_string());
    }

    fn attempt(&self, inputs: &PlanInputs, t: f64, overlays_needed: bool, now: Instant) -> Attempt {
        let fallback = |why: String| {
            self.hide();
            Attempt::Fallback(vec![why])
        };
        if !self.is_enabled() {
            return Attempt::Fallback(vec!["the GPU preview is off".into()]);
        }
        let Some(technique) = self.technique.technique else {
            return Attempt::Fallback(vec![self
                .technique
                .note
                .clone()
                .unwrap_or_else(|| "no preview surface here".into())]);
        };
        let Some(bounds) = lock(&self.bounds).current else {
            return Attempt::Fallback(vec!["the preview has not reported where it is".into()]);
        };
        if !bounds.visible {
            return fallback("the preview surface is hidden".into());
        }
        if overlays_needed && !technique.overlays() {
            return fallback("the page has something to draw over the picture, and this surface sits above the page".into());
        }

        // The plan, off the project lock.
        let plan = match Planner::new(
            &inputs.timeline,
            &inputs.assets,
            &ExportOptions::default(),
            PlanRequest::still((self.policy)()).with_media(&ProxyMedia),
        )
        .and_then(|planner| planner.at(t))
        {
            Ok(plan) => plan,
            Err(e) => return fallback(format!("no plan for this frame: {e}")),
        };

        // A backend is built off the render lock, with a deadline: a slow or stuck build is a frame
        // that goes through FFmpeg, not a preview that waits for it.
        if let Err(why) = self.ensure_backend(technique, now) {
            return fallback(why);
        }
        let mut state = lock(&self.render);
        // Looked at again under the lock: the setting may have gone off while this frame was planned.
        if !self.is_enabled() {
            drop(state);
            return fallback("the GPU preview was turned off".into());
        }
        let Some(backend) = state.backend.as_mut() else {
            drop(state);
            return fallback("the GPU preview is not up".into());
        };
        let host = Arc::clone(&backend.host);

        // A child window's surface is the frame itself; the window's covers the window.
        let window_size = match technique {
            Technique::Window => lock(&host).window_size(),
            Technique::Child => Some((0, 0)),
        };
        let Some(window_size) = window_size else {
            drop(state);
            return fallback("the window has no size".into());
        };
        let Some(layout) = layout_for(technique, &bounds, window_size) else {
            drop(state);
            return fallback("the preview frame is outside the window".into());
        };
        let canvas = (plan.canvas.width, plan.canvas.height);
        let (width, _) = place_frame(canvas, layout.area, |w| plan.size(w));
        let size = plan.size(width);
        let reasons = plan.reasons(&backend.compositor.caps(), size);
        if !reasons.is_empty() {
            drop(state);
            self.hide();
            return Attempt::Fallback(reasons.iter().map(ToString::to_string).collect());
        }

        // `None`: the surface was hidden (or the setting turned off) while the frame was drawn.
        let outcome = (|| -> Result<Option<GpuTimings>, GpuError> {
            let (frame, timings) = backend
                .compositor
                .render_plan_texture_with(&plan, size, &backend.source, Hint::Scrub)?;
            let t0 = Instant::now();
            // From here the page's frame is looked at again, and `set_bounds` waits: the layout is the
            // one of the bounds as they are now (the frame may have moved during the render; the
            // picture is scaled into where it is), and a hide that arrived is respected.
            self.under_bounds(|now_bounds| -> Result<GpuTimings, GpuError> {
                let window_size = match technique {
                    Technique::Window => lock(&host).window_size(),
                    Technique::Child => Some((0, 0)),
                }
                .ok_or_else(|| GpuError::Surface("the window has no size".into()))?;
                let layout = layout_for(technique, now_bounds, window_size)
                    .ok_or_else(|| GpuError::Surface("the preview frame is outside the window".into()))?;
                let dest = fit_contain(canvas, layout.area);
                backend.presenter.resize(layout.surface)?;
                // The surface is placed after the frame is drawn, just before it is shown: a child
                // window moved earlier would flash its previous contents.
                lock(&host).apply(&layout).map_err(GpuError::Surface)?;
                backend.presenter.present(
                    &frame,
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
        })();
        match outcome {
            Ok(Some(timings)) => {
                state.backoff.succeeded();
                if std::mem::take(&mut state.unproven) {
                    // A frame reached the screen: whatever the first start risked, it survived.
                    self.clear_marker();
                }
                tracing::debug!(?timings, t, "GPU preview frame presented");
                Attempt::Presented(timings)
            }
            Ok(None) => {
                drop(state);
                fallback("the preview surface was hidden while the frame was drawn".into())
            }
            Err(error) => {
                let reaction = react(&error);
                // A frame the GPU declines (a busy decode, a plan it refuses at this size) is the
                // ordinary fallback; a device or surface that is gone is news.
                if reaction == Reaction::Frame {
                    tracing::debug!(%error, "GPU preview frame declined; this frame goes through FFmpeg");
                } else {
                    tracing::warn!(%error, "GPU preview failed; rebuilding it, and this frame goes through FFmpeg");
                }
                drop(state);
                if reaction == Reaction::Rebuild {
                    self.drop_backend(&error.to_string(), matches!(error, GpuError::DeviceLost(_)), now);
                }
                self.hide();
                Attempt::Fallback(vec![error.to_string()])
            }
        }
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
                info.reason = None;
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

/// Build the app's backend: the device on an adapter that can present to the host, the
/// compositor, a frame source and the presenter.
fn build_backend(app: &tauri::AppHandle, technique: Technique) -> Result<Backend, String> {
    use tauri::Manager as _;

    let window = app.get_webview_window("main").ok_or("the main window does not exist")?;
    let options = GpuOptions {
        force_fallback_adapter: std::env::var("KERF_GPU_ADAPTER").is_ok_and(|v| v == "software"),
    };
    let e = |what: &str| {
        let what = what.to_string();
        move |err: GpuError| format!("{what}: {err}")
    };
    let (gpu, presenter, mut host) = match technique {
        Technique::Window => {
            let (gpu, surface) = Gpu::new_for_surface(options, window.clone()).map_err(e("the window surface"))?;
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
        presenter,
        compositor,
        source,
        gpu,
        host: Arc::new(Mutex::new(host)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_gpu::{Gpu, GpuOptions};

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect { x, y, width, height }
    }

    fn report() -> BoundsReport {
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
        assert!(mac.note.unwrap().contains("macos-private-api"));
        assert!(t("freebsd", None).technique.is_none());
        // The environment overrides, in both directions.
        assert_eq!(t("linux", Some("window")).technique, Some(Technique::Window));
        assert_eq!(t("windows", Some("off")).technique, None);
        assert_eq!(t("macos", Some("window")).technique, Some(Technique::Window));
        assert_eq!(t("linux", Some("child")).technique, Some(Technique::Child));
        assert!(t("windows", Some("child")).technique.is_none(), "no child window outside X11");
        // A value nobody defined is not obeyed.
        assert_eq!(t("windows", Some("nonsense")).technique, Some(Technique::Window));
        assert_eq!(t("linux", Some("")).technique, Some(Technique::Child));
        // Only the technique under a transparent webview lets the page draw over the picture.
        assert!(Technique::Window.overlays());
        assert!(!Technique::Child.overlays());
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
    fn with(technique: Resolution, factory: Box<Factory>) -> GpuPreview {
        let mut preview = GpuPreview::with_factory(technique, Arc::from(factory));
        preview.policy = || CompositeColorPolicy::FixedBt601;
        preview
    }

    fn failing(counter: Arc<std::sync::atomic::AtomicUsize>, why: &'static str) -> GpuPreview {
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
    fn inputs() -> PlanInputs {
        let project = Project::sample().expect("the sample project");
        plan_inputs(&project).expect("inputs")
    }

    fn visible(preview: &GpuPreview) {
        preview.set_bounds(&report()).expect("bounds");
    }

    #[test]
    fn with_the_setting_off_nothing_is_built_and_the_reason_says_so() {
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let preview = failing(built.clone(), "should not be called");
        visible(&preview);
        let Attempt::Fallback(reasons) = preview.attempt(&inputs(), 0.5, false, Instant::now()) else {
            panic!("a disabled preview drew a frame");
        };
        assert!(reasons[0].contains("off"), "{reasons:?}");
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
            let Attempt::Fallback(reasons) = preview.attempt(&inputs, 0.5, false, t0 + Duration::from_millis(n)) else {
                panic!("a failing backend drew a frame");
            };
            assert!(reasons[0].contains("no usable GPU adapter"), "{reasons:?}");
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
        let _ = preview.attempt(&inputs, 0.5, false, t0 + Duration::from_secs(6));
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
        let Attempt::Fallback(reasons) = preview.attempt(&inputs(), 0.5, true, Instant::now()) else {
            panic!("drew over the page");
        };
        assert!(reasons[0].contains("draw over the picture"), "{reasons:?}");
        assert_eq!(built.load(Ordering::SeqCst), 0, "decided before a device is made");
    }

    #[test]
    fn no_bounds_or_hidden_bounds_or_no_technique_is_a_fallback_before_any_work() {
        let built = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let preview = failing(built.clone(), "unused");
        preview.set_enabled(true);
        let Attempt::Fallback(r) = preview.attempt(&inputs(), 0.5, false, Instant::now()) else {
            panic!()
        };
        assert!(r[0].contains("not reported"), "{r:?}");
        let mut hidden = report();
        hidden.visible = false;
        preview.set_bounds(&hidden).unwrap();
        let Attempt::Fallback(r) = preview.attempt(&inputs(), 0.5, false, Instant::now()) else {
            panic!()
        };
        assert!(r[0].contains("hidden"), "{r:?}");
        assert_eq!(built.load(Ordering::SeqCst), 0);

        let macos = with(resolve_technique("macos", None), Box::new(|_| Err("unreachable".into())));
        macos.set_enabled(true);
        assert!(!macos.status().supported);
        assert!(macos.status().reason.unwrap().contains("macos-private-api"));
        visible(&macos);
        let Attempt::Fallback(r) = macos.attempt(&inputs(), 0.5, false, Instant::now()) else {
            panic!()
        };
        assert!(r[0].contains("macos-private-api"), "{r:?}");
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
            let Attempt::Fallback(reasons) = preview.attempt(&inputs, 0.5, false, Instant::now() + Duration::from_millis(n))
            else {
                panic!("drew a frame with no adapter");
            };
            assert!(reasons[0].contains("no usable GPU adapter"), "{reasons:?}");
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
        let _ = preview.attempt(&inputs, 0.5, false, t0);
        let _ = preview.attempt(&inputs, 0.5, false, t0 + Duration::from_secs(1));
        assert_eq!(built.load(Ordering::SeqCst), 1, "backing off");
        // Off and on: the next frame builds again without waiting out the 5 seconds.
        preview.set_enabled(false);
        preview.set_enabled(true);
        let _ = preview.attempt(&inputs, 0.5, false, t0 + Duration::from_secs(2));
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
            std::thread::spawn(move || (Instant::now(), preview.attempt(&inputs, 0.5, false, Instant::now())))
        };
        // While it builds, the render lock is free and another frame does not wait for the build.
        std::thread::sleep(Duration::from_millis(150));
        assert!(preview.render.try_lock().is_ok(), "the build holds the render lock");
        let t = Instant::now();
        let Attempt::Fallback(reasons) = preview.attempt(&inputs, 0.6, false, Instant::now()) else {
            panic!("drew a frame while the backend was building");
        };
        assert!(t.elapsed() < Duration::from_millis(300), "waited {:?}", t.elapsed());
        assert!(reasons[0].contains("starting"), "{reasons:?}");

        let (began, outcome) = slow.join().unwrap();
        let Attempt::Fallback(reasons) = outcome else {
            panic!("drew a frame with a stalled backend");
        };
        assert!(reasons[0].contains("took longer"), "{reasons:?}");
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
        let Attempt::Fallback(reasons) = preview.attempt(&inputs(), 0.5, false, Instant::now()) else {
            panic!("drew a frame");
        };
        assert!(
            reasons[0].contains("panicked") && reasons[0].contains("x11rb blew up"),
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
        let result = catch_unwind(AssertUnwindSafe(|| preview.frame(|| Ok(inputs()), 0.5, 320, false)));
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
        let _ = preview.attempt(&inputs(), 0.5, false, Instant::now());
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
        let _ = preview.attempt(&inputs(), 0.5, false, Instant::now());
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
