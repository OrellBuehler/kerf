//! The Linux surface technique: a child X11 window over the Preview panel.
//!
//! WebKitGTK paints into the toplevel's own window (GTK composes the page with cairo into
//! the toplevel and copies it to the screen), so a Vulkan swapchain on that window and a
//! transparent webview over it fight for the same pixels: whichever wrote last wins, and GTK
//! repaints on every expose. A *child* window of the toplevel does not have that problem —
//! X clips the parent's drawing around its children — so the surface lives in one, sized and
//! placed to the Preview frame the webview reports.
//!
//! What it costs is that the webview cannot draw over it (a child window is above its parent's
//! content): `Host::overlays` says so and the caller falls back to FFmpeg's JPEG for frames
//! that need something over the picture (a title box, a trim monitor, safe-area guides). The
//! window has an **empty input region** (the Shape extension), so pointer events fall through
//! to the webview underneath: the context menu and the title handles keep working under it.
//!
//! X11 only. A Wayland session has no way to put a window of ours inside GTK's: the handle
//! kind is checked and [`Child::create`] refuses it, which leaves the JPEG preview.

use std::ffi::{c_int, c_ulong, c_void};
use std::ptr::NonNull;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
    XlibDisplayHandle, XlibWindowHandle,
};
use x11rb::connection::Connection;
use x11rb::protocol::shape;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, StackMode, WindowClass};
use x11rb::rust_connection::RustConnection;

/// What wgpu needs to make a Vulkan surface on the child: the Xlib handles of the app's own
/// display and of the child window. Cheap to copy; the [`Child`] owns the window.
#[derive(Clone, Copy)]
pub struct Target {
    window: c_ulong,
    display: NonNull<c_void>,
    screen: c_int,
}

// SAFETY: the display pointer is GTK's `Display*`, open for the whole life of the process and
// only ever handed on (to Vulkan's WSI, which talks to the server through its xcb connection,
// itself thread-safe); nothing here dereferences it.
unsafe impl Send for Target {}
// SAFETY: as above — a shared reference can do nothing but copy the two integers and the pointer.
unsafe impl Sync for Target {}

impl HasWindowHandle for Target {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::Xlib(XlibWindowHandle::new(self.window));
        // SAFETY: the window is alive as long as the `Child` that made this target, which the
        // backend drops after the surface built from it.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for Target {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let raw = RawDisplayHandle::Xlib(XlibDisplayHandle::new(Some(self.display), self.screen));
        // SAFETY: the display outlives the process's windows, see above.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

/// The child window and the X connection that owns it (the window goes with the connection).
pub struct Child {
    conn: RustConnection,
    id: u32,
    shown: bool,
    placed: Option<(i32, i32, u32, u32)>,
    target: Target,
}

fn x11<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl Child {
    /// Make an unmapped child of `parent`'s toplevel window. `parent` is the app's window; its
    /// handles must be Xlib ones (an X11 session), else this is an error naming the window
    /// system.
    pub fn create(parent: &(impl HasWindowHandle + HasDisplayHandle)) -> Result<Self, String> {
        let window = match parent.window_handle().map_err(x11("the window handle"))?.as_raw() {
            RawWindowHandle::Xlib(h) => h.window,
            RawWindowHandle::Xcb(h) => c_ulong::from(h.window.get()),
            other => {
                return Err(format!(
                    "the window is not an X11 one ({other:?}), so no child window can be put in it"
                ))
            }
        };
        let (display, screen) = match parent.display_handle().map_err(x11("the display handle"))?.as_raw() {
            RawDisplayHandle::Xlib(d) => (d.display.ok_or("the Xlib display handle is empty")?, d.screen),
            other => return Err(format!("the display is not an Xlib one ({other:?})")),
        };
        let parent_id = u32::try_from(window).map_err(|_| "the parent window id does not fit an X11 window".to_string())?;

        let (conn, _) = RustConnection::connect(None).map_err(x11("connecting to the X server"))?;
        let id = conn.generate_id().map_err(x11("allocating a window id"))?;
        conn.create_window(
            x11rb::COPY_FROM_PARENT as u8,
            id,
            parent_id,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(0).border_pixel(0),
        )
        .map_err(x11("creating the child window"))?;
        // An empty *input* region: the pointer goes to whatever is under the window.
        shape::rectangles(
            &conn,
            shape::SO::SET,
            shape::SK::INPUT,
            x11rb::protocol::xproto::ClipOrdering::UNSORTED,
            id,
            0,
            0,
            &[],
        )
        .map_err(x11("making the child window input-transparent (the Shape extension)"))?;
        // A round trip: the errors of the requests above arrive here, not as a panic later.
        conn.get_input_focus()
            .map_err(x11("syncing with the X server"))?
            .reply()
            .map_err(x11("the X server refused the child window"))?;
        Ok(Self {
            conn,
            id,
            shown: false,
            placed: None,
            target: Target {
                window: c_ulong::from(id),
                display,
                screen,
            },
        })
    }

    /// The handles wgpu makes its surface from.
    pub fn target(&self) -> Target {
        self.target
    }

    /// Move and size the window (client pixels of the parent) and show it, above its siblings.
    /// A no-op when it is already there and mapped.
    pub fn place(&mut self, x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
        let want = (x, y, width.max(1), height.max(1));
        if self.shown && self.placed == Some(want) {
            return Ok(());
        }
        self.conn
            .configure_window(
                self.id,
                &ConfigureWindowAux::new()
                    .x(want.0)
                    .y(want.1)
                    .width(want.2)
                    .height(want.3)
                    .stack_mode(StackMode::ABOVE),
            )
            .map_err(x11("placing the child window"))?;
        if !self.shown {
            self.conn.map_window(self.id).map_err(x11("showing the child window"))?;
        }
        self.conn.flush().map_err(x11("flushing the X connection"))?;
        self.placed = Some(want);
        self.shown = true;
        Ok(())
    }

    /// Unmap the window (the webview's own picture shows through where it was).
    pub fn hide(&mut self) {
        if !self.shown {
            return;
        }
        if let Err(e) = self
            .conn
            .unmap_window(self.id)
            .and_then(|_| self.conn.flush().map_err(Into::into))
        {
            tracing::debug!(error = %e, "could not hide the GPU preview window");
        }
        self.shown = false;
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Closing the connection destroys the window; destroying it first keeps the order explicit.
        let _ = self
            .conn
            .destroy_window(self.id)
            .and_then(|_| self.conn.flush().map_err(Into::into));
    }
}
