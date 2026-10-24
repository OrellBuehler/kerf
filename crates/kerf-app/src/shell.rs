//! What the MCP server, and the helpers it reaches, need from the app around them: send an event,
//! find a piece of managed state, and know where the config and data directories are.
//!
//! In the desktop app that is the Tauri [`AppHandle`]. `kerf-app --headless` has no Tauri app at
//! all (building one opens a display), so [`Shell::Headless`] answers the same questions itself:
//! events reach the in-process listeners and nothing else, and the state it holds is the
//! [`AppState`] and the recovery state. Code the MCP server reaches takes a [`Shell`] or an
//! `&impl Host`, never an `AppHandle`. [`Shell`]'s inherent methods mirror Tauri's (`emit`,
//! `state`, `try_state`, `listen`, `unlisten`), so a tool reads the same either way.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, EventId, Listener, Manager};

pub(crate) trait Host: Clone + Send + Sync + 'static {
    /// Emit `event` (Tauri's `emit`): to the webview and the Rust listeners, or headless to the
    /// listeners alone.
    fn send<S: Serialize + Clone>(&self, event: &str, payload: S) -> tauri::Result<()>;
    /// The managed state of type `T`, if there is one (Tauri's `try_state`).
    fn managed<T: Send + Sync + 'static>(&self) -> Option<&T>;
    /// Where the settings file lives.
    fn config_dir(&self) -> Option<PathBuf>;
    /// Where the logs and the recovery sessions live.
    fn data_dir(&self) -> Option<PathBuf>;
    /// No window and no user: the desktop's recent list and the like are not touched.
    fn headless(&self) -> bool;
}

impl Host for AppHandle {
    fn send<S: Serialize + Clone>(&self, event: &str, payload: S) -> tauri::Result<()> {
        Emitter::emit(self, event, payload)
    }

    fn managed<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.try_state::<T>().map(|state| state.inner())
    }

    fn config_dir(&self) -> Option<PathBuf> {
        self.path().app_config_dir().ok()
    }

    fn data_dir(&self) -> Option<PathBuf> {
        self.path().app_data_dir().ok()
    }

    fn headless(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub(crate) enum Shell {
    Gui(AppHandle),
}

impl Host for Shell {
    fn send<S: Serialize + Clone>(&self, event: &str, payload: S) -> tauri::Result<()> {
        let Shell::Gui(app) = self;
        app.send(event, payload)
    }

    fn managed<T: Send + Sync + 'static>(&self) -> Option<&T> {
        match self {
            Shell::Gui(app) => app.managed(),
        }
    }

    fn config_dir(&self) -> Option<PathBuf> {
        match self {
            Shell::Gui(app) => app.config_dir(),
        }
    }

    fn data_dir(&self) -> Option<PathBuf> {
        match self {
            Shell::Gui(app) => app.data_dir(),
        }
    }

    fn headless(&self) -> bool {
        false
    }
}

impl Shell {
    pub(crate) fn emit<S: Serialize + Clone>(&self, event: &str, payload: S) -> tauri::Result<()> {
        self.send(event, payload)
    }

    pub(crate) fn state<T: Send + Sync + 'static>(&self) -> &T {
        self.managed()
            .unwrap_or_else(|| panic!("state() called before manage() for {}", std::any::type_name::<T>()))
    }

    pub(crate) fn try_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.managed()
    }

    /// Call `handler` with the payload (JSON) of every `event` until [`Shell::unlisten`].
    pub(crate) fn listen(&self, event: &str, handler: impl Fn(&str) + Send + Sync + 'static) -> EventId {
        match self {
            Shell::Gui(app) => app.listen(event, move |e| handler(e.payload())),
        }
    }

    pub(crate) fn unlisten(&self, id: EventId) {
        match self {
            Shell::Gui(app) => app.unlisten(id),
        }
    }
}
