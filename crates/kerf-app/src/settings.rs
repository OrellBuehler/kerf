//! App preferences — the settings that belong to this machine rather than to a
//! project.
//!
//! A `.kerf` file describes a cut; how much of *your* computer Kerf may take
//! while it renders one is not part of that, and must not travel with the
//! project to another machine. So these live in the platform config directory as
//! plain JSON, are read once at launch and written on every change.
//!
//! Everything here is best-effort: an unreadable or malformed file falls back to
//! the defaults rather than refusing to start, because a preference is never
//! worth failing a launch over.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Set deliberately in the environment, this wins over the stored preference at
/// launch (see [`apply`]).
const CPU_ENV: &str = "KERF_CPU_PERCENT";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Share of the machine one heavy job (analysis, transcription, proxy,
    /// stitch, export) may take, in percent. See `kerf_core::engine::cpu`.
    pub cpu_percent: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cpu_percent: kerf_core::DEFAULT_CPU_PERCENT,
        }
    }
}

/// What the settings surface actually shows: the stored preference resolved
/// against the engine, plus the machine it is a share *of*. The UI cannot work
/// out "9 of 12 cores" on its own — a webview's `hardwareConcurrency` is not
/// what ffmpeg sees.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub cpu_percent: u8,
    pub cpu_cores: usize,
    pub cpu_threads: usize,
    pub cpu_min_percent: u8,
}

impl SettingsView {
    /// Read straight from the engine rather than from the stored file, so what
    /// the dialog shows is what is actually in force — including an environment
    /// override the user set outside the app.
    pub fn current() -> Self {
        Self {
            cpu_percent: kerf_core::cpu_percent(),
            cpu_cores: kerf_core::cpu_cores(),
            cpu_threads: kerf_core::cpu_threads(),
            cpu_min_percent: kerf_core::MIN_CPU_PERCENT,
        }
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("settings.json"))
}

/// Read the stored preferences, falling back to the defaults for anything
/// missing, unreadable or malformed.
pub fn load(app: &AppHandle) -> Settings {
    let Some(file) = path(app) else {
        return Settings::default();
    };
    match std::fs::read_to_string(&file) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!(error = %e, path = %file.display(), "unreadable settings; using defaults");
            Settings::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            tracing::warn!(error = %e, path = %file.display(), "could not read settings; using defaults");
            Settings::default()
        }
    }
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let file = path(app).ok_or("no config directory available for settings")?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create the settings directory: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&file, raw).map_err(|e| format!("could not write settings: {e}"))
}

/// Push the preferences into the engine.
///
/// `KERF_CPU_PERCENT` deliberately wins at launch: someone who set it in the
/// environment meant it for this run. Moving the slider afterwards still takes
/// effect — a runtime choice is the newer instruction of the two.
pub fn apply(settings: &Settings) {
    if std::env::var_os(CPU_ENV).is_some() {
        tracing::info!(percent = kerf_core::cpu_percent(), "CPU budget set from {CPU_ENV}");
        return;
    }
    let applied = kerf_core::set_cpu_percent(settings.cpu_percent);
    tracing::info!(percent = applied, cores = kerf_core::cpu_cores(), "CPU budget applied");
}
