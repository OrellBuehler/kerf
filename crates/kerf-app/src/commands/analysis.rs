//! Importing media, preview proxies, analysis and speech-to-text.

use crate::*;

// ---- import / analysis -----------------------------------------------------

/// The error an abandoned import returns, for the webview to stay quiet on.
const IMPORT_CANCELLED: &str = "import cancelled";

/// Add a media file to the library. An Insta360 lens pair is stitched first, which takes
/// minutes: that streams `import-progress` and `cancel_import` ends it (and every other import
/// under way), rejecting with `"import cancelled"` and leaving no half-written stitch.
#[tauri::command]
pub(crate) async fn import_asset(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Asset> {
    let shared = state.project.clone();
    let cancelled = state.import_stop.begin();
    blocking(move || {
        let generation = lock_user(&shared).generation();
        // Probe (and, for an Insta360 lens pair, stitch) without the lock — so
        // parallel imports really run in parallel and a multi-minute stitch never
        // freezes the GUI or the agent — then take it only for the quick insert.
        let mut on_progress = |p: kerf_core::ExportProgress| {
            let _ = app.emit("import-progress", ImportProgress::new(&path, p));
        };
        let asset =
            Project::probe_import_with(std::path::Path::new(&path), &mut on_progress, &cancelled).map_err(|e| match e {
                kerf_core::Error::Cancelled => IMPORT_CANCELLED.to_string(),
                other => other.to_string(),
            })?;
        // Importing the pair's other lens (or the same file twice) resolves to
        // the asset already in the project instead of duplicating it.
        let asset = {
            let project = lock_user(&shared);
            require_generation(&project, generation, "importing")?;
            project.insert_or_get_asset(&asset).map_err(|e| e.to_string())?
        };
        // Kick off the preview proxy in the background; preview uses the original
        // until it lands (see `spawn_proxy`).
        spawn_proxy(&app, &asset);
        Ok(asset)
    })
    .await
}

/// Stop every import in flight (a stitch is the one that takes long enough to stop).
#[tauri::command(async)]
pub(crate) fn cancel_import(state: State<'_, AppState>) {
    state.import_stop.stop();
}

/// The status of every asset's proxy. The inputs are taken under the lock and the disk is
/// asked with it released (a status may run the cached `ffprobe` that keys the proxy).
#[tauri::command]
pub(crate) async fn proxy_statuses(state: State<'_, AppState>) -> CmdResult<Vec<kerf_core::ProxyStatus>> {
    let shared = state.project.clone();
    blocking(move || {
        let inputs = lock_user(&shared).proxy_inputs().map_err(|e| e.to_string())?;
        Ok(kerf_core::proxy::statuses(&inputs))
    })
    .await
}

#[tauri::command]
pub(crate) async fn rebuild_proxy(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
) -> CmdResult<kerf_core::ProxyStatus> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || rebuild_proxy_now(&app, &|| lock_user(&shared), id)).await
}

#[tauri::command]
pub(crate) async fn delete_proxy(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
) -> CmdResult<kerf_core::ProxyStatus> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || delete_proxy_now(&app, &|| lock_user(&shared), id).map(|(_, status)| status)).await
}

#[tauri::command]
pub(crate) async fn analyze_asset(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    steps: Option<Vec<String>>,
) -> CmdResult<AssetAnalysis> {
    let id = id(&asset_id, "asset_id")?;
    let steps = parse_steps(steps)?;
    let shared = state.project.clone();
    // Fresh cancel flag for this pass; `cancel_analysis` flips it from the UI.
    let cancel = state.begin_analysis();
    blocking(move || {
        run_analysis(&app, &|| lock_user(&shared), id, steps.as_deref(), &|| cancel()).map(|outcome| outcome.analysis)
    })
    .await
}

/// The per-kind analysis status of every asset: which of silence, scenes, loudness, rhythm
/// and transcript are done, running, failed or switched off. Cheap (one cache read each).
#[tauri::command(async)]
pub(crate) fn analysis_statuses(state: State<'_, AppState>) -> CmdResult<Vec<kerf_core::AnalysisStatus>> {
    warm_transcription_backend();
    state.project().analysis_statuses().map_err(|e| e.to_string())
}

// ---- speech-to-text -------------------------------------------------------

/// Which transcription backend this build will use, and whether its model is
/// already downloaded. The transcript tab reads this to explain an empty
/// transcript instead of just showing nothing.
#[tauri::command(async)]
pub(crate) fn transcription_status() -> CmdResult<kerf_core::TranscriptionStatus> {
    Ok(kerf_core::transcription_status())
}

/// Pick which speech model transcription uses, remembering it in the project.
///
/// `None` clears the choice back to the environment / built-in default. The
/// model is not downloaded here — that happens on the next transcription, or
/// via `download_speech_model`.
#[tauri::command(async)]
pub(crate) fn set_speech_model(state: State<'_, AppState>, name: Option<String>) -> CmdResult<kerf_core::TranscriptionStatus> {
    kerf_core::set_speech_model(name.as_deref());
    state
        .project()
        .set_meta(SPEECH_MODEL_KEY, name.as_deref().unwrap_or(""))
        .map_err(|e| e.to_string())?;
    Ok(kerf_core::transcription_status())
}

/// A speech model download in flight.
#[derive(Serialize, Clone)]
struct ModelProgressEvent {
    model: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    fraction: Option<f64>,
}

/// The error an abandoned model download returns, for the webview to stay quiet on.
const MODEL_CANCELLED: &str = "model download cancelled";

/// Download a speech model ahead of time, streaming `model-progress`.
///
/// Transcription downloads on demand anyway; this exists so the user can start
/// the (few hundred megabyte) fetch deliberately, and pick a model other than
/// the default, instead of discovering it mid-analysis. `cancel_model_download`
/// stops it, rejecting with `"model download cancelled"`.
#[tauri::command]
pub(crate) async fn download_speech_model(app: AppHandle, state: State<'_, AppState>, name: String) -> CmdResult<String> {
    let cancel = state.model_cancel.begin();
    blocking(move || {
        let mut on_progress = |p: kerf_core::DownloadProgress| {
            let _ = app.emit(
                "model-progress",
                ModelProgressEvent {
                    model: name.clone(),
                    downloaded_bytes: p.downloaded,
                    total_bytes: p.total,
                    fraction: p.fraction(),
                },
            );
        };
        let path = kerf_core::download_speech_model_cancellable(&name, &mut on_progress, &|| cancel()).map_err(|e| match e {
            kerf_core::Error::Cancelled => MODEL_CANCELLED.to_string(),
            other => other.to_string(),
        })?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

/// Stop the speech model download in flight (if any).
#[tauri::command(async)]
pub(crate) fn cancel_model_download(state: State<'_, AppState>) {
    state.model_cancel.stop();
}
