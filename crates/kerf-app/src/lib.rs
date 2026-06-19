//! Tauri v2 shell for Kerf.
//!
//! Owns a single [`Project`] behind a mutex and exposes Tauri commands that
//! bridge the SvelteKit frontend to `kerf-core`.

use std::sync::Mutex;

use kerf_core::{Asset, AssetAnalysis, Project, Timeline};
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

#[tauri::command]
fn list_assets(state: State<'_, AppState>) -> Result<Vec<Asset>, String> {
    state
        .project
        .lock()
        .map_err(|_| "project mutex poisoned".to_string())?
        .list_assets()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_timeline(state: State<'_, AppState>) -> Result<Timeline, String> {
    state
        .project
        .lock()
        .map_err(|_| "project mutex poisoned".to_string())?
        .timeline()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_asset_metadata(state: State<'_, AppState>, asset_id: String) -> Result<AssetMetadata, String> {
    let id = Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    let project = state
        .project
        .lock()
        .map_err(|_| "project mutex poisoned".to_string())?;
    let asset = project.require_asset(id).map_err(|e| e.to_string())?;
    let analysis = project.get_analysis(id).map_err(|e| e.to_string())?;
    Ok(AssetMetadata { asset, analysis })
}

#[tauri::command]
fn import_asset(state: State<'_, AppState>, path: String) -> Result<Asset, String> {
    state
        .project
        .lock()
        .map_err(|_| "project mutex poisoned".to_string())?
        .import_asset(path)
        .map_err(|e| e.to_string())
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
            import_asset
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kerf");
}
