//! Tauri v2 shell for Kerf.
//!
//! Owns a single [`Project`] behind a mutex and exposes Tauri commands that
//! bridge the SvelteKit frontend to `kerf-core`. Read commands return domain
//! types; editing commands perform the mutation and return the refreshed
//! [`Timeline`] so the frontend can re-render in a single round-trip.
//!
//! **No command runs on the main thread.** A plain `#[tauri::command]` executes
//! on the main thread in Tauri v2 and would freeze the window for its duration,
//! so every quick command here is `#[tauri::command(async)]` (runs on the async
//! runtime) and every heavy one (ffmpeg decode / analysis / export, disk-bound
//! project open/save) is an `async fn` that pushes its work onto the blocking
//! thread pool via [`blocking`], resolving inputs under the shared project lock
//! and releasing it before the slow part.

mod mcp;
mod settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use kerf_core::{
    Asset, AssetAnalysis, AudioEffect, CaptionOptions, Delivery, EditSource, ExportOptions, Fit, Keyframe, Mask, Project,
    Projection, ReframeKeyframe, Revision, StagedEdit, StreamKind, Task, TextKeyframe, Timeline, TimelineDiff, Transition,
    TransitionKind, VideoEffect,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

struct AppState {
    project: Arc<Mutex<Project>>,
    /// Set by `cancel_export` and polled by the in-flight export; lives outside
    /// the project lock so a cancel lands even while a render holds it.
    export_cancel: Arc<AtomicBool>,
    /// Same, for the in-flight analysis pass. Analysis downloads a speech model
    /// and then transcribes for minutes, so it has to be abandonable — and the
    /// GUI runs imported assets through it one after another, which is a long
    /// commitment to make on the user's behalf without an exit.
    analysis_cancel: Arc<AtomicBool>,
}

#[derive(Serialize)]
struct AssetMetadata {
    asset: Asset,
    analysis: Option<AssetAnalysis>,
}

type CmdResult<T> = Result<T, String>;

impl AppState {
    fn project(&self) -> std::sync::MutexGuard<'_, Project> {
        lock_user(&self.project)
    }
}

/// Lock the shared project for a GUI command, attributing edits to the user;
/// the MCP server attributes its own edits to the agent under the same lock
/// (see `mcp::KerfMcp::lock`). Recovers from a poisoned mutex (a panic while
/// another op held it) rather than failing every command for the rest of the
/// session.
fn lock_user(project: &Mutex<Project>) -> std::sync::MutexGuard<'_, Project> {
    let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
    guard.set_actor(EditSource::User);
    guard
}

/// Run a blocking (ffmpeg / disk) job on the blocking thread pool and await it.
/// Commands doing heavy work must go through this: a plain command body runs on
/// the main thread (freezing the window) and an `async` one on the shared tokio
/// workers (starving the MCP server), while the blocking pool grows on demand.
async fn blocking<T: Send + 'static>(job: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(job).await.map_err(|e| e.to_string())?
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
            let kind = TransitionKind::parse(&k).ok_or_else(|| {
                format!(
                    "invalid transition kind '{k}'; expected one of {}",
                    TransitionKind::wire_names()
                )
            })?;
            let duration = duration.ok_or("transition duration is required")?;
            Ok(Some(Transition { kind, duration }))
        }
    }
}

// ---- read ------------------------------------------------------------------

#[tauri::command(async)]
fn list_assets(state: State<'_, AppState>) -> CmdResult<Vec<Asset>> {
    state.project().list_assets().map_err(|e| e.to_string())
}

/// Distinct family names of every font installed on this machine, for the
/// text overlay font picker.
#[tauri::command(async)]
fn list_fonts() -> CmdResult<Vec<String>> {
    Ok(kerf_core::list_system_fonts())
}

#[tauri::command(async)]
fn get_timeline(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn get_asset_metadata(state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetMetadata> {
    let id = id(&asset_id)?;
    let project = state.project();
    let asset = project.require_asset(id).map_err(|e| e.to_string())?;
    let analysis = project.get_analysis(id).map_err(|e| e.to_string())?;
    Ok(AssetMetadata { asset, analysis })
}

// ---- project file (open / save) --------------------------------------------

/// Path of the `.kerf` file backing the open project, or `null` if it lives
/// only in memory (the seeded sample) and isn't persisted yet.
#[tauri::command(async)]
fn project_path(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(state.project().path().map(|p| p.display().to_string()))
}

/// Replace the open project with a fresh, empty in-memory one (no sample data).
/// Like the seeded sample it isn't persisted until `save_project_as`. The GUI and
/// the embedded MCP server share this `Project`, so both switch to it.
#[tauri::command(async)]
fn new_project(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let mut project = state.project();
    *project = Project::open_in_memory().map_err(|e| e.to_string())?;
    Ok(project.path().map(|p| p.display().to_string()))
}

/// Open an existing `.kerf` file, replacing the in-memory project. Both the GUI
/// and the embedded MCP server share this `Project`, so both now operate on —
/// and persist to — the opened file. Returns its path.
#[tauri::command]
async fn open_project(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let shared = state.project.clone();
    blocking(move || {
        // Open the file first, then swap it in — the (disk-bound) open doesn't
        // hold the shared lock, and a failed open leaves the current project intact.
        let opened = Project::open(&path).map_err(|e| e.to_string())?;
        let mut project = lock_user(&shared);
        *project = opened;
        let result = project.path().map(|p| p.display().to_string());
        let assets = project.list_assets().unwrap_or_default();
        drop(project);
        // The speech model is remembered per project, so transcribing in the
        // reopened one uses the model it was cut with.
        restore_speech_model(&shared);
        // Make sure every video asset in the reopened project has a preview proxy
        // (a cached one is a cheap no-op; a missing one regenerates in the background).
        for asset in &assets {
            spawn_proxy(&app, asset);
        }
        Ok(result)
    })
    .await
}

/// Snapshot the current project to a new `.kerf` file and switch to it, so
/// subsequent edits (from the GUI and the agent alike) write through to disk.
/// Returns the saved path.
#[tauri::command]
async fn save_project_as(state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let shared = state.project.clone();
    blocking(move || {
        let mut project = lock_user(&shared);
        project.save_as(&path).map_err(|e| e.to_string())?;
        *project = Project::open(&path).map_err(|e| e.to_string())?;
        Ok(project.path().map(|p| p.display().to_string()))
    })
    .await
}

// ---- import / analysis -----------------------------------------------------

/// Progress of a slow import (an Insta360 lens pair being stitched), tagged with
/// the file the user picked so the UI can label it while several import at once.
#[derive(Clone, serde::Serialize)]
pub(crate) struct ImportProgress {
    path: String,
    fraction: f64,
    elapsed_secs: f64,
    eta_secs: Option<f64>,
}

impl ImportProgress {
    /// Tag a render-progress tick with the file it belongs to. Shared with the
    /// MCP `import_asset` tool so an agent's import reports on the same event
    /// and drives the same overlay the user's own import does.
    pub(crate) fn new(path: &str, p: kerf_core::ExportProgress) -> Self {
        Self {
            path: path.to_string(),
            fraction: p.fraction,
            elapsed_secs: p.elapsed_secs,
            eta_secs: p.eta_secs,
        }
    }
}

#[tauri::command]
async fn import_asset(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Asset> {
    let shared = state.project.clone();
    blocking(move || {
        // Probe (and, for an Insta360 lens pair, stitch) without the lock — so
        // parallel imports really run in parallel and a multi-minute stitch never
        // freezes the GUI or the agent — then take it only for the quick insert.
        let mut on_progress = |p: kerf_core::ExportProgress| {
            let _ = app.emit("import-progress", ImportProgress::new(&path, p));
        };
        let asset = Project::probe_import(std::path::Path::new(&path), &mut on_progress).map_err(|e| e.to_string())?;
        // Importing the pair's other lens (or the same file twice) resolves to
        // the asset already in the project instead of duplicating it.
        let asset = lock_user(&shared).insert_or_get_asset(&asset).map_err(|e| e.to_string())?;
        // Kick off the preview proxy in the background; preview uses the original
        // until it lands (see `spawn_proxy`).
        spawn_proxy(&app, &asset);
        Ok(asset)
    })
    .await
}

/// Queue an asset's preview proxy (all-intra, downscaled) for background
/// generation so scrubbing decodes one keyframe instead of seeking a long GOP. Non-blocking and
/// best-effort: previews fall back to the original source until the proxy lands,
/// at which point we emit `proxy-ready` so the webview re-fetches the current
/// frame. Stills and audio-only assets are skipped (they get no proxy).
pub(crate) fn spawn_proxy(app: &AppHandle, asset: &Asset) {
    let has_video = asset.streams.iter().any(|s| s.kind == StreamKind::Video);
    if !has_video || asset.is_image() {
        return;
    }
    // 360 assets proxy larger — reframing crops most of the frame away.
    let width = kerf_core::proxy_width(asset.projection());
    if let Err(e) = proxy_jobs().send((app.clone(), asset.path.clone(), width)) {
        tracing::warn!(error = %e, "preview proxy queue is closed");
    }
}

/// How many proxy encodes may run at once. Importing many large sources (or
/// reopening a project full of them) would otherwise spawn one full-file
/// re-encode per file *at once*. The engine's CPU budget now gates every heavy
/// job anyway (`kerf_core::engine::cpu`), so raising `KERF_PROXY_WORKERS` above
/// the default of 1 buys queued encodes rather than concurrent ones — the knob
/// that decides how much of the machine they get is the CPU limit in Settings.
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
fn proxy_jobs() -> &'static std::sync::mpsc::Sender<(AppHandle, String, u32)> {
    static QUEUE: std::sync::OnceLock<std::sync::mpsc::Sender<(AppHandle, String, u32)>> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<(AppHandle, String, u32)>();
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
                let Ok((app, path, width)) = job else { break };
                match kerf_core::generate_proxy(std::path::Path::new(&path), width) {
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

/// One step of an analysis pass, tagged with the asset it belongs to so the bin
/// can badge the right row when several assets are analyzed at once.
#[derive(Serialize, Clone)]
struct AnalysisProgressEvent {
    asset_id: String,
    stage: String,
    fraction: Option<f64>,
    detail: Option<String>,
}

#[tauri::command]
async fn analyze_asset(app: AppHandle, state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetAnalysis> {
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    // Fresh cancel flag for this pass; `cancel_analysis` flips it from the UI.
    let cancel = state.analysis_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    blocking(move || {
        // Resolve the asset under the lock, run the multi-second ffmpeg analysis
        // with the lock released, then re-acquire it only to cache the result —
        // so the GUI and the MCP agent stay responsive while analysis runs.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        // Analysis is no longer a short opaque wait: the first transcription
        // downloads a speech model and then runs inference for minutes, so each
        // step is streamed to the webview rather than hidden behind a spinner.
        let mut on_progress = |p: kerf_core::AnalysisProgress| {
            let _ = app.emit(
                "analysis-progress",
                AnalysisProgressEvent {
                    asset_id: asset_id.clone(),
                    stage: p.stage,
                    fraction: p.fraction,
                    detail: p.detail,
                },
            );
        };
        let analysis = kerf_core::analyze_asset_media_cancellable(&asset, &mut on_progress, &|| cancel.load(Ordering::SeqCst))
            .map_err(|e| match e {
                // A cancel is the user's own doing, not a failure — the
                // caller keys off this string to stay quiet about it.
                kerf_core::Error::Cancelled => ANALYSIS_CANCELLED.to_string(),
                other => other.to_string(),
            })?;
        // Nothing is cached for a cancelled pass: a half-analyzed asset would
        // read as analyzed, and the missing transcript as "no speech".
        lock_user(&shared).set_analysis(&analysis).map_err(|e| e.to_string())?;
        Ok(analysis)
    })
    .await
}

// ---- speech-to-text -------------------------------------------------------

/// The project-meta key holding the user's speech-model choice.
pub(crate) const SPEECH_MODEL_KEY: &str = "speech_model";

/// Which transcription backend this build will use, and whether its model is
/// already downloaded. The transcript tab reads this to explain an empty
/// transcript instead of just showing nothing.
#[tauri::command(async)]
fn transcription_status() -> CmdResult<kerf_core::TranscriptionStatus> {
    Ok(kerf_core::transcription_status())
}

/// Pick which speech model transcription uses, remembering it in the project.
///
/// `None` clears the choice back to the environment / built-in default. The
/// model is not downloaded here — that happens on the next transcription, or
/// via `download_speech_model`.
#[tauri::command(async)]
fn set_speech_model(state: State<'_, AppState>, name: Option<String>) -> CmdResult<kerf_core::TranscriptionStatus> {
    kerf_core::set_speech_model(name.as_deref());
    state
        .project()
        .set_meta(SPEECH_MODEL_KEY, name.as_deref().unwrap_or(""))
        .map_err(|e| e.to_string())?;
    Ok(kerf_core::transcription_status())
}

/// Apply the speech-model choice stored in `project` (a no-op when unset), so a
/// reopened project transcribes with the model the user picked for it.
fn restore_speech_model(project: &Mutex<Project>) {
    let stored = lock_user(project).meta(SPEECH_MODEL_KEY).ok().flatten();
    kerf_core::set_speech_model(stored.as_deref().filter(|s| !s.is_empty()));
}

/// A speech model download in flight.
#[derive(Serialize, Clone)]
struct ModelProgressEvent {
    model: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    fraction: Option<f64>,
}

/// Download a speech model ahead of time, streaming `model-progress`.
///
/// Transcription downloads on demand anyway; this exists so the user can start
/// the (few hundred megabyte) fetch deliberately, and pick a model other than
/// the default, instead of discovering it mid-analysis.
#[tauri::command]
async fn download_speech_model(app: AppHandle, name: String) -> CmdResult<String> {
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
        let path = kerf_core::download_speech_model(&name, &mut on_progress).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

// ---- timeline editing (each returns the refreshed timeline) ----------------

#[tauri::command(async)]
fn cut_clip(state: State<'_, AppState>, asset_id: String, start: f64, end: f64) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project();
    project.cut_clip(id, start, end).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
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
    let project = state.project();
    project
        .add_clip_to_timeline(asset, track, source_in, source_out, timeline_start)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn split_clip(state: State<'_, AppState>, clip_id: String, at: f64) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.split_at(id, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn trim_clip(
    state: State<'_, AppState>,
    clip_id: String,
    source_in: Option<f64>,
    source_out: Option<f64>,
    timeline_start: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .trim(id, source_in, source_out, timeline_start)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn reorder_clip(state: State<'_, AppState>, track_id: String, clip_id: String, new_index: usize) -> CmdResult<Timeline> {
    let track = id(&track_id)?;
    let clip = id(&clip_id)?;
    let project = state.project();
    project.reorder(track, clip, new_index).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn move_clip(state: State<'_, AppState>, clip_id: String, timeline_start: f64, track_id: Option<String>) -> CmdResult<Timeline> {
    let clip = id(&clip_id)?;
    let track = track_id.as_deref().map(id).transpose()?;
    let project = state.project();
    project.move_clip(clip, timeline_start, track).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn ripple_delete(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.ripple_delete(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn cut_clip_range(state: State<'_, AppState>, clip_id: String, from: f64, to: f64) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.cut_clip_range(id, from, to).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn add_track(state: State<'_, AppState>, kind: String, name: Option<String>) -> CmdResult<Timeline> {
    let kind = self::kind(&kind)?;
    let project = state.project();
    project.add_track(kind, name).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn remove_track(state: State<'_, AppState>, track_id: String) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project();
    project.remove_track(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_track_duck(state: State<'_, AppState>, track_id: String, duck: bool) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project();
    project.set_track_duck(id, duck).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set a track's fader — the gain riding every clip on the track.
#[tauri::command(async)]
fn set_track_volume(state: State<'_, AppState>, track_id: String, volume: f32) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project();
    project.set_track_volume(id, volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set a track's stereo placement, -1 (hard left) to 1 (hard right).
#[tauri::command(async)]
fn set_track_pan(state: State<'_, AppState>, track_id: String, pan: f32) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project();
    project.set_track_pan(id, pan).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set the frame the project is cut for, or clear it back to the source shape.
/// The preview, the scrubbed still and the export all read it, so the vertical
/// crop is visible while cutting instead of only in the rendered file.
#[tauri::command(async)]
fn set_delivery_format(
    state: State<'_, AppState>,
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
) -> CmdResult<Timeline> {
    let format = match (width, height) {
        (Some(w), Some(h)) => Some(Delivery::new(w, h, fit.unwrap_or(Fit::Cover))),
        _ => None,
    };
    state.project().set_delivery_format(format).map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_track_muted(state: State<'_, AppState>, track_id: String, muted: bool) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project();
    project.set_track_muted(id, muted).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_track_solo(state: State<'_, AppState>, track_id: String, solo: bool) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project();
    project.set_track_solo(id, solo).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_track_locked(state: State<'_, AppState>, track_id: String, locked: bool) -> CmdResult<Timeline> {
    let id = id(&track_id)?;
    let project = state.project();
    project.set_track_locked(id, locked).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_clip_enabled(state: State<'_, AppState>, clip_id: String, enabled: bool) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_clip_enabled(id, enabled).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// One clipboard entry: the clip's data plus the track it should land on.
#[derive(serde::Deserialize)]
struct Placement {
    track_id: String,
    clip: kerf_core::Clip,
}

/// Paste clipboard clips. Takes clip *values*, not ids, so a cut-then-paste
/// works after the sources are gone.
#[tauri::command(async)]
fn insert_clips(state: State<'_, AppState>, placements: Vec<Placement>, at: f64) -> CmdResult<Timeline> {
    let items = placements
        .into_iter()
        .map(|p| id(&p.track_id).map(|t| (t, p.clip)))
        .collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project.insert_clips(&items, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn duplicate_clips(state: State<'_, AppState>, clip_ids: Vec<String>, at: f64) -> CmdResult<Timeline> {
    let ids = clip_ids.iter().map(|s| id(s)).collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project.duplicate_clips(&ids, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn remove_clip(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.remove(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_volume(state: State<'_, AppState>, clip_id: String, volume: f32) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_volume(id, volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_fade(state: State<'_, AppState>, clip_id: String, fade_in: Option<f64>, fade_out: Option<f64>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_fade(id, fade_in, fade_out).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_speed(state: State<'_, AppState>, clip_id: String, speed: f64) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_speed(id, speed).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
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
    let project = state.project();
    project
        .set_transform(
            id,
            scale,
            pos_x,
            pos_y,
            rotation,
            opacity,
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
        )
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_color(
    state: State<'_, AppState>,
    clip_id: String,
    brightness: Option<f64>,
    contrast: Option<f64>,
    saturation: Option<f64>,
    gamma: Option<f64>,
    temperature: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .set_color(id, brightness, contrast, saturation, gamma, temperature)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_transition(
    state: State<'_, AppState>,
    clip_id: String,
    kind: Option<String>,
    duration: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let transition = parse_transition(kind, duration)?;
    let project = state.project();
    project.set_transition(id, transition).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Cut a clip to a shape (or clear it with `mask: null`), so a lower track shows
/// through outside it.
#[tauri::command(async)]
fn set_mask(state: State<'_, AppState>, clip_id: String, mask: Option<Mask>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_mask(id, mask).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_video_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<VideoEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_video_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_audio_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<AudioEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_audio_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_keyframes(state: State<'_, AppState>, clip_id: String, keyframes: Vec<Keyframe>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_keyframes(id, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
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
    let project = state.project();
    project
        .add_keyframe(id, time, scale, pos_x, pos_y, rotation, opacity)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn clear_keyframes(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.clear_keyframes(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn set_reframe(
    state: State<'_, AppState>,
    clip_id: String,
    yaw: Option<f64>,
    pitch: Option<f64>,
    roll: Option<f64>,
    fov: Option<f64>,
    lens_fov: Option<f64>,
    input: Option<Projection>,
    output: Option<Projection>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .set_reframe(id, yaw, pitch, roll, fov, lens_fov, input, output)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Mark an asset as 360 footage (or clear the mark) for footage the probe could
/// not identify. Unlike `set_reframe` this sticks to the asset, so every clip cut
/// from it afterwards is reframed.
#[tauri::command(async)]
fn set_asset_projection(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    projection: Option<Projection>,
) -> CmdResult<Asset> {
    let id = id(&asset_id)?;
    let asset = state
        .project()
        .set_asset_projection(id, projection)
        .map_err(|e| e.to_string())?;
    // 360 assets proxy at a different size, so the cached proxy no longer matches.
    spawn_proxy(&app, &asset);
    Ok(asset)
}

#[tauri::command(async)]
fn clear_reframe(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.clear_reframe(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_reframe_keyframes(state: State<'_, AppState>, clip_id: String, keyframes: Vec<ReframeKeyframe>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_reframe_keyframes(id, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn add_reframe_keyframe(
    state: State<'_, AppState>,
    clip_id: String,
    time: f64,
    yaw: Option<f64>,
    pitch: Option<f64>,
    roll: Option<f64>,
    fov: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .add_reframe_keyframe(id, time, yaw, pitch, roll, fov)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn add_marker(state: State<'_, AppState>, time: f64, name: String, color: Option<String>) -> CmdResult<Timeline> {
    let project = state.project();
    project.add_marker(time, name, color).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn update_marker(
    state: State<'_, AppState>,
    marker_id: String,
    time: Option<f64>,
    name: Option<String>,
    color: Option<String>,
) -> CmdResult<Timeline> {
    let id = id(&marker_id)?;
    let project = state.project();
    project.update_marker(id, time, name, color).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn remove_marker(state: State<'_, AppState>, marker_id: String) -> CmdResult<Timeline> {
    let id = id(&marker_id)?;
    let project = state.project();
    project.remove_marker(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn add_overlay(state: State<'_, AppState>, text: String, start: f64, end: f64) -> CmdResult<Timeline> {
    let project = state.project();
    project.add_overlay(text, start, end).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
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
    font: Option<String>,
    bold: Option<bool>,
) -> CmdResult<Timeline> {
    let oid = id(&overlay_id)?;
    let project = state.project();
    project
        .update_overlay(oid, text, start, end, pos_x, pos_y, size, color, bg, font, bold)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn remove_overlay(state: State<'_, AppState>, overlay_id: String) -> CmdResult<Timeline> {
    let oid = id(&overlay_id)?;
    let project = state.project();
    project.remove_overlay(oid).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_overlay_keyframes(state: State<'_, AppState>, overlay_id: String, keyframes: Vec<TextKeyframe>) -> CmdResult<Timeline> {
    let oid = id(&overlay_id)?;
    let project = state.project();
    project.set_overlay_keyframes(oid, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn generate_captions(state: State<'_, AppState>, options: Option<CaptionOptions>) -> CmdResult<Timeline> {
    let project = state.project();
    project
        .generate_captions(options.unwrap_or_default())
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn clear_captions(state: State<'_, AppState>) -> CmdResult<Timeline> {
    let project = state.project();
    project.clear_captions().map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command]
async fn export_srt(state: State<'_, AppState>, asset_id: String, output_path: String) -> CmdResult<String> {
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    blocking(move || {
        let srt = lock_user(&shared).transcript_srt(id).map_err(|e| e.to_string())?;
        std::fs::write(&output_path, srt).map_err(|e| e.to_string())?;
        Ok(output_path)
    })
    .await
}

#[tauri::command(async)]
fn remove_silence(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project();
    project.remove_silence(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn snap_to_beats(state: State<'_, AppState>, track_id: Option<String>, tolerance: Option<f64>) -> CmdResult<Timeline> {
    let track = track_id.as_deref().map(id).transpose()?;
    let project = state.project();
    project.snap_to_beats(track, tolerance).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn extract_audio(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project();
    project.extract_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn concatenate(state: State<'_, AppState>, asset_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = asset_ids.iter().map(|s| id(s)).collect::<CmdResult<Vec<_>>>()?;
    let project = state.project();
    project.concatenate(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

// ---- history (undo / redo / revert) ----------------------------------------

#[tauri::command(async)]
fn get_history(state: State<'_, AppState>) -> CmdResult<Vec<Revision>> {
    state.project().history().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn undo(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().undo().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn redo(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().redo().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn revert_to(state: State<'_, AppState>, seq: i64) -> CmdResult<Timeline> {
    state.project().revert_to(seq).map_err(|e| e.to_string())
}

/// What one revision changed, so the edit log can explain itself.
#[tauri::command(async)]
fn revision_diff(state: State<'_, AppState>, seq: i64) -> CmdResult<TimelineDiff> {
    state.project().revision_diff(seq).map_err(|e| e.to_string())
}

// ---- staged edits (the agent's pending proposal) ---------------------------

/// The proposal a connected agent has staged, or `None`. Carries its own diff,
/// so the review card renders from one round-trip.
#[tauri::command(async)]
fn get_staged_edit(state: State<'_, AppState>) -> CmdResult<Option<StagedEdit>> {
    state.project().staged().map_err(|e| e.to_string())
}

/// The staged timeline itself — what the editor shows while previewing a
/// proposal, so the user can look at the cut rather than only read about it.
#[tauri::command(async)]
fn get_staged_timeline(state: State<'_, AppState>) -> CmdResult<Option<Timeline>> {
    state.project().staged_timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn apply_staged_edit(state: State<'_, AppState>, force: Option<bool>) -> CmdResult<Timeline> {
    state
        .project()
        .apply_staged(force.unwrap_or(false))
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn discard_staged_edit(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().discard_staged().map_err(|e| e.to_string())
}

// ---- media (preview frames, waveforms) -------------------------------------

#[tauri::command]
async fn get_frame(
    state: State<'_, AppState>,
    asset_id: String,
    time_secs: f64,
    max_width: Option<u32>,
    accurate: Option<bool>,
) -> CmdResult<String> {
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve the asset under the lock, then *drop the guard* before decoding: the
        // ffmpeg run must not hold the shared Project mutex for its whole duration, or
        // it freezes every other op (timeline edits, MCP, the next scrub frame).
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        // JPEG rather than PNG: the preview pane never needs lossless frames, and a
        // q=4 JPEG is ~5–10× smaller to encode and ship over IPC — which matters now
        // that the preview fetches frames continuously during playback. `accurate`
        // is false for rough scrub frames (keyframe-snap), true for the settled frame.
        let jpeg = Project::decode_preview_frame(&asset, time_secs, max_width.unwrap_or(960), 4, accurate.unwrap_or(true))
            .map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
        Ok(format!("data:image/jpeg;base64,{b64}"))
    })
    .await
}

/// The composited timeline still at `time_secs` — every visible clip put through
/// the same color / effect / transform / overlay chain the export uses, so the
/// preview reflects Inspector edits live (unlike `get_frame`, a raw source decode).
#[tauri::command]
async fn get_timeline_frame(state: State<'_, AppState>, time_secs: f64, max_width: Option<u32>) -> CmdResult<String> {
    let shared = state.project.clone();
    blocking(move || {
        // Resolve the inputs under the lock, then *drop the guard* before the ffmpeg
        // composite — the preview fetches frames continuously during playback, and
        // holding the shared Project mutex for the whole decode would freeze every
        // other op (timeline edits, MCP, the next scrub frame). Mirrors `get_frame`.
        let (timeline, assets) = lock_user(&shared).timeline_frame_inputs().map_err(|e| e.to_string())?;
        let jpeg = Project::composite_timeline_frame(&timeline, &assets, time_secs, max_width.unwrap_or(960), 4)
            .map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
        Ok(format!("data:image/jpeg;base64,{b64}"))
    })
    .await
}

/// One composited frame pushed to the webview during playback.
#[derive(Serialize, Clone)]
struct PlaybackFrame {
    /// The timeline time this frame shows, so the webview can drop a frame that
    /// arrived after the audio clock has already moved past it.
    time: f64,
    /// `data:image/jpeg;base64,…`, the same shape `get_timeline_frame` returns —
    /// so the preview renders streamed frames through its existing path.
    jpeg: String,
}

/// The id of the playback that *should* be running (0 = none). A stream keeps
/// going only while this still equals the id it was started with, so a seek or a
/// second Play supersedes the previous ffmpeg rather than racing it for the pane.
///
/// The id comes from the caller rather than being minted here, and `stop_playback`
/// only clears the id it was given, because start and stop are separate async
/// IPC calls that can arrive out of order: a bare generation counter would let a
/// stop meant for the previous stream land after the next one started and kill
/// it, which reads as playback that dies the moment you seek.
static ACTIVE_PLAYBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The last id `stop_playback` was asked to cancel.
///
/// `ACTIVE_PLAYBACK` alone is not enough: a stop can reach the backend *before*
/// the start it was meant to cancel (the webview issues them as two independent
/// IPC calls, and the start goes through a dynamic import first). Such a stop
/// finds nothing to clear and the stream then starts with nobody left to end it
/// — an ffmpeg that plays on forever. Recording the id instead means the stream
/// notices at its very next frame, whichever order the two calls land in.
static STOPPED_PLAYBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Play the timeline from `start`, streaming composited frames to `on_frame`
/// until playback is stopped, superseded, or the timeline ends.
///
/// Scrubbing and the settled frame still go through `get_timeline_frame` — one
/// process per frame is right when you want *one* frame. Playback is the case
/// that can't work that way: a spawn-seek-decode-exit cycle per frame caps well
/// below frame rate, so this hands the whole span to a single long-lived ffmpeg
/// and pushes frames up as they render.
///
/// Resolves only when playback ends; the caller is not expected to await it.
#[tauri::command]
async fn start_playback(
    state: State<'_, AppState>,
    playback_id: u64,
    start: f64,
    fps: Option<f64>,
    on_frame: tauri::ipc::Channel<PlaybackFrame>,
) -> CmdResult<()> {
    use std::sync::atomic::Ordering;

    let shared = state.project.clone();
    ACTIVE_PLAYBACK.store(playback_id, Ordering::SeqCst);
    blocking(move || {
        // Resolve the inputs under the lock and drop the guard before streaming:
        // playback runs for as long as the user watches, and holding the shared
        // mutex for that would freeze every edit and the whole MCP server.
        let (timeline, assets) = lock_user(&shared).timeline_frame_inputs().map_err(|e| e.to_string())?;
        // Counted and logged below: whether frames reached the webview at all, and
        // how long the first one took, are the two facts that tell a stream that
        // never started from one whose frames the preview received and discarded.
        // Without them a black pane looks identical either way.
        let began = std::time::Instant::now();
        let mut frames: u64 = 0;
        let mut first_frame: Option<std::time::Duration> = None;
        let result = kerf_core::stream_preview(&timeline, &assets, start, fps.unwrap_or(24.0), &mut |f| {
            // Superseded by a newer playback, or explicitly stopped — including by
            // a stop that arrived before this stream even started.
            if ACTIVE_PLAYBACK.load(Ordering::SeqCst) != playback_id || STOPPED_PLAYBACK.load(Ordering::SeqCst) == playback_id {
                return false;
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&f.jpeg);
            let sent = on_frame
                .send(PlaybackFrame {
                    time: f.time,
                    jpeg: format!("data:image/jpeg;base64,{b64}"),
                })
                .is_ok();
            if sent {
                frames += 1;
                first_frame.get_or_insert_with(|| began.elapsed());
            }
            sent
        });
        tracing::info!(
            playback_id,
            frames,
            first_frame_ms = first_frame.map(|d| d.as_millis() as u64),
            elapsed_ms = began.elapsed().as_millis() as u64,
            "playback stream ended"
        );
        // Running out of timeline, or being superseded, is not an error; only a
        // genuine ffmpeg failure is worth surfacing.
        if let Err(e) = result {
            tracing::debug!(error = %e, "preview stream ended");
        }
        let _ = ACTIVE_PLAYBACK.compare_exchange(playback_id, 0, Ordering::SeqCst, Ordering::SeqCst);
        Ok(())
    })
    .await
}

/// Stop the playback stream with this id (pause, seek, or a timeline edit).
/// A stop for a stream that has already been superseded is a no-op.
#[tauri::command(async)]
fn stop_playback(playback_id: u64) {
    use std::sync::atomic::Ordering;
    STOPPED_PLAYBACK.store(playback_id, Ordering::SeqCst);
    let _ = ACTIVE_PLAYBACK.compare_exchange(playback_id, 0, Ordering::SeqCst, Ordering::SeqCst);
}

#[tauri::command]
async fn get_waveform(state: State<'_, AppState>, asset_id: String, buckets: usize) -> CmdResult<Vec<f32>> {
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve under the lock, decode the whole audio stream with it released —
        // a long source takes seconds to bucket and must not stall other ops.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        Project::decode_waveform(&asset, buckets).map_err(|e| e.to_string())
    })
    .await
}

/// A window of an asset's audio as raw mono s16le PCM for the preview's Web
/// Audio playback. Returns raw bytes rather than JSON — a minute of 32 kHz
/// audio is ~3.8 MB, which a JSON number array would balloon ~5×.
#[tauri::command]
async fn get_audio(
    state: State<'_, AppState>,
    asset_id: String,
    start: f64,
    duration: f64,
    sample_rate: Option<u32>,
    clip_id: Option<String>,
) -> CmdResult<tauri::ipc::Response> {
    let clip_id = clip_id.as_deref().map(id).transpose()?;
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    let pcm = blocking(move || {
        // Resolve the asset under the lock, then drop the guard before the decode —
        // same reasoning as `get_frame`. `clip_id` names the clip this window is
        // being fetched for, so its effect chain is baked into the decode and the
        // monitor plays what the export will render, not the dry source.
        let (asset, effects) = {
            let project = lock_user(&shared);
            let asset = project.require_asset(id).map_err(|e| e.to_string())?;
            let effects = match clip_id {
                Some(clip_id) => project
                    .timeline()
                    .ok()
                    .and_then(|tl| tl.clip(clip_id).map(|c| c.audio.clone()))
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            (asset, effects)
        };
        let rate = sample_rate.unwrap_or(32_000).clamp(8_000, 48_000);
        Project::decode_audio_pcm(&asset, start, duration, rate, &effects).map_err(|e| e.to_string())
    })
    .await?;
    Ok(tauri::ipc::Response::new(pcm))
}

#[tauri::command]
async fn get_energy(state: State<'_, AppState>, asset_id: String, buckets: usize) -> CmdResult<Vec<f32>> {
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    blocking(move || {
        // Same lock-free decode shape as `get_waveform`.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        Project::decode_energy(&asset, buckets).map_err(|e| e.to_string())
    })
    .await
}

// ---- agent task queue (mutations return the refreshed queue) ---------------

#[tauri::command(async)]
fn list_tasks(state: State<'_, AppState>) -> CmdResult<Vec<Task>> {
    state.project().list_tasks().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn add_task(state: State<'_, AppState>, prompt: String) -> CmdResult<Task> {
    state.project().add_task(&prompt).map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn resolve_task(state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id)?;
    let project = state.project();
    project.resolve_task(id).map_err(|e| e.to_string())?;
    project.list_tasks().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn remove_task(state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id)?;
    let project = state.project();
    project.remove_task(id).map_err(|e| e.to_string())?;
    project.list_tasks().map_err(|e| e.to_string())
}

// ---- export ----------------------------------------------------------------

/// The hardware (GPU) video encoders this machine's ffmpeg can actually use —
/// verified once per process with a tiny test encode. The export dialog merges
/// them into its codec choices; empty means software encoders only.
#[tauri::command]
async fn hw_encoders() -> CmdResult<Vec<String>> {
    // First call probes by spawning ffmpeg, so keep it off the async workers.
    blocking(|| Ok(kerf_core::hw_encoders().to_vec())).await
}

#[tauri::command]
async fn export_timeline(
    app: AppHandle,
    state: State<'_, AppState>,
    output_path: String,
    options: ExportOptions,
) -> CmdResult<String> {
    // Snapshot the timeline + assets under the lock, then release it before the
    // (seconds-to-minutes) ffmpeg render. Otherwise the export would hold the
    // shared Project mutex for its whole duration and freeze every other GUI
    // command and the MCP agent until it finished.
    let (timeline, assets) = {
        let project = state.project();
        (
            project.timeline().map_err(|e| e.to_string())?,
            project.list_assets().map_err(|e| e.to_string())?,
        )
    };

    // Fresh cancel flag for this run; `cancel_export` flips it from another thread.
    let cancel = state.export_cancel.clone();
    cancel.store(false, Ordering::SeqCst);

    blocking(move || {
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
    })
    .await
}

/// Render the cut once per delivery frame — one file per shape beside
/// `output_path`, named by shape (`cut-9x16.mp4`). With `smart_crop`, every
/// shot is framed for every shape first (one revision, reused by later
/// exports); the project frame's own crop is never touched. Streams the same
/// `export-progress` event as a single export, with `variant` / `total` added.
#[tauri::command]
async fn export_variants(
    app: AppHandle,
    state: State<'_, AppState>,
    output_path: String,
    formats: Vec<Delivery>,
    smart_crop: bool,
    options: ExportOptions,
) -> CmdResult<Vec<String>> {
    if formats.is_empty() {
        return Err("pick at least one delivery frame".to_string());
    }
    let base = std::path::PathBuf::from(&output_path);
    let mut deliveries: Vec<Delivery> = Vec::new();
    for d in formats {
        let d = Delivery::new(d.width, d.height, d.fit);
        if !deliveries.contains(&d) {
            deliveries.push(d);
        }
    }
    let variants: Vec<kerf_core::ExportVariant> = deliveries
        .iter()
        .map(|d| kerf_core::ExportVariant::beside(&base, *d))
        .collect();
    let shared = state.project.clone();
    let cancel = state.export_cancel.clone();
    cancel.store(false, Ordering::SeqCst);

    blocking(move || {
        // Frame first — plan under the lock, sample without it, apply under it
        // again — then snapshot and render with the lock released, like a
        // single export.
        if smart_crop {
            let plan = lock_user(&shared).framing_inputs(&deliveries).map_err(|e| e.to_string())?;
            if !plan.jobs.is_empty() {
                let framings = Project::sample_framings(&plan).map_err(|e| e.to_string())?;
                let framed = lock_user(&shared).apply_framings(&framings).map_err(|e| e.to_string())?;
                if framed > 0 {
                    let _ = app.emit("project-changed", ());
                }
            }
        }
        let (timeline, assets) = {
            let project = lock_user(&shared);
            (
                project.timeline().map_err(|e| e.to_string())?,
                project.list_assets().map_err(|e| e.to_string())?,
            )
        };
        let mut on_progress = |p: kerf_core::VariantProgress| {
            let _ = app.emit("export-progress", p);
        };
        let (status, _) = kerf_core::render_variants(&timeline, &assets, &variants, &options, &mut on_progress, &|| {
            cancel.load(Ordering::SeqCst)
        })
        .map_err(|e| e.to_string())?;
        match status {
            kerf_core::RenderStatus::Completed => Ok(variants.iter().map(|v| v.output.to_string_lossy().into_owned()).collect()),
            // The variant in flight is already gone; the finished ones stay.
            kerf_core::RenderStatus::Cancelled => Err("export cancelled".to_string()),
        }
    })
    .await
}

/// Write the composited frame at `time_secs` to `output_path` as a **cover
/// image** — full delivery resolution, decoded from the original media rather
/// than a preview proxy. `format` follows the file extension when omitted.
#[tauri::command]
async fn export_cover(
    state: State<'_, AppState>,
    time_secs: f64,
    output_path: String,
    format: Option<kerf_core::ImageFormat>,
) -> CmdResult<String> {
    let shared = state.project.clone();
    blocking(move || {
        // Same shape as every heavy command: resolve under the lock, render
        // without it. A 4K still is a real decode.
        let (timeline, assets) = lock_user(&shared).export_still_inputs().map_err(|e| e.to_string())?;
        let path = Project::render_still(&timeline, &assets, time_secs, &output_path, format).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

/// Frame every shot for the delivery frame instead of centring it blindly —
/// samples where each clip's content sits and writes the crop that keeps it.
/// One clip when `clip_id` is given, otherwise every clip on an unlocked video
/// track. The result is an ordinary transform crop, so the inspector's sliders
/// still have the last word.
#[tauri::command]
async fn smart_crop(state: State<'_, AppState>, clip_id: Option<String>) -> CmdResult<Timeline> {
    let clip = clip_id.as_deref().map(id).transpose()?;
    let shared = state.project.clone();
    blocking(move || {
        // The usual shape for a heavy command: plan under the lock, decode
        // without it (one short ffmpeg pass per clip), apply under it again.
        let plan = lock_user(&shared).smart_crop_inputs(clip).map_err(|e| e.to_string())?;
        let crops = Project::sample_smart_crops(&plan).map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        project.apply_smart_crops(&crops).map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

/// Every publishing target Kerf knows about, with its frame and length limits.
#[tauri::command(async)]
fn platform_targets() -> Vec<kerf_core::PlatformTarget> {
    kerf_core::PLATFORM_TARGETS.to_vec()
}

/// How ready the current cut is for each target — what would be rejected, what
/// would be accepted and then under-distributed, and what would just be better.
#[tauri::command(async)]
fn platform_check(
    state: State<'_, AppState>,
    width: Option<u32>,
    height: Option<u32>,
) -> CmdResult<Vec<kerf_core::DeliveryCheck>> {
    // The export dialog can resize away from the project frame; when it does it
    // passes the frame it is actually about to render, so the verdict is about
    // the file that will exist rather than the one the project defaults to.
    let frame = width.zip(height);
    state.project().platform_check(frame).map_err(|e| e.to_string())
}

/// Show a file in the OS file manager. The last step of an export: the render
/// finished somewhere, and "somewhere" is not much use on its own.
#[tauri::command(async)]
fn reveal_path(app: AppHandle, path: String) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    // Open the containing folder, not the file — opening the file would launch
    // a player, which is not what "show me where it went" means.
    let target = std::path::Path::new(&path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(&path));
    app.opener()
        .open_path(target.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// The error an abandoned analysis pass returns. The webview matches on it to
/// tell "the user stopped this" apart from "this broke".
const ANALYSIS_CANCELLED: &str = "analysis cancelled";

/// Request cancellation of the in-flight analysis pass (if any). The running
/// [`analyze_asset`] observes the flag between steps — and, during
/// transcription, about once a second — then gives up and caches nothing.
#[tauri::command(async)]
fn cancel_analysis(state: State<'_, AppState>) {
    state.analysis_cancel.store(true, Ordering::SeqCst);
}

/// Request cancellation of the in-flight export (if any). The running
/// [`export_timeline`] observes the flag on its next progress poll, stops
/// ffmpeg, and returns the `"export cancelled"` error.
#[tauri::command(async)]
fn cancel_export(state: State<'_, AppState>) {
    state.export_cancel.store(true, Ordering::SeqCst);
}

// ---- agent connection (MCP endpoint) ---------------------------------------

/// The local MCP endpoint URL a connected LLM points at (e.g.
/// `http://127.0.0.1:7777/mcp`), honoring the `KERF_MCP_ADDR` override. The
/// agent panel surfaces this so the user knows how to connect their agent.
#[tauri::command(async)]
fn mcp_endpoint() -> String {
    mcp::endpoint_url()
}

/// Where the endpoint is, and how long ago an agent last used it.
///
/// A streamable-HTTP client holds no connection between calls, so there is no
/// "is it plugged in" to report — `last_seen_secs` is `None` until something
/// has spoken to the endpoint at all, and the panel decides from its age
/// whether to call that connected. Anything else would be the green dot the
/// panel used to show whether or not an agent existed.
#[derive(Serialize)]
struct AgentStatus {
    endpoint: String,
    last_seen_secs: Option<i64>,
}

#[tauri::command(async)]
fn agent_status() -> AgentStatus {
    AgentStatus {
        endpoint: mcp::endpoint_url(),
        last_seen_secs: mcp::agent_last_seen_secs(),
    }
}

// ---- app settings ----------------------------------------------------------

/// The current preferences, resolved against the engine (see
/// [`settings::SettingsView`]).
#[tauri::command(async)]
fn get_settings() -> settings::SettingsView {
    settings::SettingsView::current()
}

/// Write the preferences and put them into force. Returns the resolved view, so
/// the dialog can show the clamped percentage and the cores it works out to
/// without a second round-trip.
#[tauri::command(async)]
fn set_settings(app: AppHandle, settings: settings::Settings) -> CmdResult<settings::SettingsView> {
    // Clamp through the engine first, then persist what was actually applied —
    // storing an out-of-range value would keep re-clamping on every launch.
    let stored = settings::Settings {
        cpu_percent: kerf_core::set_cpu_percent(settings.cpu_percent),
    };
    settings::save(&app, &stored)?;
    Ok(settings::SettingsView::current())
}

// ---- diagnostics (logs) ----------------------------------------------------

#[tauri::command(async)]
fn log_dir(app: AppHandle) -> CmdResult<String> {
    app.path()
        .app_log_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

/// Open the log directory in the OS file manager so users can attach the file.
#[tauri::command(async)]
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

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
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
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            project: project.clone(),
            export_cancel: Arc::new(AtomicBool::new(false)),
            analysis_cancel: Arc::new(AtomicBool::new(false)),
        })
        .setup(move |app| {
            // Logging needs the resolved platform log directory, so set it up here
            // (before anything else in setup) rather than at the top of `run`.
            init_logging(app.handle());
            install_panic_hook();
            use_bundled_ffmpeg();
            // Before anything can spawn ffmpeg: how much of the machine it may take.
            settings::apply(&settings::load(app.handle()));
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
            list_fonts,
            get_timeline,
            get_asset_metadata,
            project_path,
            new_project,
            open_project,
            save_project_as,
            import_asset,
            analyze_asset,
            transcription_status,
            set_speech_model,
            download_speech_model,
            cut_clip,
            add_clip,
            split_clip,
            trim_clip,
            reorder_clip,
            move_clip,
            ripple_delete,
            cut_clip_range,
            add_track,
            remove_track,
            set_track_duck,
            set_track_volume,
            set_track_pan,
            set_delivery_format,
            set_track_muted,
            set_track_solo,
            set_track_locked,
            set_clip_enabled,
            duplicate_clips,
            insert_clips,
            remove_clip,
            set_volume,
            set_fade,
            set_speed,
            set_transform,
            set_color,
            set_transition,
            set_mask,
            set_video_effects,
            set_audio_effects,
            set_keyframes,
            add_keyframe,
            clear_keyframes,
            set_reframe,
            set_asset_projection,
            clear_reframe,
            set_reframe_keyframes,
            add_reframe_keyframe,
            add_marker,
            update_marker,
            remove_marker,
            add_overlay,
            update_overlay,
            remove_overlay,
            set_overlay_keyframes,
            generate_captions,
            clear_captions,
            export_srt,
            remove_silence,
            snap_to_beats,
            smart_crop,
            extract_audio,
            concatenate,
            get_history,
            revision_diff,
            get_staged_edit,
            get_staged_timeline,
            apply_staged_edit,
            discard_staged_edit,
            undo,
            redo,
            revert_to,
            get_frame,
            get_timeline_frame,
            start_playback,
            stop_playback,
            get_waveform,
            get_audio,
            get_energy,
            list_tasks,
            add_task,
            resolve_task,
            remove_task,
            hw_encoders,
            export_timeline,
            export_variants,
            cancel_export,
            cancel_analysis,
            export_cover,
            platform_targets,
            platform_check,
            reveal_path,
            mcp_endpoint,
            agent_status,
            get_settings,
            set_settings,
            log_dir,
            reveal_logs
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kerf");
}
