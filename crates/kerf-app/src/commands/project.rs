//! The project file: new, open, recent projects and save as.

use crate::*;

// ---- project file (open / save) --------------------------------------------

/// Path of the `.kerf` file backing the open project, or `null` if it lives
/// only in memory (the seeded sample) and isn't persisted yet.
#[tauri::command(async)]
pub(crate) fn project_path(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(state.project().path().map(|p| p.display().to_string()))
}

/// Replace the open project with a fresh, empty untitled one (no sample data), backed by a
/// crash-recovery file (`recovery.rs`) but not saved until `save_project_as`. The GUI and the
/// embedded MCP server share this `Project`, so both switch to it.
#[tauri::command]
pub(crate) async fn new_project(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let shared = state.project.clone();
    let scrub = state.scrub.clone();
    blocking(move || {
        let fresh = recovery::untitled(&app)?;
        let mut project = lock_user(&shared);
        let old = project.replace_with(fresh);
        let path = project.path().map(|p| p.display().to_string());
        drop(project);
        old.close();
        scrub.clear();
        // The speech model is remembered per project and a new one remembers none, so
        // transcribing in it uses the default, not the model the project before it chose.
        restore_speech_model(&shared);
        // A project with no file is nothing for a launch to reopen.
        note_open(&app, None);
        Ok(path)
    })
    .await
}

/// Open an existing `.kerf` file, replacing the in-memory project. Both the GUI
/// and the embedded MCP server share this `Project`, so both now operate on —
/// and persist to — the opened file. Returns its path. A path that names nothing is
/// an error: `Project::open` would create it.
#[tauri::command]
pub(crate) async fn open_project(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let shared = state.project.clone();
    let scrub = state.scrub.clone();
    blocking(move || open_project_at(&app, &shared, &scrub, &path)).await
}

/// The open itself, for `open_project` and `open_recent`.
fn open_project_at(
    app: &AppHandle,
    shared: &Mutex<Project>,
    scrub: &kerf_core::ScrubFrames,
    path: &str,
) -> CmdResult<Option<String>> {
    if !std::path::Path::new(path).is_file() {
        return Err(format!("{path} does not exist"));
    }
    // Open the file first, then swap it in — the (disk-bound) open doesn't
    // hold the shared lock, and a failed open leaves the current project intact.
    let opened = Project::open(path).map_err(|e| e.to_string())?;
    let mut project = lock_user(shared);
    let old = project.replace_with(opened);
    let result = project.path().map(|p| p.display().to_string());
    drop(project);
    // An untitled project's recovery directory goes with it: it was let go (or just saved).
    old.close();
    // The frames of the project that was open are of no use to this one.
    scrub.clear();
    // The speech model is remembered per project, so transcribing in the
    // reopened one uses the model it was cut with.
    restore_speech_model(shared);
    // Make sure every video asset in the reopened project has a preview proxy (a cached one
    // is skipped; a missing one is built in the background). One ffprobe per asset keys the
    // proxy, so it is all off this thread: the open must not wait for N probes — a project
    // of eighty clips on a network drive would otherwise sit there for seconds.
    respawn_proxies(app);
    note_open(app, result.as_deref());
    Ok(result)
}

/// The projects opened or saved most recently, newest first (at most 10), the ones whose
/// file is gone dropped and forgotten.
#[tauri::command]
pub(crate) async fn recent_projects(app: AppHandle) -> CmdResult<Vec<String>> {
    blocking(move || settings::recent(&app)).await
}

/// Open one of the recent projects. A file that has since gone is not opened (and not
/// created, as `Project::open` would): the entry is dropped from the list and the error says so.
#[tauri::command]
pub(crate) async fn open_recent(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let shared = state.project.clone();
    let scrub = state.scrub.clone();
    blocking(move || {
        if !std::path::Path::new(&path).is_file() {
            let _ = settings::recent(&app);
            return Err(format!("{path} no longer exists; it was removed from the recent projects"));
        }
        open_project_at(&app, &shared, &scrub, &path)
    })
    .await
}

/// Forget every recent project.
#[tauri::command]
pub(crate) async fn clear_recent_projects(app: AppHandle) -> CmdResult<()> {
    blocking(move || settings::clear_recent(&app).map(|_| ())).await
}

/// Snapshot the current project to a new `.kerf` file and switch to it, so
/// subsequent edits (from the GUI and the agent alike) write through to disk.
/// Returns the saved path.
///
/// The reopen runs with the lock released, like `open_project`, so a slow
/// filesystem doesn't freeze the GUI and every MCP call. An edit landing in that
/// window would be on the old project, so the reopened one is only swapped in if the
/// project has not written anything since (`Project::change_count`, which also counts the
/// analysis a background job merged, the ripple flag and the other rows an edit-only
/// fingerprint missed); otherwise the save is redone under one held lock. A project that
/// was replaced meanwhile (`Project::generation`) is not switched to at all, and the swap itself
/// (`Project::replace_with_saved_copy`) keeps the generation: it is the same project in another
/// file, so an analysis, import or voiceover in flight still lands.
#[tauri::command]
pub(crate) async fn save_project_as(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let shared = state.project.clone();
    blocking(move || {
        // Onto the file that is open already there is nothing to do (every edit is written through).
        let open = lock_user(&shared).path().map(std::path::Path::to_path_buf);
        if open.is_some_and(|open| kerf_core::paths::same_file(&open, std::path::Path::new(&path))) {
            note_open(&app, Some(&path));
            return Ok(Some(path));
        }
        // Not onto a file the project or Kerf itself needs (a source, a proxy, the settings): the
        // lock is held only to read what those are.
        let protected = lock_user(&shared).output_path_inputs().map_err(|e| e.to_string())?;
        check_output(std::path::Path::new(&path), &protected).map_err(|e| e.to_string())?;
        let (generation, changes) = {
            let project = lock_user(&shared);
            project.save_as(&path).map_err(|e| e.to_string())?;
            (project.generation(), project.change_count())
        };
        let opened = Project::open(&path).map_err(|e| e.to_string())?;
        let mut project = lock_user(&shared);
        require_generation(&project, generation, "it was being saved")?;
        // The copy is the same project in another file, so jobs in flight (an analysis, an import, a
        // voiceover) still land: the swap keeps the generation.
        let old = if project.change_count() == changes {
            project.replace_with_saved_copy(opened)
        } else {
            // Close the stale copy first: `save_as` renames over the file it replaces,
            // which Windows refuses while it is open.
            drop(opened);
            project.save_as(&path).map_err(|e| e.to_string())?;
            project.replace_with_saved_copy(Project::open(&path).map_err(|e| e.to_string())?)
        };
        let result = project.path().map(|p| p.display().to_string());
        drop(project);
        // The untitled project it was (and its recovery directory) is saved now: the directory goes.
        old.close();
        note_open(&app, result.as_deref());
        Ok(result)
    })
    .await
}
