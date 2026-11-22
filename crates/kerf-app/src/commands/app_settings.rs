//! The agent connection, app settings and theme files.

use crate::*;

// ---- agent connection (MCP endpoint) ---------------------------------------

/// Where the endpoint is, and how long ago an agent last used it.
///
/// A streamable-HTTP client holds no connection between calls, so there is no
/// "is it plugged in" to report — `last_seen_secs` is `None` until something
/// has spoken to the endpoint at all, and the panel decides from its age
/// whether to call that connected. Anything else would be the green dot the
/// panel used to show whether or not an agent existed.
#[derive(Serialize)]
pub(crate) struct AgentStatus {
    endpoint: String,
    last_seen_secs: Option<i64>,
    /// Set when the server could not start (the port is taken), so the panel
    /// can say that instead of showing an endpoint nothing answers on.
    error: Option<String>,
}

#[tauri::command(async)]
pub(crate) fn agent_status() -> AgentStatus {
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
pub(crate) fn report_ui_context(state: State<'_, editor_context::EditorContextState>, context: editor_context::UiContext) {
    state.report(context);
}

// ---- app settings ----------------------------------------------------------

/// The current preferences, resolved against the engine (see
/// [`settings::SettingsView`]).
#[tauri::command(async)]
pub(crate) fn get_settings(app: AppHandle) -> settings::SettingsView {
    settings::SettingsView::current(&settings::load(&app))
}

/// Merge a patch — only the fields that changed (`{workspaces}`, `{theme}`,
/// `{cpu_percent}`, …) — into the stored preferences and put them into force.
/// Patching rather than replacing means two call sites writing at once cannot
/// overwrite each other's field with a stale copy. Returns the resolved view, so
/// the dialog can show the clamped percentage and the cores it works out to
/// without a second round-trip.
#[tauri::command(async)]
pub(crate) fn set_settings(
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
        // The user's word has dropped any block in the file; what the check found there goes with it.
        gpu.forget_findings();
        if gpu.set_enabled(stored.gpu_preview_in_force()) {
            let gpu = gpu.inner().clone();
            std::thread::spawn(move || gpu.teardown_if_off());
        }
    }
    Ok(settings::SettingsView::current(&stored))
}

/// Put every preference back to its default (the old file is kept beside it as
/// `settings.reset-<time>.json`) and answer with the resolved view.
#[tauri::command(async)]
pub(crate) fn reset_settings(app: AppHandle, gpu: State<'_, Arc<gpu_preview::GpuPreview>>) -> CmdResult<settings::SettingsView> {
    let stored = settings::reset(&app)?;
    // The proxy size and the preview source are back to theirs: queue what is missing and have
    // the preview decode from whichever file it should now.
    respawn_proxies(&app);
    let _ = app.emit("proxy-ready", ());
    if gpu.set_enabled(stored.gpu_preview_in_force()) {
        let gpu = gpu.inner().clone();
        std::thread::spawn(move || gpu.teardown_if_off());
    }
    Ok(settings::SettingsView::current(&stored))
}

/// Show the settings file in the OS file manager (it is written first if there is none yet).
#[tauri::command(async)]
pub(crate) fn reveal_settings_file(app: AppHandle) -> CmdResult<()> {
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
pub(crate) fn require_json_path(path: &str) -> CmdResult<()> {
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
pub(crate) async fn read_text_file(path: String) -> CmdResult<String> {
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
pub(crate) async fn write_text_file(path: String, contents: String) -> CmdResult<String> {
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
