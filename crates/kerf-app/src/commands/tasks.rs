//! The agent task queue.

use crate::*;

// ---- agent task queue (mutations return the refreshed queue) ---------------

/// The agent task queue, oldest first; `limit` / `offset` window it from the newest end like
/// `get_history`.
#[tauri::command(async)]
pub(crate) fn list_tasks(state: State<'_, AppState>, limit: Option<usize>, offset: Option<usize>) -> CmdResult<Vec<Task>> {
    let page = Page {
        limit,
        offset: offset.unwrap_or(0),
    };
    state
        .project()
        .list_tasks_page(page)
        .map(|(tasks, _)| tasks)
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn add_task(state: State<'_, AppState>, prompt: String) -> CmdResult<Task> {
    state.project().add_task(&prompt).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn resolve_task(app: AppHandle, state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id, "task_id")?;
    let project = state.project();
    let had_asset_changes = staged_asset_changes(&project)?;
    project.resolve_task(id).map_err(|e| e.to_string())?;
    let tasks = project.list_tasks().map_err(|e| e.to_string())?;
    drop(project);
    if had_asset_changes {
        respawn_proxies(&app);
    }
    Ok(tasks)
}

#[tauri::command(async)]
pub(crate) fn remove_task(state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id, "task_id")?;
    let project = state.project();
    project.remove_task(id).map_err(|e| e.to_string())?;
    project.list_tasks().map_err(|e| e.to_string())
}
