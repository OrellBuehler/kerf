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
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Whether the analysis pass transcribes speech. Off, importing media still
    /// detects silence / scenes / loudness / rhythm but never fetches a speech
    /// model or runs inference.
    pub transcribe: bool,
    /// Whether the preview shades the delivery safe areas — where a phone's own
    /// UI covers a vertical cut. Off by default: it is a check you turn on,
    /// not a view you cut behind.
    pub safe_areas: bool,
    /// The workspace arrangement (dockview's serialized layout). Opaque here:
    /// the frontend validates it and falls back to its default layout.
    pub layout: Option<serde_json::Value>,
    /// The color theme. Opaque for the same reason — the frontend owns the
    /// token list, presets and the JSON file format.
    pub theme: Option<serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cpu_percent: kerf_core::DEFAULT_CPU_PERCENT,
            transcribe: true,
            safe_areas: false,
            layout: None,
            theme: None,
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

/// What the settings surface actually shows: the stored preference resolved
/// against the engine, plus the machine it is a share *of*. The UI cannot work
/// out "9 of 12 cores" on its own — a webview's `hardwareConcurrency` is not
/// what ffmpeg sees.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub cpu_percent: u8,
    pub transcribe: bool,
    pub safe_areas: bool,
    pub cpu_cores: usize,
    pub cpu_threads: usize,
    pub cpu_min_percent: u8,
    pub layout: Option<serde_json::Value>,
    pub theme: Option<serde_json::Value>,
}

impl SettingsView {
    /// The engine-held preferences read straight from the engine rather than
    /// from the stored file, so what the dialog shows is what is actually in
    /// force — including an environment override the user set outside the
    /// app. The layout and theme only exist in the file, so those come from
    /// `stored`.
    pub fn current(stored: &Settings) -> Self {
        Self {
            cpu_percent: kerf_core::cpu_percent(),
            transcribe: kerf_core::transcription_enabled(),
            safe_areas: safe_areas(),
            cpu_cores: kerf_core::cpu_cores(),
            cpu_threads: kerf_core::cpu_threads(),
            cpu_min_percent: kerf_core::MIN_CPU_PERCENT,
            layout: stored.layout.clone(),
            theme: stored.theme.clone(),
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
    kerf_core::set_transcription_enabled(settings.transcribe);
    set_safe_areas(settings.safe_areas);
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
        assert!(s.transcribe);
        assert!(s.layout.is_none());
        assert!(s.theme.is_none());
    }

    #[test]
    fn layout_and_theme_round_trip_untouched() {
        let layout = serde_json::json!({"grid": {"root": {"type": "leaf"}}, "panels": {}});
        let theme = serde_json::json!({"name": "Mine", "version": 1, "colors": {"kerf-500": "#ffffff"}});
        let s = Settings { layout: Some(layout.clone()), theme: Some(theme.clone()), ..Settings::default() };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.layout, Some(layout));
        assert_eq!(back.theme, Some(theme));
    }
}
