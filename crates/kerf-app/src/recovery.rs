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

use crate::shell::Host;
use crate::{blocking, lock_user, AppState, CmdResult};

#[derive(Default)]
pub(crate) struct RecoveryState {
    /// `<app data>/recovery`, once resolved.
    root: Option<PathBuf>,
    /// What the launch scan found, each held locked until recovered, discarded or the app exits.
    /// `None` until the webview first asks.
    found: Option<Vec<Recoverable>>,
    /// The session the user chose "Don't save" for as the app closes: deleted at exit, not kept.
    let_go: Option<String>,
}

fn root(app: &impl Host) -> Option<PathBuf> {
    app.data_dir().map(|dir| dir.join("recovery"))
}

/// A new untitled project in a recovery session, or an in-memory one when that cannot be made
/// (no data directory, or no file locking): the app still works, it only cannot recover it.
pub(crate) fn untitled(app: &impl Host) -> CmdResult<Project> {
    let root = app
        .managed::<Mutex<RecoveryState>>()
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
pub(crate) fn start(app: &impl Host) {
    if let Some(state) = app.managed::<Mutex<RecoveryState>>() {
        state.lock().unwrap_or_else(|e| e.into_inner()).root = root(app);
    }
    match untitled(app) {
        Ok(project) => {
            if let Some(id) = project.recovery_id() {
                tracing::info!(session = id, "untitled project backed by a recovery file");
            }
            if let Some(app_state) = app.managed::<AppState>() {
                let old = lock_user(&app_state.project).replace_with(project);
                old.close();
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not start an untitled project"),
    }
}

/// On a clean exit an untitled project that holds no work goes; one that does is kept for the
/// next launch to offer (`Project::close_at_exit`), since the close guard can be bypassed. A
/// crash never gets here, which is the point.
pub(crate) fn on_exit(app: &impl Host) {
    let let_go = app
        .managed::<Mutex<RecoveryState>>()
        .and_then(|s| s.lock().unwrap_or_else(|e| e.into_inner()).let_go.take());
    if let Some(state) = app.managed::<AppState>() {
        if let Ok(empty) = Project::open_in_memory() {
            let old = lock_user(&state.project).replace_with(empty);
            close_on_exit(old, let_go.as_deref());
        }
    }
}

/// Delete the session the user let go of by name; keep any other one that holds work. A session
/// replaced since the answer (a New or an Open that did not close the app) is not the one meant.
fn close_on_exit(project: Project, let_go: Option<&str>) {
    if let_go.is_some() && project.recovery_id() == let_go {
        project.close();
    } else {
        project.close_at_exit();
    }
}

/// The user answered "Don't save" to the close or quit prompt: the untitled project's session is
/// deleted at exit instead of being offered at the next launch. A prompt that failed, or a close
/// that asked nothing, never calls this, so that work is still kept.
#[tauri::command(async)]
pub(crate) fn discard_recovery_at_exit(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let id = state.project().recovery_id().map(str::to_string);
    app.state::<Mutex<RecoveryState>>()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .let_go = id;
    Ok(())
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
        // A project with no file is nothing for a launch to reopen.
        crate::note_open(&app, None);
        Ok(())
    })
    .await
}

/// Delete a session a crashed Kerf left behind.
#[tauri::command]
pub(crate) async fn discard_recovery(app: AppHandle, id: String) -> CmdResult<()> {
    blocking(move || take(&app, &id)?.discard().map_err(|e| e.to_string())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An untitled project with one edit in its history: work a recovery would offer.
    fn worked(root: &std::path::Path) -> Project {
        let project = Project::create_recovery(root).unwrap();
        project.add_marker(1.0, "m".into(), None).unwrap();
        project
    }

    #[test]
    fn a_session_let_go_by_name_is_deleted_and_any_other_kept() {
        let root = std::env::temp_dir().join(format!("kerf-app-recovery-{}", uuid::Uuid::new_v4().simple()));
        let project = worked(&root);
        let dir = project.recovery_dir().unwrap().to_path_buf();
        close_on_exit(project, None);
        assert!(dir.exists(), "work nobody let go of is kept");

        let project = worked(&root);
        let dir = project.recovery_dir().unwrap().to_path_buf();
        close_on_exit(project, Some("another-session"));
        assert!(dir.exists(), "an answer about another session does not reach this one");

        let project = worked(&root);
        let dir = project.recovery_dir().unwrap().to_path_buf();
        let id = project.recovery_id().unwrap().to_string();
        close_on_exit(project, Some(&id));
        assert!(!dir.exists(), "\"Don't save\" deletes it");
        std::fs::remove_dir_all(&root).ok();
    }
}
