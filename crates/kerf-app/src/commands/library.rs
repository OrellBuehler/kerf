//! Reading the library and repairing it: assets, fonts, the timeline, remove / relink / missing media, transcript search.

use crate::*;

// ---- read ------------------------------------------------------------------

#[tauri::command(async)]
pub(crate) fn list_assets(state: State<'_, AppState>) -> CmdResult<Vec<Asset>> {
    state.project().list_assets().map_err(|e| e.to_string())
}

/// Distinct family names of every font installed on this machine, for the
/// text overlay font picker.
#[tauri::command]
pub(crate) async fn list_fonts() -> CmdResult<Vec<String>> {
    // The first call reads every font file's name table: seconds on a machine with hundreds.
    blocking(|| Ok(kerf_core::list_system_fonts())).await
}

#[tauri::command(async)]
pub(crate) fn get_timeline(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn get_asset_metadata(state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetMetadata> {
    let id = id(&asset_id, "asset_id")?;
    let project = state.project();
    let asset = project.require_asset(id).map_err(|e| e.to_string())?;
    let analysis = project.get_analysis(id).map_err(|e| e.to_string())?;
    Ok(AssetMetadata { asset, analysis })
}

// ---- library (remove / relink / missing media) ----------------------------

/// What removing an asset did: the asset as it was, how many clips went with it (`force`
/// only) and the refreshed cut.
#[derive(Serialize)]
pub(crate) struct AssetRemoval {
    #[serde(flatten)]
    removed: RemovedAsset,
    timeline: Timeline,
}

/// Take an asset out of the library. Refused while a clip of the cut (or of an open proposal)
/// uses it, unless `force`, which first removes those clips as one `Remove media` revision.
/// Its analysis goes with it; its proxies and waveform cache are cleaned up with the lock
/// released.
#[tauri::command]
pub(crate) async fn remove_asset(state: State<'_, AppState>, asset_id: String, force: Option<bool>) -> CmdResult<AssetRemoval> {
    let asset_id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let (removed, timeline) = {
            let project = lock_user(&shared);
            let removed = project
                .remove_asset(asset_id, force.unwrap_or(false))
                .map_err(|e| e.to_string())?;
            (removed, project.timeline().map_err(|e| e.to_string())?)
        };
        // The disk, with the lock released: the proxies (and any build of them) and the waveform.
        kerf_core::proxy::delete(&removed.asset);
        Project::forget_removed_caches(&removed);
        Ok(AssetRemoval { removed, timeline })
    })
    .await
}

/// The error an abandoned relink returns, for the webview to stay quiet on.
const RELINK_CANCELLED: &str = "relink cancelled";

/// Point an asset at another file: the media was moved, renamed or restored from a backup.
/// The new file is probed, and compared with the old one's bytes, with the lock released
/// (`relink_inputs` → `probe_relink` → `apply_relink_probed`); it must be the same kind of media
/// (same streams, about the same length). The asset keeps its id, its place in the library and
/// every clip cut from it, and its analysis is kept only when the file is the old media moved.
/// Refused for an agent, as removing media is.
#[tauri::command]
pub(crate) async fn relink_asset(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    new_path: String,
) -> CmdResult<Relinked> {
    let asset_id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    let scrub = state.scrub.clone();
    // The probe reads both files; `cancel_import` stops it like it stops an import's.
    let cancelled = state.import_stop.begin();
    blocking(move || {
        let (old, generation) = {
            let project = lock_user(&shared);
            (
                project.relink_inputs(asset_id).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let probe = Project::probe_relink(&old, std::path::Path::new(&new_path), &cancelled).map_err(|e| match e {
            kerf_core::Error::Cancelled => RELINK_CANCELLED.to_string(),
            other => other.to_string(),
        })?;
        let relinked = {
            let project = lock_user(&shared);
            require_generation(&project, generation, "relinking")?;
            project.apply_relink_probed(asset_id, &probe).map_err(|e| e.to_string())?
        };
        Project::forget_relinked_caches(&relinked);
        // Frames drawn from the old file are of no use, and the new one wants its proxy.
        scrub.clear();
        spawn_proxy(&app, &relinked.asset);
        let _ = app.emit("proxy-ready", ());
        Ok(relinked)
    })
    .await
}

/// What a relink to `new_path` would cost, asked before it is made: the same probe and the same
/// refusals as `relink_asset` (the lock released while both files are read), and nothing is
/// changed. `analysis_kept` says whether the file is the old media moved; `has_analysis` whether
/// the asset has any analysis to lose, so the prompt stays quiet when a different file costs nothing.
#[tauri::command]
pub(crate) async fn check_relink(state: State<'_, AppState>, asset_id: String, new_path: String) -> CmdResult<RelinkCheck> {
    let asset_id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    let cancelled = state.import_stop.begin();
    blocking(move || {
        let (old, has_analysis) = {
            let project = lock_user(&shared);
            (
                project.relink_inputs(asset_id).map_err(|e| e.to_string())?,
                project.has_analysis(asset_id).map_err(|e| e.to_string())?,
            )
        };
        Project::check_relink(&old, has_analysis, std::path::Path::new(&new_path), &cancelled).map_err(|e| match e {
            kerf_core::Error::Cancelled => RELINK_CANCELLED.to_string(),
            other => other.to_string(),
        })
    })
    .await
}

/// The assets whose source file is no longer on disk, each with how many clips of the cut use
/// it: what a "relink media" prompt lists. The files are asked about with the project lock
/// released (a share that has gone away can take a long time to say so).
#[tauri::command]
pub(crate) async fn missing_assets(state: State<'_, AppState>) -> CmdResult<Vec<MissingAsset>> {
    let shared = state.project.clone();
    blocking(move || {
        let files = lock_user(&shared).missing_assets_inputs().map_err(|e| e.to_string())?;
        Ok(Project::find_missing_assets(files))
    })
    .await
}

/// Every asset's file size and times (what the file system says, nothing stored): what the library
/// sorts by size or date. An asset whose file cannot be read has neither. The files are asked about
/// with the project lock released.
#[tauri::command]
pub(crate) async fn asset_files(state: State<'_, AppState>) -> CmdResult<Vec<AssetFileInfo>> {
    let shared = state.project.clone();
    blocking(move || {
        let files = lock_user(&shared).asset_files_inputs().map_err(|e| e.to_string())?;
        Ok(Project::stat_asset_files(files))
    })
    .await
}

/// Where `query` is said across the library's transcripts, paged, each hit with the clips of the cut
/// that use it. The cached transcripts are read under the lock and searched with it released.
#[tauri::command]
pub(crate) async fn search_transcripts(
    state: State<'_, AppState>,
    query: kerf_core::transcript::TranscriptQuery,
) -> CmdResult<kerf_core::transcript::TranscriptSearch> {
    let shared = state.project.clone();
    blocking(move || {
        let inputs = lock_user(&shared)
            .transcript_search_inputs(query.asset_ids.as_deref())
            .map_err(|e| e.to_string())?;
        kerf_core::transcript::search_transcripts(inputs, &query).map_err(|e| e.to_string())
    })
    .await
}

/// Which of `paths` exist, in order: the export dialog asks before it overwrites a file.
/// Stats on the blocking pool; at most 256 paths.
#[tauri::command]
pub(crate) async fn paths_exist(paths: Vec<String>) -> CmdResult<Vec<bool>> {
    if paths.len() > 256 {
        return Err(format!("{} paths is too many to check at once (256 at most)", paths.len()));
    }
    blocking(move || Ok(paths.iter().map(|p| std::path::Path::new(p).exists()).collect())).await
}
