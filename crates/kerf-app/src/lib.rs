//! Tauri v2 shell for Kerf.
//!
//! Owns a single [`Project`] behind a mutex and exposes Tauri commands that
//! bridge the SvelteKit frontend to `kerf-core`. Read commands return domain
//! types; editing commands perform the mutation and return the refreshed
//! [`Timeline`] so the frontend can re-render in a single round-trip.

mod mcp;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use kerf_core::{
    Asset, AssetAnalysis, AudioEffect, EditSource, ExportOptions, Keyframe, Project, Revision, StreamKind, Task,
    TextKeyframe, Timeline, Transition, TransitionKind, VideoEffect,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

struct AppState {
    project: Arc<Mutex<Project>>,
    /// Set by `cancel_export` and polled by the in-flight export; lives outside
    /// the project lock so a cancel lands even while a render holds it.
    export_cancel: Arc<AtomicBool>,
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

fn kind(s: &str) -> CmdResult<StreamKind> {
    match s.to_lowercase().as_str() {
        "video" => Ok(StreamKind::Video),
        "audio" => Ok(StreamKind::Audio),
        other => Err(format!("invalid track kind '{other}'; expected \"video\" or \"audio\"")),
    }
}

/// Build a `Transition` from a kind string + duration, or `None` to clear it.
fn parse_transition(kind: Option<String>, duration: Option<f64>) -> CmdResult<Option<Transition>> {
    match kind {
        None => Ok(None),
        Some(k) => {
            let kind = TransitionKind::parse(&k)
                .ok_or_else(|| format!("invalid transition kind '{k}'; expected \"crossfade\" or \"dip_to_black\""))?;
            let duration = duration.ok_or("transition duration is required")?;
            Ok(Some(Transition { kind, duration }))
        }
    }
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
fn open_project(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let mut project = state.project()?;
    *project = Project::open(&path).map_err(|e| e.to_string())?;
    let result = project.path().map(|p| p.display().to_string());
    let assets = project.list_assets().unwrap_or_default();
    drop(project);
    // Make sure every video asset in the reopened project has a preview proxy
    // (a cached one is a cheap no-op; a missing one regenerates in the background).
    for asset in &assets {
        spawn_proxy(&app, asset);
    }
    Ok(result)
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
fn import_asset(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Asset> {
    let asset = state.project()?.import_asset(path).map_err(|e| e.to_string())?;
    // Kick off the preview proxy in the background; preview uses the original
    // until it lands (see `spawn_proxy`).
    spawn_proxy(&app, &asset);
    Ok(asset)
}

/// Queue an asset's preview proxy (all-intra, ~720p) for background generation so
/// scrubbing decodes one keyframe instead of seeking a long GOP. Non-blocking and
/// best-effort: previews fall back to the original source until the proxy lands,
/// at which point we emit `proxy-ready` so the webview re-fetches the current
/// frame. Stills and audio-only assets are skipped (they get no proxy).
fn spawn_proxy(app: &AppHandle, asset: &Asset) {
    let has_video = asset.streams.iter().any(|s| s.kind == StreamKind::Video);
    if !has_video || asset.is_image() {
        return;
    }
    if let Err(e) = proxy_jobs().send((app.clone(), asset.path.clone())) {
        tracing::warn!(error = %e, "preview proxy queue is closed");
    }
}

/// How many proxy encodes may run at once. Importing many large sources (or
/// reopening a project full of them) would otherwise spawn one full-file
/// re-encode per file *at once* — and each ffmpeg grabs every core — so the CPU
/// saturates and both the GUI and the agent freeze. The default of 1 keeps at
/// most one encode running; raise it with `KERF_PROXY_WORKERS` on a machine with
/// cores to spare (pair with `KERF_PROXY_THREADS` so workers × threads stays
/// under your core count, or you're back to oversubscribing).
fn proxy_workers() -> usize {
    std::env::var("KERF_PROXY_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or(1)
}

/// The bounded background worker pool that generates preview proxies. Every proxy
/// job is funnelled through `proxy_workers()` workers (each encode also
/// thread-capped in the engine), leaving the machine responsive while proxies
/// trickle in; previews use the original source until each one lands.
fn proxy_jobs() -> &'static std::sync::mpsc::Sender<(AppHandle, String)> {
    static QUEUE: std::sync::OnceLock<std::sync::mpsc::Sender<(AppHandle, String)>> =
        std::sync::OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<(AppHandle, String)>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..proxy_workers() {
            let rx = Arc::clone(&rx);
            std::thread::spawn(move || loop {
                // Hold the lock only to dequeue, then release it before the encode
                // so the other workers can pull the next job concurrently.
                let job = match rx.lock() {
                    Ok(guard) => guard.recv(),
                    Err(_) => break,
                };
                let Ok((app, path)) = job else { break };
                match kerf_core::generate_proxy(std::path::Path::new(&path)) {
                    Ok(_) => {
                        if let Err(e) = app.emit("proxy-ready", ()) {
                            tracing::warn!(error = %e, "failed to emit proxy-ready");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, path = %path, "preview proxy generation failed"),
                }
            });
        }
        tx
    })
}

#[tauri::command]
fn analyze_asset(state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetAnalysis> {
    let id = id(&asset_id)?;
    // Probe the asset under the lock, run the multi-second ffmpeg analysis with
    // the lock released, then re-acquire it only to cache the result — so the
    // GUI and the MCP agent stay responsive while analysis runs.
    let asset = state.project()?.require_asset(id).map_err(|e| e.to_string())?;
    let analysis = kerf_core::analyze_asset_media(&asset).map_err(|e| e.to_string())?;
    state.project()?.set_analysis(&analysis).map_err(|e| e.to_string())?;
    Ok(analysis)
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
fn move_clip(
    state: State<'_, AppState>,
    clip_id: String,
    timeline_start: f64,
    track_id: Option<String>,
) -> CmdResult<Timeline> {
    let clip = id(&clip_id)?;
    let track = track_id.as_deref().map(id).transpose()?;
    let project = state.project()?;
    project.move_clip(clip, timeline_start, track).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn ripple_delete(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.ripple_delete(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn add_track(state: State<'_, AppState>, kind: String, name: Option<String>) -> CmdResult<Timeline> {
    let kind = self::kind(&kind)?;
    let project = state.project()?;
    project.add_track(kind, name).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_track(state: State<'_, AppState>, track_id: String) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project()?;
    project.remove_track(id).map_err(|e| e.to_string())?;
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
fn set_speed(state: State<'_, AppState>, clip_id: String, speed: f64) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.set_speed(id, speed).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn set_transform(
    state: State<'_, AppState>,
    clip_id: String,
    scale: Option<f64>,
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    rotation: Option<f64>,
    opacity: Option<f64>,
    crop_left: Option<f64>,
    crop_right: Option<f64>,
    crop_top: Option<f64>,
    crop_bottom: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project
        .set_transform(id, scale, pos_x, pos_y, rotation, opacity, crop_left, crop_right, crop_top, crop_bottom)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_color(
    state: State<'_, AppState>,
    clip_id: String,
    brightness: Option<f64>,
    contrast: Option<f64>,
    saturation: Option<f64>,
    gamma: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.set_color(id, brightness, contrast, saturation, gamma).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_transition(
    state: State<'_, AppState>,
    clip_id: String,
    kind: Option<String>,
    duration: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let transition = parse_transition(kind, duration)?;
    let project = state.project()?;
    project.set_transition(id, transition).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_video_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<VideoEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.set_video_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_audio_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<AudioEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.set_audio_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_keyframes(state: State<'_, AppState>, clip_id: String, keyframes: Vec<Keyframe>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.set_keyframes(id, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn add_keyframe(
    state: State<'_, AppState>,
    clip_id: String,
    time: f64,
    scale: Option<f64>,
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    rotation: Option<f64>,
    opacity: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.add_keyframe(id, time, scale, pos_x, pos_y, rotation, opacity).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_keyframes(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project()?;
    project.clear_keyframes(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn add_overlay(state: State<'_, AppState>, text: String, start: f64, end: f64) -> CmdResult<Timeline> {
    let project = state.project()?;
    project.add_overlay(text, start, end).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn update_overlay(
    state: State<'_, AppState>,
    overlay_id: String,
    text: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    size: Option<f64>,
    color: Option<String>,
    bg: Option<String>,
    bold: Option<bool>,
) -> CmdResult<Timeline> {
    let oid = id(&overlay_id)?;
    let project = state.project()?;
    project
        .update_overlay(oid, text, start, end, pos_x, pos_y, size, color, bg, bold)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_overlay(state: State<'_, AppState>, overlay_id: String) -> CmdResult<Timeline> {
    let oid = id(&overlay_id)?;
    let project = state.project()?;
    project.remove_overlay(oid).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_overlay_keyframes(state: State<'_, AppState>, overlay_id: String, keyframes: Vec<TextKeyframe>) -> CmdResult<Timeline> {
    let oid = id(&overlay_id)?;
    let project = state.project()?;
    project.set_overlay_keyframes(oid, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn captions_from_transcript(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project()?;
    project.captions_from_transcript(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
fn export_srt(state: State<'_, AppState>, asset_id: String, output_path: String) -> CmdResult<String> {
    let id = id(&asset_id)?;
    let srt = {
        let project = state.project()?;
        project.transcript_srt(id).map_err(|e| e.to_string())?
    };
    std::fs::write(&output_path, srt).map_err(|e| e.to_string())?;
    Ok(output_path)
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
    accurate: Option<bool>,
) -> CmdResult<String> {
    let id = id(&asset_id)?;
    // Resolve the asset under the lock, then *drop the guard* before decoding: the
    // ffmpeg run must not hold the shared Project mutex for its whole duration, or
    // it freezes every other op (timeline edits, MCP, the next scrub frame).
    let asset = state.project()?.require_asset(id).map_err(|e| e.to_string())?;
    // JPEG rather than PNG: the preview pane never needs lossless frames, and a
    // q=4 JPEG is ~5–10× smaller to encode and ship over IPC — which matters now
    // that the preview fetches frames continuously during playback. `accurate`
    // is false for rough scrub frames (keyframe-snap), true for the settled frame.
    let jpeg = Project::decode_preview_frame(&asset, time_secs, max_width.unwrap_or(960), 4, accurate.unwrap_or(true))
        .map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
    Ok(format!("data:image/jpeg;base64,{b64}"))
}

/// The composited timeline still at `time_secs` — every visible clip put through
/// the same color / effect / transform / overlay chain the export uses, so the
/// preview reflects Inspector edits live (unlike `get_frame`, a raw source decode).
#[tauri::command]
fn get_timeline_frame(state: State<'_, AppState>, time_secs: f64, max_width: Option<u32>) -> CmdResult<String> {
    // Resolve the inputs under the lock, then *drop the guard* before the ffmpeg
    // composite — the preview fetches frames continuously during playback, and
    // holding the shared Project mutex for the whole decode would freeze every
    // other op (timeline edits, MCP, the next scrub frame). Mirrors `get_frame`.
    let (timeline, assets) = state.project()?.timeline_frame_inputs().map_err(|e| e.to_string())?;
    let jpeg = Project::composite_timeline_frame(&timeline, &assets, time_secs, max_width.unwrap_or(960), 4)
        .map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
    Ok(format!("data:image/jpeg;base64,{b64}"))
}

#[tauri::command]
fn get_waveform(state: State<'_, AppState>, asset_id: String, buckets: usize) -> CmdResult<Vec<f32>> {
    let id = id(&asset_id)?;
    state.project()?.waveform(id, buckets).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_energy(state: State<'_, AppState>, asset_id: String, buckets: usize) -> CmdResult<Vec<f32>> {
    let id = id(&asset_id)?;
    state.project()?.energy(id, buckets).map_err(|e| e.to_string())
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
fn export_timeline(app: AppHandle, state: State<'_, AppState>, output_path: String, options: ExportOptions) -> CmdResult<String> {
    // Snapshot the timeline + assets under the lock, then release it before the
    // (seconds-to-minutes) ffmpeg render. Otherwise the export would hold the
    // shared Project mutex for its whole duration and freeze every other GUI
    // command and the MCP agent until it finished.
    let (timeline, assets) = {
        let project = state.project()?;
        (
            project.timeline().map_err(|e| e.to_string())?,
            project.list_assets().map_err(|e| e.to_string())?,
        )
    };

    // Fresh cancel flag for this run; `cancel_export` flips it from another thread.
    let cancel = state.export_cancel.clone();
    cancel.store(false, Ordering::SeqCst);

    // Stream `export-progress` events ({ fraction, elapsed_secs, eta_secs }) so
    // the UI can show a bar + ETA. ffmpeg emits ~2/sec, no extra throttle needed.
    let mut on_progress = |p: kerf_core::ExportProgress| {
        let _ = app.emit("export-progress", p);
    };
    let status = kerf_core::render_with_progress(
        &timeline,
        &assets,
        std::path::Path::new(&output_path),
        &options,
        &mut on_progress,
        &|| cancel.load(Ordering::SeqCst),
    )
    .map_err(|e| e.to_string())?;

    match status {
        kerf_core::RenderStatus::Completed => Ok(output_path),
        kerf_core::RenderStatus::Cancelled => {
            // Drop the half-written file so a cancelled export leaves no debris.
            let _ = std::fs::remove_file(&output_path);
            Err("export cancelled".to_string())
        }
    }
}

/// Request cancellation of the in-flight export (if any). The running
/// [`export_timeline`] observes the flag on its next progress poll, stops
/// ffmpeg, and returns the `"export cancelled"` error.
#[tauri::command]
fn cancel_export(state: State<'_, AppState>) {
    state.export_cancel.store(true, Ordering::SeqCst);
}

// ---- agent connection (MCP endpoint) ---------------------------------------

/// The local MCP endpoint URL a connected LLM points at (e.g.
/// `http://127.0.0.1:7777/mcp`), honoring the `KERF_MCP_ADDR` override. The
/// agent panel surfaces this so the user knows how to connect their agent.
#[tauri::command]
fn mcp_endpoint() -> String {
    mcp::endpoint_url()
}

// ---- diagnostics (logs) ----------------------------------------------------

#[tauri::command]
fn log_dir(app: AppHandle) -> CmdResult<String> {
    app.path()
        .app_log_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

/// Open the log directory in the OS file manager so users can attach the file.
#[tauri::command]
fn reveal_logs(app: AppHandle) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| e.to_string())
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

/// Install the global tracing subscriber: always to stdout, and — when the
/// platform log directory is writable — to a daily-rolling `kerf.<date>.log`
/// there (the last 14 days are kept) so users hitting an issue can attach it.
/// Level is `info` by default; override with `RUST_LOG` (e.g. `RUST_LOG=debug`).
fn init_logging(app: &AppHandle) {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let stdout = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

    let file = app.path().app_log_dir().ok().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("kerf")
            .filename_suffix("log")
            .max_log_files(14)
            .build(&dir)
            .ok()?;
        let (writer, guard) = tracing_appender::non_blocking(appender);
        // Keep the flush worker alive for the whole process; we never tear it down.
        Box::leak(Box::new(guard));
        Some((tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer), dir))
    });

    match file {
        Some((layer, dir)) => {
            tracing_subscriber::registry().with(filter).with(stdout).with(layer).init();
            tracing::info!(dir = %dir.display(), "logging to file");
        }
        None => {
            tracing_subscriber::registry().with(filter).with(stdout).init();
            tracing::warn!("file logging unavailable; logging to stdout only");
        }
    }
}

/// Route panics through tracing so they land in the logfile, then run the
/// default hook (which still prints the backtrace to stderr).
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        tracing::error!(location = %location, "panic: {message}");
        default(info);
    }));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Start on a fresh, empty in-memory project; the user opens an existing
    // `.kerf` file or imports media to populate it.
    let project = Arc::new(Mutex::new(Project::open_in_memory().expect("failed to create empty project")));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            project: project.clone(),
            export_cancel: Arc::new(AtomicBool::new(false)),
        })
        .setup(move |app| {
            // Logging needs the resolved platform log directory, so set it up here
            // (before anything else in setup) rather than at the top of `run`.
            init_logging(app.handle());
            install_panic_hook();
            use_bundled_ffmpeg();
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                os = std::env::consts::OS,
                arch = std::env::consts::ARCH,
                "kerf starting"
            );

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
            move_clip,
            ripple_delete,
            add_track,
            remove_track,
            remove_clip,
            set_volume,
            set_fade,
            set_speed,
            set_transform,
            set_color,
            set_transition,
            set_video_effects,
            set_audio_effects,
            set_keyframes,
            add_keyframe,
            clear_keyframes,
            add_overlay,
            update_overlay,
            remove_overlay,
            set_overlay_keyframes,
            captions_from_transcript,
            export_srt,
            remove_silence,
            extract_audio,
            concatenate,
            get_history,
            undo,
            redo,
            revert_to,
            get_frame,
            get_timeline_frame,
            get_waveform,
            get_energy,
            list_tasks,
            add_task,
            resolve_task,
            remove_task,
            export_timeline,
            cancel_export,
            mcp_endpoint,
            log_dir,
            reveal_logs
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kerf");
}
