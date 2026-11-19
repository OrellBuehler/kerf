//! History and the staged edits of the agent's proposal.

use crate::*;

// ---- history (undo / redo / revert) ----------------------------------------

/// The edit history, oldest first. `limit` / `offset` window it from the newest end: `offset`
/// of the latest revisions are skipped and up to `limit` of the ones before them returned, so
/// `limit: 50` is the last fifty edits and a following `offset: 50` the fifty before. Neither
/// is everything.
#[tauri::command(async)]
pub(crate) fn get_history(state: State<'_, AppState>, limit: Option<usize>, offset: Option<usize>) -> CmdResult<Vec<Revision>> {
    let page = Page {
        limit,
        offset: offset.unwrap_or(0),
    };
    state
        .project()
        .history_page(page)
        .map(|(revisions, _)| revisions)
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn undo(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().undo().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn redo(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().redo().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn revert_to(state: State<'_, AppState>, seq: i64) -> CmdResult<Timeline> {
    state.project().revert_to(seq).map_err(|e| e.to_string())
}

/// What one revision changed, so the edit log can explain itself.
#[tauri::command(async)]
pub(crate) fn revision_diff(state: State<'_, AppState>, seq: i64) -> CmdResult<TimelineDiff> {
    state.project().revision_diff(seq).map_err(|e| e.to_string())
}

// ---- staged edits (the agent's pending proposal) ---------------------------

/// The proposal a connected agent has staged, or `None`. Carries its own diff,
/// so the review card renders from one round-trip.
#[tauri::command(async)]
pub(crate) fn get_staged_edit(state: State<'_, AppState>) -> CmdResult<Option<StagedEdit>> {
    state.project().staged().map_err(|e| e.to_string())
}

/// The staged timeline itself — what the editor shows while previewing a
/// proposal, so the user can look at the cut rather than only read about it.
#[tauri::command(async)]
pub(crate) fn get_staged_timeline(state: State<'_, AppState>) -> CmdResult<Option<Timeline>> {
    state.project().staged_timeline().map_err(|e| e.to_string())
}

/// Accept the whole proposal: it becomes the live timeline as one revision. `force` applies
/// one that is `stale` (the cut moved on since it branched), replacing the newer work.
#[tauri::command(async)]
pub(crate) fn apply_staged_edit(app: AppHandle, state: State<'_, AppState>, force: Option<bool>) -> CmdResult<Timeline> {
    let project = state.project();
    let had_asset_changes = staged_asset_changes(&project)?;
    let timeline = project.apply_staged(force.unwrap_or(false)).map_err(|e| e.to_string())?;
    drop(project);
    if had_asset_changes {
        respawn_proxies(&app);
    }
    Ok(timeline)
}

/// Accept only the picked changes (`entry_ids`: the ids of the proposal's diff entries) onto
/// the live cut. A partner that has to come along (a picture and its sound) is applied too and
/// reported in `auto_included`; a pick that cannot stand alone, or no longer applies, is refused
/// or `skipped` with why. The rest of the proposal stays open until nothing is left of it.
#[tauri::command(async)]
pub(crate) fn apply_staged_subset(
    app: AppHandle,
    state: State<'_, AppState>,
    entry_ids: Vec<String>,
    force: Option<bool>,
) -> CmdResult<SubsetApplied> {
    let project = state.project();
    let had_asset_changes = staged_asset_changes(&project)?;
    let applied = project
        .apply_staged_subset(&entry_ids, force.unwrap_or(false))
        .map_err(|e| e.to_string())?;
    drop(project);
    if had_asset_changes {
        respawn_proxies(&app);
    }
    Ok(applied)
}

#[tauri::command(async)]
pub(crate) fn discard_staged_edit(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().discard_staged().map_err(|e| e.to_string())
}

/// What taking back a staged step left: its label, and the proposal as it now stands.
#[derive(Serialize)]
pub(crate) struct StagedUndone {
    label: String,
    staged: StagedEdit,
}

/// Take back the latest edit the agent staged, inside the proposal (the live cut is untouched).
#[tauri::command(async)]
pub(crate) fn undo_staged_edit(state: State<'_, AppState>) -> CmdResult<StagedUndone> {
    let (label, staged) = state.project().undo_staged().map_err(|e| e.to_string())?;
    Ok(StagedUndone { label, staged })
}

/// The proposal the user last discarded, while it can still be brought back; `None` once a
/// new one opened or nothing was discarded.
#[tauri::command(async)]
pub(crate) fn discarded_staged_edit(state: State<'_, AppState>) -> CmdResult<Option<StagedEdit>> {
    state.project().discarded_staged().map_err(|e| e.to_string())
}

/// Bring the discarded proposal back (only while no other proposal is open).
#[tauri::command(async)]
pub(crate) fn restore_discarded_edits(state: State<'_, AppState>) -> CmdResult<StagedEdit> {
    state.project().restore_discarded().map_err(|e| e.to_string())
}
