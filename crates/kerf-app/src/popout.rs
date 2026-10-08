//! Detached panels: the OS windows dockview's popout groups live in.
//!
//! dockview opens a popout with `window.open` and then **moves the panel's DOM into
//! the new window's document**, so the panel keeps running in the editor window's
//! JavaScript realm (the `editor` / `ui` singletons, the transport clock, the Web
//! Audio engine, Tauri channels — nothing is synchronised, because nothing is
//! duplicated). That needs a window whose script the editor can reach, and a webview
//! only hands one back when its host answers `window.open` with a window built *from
//! the opener* (wry's `NewWindowResponse::Create`). So:
//!
//! * the main window is **built here from `tauri.conf.json`** (`create: false` there)
//!   rather than by Tauri, because only a builder can carry an `on_new_window`
//!   handler; a window declared in the config has none and `window.open` returns
//!   null from it;
//! * the handler answers **only** a request for the popout page that the editor
//!   announced first ([`PopoutQueue`]): the page asks `popout_expect` for a label, a
//!   rectangle and a background colour, then calls `window.open`. Anything else is
//!   denied, so a stray `window.open` or `target="_blank"` never makes a window;
//! * the window is built `related to` the opener (`window_features`: same web
//!   process on WebKitGTK, same environment on WebView2, same configuration on
//!   WKWebView), sized and placed from the announced rectangle — WebKitGTK ignores
//!   the `window.open` features, and the rectangle is clamped onto a live monitor
//!   first ([`place`], pure) so a layout saved on a screen that is gone cannot open
//!   off-screen;
//! * closing is per platform: wry's WebKitGTK `close` signal destroys only the
//!   webview widget (the window stays, blank), so Linux destroys the window from the
//!   widget's `destroy`; WebView2 destroys the window itself; WKWebView has no
//!   `webViewDidClose:`, so `window.close()` does nothing there and the page asks
//!   [`close_popout`] instead.
//!
//! Evidence for all of it, and what is still unmeasured on Windows and macOS, is in
//! `.claude/plans/progress.md` (B9).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Emitter, Manager, State, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window, WindowEvent};

/// Every detached panel's window label starts with this.
const LABEL_PREFIX: &str = "popout-";
/// The page dockview is told to open (`frontend/static/popout.html`).
const POPOUT_PATH: &str = "/popout.html";
/// Emitted to the editor when a detached panel's window has been destroyed, whichever
/// way (the window manager's close button, `close_popout`, the editor going away).
const CLOSED_EVENT: &str = "popout-closed";

/// How many announced windows may be waiting at once. A restored workspace announces
/// one per popout it holds; this is far above that and well below a runaway.
const MAX_EXPECTED: usize = 32;
/// How long an announcement stays good. A page announces and opens in the same
/// breath; one older than this was never opened (dockview refused before calling
/// `window.open`) and must not be handed to some later, unrelated request.
const EXPECTED_TTL: Duration = Duration::from_secs(15);

/// The smallest a detached panel's window may be: enough for the narrowest panel's
/// own minimum (the library rail plus a media row, the inspector's controls).
const MIN_WIDTH: f64 = 240.0;
const MIN_HEIGHT: f64 = 160.0;
/// How much of a window has to be on a screen for it to count as reachable: its title
/// bar, at least, is where a user takes hold of it.
const REACH_WIDTH: f64 = 120.0;
const REACH_HEIGHT: f64 = 40.0;
/// Where an unplaced window lands relative to the window it was detached from.
const CASCADE: f64 = 48.0;

/// A rectangle in logical pixels, in the coordinates `window.screenX` / `screenY` and
/// `innerWidth` / `innerHeight` use.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.width
    }

    fn bottom(&self) -> f64 {
        self.y + self.height
    }

    /// The area two rectangles share (0 when they do not meet).
    fn overlap(&self, other: &Rect) -> f64 {
        let w = self.right().min(other.right()) - self.x.max(other.x);
        let h = self.bottom().min(other.bottom()) - self.y.max(other.y);
        if w > 0.0 && h > 0.0 {
            w * h
        } else {
            0.0
        }
    }

    fn is_finite(&self) -> bool {
        [self.x, self.y, self.width, self.height].iter().all(|v| v.is_finite())
    }
}

/// Where a detached panel's window opens: a size, and a position unless the platform
/// is left to choose one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub width: f64,
    pub height: f64,
    pub position: Option<(f64, f64)>,
}

/// A window's size, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    fn is_finite(&self) -> bool {
        self.width.is_finite() && self.height.is_finite()
    }
}

/// Where a detached panel's window may open, given the screens there are.
///
/// * A rectangle asked for (a layout being restored) on a screen: kept, shrunk to that
///   screen if it is bigger and shifted in if it hangs over an edge.
/// * A rectangle that no longer reaches any screen (the monitor it was saved on is
///   gone, or the resolution fell): the same size, on `home`, cascaded from `home`'s
///   corner.
/// * No rectangle (a panel being detached by hand): `size` — the panel's own, or the
///   default — **centred on another screen** when there is one, which is what a second
///   monitor is for; on a single screen the platform places it.
///
/// `monitors` and `home` are in the same logical pixels as `want`. With no monitor
/// known at all (a platform that would not say) the rectangle is trusted as it is,
/// bar the minimum size: nothing here may stop a window opening.
pub fn place(want: Option<Rect>, size: Option<Size>, monitors: &[Rect], home: Option<Rect>) -> Placement {
    let home = home.or_else(|| monitors.first().copied());
    let Some(want) = want.filter(Rect::is_finite) else {
        let size = size.filter(Size::is_finite).unwrap_or(Size {
            width: default_size().0,
            height: default_size().1,
        });
        let other = monitors.iter().find(|m| Some(**m) != home);
        let on = other.copied().or(home);
        let width = on.map_or(size.width, |m| size.width.min(m.width)).max(MIN_WIDTH);
        let height = on.map_or(size.height, |m| size.height.min(m.height)).max(MIN_HEIGHT);
        let position = other.map(|m| (m.x + (m.width - width) / 2.0, m.y + (m.height - height) / 2.0));
        return Placement { width, height, position };
    };
    let width = want.width.max(MIN_WIDTH);
    let height = want.height.max(MIN_HEIGHT);
    let asked = Rect { width, height, ..want };
    if monitors.is_empty() {
        return Placement {
            width,
            height,
            position: Some((want.x, want.y)),
        };
    }
    let reach = Rect {
        x: asked.x,
        y: asked.y,
        width: REACH_WIDTH.min(width),
        height: REACH_HEIGHT.min(height),
    };
    // The screen the window mostly sits on; one it only touches by the reach box
    // still counts, so a window dragged mostly off the right edge is pulled back in.
    let on = monitors
        .iter()
        .max_by(|a, b| asked.overlap(a).total_cmp(&asked.overlap(b)))
        .filter(|m| asked.overlap(m) > 0.0 && m.overlap(&reach) >= reach.width * reach.height)
        .or_else(|| monitors.iter().find(|m| m.overlap(&reach) >= reach.width * reach.height));
    match on {
        Some(screen) => fit(asked, screen),
        None => {
            let screen = home.unwrap_or(monitors[0]);
            let mut p = fit(
                Rect {
                    x: screen.x + CASCADE,
                    y: screen.y + CASCADE,
                    width,
                    height,
                },
                &screen,
            );
            if p.position.is_none() {
                p.position = Some((screen.x, screen.y));
            }
            p
        }
    }
}

/// `r` shrunk to `screen` where it is larger and shifted in where it overhangs.
fn fit(r: Rect, screen: &Rect) -> Placement {
    let width = r.width.min(screen.width).max(MIN_WIDTH.min(screen.width));
    let height = r.height.min(screen.height).max(MIN_HEIGHT.min(screen.height));
    let x = r.x.min(screen.right() - width).max(screen.x);
    let y = r.y.min(screen.bottom() - height).max(screen.y);
    Placement {
        width,
        height,
        position: Some((x, y)),
    }
}

/// What a detached panel's window measures when nothing says otherwise.
const fn default_size() -> (f64, f64) {
    (640.0, 480.0)
}

/// `#rrggbb` as its channels. Anything else is `None`: the window keeps its default
/// backdrop rather than guessing.
pub fn parse_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// Whether `label` names a window this module made.
pub fn is_popout_label(label: &str) -> bool {
    label.len() <= 40
        && label
            .strip_prefix(LABEL_PREFIX)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// Whether `url` is the popout page of the app `main` (the editor webview's own URL) is
/// serving: the same scheme, host and port — whichever those are on this platform
/// (`tauri://localhost`, `http://tauri.localhost`, the dev server) — and the popout path.
/// A script in the editor page could otherwise ask for any page at that path.
fn is_popout_url(url: &Url, main: &Url) -> bool {
    url.path() == POPOUT_PATH
        && url.scheme() == main.scheme()
        && url.host_str() == main.host_str()
        && url.port_or_known_default() == main.port_or_known_default()
}

/// A window the editor announced and has yet to open.
#[derive(Debug, Clone, PartialEq)]
pub struct Expected {
    pub label: String,
    pub placement: Placement,
    pub background: Option<[u8; 3]>,
    announced: Instant,
}

impl Expected {
    pub fn new(label: String, placement: Placement, background: Option<[u8; 3]>, now: Instant) -> Self {
        Expected {
            label,
            placement,
            background,
            announced: now,
        }
    }
}

/// The windows announced and not yet opened, oldest first. `window.open` carries no
/// way to say which one it is for, so they are taken in the order they were
/// announced; the editor announces and opens one at a time (dockview restores
/// popouts serially), and a page that gave up on one cancels it.
#[derive(Debug, Default)]
pub struct PopoutQueue {
    items: VecDeque<Expected>,
}

impl PopoutQueue {
    /// Announce a window. Refused when the queue is full or the label is waiting.
    pub fn expect(&mut self, item: Expected, now: Instant) -> Result<(), &'static str> {
        self.items
            .retain(|e| now.saturating_duration_since(e.announced) < EXPECTED_TTL);
        if self.items.len() >= MAX_EXPECTED {
            return Err("too many popout windows are waiting to open");
        }
        if self.items.iter().any(|e| e.label == item.label) {
            return Err("that popout window was already announced");
        }
        self.items.push_back(item);
        Ok(())
    }

    /// The window the next `window.open` is for, if one is waiting and fresh.
    pub fn take(&mut self, now: Instant) -> Option<Expected> {
        while let Some(item) = self.items.pop_front() {
            if now.saturating_duration_since(item.announced) < EXPECTED_TTL {
                return Some(item);
            }
        }
        None
    }

    /// Forget an announcement that will not be opened. Whether it was waiting.
    pub fn cancel(&mut self, label: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|e| e.label != label);
        self.items.len() != before
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// App state: the announcements and the labels handed out.
#[derive(Default)]
pub struct PopoutState {
    queue: Mutex<PopoutQueue>,
    next: AtomicU32,
}

impl PopoutState {
    fn queue(&self) -> MutexGuard<'_, PopoutQueue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What the page asks for when it is about to open a detached panel's window.
#[derive(Debug, Deserialize)]
pub struct ExpectRequest {
    /// Where it should open, in `screenX` / `innerWidth` pixels (a layout being
    /// restored); absent for a panel detached by hand.
    pub rect: Option<Rect>,
    /// How big it should be when there is no rectangle: the panel's own size.
    pub size: Option<Size>,
    /// The window's backdrop until the panel is in it (`#rrggbb`): the live theme's
    /// app surface, so a light theme does not flash dark.
    pub background: Option<String>,
}

/// A monitor's area in logical pixels (its work area, so a window is not placed under
/// a taskbar or a menu bar).
fn area(m: &tauri::Monitor) -> Rect {
    let scale = if m.scale_factor() > 0.0 { m.scale_factor() } else { 1.0 };
    let w = m.work_area();
    Rect {
        x: f64::from(w.position.x) / scale,
        y: f64::from(w.position.y) / scale,
        width: f64::from(w.size.width) / scale,
        height: f64::from(w.size.height) / scale,
    }
}

/// The monitors there are and the one the editor window is on.
fn screens(app: &AppHandle) -> (Vec<Rect>, Option<Rect>) {
    let monitors = app.available_monitors().unwrap_or_default();
    let home = app
        .get_webview_window("main")
        .and_then(|w| w.current_monitor().ok().flatten())
        .map(|m| area(&m));
    (monitors.iter().map(area).collect(), home)
}

/// What `popout_expect` answers: the label, and where the window will open — which is not
/// always where it was asked to (a rectangle off every screen is moved onto one), so the
/// page compares the window it gets with this, not with its own request.
#[derive(Debug, Serialize)]
pub struct Announced {
    pub label: String,
    pub position: Option<(f64, f64)>,
}

/// Announce a detached panel's window, and get its label. The page calls this right
/// before `window.open` (dockview's `addPopoutGroup`, or `fromJSON` restoring one) and
/// keeps the label for [`close_popout`]. The rectangle is placed onto a live monitor
/// here, so the page can hand over whatever a saved layout holds.
#[tauri::command(async)]
pub fn popout_expect(app: AppHandle, state: State<'_, PopoutState>, request: ExpectRequest) -> Result<Announced, String> {
    let (monitors, home) = screens(&app);
    let placement = place(request.rect, request.size, &monitors, home);
    tracing::info!(?placement, ?home, monitors = ?monitors, "detached panel window placed");
    let label = format!("{LABEL_PREFIX}{}", state.next.fetch_add(1, Ordering::Relaxed));
    let background = request.background.as_deref().and_then(parse_color);
    let now = Instant::now();
    state
        .queue()
        .expect(Expected::new(label.clone(), placement, background, now), now)
        .map_err(str::to_string)?;
    Ok(Announced {
        label,
        position: placement.position,
    })
}

/// Move a detached panel's window. The page uses it once, right after a window opened
/// where a saved layout asked: the platform reads a window's position (`screenX`) from
/// where it puts it (`position`) a title bar or a shadow apart, and a window saved at the
/// first and reopened at the second would creep that far at every launch.
#[tauri::command(async)]
pub fn popout_move(app: AppHandle, label: String, x: f64, y: f64) -> Result<(), String> {
    if !is_popout_label(&label) {
        return Err(format!("{label} is not a detached panel window"));
    }
    if !x.is_finite() || !y.is_finite() {
        return Err("a window cannot be moved to a place that is not a number".to_string());
    }
    if let Some(window) = app.get_webview_window(&label) {
        window
            .set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The page announced a window and then did not open it (dockview refused, or
/// `window.open` was blocked). Without this the announcement would wait for its
/// time-out and, until then, be handed to the next window that opens.
#[tauri::command(async)]
pub fn popout_cancel(state: State<'_, PopoutState>, label: String) {
    state.queue().cancel(&label);
}

/// Destroy a detached panel's window. WKWebView has no way for `window.close()` to do
/// it (wry's UI delegate lacks `webViewDidClose:`), so dockview's close of a popout
/// asks here; on the other platforms it is a no-op for a window already gone.
#[tauri::command(async)]
pub fn close_popout(app: AppHandle, label: String) -> Result<(), String> {
    if !is_popout_label(&label) {
        return Err(format!("{label} is not a detached panel window"));
    }
    match app.get_webview_window(&label) {
        Some(window) => window.destroy().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Bring a detached panel's window to the front. `window.focus()` from the page asks, and
/// a webview is free to ignore it without raising the native window; this raises it.
#[tauri::command(async)]
pub fn popout_focus(app: AppHandle, label: String) -> Result<(), String> {
    if !is_popout_label(&label) {
        return Err(format!("{label} is not a detached panel window"));
    }
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The editor window, built from `tauri.conf.json` with the handler that lets
/// `window.open` make a detached panel's window.
pub fn create_main_window(app: &tauri::App) -> tauri::Result<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
        .ok_or_else(|| tauri::Error::WindowNotFound)?;
    let handle = app.handle().clone();
    let window = WebviewWindowBuilder::from_config(app.handle(), &config)?
        .on_new_window(move |url, features| open_popout(&handle, &url, features))
        // The panels in a detached window are the editor page's; when that page goes (a
        // reload in development) they have nothing left to show or do, and the page
        // restores the windows it should have when it is back.
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Started {
                destroy_popouts(window.app_handle());
            }
        })
        .build()?;
    #[cfg(target_os = "linux")]
    linux::allow_script_windows(&window);
    Ok(window)
}

/// Answer a `window.open`: a window for the announced panel, or nothing.
fn open_popout(app: &AppHandle, url: &Url, features: tauri::webview::NewWindowFeatures) -> NewWindowResponse<tauri::Wry> {
    let Some(main) = app.get_webview_window("main").and_then(|w| w.url().ok()) else {
        tracing::warn!(%url, "window.open refused: the editor's own URL is unknown");
        return NewWindowResponse::Deny;
    };
    if !is_popout_url(url, &main) {
        tracing::warn!(%url, %main, "window.open refused: not the app's popout page");
        return NewWindowResponse::Deny;
    }
    let Some(state) = app.try_state::<PopoutState>() else {
        return NewWindowResponse::Deny;
    };
    let Some(expected) = state.queue().take(Instant::now()) else {
        tracing::warn!(%url, "window.open refused: no detached panel was announced");
        return NewWindowResponse::Deny;
    };
    let Expected {
        label,
        placement,
        background,
        ..
    } = expected;
    // `about:blank`: the engine itself navigates the new webview to the requested
    // URL once it has the window; loading it here too would be a second navigation.
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External("about:blank".parse().expect("static URL")))
        // Carries what makes the new webview related to the opener, and the
        // opener's features where the platform reports them.
        .window_features(features)
        .title("Kerf")
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .inner_size(placement.width, placement.height)
        // A click that raises an inactive window is also the click on what is under
        // it (macOS); a panel window is usually not the active one.
        .accept_first_mouse(true)
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        });
    if let Some((x, y)) = placement.position {
        builder = builder.position(x, y);
    }
    if let Some([r, g, b]) = background {
        builder = builder.background_color(tauri::window::Color(r, g, b, 255));
    }
    match builder.build() {
        Ok(window) => {
            tracing::info!(label, "detached panel window opened");
            #[cfg(target_os = "linux")]
            linux::honour_script_close(&window);
            NewWindowResponse::Create { window }
        }
        Err(e) => {
            tracing::error!(label, error = %e, "could not open a detached panel window");
            NewWindowResponse::Deny
        }
    }
}

/// The app-wide window events that concern detached panels: one going away is told to
/// the editor, and the editor window going away takes every one with it (they have no
/// life of their own — the panels in them are the editor's, and an orphaned window
/// would keep the app running with nothing to drive it).
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if !matches!(event, WindowEvent::Destroyed) {
        return;
    }
    let label = window.label();
    if label == "main" {
        destroy_popouts(window.app_handle());
    } else if is_popout_label(label) {
        tracing::info!(label, "detached panel window closed");
        let _ = window.app_handle().emit_to("main", CLOSED_EVENT, label.to_string());
    }
}

/// Destroy every detached panel's window.
fn destroy_popouts(app: &AppHandle) {
    for (label, window) in app.webview_windows() {
        if is_popout_label(&label) {
            let _ = window.destroy();
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! WebKitGTK needs two things wry does not do.

    use gtk::prelude::WidgetExt;
    use tauri::WebviewWindow;
    use webkit2gtk::{SettingsExt, WebViewExt};

    /// WebKitGTK refuses a `window.open` that no user gesture asked for
    /// (`javascript-can-open-windows-automatically` defaults to false), and the editor
    /// opens detached panels from a restored layout at launch, with none. The
    /// handler still decides whether a window opens at all.
    pub fn allow_script_windows(window: &WebviewWindow) {
        let _ = window.with_webview(|webview| {
            if let Some(settings) = WebViewExt::settings(&webview.inner()) {
                settings.set_javascript_can_open_windows_automatically(true);
            }
        });
    }

    /// `window.close()` from the page makes wry destroy the webview widget and nothing
    /// else, which leaves the window open and blank. Destroy the window when its
    /// webview goes.
    pub fn honour_script_close(window: &WebviewWindow) {
        let handle = window.clone();
        let _ = window.with_webview(move |webview| {
            webview.inner().connect_destroy(move |_| {
                let _ = handle.destroy();
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
    }

    const MAIN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1040.0,
    };
    const SECOND: Rect = Rect {
        x: 1920.0,
        y: 0.0,
        width: 2560.0,
        height: 1400.0,
    };

    fn want(x: f64, y: f64, width: f64, height: f64) -> Option<Rect> {
        Some(screen(x, y, width, height))
    }

    #[test]
    fn a_rectangle_on_a_screen_is_kept() {
        let p = place(want(2100.0, 100.0, 800.0, 600.0), None, &[MAIN, SECOND], Some(MAIN));
        assert_eq!(p.position, Some((2100.0, 100.0)));
        assert_eq!((p.width, p.height), (800.0, 600.0));
    }

    #[test]
    fn a_rectangle_hanging_over_an_edge_is_pulled_in() {
        // Mostly on the second screen, 300 px past its right edge.
        let p = place(want(4180.0, 100.0, 800.0, 600.0), None, &[MAIN, SECOND], Some(MAIN));
        assert_eq!(p.position, Some((4480.0 - 800.0, 100.0)));
        // Past the bottom.
        let p = place(want(100.0, 900.0, 800.0, 600.0), None, &[MAIN], Some(MAIN));
        assert_eq!(p.position, Some((100.0, 1040.0 - 600.0)));
    }

    #[test]
    fn a_rectangle_larger_than_its_screen_shrinks_to_it() {
        let p = place(want(10.0, 10.0, 5000.0, 3000.0), None, &[MAIN], Some(MAIN));
        assert_eq!((p.width, p.height), (1920.0, 1040.0));
        assert_eq!(p.position, Some((0.0, 0.0)));
    }

    #[test]
    fn a_rectangle_on_a_monitor_that_is_gone_opens_on_the_editors_screen() {
        // Saved on a screen at x = 1920; only the main one is left.
        let p = place(want(2100.0, 100.0, 800.0, 600.0), None, &[MAIN], Some(MAIN));
        assert_eq!(p.position, Some((CASCADE, CASCADE)));
        assert_eq!((p.width, p.height), (800.0, 600.0));
        // …and on the other screen when that is where the editor is.
        let p = place(want(-1200.0, 50.0, 800.0, 600.0), None, &[MAIN, SECOND], Some(SECOND));
        assert_eq!(p.position, Some((SECOND.x + CASCADE, SECOND.y + CASCADE)));
    }

    #[test]
    fn a_window_whose_title_bar_is_off_every_screen_is_brought_back() {
        // Its body overlaps the screen by a sliver but the part you grab does not.
        let p = place(want(100.0, -590.0, 800.0, 600.0), None, &[MAIN], Some(MAIN));
        let (x, y) = p.position.unwrap();
        assert!(y >= 0.0 && y + p.height <= MAIN.bottom(), "y {y} h {}", p.height);
        assert!(x >= 0.0 && x + p.width <= MAIN.right());
    }

    #[test]
    fn on_a_single_screen_the_platform_places_a_window_nobody_placed() {
        let p = place(None, None, &[MAIN], Some(MAIN));
        assert_eq!(p.position, None);
        assert_eq!((p.width, p.height), default_size());
        // On a screen smaller than the default.
        let tiny = screen(0.0, 0.0, 500.0, 300.0);
        let p = place(None, None, &[tiny], Some(tiny));
        assert_eq!((p.width, p.height), (500.0, 300.0));
    }

    #[test]
    fn a_panel_detached_by_hand_goes_to_the_other_screen_at_its_own_size() {
        let size = Some(Size {
            width: 800.0,
            height: 600.0,
        });
        let p = place(None, size, &[MAIN, SECOND], Some(MAIN));
        assert_eq!((p.width, p.height), (800.0, 600.0));
        assert_eq!(
            p.position,
            Some((SECOND.x + (SECOND.width - 800.0) / 2.0, (SECOND.height - 600.0) / 2.0))
        );
        // From the other screen it goes back to the first.
        let p = place(None, size, &[MAIN, SECOND], Some(SECOND));
        assert_eq!(p.position, Some(((MAIN.width - 800.0) / 2.0, (MAIN.height - 600.0) / 2.0)));
    }

    #[test]
    fn a_size_that_does_not_fit_the_screen_it_is_sent_to_is_cut_down() {
        let size = Some(Size {
            width: 9000.0,
            height: 9000.0,
        });
        let p = place(None, size, &[MAIN, SECOND], Some(MAIN));
        assert_eq!((p.width, p.height), (SECOND.width, SECOND.height));
        assert_eq!(p.position, Some((SECOND.x, SECOND.y)));
        // A size that is not a number is the default.
        let p = place(
            None,
            Some(Size {
                width: f64::NAN,
                height: 10.0,
            }),
            &[MAIN],
            Some(MAIN),
        );
        assert_eq!((p.width, p.height), default_size());
    }

    #[test]
    fn the_smallest_window_is_enforced() {
        let p = place(want(100.0, 100.0, 10.0, 10.0), None, &[MAIN], Some(MAIN));
        assert_eq!((p.width, p.height), (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn a_platform_that_reports_no_monitor_is_trusted() {
        let p = place(want(-5000.0, 40.0, 700.0, 500.0), None, &[], None);
        assert_eq!(p.position, Some((-5000.0, 40.0)));
        assert_eq!((p.width, p.height), (700.0, 500.0));
        assert_eq!(place(None, None, &[], None).position, None);
    }

    #[test]
    fn a_rectangle_that_is_not_a_number_is_ignored() {
        let p = place(want(f64::NAN, 0.0, 800.0, 600.0), None, &[MAIN], Some(MAIN));
        assert_eq!(p.position, None);
        let p = place(want(0.0, 0.0, f64::INFINITY, 600.0), None, &[MAIN], Some(MAIN));
        assert_eq!(p.position, None);
    }

    #[test]
    fn the_nearest_screen_wins_when_a_window_straddles_two() {
        // 700 px on the first screen, 100 on the second.
        let p = place(want(1220.0, 100.0, 800.0, 600.0), None, &[MAIN, SECOND], Some(MAIN));
        assert_eq!(p.position, Some((1120.0, 100.0)));
    }

    #[test]
    fn colors_are_six_digit_hex_or_nothing() {
        assert_eq!(parse_color("#0f1318"), Some([0x0f, 0x13, 0x18]));
        assert_eq!(parse_color("#FFFFFF"), Some([255, 255, 255]));
        for bad in ["0f1318", "#0f131", "#0f13180", "#gggggg", "rgb(1,2,3)", "", "#é12345"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_label_is_ours_only_if_it_has_the_prefix_and_plain_characters() {
        assert!(is_popout_label("popout-0"));
        assert!(is_popout_label("popout-12-a"));
        for bad in [
            "main",
            "popout-",
            "popout-a b",
            "popout-../x",
            "Popout-1",
            &format!("popout-{}", "9".repeat(40)),
        ] {
            assert!(!is_popout_label(bad), "{bad}");
        }
    }

    #[test]
    fn only_the_popout_page_is_opened() {
        // The page the editor webview is on, per platform: macOS and Linux, Windows
        // (plain and with `use_https_scheme`), and the dev server.
        let mains = [
            "tauri://localhost/",
            "http://tauri.localhost/",
            "https://tauri.localhost/",
            "http://localhost:1420/",
        ];
        for main in mains {
            let main: Url = main.parse().unwrap();
            let own = main.join("popout.html").unwrap();
            assert!(is_popout_url(&own, &main), "{own} on {main}");
            for bad in [
                "https://example.com/popout.html",
                "ftp://x/popout.html",
                "http://127.0.0.1:9999/popout.html",
                "file:///popout.html",
                "tauri://elsewhere/popout.html",
                "http://localhost:1421/popout.html",
                "http://tauri.localhost:8080/popout.html",
            ] {
                assert!(!is_popout_url(&bad.parse().unwrap(), &main), "{bad} on {main}");
            }
            for bad in ["/", "/index.html", "/popout.html/x", "/other/popout.html", "/popout.htm"] {
                let url = own.join(bad).unwrap();
                assert!(!is_popout_url(&url, &main), "{url} on {main}");
            }
        }
        // Another platform's origin is not this one's.
        let main: Url = "tauri://localhost/".parse().unwrap();
        for other in ["http://tauri.localhost/popout.html", "http://localhost:1420/popout.html"] {
            assert!(!is_popout_url(&other.parse().unwrap(), &main), "{other}");
        }
    }

    fn item(label: &str, now: Instant) -> Expected {
        Expected::new(label.to_string(), place(None, None, &[], None), None, now)
    }

    #[test]
    fn announced_windows_open_in_the_order_they_were_announced() {
        let now = Instant::now();
        let mut q = PopoutQueue::default();
        q.expect(item("popout-0", now), now).unwrap();
        q.expect(item("popout-1", now), now).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q.take(now).unwrap().label, "popout-0");
        assert_eq!(q.take(now).unwrap().label, "popout-1");
        assert!(q.take(now).is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn nothing_waiting_means_no_window() {
        let mut q = PopoutQueue::default();
        assert!(q.take(Instant::now()).is_none());
    }

    #[test]
    fn a_window_that_was_never_opened_goes_stale() {
        let then = Instant::now();
        let mut q = PopoutQueue::default();
        q.expect(item("popout-0", then), then).unwrap();
        let later = then + EXPECTED_TTL + Duration::from_millis(1);
        // A later request must not be given a window announced for something else.
        assert!(q.take(later).is_none());
        // …and a stale one is dropped when the next is announced.
        q.expect(item("popout-1", then), then).unwrap();
        q.expect(item("popout-2", later), later).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q.take(later).unwrap().label, "popout-2");
    }

    #[test]
    fn a_window_the_page_gave_up_on_is_forgotten() {
        let now = Instant::now();
        let mut q = PopoutQueue::default();
        q.expect(item("popout-0", now), now).unwrap();
        q.expect(item("popout-1", now), now).unwrap();
        assert!(q.cancel("popout-0"));
        assert!(!q.cancel("popout-0"), "already gone");
        assert_eq!(q.take(now).unwrap().label, "popout-1");
    }

    #[test]
    fn the_queue_is_bounded_and_refuses_a_label_twice() {
        let now = Instant::now();
        let mut q = PopoutQueue::default();
        for i in 0..MAX_EXPECTED {
            q.expect(item(&format!("popout-{i}"), now), now).unwrap();
        }
        assert!(q.expect(item("popout-extra", now), now).is_err());
        let mut q = PopoutQueue::default();
        q.expect(item("popout-0", now), now).unwrap();
        assert!(q.expect(item("popout-0", now), now).is_err());
    }
}
