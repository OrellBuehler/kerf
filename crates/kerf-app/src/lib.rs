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

mod editor_context;
mod gpu_preview;
mod mcp;
mod mcp_commands;
mod popout;
mod recovery;
mod settings;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use kerf_core::{
    Asset, AssetAnalysis, AudioEffect, CaptionFile, CaptionFormat, CaptionImportRequest, CaptionOptions, CaptionTimeBase,
    ClipCut, ClipMove, Delivery, EditSource, ExportOptions, Filmstrip, FilmstripSheet, Fit, Grade, ImportOutcome, ImportSummary,
    Keyframe, Levels, Mask, MissingAsset, Page, Project, Projection, Property, PropertyKey, ReframeKeyframe, RelinkCheck,
    Relinked, RemovedAsset, Revision, SplitSide, StagedEdit, StreamKind, SubsetApplied, SvgCheck, SvgLayout, SvgOverlayPatch,
    SvgPresetInfo, SvgPresetKind, SvgPresetParams, SvgSource, Task, TemplateControl, TemplateLibrary, TemplateList,
    TemplateSaveRequest, TemplateSaved, TextKeyframe, TimeRange, Timeline, TimelineDiff, TitleClipPatch, TitleClipPlacement,
    TitleClipResult, TitleEdited, Transition, TransitionKind, VideoEffect, WaveformRange,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

struct AppState {
    project: Arc<Mutex<Project>>,
    /// The stops for the long jobs (`cancel_export` and the rest): lives outside the project
    /// lock so a cancel lands even while a render holds it. Each is a [`StopEpoch`], so a stop
    /// reaches every run of that job that began before it — the GUI's and an agent's alike — and
    /// a run that begins after it starts clean; there is no flag to reset under a run about to
    /// start, which is how a Stop used to be lost.
    export_cancel: StopEpoch,
    /// Same, for the in-flight analysis pass. Analysis downloads a speech model
    /// and then transcribes for minutes, so it has to be abandonable — and the
    /// GUI runs imported assets through it one after another, which is a long
    /// commitment to make on the user's behalf without an exit.
    analysis_cancel: StopEpoch,
    /// Same, for the Mixer's loudness measurement. It holds the heavy-job lease for
    /// a whole-mix decode (minutes on a long cut), so it has to be stoppable.
    levels_cancel: StopEpoch,
    /// Same, for the sync by audio (`cancel_audio_sync`).
    sync_cancel: StopEpoch,
    /// The last sync measured and the decode it measured, so "Move anyway" applies that
    /// answer instead of decoding the same sound again.
    last_sync: Arc<Mutex<Option<(kerf_core::AudioSyncPlan, kerf_core::AudioSyncMeasure)>>>,
    /// Same, for a voiceover being synthesized (or its model downloading).
    voiceover_cancel: StopEpoch,
    /// Same, for music being composed (or its SoundFont downloading).
    music_cancel: StopEpoch,
    /// Same, for a band-balance measurement (`cancel_band_balance`).
    bands_cancel: StopEpoch,
    stems_cancel: StopEpoch,
    /// Same, for a speech model being downloaded (`cancel_model_download`).
    model_cancel: StopEpoch,
    /// Same, for a cover frame being written (`cancel_cover_export`).
    cover_cancel: StopEpoch,
    /// Same, for the shots a smart crop is sampling (`cancel_smart_crop`).
    smart_crop_cancel: StopEpoch,
    /// Imports run side by side, so their stop (`cancel_import`) is a count rather than a
    /// flag: it ends every import that began before it and none that begin after.
    import_stop: StopEpoch,
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
    /// The Preview's composited frames, cached, and the one process that draws the frames
    /// around the playhead. Shared with the GPU preview, whose JPEG route reads it too.
    scrub: Arc<kerf_core::ScrubFrames>,
}

#[derive(Serialize)]
struct AssetMetadata {
    asset: Asset,
    analysis: Option<AssetAnalysis>,
}

type CmdResult<T> = Result<T, String>;

/// A stop request for jobs that run side by side (several imports at once): [`StopEpoch::stop`]
/// reaches every job that began before it and none that begin after, so a stop never has to be
/// "reset" under a job about to start, and one press ends the whole batch.
#[derive(Clone, Default)]
struct StopEpoch(Arc<AtomicU64>);

impl StopEpoch {
    /// Note now as a job's start; the closure says whether a stop has come since.
    fn begin(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        let (epoch, began) = (self.0.clone(), self.0.load(Ordering::SeqCst));
        move || epoch.load(Ordering::SeqCst) != began
    }

    fn stop(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A question that is expensive to ask (it takes the project lock), put to a poll that runs fifty
/// times a second: asked again only once `every` has passed, and once it has been answered `true`
/// it stays `true`.
pub(crate) struct Throttled<F: Fn() -> bool> {
    ask: F,
    every: std::time::Duration,
    asked_at: std::cell::Cell<Option<std::time::Instant>>,
    answer: std::cell::Cell<bool>,
}

impl<F: Fn() -> bool> Throttled<F> {
    pub(crate) fn new(every: std::time::Duration, ask: F) -> Self {
        Self {
            ask,
            every,
            asked_at: std::cell::Cell::new(None),
            answer: std::cell::Cell::new(false),
        }
    }

    pub(crate) fn ask(&self) -> bool {
        if self.answer.get() {
            return true;
        }
        if self.asked_at.get().is_some_and(|at| at.elapsed() < self.every) {
            return false;
        }
        self.asked_at.set(Some(std::time::Instant::now()));
        self.answer.set((self.ask)());
        self.answer.get()
    }
}

/// The stop for a long job begun under project `generation`: `cancel`, or the project having been
/// replaced since (an open or a new project) — a result for a project nobody has open is dropped on
/// arrival, so the ffmpeg still working towards it is not left running. The generation is read
/// under the lock at most twice a second; the closure is polled from the job's own thread.
pub(crate) fn stop_or_replaced<'a>(
    cancel: impl Fn() -> bool + 'a,
    shared: &'a Mutex<Project>,
    generation: u64,
) -> impl Fn() -> bool + 'a {
    let replaced = Throttled::new(std::time::Duration::from_millis(500), move || {
        lock_user(shared).generation() != generation
    });
    move || cancel() || replaced.ask()
}

/// Fail a job whose project was swapped out (an open or a new project) while its lock was
/// released: what it made belongs to a project that is no longer the open one. `noted` is
/// [`Project::generation`] as read under the lock the job resolved its inputs with.
pub(crate) fn require_generation(project: &Project, noted: u64, doing: &str) -> CmdResult<()> {
    if project.generation() == noted {
        Ok(())
    } else {
        Err(format!("the project was replaced while {doing}; the result was dropped"))
    }
}

impl AppState {
    fn new(
        project: Arc<Mutex<Project>>,
        main_window_shown: Arc<AtomicBool>,
        launch: LaunchSlot,
        scrub: Arc<kerf_core::ScrubFrames>,
    ) -> Self {
        Self {
            project,
            export_cancel: StopEpoch::default(),
            analysis_cancel: StopEpoch::default(),
            levels_cancel: StopEpoch::default(),
            sync_cancel: StopEpoch::default(),
            last_sync: Arc::new(Mutex::new(None)),
            voiceover_cancel: StopEpoch::default(),
            music_cancel: StopEpoch::default(),
            bands_cancel: StopEpoch::default(),
            stems_cancel: StopEpoch::default(),
            model_cancel: StopEpoch::default(),
            cover_cancel: StopEpoch::default(),
            smart_crop_cancel: StopEpoch::default(),
            import_stop: StopEpoch::default(),
            main_window_shown,
            launch: Mutex::new(launch),
            scrub,
        }
    }

    fn project(&self) -> std::sync::MutexGuard<'_, Project> {
        lock_user(&self.project)
    }

    /// An analysis run is starting: hand back what says whether `cancel_analysis` has been pressed
    /// since, for the run to poll. The GUI's `analyze_asset` does this; the MCP `analyze_asset`
    /// takes it the same way and combines it with its own cancellation, so the GUI's Stop ends an
    /// agent's analysis too — and a Stop pressed just before another run starts is not undone by it.
    pub(crate) fn begin_analysis(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.analysis_cancel.begin()
    }

    /// A speech-model download is starting; the closure says whether `cancel_model_download` has
    /// been pressed since. The GUI's download and an agent's alike take one.
    pub(crate) fn begin_model_download(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.model_cancel.begin()
    }

    /// A cover frame is being written; the closure says whether `cancel_cover_export` has been
    /// pressed since. The GUI's export and an agent's alike take one.
    pub(crate) fn begin_cover_export(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.cover_cancel.begin()
    }

    /// An import is starting; the closure says whether `cancel_import` has been pressed since.
    pub(crate) fn begin_import(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.import_stop.begin()
    }

    /// A voiceover is being synthesized; the closure says whether `cancel_voiceover` has been pressed
    /// since. The GUI's voiceover dialog and an agent's `generate_voiceover` take one alike.
    pub(crate) fn begin_voiceover(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.voiceover_cancel.begin()
    }

    /// Music is being composed; the closure says whether `cancel_music` has been pressed since. The
    /// GUI's Compose dialog and an agent's `compose_music` take one alike.
    pub(crate) fn begin_music(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.music_cancel.begin()
    }

    /// A stem separation is starting; the closure says whether `cancel_stems` has been pressed since.
    pub(crate) fn begin_stems(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.stems_cancel.begin()
    }

    /// A smart crop is sampling; the closure says whether `cancel_smart_crop` has been pressed since.
    pub(crate) fn begin_smart_crop(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.smart_crop_cancel.begin()
    }

    /// A render is starting (a single export or a multi-format one); the closure says whether
    /// `cancel_export` has been pressed since.
    pub(crate) fn begin_export(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.export_cancel.begin()
    }
}

/// Lock the shared project for a GUI command, attributing edits to the user;
/// the MCP server attributes its own edits to the agent under the same lock
/// (see `mcp::KerfMcp::lock`). Recovers from a poisoned mutex (a panic while
/// another op held it) rather than failing every command for the rest of the
/// session.
pub(crate) fn lock_user(project: &Mutex<Project>) -> std::sync::MutexGuard<'_, Project> {
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

/// Parse the id a command was handed, naming the argument in the error: a bare "invalid
/// character" says nothing about which of a command's several ids was wrong.
fn id(s: &str, arg: &str) -> CmdResult<Uuid> {
    Uuid::parse_str(s).map_err(|e| {
        let shown: String = s.chars().take(64).collect();
        format!("invalid {arg} {shown:?}: not an id ({e})")
    })
}

/// [`id`] for an argument that may be left out.
fn opt_id(s: Option<&str>, arg: &str) -> CmdResult<Option<Uuid>> {
    s.map(|s| id(s, arg)).transpose()
}

/// [`id`] for a list of ids; the error names the entry (`clip_ids[2]`).
fn ids(list: &[String], arg: &str) -> CmdResult<Vec<Uuid>> {
    list.iter().enumerate().map(|(i, s)| id(s, &format!("{arg}[{i}]"))).collect()
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

/// Kerf's own directories an output must never be written into: the settings (which hold the agent
/// token) and their backups, and the logs. Filled once at startup ([`set_app_dirs`]).
static APP_DIRS: std::sync::OnceLock<Vec<(std::path::PathBuf, String)>> = std::sync::OnceLock::new();

fn set_app_dirs(app: &AppHandle) {
    let mut dirs = Vec::new();
    if let Ok(config) = app.path().app_config_dir() {
        dirs.push((config, "Kerf's settings".to_string()));
    }
    if let Ok(logs) = log_dir_path(app) {
        dirs.push((logs, "Kerf's logs".to_string()));
    }
    let _ = crate::APP_DIRS.set(dirs);
}

/// `output` against what the project protects (a source, a capture, a proxy, the project's own file,
/// the cache) **and Kerf's own settings and logs**: the checks every export, cover, subtitle file and
/// Save As make, with the lock released (this canonicalises and stats).
pub(crate) fn check_output(output: &std::path::Path, protected: &kerf_core::paths::Protected) -> kerf_core::Result<()> {
    let mut all = protected.clone();
    all.dirs.extend(APP_DIRS.get().into_iter().flatten().cloned());
    Project::check_output_path_with(output, &all)
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
///
/// A **network path** is refused for a path an agent supplies (`\\host\share\out.mp4` and the
/// mixed forms Windows reads the same way, `\\?\UNC\…`): opening one makes the machine
/// authenticate to whatever host the string names, and a render may not be sent off the machine
/// this way. Only the verbatim form of a local drive (`\\?\C:\...`) starts with two separators and
/// is let through. `//host/share` is a network path **on Windows only**: on macOS and Linux
/// `//tmp/out.mp4` is `/tmp/out.mp4`. The device namespace (`\\.\pipe\…`) is never an output.
pub(crate) fn require_local_output_path(path: &str) -> Result<(), String> {
    check_output_path(path, PathSource::Agent, cfg!(windows))
}

/// [`require_local_output_path`] for a path the user picked in Kerf's own save dialog: a network
/// share they chose (`\\nas\share\out.mp4`) is their intent, so on Windows a UNC path passes. URLs,
/// protocol sinks, relative paths and the device namespace are refused all the same.
pub(crate) fn require_user_output_path(path: &str) -> Result<(), String> {
    check_output_path(path, PathSource::User, cfg!(windows))
}

/// Who named a path: the user, in a dialog, or an agent, in a tool call.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathSource {
    User,
    Agent,
}

/// `\\?\C:\`: the verbatim spelling of a drive, which is local.
fn is_verbatim_drive(bytes: &[u8]) -> bool {
    let separator = |at: usize| bytes.get(at).is_some_and(|b| matches!(b, b'\\' | b'/'));
    separator(0)
        && separator(1)
        && bytes.get(2) == Some(&b'?')
        && separator(3)
        && bytes.get(4).is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(5) == Some(&b':')
        && separator(6)
}

/// Whether `path` names a network share or the device namespace: `Some(true)` for a device
/// (`\\.\pipe\…`, `\\?\GLOBALROOT\…`), `Some(false)` for a UNC path (`\\host\share`, `\\?\UNC\…`,
/// and `//host/share` on Windows). `//host/share` is an ordinary absolute path elsewhere; the
/// verbatim spelling of a local drive is local.
fn network_path(path: &str, windows: bool) -> Option<bool> {
    let bytes = path.as_bytes();
    let separator = |at: usize| bytes.get(at).is_some_and(|b| matches!(b, b'\\' | b'/'));
    if !(separator(0) && separator(1)) || is_verbatim_drive(bytes) {
        return None;
    }
    let backslashed = bytes[0] == b'\\' || bytes[1] == b'\\';
    if !(backslashed || windows) {
        return None;
    }
    let normal = path.replace('\\', "/");
    Some(normal.starts_with("//./") || (normal.starts_with("//?/") && !normal[4..].to_ascii_uppercase().starts_with("UNC/")))
}

/// [`require_local_output_path`] for a file an agent asks Kerf to read: opening a network path makes
/// the machine authenticate to whatever host the string names, so UNC and device-namespace paths
/// are refused **before any filesystem call**, on every surface an agent names a file through.
pub(crate) fn require_local_input_path(path: &str) -> Result<(), String> {
    check_input_path(path, cfg!(windows))
}

/// The rule of [`require_local_input_path`], with the platform a parameter so every spelling is
/// tested everywhere.
pub(crate) fn check_input_path(path: &str, windows: bool) -> Result<(), String> {
    match network_path(path, windows) {
        Some(_) => Err(format!(
            "{path:?} is a network (UNC) or device path; copy the file to a local drive and import it from there"
        )),
        None => Ok(()),
    }
}

/// The rules of [`require_local_output_path`] and [`require_user_output_path`], with the platform
/// a parameter so every spelling is tested everywhere.
pub(crate) fn check_output_path(path: &str, source: PathSource, windows: bool) -> Result<(), String> {
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
    if let Some(device) = network_path(path, windows) {
        if device || source == PathSource::Agent || !windows {
            return Err(format!(
                "output path {path:?} is a network (UNC) or device path; save to a local drive instead"
            ));
        }
        // A share the user chose in the save dialog.
        return Ok(());
    }
    let bytes = path.as_bytes();
    let verbatim_drive = is_verbatim_drive(bytes);
    let is_windows_absolute =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    if !(path.starts_with('/') || verbatim_drive || is_windows_absolute) {
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
#[tauri::command]
async fn list_fonts() -> CmdResult<Vec<String>> {
    // The first call reads every font file's name table: seconds on a machine with hundreds.
    blocking(|| Ok(kerf_core::list_system_fonts())).await
}

#[tauri::command(async)]
fn get_timeline(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn get_asset_metadata(state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetMetadata> {
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
struct AssetRemoval {
    #[serde(flatten)]
    removed: RemovedAsset,
    timeline: Timeline,
}

/// Take an asset out of the library. Refused while a clip of the cut (or of an open proposal)
/// uses it, unless `force`, which first removes those clips as one `Remove media` revision.
/// Its analysis goes with it; its proxies and waveform cache are cleaned up with the lock
/// released.
#[tauri::command]
async fn remove_asset(state: State<'_, AppState>, asset_id: String, force: Option<bool>) -> CmdResult<AssetRemoval> {
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
        Project::forget_media_caches(std::path::Path::new(&removed.asset.path));
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
async fn relink_asset(app: AppHandle, state: State<'_, AppState>, asset_id: String, new_path: String) -> CmdResult<Relinked> {
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
        if relinked.previous_path != relinked.asset.path {
            Project::forget_media_caches(std::path::Path::new(&relinked.previous_path));
        }
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
async fn check_relink(state: State<'_, AppState>, asset_id: String, new_path: String) -> CmdResult<RelinkCheck> {
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
async fn missing_assets(state: State<'_, AppState>) -> CmdResult<Vec<MissingAsset>> {
    let shared = state.project.clone();
    blocking(move || {
        let files = lock_user(&shared).missing_assets_inputs().map_err(|e| e.to_string())?;
        Ok(Project::find_missing_assets(files))
    })
    .await
}

/// Which of `paths` exist, in order: the export dialog asks before it overwrites a file.
/// Stats on the blocking pool; at most 256 paths.
#[tauri::command]
async fn paths_exist(paths: Vec<String>) -> CmdResult<Vec<bool>> {
    if paths.len() > 256 {
        return Err(format!("{} paths is too many to check at once (256 at most)", paths.len()));
    }
    blocking(move || Ok(paths.iter().map(|p| std::path::Path::new(p).exists()).collect())).await
}

// ---- project file (open / save) --------------------------------------------

/// Path of the `.kerf` file backing the open project, or `null` if it lives
/// only in memory (the seeded sample) and isn't persisted yet.
#[tauri::command(async)]
fn project_path(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(state.project().path().map(|p| p.display().to_string()))
}

/// Replace the open project with a fresh, empty untitled one (no sample data), backed by a
/// crash-recovery file (`recovery.rs`) but not saved until `save_project_as`. The GUI and the
/// embedded MCP server share this `Project`, so both switch to it.
#[tauri::command]
async fn new_project(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<String>> {
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
async fn open_project(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
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

/// Remember which project is open now: `Some(path)` goes to the front of the recent list and is
/// the one a launch with "reopen the last project" opens; `None` is a new project with no file,
/// which leaves nothing to reopen. Used by every way a project becomes the open one, the MCP
/// server's included. A list that cannot be written is not worth failing the open for; touches
/// the disk, so call it off the main thread.
pub(crate) fn note_open(app: &AppHandle, path: Option<&str>) {
    if let Err(e) = settings::record_open(app, path) {
        tracing::warn!(error = %e, "could not record the open project");
    }
}

/// The projects opened or saved most recently, newest first (at most 10), the ones whose
/// file is gone dropped and forgotten.
#[tauri::command]
async fn recent_projects(app: AppHandle) -> CmdResult<Vec<String>> {
    blocking(move || settings::recent(&app)).await
}

/// Open one of the recent projects. A file that has since gone is not opened (and not
/// created, as `Project::open` would): the entry is dropped from the list and the error says so.
#[tauri::command]
async fn open_recent(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
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
async fn clear_recent_projects(app: AppHandle) -> CmdResult<()> {
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
async fn save_project_as(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Option<String>> {
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

/// The error an abandoned import returns, for the webview to stay quiet on.
const IMPORT_CANCELLED: &str = "import cancelled";

/// Add a media file to the library. An Insta360 lens pair is stitched first, which takes
/// minutes: that streams `import-progress` and `cancel_import` ends it (and every other import
/// under way), rejecting with `"import cancelled"` and leaving no half-written stitch.
#[tauri::command]
async fn import_asset(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<Asset> {
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
fn cancel_import(state: State<'_, AppState>) {
    state.import_stop.stop();
}

/// What the webview is told about an asset's proxy: the `proxy-progress` event, on every
/// change (queued, each percent of a build, ready, failed), and `proxy-ready` once one
/// lands so the preview re-decodes the current frame from it.
pub(crate) fn proxy_notifier(app: &AppHandle) -> kerf_core::proxy::Notify {
    let app = app.clone();
    Arc::new(move |status: &kerf_core::ProxyStatus| {
        if let Err(e) = app.emit("proxy-progress", status) {
            tracing::warn!(error = %e, "failed to emit proxy-progress");
        }
        if status.state == kerf_core::ProxyPhase::Ready {
            if let Err(e) = app.emit("proxy-ready", ()) {
                tracing::warn!(error = %e, "failed to emit proxy-ready");
            }
        }
    })
}

/// Queue an asset's preview proxy (all-intra, downscaled) for background generation so
/// scrubbing decodes one keyframe instead of seeking a long GOP. Previews fall back to
/// the original source until it lands (`proxy-progress` says how far it is, `proxy-ready`
/// that it is there). A still, an audio-only file, an asset whose proxy was deleted on
/// purpose and one that already has a proxy are skipped, and so is everything while
/// proxies are off or the preview source is the original.
///
/// The proxy takes its place in front of analysis in the heavy-job queue **before this
/// returns** (`kerf_core::proxy`), which is why imports call it before the webview can
/// start analysing what they brought in. It may run an `ffprobe` (cached) the first time
/// per file: call it from the blocking pool, never from an async task.
pub(crate) fn spawn_proxy(app: &AppHandle, asset: &Asset) {
    let declined = app
        .try_state::<AppState>()
        .and_then(|state| state.project().proxy_declined(asset.id).ok())
        .unwrap_or(false);
    let input = kerf_core::ProxyInput {
        asset: asset.clone(),
        declined,
    };
    kerf_core::proxy::queue_auto(&input, proxy_notifier(app));
}

/// [`spawn_proxy`] for every asset of the open project, off the calling thread (a project
/// can hold hundreds of them and each may need its probe): what a changed proxy size or
/// preview source asks for.
fn respawn_proxies(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let (assets, generation) = match app.try_state::<AppState>() {
            Some(state) => {
                let project = state.project();
                (project.list_assets().unwrap_or_default(), project.generation())
            }
            None => return,
        };
        for asset in &assets {
            // Another project opened meanwhile: these are not its assets.
            let current = app
                .try_state::<AppState>()
                .is_some_and(|state| state.project().generation() == generation);
            if !current {
                return;
            }
            spawn_proxy(&app, asset);
        }
    });
}

/// The status of every asset's proxy. The inputs are taken under the lock and the disk is
/// asked with it released (a status may run the cached `ffprobe` that keys the proxy).
#[tauri::command]
async fn proxy_statuses(state: State<'_, AppState>) -> CmdResult<Vec<kerf_core::ProxyStatus>> {
    let shared = state.project.clone();
    blocking(move || {
        let inputs = lock_user(&shared).proxy_inputs().map_err(|e| e.to_string())?;
        Ok(kerf_core::proxy::statuses(&inputs))
    })
    .await
}

/// Build an asset's proxy again from the original: the one it has (and any being built) is
/// dropped first, and a proxy the user had deleted is wanted again — but only once the rebuild
/// was accepted (proxies off, or a file that needs none, refuse it and change nothing).
/// Returns where the new build starts; `proxy-progress` follows it. `lock` is how the caller
/// takes the project (the GUI as the user, the MCP server as the agent).
pub(crate) fn rebuild_proxy_now<'a>(
    app: &AppHandle,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
) -> CmdResult<kerf_core::ProxyStatus> {
    let (asset, generation) = {
        let project = lock();
        (
            project.require_asset(asset_id).map_err(|e| e.to_string())?,
            project.generation(),
        )
    };
    let status = kerf_core::proxy::rebuild(&asset, proxy_notifier(app)).map_err(|e| e.to_string())?;
    let project = lock();
    require_generation(&project, generation, "its proxy was being rebuilt")?;
    project.set_proxy_declined(asset_id, false).map_err(|e| e.to_string())?;
    Ok(status)
}

/// Delete an asset's proxy files at every size and stop any build of them; the proxy is not
/// made again by itself until it is rebuilt. Returns the bytes freed and the new status.
pub(crate) fn delete_proxy_now<'a>(
    app: &AppHandle,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
) -> CmdResult<(u64, kerf_core::ProxyStatus)> {
    let input = {
        let project = lock();
        project.set_proxy_declined(asset_id, true).map_err(|e| e.to_string())?;
        project.proxy_input(asset_id).map_err(|e| e.to_string())?
    };
    let freed = kerf_core::proxy::delete(&input.asset);
    let status = kerf_core::proxy::status(&input);
    if let Err(e) = app.emit("proxy-progress", &status) {
        tracing::warn!(error = %e, "failed to emit proxy-progress");
    }
    // Previews that were decoding the proxy decode the original from here on.
    if let Err(e) = app.emit("proxy-ready", ()) {
        tracing::warn!(error = %e, "failed to emit proxy-ready");
    }
    Ok((freed, status))
}

#[tauri::command]
async fn rebuild_proxy(app: AppHandle, state: State<'_, AppState>, asset_id: String) -> CmdResult<kerf_core::ProxyStatus> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || rebuild_proxy_now(&app, &|| lock_user(&shared), id)).await
}

#[tauri::command]
async fn delete_proxy(app: AppHandle, state: State<'_, AppState>, asset_id: String) -> CmdResult<kerf_core::ProxyStatus> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || delete_proxy_now(&app, &|| lock_user(&shared), id).map(|(_, status)| status)).await
}

/// One step of an analysis pass, tagged with the asset it belongs to so the bin can badge
/// the right row when several assets are analyzed at once.
#[derive(Serialize, Clone)]
struct AnalysisProgressEvent {
    asset_id: String,
    stage: String,
    fraction: Option<f64>,
    detail: Option<String>,
}

/// What an analysis run left behind.
pub(crate) struct AnalysisOutcome {
    /// The cached analysis after the run (the steps that finished merged in).
    pub analysis: AssetAnalysis,
    pub status: kerf_core::AnalysisStatus,
    /// Steps that failed, with why — the rest of the run still landed.
    pub failed: Vec<(kerf_core::AnalysisKind, String)>,
}

/// Run analysis `steps` (the default set when `None`) on an asset and cache what they find.
///
/// The one path the GUI command and the MCP tool share, so both stream `analysis-progress`
/// (the step, with a fraction where it has one) and `analysis-status` (the per-kind state,
/// which the bin's chips draw), and both follow the same rules: the heavy work runs with
/// the project lock released; **each step that finishes is merged into the cached analysis
/// at once** (without touching the other kinds), so its chip turns done and its markers appear
/// while the slower steps still run; a step that was cancelled or failed caches nothing.
/// Steps wait their turn behind exports and preview proxies, say so (`waiting`), and can be
/// stopped while queued. A run in which every step failed is an error; a partial failure is
/// not (it is in the outcome and the status). `lock` is how the caller takes the project: the
/// GUI as the user, the MCP server as the agent (which also stamps its activity).
pub(crate) fn run_analysis<'a>(
    app: &AppHandle,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
    steps: Option<&[kerf_core::AnalysisKind]>,
    cancel: &dyn Fn() -> bool,
) -> CmdResult<AnalysisOutcome> {
    let (asset, stored, generation) = {
        let project = lock();
        (
            project.require_asset(asset_id).map_err(|e| e.to_string())?,
            project.get_analysis(asset_id).map_err(|e| e.to_string())?,
            project.generation(),
        )
    };
    // What is cached as of now: it grows with each finished step, and the status reads it.
    let current = std::cell::RefCell::new(stored);
    let emit_status = || {
        let _ = app.emit(
            "analysis-status",
            kerf_core::analysis_status(asset_id, current.borrow().as_ref()),
        );
    };
    let id_text = asset_id.to_string();
    // Analysis is no longer a short opaque wait: the first transcription downloads a speech
    // model and then runs inference for minutes, so each step is streamed to the webview
    // rather than hidden behind a spinner.
    let mut on_progress = |p: kerf_core::AnalysisProgress| {
        let _ = app.emit(
            "analysis-progress",
            AnalysisProgressEvent {
                asset_id: id_text.clone(),
                stage: p.stage.clone(),
                fraction: p.fraction,
                detail: p.detail,
            },
        );
        // A step starting is the running chip; a model download is part of its step.
        if !matches!(p.stage.as_str(), "waiting" | "download_model" | "done") && p.fraction.is_none() {
            emit_status();
        }
    };
    let mut on_step = |so_far: &AssetAnalysis| {
        let project = lock();
        // A step that finished for a project that is no longer open is not cached in this one.
        if project.generation() != generation {
            return;
        }
        match project.merge_analysis(so_far) {
            Ok(merged) => {
                drop(project);
                *current.borrow_mut() = Some(merged);
                emit_status();
            }
            // The final merge below tries again and reports it.
            Err(e) => tracing::warn!(error = %e, "could not cache a finished analysis step"),
        }
    };
    // A project opened (or started) while the steps run makes the rest of the run pointless — its
    // result is dropped below — so the running ffmpeg is stopped too, not left to finish the machine's
    // time on a file nobody has open any more. The generation is asked of the project (a lock) at most
    // twice a second.
    let replaced = Throttled::new(std::time::Duration::from_millis(500), || lock().generation() != generation);
    let stop = || cancel() || replaced.ask();
    let run = kerf_core::analyze_asset_steps(&asset, steps, &mut on_progress, &mut on_step, &stop).map_err(|e| e.to_string())?;
    let merged = {
        let project = lock();
        // The project was replaced while the steps ran: nothing of this run belongs in the new one,
        // and the person who opened it knows (the same quiet stop as a cancel).
        if project.generation() != generation {
            return Err(ANALYSIS_CANCELLED.to_string());
        }
        project.merge_analysis(&run.patch).map_err(|e| e.to_string())?
    };
    *current.borrow_mut() = Some(merged.clone());
    emit_status();
    if run.cancelled {
        // A cancel is the user's own doing, not a failure — the caller keys off this string to
        // stay quiet about it. What finished before it is cached; the step it stopped is not.
        return Err(ANALYSIS_CANCELLED.to_string());
    }
    if run.patch.ran.is_empty() {
        if let Some(why) = run.failure_summary() {
            return Err(why);
        }
    }
    Ok(AnalysisOutcome {
        status: kerf_core::analysis_status(asset_id, Some(&merged)),
        analysis: merged,
        failed: run.failed,
    })
}

/// Re-time an asset's cached transcript to the speech energy and label its speakers, without
/// re-transcribing — the Tauri command's and the MCP tool's shared body. The transcript is read
/// under the lock; the decode and clustering run with it released, in the background lane,
/// streaming `analysis-progress` and giving up on `cancel`; the result is stored under the
/// lock again, unless a newer transcript landed meanwhile. Only analysis moved, so it says so
/// with `analysis-status`, not `project-changed`.
pub(crate) fn run_refine<'a>(
    app: &AppHandle,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
    cancel: &dyn Fn() -> bool,
) -> CmdResult<AssetAnalysis> {
    let (asset, read, generation) = {
        let project = lock();
        (
            project.require_asset(asset_id).map_err(|e| e.to_string())?,
            project.transcript(asset_id).map_err(|e| e.to_string())?,
            project.generation(),
        )
    };
    let id_text = asset_id.to_string();
    let emit = |p: kerf_core::AnalysisProgress| {
        let _ = app.emit(
            "analysis-progress",
            AnalysisProgressEvent {
                asset_id: id_text.clone(),
                stage: p.stage,
                fraction: p.fraction,
                detail: p.detail,
            },
        );
    };
    let refined = kerf_core::refine_transcript_media(&asset, read.clone(), &mut |p| emit(p), cancel);
    emit(kerf_core::AnalysisProgress::stage("done"));
    let refined = refined.map_err(|e| match e {
        kerf_core::Error::Cancelled => ANALYSIS_CANCELLED.to_string(),
        e => e.to_string(),
    })?;
    let stored = {
        let project = lock();
        // A project opened meanwhile is not told its asset was refined.
        require_generation(&project, generation, "the transcript was being refined")?;
        project
            .store_transcript(asset_id, &read, refined)
            .map_err(|e| e.to_string())?
    };
    let _ = app.emit("analysis-status", kerf_core::analysis_status(asset_id, Some(&stored)));
    Ok(stored)
}

/// Parse the `steps` an adapter was handed (`None` = the default set).
pub(crate) fn parse_steps(steps: Option<Vec<String>>) -> CmdResult<Option<Vec<kerf_core::AnalysisKind>>> {
    steps
        .map(|names| kerf_core::AnalysisKind::parse_list(&names))
        .transpose()
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn analyze_asset(
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
fn analysis_statuses(state: State<'_, AppState>) -> CmdResult<Vec<kerf_core::AnalysisStatus>> {
    warm_transcription_backend();
    state.project().analysis_statuses().map_err(|e| e.to_string())
}

/// Ask whether a speech-to-text backend exists, which the first time runs `ffmpeg -h filter=whisper`
/// (cached for the process, bounded at a minute) — and every analysis status reads it. So it is asked
/// here, **before a project lock is taken**, never inside one: a cold probe must not hold the shared
/// project for as long as an ffmpeg start takes on a slow disk. Setup warms it on a thread of its own.
pub(crate) fn warm_transcription_backend() {
    let _ = kerf_core::transcription_backend();
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

/// The error an abandoned model download returns, for the webview to stay quiet on.
const MODEL_CANCELLED: &str = "model download cancelled";

/// Download a speech model ahead of time, streaming `model-progress`.
///
/// Transcription downloads on demand anyway; this exists so the user can start
/// the (few hundred megabyte) fetch deliberately, and pick a model other than
/// the default, instead of discovering it mid-analysis. `cancel_model_download`
/// stops it, rejecting with `"model download cancelled"`.
#[tauri::command]
async fn download_speech_model(app: AppHandle, state: State<'_, AppState>, name: String) -> CmdResult<String> {
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
fn cancel_model_download(state: State<'_, AppState>) {
    state.model_cancel.stop();
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
    let cancel = state.voiceover_cancel.begin();
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
        kerf_core::prepare_voiceover(&voice, &mut on_progress, &|| cancel()).map_err(voiceover_err)?;
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
    let track = opt_id(track_id.as_deref(), "track_id")?;
    let shared = state.project.clone();
    let cancel = state.voiceover_cancel.begin();
    blocking(move || {
        let generation = lock_user(&shared).generation();
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
        let stop = stop_or_replaced(cancel, &shared, generation);
        let asset =
            Project::synthesize_voiceover(&text, &voice, speed.unwrap_or(1.0), &mut on_progress, &stop).map_err(voiceover_err)?;
        let project = lock_user(&shared);
        // Spoken for a project that is no longer open: the quiet stop of a cancel, not a failure.
        if project.generation() != generation {
            return Err(VOICEOVER_CANCELLED.to_string());
        }
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
    state.voiceover_cancel.stop();
}

// ---- music -------------------------------------------------------------------

/// Whether music can be composed here: the SoundFont's state, compositions in flight and the styles.
#[tauri::command(async)]
fn music_status() -> CmdResult<kerf_core::MusicStatus> {
    Ok(kerf_core::music_status())
}

/// The error an abandoned composition returns, for the webview to stay quiet on.
const MUSIC_CANCELLED: &str = "music cancelled";

fn music_err(e: kerf_core::Error) -> String {
    match e {
        kerf_core::Error::Cancelled => MUSIC_CANCELLED.to_string(),
        other => other.to_string(),
    }
}

/// Download the SoundFont ahead of time, streaming `music-progress` (`download_soundfont`), so the
/// first composition starts rendering at once.
#[tauri::command]
async fn prepare_music(app: AppHandle, state: State<'_, AppState>) -> CmdResult<kerf_core::MusicStatus> {
    let cancel = state.music_cancel.begin();
    blocking(move || {
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "music-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        kerf_core::prepare_soundfont(&mut on_progress, &|| cancel()).map_err(music_err)?;
        Ok(kerf_core::music_status())
    })
    .await
}

/// A composition landed: what [`kerf_core::Composed`] says, and the refreshed timeline.
#[derive(Serialize)]
struct ComposeResult {
    #[serde(flatten)]
    composed: kerf_core::Composed,
    timeline: Timeline,
}

/// Compose music from a score and put it on the timeline: on the `M` track unless `request.track_id`
/// names another audio track, at `request.at` or after what is already there — or, with
/// `request.replace_clip_id`, swapped into that composed clip. With `request.align` the score is
/// first fitted to the cut's picture (tempo nudged, hits added).
///
/// The request is checked and aligned under the project lock, rendering (and a first-use SoundFont
/// download) runs with it released, streaming `music-progress` (`download_soundfont`, `waiting`,
/// `render`); only landing the result takes it again.
#[tauri::command]
async fn compose_music(
    app: AppHandle,
    state: State<'_, AppState>,
    request: kerf_core::ComposeRequest,
) -> CmdResult<ComposeResult> {
    let shared = state.project.clone();
    let cancel = state.music_cancel.begin();
    blocking(move || {
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.compose_music_inputs(&request).map_err(music_err)?,
                project.generation(),
            )
        };
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "music-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        let asset = Project::synthesize_music(&plan.score, &mut on_progress, &stop).map_err(music_err)?;
        let project = lock_user(&shared);
        // Composed for a project that is no longer open: the quiet stop of a cancel, not a failure.
        if project.generation() != generation {
            return Err(MUSIC_CANCELLED.to_string());
        }
        let (asset, clip) = project
            .place_music(&asset, plan.track_id, plan.at, plan.replace_clip_id)
            .map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(ComposeResult {
            composed: kerf_core::Composed {
                asset,
                clip,
                score: plan.score,
                align: plan.align,
                replaced: plan.replaces,
            },
            timeline,
        })
    })
    .await
}

/// Request cancellation of the music being composed (or its SoundFont download).
#[tauri::command(async)]
fn cancel_music(state: State<'_, AppState>) {
    state.music_cancel.stop();
}

/// The error a stopped band-balance measurement returns, for the webview to stay quiet on.
const BAND_BALANCE_CANCELLED: &str = "band balance cancelled";

/// Where a stretch of sound keeps its energy — six bands per window, with RMS and loudness — for one
/// asset, one clip's source range or a range of the mix. The target and windows are resolved under
/// the project lock; the decode runs with it released and `cancel_band_balance` stops it.
#[tauri::command]
async fn get_band_balance(
    state: State<'_, AppState>,
    request: kerf_core::BandBalanceRequest,
) -> CmdResult<kerf_core::BandBalance> {
    let shared = state.project.clone();
    let cancel = state.bands_cancel.begin();
    blocking(move || {
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.band_balance_inputs(&request).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        Project::measure_band_balance(&plan, &stop).map_err(|e| match e {
            kerf_core::Error::Cancelled => BAND_BALANCE_CANCELLED.to_string(),
            other => other.to_string(),
        })
    })
    .await
}

/// Request cancellation of the band-balance measurement in flight (if any).
#[tauri::command(async)]
fn cancel_band_balance(state: State<'_, AppState>) {
    state.bands_cancel.stop();
}

/// Whether stem separation would still download its runtime or model.
#[tauri::command(async)]
fn stems_status() -> CmdResult<kerf_core::StemsStatus> {
    Ok(kerf_core::stems_status())
}

/// The error an abandoned separation returns, for the webview to stay quiet on.
const STEMS_CANCELLED: &str = "stems cancelled";

#[derive(Serialize)]
struct StemsResult {
    placed: kerf_core::StemsPlaced,
    timeline: Timeline,
}

/// Split an asset's sound into drums / bass / other / vocals, streaming
/// `stems-progress` (`download_runtime`, `download_model`, `separate`, `encode`), and
/// add them to the library — laid under `clip_id` on new tracks when given. The
/// separation runs with the project lock released; only landing it takes the lock.
#[tauri::command]
async fn separate_stems(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    clip_id: Option<String>,
) -> CmdResult<StemsResult> {
    let asset_id = id(&asset_id, "asset_id")?;
    let clip = opt_id(clip_id.as_deref(), "clip_id")?;
    let shared = state.project.clone();
    let cancel = state.stems_cancel.begin();
    blocking(move || {
        let (asset, generation) = {
            let project = lock_user(&shared);
            (
                project.require_asset(asset_id).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "stems-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        let stems = Project::separate_stems_media(&asset, &mut on_progress, &stop).map_err(|e| match e {
            kerf_core::Error::Cancelled => STEMS_CANCELLED.to_string(),
            other => other.to_string(),
        })?;
        let project = lock_user(&shared);
        // Separated for a project that is no longer open: the quiet stop of a cancel.
        if project.generation() != generation {
            return Err(STEMS_CANCELLED.to_string());
        }
        let placed = project.place_stems(asset_id, &stems, clip).map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(StemsResult { placed, timeline })
    })
    .await
}

/// Request cancellation of the running stem separation (or its download).
#[tauri::command(async)]
fn cancel_stems(state: State<'_, AppState>) {
    state.stems_cancel.stop();
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
    let id = id(&asset_id, "asset_id")?;
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
    let asset = id(&asset_id, "asset_id")?;
    let track = opt_id(track_id.as_deref(), "track_id")?;
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
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.trim(id, source_in, source_out, timeline_start))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn reorder_clip(state: State<'_, AppState>, track_id: String, clip_id: String, new_index: usize) -> CmdResult<Timeline> {
    let track = id(&track_id, "track_id")?;
    let clip = id(&clip_id, "clip_id")?;
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
    let clip = id(&clip_id, "clip_id")?;
    let track = opt_id(track_id.as_deref(), "track_id")?;
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
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.with_links(link, |p| p.ripple_delete(id)).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn cut_clip_range(state: State<'_, AppState>, clip_id: String, from: f64, to: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
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
    let (a, b) = (id(&clip_a, "clip_a")?, id(&clip_b, "clip_b")?);
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
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.remove_track(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_track_duck(state: State<'_, AppState>, track_id: String, duck: bool) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_duck(id, duck).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set a track's fader — the gain riding every clip on the track.
#[tauri::command(async)]
fn set_track_volume(state: State<'_, AppState>, track_id: String, volume: f32) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_volume(id, volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set a track's stereo placement, -1 (hard left) to 1 (hard right).
#[tauri::command(async)]
fn set_track_pan(state: State<'_, AppState>, track_id: String, pan: f32) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
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

/// Make the master limiter hold the true peak (4x oversampled) or the sample peak.
#[tauri::command(async)]
fn set_master_true_peak(state: State<'_, AppState>, enabled: bool) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_true_peak(enabled).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Choose how ducked tracks dip: `depth_db` (negative dB) for the speech gate, omitted
/// for the sidechain compressor.
#[tauri::command(async)]
fn set_master_duck(state: State<'_, AppState>, depth_db: Option<f64>) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_duck(depth_db).map_err(|e| e.to_string())?;
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
    let cancel = state.levels_cancel.begin();
    blocking(move || {
        let ((timeline, assets), generation) = {
            let project = lock_user(&shared);
            (project.levels_inputs().map_err(|e| e.to_string())?, project.generation())
        };
        let levels =
            Project::measure_levels(&timeline, &assets, range, loudnorm.unwrap_or(false), &|| cancel()).map_err(|e| match e {
                kerf_core::Error::Cancelled => LEVELS_CANCELLED.to_string(),
                other => other.to_string(),
            })?;
        // The levels of a cut that is no longer the open one are of no use to the one that is.
        if lock_user(&shared).generation() != generation {
            return Err(LEVELS_CANCELLED.to_string());
        }
        Ok(levels)
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
    state.levels_cancel.stop();
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
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_muted(id, muted).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_track_solo(state: State<'_, AppState>, track_id: String, solo: bool) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_solo(id, solo).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_track_locked(state: State<'_, AppState>, track_id: String, locked: bool) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_locked(id, locked).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_clip_enabled(state: State<'_, AppState>, clip_id: String, enabled: bool) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.detach_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// **Detach audio** from several picture clips in **one** revision. A clip that
/// cannot be detached is skipped and reported; an error only when none could be.
#[tauri::command(async)]
fn detach_audio_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<AudioDetached> {
    let ids = ids(&clip_ids, "clip_ids")?;
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
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.reattach_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// **Reattach audio** on several pictures in **one** revision, all or nothing: name
/// either clip of each pair; a pair that cannot be reattached refuses the lot.
#[tauri::command(async)]
fn reattach_audio_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.reattach_audio_clips(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Link clips (at least two, on different tracks) so an edit to one is carried to
/// the others. One revision.
#[tauri::command(async)]
fn link_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.link_clips(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Unlink clips; a group left with a single clip dissolves. One revision.
#[tauri::command(async)]
fn unlink_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
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
        .map(|p| id(&p.track_id, "placements[].track_id").map(|t| (t, p.clip)))
        .collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project.insert_clips(&items, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn duplicate_clips(state: State<'_, AppState>, clip_ids: Vec<String>, at: f64) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.duplicate_clips(&ids, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Remove a clip — and its linked partners, unless `link` is `false`.
#[tauri::command(async)]
fn remove_clip(state: State<'_, AppState>, clip_id: String, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
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
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project
        .with_links(link, |p| p.with_ripple(ripple, |p| p.remove_clips(&ids)))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_volume(state: State<'_, AppState>, clip_id: String, volume: f32) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_volume(id, volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_fade(state: State<'_, AppState>, clip_id: String, fade_in: Option<f64>, fade_out: Option<f64>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_fade(id, fade_in, fade_out).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Retime a clip; its linked partners are retimed by the same ratio unless `link`
/// is `false`.
#[tauri::command(async)]
fn set_speed(state: State<'_, AppState>, clip_id: String, speed: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
    let transition = parse_transition(kind, duration)?;
    let project = state.project();
    project.set_transition(id, transition).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Cut a clip to a shape (or clear it with `mask: null`), so a lower track shows
/// through outside it.
#[tauri::command(async)]
fn set_mask(state: State<'_, AppState>, clip_id: String, mask: Option<Mask>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_mask(id, mask).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Replace a clip's curves, colour wheels and LUT. A LUT file is read and checked first, with the
/// project unlocked.
#[tauri::command]
async fn set_grade(state: State<'_, AppState>, clip_id: String, grade: Grade) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Only a newly chosen LUT file is checked, so a curve or wheel edit still applies to a
        // clip whose LUT went missing.
        let before = {
            let timeline = lock_user(&shared).timeline().map_err(|e| e.to_string())?;
            timeline.clip(id).and_then(|c| c.grade.lut.as_ref().map(|l| l.path.clone()))
        };
        if grade.lut.as_ref().is_some_and(|l| Some(&l.path) != before.as_ref()) {
            kerf_core::color_lut::check_grade(&grade).map_err(|e| e.to_string())?;
        }
        let project = lock_user(&shared);
        project.set_grade(id, grade).map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

/// What a `.cube` file holds (its size and title), or why it cannot be used as a LUT.
#[tauri::command]
async fn inspect_lut(path: String) -> CmdResult<kerf_core::color_lut::LutInfo> {
    blocking(move || kerf_core::color_lut::inspect_lut(&path).map_err(|e| e.to_string())).await
}

#[tauri::command(async)]
fn set_video_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<VideoEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_video_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_audio_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<AudioEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_audio_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_keyframes(state: State<'_, AppState>, clip_id: String, keyframes: Vec<Keyframe>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .add_keyframe(id, time, scale, pos_x, pos_y, rotation, opacity)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// With `prop`, one number's segment alone; without, the transform key at `time` (the
/// bundle's and every transform number's own track).
#[tauri::command(async)]
fn set_keyframe_easing(
    state: State<'_, AppState>,
    clip_id: String,
    time: f64,
    easing: kerf_core::Easing,
    prop: Option<Property>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    match prop {
        Some(prop) => project.set_property_easing(id, prop, time, easing),
        None => project.set_keyframe_easing(id, time, easing),
    }
    .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Replace the keys of one animatable number — a transform number, a colour number or the
/// clip's volume. No keys leaves it static.
#[tauri::command(async)]
fn set_property_keyframes(
    state: State<'_, AppState>,
    clip_id: String,
    prop: Property,
    keys: Vec<PropertyKey>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_property_keyframes(id, prop, keys).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Copy the animation of `props` (every keyed number when empty) from one clip to another,
/// `offset` seconds later.
#[tauri::command(async)]
fn copy_keyframes(
    state: State<'_, AppState>,
    from_clip_id: String,
    to_clip_id: String,
    props: Option<Vec<Property>>,
    offset: Option<f64>,
) -> CmdResult<Timeline> {
    let (from, to) = (id(&from_clip_id, "from_clip_id")?, id(&to_clip_id, "to_clip_id")?);
    let project = state.project();
    project
        .copy_keyframes(from, to, &props.unwrap_or_default(), offset.unwrap_or(0.0))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn clear_keyframes(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .set_reframe(id, yaw, pitch, roll, fov, lens_fov, input, output)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Mark an asset as 360 footage (or clear the mark) for footage the probe could
/// not identify. Unlike `set_reframe` this sticks to the asset, so every clip cut
/// from it afterwards is reframed.
#[tauri::command]
async fn set_asset_projection(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    projection: Option<Projection>,
) -> CmdResult<Asset> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    // On the blocking pool: queueing the proxy runs the cached ffprobe that keys it.
    blocking(move || {
        let asset = lock_user(&shared)
            .set_asset_projection(id, projection)
            .map_err(|e| e.to_string())?;
        // 360 assets proxy at a different size, so the cached proxy no longer matches.
        spawn_proxy(&app, &asset);
        Ok(asset)
    })
    .await
}

#[tauri::command(async)]
fn clear_reframe(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.clear_reframe(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_reframe_keyframes(state: State<'_, AppState>, clip_id: String, keyframes: Vec<ReframeKeyframe>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
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
    let id = id(&clip_id, "clip_id")?;
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
#[allow(clippy::too_many_arguments)]
fn update_marker(
    state: State<'_, AppState>,
    marker_id: String,
    time: Option<f64>,
    name: Option<String>,
    color: Option<String>,
    duration: Option<f64>,
    comment: Option<String>,
    kind: Option<kerf_core::MarkerKind>,
) -> CmdResult<Timeline> {
    let id = id(&marker_id, "marker_id")?;
    let patch = kerf_core::MarkerPatch {
        time,
        name,
        color,
        duration,
        comment,
        kind,
    };
    let project = state.project();
    project.edit_marker(id, patch).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn remove_marker(state: State<'_, AppState>, marker_id: String) -> CmdResult<Timeline> {
    let id = id(&marker_id, "marker_id")?;
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
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project
        .update_overlay(oid, text, start, end, pos_x, pos_y, size, color, bg, font, bold)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn remove_overlay(state: State<'_, AppState>, overlay_id: String) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project.remove_overlay(oid).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn set_overlay_keyframes(state: State<'_, AppState>, overlay_id: String, keyframes: Vec<TextKeyframe>) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project.set_overlay_keyframes(oid, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Reading the artwork (sanitise + parse) takes up to a second and a half for a heavy document, so
/// it runs on the blocking pool *before* the project lock is taken; the lock then only places it.
#[tauri::command]
async fn add_svg_overlay(
    state: State<'_, AppState>,
    source: SvgSource,
    start: f64,
    end: Option<f64>,
    layout: Option<SvgLayout>,
) -> CmdResult<Timeline> {
    let shared = state.project.clone();
    blocking(move || {
        let checked = source.check().map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        project
            .add_svg_overlay_parsed(checked, start, end, layout.unwrap_or_default())
            .map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
async fn update_svg_overlay(state: State<'_, AppState>, overlay_id: String, patch: SvgOverlayPatch) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let doc = patch.check().map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        project
            .update_svg_overlay_parsed(oid, patch, doc)
            .map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command(async)]
fn remove_svg_overlay(state: State<'_, AppState>, overlay_id: String) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project.remove_svg_overlay(oid).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// What `add_svg_overlay` / `update_svg_overlay` would make of this source: the sanitised
/// document, what the sanitiser or the SMIL reader could not honour, and how long it runs.
/// Needs no project, so it never waits for the lock, and parses on the blocking pool.
#[tauri::command]
async fn validate_svg(svg: String) -> CmdResult<SvgCheck> {
    blocking(move || kerf_core::check_svg(&svg).map_err(|e| e.to_string())).await
}

#[tauri::command(async)]
fn list_svg_presets() -> Vec<SvgPresetInfo> {
    kerf_core::svg::preset_infos()
}

/// The artwork a preset would make for the project's delivery frame, as SVG text (the page shows
/// it through an `<img>` data URL, never inline). `duration` defaults to 4 s.
#[tauri::command(async)]
fn svg_preset_preview(
    state: State<'_, AppState>,
    kind: SvgPresetKind,
    params: SvgPresetParams,
    duration: Option<f64>,
) -> CmdResult<String> {
    kerf_core::svg::check_params(&params).map_err(|e| e.to_string())?;
    let frame = state.project().timeline().map_err(|e| e.to_string())?.preset_frame();
    Ok(kerf_core::render_preset(kind, &params, duration.unwrap_or(4.0), frame))
}

// ---- motion templates --------------------------------------------------------

/// The user's template library (`KERF_TEMPLATES_DIR` or the data directory). Library calls need no
/// project, so none of them takes the lock.
fn template_library() -> CmdResult<TemplateLibrary> {
    TemplateLibrary::default_location().map_err(|e| e.to_string())
}

/// An image file as the `data:` URL an `image` control takes (`kerf_core::template::image_data_url`
/// judges the picture; this only reads the file, bounded). Shared with the MCP tools.
///
/// The bound is the one the engine holds a picture to ([`kerf_core::template::MAX_IMAGE_BYTES`], a little
/// under 96 KB: a title is at most 128 KiB), and it is kept by the read itself rather than by a `stat`
/// that the file could outgrow: the file is opened, looked at through the handle it was opened as, and read
/// at most one byte past the bound.
pub(crate) fn template_image_data_url(path: &str) -> kerf_core::Result<String> {
    use kerf_core::Error;
    use std::io::Read;
    let refuse = |why: String| Error::InvalidArgument(why);
    let limit = kerf_core::template::MAX_IMAGE_BYTES as u64;
    let file = std::path::Path::new(path);
    if !file.is_absolute() {
        return Err(refuse(format!("{path} must be an absolute file path")));
    }
    // Not opened if it is not a regular file at all: opening a named pipe would wait for a writer.
    let meta = std::fs::metadata(file).map_err(|e| refuse(format!("cannot read {path}: {e}")))?;
    if !meta.is_file() {
        return Err(refuse(format!("{path} is not a regular file")));
    }
    let handle = std::fs::File::open(file).map_err(|e| refuse(format!("cannot read {path}: {e}")))?;
    let meta = handle.metadata().map_err(|e| refuse(format!("cannot read {path}: {e}")))?;
    if !meta.is_file() {
        return Err(refuse(format!("{path} is not a regular file")));
    }
    let too_big = |bytes: u64| {
        refuse(format!(
            "{path} is {} KiB: too large for an image in a title (at most {} KiB)",
            bytes.div_ceil(1024),
            limit / 1024
        ))
    };
    if meta.len() > limit {
        return Err(too_big(meta.len()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    handle
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| refuse(format!("cannot read {path}: {e}")))?;
    if bytes.len() as u64 > limit {
        return Err(too_big(bytes.len() as u64));
    }
    kerf_core::template::image_data_url(&bytes)
}

/// Only `.kerftemplate` files are read as templates, and the path is the one a file picker returned.
fn require_template_path(path: &str) -> CmdResult<()> {
    let is_template = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(kerf_core::template::EXTENSION));
    if is_template {
        Ok(())
    } else {
        Err(format!("{path} is not a .{} file", kerf_core::template::EXTENSION))
    }
}

/// What applying a template or changing the values of a template title hands back: the refreshed cut,
/// the title that was made or changed, and what the sanitiser or the machine could not honour (a font
/// that is not installed, ...).
#[derive(Serialize)]
struct TemplateEdit {
    timeline: Timeline,
    /// The floating title made or changed; absent when it is a title on a track (`clip_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    overlay_id: Option<Uuid>,
    /// The title clip made or changed, when the title is on a track.
    #[serde(skip_serializing_if = "Option::is_none")]
    clip_id: Option<Uuid>,
    warnings: Vec<String>,
}

impl TemplateEdit {
    fn of_clip(timeline: Timeline, done: TitleClipResult) -> Self {
        Self {
            timeline,
            overlay_id: None,
            clip_id: Some(done.clip.id),
            warnings: done.warnings,
        }
    }
}

/// Where a new title clip goes: a video track (by default the lowest one free over its span, else a
/// new one on top) and its name.
fn title_placement(track_id: Option<String>, name: Option<String>) -> CmdResult<TitleClipPlacement> {
    Ok(TitleClipPlacement {
        track_id: track_id.as_deref().map(|t| id(t, "track_id")).transpose()?,
        name,
    })
}

/// An element of a title that has an `id`, and the tag it is: what a control can aim at.
#[derive(Serialize)]
struct SvgElementId {
    id: String,
    tag: String,
}

/// Every template in the library (by name) and the files that did not read. Only manifests are read;
/// a poster is fetched on its own (`template_poster`).
#[tauri::command]
async fn list_templates() -> CmdResult<TemplateList> {
    blocking(|| template_library()?.list().map_err(|e| e.to_string())).await
}

/// A template's poster as a `data:image/png;base64,` URL (a PNG of at most 512 px), `null` when it has
/// none.
#[tauri::command]
async fn template_poster(template_id: String) -> CmdResult<Option<String>> {
    blocking(move || {
        let poster = template_library()?.poster(&template_id).map_err(|e| e.to_string())?;
        Ok(poster.map(|png| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )
        }))
    })
    .await
}

/// The `id`s of a title's elements and their tags, for the save dialog to offer as targets. Pure.
#[tauri::command(async)]
fn svg_element_ids(svg: String) -> CmdResult<Vec<SvgElementId>> {
    let ids = kerf_core::template::element_ids(&svg).map_err(|e| e.to_string())?;
    Ok(ids.into_iter().map(|(id, tag)| SvgElementId { id, tag }).collect())
}

/// An image file as the `data:` URL a template `image` value takes (PNG, JPEG, GIF or WebP).
#[tauri::command]
async fn template_image_value(path: String) -> CmdResult<String> {
    blocking(move || template_image_data_url(&path).map_err(|e| e.to_string())).await
}

/// Keep the title `overlay_id` (as the working cut has it) in the library as a template with the given
/// controls. The title is read under the lock; the fonts, the sanitiser and the poster run with it
/// released. A library write, not a timeline edit: nothing is undone or staged.
#[tauri::command]
async fn save_template(
    state: State<'_, AppState>,
    overlay_id: String,
    name: String,
    description: Option<String>,
    controls: Vec<TemplateControl>,
) -> CmdResult<TemplateSaved> {
    let oid = id(&overlay_id, "overlay_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let source = lock_user(&shared).template_source(oid).map_err(|e| e.to_string())?;
        let request = TemplateSaveRequest {
            name,
            description: description.unwrap_or_default(),
            controls,
        };
        template_library()?.save(&source, request).map_err(|e| e.to_string())
    })
    .await
}

/// Add a library template to the timeline as an animated title: `values` (control id to value, partial)
/// are spliced into its artwork and the result sanitised **before** the project lock is taken; the lock
/// only places the title and stores the template's fonts in the project. `end` defaults to `start` plus
/// the template's duration; `layout` places and names it like `add_svg_overlay`'s.
#[tauri::command]
async fn apply_template(
    state: State<'_, AppState>,
    template_id: String,
    values: std::collections::BTreeMap<String, String>,
    start: f64,
    end: Option<f64>,
    layout: Option<SvgLayout>,
) -> CmdResult<TemplateEdit> {
    let shared = state.project.clone();
    blocking(move || {
        let prepared = template_library()?
            .get(&template_id)
            .and_then(|t| t.prepare(&values))
            .map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project
            .apply_template_parsed(prepared, start, end, layout.unwrap_or_default())
            .map_err(|e| e.to_string())?;
        Ok(TemplateEdit {
            timeline: project.timeline().map_err(|e| e.to_string())?,
            overlay_id: Some(done.overlay.id),
            clip_id: None,
            warnings: done.warnings,
        })
    })
    .await
}

/// Add a library template as a title clip on a video track, so it takes transitions, fades and keys
/// like any clip: prepared before the lock, as `apply_template`. It runs from `start` for `duration`
/// seconds (the template's own length by default) on `track_id` (by default the lowest video track
/// free over its span, else a new one on top).
#[tauri::command]
async fn apply_template_clip(
    state: State<'_, AppState>,
    template_id: String,
    values: std::collections::BTreeMap<String, String>,
    start: f64,
    duration: Option<f64>,
    track_id: Option<String>,
    name: Option<String>,
) -> CmdResult<TemplateEdit> {
    let placement = title_placement(track_id, name)?;
    let shared = state.project.clone();
    blocking(move || {
        let prepared = template_library()?
            .get(&template_id)
            .and_then(|t| t.prepare(&values))
            .map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project
            .apply_template_clip_parsed(prepared, start, duration, placement)
            .map_err(|e| e.to_string())?;
        Ok(TemplateEdit::of_clip(project.timeline().map_err(|e| e.to_string())?, done))
    })
    .await
}

/// Add custom SVG artwork as a title clip on a video track (read before the lock). `duration` defaults
/// to how long its animations run (2–30 s, else 4).
#[tauri::command]
async fn add_title_clip(
    state: State<'_, AppState>,
    svg: String,
    start: f64,
    duration: Option<f64>,
    track_id: Option<String>,
    name: Option<String>,
) -> CmdResult<TemplateEdit> {
    let placement = title_placement(track_id, name)?;
    let shared = state.project.clone();
    blocking(move || {
        let doc = kerf_core::SvgDoc::parse(&svg).map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project
            .add_title_clip_parsed(doc, start, duration, placement)
            .map_err(|e| e.to_string())?;
        Ok(TemplateEdit::of_clip(project.timeline().map_err(|e| e.to_string())?, done))
    })
    .await
}

/// Rename a title clip or replace its artwork (`svg`, read before the lock; it ends a template). Its
/// timing, place and look are the clip's own edits.
#[tauri::command]
async fn update_title_clip(state: State<'_, AppState>, clip_id: String, patch: TitleClipPatch) -> CmdResult<TemplateEdit> {
    let cid = id(&clip_id, "clip_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let doc = patch
            .svg
            .as_deref()
            .map(kerf_core::SvgDoc::parse)
            .transpose()
            .map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project.update_title_clip_parsed(cid, patch, doc).map_err(|e| e.to_string())?;
        Ok(TemplateEdit::of_clip(project.timeline().map_err(|e| e.to_string())?, done))
    })
    .await
}

/// Change control values of a template title, floating or on a track (partial: only the named
/// controls; `overlay_id` is either's id). The title is read under the lock, the splice and the
/// sanitiser run with it released, and the result is stored only if the title is still the one it was
/// made from; one that changed meanwhile is an error saying to try again.
#[tauri::command]
async fn set_template_values(
    state: State<'_, AppState>,
    overlay_id: String,
    values: std::collections::BTreeMap<String, String>,
) -> CmdResult<TemplateEdit> {
    let oid = id(&overlay_id, "overlay_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let inputs = lock_user(&shared).template_edit_inputs(oid).map_err(|e| e.to_string())?;
        let prepared = inputs.prepare(&values).map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project.set_title_values_parsed(oid, prepared).map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(match done {
            TitleEdited::Overlay(o) => TemplateEdit {
                timeline,
                overlay_id: Some(o.overlay.id),
                clip_id: None,
                warnings: o.warnings,
            },
            TitleEdited::Clip(c) => TemplateEdit::of_clip(timeline, *c),
        })
    })
    .await
}

/// Bring a `.kerftemplate` file into the library. Never overwrites: the same template is
/// `already_present`, another one with the same id is kept under a new id (`added_as_copy`). The
/// path is the one the file picker returned: an absolute path to a regular `.kerftemplate` file.
#[tauri::command]
async fn import_template(path: String) -> CmdResult<ImportOutcome> {
    require_template_path(&path)?;
    blocking(move || {
        let file = std::path::Path::new(&path);
        kerf_core::paths::validate_import_path(file).map_err(|e| e.to_string())?;
        template_library()?.import(file).map_err(|e| e.to_string())
    })
    .await
}

/// Write a library template to `path` (a directory gets `<name>.kerftemplate` in it, a path with no
/// extension gets `.kerftemplate`); resolves to where it went. The picker has asked about replacing.
/// Not onto a file the project still needs, into Kerf's own settings and logs, or into the library.
#[tauri::command]
async fn export_template(state: State<'_, AppState>, template_id: String, path: String) -> CmdResult<String> {
    require_user_output_path(&path)?;
    let shared = state.project.clone();
    blocking(move || {
        let library = template_library()?;
        let mut protected = lock_user(&shared).output_path_inputs().map_err(|e| e.to_string())?;
        protected
            .dirs
            .push((library.dir().to_path_buf(), "the template library".to_string()));
        check_output(std::path::Path::new(&path), &protected).map_err(|e| e.to_string())?;
        let written = library
            .export(&template_id, std::path::Path::new(&path))
            .map_err(|e| e.to_string())?;
        Ok(written.to_string_lossy().into_owned())
    })
    .await
}

/// Remove a template from the library. Titles already made from it keep working: they carry their own
/// artwork and fonts.
#[tauri::command]
async fn delete_template(template_id: String) -> CmdResult<()> {
    blocking(move || template_library()?.delete(&template_id).map_err(|e| e.to_string())).await
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
    let asset = opt_id(asset_id, "asset_id")?;
    Ok(CaptionImportRequest {
        base: CaptionTimeBase::resolve(base, asset).map_err(|e| e.to_string())?,
        options: options.unwrap_or_default(),
        offset: offset.unwrap_or(0.0),
    })
}

/// The part of an import that holds the project lock: place a file that was
/// already read and parsed with the lock released.
fn place_caption_file(
    shared: &Mutex<Project>,
    generation: u64,
    file: &CaptionFile,
    req: CaptionImportRequest,
) -> CmdResult<CaptionImport> {
    let project = lock_user(shared);
    require_generation(&project, generation, "captions were being imported")?;
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
        let generation = lock_user(&shared).generation();
        let text = kerf_core::read_caption_file(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
        let file = kerf_core::parse_captions(&text, None).map_err(|e| e.to_string())?;
        place_caption_file(&shared, generation, &file, req)
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
        let generation = lock_user(&shared).generation();
        let file = kerf_core::parse_captions(&text, format).map_err(|e| e.to_string())?;
        place_caption_file(&shared, generation, &file, req)
    })
    .await
}

#[tauri::command]
async fn export_srt(state: State<'_, AppState>, asset_id: String, output_path: String) -> CmdResult<String> {
    let id = id(&asset_id, "asset_id")?;
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    blocking(move || {
        let (protected, srt) = {
            let project = lock_user(&shared);
            (
                project.output_path_inputs().map_err(|e| e.to_string())?,
                project.transcript_srt(id).map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        std::fs::write(&output_path, srt).map_err(|e| e.to_string())?;
        Ok(output_path)
    })
    .await
}

/// Write the cut's captions (its generated overlays, as the export draws them)
/// to `output_path` as SubRip or WebVTT, by `format` or the path's extension.
#[tauri::command]
async fn export_captions(
    state: State<'_, AppState>,
    output_path: String,
    format: Option<kerf_core::SubtitleFormat>,
) -> CmdResult<String> {
    require_user_output_path(&output_path)?;
    let path = std::path::PathBuf::from(&output_path);
    let format = match format {
        Some(f) => f,
        None => kerf_core::SubtitleFormat::from_path(&path).map_err(|e| e.to_string())?,
    };
    let shared = state.project.clone();
    blocking(move || {
        let (timeline, protected) = {
            let project = lock_user(&shared);
            (
                project.working_timeline().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        check_output(&path, &protected).map_err(|e| e.to_string())?;
        let text = Project::captions_document(&timeline, format).map_err(|e| e.to_string())?;
        Project::write_document(&path, &text).map_err(|e| e.to_string())?;
        Ok(output_path)
    })
    .await
}

/// Write the cut as OpenTimelineIO or a CMX 3600 EDL (`video_track`: which video
/// track an EDL holds, 0 = V1), returning the path and what the format dropped.
#[tauri::command]
async fn export_interchange(
    state: State<'_, AppState>,
    output_path: String,
    format: kerf_core::InterchangeFormat,
    video_track: Option<usize>,
) -> CmdResult<kerf_core::ExportedFile> {
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    blocking(move || {
        let (inputs, protected) = {
            let project = lock_user(&shared);
            (
                project.delivery_inputs().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        let (text, report) =
            Project::interchange_document(&inputs, format, video_track.unwrap_or(0)).map_err(|e| e.to_string())?;
        Project::write_document(std::path::Path::new(&output_path), &text).map_err(|e| e.to_string())?;
        Ok(kerf_core::ExportedFile {
            path: output_path,
            report,
        })
    })
    .await
}

/// Write the markers as a CSV (formula-safe) or as YouTube chapter text.
#[tauri::command]
async fn export_markers(
    state: State<'_, AppState>,
    output_path: String,
    format: kerf_core::MarkerListFormat,
) -> CmdResult<kerf_core::ExportedFile> {
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    blocking(move || {
        let (inputs, protected) = {
            let project = lock_user(&shared);
            (
                project.delivery_inputs().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        let (text, report) = Project::markers_document(&inputs, format).map_err(|e| e.to_string())?;
        Project::write_document(std::path::Path::new(&output_path), &text).map_err(|e| e.to_string())?;
        Ok(kerf_core::ExportedFile {
            path: output_path,
            report,
        })
    })
    .await
}

/// Re-time an asset's cached transcript to the speech energy and label its speakers, without
/// re-transcribing ([`run_refine`]). Stopped by `cancel_analysis`, like an analysis pass.
#[tauri::command]
async fn refine_transcript(app: AppHandle, state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetAnalysis> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    let cancel = state.begin_analysis();
    blocking(move || run_refine(&app, &|| lock_user(&shared), id, &|| cancel())).await
}

#[tauri::command(async)]
fn remove_silence(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id, "asset_id")?;
    let project = state.project();
    project.remove_silence(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn snap_to_beats(state: State<'_, AppState>, track_id: Option<String>, tolerance: Option<f64>) -> CmdResult<Timeline> {
    let track = opt_id(track_id.as_deref(), "track_id")?;
    let project = state.project();
    project.snap_to_beats(track, tolerance).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Plan "fit music to length" for a music clip without changing anything. `target`
/// defaults to the picture's length from the clip's start.
#[tauri::command(async)]
fn plan_music_fit(state: State<'_, AppState>, clip_id: String, target: Option<f64>) -> CmdResult<kerf_core::model::MusicFit> {
    let clip = id(&clip_id, "clip_id")?;
    state.project().plan_music_fit(clip, target).map_err(|e| e.to_string())
}

/// What fitting music did: the refreshed timeline and the report.
#[derive(serde::Serialize)]
struct MusicFitted {
    timeline: Timeline,
    report: kerf_core::model::MusicFitReport,
}

/// Fit a music clip to `target` (default: the picture's length from its start) with
/// bar-aligned phrase splices, fading out an overrun when `fade_out`.
#[tauri::command(async)]
fn fit_music(state: State<'_, AppState>, clip_id: String, target: Option<f64>, fade_out: bool) -> CmdResult<MusicFitted> {
    let clip = id(&clip_id, "clip_id")?;
    let project = state.project();
    let report = project.fit_music(clip, target, fade_out).map_err(|e| e.to_string())?;
    let timeline = project.timeline().map_err(|e| e.to_string())?;
    Ok(MusicFitted { timeline, report })
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
    let id = id(&asset_id, "asset_id")?;
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
    let id = id(&asset_id, "asset_id")?;
    let project = state.project();
    project.add_asset_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn concatenate(state: State<'_, AppState>, asset_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&asset_ids, "asset_ids")?;
    let project = state.project();
    project.concatenate(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

// ---- history (undo / redo / revert) ----------------------------------------

/// The edit history, oldest first. `limit` / `offset` window it from the newest end: `offset`
/// of the latest revisions are skipped and up to `limit` of the ones before them returned, so
/// `limit: 50` is the last fifty edits and a following `offset: 50` the fifty before. Neither
/// is everything.
#[tauri::command(async)]
fn get_history(state: State<'_, AppState>, limit: Option<usize>, offset: Option<usize>) -> CmdResult<Vec<Revision>> {
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

/// Whether the open proposal also marks assets (a 360 projection): applying it changes the
/// footage, so their preview proxies have to be made again.
fn staged_asset_changes(project: &Project) -> CmdResult<bool> {
    project
        .staged()
        .map(|staged| staged.is_some_and(|staged| !staged.asset_changes.is_empty()))
        .map_err(|e| e.to_string())
}

/// Accept the whole proposal: it becomes the live timeline as one revision. `force` applies
/// one that is `stale` (the cut moved on since it branched), replacing the newer work.
#[tauri::command(async)]
fn apply_staged_edit(app: AppHandle, state: State<'_, AppState>, force: Option<bool>) -> CmdResult<Timeline> {
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
fn apply_staged_subset(
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
fn discard_staged_edit(state: State<'_, AppState>) -> CmdResult<Timeline> {
    state.project().discard_staged().map_err(|e| e.to_string())
}

/// What taking back a staged step left: its label, and the proposal as it now stands.
#[derive(Serialize)]
struct StagedUndone {
    label: String,
    staged: StagedEdit,
}

/// Take back the latest edit the agent staged, inside the proposal (the live cut is untouched).
#[tauri::command(async)]
fn undo_staged_edit(state: State<'_, AppState>) -> CmdResult<StagedUndone> {
    let (label, staged) = state.project().undo_staged().map_err(|e| e.to_string())?;
    Ok(StagedUndone { label, staged })
}

/// The proposal the user last discarded, while it can still be brought back; `None` once a
/// new one opened or nothing was discarded.
#[tauri::command(async)]
fn discarded_staged_edit(state: State<'_, AppState>) -> CmdResult<Option<StagedEdit>> {
    state.project().discarded_staged().map_err(|e| e.to_string())
}

/// Bring the discarded proposal back (only while no other proposal is open).
#[tauri::command(async)]
fn restore_discarded_edits(state: State<'_, AppState>) -> CmdResult<StagedEdit> {
    state.project().restore_discarded().map_err(|e| e.to_string())
}

// ---- media (preview frames, waveforms) -------------------------------------

/// While the preview source is **Proxy only**, the preview never decodes an original: a frame of
/// (or playback over) clips whose proxy is not ready is refused with what it is waiting for.
/// Under any other source this is nothing. The page keeps the last good frame and says what the
/// preview is waiting on from the proxy statuses it already holds.
fn refuse_unless_proxied(project: &Project, from: f64, to: Option<f64>) -> CmdResult<()> {
    let waits = project.proxy_waits(from, to).map_err(|e| e.to_string())?;
    match kerf_core::proxy::waits_message(&waits) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

#[tauri::command]
async fn get_frame(
    state: State<'_, AppState>,
    asset_id: String,
    time_secs: f64,
    max_width: Option<u32>,
    accurate: Option<bool>,
) -> CmdResult<String> {
    let id = id(&asset_id, "asset_id")?;
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
    let scrub = state.scrub.clone();
    blocking(move || {
        // Resolve the inputs under the lock, then *drop the guard* before the ffmpeg
        // composite — the preview fetches frames continuously during playback, and
        // holding the shared Project mutex for the whole decode would freeze every
        // other op (timeline edits, MCP, the next scrub frame). Mirrors `get_frame`.
        let (timeline, assets) = {
            let project = lock_user(&shared);
            refuse_unless_proxied(&project, time_secs, Some(time_secs))?;
            project.timeline_frame_inputs().map_err(|e| e.to_string())?
        };
        // Through the scrub cache: the frame of the cut's grid that holds the time, from the cache,
        // or from the window of neighbours one process draws for it, or alone (`ScrubFrames`).
        let jpeg = scrub
            .frame(&timeline, &assets, time_secs, max_width.unwrap_or(960), 4)
            .map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg.as_slice());
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
///
/// **Latest wins**: a request does not wait behind the ones before it. It replaces the one waiting
/// (that call answers `renderer: "superseded"` at once, with nothing drawn) and is drawn when the
/// frame in hand is done, so a drag can post a request per playhead move without queueing them. The
/// frame's inputs are a snapshot of the project (`gpu_preview::snapshot`): the project lock is taken
/// only when the plan epoch says something changed, never per frame.
///
/// `client` and `seq` say which request of which page this is: Tauri runs each call as a task of
/// its own, so two can arrive in the other order than they were made, and one that arrives after a
/// newer request of the same `client` answers `superseded` instead of replacing it.
///
/// `hint: "forward"` (with `client` and a `session` number) is a frame of **GPU playback**: the page
/// asks for the time the picture should show, once per animation frame while the cut plays at 1x,
/// and the first request of a session decides whether the GPU plays it (`renderer: "gpu"`) or the
/// stream does (`renderer: "stream"`, with the reasons); every answer says in `play.ahead` where the
/// GPU will stop drawing, if it will. See [`gpu_preview::play`]'s module docs; no other hint is known.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn get_preview_frame(
    state: State<'_, AppState>,
    gpu: State<'_, Arc<gpu_preview::GpuPreview>>,
    time_secs: f64,
    max_width: Option<u32>,
    overlays: Option<bool>,
    client: Option<String>,
    seq: Option<u64>,
    hint: Option<String>,
    session: Option<u64>,
) -> CmdResult<gpu_preview::PreviewFrameResult> {
    let shared = state.project.clone();
    let mut request = gpu_preview::FrameRequest::new(
        Arc::new(move || {
            let project = lock_user(&shared);
            gpu_preview::take_inputs(&project)
        }),
        time_secs,
        max_width.unwrap_or(960),
        overlays.unwrap_or(false),
    );
    match hint.as_deref() {
        None => {}
        Some("forward") => {
            let (Some(client), Some(session)) = (client.clone(), session) else {
                return Err("a forward frame names its client and session".to_string());
            };
            request = request.forward(client, session);
        }
        Some(other) => return Err(format!("unknown preview frame hint {other:?}")),
    }
    if let (Some(client), Some(seq)) = (client, seq) {
        request = request.ordered(client, seq);
    }
    let answer = gpu.post(request, |job| {
        // The worker waits for FFmpeg and the GPU: the blocking pool's, never a runtime thread.
        tauri::async_runtime::spawn_blocking(job);
    });
    match answer.await {
        Ok(gpu_preview::Delivery::Done(result)) => result,
        Ok(gpu_preview::Delivery::Superseded) => Ok(gpu_preview::PreviewFrameResult::superseded()),
        Ok(gpu_preview::Delivery::Abandoned) | Err(_) => Err("the GPU preview's frame worker stopped".to_string()),
    }
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

/// Bytes in front of the JPEG in a playback message: the frame's timeline time as an `f64`, little
/// endian. Mirrored by `parsePlaybackMessage` in `frontend/src/lib/frame-transport.ts`.
const PLAYBACK_HEADER_BYTES: usize = 8;

/// One composited frame as it goes to the webview during playback: [`PLAYBACK_HEADER_BYTES`] of
/// timeline time (so the webview can drop a frame that arrived after the audio clock has already
/// moved past it), then the JPEG, undecoded.
///
/// Raw bytes through the channel rather than a base64 `data:` URL in JSON: a quarter fewer bytes
/// (76 -> 57 KB a frame at 1080p), no JSON to parse and no base64 to encode here. It is *not* a faster
/// decode: measured in headless Chromium, the old path's `JSON.parse` + `img.src =` cost 0.1-0.3 ms a
/// frame, since a data-URL image is decoded lazily and off the main thread, and `createImageBitmap`
/// plus a canvas draw took no less than the `<img>` did (see `frame-transport.ts`). The page builds
/// the data URL itself, for the frames it shows.
fn playback_message(time: f64, jpeg: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(PLAYBACK_HEADER_BYTES + jpeg.len());
    message.extend_from_slice(&time.to_le_bytes());
    message.extend_from_slice(jpeg);
    message
}

/// Which playback stream should be running, by the id the page gave it (0 = none).
///
/// The ids come from the caller rather than being minted here, because start and stop are
/// separate async IPC calls that can arrive out of order, and a bare generation counter would
/// let a stop meant for the previous stream land after the next one started and kill it, which
/// reads as playback that dies the moment you seek. Ids only ever go up: within a page a new
/// playback's is higher than the last one's, and a page that was reloaded draws its ids from a
/// newer [`playback_session`], so they are higher than anything the page before it used (a
/// counter restarting at 1 would collide with the stale ids still held here). Both questions are
/// then plain comparisons, whichever order the calls arrive in:
///
/// * the stream with the **highest id started** is the one that should run (`begin` keeps the
///   maximum, so a start that arrives late cannot take the pane back from a newer one);
/// * a stream is **stopped** once a stop at or above its id has been seen, which also covers a
///   stop that reaches the backend *before* the start it was meant for: such a stop finds
///   nothing to clear, and the stream would otherwise start with nobody left to end it.
struct Playbacks {
    active: AtomicU64,
    stopped: AtomicU64,
}

impl Playbacks {
    const fn new() -> Self {
        Self {
            active: AtomicU64::new(0),
            stopped: AtomicU64::new(0),
        }
    }

    /// Playback `id` is starting: it supersedes every lower id.
    fn begin(&self, id: u64) {
        self.active.fetch_max(id, Ordering::SeqCst);
    }

    /// The page stopped playback `id` (a pause, a seek, an edit).
    fn stop(&self, id: u64) {
        self.stopped.fetch_max(id, Ordering::SeqCst);
        let _ = self.active.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Whether playback `id` should end: superseded by a newer one, or stopped.
    fn ended(&self, id: u64) -> bool {
        self.active.load(Ordering::SeqCst) != id || self.stopped.load(Ordering::SeqCst) >= id
    }

    /// Playback `id` ran out; free the slot if it is still the one in it.
    fn finish(&self, id: u64) {
        let _ = self.active.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

static PLAYBACKS: Playbacks = Playbacks::new();

/// Pages (webview loads) that have asked for playback ids this run.
static PLAYBACK_SESSIONS: AtomicU64 = AtomicU64::new(0);

/// A number for a page to build its playback ids from, higher than any this run has handed
/// out: the page's ids are `session * 2^32 + n` (`playback-id.ts`), so a reloaded page's ids
/// outrank the ones the page before it left behind. 2^21 sessions fit the 2^53 a JS number
/// holds exactly; a run has no use for that many reloads.
#[tauri::command(async)]
fn playback_session() -> u64 {
    PLAYBACK_SESSIONS.fetch_add(1, Ordering::SeqCst) + 1
}

/// Play the timeline from `start`, streaming composited frames to `on_frame`
/// until playback is stopped, superseded, or the timeline ends.
///
/// Scrubbing and the settled frame still go through `get_timeline_frame` — one
/// process per frame is right when you want *one* frame. Playback is the case
/// that can't work that way: a spawn-seek-decode-exit cycle per frame caps out
/// well below frame rate, so this hands the whole span to a single long-lived
/// ffmpeg and pushes frames up as they render, each as a raw message
/// ([`playback_message`]).
///
/// `fps` and `max_width` are what the preview asks for; left out, the stream plays at the
/// timeline's own rate and the default width (`kerf_core::playback_params`). The preview states
/// them only after a stream could not keep up and it is restarting cheaper.
///
/// Resolves only when playback ends; the caller is not expected to await it.
#[tauri::command]
async fn start_playback(
    state: State<'_, AppState>,
    playback_id: u64,
    start: f64,
    fps: Option<f64>,
    max_width: Option<u32>,
    on_frame: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> CmdResult<()> {
    let shared = state.project.clone();
    // The stream takes the pane over: the window of frames being drawn around the playhead is of
    // no use until it stops, and is not left running beside it.
    state.scrub.stop();
    PLAYBACKS.begin(playback_id);
    blocking(move || {
        // Resolve the inputs under the lock and drop the guard before streaming:
        // playback runs for as long as the user watches, and holding the shared
        // mutex for that would freeze every edit and the whole MCP server.
        let (timeline, assets) = {
            let project = lock_user(&shared);
            refuse_unless_proxied(&project, start, None)?;
            project.timeline_frame_inputs().map_err(|e| e.to_string())?
        };
        // Counted and logged below: whether frames reached the webview at all, and
        // how long the first one took, are the two facts that tell a stream that
        // never started from one whose frames the preview received and discarded.
        // Without them a black pane looks identical either way. Late frames are the
        // third: a stream whose count of them keeps growing is one that cannot render
        // at the rate it was given, and the preview is restarting it cheaper.
        let request = kerf_core::PlaybackRequest { fps, max_width };
        let params = kerf_core::playback_params(&timeline, &assets, request);
        let began = std::time::Instant::now();
        let mut frames: u64 = 0;
        let mut late_frames: u64 = 0;
        let mut worst_late = 0.0_f64;
        let mut first_frame: Option<std::time::Duration> = None;
        // Superseded by a newer playback, or explicitly stopped — including by a stop that arrived
        // before this stream even started.
        let ended = || PLAYBACKS.ended(playback_id);
        // `ended` is also the cancel of the animated titles' drawing, which can take seconds before
        // the first frame exists for the callback below to decline.
        let result = kerf_core::stream_preview_until(&timeline, &assets, start, request, &ended, &mut |f| {
            if ended() {
                return false;
            }
            let sent = on_frame
                .send(tauri::ipc::InvokeResponseBody::Raw(playback_message(f.time, &f.jpeg)))
                .is_ok();
            if sent {
                frames += 1;
                if f.late > 2.0 / params.fps {
                    late_frames += 1;
                }
                worst_late = worst_late.max(f.late);
                first_frame.get_or_insert_with(|| began.elapsed());
            }
            sent
        });
        tracing::info!(
            playback_id,
            frames,
            fps = params.fps,
            max_width = params.max_width,
            late_frames,
            worst_late_ms = (worst_late * 1000.0) as u64,
            first_frame_ms = first_frame.map(|d| d.as_millis() as u64),
            elapsed_ms = began.elapsed().as_millis() as u64,
            "playback stream ended"
        );
        // Running out of timeline, or being superseded or stopped, is not an
        // error; only a genuine ffmpeg failure on a stream someone is still
        // watching is reported — the preview would otherwise just go black.
        let current = !PLAYBACKS.ended(playback_id);
        PLAYBACKS.finish(playback_id);
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
    PLAYBACKS.stop(playback_id);
}

#[tauri::command]
async fn get_waveform_range(
    state: State<'_, AppState>,
    asset_id: String,
    start: f64,
    end: f64,
    buckets: usize,
) -> CmdResult<WaveformRange> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve under the lock, read (and on a first call decode) with it
        // released.
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
    let id = id(&asset_id, "asset_id")?;
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
    let clip_id = opt_id(clip_id.as_deref(), "clip_id")?;
    let id = id(&asset_id, "asset_id")?;
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
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve under the lock, decode with it released.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        Project::decode_energy(&asset, buckets).map_err(|e| e.to_string())
    })
    .await
}

// ---- agent task queue (mutations return the refreshed queue) ---------------

/// The agent task queue, oldest first; `limit` / `offset` window it from the newest end like
/// `get_history`.
#[tauri::command(async)]
fn list_tasks(state: State<'_, AppState>, limit: Option<usize>, offset: Option<usize>) -> CmdResult<Vec<Task>> {
    let page = Page {
        limit,
        offset: offset.unwrap_or(0),
    };
    state
        .project()
        .list_tasks_page(page)
        .map(|(tasks, _)| tasks)
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn add_task(state: State<'_, AppState>, prompt: String) -> CmdResult<Task> {
    state.project().add_task(&prompt).map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn resolve_task(app: AppHandle, state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id, "task_id")?;
    let project = state.project();
    let had_asset_changes = staged_asset_changes(&project)?;
    project.resolve_task(id).map_err(|e| e.to_string())?;
    let tasks = project.list_tasks().map_err(|e| e.to_string())?;
    drop(project);
    if had_asset_changes {
        respawn_proxies(&app);
    }
    Ok(tasks)
}

#[tauri::command(async)]
fn remove_task(state: State<'_, AppState>, task_id: String) -> CmdResult<Vec<Task>> {
    let id = id(&task_id, "task_id")?;
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
    require_user_output_path(&output_path)?;
    // Snapshot the timeline + assets under the lock, then release it before the
    // (seconds-to-minutes) ffmpeg render. Otherwise the export would hold the
    // shared Project mutex for its whole duration and freeze every other GUI
    // command and the MCP agent until it finished. What the output must not be (a source
    // file, a proxy, the project itself) is read under it too; the check, which
    // canonicalises and stats, runs without it.
    let (timeline, assets, protected) = {
        let project = state.project();
        (
            project.timeline().map_err(|e| e.to_string())?,
            project.list_assets().map_err(|e| e.to_string())?,
            project.output_path_inputs().map_err(|e| e.to_string())?,
        )
    };

    // Fresh cancel flag for this run; `cancel_export` flips it from another thread.
    let cancel = state.export_cancel.begin();

    blocking(move || {
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        // Stream `export-progress` events ({ fraction, elapsed_secs, eta_secs }) so
        // the UI can show a bar + ETA. ffmpeg emits ~2/sec, no extra throttle needed.
        let mut on_progress = |p: kerf_core::ExportProgress| {
            let _ = app.emit("export-progress", p);
        };
        // A render that fails or is cancelled removes what it wrote (`kerf_core::discard_partial`,
        // only if this run touched the file), so no earlier export at the path is lost to it.
        let status = kerf_core::render_with_progress(
            &timeline,
            &assets,
            std::path::Path::new(&output_path),
            &options,
            &mut on_progress,
            &|| cancel(),
        )
        .map_err(|e| e.to_string())?;

        match status {
            kerf_core::RenderStatus::Completed => Ok(output_path),
            kerf_core::RenderStatus::Cancelled => Err("export cancelled".to_string()),
        }
    })
    .await
}

/// Progress of the shot-framing pass of a multi-format export, which runs before the first
/// file renders: the same event as the render's, flagged `waiting` so the bar does not sit at 0%.
#[derive(Serialize, Clone)]
struct FramingProgress {
    variant: usize,
    total: usize,
    fraction: f64,
    elapsed_secs: f64,
    eta_secs: Option<f64>,
    waiting: &'static str,
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
    require_user_output_path(&output_path)?;
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
    for variant in &variants {
        kerf_core::check_caption_sidecars(&variant.output, &options).map_err(|e| e.to_string())?;
    }
    let shared = state.project.clone();
    let cancel = state.export_cancel.begin();

    blocking(move || {
        // None of the files may be a source, a proxy or the project itself.
        let protected = lock_user(&shared).output_path_inputs().map_err(|e| e.to_string())?;
        for variant in &variants {
            check_output(&variant.output, &protected).map_err(|e| e.to_string())?;
        }
        // Frame first — plan under the lock, sample without it, apply under it
        // again — then snapshot and render with the lock released, like a
        // single export.
        if smart_crop {
            let (plan, generation) = {
                let project = lock_user(&shared);
                (
                    project.framing_inputs(&deliveries).map_err(|e| e.to_string())?,
                    project.generation(),
                )
            };
            if !plan.jobs.is_empty() {
                let (total, started) = (variants.len(), std::time::Instant::now());
                let mut on_framed = |fraction: f32| {
                    let _ = app.emit(
                        "export-progress",
                        FramingProgress {
                            variant: 0,
                            total,
                            fraction: f64::from(fraction),
                            elapsed_secs: started.elapsed().as_secs_f64(),
                            eta_secs: None,
                            waiting: "framing the shots",
                        },
                    );
                };
                let stop = stop_or_replaced(&cancel, &shared, generation);
                let framings = Project::sample_framings_with(&plan, &mut on_framed, &stop).map_err(|e| match e {
                    kerf_core::Error::Cancelled => "export cancelled".to_string(),
                    other => other.to_string(),
                })?;
                let framed = {
                    let project = lock_user(&shared);
                    require_generation(&project, generation, "the shots were being framed")?;
                    project.apply_framings(&framings).map_err(|e| e.to_string())?
                };
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
        // `render_variants` writes the files one after another and removes the one that failed
        // or was cancelled (only if this run touched it); the finished ones stay. Its progress
        // is the event the webview already reads: which file, of how many, and the whole.
        let mut on_progress = |p: kerf_core::VariantProgress| {
            let _ = app.emit("export-progress", p);
        };
        match kerf_core::render_variants(&timeline, &assets, &variants, &options, &mut on_progress, &|| cancel()) {
            Ok((kerf_core::RenderStatus::Completed, _)) => {}
            Ok((kerf_core::RenderStatus::Cancelled, _)) => return Err("export cancelled".to_string()),
            Err(e) => return Err(e.to_string()),
        }
        Ok(variants.iter().map(|v| v.output.to_string_lossy().into_owned()).collect())
    })
    .await
}

/// The error an abandoned cover export returns, for the webview to stay quiet on.
const COVER_CANCELLED: &str = "cover export cancelled";

/// Write the composited frame at `time_secs` to `output_path` as a **cover
/// image** — full delivery resolution, decoded from the original media rather
/// than a preview proxy. `format` follows the file extension when omitted.
/// `cancel_cover_export` stops it, rejecting with `"cover export cancelled"`.
#[tauri::command]
async fn export_cover(
    state: State<'_, AppState>,
    time_secs: f64,
    output_path: String,
    format: Option<kerf_core::ImageFormat>,
) -> CmdResult<String> {
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    let cancel = state.cover_cancel.begin();
    blocking(move || {
        // Same shape as every heavy command: resolve under the lock, render
        // without it. A 4K still is a real decode.
        let ((timeline, assets), protected) = {
            let project = lock_user(&shared);
            (
                project.export_still_inputs().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        let path = Project::render_still_cancellable(&timeline, &assets, time_secs, &output_path, format, &|| cancel()).map_err(
            |e| match e {
                kerf_core::Error::Cancelled => COVER_CANCELLED.to_string(),
                other => other.to_string(),
            },
        )?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

/// The video scopes of the composited frame at `time_secs`: the export's still of the cut (from
/// the originals) read with the delivery's matrix. The summary always; the grids in `grids`.
#[tauri::command]
async fn get_scopes(
    state: State<'_, AppState>,
    time_secs: f64,
    grids: Option<Vec<kerf_core::scopes::ScopeKind>>,
    matrix: Option<kerf_core::scopes::ScopeMatrix>,
    size: Option<kerf_core::scopes::GridSize>,
    max_width: Option<u32>,
) -> CmdResult<kerf_core::scopes::Scopes> {
    let shared = state.project.clone();
    blocking(move || {
        let (timeline, assets) = lock_user(&shared).export_still_inputs().map_err(|e| e.to_string())?;
        let req = kerf_core::scopes::ScopeRequest {
            time_secs,
            max_width: max_width.map(|w| w.clamp(64, kerf_core::scopes::MAX_SCOPE_WIDTH)),
            matrix: matrix.unwrap_or_default(),
            options: kerf_core::scopes::ScopeOptions {
                grids: grids.unwrap_or_default(),
                size: size.unwrap_or_default(),
            },
        };
        kerf_core::scopes::timeline_scopes(&timeline, &assets, &req).map_err(|e| e.to_string())
    })
    .await
}

/// How the Preview will play each span of the cut (`kerf_core::render_bar`): the GPU preview's
/// compositor where it is on and draws the frame, the FFmpeg stream where it hands over, and the
/// spans where playback stalls. `width` is the Preview's render width.
#[tauri::command]
async fn get_render_bar(
    state: State<'_, AppState>,
    gpu: State<'_, Arc<gpu_preview::GpuPreview>>,
    width: Option<u32>,
) -> CmdResult<kerf_core::render_bar::RenderBar> {
    let shared = state.project.clone();
    let caps = gpu.is_enabled().then_some(kerf_gpu::Compositor::CAPS);
    blocking(move || {
        // The project's rows under the lock; a proxy's state (a cached ffprobe, once per file)
        // with it released.
        let (timeline, assets, inputs) = {
            let project = lock_user(&shared);
            let (timeline, assets) = project.export_still_inputs().map_err(|e| e.to_string())?;
            (timeline, assets, project.proxy_inputs().map_err(|e| e.to_string())?)
        };
        let waits = Project::proxy_waits_in(&timeline, kerf_core::proxy::effective_preview_source(), 0.0, None, |id| {
            inputs
                .iter()
                .find(|i| i.asset.id == id)
                .cloned()
                .ok_or(kerf_core::Error::AssetNotFound(id))
        })
        .map_err(|e| e.to_string())?;
        let req = kerf_core::render_bar::BarRequest {
            caps,
            width: width.unwrap_or(960).clamp(64, 1920),
            color: kerf_core::composite_color_policy(),
            media: &kerf_core::ProxyMedia,
            // Named, not with the build's progress: the bar is asked again on an edit, not as a
            // proxy builds, so a percentage would go stale under the pointer.
            waits: waits
                .iter()
                .map(|w| (w.asset_id, format!("{} of {}", kerf_core::proxy::PROXY_WAIT_PREFIX, w.name)))
                .collect(),
        };
        Ok(kerf_core::render_bar::render_bar(&timeline, &assets, &req))
    })
    .await
}

/// Stop the cover frame being written (if any).
#[tauri::command(async)]
fn cancel_cover_export(state: State<'_, AppState>) {
    state.cover_cancel.stop();
}

/// The error an abandoned smart crop returns, for the webview to stay quiet on.
const SMART_CROP_CANCELLED: &str = "smart crop cancelled";

/// `smart-crop-progress`: how much of the shots have been sampled.
#[derive(Serialize, Clone)]
struct SmartCropProgress {
    fraction: f64,
}

/// Frame every shot for the delivery frame instead of centring it blindly —
/// samples where each clip's content sits and writes the crop that keeps it.
/// One clip when `clip_id` is given, otherwise every clip on an unlocked video
/// track. The result is an ordinary transform crop, so the inspector's sliders
/// still have the last word. Streams `smart-crop-progress` (the fraction of clips
/// sampled); `cancel_smart_crop` stops it, rejecting with `"smart crop cancelled"`.
#[tauri::command]
async fn smart_crop(app: AppHandle, state: State<'_, AppState>, clip_id: Option<String>) -> CmdResult<Timeline> {
    let clip = opt_id(clip_id.as_deref(), "clip_id")?;
    let shared = state.project.clone();
    let cancel = state.smart_crop_cancel.begin();
    blocking(move || {
        // The usual shape for a heavy command: plan under the lock, decode
        // without it (one short ffmpeg pass per clip), apply under it again.
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.smart_crop_inputs(clip).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let mut on_progress = |fraction: f32| {
            let _ = app.emit(
                "smart-crop-progress",
                SmartCropProgress {
                    fraction: f64::from(fraction),
                },
            );
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        let crops = Project::sample_smart_crops_with(&plan, &mut on_progress, &stop).map_err(|e| match e {
            kerf_core::Error::Cancelled => SMART_CROP_CANCELLED.to_string(),
            other => other.to_string(),
        })?;
        let project = lock_user(&shared);
        require_generation(&project, generation, "the shots were being framed")?;
        project.apply_smart_crops(&crops).map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

/// Stop the smart crop in flight (if any).
#[tauri::command(async)]
fn cancel_smart_crop(state: State<'_, AppState>) {
    state.smart_crop_cancel.stop();
}

/// What syncing a clip by its sound found, and the timeline after it.
#[derive(serde::Serialize)]
struct SyncedByAudio {
    sync: kerf_core::AudioSync,
    timeline: Timeline,
}

/// Move `clip_id` so its sound lines up with `reference_id`'s (linked partners follow).
/// An unreliable match is reported without moving anything unless `force`; a forced sync of
/// the pair just measured (the same decode) applies that measurement instead of decoding
/// again. A wait for the heavy-job slot is told through `audio-sync-progress`;
/// `cancel_audio_sync` stops it, which then rejects with `"audio sync cancelled"`.
#[tauri::command]
async fn sync_clips_by_audio(
    app: AppHandle,
    state: State<'_, AppState>,
    reference_id: String,
    clip_id: String,
    force: Option<bool>,
) -> CmdResult<SyncedByAudio> {
    let (reference, clip) = (id(&reference_id, "reference_id")?, id(&clip_id, "clip_id")?);
    let force = force.unwrap_or(false);
    let shared = state.project.clone();
    let cancel = state.sync_cancel.begin();
    let last = state.last_sync.clone();
    blocking(move || {
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.audio_sync_inputs(reference, clip).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let cached = last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(p, _)| force && *p == plan)
            .map(|(_, m)| *m);
        let measured = match cached {
            Some(m) => m,
            None => {
                let stop = stop_or_replaced(cancel, &shared, generation);
                let m = Project::measure_audio_sync(
                    &plan,
                    &mut |w| {
                        let _ = app.emit(
                            "audio-sync-progress",
                            VoiceoverProgressEvent {
                                stage: "waiting".to_string(),
                                fraction: None,
                                detail: Some(w.message().to_string()),
                            },
                        );
                    },
                    &stop,
                )
                .map_err(|e| match e {
                    kerf_core::Error::Cancelled => AUDIO_SYNC_CANCELLED.to_string(),
                    other => other.to_string(),
                })?;
                *last.lock().unwrap_or_else(|e| e.into_inner()) = Some((plan, m));
                m
            }
        };
        let project = lock_user(&shared);
        require_generation(&project, generation, "the clips were being synced")?;
        let sync = project.apply_audio_sync(&measured, force).map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(SyncedByAudio { sync, timeline })
    })
    .await
}

/// The error a stopped sync returns, for the webview to stay quiet on.
const AUDIO_SYNC_CANCELLED: &str = "audio sync cancelled";

/// Stop the sync by audio in flight (if any): it gives up while it waits for the slot or
/// between its decodes, and [`sync_clips_by_audio`] rejects with `"audio sync cancelled"`.
#[tauri::command(async)]
fn cancel_audio_sync(state: State<'_, AppState>) {
    state.sync_cancel.stop();
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
    range: Option<kerf_core::TimeRange>,
) -> CmdResult<kerf_core::PlatformReport> {
    // The export dialog can resize away from the project frame and write only
    // the marked span; when it does it passes the frame and range it is actually
    // about to render, so the verdict is about the file that will exist rather
    // than the one the project defaults to. The answer carries the cut it judged
    // so the dialog can say its frame and length in words.
    let frame = width.zip(height);
    let range = range.map(|r| (r.start, r.end));
    state.project().platform_report(frame, range).map_err(|e| e.to_string())
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
    state.analysis_cancel.stop();
}

/// Request cancellation of the in-flight export (if any). The running
/// [`export_timeline`] observes the flag on its next progress poll, stops
/// ffmpeg, and returns the `"export cancelled"` error.
#[tauri::command(async)]
fn cancel_export(state: State<'_, AppState>) {
    state.export_cancel.stop();
}

// ---- agent connection (MCP endpoint) ---------------------------------------

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

/// What the editor window shows (playhead, selection, marks, visible range,
/// focused panel), for the MCP `get_editor_context` tool. The webview sends it
/// debounced and as a heartbeat; it is kept in app state, not in the project.
#[tauri::command(async)]
fn report_ui_context(state: State<'_, editor_context::EditorContextState>, context: editor_context::UiContext) {
    state.report(context);
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
    if patch.get("proxy_size").is_some() || patch.get("preview_source").is_some() {
        // A new size is a different proxy file, and switching back from the original wants them
        // again: queue whatever is missing (a cached one is skipped). The preview re-decodes
        // from whichever file it should now.
        respawn_proxies(&app);
        let _ = app.emit("proxy-ready", ());
    }
    if patch.get("gpu_preview").is_some() {
        // The flag flips here, now: the page asks for its next frame as soon as this returns. Only
        // the teardown of turning it off, which may wait for a frame in flight, is off this thread.
        if gpu.set_enabled(stored.gpu_preview) {
            let gpu = gpu.inner().clone();
            std::thread::spawn(move || gpu.teardown_if_off());
        }
    }
    Ok(settings::SettingsView::current(&stored))
}

/// Put every preference back to its default (the old file is kept beside it as
/// `settings.reset-<time>.json`) and answer with the resolved view.
#[tauri::command(async)]
fn reset_settings(app: AppHandle, gpu: State<'_, Arc<gpu_preview::GpuPreview>>) -> CmdResult<settings::SettingsView> {
    let stored = settings::reset(&app)?;
    // The proxy size and the preview source are back to theirs: queue what is missing and have
    // the preview decode from whichever file it should now.
    respawn_proxies(&app);
    let _ = app.emit("proxy-ready", ());
    if gpu.set_enabled(stored.gpu_preview) {
        let gpu = gpu.inner().clone();
        std::thread::spawn(move || gpu.teardown_if_off());
    }
    Ok(settings::SettingsView::current(&stored))
}

/// Show the settings file in the OS file manager (it is written first if there is none yet).
#[tauri::command(async)]
fn reveal_settings_file(app: AppHandle) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let file = settings::file_path(&app).ok_or("no config directory available for settings")?;
    if !file.exists() {
        settings::update(&app, &serde_json::json!({}))?;
    }
    app.opener().reveal_item_in_dir(&file).map_err(|e| e.to_string())
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
/// decided, so `init_logging` and `reveal_logs` cannot disagree.
fn log_dir_path(app: &AppHandle) -> tauri::Result<std::path::PathBuf> {
    app.path().app_data_dir().map(|dir| dir.join("logs"))
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
///
/// **Must run before anything else in the process starts a thread** (it is the first statement of
/// [`run`]): `set_var` races every concurrent read of the environment, and the runtime, the
/// webview and ffmpeg's own children all read it. Logging is not up yet either, so what was set
/// is returned for `setup` to log.
fn use_bundled_ffmpeg() -> Vec<(&'static str, std::path::PathBuf)> {
    let mut set = Vec::new();
    let Ok(exe) = std::env::current_exe() else { return set };
    let Some(dir) = exe.parent() else { return set };
    for (var, name) in [("KERF_FFMPEG", "ffmpeg"), ("KERF_FFPROBE", "ffprobe")] {
        if std::env::var_os(var).is_some() {
            continue;
        }
        let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if path.is_file() {
            std::env::set_var(var, &path);
            set.push((var, path));
        }
    }
    set
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
    /// Something was asked for by name (a command line, a second launch), so the project a
    /// setting would reopen is not wanted.
    named: bool,
}

impl LaunchSlot {
    fn new(first: LaunchProject) -> Self {
        let mut slot = Self::default();
        slot.hold(first);
        slot
    }

    fn hold(&mut self, request: LaunchProject) {
        if request != LaunchProject::Nothing {
            self.named = true;
            self.request = Some(request);
        }
    }

    /// The project a setting reopens, once the disk has said it is there. It gives way to anything
    /// asked for by name, however early or late either arrives: `None` when the page will pick it
    /// up (or something was named), `Some` when the page is already listening and must be told.
    fn offer_reopen(&mut self, path: String) -> Option<LaunchProject> {
        if self.named {
            return None;
        }
        if self.taken {
            return Some(LaunchProject::Open(path));
        }
        self.request = Some(LaunchProject::Open(path));
        None
    }

    /// A second launch's request. `None`: held for the webview to take. `Some`:
    /// the webview is already listening, deliver it as an event.
    fn offer(&mut self, request: LaunchProject) -> Option<LaunchProject> {
        if request == LaunchProject::Nothing {
            return None;
        }
        self.named = true;
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

/// With nothing named on the command line and the setting on: ask the disk whether the project
/// that was open last is still there and hand it to the page like a named one — held for the
/// page's pull while it boots, or as an event when it is already listening (a slow share).
fn reopen_last_project(app: &AppHandle, stored: &settings::Settings) {
    let Some(path) = settings::reopen_target(stored, |p| p.is_file()) else {
        return;
    };
    tracing::info!(path, "reopening the project that was open last");
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let undelivered = state.launch.lock().unwrap_or_else(|e| e.into_inner()).offer_reopen(path);
    if let Some(request) = undelivered {
        deliver_launch(app, request);
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
    // Before a thread exists: this sets environment variables.
    let bundled_ffmpeg = use_bundled_ffmpeg();
    // Start on a fresh, empty in-memory project; the user opens an existing
    // `.kerf` file or imports media to populate it.
    let project = Arc::new(Mutex::new(Project::open_in_memory().expect("failed to create empty project")));
    // A `.kerf` on the command line of the very first launch (a second launch's is
    // forwarded by the single-instance plugin, which never reaches this far).
    let launch = first_launch_project();
    let main_window_shown = Arc::new(AtomicBool::new(false));
    let scrub = Arc::new(kerf_core::ScrubFrames::new(kerf_core::ScrubConfig::default()));

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
        .manage(AppState::new(
            project.clone(),
            main_window_shown.clone(),
            LaunchSlot::new(launch.clone()),
            scrub.clone(),
        ))
        .manage(popout::PopoutState::default())
        .manage(editor_context::EditorContextState::default())
        .manage(Mutex::new(recovery::RecoveryState::default()))
        .on_window_event(popout::on_window_event)
        .setup(move |app| {
            // Logging needs the resolved app data directory, so set it up here
            // (before anything else in setup) rather than at the top of `run`.
            init_logging(app.handle());
            install_panic_hook();
            set_app_dirs(app.handle());
            // The untitled project moves into a crash-recovery file before anything can edit it.
            recovery::start(app.handle());
            // The editor window is built here, not by Tauri from the config (`create:
            // false` there): only a builder can carry the handler that lets a panel
            // be detached into a window of its own. Otherwise exactly what the config
            // says — hidden until the page has themed itself, see `show_main_window`.
            popout::create_main_window(app)?;
            for (var, path) in &bundled_ffmpeg {
                tracing::info!(%var, path = %path.display(), "using bundled FFmpeg binary");
            }
            // Before anything can spawn ffmpeg: how much of the machine it may take.
            // The previous run died during the GPU preview's first frame (a driver crash): it must not
            // do it again on every launch, so the setting goes off before it is read.
            let crashed = gpu_preview::take_crash_marker_in(app.handle());
            if crashed {
                tracing::warn!("the last run ended while the GPU preview was starting; turning the setting off");
                if let Err(e) = settings::update(app.handle(), &serde_json::json!({ "gpu_preview": false })) {
                    tracing::warn!(error = %e, "could not turn the GPU preview setting off");
                }
            }
            let stored = settings::load(app.handle());
            settings::apply(&stored);
            // With nothing named on the command line, the setting may ask for the project that
            // was open last. The page takes it like any other launch request.
            // Whether the file is still there is asked of the disk on a thread of its own: a project on
            // a share that is offline would otherwise hold the whole launch (this is the main thread).
            if launch == LaunchProject::Nothing && stored.reopen_last_project {
                let (handle, stored) = (app.handle().clone(), stored.clone());
                std::thread::spawn(move || reopen_last_project(&handle, &stored));
            }
            // The GPU preview builds nothing until a frame wants it; this is only the setting.
            let gpu = Arc::new(gpu_preview::GpuPreview::for_app(app.handle().clone(), crashed, scrub));
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
            // Read the machine's fonts off the first Play of an animated title.
            std::thread::spawn(kerf_core::fonts::warm);
            std::thread::spawn(warm_transcription_backend);
            // The window is created hidden; if the webview never asks for it
            // (`show_main_window`), this does.
            spawn_reveal_failsafe(app.handle().clone(), main_window_shown, REVEAL_FAILSAFE);

            // The app *is* the MCP server: host the tools over HTTP, sharing the
            // same Project the GUI edits, so a connected LLM works on the open
            // project and its edits show up live.
            mcp::start(app.handle(), project);
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
            recent_projects,
            open_recent,
            clear_recent_projects,
            remove_asset,
            relink_asset,
            check_relink,
            missing_assets,
            paths_exist,
            import_asset,
            cancel_import,
            analyze_asset,
            analysis_statuses,
            proxy_statuses,
            rebuild_proxy,
            delete_proxy,
            transcription_status,
            set_speech_model,
            download_speech_model,
            cancel_model_download,
            voiceover_status,
            prepare_voiceover,
            generate_voiceover,
            cancel_voiceover,
            music_status,
            prepare_music,
            compose_music,
            cancel_music,
            get_band_balance,
            cancel_band_balance,
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
            set_master_duck,
            set_master_true_peak,
            plan_music_fit,
            fit_music,
            stems_status,
            separate_stems,
            cancel_stems,
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
            set_grade,
            inspect_lut,
            set_video_effects,
            set_audio_effects,
            set_keyframes,
            add_keyframe,
            set_keyframe_easing,
            set_property_keyframes,
            copy_keyframes,
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
            add_svg_overlay,
            update_svg_overlay,
            remove_svg_overlay,
            validate_svg,
            list_svg_presets,
            svg_preset_preview,
            list_templates,
            template_poster,
            svg_element_ids,
            template_image_value,
            save_template,
            apply_template,
            apply_template_clip,
            add_title_clip,
            update_title_clip,
            set_template_values,
            import_template,
            export_template,
            delete_template,
            generate_captions,
            clear_captions,
            import_captions,
            import_captions_text,
            export_srt,
            export_captions,
            export_interchange,
            export_markers,
            refine_transcript,
            remove_silence,
            snap_to_beats,
            smart_crop,
            sync_clips_by_audio,
            cancel_audio_sync,
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
            apply_staged_subset,
            discard_staged_edit,
            undo_staged_edit,
            discarded_staged_edit,
            restore_discarded_edits,
            undo,
            redo,
            revert_to,
            get_frame,
            get_timeline_frame,
            get_preview_frame,
            set_preview_bounds,
            gpu_preview_status,
            playback_session,
            start_playback,
            stop_playback,
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
            cancel_cover_export,
            cancel_smart_crop,
            get_scopes,
            get_render_bar,
            platform_targets,
            platform_check,
            reveal_path,
            agent_status,
            mcp_commands::mcp_connection_info,
            mcp_commands::regenerate_mcp_token,
            mcp_commands::set_mcp_settings,
            report_ui_context,
            get_settings,
            set_settings,
            reset_settings,
            reveal_settings_file,
            read_text_file,
            write_text_file,
            reveal_logs,
            log_frontend,
            show_main_window,
            take_launch_project,
            recovery::list_recoveries,
            recovery::recover_project,
            recovery::discard_recovery,
            recovery::discard_recovery_at_exit,
            popout::popout_expect,
            popout::popout_cancel,
            popout::close_popout,
            popout::popout_focus,
            popout::popout_move
        ])
        .build(tauri::generate_context!())
        .expect("error while building Kerf")
        .run(|app, event| {
            // The logfile's last line then says whether the end was a clean exit
            // or a crash.
            if let tauri::RunEvent::Exit = event {
                recovery::on_exit(app);
                tracing::info!("kerf exiting");
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{
        bring_forward, check_output_path, file_appender, file_layer, filmstrip_payload, id, ids, launch_project, log_panic,
        opt_id, panic_summary, playback_message, playback_session, project_arg, require_generation, require_json_path,
        require_local_output_path, reveal_once, truncate_log, LaunchProject, LaunchSlot, LogBudget, PathSource, Playbacks,
        StopEpoch, FRONTEND_LOG_PER_SEC, PLAYBACK_HEADER_BYTES, REVEAL_FAILSAFE,
    };
    use base64::Engine as _;
    use kerf_core::{Filmstrip, FilmstripSheet};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tracing_subscriber::prelude::*;

    /// The bytes `parsePlaybackMessage` (frontend/src/lib/frame-transport.test.ts) is held to: the
    /// same vector is spelled there, so the two ends cannot drift apart.
    #[test]
    fn a_playback_message_is_the_time_then_the_jpeg() {
        let jpeg = [0xFF, 0xD8, 1, 2, 0xFF, 0xD9];
        let message = playback_message(1.5, &jpeg);
        assert_eq!(
            message,
            [0, 0, 0, 0, 0, 0, 0xF8, 0x3F, 0xFF, 0xD8, 1, 2, 0xFF, 0xD9],
            "1.5 as a little-endian f64, then the frame untouched"
        );
        assert_eq!(message.len(), PLAYBACK_HEADER_BYTES + jpeg.len());
        assert_eq!(f64::from_le_bytes(message[..8].try_into().unwrap()), 1.5);
        let empty = playback_message(0.0, &[]);
        assert_eq!(empty, [0u8; PLAYBACK_HEADER_BYTES]);
    }

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
        assert!(
            require_local_output_path("\\\\?\\C:\\Users\\me\\out.mp4").is_ok(),
            "the verbatim spelling of a local drive is local"
        );
    }

    const NETWORK_PATHS: [&str; 8] = [
        "\\\\server\\share\\out.mp4",
        "//server/share/out.mp4",
        "\\/server/share/out.mp4",
        "/\\server\\share\\out.mp4",
        "\\\\?\\UNC\\server\\share\\out.mp4",
        "//?/UNC/server/share/out.mp4",
        "\\\\.\\pipe\\kerf",
        "\\\\localhost\\c$\\out.mp4",
    ];

    #[test]
    fn an_agent_may_not_name_a_network_share_on_windows() {
        for bad in NETWORK_PATHS {
            let why = check_output_path(bad, PathSource::Agent, true).expect_err(bad);
            assert!(why.contains("network"), "{bad}: {why}");
        }
    }

    #[test]
    fn a_double_slash_is_an_ordinary_absolute_path_off_windows() {
        for ok in ["//tmp/out.mp4", "///tmp/out.mp4", "//home/user/out.mp4"] {
            assert!(check_output_path(ok, PathSource::Agent, false).is_ok(), "{ok}");
            assert!(check_output_path(ok, PathSource::User, false).is_ok(), "{ok}");
        }
        // The backslash forms are Windows spellings and no absolute path anywhere else.
        for bad in ["\\\\server\\share\\out.mp4", "\\/server/share/out.mp4", "\\\\.\\pipe\\kerf"] {
            assert!(check_output_path(bad, PathSource::Agent, false).is_err(), "{bad}");
            assert!(check_output_path(bad, PathSource::User, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_share_picked_in_the_save_dialog_is_the_users_intent_on_windows() {
        for ok in [
            "\\\\nas\\share\\out.mp4",
            "//nas/share/out.mp4",
            "\\\\?\\UNC\\nas\\share\\out.mp4",
            "C:\\Users\\me\\out.mp4",
        ] {
            assert!(check_output_path(ok, PathSource::User, true).is_ok(), "{ok}");
        }
        // The device namespace is never an output, and the rest of the rules still hold.
        for bad in [
            "\\\\.\\pipe\\kerf",
            "\\\\?\\GLOBALROOT\\Device\\x",
            "https://nas/share/out.mp4",
            "out.mp4",
            "pipe:1",
        ] {
            assert!(check_output_path(bad, PathSource::User, true).is_err(), "{bad}");
        }
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
    fn template_files_are_only_read_as_kerftemplate() {
        use super::require_template_path;
        assert!(require_template_path("/t/card.kerftemplate").is_ok());
        assert!(require_template_path("C:\\t\\CARD.KERFTEMPLATE").is_ok());
        assert!(require_template_path("/home/u/.bashrc").is_err());
        assert!(require_template_path("/t/card.kerftemplate.zip").is_err());
        assert!(require_template_path("/t/card").is_err());
    }

    /// The ids command takes text straight from the webview: a title is at most 128 KiB, and a document
    /// over that is refused before anything is parsed.
    #[test]
    fn the_ids_of_a_title_are_listed_for_a_title_that_could_be_one() {
        let ok = super::svg_element_ids(r#"<svg xmlns="http://www.w3.org/2000/svg"><text id="a"/><g id="b"/></svg>"#.to_string())
            .unwrap();
        assert_eq!(
            ok.iter().map(|e| (e.id.as_str(), e.tag.as_str())).collect::<Vec<_>>(),
            [("a", "text"), ("b", "g")]
        );
        let big = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><text id="a"/><!--{}--></svg>"#,
            "x".repeat(200 * 1024)
        );
        let e = super::svg_element_ids(big).err().expect("over the size of a title");
        assert!(e.contains("larger than 128 KiB"), "{e}");
        assert!(super::svg_element_ids("not svg".to_string()).is_err());
    }

    /// A picture for an image control is read bounded, and the engine judges it: a file that is not a
    /// picture, a relative path, a directory and an oversized file are all refused with a reason.
    #[test]
    fn a_picture_for_a_template_is_read_bounded_and_judged_by_the_engine() {
        use super::template_image_data_url;
        let limit = kerf_core::template::MAX_IMAGE_BYTES as u64;
        let dir = Scratch::new("template-image");
        let png = dir.0.join("logo.png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAAEklEQVR4nGP4z8DwHxkzkC4AADxAH+HggXe0AAAAAElFTkSuQmCC")
            .unwrap();
        std::fs::write(&png, &bytes).unwrap();
        let url = template_image_data_url(png.to_str().unwrap()).unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "{url}");

        let refused = |path: &Path| template_image_data_url(path.to_str().unwrap()).unwrap_err().to_string();
        assert!(template_image_data_url("logo.png")
            .unwrap_err()
            .to_string()
            .contains("absolute"));
        assert!(refused(&dir.0).contains("not a regular file"));
        assert!(refused(&dir.0.join("missing.png")).contains("cannot read"));
        let text = dir.0.join("notes.txt");
        std::fs::write(&text, "not a picture").unwrap();
        assert!(refused(&text).contains("PNG, JPEG, GIF or WebP"));
        // The bound is the engine's own: a file one byte over what fits in a title is refused, whatever
        // it is, and without being read whole (a huge sparse one answers at once).
        let big = dir.0.join("big.png");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(limit + 1).unwrap();
        let why = refused(&big);
        assert!(why.contains("is 95 KiB") && why.contains("at most 94 KiB"), "{why}");
        let huge = dir.0.join("huge.png");
        std::fs::File::create(&huge).unwrap().set_len(8 << 30).unwrap();
        assert!(refused(&huge).contains("too large"));
        // A picture that is within the bound but not one the engine takes is the engine's refusal.
        let mut padded = bytes;
        padded.resize(limit as usize, 0);
        assert!(template_image_data_url(write(&dir.0, "padded.png", &padded).to_str().unwrap()).is_ok());
        padded.push(0);
        assert!(refused(&write(&dir.0, "over.png", &padded)).contains("too large"));
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
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
    fn the_project_a_setting_reopens_arrives_whenever_the_disk_answers_and_never_beats_a_named_one() {
        let open = |p: &str| LaunchProject::Open(p.to_string());
        // The disk answers before the page asks: it is held for the page's pull.
        let mut slot = LaunchSlot::new(LaunchProject::Nothing);
        assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None);
        assert_eq!(slot.take(), Some(open("/last.kerf")));

        // After: a slow share. The page is listening, so it is told as an event, once.
        let mut slot = LaunchSlot::new(LaunchProject::Nothing);
        assert_eq!(slot.take(), None);
        assert_eq!(slot.offer_reopen("/last.kerf".to_string()), Some(open("/last.kerf")));

        // A project named on the command line, or by a second launch, is the one wanted.
        let mut slot = LaunchSlot::new(open("/named.kerf"));
        assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None);
        assert_eq!(slot.take(), Some(open("/named.kerf")));
        let mut slot = LaunchSlot::new(LaunchProject::Nothing);
        assert_eq!(slot.take(), None);
        assert_eq!(slot.offer(open("/second.kerf")), Some(open("/second.kerf")));
        assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None, "the late reopen gives way");
        let mut slot = LaunchSlot::new(LaunchProject::Nothing);
        assert_eq!(slot.offer(open("/second.kerf")), None);
        assert_eq!(slot.offer_reopen("/last.kerf".to_string()), None);
        assert_eq!(slot.take(), Some(open("/second.kerf")));
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
        // …by `popout::create_main_window`, from this very entry: Tauri must not also create
        // it, and a second window labelled `main` would be refused.
        assert_eq!(
            main["create"], false,
            "the main window is built in code (popout.rs) so it can answer window.open"
        );
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

    #[test]
    fn a_bad_id_names_the_argument_it_came_from() {
        let good = "3f2b1c9e-5a7d-4e0a-9c1b-2d4f6a8b0c1d";
        assert!(id(good, "clip_id").is_ok());
        let why = id("not-an-id", "clip_id").unwrap_err();
        assert!(why.contains("clip_id") && why.contains("not-an-id"), "{why}");
        let why = opt_id(Some(""), "track_id").unwrap_err();
        assert!(why.contains("track_id"), "{why}");
        assert_eq!(opt_id(None, "track_id"), Ok(None));
        let list = vec![good.to_string(), good.to_string(), "oops".to_string()];
        let why = ids(&list, "clip_ids").unwrap_err();
        assert!(why.contains("clip_ids[2]"), "the entry that is wrong is named: {why}");
        assert_eq!(ids(&list[..2], "clip_ids").unwrap().len(), 2);
        let long = "x".repeat(10_000);
        assert!(
            id(&long, "asset_id").unwrap_err().len() < 300,
            "a pasted blob is not echoed whole"
        );
    }

    #[test]
    fn a_stop_reaches_every_job_that_began_before_it_and_none_after() {
        let stop = StopEpoch::default();
        let (first, second) = (stop.begin(), stop.begin());
        assert!(!first() && !second());
        stop.stop();
        assert!(first() && second(), "one stop ends the whole batch");
        let later = stop.begin();
        assert!(!later(), "a job that began after the stop is not cancelled by it");
        stop.stop();
        assert!(later());
    }

    /// An agent's long job takes its stop from the same epoch as the GUI button that cancels it, and
    /// from no other: pressing Stop on a voiceover must not end an export.
    #[test]
    fn an_agents_long_jobs_stop_with_their_own_cancel_buttons() {
        use crate::AppState;
        use kerf_core::Project;
        use std::sync::{Arc, Mutex};
        let state = AppState::new(
            Arc::new(Mutex::new(Project::open_in_memory().unwrap())),
            Arc::new(AtomicBool::new(false)),
            LaunchSlot::default(),
            Arc::new(kerf_core::ScrubFrames::new(kerf_core::ScrubConfig::default())),
        );
        type Running = Box<dyn Fn() -> bool>;
        let jobs: [(&str, Running, &StopEpoch); 8] = [
            ("voiceover", Box::new(state.begin_voiceover()), &state.voiceover_cancel),
            ("music", Box::new(state.begin_music()), &state.music_cancel),
            ("stems", Box::new(state.begin_stems()), &state.stems_cancel),
            ("smart crop", Box::new(state.begin_smart_crop()), &state.smart_crop_cancel),
            ("export", Box::new(state.begin_export()), &state.export_cancel),
            ("analysis", Box::new(state.begin_analysis()), &state.analysis_cancel),
            ("cover", Box::new(state.begin_cover_export()), &state.cover_cancel),
            ("import", Box::new(state.begin_import()), &state.import_stop),
        ];
        for (name, running, _) in &jobs {
            assert!(!running(), "{name} starts clean");
        }
        for (pressed, (name, _, button)) in jobs.iter().enumerate() {
            button.stop();
            for (at, (other, running, _)) in jobs.iter().enumerate() {
                assert_eq!(running(), at <= pressed, "{other} after the {name} button");
            }
        }
        let later = state.begin_voiceover();
        assert!(!later(), "a Stop pressed before a run began does not end it");
        state.voiceover_cancel.stop();
        assert!(later());
    }

    #[test]
    fn a_job_for_a_replaced_project_is_refused() {
        use kerf_core::Project;
        let mut project = Project::open_in_memory().unwrap();
        let noted = project.generation();
        assert!(require_generation(&project, noted, "importing").is_ok());
        project.replace_with(Project::open_in_memory().unwrap()).close();
        let why = require_generation(&project, noted, "importing").unwrap_err();
        assert!(why.contains("replaced") && why.contains("importing"), "{why}");
        assert!(require_generation(&project, project.generation(), "importing").is_ok());
    }

    /// Save As swaps in the reopened copy without moving the generation: a job that noted it before
    /// (an analysis, an import, a voiceover) still lands in the project, and a swap that is another
    /// project (open / new) still drops it.
    #[test]
    fn a_job_in_flight_still_lands_after_a_save_as() {
        use kerf_core::{AnalysisKind, AssetAnalysis, Project, TimeRange};
        let dir = std::env::temp_dir().join(format!("kerf-save-as-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut project = Project::sample().unwrap();
        let Some(asset) = project.list_assets().ok().and_then(|a| a.into_iter().next()) else {
            panic!("the sample project lists an asset");
        };

        // The job resolved its inputs under the old project, and is running with the lock released.
        let noted = project.generation();
        let changes = project.change_count();
        let copy = dir.join("copy.kerf");
        project.save_as(&copy).unwrap();
        project.replace_with_saved_copy(Project::open(&copy).unwrap()).close();

        require_generation(&project, noted, "it was being analysed").expect("the same project, in another file");
        assert!(project.change_count() > changes, "the count only goes up");
        let patch = AssetAnalysis {
            asset_id: asset.id,
            silence_segments: vec![TimeRange { start: 1.0, end: 2.0 }],
            ran: vec![AnalysisKind::Silence],
            ..AssetAnalysis::default()
        };
        project.merge_analysis(&patch).expect("what the job found lands");
        assert!(project.get_analysis(asset.id).unwrap().is_some());

        // Another project is another generation, and drops the job.
        project.replace_with(Project::open_in_memory().unwrap()).close();
        assert!(require_generation(&project, noted, "it was being analysed").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The generation check polled from a job's cancel is asked at most once per interval.
    #[test]
    fn the_replaced_check_is_throttled_and_sticks_once_true() {
        use std::cell::Cell;
        let asked = Cell::new(0u32);
        let moved = Cell::new(false);
        let check = crate::Throttled::new(std::time::Duration::from_millis(60), || {
            asked.set(asked.get() + 1);
            moved.get()
        });
        for _ in 0..100 {
            assert!(!check.ask());
        }
        assert_eq!(asked.get(), 1, "a hundred polls in a hurry are one question");
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(!check.ask());
        assert_eq!(asked.get(), 2, "and it is asked again after the interval");
        moved.set(true);
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(check.ask(), "the project was replaced");
        let after = asked.get();
        assert!(check.ask() && check.ask());
        assert_eq!(asked.get(), after, "once it is true it stays true, unasked");
    }

    /// A job stops for the user's Stop or for a replaced project, whichever comes first.
    #[test]
    fn a_job_stops_for_a_stop_or_for_a_replaced_project() {
        use kerf_core::Project;
        let shared = std::sync::Mutex::new(Project::open_in_memory().unwrap());
        let generation = crate::lock_user(&shared).generation();
        let stop = StopEpoch::default();
        let cancel = crate::stop_or_replaced(stop.begin(), &shared, generation);
        assert!(!cancel(), "nothing happened");
        stop.stop();
        assert!(cancel(), "the user's Stop");

        let cancel = crate::stop_or_replaced(stop.begin(), &shared, generation);
        assert!(!cancel());
        crate::lock_user(&shared)
            .replace_with(Project::open_in_memory().unwrap())
            .close();
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(cancel(), "the project it began on was replaced");
    }

    /// Kerf's own settings and logs are never somewhere to write an output.
    #[test]
    fn an_output_is_refused_inside_kerfs_own_directories() {
        let config = std::env::temp_dir().join(format!("kerf-config-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&config).unwrap();
        let _ = crate::APP_DIRS.set(vec![(config.clone(), "Kerf's settings".to_string())]);
        let protected = kerf_core::paths::Protected::default();
        let inside = config.join("settings.json");
        let err = crate::check_output(&inside, &protected).unwrap_err().to_string();
        assert!(err.contains("Kerf's settings"), "{err}");
        assert!(crate::check_output(&config.join("anything").join("deeper.mp4"), &protected).is_err());
        let elsewhere = std::env::temp_dir().join("kerf-out-elsewhere.mp4");
        assert!(crate::check_output(&elsewhere, &protected).is_ok());
        let _ = std::fs::remove_dir_all(&config);
    }

    /// A page's id for its `n`th playback, as `playback-id.ts` builds it.
    fn page_id(session: u64, n: u64) -> u64 {
        (session << 32) | n
    }

    #[test]
    fn the_newest_playback_wins_whatever_order_the_calls_arrive_in() {
        let p = Playbacks::new();
        p.begin(6);
        p.begin(5);
        assert!(!p.ended(6), "a start that arrives late does not take the pane back");
        assert!(p.ended(5), "and its own stream ends at once");
        assert!(p.ended(7), "an id that never began is not running");
    }

    #[test]
    fn a_stop_that_arrives_before_its_start_still_ends_the_stream() {
        let p = Playbacks::new();
        p.stop(4);
        p.begin(4);
        assert!(p.ended(4), "the stream notices at its first frame");
        p.begin(5);
        assert!(!p.ended(5), "the next playback is not held back by it");
    }

    #[test]
    fn stopping_an_old_stream_does_not_end_the_one_that_replaced_it() {
        let p = Playbacks::new();
        p.begin(1);
        p.begin(2);
        p.stop(1);
        assert!(!p.ended(2));
        p.stop(2);
        assert!(p.ended(2));
        p.finish(2);
        p.begin(3);
        assert!(!p.ended(3));
    }

    #[test]
    fn a_reloaded_page_cannot_collide_with_the_ids_the_one_before_it_left_behind() {
        let p = Playbacks::new();
        let (old, new) = (playback_session(), playback_session());
        assert!(new > old, "each page gets a higher session than the last");
        // The first page played a lot and was reloaded mid-stream; its counter restarts at 1.
        p.begin(page_id(old, 900));
        p.stop(page_id(old, 899));
        p.begin(page_id(new, 1));
        assert!(p.ended(page_id(old, 900)), "the old page's stream is superseded");
        assert!(
            !p.ended(page_id(new, 1)),
            "the new page's first playback runs, though 1 < 900"
        );
        // A stop the old page sent last is not a stop of anything the new page plays.
        p.stop(page_id(old, 950));
        assert!(!p.ended(page_id(new, 1)));
        p.begin(page_id(new, 2));
        assert!(p.ended(page_id(new, 1)) && !p.ended(page_id(new, 2)));
    }

    #[test]
    fn the_ids_a_page_builds_stay_exact_in_a_javascript_number() {
        // 2^21 sessions of 2^32 ids: the largest is below 2^53, where doubles stop being exact.
        let largest = page_id((1 << 21) - 1, u64::from(u32::MAX));
        assert!(largest < (1 << 53));
        assert_eq!(largest as f64 as u64, largest);
    }
}
