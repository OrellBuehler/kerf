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
            layout: None,
            theme: None,
            workspaces: None,
            keybindings: None,
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
    pub auto_analysis: kerf_core::AutoAnalysis,
    pub proxy_size: kerf_core::ProxySize,
    pub preview_source: kerf_core::PreviewSource,
    pub safe_areas: bool,
    pub gpu_preview: bool,
    pub cpu_cores: usize,
    pub cpu_threads: usize,
    pub cpu_min_percent: u8,
    pub layout: Option<serde_json::Value>,
    pub theme: Option<serde_json::Value>,
    pub workspaces: Option<serde_json::Value>,
    pub keybindings: Option<serde_json::Value>,
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
            cpu_cores: kerf_core::cpu_cores(),
            cpu_threads: kerf_core::cpu_threads(),
            cpu_min_percent: kerf_core::MIN_CPU_PERCENT,
            layout: stored.layout.clone(),
            theme: stored.theme.clone(),
            workspaces: stored.workspaces.clone(),
            keybindings: stored.keybindings.clone(),
        }
    }
}

/// Serializes every read-modify-write of the settings file. The webview writes
/// layout, theme and the toggles from independent call sites, so two patches can
/// be in flight at once; without this the second would load the file before the
/// first had saved and drop its change.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// The keys a patch may carry — every field of [`Settings`].
const KEYS: [&str; 10] = [
    "cpu_percent",
    "auto_analysis",
    "proxy_size",
    "preview_source",
    "safe_areas",
    "gpu_preview",
    "layout",
    "theme",
    "workspaces",
    "keybindings",
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
    match parse(&raw) {
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

/// Read a settings file, carrying an older one forward ([`migrate`]).
fn parse(raw: &str) -> Result<Settings, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let has_auto_analysis = value.get("auto_analysis").is_some();
    let mut settings: Settings = serde_json::from_value(value)?;
    migrate(&mut settings, has_auto_analysis);
    Ok(settings)
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
    kerf_core::set_auto_analysis(settings.auto_analysis);
    kerf_core::proxy::set_proxy_size(settings.proxy_size);
    kerf_core::proxy::set_preview_source_setting(settings.preview_source);
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
        assert_eq!(s.auto_analysis, kerf_core::AutoAnalysis::default());
        assert_eq!(s.proxy_size, kerf_core::ProxySize::W1280);
        assert_eq!(s.preview_source, kerf_core::PreviewSource::Auto);
        assert!(!s.gpu_preview, "the GPU preview is opt-in");
        assert!(s.layout.is_none());
        assert!(s.theme.is_none());
        assert!(s.workspaces.is_none());
        assert!(s.keybindings.is_none());
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
}
