//! Tauri v2 shell for Kerf.
//!
//! Owns a single [`Project`] behind a mutex and exposes Tauri commands that
//! bridge the SvelteKit frontend to `kerf-core`. Read commands return domain
//! types; editing commands perform the mutation and return the refreshed
//! [`Timeline`] so the frontend can re-render in a single round-trip.

use std::sync::Mutex;

use base64::Engine as _;
use kerf_core::{Asset, AssetAnalysis, Project, Revision, Task, Timeline};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

struct AppState {
    project: Mutex<Project>,
}

#[derive(Serialize)]
struct AssetMetadata {
    asset: Asset,
    analysis: Option<AssetAnalysis>,
}

type CmdResult<T> = Result<T, String>;

impl AppState {
    fn project(&self) -> CmdResult<std::sync::MutexGuard<'_, Project>> {
        self.project.lock().map_err(|_| "project mutex poisoned".to_string())
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
fn reorder_clip(
    state: State<'_, AppState>,
    track_id: String,
    clip_id: String,
    new_index: usize,
) -> CmdResult<Timeline> {
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
fn get_frame(
    state: State<'_, AppState>,
    asset_id: String,
    time_secs: f64,
    max_width: Option<u32>,
) -> CmdResult<String> {
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
    let out = state
        .project()?
        .export(&output_path, &format)
        .map_err(|e| e.to_string())?;
    Ok(out.to_string_lossy().into_owned())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Start on a seeded in-memory sample so the UI has content immediately.
    let project = Project::sample().expect("failed to seed sample project");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            project: Mutex::new(project),
        })
        .invoke_handler(tauri::generate_handler![
            list_assets,
            get_timeline,
            get_asset_metadata,
            import_asset,
            analyze_asset,
            cut_clip,
            add_clip,
            split_clip,
            trim_clip,
            reorder_clip,
            remove_clip,
            set_volume,
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
