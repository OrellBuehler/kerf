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
//! worth failing a launch over — but a malformed file is kept (moved aside), not
//! overwritten. Writes are atomic and serialized, and arrive as patches.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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
    /// The workspaces: which one is active, the saved arrangement of each, and
    /// the state of the library rail. Opaque like `layout` and `theme` — the
    /// frontend owns the shape, validates it on the way back in and falls back
    /// to its presets. `layout` is kept as the pre-workspaces arrangement the
    /// frontend migrates into the Edit workspace when this is absent.
    pub workspaces: Option<serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cpu_percent: kerf_core::DEFAULT_CPU_PERCENT,
            transcribe: true,
            safe_areas: false,
            layout: None,
            theme: None,
            workspaces: None,
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
    pub workspaces: Option<serde_json::Value>,
}

impl SettingsView {
    /// The engine-held preferences read straight from the engine rather than
    /// from the stored file, so what the dialog shows is what is actually in
    /// force — including an environment override the user set outside the
    /// app. The layout, theme and workspaces only exist in the file, so those
    /// come from `stored`.
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
            workspaces: stored.workspaces.clone(),
        }
    }
}

/// Serializes every read-modify-write of the settings file. The webview writes
/// layout, theme and the toggles from independent call sites, so two patches can
/// be in flight at once; without this the second would load the file before the
/// first had saved and drop its change.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// The keys a patch may carry — every field of [`Settings`].
const KEYS: [&str; 6] = ["cpu_percent", "transcribe", "safe_areas", "layout", "theme", "workspaces"];

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
/// missing or unreadable. A file that does not parse is moved aside to
/// `settings.corrupt-<unix-ms>.json` first: the next save would otherwise
/// overwrite someone's imported theme and layout with defaults.
fn load_from(file: &Path) -> Settings {
    let raw = match std::fs::read_to_string(file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            tracing::warn!(error = %e, path = %file.display(), "could not read settings; using defaults");
            return Settings::default();
        }
    };
    match serde_json::from_str(&raw) {
        Ok(settings) => settings,
        Err(e) => {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let aside = sibling(file, &format!("settings.corrupt-{ms}.json"));
            match std::fs::rename(file, &aside) {
                Ok(()) => tracing::warn!(
                    error = %e,
                    path = %file.display(),
                    kept_as = %aside.display(),
                    "malformed settings moved aside; using defaults"
                ),
                Err(re) => tracing::warn!(
                    error = %e,
                    rename_error = %re,
                    path = %file.display(),
                    "malformed settings could not be moved aside; using defaults"
                ),
            }
            Settings::default()
        }
    }
}

fn save_to(file: &Path, settings: &Settings) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    write_atomic(file, raw.as_bytes()).map_err(|e| format!("could not write settings: {e}"))
}

/// Merge a patch — a JSON object holding only the fields that changed — into
/// `settings`. A field present with `null` clears it (the layout, theme and
/// workspaces are nullable); a field left out is untouched.
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
    serde_json::from_value(serde_json::Value::Object(merged)).map_err(|e| format!("invalid settings: {e}"))
}

/// The stored preferences (see [`load_from`]).
pub fn load(app: &AppHandle) -> Settings {
    let Some(file) = path(app) else {
        return Settings::default();
    };
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_from(&file)
}

/// Merge `patch` into the stored preferences under the file lock, put what
/// changed into force and persist the result. Only the fields the patch carries
/// reach the engine: re-applying the stored CPU budget because the layout moved
/// would undo a `KERF_CPU_PERCENT` override. The CPU share is clamped through
/// the engine first and the clamped value stored — an out-of-range number would
/// otherwise be re-clamped on every launch.
pub fn update(app: &AppHandle, patch: &serde_json::Value) -> Result<Settings, String> {
    let file = path(app).ok_or("no config directory available for settings")?;
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut merged = merge(&load_from(&file), patch)?;
    if patch.get("transcribe").is_some() {
        kerf_core::set_transcription_enabled(merged.transcribe);
    }
    if patch.get("safe_areas").is_some() {
        set_safe_areas(merged.safe_areas);
    }
    if patch.get("cpu_percent").is_some() {
        merged.cpu_percent = kerf_core::set_cpu_percent(merged.cpu_percent);
    }
    save_to(&file, &merged)?;
    Ok(merged)
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
        assert!(s.workspaces.is_none());
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
        let merged = merge(&base, &serde_json::json!({"transcribe": false})).unwrap();
        assert!(!merged.transcribe);
        assert_eq!(merged.cpu_percent, 40);
        assert_eq!(merged.layout, base.layout);
        assert_eq!(merged.theme, base.theme);
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

    #[test]
    fn save_then_load_round_trips() {
        let dir = scratch();
        let file = dir.join("nested").join("settings.json");
        let s = Settings {
            cpu_percent: 33,
            safe_areas: true,
            ..Settings::default()
        };
        save_to(&file, &s).unwrap();
        let back = load_from(&file);
        assert_eq!(back.cpu_percent, 33);
        assert!(back.safe_areas);
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
}
