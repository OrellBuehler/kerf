//! Tauri v2 shell for Kerf.
//!
//! Owns a single [`Project`] behind a mutex and exposes Tauri commands that
//! bridge the SvelteKit frontend to `kerf-core`. Read commands return domain
//! types; editing commands perform the mutation and return the refreshed
//! [`Timeline`] so the frontend can re-render in a single round-trip.

mod mcp;

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use kerf_core::{Asset, AssetAnalysis, EditSource, Project, Revision, Task, Timeline};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

struct AppState {
    project: Arc<Mutex<Project>>,
}

#[derive(Serialize)]
struct AssetMetadata {
    asset: Asset,
    analysis: Option<AssetAnalysis>,
}

type CmdResult<T> = Result<T, String>;

impl AppState {
    fn project(&self) -> CmdResult<std::sync::MutexGuard<'_, Project>> {
        let mut guard = self.project.lock().map_err(|_| "project mutex poisoned".to_string())?;
        // The GUI is the user; the MCP server attributes its own edits to the
        // agent under the same shared lock (see `mcp::KerfMcp::lock`).
        guard.set_actor(EditSource::User);
        Ok(guard)
    }
}

fn id(s: &str) -> CmdResult<Uuid> {
    Uuid::parse_str(s).map_err(|e| e.to_string())
}

// ---- read ------------------------------------------------------------------

#[tauri::command]
fn list_assets(state: State<'_, AppState>) -> CmdResult<Vec<Asset>> {
    state.project()?.list_assets().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_timeline(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project()?.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_asset_metadata(state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetMetadata> {
    let id = id(&asset_id)?;
    let project = state.project()?;
    let asset = project.require_asset(id).map_err(|e| e.to_string())?;
    let analysis = project.get_analysis(id).map_err(|e| e.to_string())?;
    Ok(AssetMetadata { asset, analysis })
}

// ---- project file (open / save) --------------------------------------------

/// Path of the `.kerf` file backing the open project, or `null` if it lives
/// only in memory (the seeded sample) and isn't persisted yet.
#[tauri::command]
fn project_path(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(state.project()?.path().map(|p| p.display().to_string()))
}

/// Replace the open project with a fresh, empty in-memory one (no sample data).
/// Like the seeded sample it isn't persisted until `save_project_as`. The GUI and
/// the embedded MCP server share this `Project`, so both switch to it.
#[tauri::command]
fn new_project(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let mut project = state.project()?;
    *project = Project::open_in_memory().map_err(|e| e.to_string())?;
    Ok(project.path().map(|p| p.display().to_string()))
}

/// Open an existing `.kerf` file, replacing the in-memory project. Both the GUI
/// and the embedded MCP server share this `Project`, so both now operate on —
/// and persist to — the opened file. Returns its path.
#[tauri::command]
fn open_project(state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let mut project = state.project()?;
    *project = Project::open(&path).map_err(|e| e.to_string())?;
    Ok(project.path().map(|p| p.display().to_string()))
}

/// Snapshot the current project to a new `.kerf` file and switch to it, so
/// subsequent edits (from the GUI and the agent alike) write through to disk.
/// Returns the saved path.
#[tauri::command]
fn save_project_as(state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let mut project = state.project()?;
    project.save_as(&path).map_err(|e| e.to_string())?;
    *project = Project::open(&path).map_err(|e| e.to_string())?;
    Ok(project.path().map(|p| p.display().to_string()))
}

// ---- import / analysis -----------------------------------------------------

#[tauri::command]
fn import_asset(state: State<'_, AppState>, path: String) -> CmdResult<Asset> {
    state.project()?.import_asset(path).map_err(|e| e.to_string())
}

#[tauri::command]
fn analyze_asset(state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetAnalysis> {
    let id = id(&asset_id)?;
    state.project()?.analyze_asset(id).map_err(|e| e.to_string())
}

// ---- timeline editing (each returns the refreshed timeline) ----------------

#[tauri::command]
fn cut_clip(state: State<'_, AppState>, asset_id: String, start: f64, end: f64) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project()?;
    project.cut_clip(id, start, end).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn add_clip(
    state: State<'_, AppState>,
    asset_id: String,
    track_id: Option<String>,
    source_in: f64,
    source_out: f64,
    timeline_start: Option<f64>,
) -> CmdResult<Timeline> {
    let asset = id(&asset_id)?;
    let track = track_id.as_deref().map(id).transpose()?;
    let project = state.project()?;
    project
        .add_clip_to_timeline(asset, track, source_in, source_out, timeline_start)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn split_clip(state: State<'_, AppState>, clip_id: String, at: f64) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.split_at(id, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn trim_clip(
    state: State<'_, AppState>,
    clip_id: String,
    source_in: Option<f64>,
    source_out: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.trim(id, source_in, source_out).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn reorder_clip(state: State<'_, AppState>, track_id: String, clip_id: String, new_index: usize) -> CmdResult<Timeline> {
    let track = id(&track_id)?;
    let clip = id(&clip_id)?;
    let project = state.project()?;
    project.reorder(track, clip, new_index).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_clip(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.remove(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_volume(state: State<'_, AppState>, clip_id: String, volume: f32) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.set_volume(id, volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_fade(
    state: State<'_, AppState>,
    clip_id: String,
    fade_in: Option<f64>,
    fade_out: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.set_fade(id, fade_in, fade_out).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_silence(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project()?;
    project.remove_silence(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn extract_audio(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project()?;
    project.extract_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn concatenate(state: State<'_, AppState>, asset_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = asset_ids.iter().map(|s| id(s)).collect::<CmdResult<Vec<_>>>()?;
    let project = state.project()?;
    project.concatenate(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

// ---- history (undo / redo / revert) ----------------------------------------

#[tauri::command]
fn get_history(state: State<'_, AppState>) -> CmdResult<Vec<Revision>> {
    state.project()?.history().map_err(|e| e.to_string())
}

#[tauri::command]
fn undo(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project()?.undo().map_err(|e| e.to_string())
}

#[tauri::command]
fn redo(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project()?.redo().map_err(|e| e.to_string())
}

#[tauri::command]
fn revert_to(state: State<'_, AppState>, seq: i64) -> CmdResult<Timeline> {
    state.project()?.revert_to(seq).map_err(|e| e.to_string())
}

// ---- media (preview frames, waveforms) -------------------------------------

#[tauri::command]
fn get_frame(state: State<'_, AppState>, asset_id: String, time_secs: f64, max_width: Option<u32>) -> CmdResult<String> {
    let id = id(&asset_id)?;
    let png = state
        .project()?
        .frame_at(id, time_secs, max_width.unwrap_or(960))
        .map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    Ok(format!("data:image/png;base64,{b64}"))
}

#[tauri::command]
fn get_waveform(state: State<'_, AppState>, asset_id: String, buckets: usize) -> CmdResult<Vec<f32>> {
    let id = id(&asset_id)?;
    state.project()?.waveform(id, buckets).map_err(|e| e.to_string())
}

// ---- agent task queue (mutations return the refreshed queue) ---------------

#[tauri::command]
fn list_tasks(state: State<'_, AppState>) -> CmdResult<Vec<Task>> {
    state.project()?.list_tasks().map_err(|e| e.to_string())
}

#[tauri::command]
fn add_task(state: State<'_, AppState>, prompt: String) -> CmdResult<Task> {
    state.project()?.add_task(&prompt).map_err(|e| e.to_string())
}

#[tauri::command]
fn resolve_task(state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id)?;
    let project = state.project()?;
    project.resolve_task(id).map_err(|e| e.to_string())?;
    project.list_tasks().map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_task(state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id)?;
    let project = state.project()?;
    project.remove_task(id).map_err(|e| e.to_string())?;
    project.list_tasks().map_err(|e| e.to_string())
}

// ---- export ----------------------------------------------------------------

#[tauri::command]
fn export_timeline(state: State<'_, AppState>, output_path: String, format: String) -> CmdResult<String> {
    let out = state.project()?.export(&output_path, &format).map_err(|e| e.to_string())?;
    Ok(out.to_string_lossy().into_owned())
}

/// Packaged builds ship `ffmpeg`/`ffprobe` next to the executable as Tauri
/// `externalBin` sidecars (see `tauri.conf.json`'s `bundle.externalBin`, injected
/// for Windows where there is no system FFmpeg). Point the CLI engine at them via
/// the `KERF_FFMPEG`/`KERF_FFPROBE` overrides it already honors. We only set a var
/// when the user hasn't (an explicit override wins) and the bundled binary is
/// actually present, so dev builds — which have no sidecar — transparently fall
/// back to a bare `ffmpeg`/`ffprobe` PATH lookup.
fn use_bundled_ffmpeg() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    for (var, name) in [("KERF_FFMPEG", "ffmpeg"), ("KERF_FFPROBE", "ffprobe")] {
        if std::env::var_os(var).is_some() {
            continue;
        }
        let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if path.is_file() {
            std::env::set_var(var, &path);
            tracing::info!(%var, path = %path.display(), "using bundled FFmpeg binary");
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    use_bundled_ffmpeg();

    // Start on a fresh, empty in-memory project; the user opens an existing
    // `.kerf` file or imports media to populate it.
    let project = Arc::new(Mutex::new(Project::open_in_memory().expect("failed to create empty project")));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            project: project.clone(),
        })
        .setup(move |app| {
            // The app *is* the MCP server: host the tools over HTTP, sharing the
            // same Project the GUI edits, so a connected LLM works on the open
            // project and its edits show up live.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = mcp::serve(project, handle).await {
                    tracing::error!(error = %e, "MCP server stopped");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_assets,
            get_timeline,
            get_asset_metadata,
            project_path,
            new_project,
            open_project,
            save_project_as,
            import_asset,
            analyze_asset,
            cut_clip,
            add_clip,
            split_clip,
            trim_clip,
            reorder_clip,
            remove_clip,
            set_volume,
            set_fade,
            remove_silence,
            extract_audio,
            concatenate,
            get_history,
            undo,
            redo,
            revert_to,
            get_frame,
            get_waveform,
            list_tasks,
            add_task,
            resolve_task,
            remove_task,
            export_timeline
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kerf");
}
