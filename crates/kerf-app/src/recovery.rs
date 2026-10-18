//! Crash recovery for the untitled project (`kerf_core::recovery`): where its sessions live, the
//! sessions a crashed run left behind, and the commands that recover or discard them.
//!
//! The sessions live under the app data directory, which the debug identity keeps apart from an
//! installed Kerf's. Only the first instance gets here: a second launch is handed to it by the
//! single-instance plugin before `setup` runs, so it never makes a session of its own.

use std::path::PathBuf;
use std::sync::Mutex;

use kerf_core::{Project, Recoverable, RecoveryInfo};
use tauri::{AppHandle, Manager, State};

use crate::{blocking, lock_user, AppState, CmdResult};

#[derive(Default)]
pub(crate) struct RecoveryState {
    /// `<app data>/recovery`, once resolved.
    root: Option<PathBuf>,
    /// What the launch scan found, each held locked until recovered, discarded or the app exits.
    /// `None` until the webview first asks.
    found: Option<Vec<Recoverable>>,
}

fn root(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join("recovery"))
}

/// A new untitled project in a recovery session, or an in-memory one when that cannot be made
/// (no data directory, or no file locking): the app still works, it only cannot recover it.
pub(crate) fn untitled(app: &AppHandle) -> CmdResult<Project> {
    let root = app
        .try_state::<Mutex<RecoveryState>>()
        .and_then(|s| s.lock().ok()?.root.clone());
    if let Some(root) = root {
        match Project::create_recovery(&root) {
            Ok(project) => return Ok(project),
            Err(e) => tracing::warn!(error = %e, "could not back the untitled project with a recovery file"),
        }
    }
    Project::open_in_memory().map_err(|e| e.to_string())
}

/// Resolve the recovery root and move the launch's empty project into a session.
pub(crate) fn start(app: &AppHandle) {
    let state = app.state::<Mutex<RecoveryState>>();
    state.lock().unwrap_or_else(|e| e.into_inner()).root = root(app);
    match untitled(app) {
        Ok(project) => {
            if let Some(id) = project.recovery_id() {
                tracing::info!(session = id, "untitled project backed by a recovery file");
            }
            let app_state = app.state::<AppState>();
            let old = lock_user(&app_state.project).replace_with(project);
            old.close();
        }
        Err(e) => tracing::warn!(error = %e, "could not start an untitled project"),
    }
}

/// On a clean exit an untitled project that holds no work goes; one that does is kept for the
/// next launch to offer (`Project::close_at_exit`), since the close guard can be bypassed. A
/// crash never gets here, which is the point.
pub(crate) fn on_exit(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(empty) = Project::open_in_memory() {
            let old = lock_user(&state.project).replace_with(empty);
            old.close_at_exit();
        }
    }
}

/// The untitled projects a crashed Kerf left behind that hold real work, newest first. Scanned
/// once per run; a reloaded webview is offered the ones still waiting.
#[tauri::command]
pub(crate) async fn list_recoveries(app: AppHandle) -> CmdResult<Vec<RecoveryInfo>> {
    blocking(move || {
        let own = app.state::<AppState>().project().recovery_id().map(str::to_string);
        let state = app.state::<Mutex<RecoveryState>>();
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if state.found.is_none() {
            let found = match &state.root {
                Some(root) => kerf_core::recovery::scan(root, own.as_deref()),
                None => Vec::new(),
            };
            state.found = Some(found);
        }
        Ok(state.found.iter().flatten().map(|r| r.info.clone()).collect())
    })
    .await
}

fn take(app: &AppHandle, id: &str) -> CmdResult<Recoverable> {
    let state = app.state::<Mutex<RecoveryState>>();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let found = state.found.get_or_insert_with(Vec::new);
    let at = found
        .iter()
        .position(|r| r.info.id == id)
        .ok_or_else(|| format!("no recoverable project {id}"))?;
    Ok(found.remove(at))
}

/// Make a recovered session the open project. It stays untitled until saved; the session the
/// app was using is let go.
#[tauri::command]
pub(crate) async fn recover_project(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let shared = state.project.clone();
    let scrub = state.scrub.clone();
    blocking(move || {
        let found = take(&app, &id)?;
        let recovered = Project::open_recovered(found).map_err(|e| e.to_string())?;
        let old = lock_user(&shared).replace_with(recovered);
        old.close();
        scrub.clear();
        crate::restore_speech_model(&shared);
        crate::respawn_proxies(&app);
        Ok(())
    })
    .await
}

/// Delete a session a crashed Kerf left behind.
#[tauri::command]
pub(crate) async fn discard_recovery(app: AppHandle, id: String) -> CmdResult<()> {
    blocking(move || take(&app, &id)?.discard().map_err(|e| e.to_string())).await
}
