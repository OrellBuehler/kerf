//! App preferences — the settings that belong to this machine rather than to a
//! project.
//!
//! A `.kerf` file describes a cut; how much of *your* computer Kerf may take
//! while it renders one is not part of that, and must not travel with the
//! project to another machine. So these live in the platform config directory as
//! plain JSON, are read once at launch and written on every change.
//!
//! Everything here is best-effort: an unreadable, undecodable or malformed file
//! falls back to the defaults rather than refusing to start, because a preference
//! is never worth failing a launch over — but the file is kept (moved aside), not
//! overwritten. A file that is valid JSON but holds a bad value reads field by
//! field: the bad one is its default and the rest are kept. Writes are atomic and
//! serialized, arrive as patches, and reach the disk before they reach the engine.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Set deliberately in the environment, this wins over the stored preference at
/// launch (see [`apply`]).
const CPU_ENV: &str = "KERF_CPU_PERCENT";

/// How the interface writes a time: frame-accurate timecode, or plain seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeDisplay {
    #[default]
    Timecode,
    Seconds,
}

/// The longest the recent-projects list grows.
pub const RECENT_MAX: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Share of the machine one heavy job (analysis, transcription, proxy,
    /// stitch, export) may take, in percent. See `kerf_core::engine::cpu`.
    pub cpu_percent: u8,
    /// Which analyses run by themselves when media is imported: a master switch and
    /// one per kind (silence, scenes, loudness, rhythm, transcript). Replaces the old
    /// `transcribe` flag, which a file written by an older build still carries: see
    /// [`migrate`].
    pub auto_analysis: kerf_core::AutoAnalysis,
    /// The pre-`auto_analysis` transcription flag. Read once to carry an older file
    /// forward and never written back.
    #[serde(skip_serializing)]
    pub transcribe: Option<bool>,
    /// How wide preview proxies are — 720, 1080 or 1280 pixels across, or 0 for none.
    /// The default is what Kerf always made, so its cache stays good.
    pub proxy_size: kerf_core::ProxySize,
    /// Which file the preview decodes: the proxy once it is ready (`auto`), always the
    /// original, or only the proxy (waiting for it). Export always reads the originals.
    pub preview_source: kerf_core::PreviewSource,
    /// Whether the preview shades the delivery safe areas — where a phone's own
    /// UI covers a vertical cut. Off by default: it is a check you turn on,
    /// not a view you cut behind.
    pub safe_areas: bool,
    /// Whether the Preview panel is drawn by the GPU compositor in a native surface where
    /// the plan allows it, instead of FFmpeg's JPEG for every frame. Experimental, and off
    /// by default: the surface is confirmed on Linux/X11 only (see `gpu_preview`).
    pub gpu_preview: bool,
    /// How times are written: `timecode` (frame-accurate, the default) or `seconds`.
    pub time_display: TimeDisplay,
    /// Open the project that was open last when Kerf starts without one named on its
    /// command line. Off by default: a launch starts empty.
    pub reopen_last_project: bool,
    /// The `.kerf` files opened or saved most recently, newest first, at most
    /// [`RECENT_MAX`]. Maintained by the app (`record_recent`, `recent`,
    /// `clear_recent`); a settings patch cannot write it.
    pub recent_projects: Vec<String>,
    /// The workspace arrangement (dockview's serialized layout). Opaque here:
    /// the frontend validates it and falls back to its default layout.
    pub layout: Option<serde_json::Value>,
    /// The color theme. Opaque for the same reason — the frontend owns the
    /// token list, presets and the JSON file format.
    pub theme: Option<serde_json::Value>,
    /// The workspaces: which one is active, the saved arrangement of each, and
    /// the state of the library rail. Opaque like `layout` and `theme` — the
    /// frontend owns the shape, validates it on the way back in and falls back
    /// to its presets. `layout` is kept as the pre-workspaces arrangement the
    /// frontend migrates into the Edit workspace when this is absent.
    pub workspaces: Option<serde_json::Value>,
    /// The keyboard shortcuts the user changed: `{ version, bindings: { <action
    /// id>: [<chord>, …] } }`, only the actions they touched — an untouched one
    /// follows the defaults of the running build. Opaque like the rest: the
    /// frontend owns the action registry, the chord syntax and the migration
    /// between versions, so a new action or a renamed one needs no change here.
    pub keybindings: Option<serde_json::Value>,
    /// The editing defaults — snapping on at launch, the length of a still image and of a title
    /// when one is placed: `{ snap, still_seconds, title_seconds }`. Opaque like the rest: the
    /// frontend owns the shape and clamps what it reads.
    pub editing: Option<serde_json::Value>,
    // ---- agent / MCP server (owned by `mcp.rs`; keep together) ----
    /// Whether an agent's edits are held back for the user to review before they reach the cut
    /// (the default). The agent-facing `set_settings` tool cannot change it.
    pub agent_review: bool,
    /// Whether the embedded MCP server listens at all.
    pub mcp_enabled: bool,
    /// Where it listens (`host:port`); `None` is `127.0.0.1:7777`. `KERF_MCP_ADDR` in the
    /// environment still overrides it.
    pub mcp_addr: Option<String>,
    /// Whether a request must carry `Authorization: Bearer <mcp_token>`.
    pub mcp_require_token: bool,
    /// The bearer token. Generated on the first start and kept here.
    pub mcp_token: Option<String>,
    // ---- end agent / MCP server ----
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cpu_percent: kerf_core::DEFAULT_CPU_PERCENT,
            auto_analysis: kerf_core::AutoAnalysis::default(),
            transcribe: None,
            proxy_size: kerf_core::ProxySize::default(),
            preview_source: kerf_core::PreviewSource::default(),
            safe_areas: false,
            gpu_preview: false,
            time_display: TimeDisplay::default(),
            reopen_last_project: false,
            recent_projects: Vec::new(),
            layout: None,
            theme: None,
            workspaces: None,
            keybindings: None,
            editing: None,
            agent_review: true,
            mcp_enabled: true,
            mcp_addr: None,
            mcp_require_token: true,
            mcp_token: None,
        }
    }
}

/// The safe-area preference in force. Nothing in the engine cares about it, so
/// unlike the CPU budget and the transcription flag it is held here.
static SAFE_AREAS: AtomicBool = AtomicBool::new(false);

pub fn safe_areas() -> bool {
    SAFE_AREAS.load(Ordering::Relaxed)
}

pub fn set_safe_areas(on: bool) {
    SAFE_AREAS.store(on, Ordering::Relaxed);
}

/// Whether an agent's edits wait for review. Every agent entry point reads it (and sets it on the
/// project it locks), so a change applies to the very next agent call. On until a setting says
/// otherwise: that is also what a call that races the startup sees.
static AGENT_REVIEW: AtomicBool = AtomicBool::new(true);

pub fn agent_review() -> bool {
    AGENT_REVIEW.load(Ordering::Relaxed)
}

pub fn set_agent_review(on: bool) {
    AGENT_REVIEW.store(on, Ordering::Relaxed);
}

/// What a settings patch must not do to the agent's door: a bad address or token, or a network
/// address with no token to guard it.
fn check_agent_fields(settings: &Settings) -> Result<(), String> {
    if let Some(addr) = settings.mcp_addr.as_deref() {
        crate::mcp::check_addr(addr)?;
    }
    if let Some(token) = settings.mcp_token.as_deref() {
        crate::mcp::check_token(token)?;
    }
    let addr = settings.mcp_addr.as_deref().unwrap_or(crate::mcp::DEFAULT_ADDR);
    match crate::mcp::bind_problem(addr, settings.mcp_require_token) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// What the settings surface actually shows: the stored preference resolved
/// against the engine, plus the machine it is a share *of*. The UI cannot work
/// out "9 of 12 cores" on its own — a webview's `hardwareConcurrency` is not
/// what ffmpeg sees.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub cpu_percent: u8,
    pub auto_analysis: kerf_core::AutoAnalysis,
    pub proxy_size: kerf_core::ProxySize,
    pub preview_source: kerf_core::PreviewSource,
    pub safe_areas: bool,
    pub gpu_preview: bool,
    pub time_display: TimeDisplay,
    pub reopen_last_project: bool,
    pub recent_projects: Vec<String>,
    pub cpu_cores: usize,
    pub cpu_threads: usize,
    pub cpu_min_percent: u8,
    pub layout: Option<serde_json::Value>,
    pub theme: Option<serde_json::Value>,
    pub workspaces: Option<serde_json::Value>,
    pub keybindings: Option<serde_json::Value>,
    pub editing: Option<serde_json::Value>,
    pub agent_review: bool,
    pub mcp_enabled: bool,
    pub mcp_addr: Option<String>,
    pub mcp_require_token: bool,
}

impl SettingsView {
    /// The engine-held preferences read straight from the engine rather than
    /// from the stored file, so what the dialog shows is what is actually in
    /// force — including an environment override the user set outside the
    /// app. The layout, theme, workspaces and keybindings only exist in the
    /// file, so those come from `stored`.
    pub fn current(stored: &Settings) -> Self {
        Self {
            cpu_percent: kerf_core::cpu_percent(),
            auto_analysis: kerf_core::auto_analysis(),
            proxy_size: kerf_core::proxy::proxy_size(),
            preview_source: kerf_core::proxy::preview_source_setting(),
            safe_areas: safe_areas(),
            gpu_preview: stored.gpu_preview,
            time_display: stored.time_display,
            reopen_last_project: stored.reopen_last_project,
            recent_projects: stored.recent_projects.clone(),
            cpu_cores: kerf_core::cpu_cores(),
            cpu_threads: kerf_core::cpu_threads(),
            cpu_min_percent: kerf_core::MIN_CPU_PERCENT,
            layout: stored.layout.clone(),
            theme: stored.theme.clone(),
            workspaces: stored.workspaces.clone(),
            keybindings: stored.keybindings.clone(),
            editing: stored.editing.clone(),
            agent_review: stored.agent_review,
            mcp_enabled: stored.mcp_enabled,
            mcp_addr: stored.mcp_addr.clone(),
            mcp_require_token: stored.mcp_require_token,
        }
    }
}

/// Serializes every read-modify-write of the settings file. The webview writes
/// layout, theme and the toggles from independent call sites, so two patches can
/// be in flight at once; without this the second would load the file before the
/// first had saved and drop its change.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// The keys a patch may carry — every field of [`Settings`] but `recent_projects`,
/// which the app keeps itself.
const KEYS: [&str; 18] = [
    "cpu_percent",
    "auto_analysis",
    "proxy_size",
    "preview_source",
    "safe_areas",
    "gpu_preview",
    "time_display",
    "reopen_last_project",
    "layout",
    "theme",
    "workspaces",
    "keybindings",
    "editing",
    "agent_review",
    "mcp_enabled",
    "mcp_addr",
    "mcp_require_token",
    "mcp_token",
];

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("settings.json"))
}

fn sibling(path: &Path, name: &str) -> PathBuf {
    path.with_file_name(name)
}

/// Replace `path` with `bytes` so a crash leaves the old file or the new one,
/// never a truncated one: write a temp file beside it, fsync, rename over.
/// `std::fs::rename` replaces an existing file on every platform (Windows uses
/// `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = sibling(path, &format!("settings.tmp-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        // The file holds the agent token: readable by the user alone (a no-op on Windows, where the
        // profile directory already is).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Read the stored preferences, falling back to the defaults for anything
/// missing. A file that cannot be read, is not UTF-8, is not JSON or is not an
/// object is moved aside to `settings.corrupt-<unix-ms>.json` first (the next save
/// would otherwise overwrite someone's imported theme and layout with defaults); a
/// file that parses but holds a bad value for one field keeps the rest ([`parse`]).
fn load_from(file: &Path) -> Settings {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            move_aside(file, &format!("could not read it: {e}"));
            return Settings::default();
        }
    };
    let parsed = String::from_utf8(bytes)
        .map_err(|e| format!("not valid UTF-8: {e}"))
        .and_then(|raw| parse(&raw).map_err(|e| e.to_string()));
    match parsed {
        Ok(settings) => settings,
        Err(why) => {
            move_aside(file, &why);
            Settings::default()
        }
    }
}

/// Keep an unusable settings file as `settings.corrupt-<unix-ms>.json` beside itself.
fn move_aside(file: &Path, why: &str) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let aside = sibling(file, &format!("settings.corrupt-{ms}.json"));
    match std::fs::rename(file, &aside) {
        Ok(()) => tracing::warn!(
            error = %why,
            path = %file.display(),
            kept_as = %aside.display(),
            "unusable settings moved aside; using defaults"
        ),
        Err(re) => tracing::warn!(
            error = %why,
            rename_error = %re,
            path = %file.display(),
            "unusable settings could not be moved aside; using defaults"
        ),
    }
}

/// Read a settings file, carrying an older one forward ([`migrate`]).
///
/// The text must be a JSON object. Past that it is read **field by field**: a value
/// that does not fit its field (an unknown enum name, a number out of the type's range,
/// the wrong JSON type) is logged and leaves that one field at its default, so a
/// hand-edited or newer-build file keeps everything else: theme, layout, shortcuts.
fn parse(raw: &str) -> Result<Settings, serde_json::Error> {
    let serde_json::Value::Object(file) = serde_json::from_str(raw)? else {
        return Err(serde_json::Error::custom("the settings file must hold a JSON object"));
    };
    let mut kept = serde_json::Map::new();
    for (key, value) in file {
        let probe = serde_json::Map::from_iter([(key.clone(), value.clone())]);
        match serde_json::from_value::<Settings>(serde_json::Value::Object(probe)) {
            Ok(_) => {
                kept.insert(key, value);
            }
            Err(e) => tracing::warn!(setting = %key, error = %e, "ignoring an invalid setting; its default is used"),
        }
    }
    let has_auto_analysis = kept.contains_key("auto_analysis");
    let mut settings: Settings = serde_json::from_value(serde_json::Value::Object(kept))?;
    migrate(&mut settings, has_auto_analysis);
    keep_agent_door_sound(&mut settings);
    Ok(settings)
}

/// A hand-edited file can hold a token too short to protect anything, or an address the server
/// cannot listen on: the token is made again at the next start, and the address falls back to the
/// default (the same two checks a patch goes through on its way in).
fn keep_agent_door_sound(settings: &mut Settings) {
    if settings
        .mcp_token
        .as_deref()
        .is_some_and(|t| crate::mcp::check_token(t).is_err())
    {
        tracing::warn!("the stored MCP token is too weak to use; a new one is made at the next start");
        settings.mcp_token = None;
    }
    if settings
        .mcp_addr
        .as_deref()
        .is_some_and(|a| crate::mcp::check_addr(a).is_err())
    {
        tracing::warn!("the stored MCP address is not one the server can listen on; the default is used");
        settings.mcp_addr = None;
    }
}

/// The `transcribe` flag of a file written before `auto_analysis` existed becomes
/// that set's `transcript` switch (everything else stays on, as it always was). A
/// file that has both keeps `auto_analysis`: the newer one is the one the user last
/// touched. Either way the old flag is dropped, so the next save no longer has it.
fn migrate(settings: &mut Settings, has_auto_analysis: bool) {
    if let Some(transcribe) = settings.transcribe.take() {
        if !has_auto_analysis {
            settings.auto_analysis.transcript = transcribe;
        }
    }
}

fn save_to(file: &Path, settings: &Settings) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    write_atomic(file, raw.as_bytes()).map_err(|e| format!("could not write settings: {e}"))
}

/// Merge a patch — a JSON object holding only the fields that changed — into
/// `settings`. A field present with `null` clears it (the layout, theme,
/// workspaces and keybindings are nullable); a field left out is untouched.
fn merge(settings: &Settings, patch: &serde_json::Value) -> Result<Settings, String> {
    let serde_json::Value::Object(patch) = patch else {
        return Err("a settings patch must be an object".to_string());
    };
    if let Some(unknown) = patch.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown setting `{unknown}`"));
    }
    let serde_json::Value::Object(mut merged) = serde_json::to_value(settings).map_err(|e| e.to_string())? else {
        return Err("settings did not serialize to an object".to_string());
    };
    for (key, value) in patch {
        merged.insert(key.clone(), value.clone());
    }
    let merged: Settings =
        serde_json::from_value(serde_json::Value::Object(merged)).map_err(|e| format!("invalid settings: {e}"))?;
    // Only a patch that touches the agent's door is held to its rules: a stored file with an odd
    // address in it must not stop the theme from being saved.
    if patch.keys().any(|k| k.starts_with("mcp_")) {
        check_agent_fields(&merged)?;
    }
    Ok(merged)
}

/// The stored preferences (see [`load_from`]).
pub fn load(app: &AppHandle) -> Settings {
    let Some(file) = path(app) else {
        return Settings::default();
    };
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_from(&file)
}

/// The CPU share as the engine will hold it: the engine clamps to this range, and the
/// clamped number is what is stored, so an out-of-range one is not re-clamped on every launch.
fn clamp_cpu(percent: u8) -> u8 {
    percent.clamp(kerf_core::MIN_CPU_PERCENT, 100)
}

/// Merge `patch` into the stored preferences under the file lock, persist the result
/// and **then** put what changed into force: a write that fails (a full disk, a
/// read-only config directory) is an error and changes nothing, instead of leaving the
/// running app on settings the next launch will not have. Only the fields the patch
/// carries reach the engine: re-applying the stored CPU budget because the layout moved
/// would undo a `KERF_CPU_PERCENT` override.
pub fn update(app: &AppHandle, patch: &serde_json::Value) -> Result<Settings, String> {
    let file = path(app).ok_or("no config directory available for settings")?;
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    update_file(&file, patch)
}

fn update_file(file: &Path, patch: &serde_json::Value) -> Result<Settings, String> {
    let mut merged = merge(&load_from(file), patch)?;
    if patch.get("cpu_percent").is_some() {
        merged.cpu_percent = clamp_cpu(merged.cpu_percent);
    }
    save_to(file, &merged)?;
    if patch.get("auto_analysis").is_some() {
        kerf_core::set_auto_analysis(merged.auto_analysis);
    }
    if patch.get("proxy_size").is_some() {
        kerf_core::proxy::set_proxy_size(merged.proxy_size);
    }
    if patch.get("preview_source").is_some() {
        kerf_core::proxy::set_preview_source_setting(merged.preview_source);
    }
    if patch.get("safe_areas").is_some() {
        set_safe_areas(merged.safe_areas);
    }
    if patch.get("cpu_percent").is_some() {
        kerf_core::set_cpu_percent(merged.cpu_percent);
    }
    if patch.get("agent_review").is_some() {
        set_agent_review(merged.agent_review);
    }
    // The MCP server follows its own fields (a new address restarts it, a new token applies at once).
    crate::mcp::settings_changed(&merged, patch);
    Ok(merged)
}

/// The preferences a reset starts from: the defaults, with what is not a preference kept
/// (the recent-projects list). The one place to add anything else a reset must not lose.
fn reset_keep(current: &Settings) -> Settings {
    Settings {
        recent_projects: current.recent_projects.clone(),
        // The agent's door is not a preference to put back: a reset must not change where the
        // server listens or lock out the clients that hold its token. (The review switch is a
        // preference, and goes back on.)
        mcp_enabled: current.mcp_enabled,
        mcp_addr: current.mcp_addr.clone(),
        mcp_require_token: current.mcp_require_token,
        mcp_token: current.mcp_token.clone(),
        ..Settings::default()
    }
}

/// Put every preference back to its default and into force, keeping the old file as
/// `settings.reset-<unix-ms>.json` beside it so a reset is never the end of a theme or a
/// layout. `KERF_CPU_PERCENT` does not hold back an explicit reset.
pub fn reset(app: &AppHandle) -> Result<Settings, String> {
    let file = path(app).ok_or("no config directory available for settings")?;
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_file(&file)
}

fn reset_file(file: &Path) -> Result<Settings, String> {
    let fresh = reset_keep(&load_from(file));
    if file.exists() {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        if let Err(e) = std::fs::copy(file, sibling(file, &format!("settings.reset-{ms}.json"))) {
            tracing::warn!(error = %e, "could not keep a copy of the settings being reset");
        }
    }
    save_to(file, &fresh)?;
    kerf_core::set_auto_analysis(fresh.auto_analysis);
    kerf_core::proxy::set_proxy_size(fresh.proxy_size);
    kerf_core::proxy::set_preview_source_setting(fresh.preview_source);
    set_safe_areas(fresh.safe_areas);
    set_agent_review(fresh.agent_review);
    kerf_core::set_cpu_percent(fresh.cpu_percent);
    Ok(fresh)
}

/// Where the settings file lives (it may not exist yet).
pub fn file_path(app: &AppHandle) -> Option<PathBuf> {
    path(app)
}

// ---- recent projects -------------------------------------------------------

/// Put `path` at the front of `list`: a project opened again moves up rather than
/// appearing twice, and the list never grows past [`RECENT_MAX`].
pub fn push_recent(list: &mut Vec<String>, path: &str) {
    list.retain(|p| p != path);
    list.insert(0, path.to_string());
    list.truncate(RECENT_MAX);
}

/// Drop the entries `exists` says are gone; whether the list changed.
pub fn prune_recent(list: &mut Vec<String>, exists: impl Fn(&Path) -> bool) -> bool {
    let before = list.len();
    list.retain(|p| exists(Path::new(p)));
    list.len() != before
}

/// What a launch with nothing named on its command line should open: the newest recent
/// project, when the setting is on and the file is still there.
pub fn reopen_target(settings: &Settings, exists: impl Fn(&Path) -> bool) -> Option<String> {
    if !settings.reopen_last_project {
        return None;
    }
    settings.recent_projects.iter().find(|p| exists(Path::new(p))).cloned()
}

/// Edit the stored list under the file lock and write it back if `edit` says it changed.
fn edit_recent(app: &AppHandle, edit: impl FnOnce(&mut Vec<String>) -> bool) -> Result<Vec<String>, String> {
    let file = path(app).ok_or("no config directory available for settings")?;
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    edit_recent_file(&file, edit)
}

fn edit_recent_file(file: &Path, edit: impl FnOnce(&mut Vec<String>) -> bool) -> Result<Vec<String>, String> {
    let mut stored = load_from(file);
    if edit(&mut stored.recent_projects) {
        save_to(file, &stored)?;
    }
    Ok(stored.recent_projects)
}

/// Note that `path` was just opened or saved to. Touches the disk: call it off the
/// main thread.
pub fn record_recent(app: &AppHandle, path: &str) -> Result<Vec<String>, String> {
    edit_recent(app, |list| {
        push_recent(list, path);
        true
    })
}

/// The recent projects, the ones whose file is gone dropped (and forgotten). Stats the
/// files: call it off the main thread.
pub fn recent(app: &AppHandle) -> Result<Vec<String>, String> {
    edit_recent(app, |list| prune_recent(list, Path::is_file))
}

/// Forget every recent project.
pub fn clear_recent(app: &AppHandle) -> Result<Vec<String>, String> {
    edit_recent(app, |list| {
        let had = !list.is_empty();
        list.clear();
        had
    })
}

/// Push the preferences into the engine.
///
/// `KERF_CPU_PERCENT` deliberately wins at launch: someone who set it in the
/// environment meant it for this run. Moving the slider afterwards still takes
/// effect — a runtime choice is the newer instruction of the two.
pub fn apply(settings: &Settings) {
    kerf_core::set_auto_analysis(settings.auto_analysis);
    kerf_core::proxy::set_proxy_size(settings.proxy_size);
    kerf_core::proxy::set_preview_source_setting(settings.preview_source);
    set_safe_areas(settings.safe_areas);
    set_agent_review(settings.agent_review);
    if std::env::var_os(CPU_ENV).is_some() {
        tracing::info!(percent = kerf_core::cpu_percent(), "CPU budget set from {CPU_ENV}");
        return;
    }
    let applied = kerf_core::set_cpu_percent(settings.cpu_percent);
    tracing::info!(percent = applied, cores = kerf_core::cpu_cores(), "CPU budget applied");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_file_reads_as_defaults_for_what_it_lacks() {
        let s: Settings = serde_json::from_str(r#"{"cpu_percent": 50}"#).unwrap();
        assert_eq!(s.cpu_percent, 50);
        assert_eq!(s.auto_analysis, kerf_core::AutoAnalysis::default());
        assert_eq!(s.proxy_size, kerf_core::ProxySize::W1280);
        assert_eq!(s.preview_source, kerf_core::PreviewSource::Auto);
        assert!(!s.gpu_preview, "the GPU preview is opt-in");
        assert_eq!(s.time_display, TimeDisplay::Timecode);
        assert!(!s.reopen_last_project, "a launch starts empty unless asked otherwise");
        assert!(s.recent_projects.is_empty());
        assert!(s.layout.is_none());
        assert!(s.theme.is_none());
        assert!(s.workspaces.is_none());
        assert!(s.keybindings.is_none());
        assert!(s.editing.is_none());
    }

    #[test]
    fn layout_and_theme_round_trip_untouched() {
        let layout = serde_json::json!({"grid": {"root": {"type": "leaf"}}, "panels": {}});
        let theme = serde_json::json!({"name": "Mine", "version": 1, "colors": {"kerf-500": "#ffffff"}});
        let s = Settings {
            layout: Some(layout.clone()),
            theme: Some(theme.clone()),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.layout, Some(layout));
        assert_eq!(back.theme, Some(theme));
    }

    #[test]
    fn workspaces_round_trip_untouched_beside_the_legacy_layout() {
        let layout = serde_json::json!({"grid": {"root": {"type": "leaf"}}, "panels": {}});
        let workspaces = serde_json::json!({
            "active": "color",
            "layouts": {"edit": {"grid": {}, "panels": {}}},
            "library": {"tab": "effects", "collapsed": true}
        });
        let s = Settings {
            layout: Some(layout.clone()),
            workspaces: Some(workspaces.clone()),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.workspaces, Some(workspaces.clone()));
        assert_eq!(back.layout, Some(layout));
        // And it is what the webview is handed back.
        assert_eq!(SettingsView::current(&back).workspaces, Some(workspaces));
    }

    #[test]
    fn a_workspaces_patch_changes_only_that_field() {
        let base = Settings {
            cpu_percent: 40,
            layout: Some(serde_json::json!({"grid": 1})),
            theme: Some(serde_json::json!({"name": "Mine"})),
            workspaces: Some(serde_json::json!({"active": "edit"})),
            ..Settings::default()
        };
        let next = serde_json::json!({"active": "audio", "library": {"tab": "audio", "collapsed": false}});
        let merged = merge(&base, &serde_json::json!({ "workspaces": next })).unwrap();
        assert_eq!(merged.workspaces, Some(next));
        assert_eq!(merged.cpu_percent, 40);
        assert_eq!(merged.layout, base.layout);
        assert_eq!(merged.theme, base.theme);
        // A null clears it, back to "never customized".
        let cleared = merge(&merged, &serde_json::json!({"workspaces": null})).unwrap();
        assert!(cleared.workspaces.is_none());
    }

    #[test]
    fn keybindings_round_trip_untouched_and_reach_the_view() {
        let keybindings = serde_json::json!({
            "version": 1,
            "bindings": {"playback.toggle": ["P"], "edit.undo": [], "edit.redo": ["Mod+Shift+Z", "Mod+Y"]}
        });
        let s = Settings {
            keybindings: Some(keybindings.clone()),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.keybindings, Some(keybindings.clone()));
        // An empty list is a choice (unbound) and survives as one.
        assert_eq!(SettingsView::current(&back).keybindings, Some(keybindings));
    }

    #[test]
    fn a_keybindings_patch_changes_only_that_field() {
        let base = Settings {
            cpu_percent: 40,
            theme: Some(serde_json::json!({"name": "Mine"})),
            workspaces: Some(serde_json::json!({"active": "edit"})),
            keybindings: Some(serde_json::json!({"version": 1, "bindings": {"tool.razor": ["B"]}})),
            ..Settings::default()
        };
        let next = serde_json::json!({"version": 1, "bindings": {"playback.toggle": ["P"]}});
        let merged = merge(&base, &serde_json::json!({ "keybindings": next })).unwrap();
        assert_eq!(merged.keybindings, Some(next));
        assert_eq!(merged.cpu_percent, 40);
        assert_eq!(merged.theme, base.theme);
        assert_eq!(merged.workspaces, base.workspaces);
        // A patch for something else leaves the shortcuts alone…
        let other = merge(&base, &serde_json::json!({"safe_areas": true})).unwrap();
        assert_eq!(other.keybindings, base.keybindings);
        // …and a null is "back to the defaults".
        let cleared = merge(&merged, &serde_json::json!({"keybindings": null})).unwrap();
        assert!(cleared.keybindings.is_none());
    }

    #[test]
    fn editing_defaults_round_trip_untouched_patch_alone_and_reset() {
        let editing = serde_json::json!({"snap": false, "still_seconds": 8, "title_seconds": 4});
        let s = Settings {
            editing: Some(editing.clone()),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.editing, Some(editing.clone()));
        assert_eq!(SettingsView::current(&back).editing, Some(editing.clone()));

        let merged = merge(&Settings::default(), &serde_json::json!({ "editing": editing })).unwrap();
        assert_eq!(merged.editing, Some(editing));
        let other = merge(&merged, &serde_json::json!({"safe_areas": true})).unwrap();
        assert_eq!(other.editing, merged.editing, "another patch leaves the defaults alone");
        let cleared = merge(&merged, &serde_json::json!({"editing": null})).unwrap();
        assert!(cleared.editing.is_none());
        assert!(reset_keep(&merged).editing.is_none(), "a reset puts them back");
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-settings-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_patch_changes_only_the_fields_it_carries() {
        let base = Settings {
            cpu_percent: 40,
            layout: Some(serde_json::json!({"grid": 1})),
            theme: Some(serde_json::json!({"name": "Mine"})),
            ..Settings::default()
        };
        let merged = merge(&base, &serde_json::json!({"auto_analysis": {"transcript": false}})).unwrap();
        assert!(!merged.auto_analysis.transcript);
        assert!(merged.auto_analysis.silence, "the rest of the set is its default");
        assert_eq!(merged.cpu_percent, 40);
        assert_eq!(merged.layout, base.layout);
        assert_eq!(merged.theme, base.theme);
    }

    #[test]
    fn the_gpu_preview_is_a_patchable_field_and_reaches_the_view() {
        let base = Settings::default();
        let on = merge(&base, &serde_json::json!({"gpu_preview": true})).unwrap();
        assert!(on.gpu_preview);
        assert_eq!(on.cpu_percent, base.cpu_percent, "nothing else moved");
        assert!(SettingsView::current(&on).gpu_preview);
        assert!(!SettingsView::current(&base).gpu_preview);
        assert!(merge(&base, &serde_json::json!({"gpu_preview": "yes"})).is_err());
    }

    #[test]
    fn a_null_in_a_patch_clears_a_field() {
        let base = Settings {
            theme: Some(serde_json::json!({"name": "Mine"})),
            ..Settings::default()
        };
        let merged = merge(&base, &serde_json::json!({"theme": null})).unwrap();
        assert!(merged.theme.is_none());
    }

    #[test]
    fn a_bad_patch_is_refused() {
        let base = Settings::default();
        assert!(merge(&base, &serde_json::json!({"cpu_percnt": 5})).is_err());
        assert!(merge(&base, &serde_json::json!({"cpu_percent": "lots"})).is_err());
        assert!(merge(&base, &serde_json::json!([1])).is_err());
    }

    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_no_temp() {
        let dir = scratch();
        let file = dir.join("settings.json");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("settings.json")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_settings_file_is_readable_by_its_owner_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let file = dir.join("settings.json");
        save_to(&file, &Settings::default()).unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "it holds the MCP token");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = scratch();
        let file = dir.join("nested").join("settings.json");
        let keybindings = serde_json::json!({"version": 1, "bindings": {"tool.razor": ["B"]}});
        let s = Settings {
            cpu_percent: 33,
            safe_areas: true,
            gpu_preview: true,
            keybindings: Some(keybindings.clone()),
            ..Settings::default()
        };
        save_to(&file, &s).unwrap();
        let back = load_from(&file);
        assert_eq!(back.cpu_percent, 33);
        assert!(back.safe_areas);
        assert!(back.gpu_preview);
        assert_eq!(back.keybindings, Some(keybindings));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_not_overwritten() {
        let dir = scratch();
        let file = dir.join("settings.json");
        std::fs::write(&file, "{ \"theme\": {oops").unwrap();
        let loaded = load_from(&file);
        assert_eq!(loaded.cpu_percent, Settings::default().cpu_percent);
        assert!(!file.exists());
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("settings.corrupt-") && n.ends_with(".json"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(dir.join(&aside[0])).unwrap(), "{ \"theme\": {oops");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_just_the_defaults() {
        let dir = scratch();
        let loaded = load_from(&dir.join("settings.json"));
        assert!(loaded.theme.is_none());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_old_transcribe_flag_becomes_the_transcript_switch_and_is_not_written_back() {
        let off = parse(r#"{"cpu_percent": 40, "transcribe": false}"#).unwrap();
        assert!(!off.auto_analysis.transcript);
        assert!(
            off.auto_analysis.enabled && off.auto_analysis.silence && off.auto_analysis.scenes,
            "everything else stays on, as it always was"
        );
        assert_eq!(off.cpu_percent, 40);
        assert!(off.transcribe.is_none(), "the flag is consumed");
        let saved = serde_json::to_value(&off).unwrap();
        assert!(saved.get("transcribe").is_none(), "the next save drops it: {saved}");
        assert_eq!(saved["auto_analysis"]["transcript"], false);

        let on = parse(r#"{"transcribe": true}"#).unwrap();
        assert!(on.auto_analysis.transcript);

        // A file with neither is the defaults.
        assert_eq!(parse("{}").unwrap().auto_analysis, kerf_core::AutoAnalysis::default());
    }

    #[test]
    fn a_file_that_has_both_keeps_the_newer_auto_analysis() {
        let both = parse(
            r#"{"transcribe": false, "auto_analysis": {"enabled": true, "silence": true, "scenes": true, "loudness": true, "rhythm": true, "transcript": true}}"#,
        )
        .unwrap();
        assert!(
            both.auto_analysis.transcript,
            "the set the user chose last wins over the old flag"
        );
        assert!(both.transcribe.is_none());
    }

    #[test]
    fn analysis_proxy_and_preview_settings_patch_round_trip_and_reach_the_view() {
        let base = Settings::default();
        let merged = merge(
            &base,
            &serde_json::json!({
                "auto_analysis": {"enabled": false, "silence": true, "scenes": false, "loudness": true, "rhythm": true, "transcript": false},
                "proxy_size": 720,
                "preview_source": "proxy_only"
            }),
        )
        .unwrap();
        assert!(!merged.auto_analysis.enabled && !merged.auto_analysis.scenes);
        assert_eq!(merged.proxy_size, kerf_core::ProxySize::W720);
        assert_eq!(merged.preview_source, kerf_core::PreviewSource::ProxyOnly);
        assert_eq!(merged.cpu_percent, base.cpu_percent, "nothing else moved");
        let back: Settings = serde_json::from_str(&serde_json::to_string(&merged).unwrap()).unwrap();
        assert_eq!(back.proxy_size, kerf_core::ProxySize::W720);
        assert_eq!(back.auto_analysis, merged.auto_analysis);
        let view = serde_json::to_value(SettingsView::current(&merged)).unwrap();
        assert!(view.get("transcribe").is_none(), "the old key is gone from the view");
        assert!(view.get("auto_analysis").is_some() && view.get("proxy_size").is_some() && view.get("preview_source").is_some());
        // A size this build does not offer rounds to one it does, and a value that is no
        // number is refused as a bad patch.
        let odd = merge(&base, &serde_json::json!({"proxy_size": 900})).unwrap();
        assert_eq!(odd.proxy_size, kerf_core::ProxySize::W1080);
        assert!(merge(&base, &serde_json::json!({"proxy_size": "big"})).is_err());
        assert!(
            merge(&base, &serde_json::json!({"transcribe": false})).is_err(),
            "the old key is no longer a setting"
        );
        // A stored source this build has not heard of is Auto, not a corrupt file.
        let unknown = parse(r#"{"preview_source": "somewhere_else"}"#).unwrap();
        assert_eq!(unknown.preview_source, kerf_core::PreviewSource::Auto);
    }

    #[test]
    fn the_agent_door_is_closed_by_default() {
        let s = Settings::default();
        assert!(s.agent_review, "an agent's edits wait for review");
        assert!(s.mcp_enabled);
        assert!(s.mcp_require_token, "a token is required");
        assert_eq!(s.mcp_addr, None, "the default address is loopback");
        assert_eq!(s.mcp_token, None, "the token is made on the first start");
        let old: Settings = serde_json::from_str(r#"{"cpu_percent": 50}"#).unwrap();
        assert!(
            old.agent_review && old.mcp_require_token,
            "an older file gets the safe values"
        );
    }

    #[test]
    fn every_stored_field_can_be_patched_and_nothing_else() {
        let stored = serde_json::to_value(Settings::default()).unwrap();
        for key in stored.as_object().unwrap().keys() {
            // The recent-projects list is the app's own, kept as projects open and save.
            if key == "recent_projects" {
                continue;
            }
            assert!(KEYS.contains(&key.as_str()), "`{key}` is a setting but cannot be patched");
        }
        for key in KEYS {
            assert!(stored.get(key).is_some(), "`{key}` can be patched but is not stored");
        }
    }

    #[test]
    fn the_view_shows_the_switches_but_never_the_token() {
        let s = Settings {
            mcp_token: Some("s".repeat(64)),
            agent_review: false,
            ..Settings::default()
        };
        let view = serde_json::to_value(SettingsView::current(&s)).unwrap();
        assert_eq!(view["agent_review"], false);
        assert_eq!(view["mcp_require_token"], true);
        assert!(view.get("mcp_token").is_none(), "{view}");
        assert!(!view.to_string().contains(&"s".repeat(64)));
    }

    #[test]
    fn an_agent_patch_changes_only_its_own_fields() {
        let base = Settings {
            cpu_percent: 40,
            theme: Some(serde_json::json!({"name": "Mine"})),
            ..Settings::default()
        };
        let merged = merge(
            &base,
            &serde_json::json!({"agent_review": false, "mcp_addr": "127.0.0.1:9000"}),
        )
        .unwrap();
        assert!(!merged.agent_review);
        assert_eq!(merged.mcp_addr.as_deref(), Some("127.0.0.1:9000"));
        assert_eq!(merged.cpu_percent, 40);
        assert_eq!(merged.theme, base.theme);
        assert!(merged.mcp_require_token);
        let cleared = merge(&merged, &serde_json::json!({"mcp_addr": null})).unwrap();
        assert_eq!(cleared.mcp_addr, None, "null goes back to the default");
    }

    #[test]
    fn a_bad_address_or_token_is_refused() {
        let base = Settings::default();
        for bad in ["7777", "example.com:80", "", "127.0.0.1"] {
            assert!(merge(&base, &serde_json::json!({ "mcp_addr": bad })).is_err(), "{bad:?}");
        }
        assert!(merge(&base, &serde_json::json!({"mcp_token": "short"})).is_err());
        assert!(merge(&base, &serde_json::json!({"mcp_token": "a".repeat(40)})).is_ok());
        assert!(merge(&base, &serde_json::json!({"mcp_enabled": "yes"})).is_err());
    }

    #[test]
    fn the_server_cannot_be_opened_to_the_network_without_a_token() {
        let base = Settings::default();
        let err = merge(
            &base,
            &serde_json::json!({"mcp_addr": "0.0.0.0:7777", "mcp_require_token": false}),
        )
        .unwrap_err();
        assert!(err.contains("0.0.0.0:7777") && err.contains("token"), "{err}");
        assert!(
            merge(&base, &serde_json::json!({"mcp_addr": "0.0.0.0:7777"})).is_ok(),
            "with the token it may"
        );
        assert!(
            merge(&base, &serde_json::json!({"mcp_require_token": false})).is_ok(),
            "loopback needs none"
        );
        let open = Settings {
            mcp_addr: Some("0.0.0.0:7777".to_string()),
            ..Settings::default()
        };
        let err = merge(&open, &serde_json::json!({"mcp_require_token": false})).unwrap_err();
        assert!(err.contains("token"), "{err}");
    }

    #[test]
    fn a_stored_oddity_does_not_block_an_unrelated_patch() {
        let odd = Settings {
            mcp_addr: Some("not an address".to_string()),
            ..Settings::default()
        };
        let merged = merge(&odd, &serde_json::json!({"safe_areas": true})).unwrap();
        assert!(
            merged.safe_areas,
            "only a patch that touches the agent's door is held to its rules"
        );
    }

    #[test]
    fn the_agent_review_switch_is_one_the_agent_tools_read() {
        let before = agent_review();
        set_agent_review(false);
        assert!(!agent_review());
        set_agent_review(true);
        assert!(agent_review());
        set_agent_review(before);
    }

    #[test]
    fn a_bad_value_costs_only_its_own_field() {
        let raw = r#"{
            "cpu_percent": 500,
            "proxy_size": "banana",
            "time_display": "frames",
            "safe_areas": true,
            "reopen_last_project": "yes",
            "auto_analysis": {"silence": 7},
            "recent_projects": ["/a.kerf", 3],
            "theme": {"name": "Mine"},
            "keybindings": {"version": 1, "bindings": {}},
            "from_a_newer_build": [1, 2]
        }"#;
        let s = parse(raw).unwrap();
        let d = Settings::default();
        assert_eq!(s.cpu_percent, d.cpu_percent, "500 does not fit a percentage");
        assert_eq!(s.proxy_size, d.proxy_size);
        assert_eq!(s.time_display, TimeDisplay::Timecode, "an unknown enum name is the default");
        assert!(!s.reopen_last_project);
        assert_eq!(s.auto_analysis, d.auto_analysis, "a bad member costs the whole set, no more");
        assert!(s.recent_projects.is_empty());
        assert!(s.safe_areas, "a good neighbour is kept");
        assert_eq!(s.theme, Some(serde_json::json!({"name": "Mine"})));
        assert!(s.keybindings.is_some());
    }

    #[test]
    fn a_lenient_read_keeps_the_good_fields_and_the_file_in_place() {
        let dir = scratch();
        let file = dir.join("settings.json");
        std::fs::write(&file, r#"{"cpu_percent": 33, "time_display": "seconds", "proxy_size": {}}"#).unwrap();
        let s = load_from(&file);
        assert_eq!(s.cpu_percent, 33);
        assert_eq!(s.time_display, TimeDisplay::Seconds);
        assert!(file.exists(), "a file that parses is not moved aside");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn asides(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("settings.corrupt-"))
            .collect()
    }

    #[test]
    fn a_file_that_is_not_text_is_moved_aside_too() {
        let dir = scratch();
        let file = dir.join("settings.json");
        std::fs::write(&file, [0xFF, 0xFE, 0x00, 0x7B, 0x80]).unwrap();
        assert_eq!(load_from(&file).cpu_percent, Settings::default().cpu_percent);
        assert!(!file.exists());
        assert_eq!(asides(&dir).len(), 1, "kept, not deleted");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn json_that_is_not_an_object_is_moved_aside() {
        for raw in ["[1, 2]", "null", "\"settings\"", "", "   "] {
            let dir = scratch();
            let file = dir.join("settings.json");
            std::fs::write(&file, raw).unwrap();
            let _ = load_from(&file);
            assert!(!file.exists(), "{raw:?} should have been moved aside");
            assert_eq!(asides(&dir).len(), 1, "{raw:?}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_is_moved_aside() {
        let dir = scratch();
        let file = dir.join("settings.json");
        std::fs::create_dir(&file).unwrap();
        let _ = load_from(&file);
        assert!(!file.exists());
        assert_eq!(asides(&dir).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Everything that touches the engine's process-wide preferences, in one test so two
    /// of them never race for the same global.
    #[test]
    fn a_patch_is_saved_before_it_is_put_into_force_and_a_reset_goes_back_to_the_defaults() {
        let dir = scratch();
        let before = safe_areas();

        // A config "directory" that is a file: the write cannot happen, so nothing changes.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let unwritable = blocker.join("settings.json");
        let err = update_file(&unwritable, &serde_json::json!({"safe_areas": !before})).unwrap_err();
        assert!(err.contains("could not write settings"), "{err}");
        assert_eq!(safe_areas(), before, "a patch that did not reach the disk is not in force");

        let file = dir.join("settings.json");
        let saved = update_file(&file, &serde_json::json!({"safe_areas": !before, "cpu_percent": 3})).unwrap();
        assert_eq!(safe_areas(), !before);
        assert_eq!(
            saved.cpu_percent,
            kerf_core::MIN_CPU_PERCENT,
            "the clamped share is what is stored"
        );
        assert_eq!(load_from(&file).cpu_percent, kerf_core::MIN_CPU_PERCENT);

        edit_recent_file(&file, |l| {
            push_recent(l, "/p/a.kerf");
            true
        })
        .unwrap();
        let fresh = reset_file(&file).unwrap();
        assert_eq!(fresh.cpu_percent, Settings::default().cpu_percent);
        assert!(!safe_areas(), "a reset is in force");
        assert_eq!(
            fresh.recent_projects,
            vec!["/p/a.kerf".to_string()],
            "the recent list is not a preference"
        );
        let on_disk = load_from(&file);
        assert_eq!(on_disk.cpu_percent, Settings::default().cpu_percent);
        let kept: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("settings.reset-"))
            .collect();
        assert_eq!(kept.len(), 1, "the file that was reset is kept: {kept:?}");
        set_safe_areas(before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_recent_list_moves_a_reopened_project_up_and_stops_at_ten() {
        let mut list: Vec<String> = Vec::new();
        for i in 0..12 {
            push_recent(&mut list, &format!("/p/{i}.kerf"));
        }
        assert_eq!(list.len(), RECENT_MAX);
        assert_eq!(list[0], "/p/11.kerf");
        assert_eq!(list[RECENT_MAX - 1], "/p/2.kerf", "the oldest two fell off");
        push_recent(&mut list, "/p/5.kerf");
        assert_eq!(list[0], "/p/5.kerf");
        assert_eq!(list.iter().filter(|p| *p == "/p/5.kerf").count(), 1, "never twice");
        assert_eq!(list.len(), RECENT_MAX);
    }

    #[test]
    fn missing_projects_are_pruned_and_a_patch_cannot_write_the_list() {
        let mut list = vec!["/p/a.kerf".to_string(), "/p/gone.kerf".to_string(), "/p/b.kerf".to_string()];
        let changed = prune_recent(&mut list, |p| !p.ends_with("gone.kerf"));
        assert!(changed);
        assert_eq!(list, ["/p/a.kerf", "/p/b.kerf"]);
        assert!(!prune_recent(&mut list, |_| true), "nothing to prune is no change");

        assert!(merge(&Settings::default(), &serde_json::json!({"recent_projects": []})).is_err());
    }

    #[test]
    fn the_recent_list_persists_beside_the_other_settings() {
        let dir = scratch();
        let file = dir.join("settings.json");
        save_to(
            &file,
            &Settings {
                cpu_percent: 44,
                theme: Some(serde_json::json!({"name": "Mine"})),
                ..Settings::default()
            },
        )
        .unwrap();
        let after = edit_recent_file(&file, |l| {
            push_recent(l, "/p/a.kerf");
            push_recent(l, "/p/b.kerf");
            true
        })
        .unwrap();
        assert_eq!(after, ["/p/b.kerf", "/p/a.kerf"]);
        let back = load_from(&file);
        assert_eq!(back.recent_projects, after);
        assert_eq!(back.cpu_percent, 44, "the rest of the file is what it was");
        assert!(back.theme.is_some());
        let cleared = edit_recent_file(&file, |l| {
            l.clear();
            true
        })
        .unwrap();
        assert!(cleared.is_empty());
        assert!(load_from(&file).recent_projects.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reopening_the_last_project_needs_the_setting_and_a_file_that_is_still_there() {
        let mut s = Settings {
            recent_projects: vec!["/p/gone.kerf".to_string(), "/p/b.kerf".to_string()],
            ..Settings::default()
        };
        let exists = |p: &Path| p.ends_with("b.kerf");
        assert_eq!(reopen_target(&s, exists), None, "off by default");
        s.reopen_last_project = true;
        assert_eq!(
            reopen_target(&s, exists).as_deref(),
            Some("/p/b.kerf"),
            "the newest one that exists"
        );
        assert_eq!(reopen_target(&s, |_| false), None);
        s.recent_projects.clear();
        assert_eq!(reopen_target(&s, exists), None);
    }

    #[test]
    fn the_new_preferences_are_patchable_and_reach_the_view() {
        let base = Settings::default();
        let merged = merge(
            &base,
            &serde_json::json!({"time_display": "seconds", "reopen_last_project": true}),
        )
        .unwrap();
        assert_eq!(merged.time_display, TimeDisplay::Seconds);
        assert!(merged.reopen_last_project);
        let view = serde_json::to_value(SettingsView::current(&merged)).unwrap();
        assert_eq!(view["time_display"], "seconds");
        assert_eq!(view["reopen_last_project"], true);
        assert_eq!(view["recent_projects"], serde_json::json!([]));
        assert!(merge(&base, &serde_json::json!({"time_display": "ticks"})).is_err());
    }

    #[test]
    fn a_weak_stored_token_or_a_bad_stored_address_is_not_used() {
        let loaded = parse(r#"{"mcp_token": "short", "mcp_addr": "not an address", "cpu_percent": 40}"#).unwrap();
        assert_eq!(loaded.mcp_token, None, "a new token is made at the next start");
        assert_eq!(loaded.mcp_addr, None, "the default address is used");
        assert_eq!(loaded.cpu_percent, 40, "nothing else is lost");
        let good = parse(&format!(
            r#"{{"mcp_token": "{}", "mcp_addr": "127.0.0.1:9000"}}"#,
            "a".repeat(40)
        ))
        .unwrap();
        assert_eq!(good.mcp_token.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!(good.mcp_addr.as_deref(), Some("127.0.0.1:9000"));
    }

    #[test]
    fn a_reset_keeps_the_agents_door_and_turns_review_back_on() {
        let current = Settings {
            agent_review: false,
            mcp_enabled: false,
            mcp_addr: Some("127.0.0.1:9000".to_string()),
            mcp_require_token: false,
            mcp_token: Some("t".repeat(40)),
            cpu_percent: 30,
            ..Settings::default()
        };
        let fresh = reset_keep(&current);
        assert!(!fresh.mcp_enabled && !fresh.mcp_require_token);
        assert_eq!(fresh.mcp_addr.as_deref(), Some("127.0.0.1:9000"));
        assert_eq!(fresh.mcp_token, current.mcp_token, "clients hold this token");
        assert!(fresh.agent_review, "review is a preference, and goes back on");
        assert_eq!(fresh.cpu_percent, Settings::default().cpu_percent);
    }
}
