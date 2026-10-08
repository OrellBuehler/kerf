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
//! content): `Technique::overlays` says so and the caller falls back to FFmpeg's JPEG for frames
//! that need something over the picture (a title box, a trim monitor, safe-area guides). The
//! window has an **empty input region** (the Shape extension), so pointer events fall through
//! to the webview underneath: the context menu and the title handles keep working under it.
//!
//! X11 only. A Wayland session has no way to put a window of ours inside GTK's: the handle
//! kind is checked and [`Child::create`] refuses it, which leaves the JPEG preview.
//!
//! **The display is ours, and opened once.** `tao`'s `display_handle()` calls `XOpenDisplay` anew
//! each time it is asked and never closes the result (and `new_unchecked`s a null one), so its
//! handle is never used: the process opens one Xlib `Display` of its own ([`display`]), keeps it
//! for its whole life and hands the same pointer to every surface, however often the backend is
//! rebuilt. Only the toplevel's window id comes from the window handle.
//!
//! **Connecting is bounded.** `RustConnection::connect` tries the filesystem socket and then TCP,
//! never the *abstract* socket libxcb tries first, so on a server that listens there only (an
//! `Xvfb` where `/tmp/.X11-unix` is not writable, which WSL is) it waited out a TCP timeout of
//! minutes. [`connect`] tries the abstract socket first, as libxcb does; and the whole backend
//! build runs with a deadline in `gpu_preview`, so whatever else may stall cannot freeze the
//! preview.

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::ptr::NonNull;
use std::sync::Mutex;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
    XlibDisplayHandle, XlibWindowHandle,
};
use x11rb::connection::Connection;
use x11rb::protocol::shape;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, StackMode, WindowClass};
use x11rb::reexports::x11rb_protocol::parse_display::parse_display;
use x11rb::reexports::x11rb_protocol::xauth::get_auth;
use x11rb::rust_connection::{DefaultStream, RustConnection};

/// The process's own Xlib display: opened once, never closed, shared by every surface.
pub struct XlibDisplay {
    ptr: NonNull<c_void>,
    screen: c_int,
    /// What `XDisplayString` says (`:0`, `localhost:10.0`): the name the private connection uses.
    name: String,
    /// libX11 stays loaded for as long as the display is open: forever.
    _lib: libloading::Library,
}

// SAFETY: the pointer is an Xlib `Display*` that is never dereferenced by this process's Rust code
// after `open` and never closed; it is only handed to Vulkan's WSI, which reaches the server through
// its xcb connection (itself thread-safe).
unsafe impl Send for XlibDisplay {}
// SAFETY: as above — shared access copies the pointer and the integers, nothing else.
unsafe impl Sync for XlibDisplay {}

impl XlibDisplay {
    fn open() -> Result<Self, String> {
        // SAFETY: loading libX11 runs its constructors, which do nothing observable here.
        let lib = ["libX11.so.6", "libX11.so"]
            .into_iter()
            .find_map(|name| unsafe { libloading::Library::new(name) }.ok())
            .ok_or("libX11 could not be loaded")?;
        // SAFETY: the three symbols are `Display *XOpenDisplay(const char *)`,
        // `char *XDisplayString(Display *)` and `int XDefaultScreen(Display *)`, as libX11 declares
        // them; the display they return is checked for null before any further use.
        let (ptr, name, screen) = unsafe {
            let open: libloading::Symbol<unsafe extern "C" fn(*const c_char) -> *mut c_void> =
                lib.get(b"XOpenDisplay\0").map_err(|e| e.to_string())?;
            let string: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> *const c_char> =
                lib.get(b"XDisplayString\0").map_err(|e| e.to_string())?;
            let default_screen: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> c_int> =
                lib.get(b"XDefaultScreen\0").map_err(|e| e.to_string())?;
            let ptr = NonNull::new(open(std::ptr::null())).ok_or("the X display could not be opened ($DISPLAY)")?;
            let name = CStr::from_ptr(string(ptr.as_ptr())).to_string_lossy().into_owned();
            (ptr, name, default_screen(ptr.as_ptr()))
        };
        Ok(Self {
            ptr,
            screen,
            name,
            _lib: lib,
        })
    }

    /// The name this display answers to (`:0`).
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The process's one display, opened the first time it is asked for. A failure is not remembered:
/// the next build tries again (behind the backoff).
pub fn display() -> Result<&'static XlibDisplay, String> {
    static OPEN: Mutex<Option<&'static XlibDisplay>> = Mutex::new(None);
    let mut slot = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(display) = *slot {
        return Ok(display);
    }
    let display: &'static XlibDisplay = Box::leak(Box::new(XlibDisplay::open()?));
    *slot = Some(display);
    Ok(display)
}

/// A private connection to the display named `name`, abstract socket first.
fn connect(name: &str) -> Result<(RustConnection, usize), String> {
    let parsed = parse_display(Some(name)).map_err(|e| format!("the display name {name:?}: {e}"))?;
    if parsed.host.is_empty() {
        match connect_abstract(parsed.display, usize::from(parsed.screen)) {
            Ok(connected) => return Ok(connected),
            Err(e) => tracing::debug!(error = %e, display = name, "no abstract X socket; trying the usual ones"),
        }
    }
    RustConnection::connect(Some(name)).map_err(|e| format!("connecting to the X server {name:?}: {e}"))
}

/// The abstract socket (`@/tmp/.X11-unix/X<n>`) libxcb tries before the file one. Refused at once
/// where there is none.
fn connect_abstract(display: u16, screen: usize) -> Result<(RustConnection, usize), String> {
    let address = SocketAddr::from_abstract_name(format!("/tmp/.X11-unix/X{display}")).map_err(|e| e.to_string())?;
    let stream = UnixStream::connect_addr(&address).map_err(|e| e.to_string())?;
    let (stream, (family, peer)) = DefaultStream::from_unix_stream(stream).map_err(|e| e.to_string())?;
    // As `RustConnection::connect` does: no authority, or one that does not match, is tried without.
    let (auth_name, auth_data) = get_auth(family, &peer, display).unwrap_or(None).unwrap_or_default();
    let connection =
        RustConnection::connect_to_stream_with_auth_info(stream, screen, auth_name, auth_data).map_err(|e| e.to_string())?;
    Ok((connection, screen))
}

/// What wgpu needs to make a Vulkan surface on the child: the Xlib handles of the process's display
/// and of the child window. Cheap to copy; the [`Child`] owns the window.
#[derive(Clone, Copy)]
pub struct Target {
    window: c_ulong,
    display: &'static XlibDisplay,
}

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
        let raw = RawDisplayHandle::Xlib(XlibDisplayHandle::new(Some(self.display.ptr), self.display.screen));
        // SAFETY: the display is never closed, see [`XlibDisplay`].
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
    /// handle must be an Xlib one (an X11 session), else this is an error naming the window
    /// system. Every request is checked: a server that refuses the window is an error here, not an
    /// event nobody reads.
    pub fn create(parent: &impl HasWindowHandle) -> Result<Self, String> {
        let window = match parent.window_handle().map_err(x11("the window handle"))?.as_raw() {
            RawWindowHandle::Xlib(h) => h.window,
            RawWindowHandle::Xcb(h) => c_ulong::from(h.window.get()),
            other => {
                return Err(format!(
                    "the window is not an X11 one ({other:?}), so no child window can be put in it"
                ))
            }
        };
        let parent_id = u32::try_from(window).map_err(|_| "the parent window id does not fit an X11 window".to_string())?;
        let display = display()?;

        let (conn, _) = connect(display.name())?;
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
        .map_err(x11("creating the child window"))?
        .check()
        .map_err(x11("the X server refused the child window"))?;
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
        .map_err(x11("making the child window input-transparent (the Shape extension)"))?
        .check()
        .map_err(x11("the X server refused an empty input region (the Shape extension)"))?;
        Ok(Self {
            conn,
            id,
            shown: false,
            placed: None,
            target: Target {
                window: c_ulong::from(id),
                display,
            },
        })
    }

    /// The handles wgpu makes its surface from.
    pub fn target(&self) -> Target {
        self.target
    }

    /// Move and size the window (client pixels of the parent), show it and raise it above its
    /// siblings. The geometry is only sent when it changed, but the raise goes out every time: a
    /// sibling made since (GTK makes native windows of its own) would otherwise end up above it.
    pub fn place(&mut self, x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
        let want = (x, y, width.max(1), height.max(1));
        if self.placed != Some(want) || !self.shown {
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
                .map_err(x11("placing the child window"))?
                .check()
                .map_err(x11("the X server refused to place the child window"))?;
            if !self.shown {
                self.conn
                    .map_window(self.id)
                    .map_err(x11("showing the child window"))?
                    .check()
                    .map_err(x11("the X server refused to show the child window"))?;
            }
            self.placed = Some(want);
            self.shown = true;
        } else {
            self.conn
                .configure_window(self.id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))
                .map_err(x11("raising the child window"))?;
        }
        self.conn.flush().map_err(x11("flushing the X connection"))?;
        Ok(())
    }

    /// Move and size the window **only if it is on show**, at once: the page's frame moved and the
    /// next picture is a while away, so the window follows it rather than sitting over what is
    /// there now. A window that is hidden stays hidden.
    pub fn follow(&mut self, x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
        if !self.shown {
            return Ok(());
        }
        let want = (x, y, width.max(1), height.max(1));
        if self.placed == Some(want) {
            return Ok(());
        }
        self.conn
            .configure_window(
                self.id,
                &ConfigureWindowAux::new().x(want.0).y(want.1).width(want.2).height(want.3),
            )
            .map_err(x11("moving the child window"))?
            .check()
            .map_err(x11("the X server refused to move the child window"))?;
        self.placed = Some(want);
        Ok(())
    }

    /// Unmap the window (the webview's own picture shows through where it was).
    pub fn hide(&mut self) {
        if !self.shown {
            return;
        }
        if let Err(e) = self.conn.unmap_window(self.id).and_then(|_| self.conn.flush()) {
            tracing::debug!(error = %e, "could not hide the GPU preview window");
        }
        self.shown = false;
    }

    #[cfg(test)]
    pub fn is_shown(&self) -> bool {
        self.shown
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Closing the connection destroys the window; destroying it first keeps the order explicit.
        let _ = self.conn.destroy_window(self.id).and_then(|_| self.conn.flush());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use kerf_gpu::{Gpu, GpuError, GpuOptions, PixelRect, Presenter, RenderedFrame, RgbaFrame, Surround};
    use x11rb::protocol::xproto::{CreateWindowAux, ImageFormat};
    use x11rb::COPY_FROM_PARENT;

    /// The app's own window stands in as a plain X window.
    struct Parent {
        window: c_ulong,
    }

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            // SAFETY: the window outlives this value in the test below.
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(XlibWindowHandle::new(self.window))) })
        }
    }

    fn pixel(frame: &RgbaFrame, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * frame.width + x) * 4) as usize;
        [frame.data[i], frame.data[i + 1], frame.data[i + 2]]
    }

    /// The whole technique, minus Tauri: a child window of a toplevel, a Vulkan surface on it, a
    /// frame presented through the presenter and read back **from the X server**, then the device
    /// destroyed to rehearse a loss. Needs an X server (WSLg, Xvfb) and an adapter (lavapipe).
    #[test]
    #[ignore = "needs an X server and a Vulkan adapter (lavapipe is enough)"]
    fn a_frame_presented_to_the_child_window_is_what_the_x_server_holds() {
        let display = display().expect("an X display (is DISPLAY set?)");
        let (conn, screen) = connect(display.name()).expect("connect to the X server");
        let root = conn.setup().roots[screen].root;
        let top = conn.generate_id().unwrap();
        conn.create_window(
            COPY_FROM_PARENT as u8,
            top,
            root,
            0,
            0,
            300,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(0),
        )
        .unwrap();
        conn.map_window(top).unwrap();
        conn.flush().unwrap();

        let parent = Parent {
            window: c_ulong::from(top),
        };
        let mut child = Child::create(&parent).expect("a child window");
        child.place(10, 20, 64, 48).expect("place it");
        let (gpu, surface) = Gpu::new_for_surface(GpuOptions::for_tests(), child.target()).expect("a surface on the child");
        let mut presenter = Presenter::new(Arc::clone(&gpu), surface, (64, 48)).expect("a presenter");

        // Four quadrants of four colours, presented 1:1.
        let colours = [[250, 10, 10], [10, 250, 10], [10, 10, 250], [240, 240, 20]];
        let mut data = Vec::new();
        for y in 0..48u32 {
            for x in 0..64u32 {
                let c = colours[usize::from(x >= 32) + 2 * usize::from(y >= 24)];
                data.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        let sent = RgbaFrame {
            width: 64,
            height: 48,
            data,
        };
        let frame = RenderedFrame::from_rgba(&gpu, &sent).expect("upload");
        let whole = PixelRect {
            x: 0,
            y: 0,
            width: 64,
            height: 48,
        };
        presenter.present(&frame, whole, Surround::whole([0, 0, 0])).expect("present");

        // Let the server have the image, then read the child back from it.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let image = conn
            .get_image(ImageFormat::Z_PIXMAP, child.id, 0, 0, 64, 48, !0)
            .unwrap()
            .reply()
            .expect("read the child window");
        let got = RgbaFrame {
            width: 64,
            height: 48,
            // The server's pixels are BGRX.
            data: image.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect(),
        };
        for (x, y) in [(5, 5), (60, 5), (5, 40), (60, 40), (31, 23), (32, 24)] {
            assert_eq!(pixel(&got, x, y), pixel(&sent, x, y), "pixel ({x}, {y}) of the child window");
        }

        // Moved and hidden like the page asks.
        child.place(0, 0, 32, 24).expect("move it");
        assert!(child.is_shown());
        // A hidden window stays hidden when the page's frame moves; a shown one follows it.
        child.follow(5, 6, 40, 30).expect("follow the frame");
        assert!(child.is_shown());
        child.hide();
        assert!(!child.is_shown());
        child.follow(7, 8, 20, 10).expect("a hidden window ignores it");
        assert!(!child.is_shown());

        // A device that goes away is an error to the caller, the next call says so too, and a
        // new device and presenter on a new surface draw again.
        gpu.destroy();
        let err = presenter.present(&frame, whole, Surround::whole([0, 0, 0])).unwrap_err();
        assert!(matches!(err, GpuError::DeviceLost(_)), "{err:?}");
        assert!(gpu.lost().is_some());
        let (gpu2, surface2) = Gpu::new_for_surface(GpuOptions::for_tests(), child.target()).expect("a surface on a new device");
        let mut presenter2 = Presenter::new(Arc::clone(&gpu2), surface2, (64, 48)).expect("a new presenter");
        let frame2 = RenderedFrame::from_rgba(&gpu2, &sent).expect("upload to the new device");
        presenter2
            .present(&frame2, whole, Surround::whole([0, 0, 0]))
            .expect("present on the new device");
    }

    /// One Xlib display for the life of the process, whoever asks and however often.
    #[test]
    #[ignore = "needs an X server"]
    fn the_process_opens_one_display_and_hands_out_the_same_one() {
        let a = display().expect("an X display (is DISPLAY set?)");
        let b = display().expect("the display again");
        assert!(std::ptr::eq(a, b));
        assert_eq!(a.ptr, b.ptr);
    }

    /// A server that listens on the abstract socket only (an `Xvfb` where `/tmp/.X11-unix` cannot be
    /// written, WSL's case) is reached at once: `RustConnection::connect` alone never tried that
    /// socket and waited out a TCP timeout of minutes.
    #[test]
    #[ignore = "needs Xvfb"]
    fn an_abstract_only_x_server_is_reached_at_once() {
        let number = 60 + std::process::id() % 30;
        let mut server = std::process::Command::new("Xvfb")
            .args([
                format!(":{number}"),
                "-nolisten".into(),
                "unix".into(),
                "-nolisten".into(),
                "tcp".into(),
            ])
            .args(["-screen", "0", "320x200x24"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start Xvfb");
        let name = format!(":{number}");
        // Wait for it to listen (the abstract socket answers or refuses at once), and only then time
        // `connect`: the fallbacks it has after that one are what hung.
        let started = Instant::now();
        while connect_abstract(number as u16, 0).is_err() {
            if started.elapsed() > Duration::from_secs(10) {
                let _ = server.kill();
                panic!("Xvfb never listened on the abstract socket");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let t0 = Instant::now();
        let connected = connect(&name);
        let took = t0.elapsed();
        let _ = server.kill();
        let _ = server.wait();
        let (conn, screen) = connected.expect("connect to Xvfb");
        assert_eq!(screen, 0);
        assert!(!conn.setup().roots.is_empty());
        assert!(took < Duration::from_secs(8), "took {took:?}");
    }
}
