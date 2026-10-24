//! What the MCP server, and the helpers it reaches, need from the app around them: send an event,
//! find a piece of managed state, and know where the config and data directories are.
//!
//! In the desktop app that is the Tauri [`AppHandle`]. `kerf-app --headless` has no Tauri app at
//! all (building one opens a display), so [`Shell::Headless`] answers the same questions itself:
//! events reach the in-process listeners and nothing else, and the state it holds is the
//! [`AppState`] and the recovery state. Code the MCP server reaches takes a [`Shell`] or an
//! `&impl Host`, never an `AppHandle`. [`Shell`]'s inherent methods mirror Tauri's (`emit`,
//! `state`, `try_state`, `listen`, `unlisten`), so a tool reads the same either way.

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, EventId, Listener, Manager};

use crate::recovery::RecoveryState;
use crate::AppState;

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

type Handler = Arc<dyn Fn(&str) + Send + Sync>;

/// The headless app: its state, its directories and the in-process event listeners.
pub(crate) struct Headless {
    state: AppState,
    recovery: Mutex<RecoveryState>,
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    next_id: AtomicU32,
    listeners: Mutex<Vec<(EventId, String, Handler)>>,
}

impl Headless {
    pub(crate) fn new(state: AppState, config: Option<PathBuf>, data: Option<PathBuf>) -> Self {
        Self {
            state,
            recovery: Mutex::new(RecoveryState::default()),
            config,
            data,
            next_id: AtomicU32::new(0),
            listeners: Mutex::new(Vec::new()),
        }
    }

    fn emit<S: Serialize>(&self, event: &str, payload: S) -> tauri::Result<()> {
        let handlers: Vec<Handler> = self
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, name, _)| name == event)
            .map(|(_, _, handler)| handler.clone())
            .collect();
        if handlers.is_empty() {
            return Ok(());
        }
        let payload = serde_json::to_string(&payload)?;
        for handler in handlers {
            handler(&payload);
        }
        Ok(())
    }

    fn listen(&self, event: &str, handler: Handler) -> EventId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, event.to_string(), handler));
        id
    }

    fn unlisten(&self, id: EventId) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(listening, _, _)| *listening != id);
    }

    fn managed<T: Send + Sync + 'static>(&self) -> Option<&T> {
        let held: [&dyn Any; 2] = [&self.state, &self.recovery];
        held.into_iter().find_map(|s| s.downcast_ref::<T>())
    }
}

#[derive(Clone)]
pub(crate) enum Shell {
    Gui(AppHandle),
    Headless(Arc<Headless>),
}

impl Host for Shell {
    fn send<S: Serialize + Clone>(&self, event: &str, payload: S) -> tauri::Result<()> {
        match self {
            Shell::Gui(app) => app.send(event, payload),
            Shell::Headless(h) => h.emit(event, payload),
        }
    }

    fn managed<T: Send + Sync + 'static>(&self) -> Option<&T> {
        match self {
            Shell::Gui(app) => app.managed(),
            Shell::Headless(h) => h.managed(),
        }
    }

    fn config_dir(&self) -> Option<PathBuf> {
        match self {
            Shell::Gui(app) => app.config_dir(),
            Shell::Headless(h) => h.config.clone(),
        }
    }

    fn data_dir(&self) -> Option<PathBuf> {
        match self {
            Shell::Gui(app) => app.data_dir(),
            Shell::Headless(h) => h.data.clone(),
        }
    }

    fn headless(&self) -> bool {
        matches!(self, Shell::Headless(_))
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
            Shell::Headless(h) => h.listen(event, Arc::new(handler)),
        }
    }

    pub(crate) fn unlisten(&self, id: EventId) {
        match self {
            Shell::Gui(app) => app.unlisten(id),
            Shell::Headless(h) => h.unlisten(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> Shell {
        let project = Arc::new(Mutex::new(kerf_core::Project::open_in_memory().unwrap()));
        let state = AppState::new(
            project,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            crate::LaunchSlot::default(),
            Arc::new(kerf_core::ScrubFrames::new(kerf_core::ScrubConfig::default())),
        );
        Shell::Headless(Arc::new(Headless::new(state, None, None)))
    }

    #[test]
    fn a_headless_event_reaches_the_listeners_of_its_name_until_they_stop() {
        let shell = headless();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let id = {
            let heard = heard.clone();
            shell.listen("analysis-progress", move |payload| {
                heard.lock().unwrap().push(payload.to_string());
            })
        };
        shell
            .emit("analysis-progress", serde_json::json!({ "stage": "scenes" }))
            .unwrap();
        shell.emit("project-changed", ()).unwrap();
        assert_eq!(*heard.lock().unwrap(), vec![r#"{"stage":"scenes"}"#.to_string()]);
        shell.unlisten(id);
        shell.emit("analysis-progress", 1).unwrap();
        assert_eq!(heard.lock().unwrap().len(), 1, "nothing after unlisten");
    }

    #[test]
    fn headless_state_is_the_app_state_and_the_recovery_state_and_nothing_else() {
        let shell = headless();
        assert!(shell.headless());
        assert!(shell.try_state::<AppState>().is_some());
        assert!(shell.try_state::<Mutex<RecoveryState>>().is_some());
        assert!(shell.try_state::<crate::editor_context::EditorContextState>().is_none());
        assert!(shell.config_dir().is_none() && shell.data_dir().is_none());
    }
}
