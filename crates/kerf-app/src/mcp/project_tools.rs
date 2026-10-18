//! The tools about the project as a file, and about the settings an agent may know and change.
//!
//! A project the user has open is theirs: an agent that opens or starts another replaces what is on
//! their screen. So these refuse to drop work that exists only in memory (the sample, a project that was
//! never saved) unless the call says `discard_unsaved`, and they tell the window the project was
//! replaced (`project-replaced`) so it reloads. The settings tools are an allow-list: what an agent
//! needs to know to work (and the few numbers that change how fast it can), never the user's theme,
//! layout, shortcuts, the agent review switch or the server's address and token.

use std::path::Path;
use std::sync::{Arc, Mutex};

use kerf_core::Project;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{schemars, tool, tool_router, ErrorData as McpError};
use tauri::Manager;

use super::{blocking, core_err, json, lock_agent_as, reads, KerfMcp};
use crate::settings;

/// Whether `project` holds work that exists only in memory: it has no file, and something in it.
/// The same test as the window's "unsaved work" prompt.
fn unsaved_work(project: &Project) -> Result<bool, McpError> {
    if project.path().is_some() {
        return Ok(false);
    }
    let timeline = project.timeline().map_err(core_err)?;
    Ok(!project.list_assets().map_err(core_err)?.is_empty()
        || !timeline.overlays.is_empty()
        || !timeline.svg_overlays.is_empty()
        || timeline.tracks.iter().any(|t| !t.clips.is_empty()))
}

/// Refuse to replace the open project when that would lose unsaved work, unless told to.
fn require_discard_ok(project: &Project, discard_unsaved: bool) -> Result<(), McpError> {
    if !discard_unsaved && unsaved_work(project)? {
        return Err(McpError::invalid_params(
            "the open project has work that was never saved (it lives only in memory) and would be lost. Save it first with \
             save_project_as, or pass discard_unsaved=true if the user said to throw it away",
            None,
        ));
    }
    Ok(())
}

/// A `.kerf` path an agent may name: absolute, a local file path, ending in `.kerf`.
fn project_path_arg(path: &str) -> Result<std::path::PathBuf, McpError> {
    crate::require_local_output_path(path).map_err(|e| McpError::invalid_params(e, None))?;
    let path = std::path::PathBuf::from(path);
    let is_kerf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("kerf"));
    if !is_kerf {
        return Err(McpError::invalid_params(
            format!("{} is not a project file: Kerf projects end in .kerf", path.display()),
            None,
        ));
    }
    Ok(path)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SaveAsParams {
    #[schemars(description = "Absolute path of the .kerf file to write")]
    path: String,
    #[schemars(description = "Replace the file if it already exists (default false: the call fails naming it)")]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct OpenParams {
    #[schemars(description = "Absolute path of the .kerf file to open")]
    path: String,
    #[schemars(
        description = "Open it even though the project that is open has work that was never saved (it would be lost). Default false"
    )]
    discard_unsaved: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct NewParams {
    #[schemars(
        description = "Start the new project even though the project that is open has work that was never saved (it would be lost). Default false"
    )]
    discard_unsaved: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AutoAnalysisParams {
    #[schemars(description = "Analyze new media on import at all")]
    enabled: Option<bool>,
    silence: Option<bool>,
    scenes: Option<bool>,
    loudness: Option<bool>,
    rhythm: Option<bool>,
    #[schemars(description = "Transcribe on import (downloads a speech model on first use)")]
    transcript: Option<bool>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum PreviewSourceParam {
    /// The proxy once it is ready, the original until then.
    Auto,
    /// Always the original, full resolution; proxies are not built.
    Original,
    /// Only the proxy: a frame whose proxy is not ready is not decoded.
    ProxyOnly,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum TimeDisplayParam {
    Timecode,
    Seconds,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetSettingsParams {
    #[schemars(description = "Share of the machine one heavy job may take, in percent (10 to 100)")]
    cpu_percent: Option<u8>,
    #[schemars(description = "Which analyses run by themselves on import; fields you leave out keep their value")]
    auto_analysis: Option<AutoAnalysisParams>,
    #[schemars(description = "Preview proxy width in pixels: 0 (none), 720, 1080 or 1280")]
    proxy_size: Option<u32>,
    #[schemars(description = "Which file the preview decodes: auto, original or proxy_only. Export always reads the originals")]
    preview_source: Option<PreviewSourceParam>,
    #[schemars(description = "How the app shows times: timecode or seconds")]
    time_display: Option<TimeDisplayParam>,
}

/// The settings keys an agent may read, and (a subset) write. Everything else — the theme, the layout,
/// the workspaces, the shortcuts, `agent_review` and the server's address, switch and token — is the
/// user's.
pub(super) const READABLE: [&str; 6] = [
    "auto_analysis",
    "cpu_percent",
    "proxy_size",
    "preview_source",
    "agent_review",
    "time_display",
];
pub(super) const WRITABLE: [&str; 5] = ["auto_analysis", "cpu_percent", "proxy_size", "preview_source", "time_display"];

/// The settings an agent can see, from the stored file and what the engine has in force.
pub(super) fn readable_settings(stored: &settings::Settings) -> serde_json::Value {
    let mut all = serde_json::to_value(stored).unwrap_or_default();
    if let (Some(all), Ok(serde_json::Value::Object(view))) = (
        all.as_object_mut(),
        serde_json::to_value(settings::SettingsView::current(stored)),
    ) {
        for (k, v) in view {
            all.insert(k, v);
        }
    }
    let picked: serde_json::Map<String, serde_json::Value> = READABLE
        .iter()
        .filter_map(|k| all.get(*k).map(|v| ((*k).to_string(), v.clone())))
        .collect();
    serde_json::Value::Object(picked)
}

/// The settings patch a `set_settings` call asks for, or why it cannot be one. Only [`WRITABLE`]
/// keys can come out of this.
pub(super) fn settings_patch(
    p: &SetSettingsParams,
    current: kerf_core::AutoAnalysis,
) -> Result<serde_json::Map<String, serde_json::Value>, McpError> {
    let bad = |msg: String| McpError::invalid_params(msg, None);
    let mut patch = serde_json::Map::new();
    if let Some(percent) = p.cpu_percent {
        if !(kerf_core::MIN_CPU_PERCENT..=100).contains(&percent) {
            return Err(bad(format!(
                "`cpu_percent` must be {} to 100, got {percent}",
                kerf_core::MIN_CPU_PERCENT
            )));
        }
        patch.insert("cpu_percent".into(), percent.into());
    }
    if let Some(a) = &p.auto_analysis {
        let merged = kerf_core::AutoAnalysis {
            enabled: a.enabled.unwrap_or(current.enabled),
            silence: a.silence.unwrap_or(current.silence),
            scenes: a.scenes.unwrap_or(current.scenes),
            loudness: a.loudness.unwrap_or(current.loudness),
            rhythm: a.rhythm.unwrap_or(current.rhythm),
            transcript: a.transcript.unwrap_or(current.transcript),
        };
        patch.insert(
            "auto_analysis".into(),
            serde_json::to_value(merged).map_err(|e| McpError::internal_error(e.to_string(), None))?,
        );
    }
    if let Some(size) = p.proxy_size {
        if ![0, 720, 1080, 1280].contains(&size) {
            return Err(bad(format!("`proxy_size` must be 0, 720, 1080 or 1280, got {size}")));
        }
        patch.insert("proxy_size".into(), size.into());
    }
    if let Some(source) = p.preview_source {
        let name = match source {
            PreviewSourceParam::Auto => "auto",
            PreviewSourceParam::Original => "original",
            PreviewSourceParam::ProxyOnly => "proxy_only",
        };
        patch.insert("preview_source".into(), name.into());
    }
    if let Some(mode) = p.time_display {
        let name = match mode {
            TimeDisplayParam::Timecode => "timecode",
            TimeDisplayParam::Seconds => "seconds",
        };
        patch.insert("time_display".into(), name.into());
    }
    debug_assert!(patch.keys().all(|k| WRITABLE.contains(&k.as_str())));
    Ok(patch)
}

#[tool_router(router = tool_router_project, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "About the open project: its `path` (null while it lives only in memory), `in_memory` and \
                       `unsaved_work` (it holds something that would be lost on close — save it with save_project_as \
                       first), counts of assets and clips, `agent_review` (whether your edits wait for the user to \
                       accept them), the staged proposal if one is open, and `generation` (changes when another \
                       project is opened or started)."
    )]
    fn project_info(&self) -> Result<String, McpError> {
        let project = self.lock();
        let timeline = project.working_timeline().map_err(core_err)?;
        let assets = project.list_assets().map_err(core_err)?;
        let staged = project.staged().map_err(core_err)?;
        json(&serde_json::json!({
            "path": project.path().map(|p| p.display().to_string()),
            "in_memory": project.path().is_none(),
            "unsaved_work": unsaved_work(&project)?,
            "generation": project.generation(),
            "change_count": project.change_count(),
            "assets": assets.len(),
            "clips": timeline.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
            "duration_secs": timeline.duration(),
            "agent_review": project.agent_review(),
            "staged": staged.as_ref().map(reads::staged_brief),
        }))
    }

    #[tool(
        description = "Make sure the open project is on disk. A project backed by a .kerf file writes every edit as it \
                       happens, so there is nothing to do and this just says where it is; one that lives only in memory \
                       has no file yet — use save_project_as."
    )]
    fn save_project(&self) -> Result<String, McpError> {
        let project = self.lock();
        match project.path() {
            Some(path) => json(&serde_json::json!({
                "path": path.display().to_string(),
                "saved": true,
                "note": "every edit is already written to this file",
            })),
            None => Err(McpError::invalid_params(
                "the project lives only in memory and has no file; use save_project_as with an absolute .kerf path",
                None,
            )),
        }
    }

    #[tool(
        description = "Save the project to a new .kerf file and carry on working in that file. `path` is absolute and \
                       ends in .kerf; an existing file is refused unless `overwrite`. Onto the file that is already open \
                       this does nothing. The window follows to the new file."
    )]
    async fn save_project_as(&self, Parameters(p): Parameters<SaveAsParams>) -> Result<String, McpError> {
        let path = project_path_arg(&p.path)?;
        let overwrite = p.overwrite.unwrap_or(false);
        let (shared, owner, app) = (self.project.clone(), self.owner(), self.app.clone());
        let saved = blocking(move || {
            let open = lock_agent_as(&shared, owner.as_deref()).path().map(Path::to_path_buf);
            // Onto the file that is open there is nothing to do; onto another one that exists, only
            // if told. Both look at the disk, so neither is asked under the lock.
            if open.is_some_and(|open| kerf_core::paths::same_file(&open, &path)) {
                return Ok(None);
            }
            super::refuse_overwrite(&path, overwrite)?;
            let before = {
                // `save_as` refuses a file the project still uses (a source, a proxy) itself.
                let project = lock_agent_as(&shared, owner.as_deref());
                project.save_as(&path).map_err(core_err)?;
                project.change_count()
            };
            let opened = Project::open(&path).map_err(core_err)?;
            let mut project = lock_agent_as(&shared, owner.as_deref());
            if project.change_count() == before {
                project.replace_with(opened);
            } else {
                // Something wrote while the copy was being reopened (analysis merged, an edit):
                // the copy is out of date, so make it again under one held lock.
                drop(opened);
                project.save_as(&path).map_err(core_err)?;
                project.replace_with(Project::open(&path).map_err(core_err)?);
            }
            let saved = project.path().map(|p| p.display().to_string());
            drop(project);
            app.state::<crate::AppState>().scrub.clear();
            Ok(Some(saved))
        })
        .await?;
        match saved {
            Some(path) => {
                self.announce_replaced();
                json(&serde_json::json!({ "path": path, "saved": true }))
            }
            None => json(&serde_json::json!({ "path": p.path, "saved": true, "note": "that is the file already open" })),
        }
    }

    #[tool(
        description = "Open an existing .kerf project, replacing the one that is open (the window switches to it). Refused \
                       while the open project has work that was never saved, unless `discard_unsaved` — a saved project \
                       loses nothing. Your staged proposal belongs to the project it was made on and does not follow."
    )]
    async fn open_project(&self, Parameters(p): Parameters<OpenParams>) -> Result<String, McpError> {
        let path = project_path_arg(&p.path)?;
        kerf_core::paths::validate_import_path(&path).map_err(core_err)?;
        let discard = p.discard_unsaved.unwrap_or(false);
        require_discard_ok(&self.lock(), discard)?;
        let (shared, owner, app) = (self.project.clone(), self.owner(), self.app.clone());
        let opened_path = blocking(move || {
            // Open the file first, then swap it in — the (disk-bound) open doesn't hold the lock, and a
            // failed open leaves the current project intact.
            let opened = Project::open(&path).map_err(core_err)?;
            let mut project = lock_agent_as(&shared, owner.as_deref());
            // Asked again under the lock that swaps: the user may have added to it meanwhile.
            require_discard_ok(&project, discard)?;
            project.replace_with(opened);
            let now = project.path().map(|p| p.display().to_string());
            drop(project);
            Self::after_replace(&app, &shared);
            Ok(now)
        })
        .await?;
        self.announce_replaced();
        json(&serde_json::json!({ "path": opened_path, "opened": true }))
    }

    #[tool(
        description = "Start a new, empty project in memory, replacing the one that is open (the window switches to it). \
                       Refused while the open project has work that was never saved, unless `discard_unsaved`. The new \
                       project has no file until save_project_as."
    )]
    async fn new_project(&self, Parameters(p): Parameters<NewParams>) -> Result<String, McpError> {
        let discard = p.discard_unsaved.unwrap_or(false);
        require_discard_ok(&self.lock(), discard)?;
        let (shared, owner, app) = (self.project.clone(), self.owner(), self.app.clone());
        blocking(move || {
            let fresh = Project::open_in_memory().map_err(core_err)?;
            let mut project = lock_agent_as(&shared, owner.as_deref());
            require_discard_ok(&project, discard)?;
            project.replace_with(fresh);
            drop(project);
            Self::after_replace(&app, &shared);
            Ok(())
        })
        .await?;
        self.announce_replaced();
        json(&serde_json::json!({ "path": null, "new": true }))
    }

    #[tool(
        description = "The settings that matter to your work: `auto_analysis` (which analyses run on import), \
                       `cpu_percent` (the share of the machine one heavy job may take), `proxy_size`, `preview_source`, \
                       `time_display` and `agent_review` (true: your edits wait in a proposal for the user to accept; \
                       you cannot change this). Nothing else of the user's settings is visible or changeable from here."
    )]
    fn get_settings(&self) -> Result<String, McpError> {
        json(&readable_settings(&settings::load(&self.app)))
    }

    #[tool(
        description = "Change a setting that bears on your work: `cpu_percent` (10 to 100), `auto_analysis` (which \
                       analyses run on import; fields you omit keep their value), `proxy_size` (0, 720, 1080 or 1280), \
                       `preview_source` (auto, original, proxy_only) or `time_display` (timecode, seconds). These are the \
                       USER's preferences and change what they see — touch them only when the task needs it. The theme, \
                       layout, shortcuts, the agent review switch and the MCP server's address and token are not available \
                       here. Returns the settings after."
    )]
    async fn set_settings(&self, Parameters(p): Parameters<SetSettingsParams>) -> Result<String, McpError> {
        let patch = settings_patch(&p, kerf_core::auto_analysis())?;
        if patch.is_empty() {
            return Err(McpError::invalid_params(
                "name at least one setting to change: cpu_percent, auto_analysis, proxy_size, preview_source or time_display",
                None,
            ));
        }
        let (app, patch) = (self.app.clone(), serde_json::Value::Object(patch));
        let stored = blocking(move || {
            let stored = settings::update(&app, &patch).map_err(|e| McpError::invalid_params(e, None))?;
            // A new proxy size or source is different files: queue what is missing, and have the
            // preview decode from whichever it should now (what the GUI's own change does).
            if patch.get("proxy_size").is_some() || patch.get("preview_source").is_some() {
                crate::respawn_proxies(&app);
                let _ = tauri::Emitter::emit(&app, "proxy-ready", ());
            }
            Ok(stored)
        })
        .await?;
        self.notify("settings-changed");
        json(&readable_settings(&stored))
    }
}

impl KerfMcp {
    /// What a replaced project needs besides the swap, as the window's own open does it.
    fn after_replace(app: &tauri::AppHandle, shared: &Arc<Mutex<Project>>) {
        // The frames of the project that was open are of no use to this one.
        app.state::<crate::AppState>().scrub.clear();
        // The speech model is remembered per project, so transcribing in the new one uses the model
        // it was cut with (a new one resets to the default).
        crate::restore_speech_model(shared);
        // Every video asset of the opened project gets its preview proxy (a cached one is skipped).
        crate::respawn_proxies(app);
    }

    /// Tell the window another project is open: it reloads everything, then refreshes as for any change.
    fn announce_replaced(&self) {
        let path = self.lock().path().map(Path::to_path_buf);
        if let Err(e) = tauri::Emitter::emit(&self.app, "project-replaced", path.map(|p| p.display().to_string())) {
            tracing::warn!(error = %e, "failed to emit project-replaced");
        }
        self.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> kerf_core::AutoAnalysis {
        kerf_core::AutoAnalysis::default()
    }

    #[test]
    fn a_project_path_is_an_absolute_local_kerf_file() {
        assert!(project_path_arg("/tmp/cut.kerf").is_ok());
        assert!(project_path_arg("/tmp/CUT.KERF").is_ok());
        for bad in ["cut.kerf", "/tmp/cut.mp4", "/tmp/cut", "https://x.example/cut.kerf", "pipe:1"] {
            let err = project_path_arg(bad).expect_err(bad);
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{bad}");
        }
    }

    #[test]
    fn a_fresh_in_memory_project_has_nothing_to_lose() {
        let project = Project::open_in_memory().unwrap();
        assert!(!unsaved_work(&project).unwrap());
        assert!(require_discard_ok(&project, false).is_ok());
    }

    #[test]
    fn work_that_exists_only_in_memory_is_not_dropped_without_saying_so() {
        let project = Project::open_in_memory().unwrap();
        project.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
        assert!(unsaved_work(&project).unwrap());
        let err = require_discard_ok(&project, false).expect_err("refused");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            err.message.contains("save_project_as") && err.message.contains("discard_unsaved"),
            "{}",
            err.message
        );
        assert!(require_discard_ok(&project, true).is_ok(), "with the flag it goes ahead");
    }

    #[test]
    fn a_saved_project_loses_nothing_when_replaced() {
        let dir = std::env::temp_dir().join(format!("kerf-mcp-proj-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.kerf");
        let project = Project::open(&file).unwrap();
        project.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
        assert!(!unsaved_work(&project).unwrap(), "every edit is on disk");
        assert!(require_discard_ok(&project, false).is_ok());
        drop(project);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_agent_may_read_and_write_only_its_allow_list() {
        for key in WRITABLE {
            assert!(READABLE.contains(&key), "{key} is writable but not readable");
        }
        for never in [
            "theme",
            "layout",
            "workspaces",
            "keybindings",
            "mcp_token",
            "mcp_addr",
            "mcp_enabled",
            "mcp_require_token",
            "gpu_preview",
            "safe_areas",
        ] {
            assert!(!READABLE.contains(&never), "{never} must not be readable");
            assert!(!WRITABLE.contains(&never), "{never} must not be writable");
        }
        assert!(READABLE.contains(&"agent_review"));
        assert!(
            !WRITABLE.contains(&"agent_review"),
            "the agent cannot turn off its own review"
        );
    }

    #[test]
    fn readable_settings_leave_out_the_token_the_theme_and_the_server() {
        let stored = settings::Settings {
            theme: Some(serde_json::json!({"name": "mine"})),
            layout: Some(serde_json::json!({"grid": 1})),
            mcp_token: Some("t".repeat(48)),
            mcp_addr: Some("127.0.0.1:9000".to_string()),
            ..settings::Settings::default()
        };
        let view = readable_settings(&stored);
        let object = view.as_object().unwrap();
        for key in object.keys() {
            assert!(READABLE.contains(&key.as_str()), "{key} leaked");
        }
        assert_eq!(object["agent_review"], true);
        assert!(object.contains_key("auto_analysis") && object.contains_key("cpu_percent"));
        let text = view.to_string();
        assert!(
            !text.contains("ttttttttt") && !text.contains("9000") && !text.contains("mine"),
            "{text}"
        );
    }

    #[test]
    fn a_settings_patch_holds_only_what_was_asked_and_checks_ranges() {
        let none = settings_patch(&SetSettingsParams::default(), defaults()).unwrap();
        assert!(none.is_empty());

        let p = SetSettingsParams {
            cpu_percent: Some(50),
            proxy_size: Some(720),
            preview_source: Some(PreviewSourceParam::ProxyOnly),
            ..SetSettingsParams::default()
        };
        let patch = settings_patch(&p, defaults()).unwrap();
        assert_eq!(patch["cpu_percent"], 50);
        assert_eq!(patch["proxy_size"], 720);
        assert_eq!(patch["preview_source"], "proxy_only");
        assert_eq!(patch.len(), 3);

        for bad in [
            SetSettingsParams {
                cpu_percent: Some(0),
                ..SetSettingsParams::default()
            },
            SetSettingsParams {
                cpu_percent: Some(101),
                ..SetSettingsParams::default()
            },
            SetSettingsParams {
                proxy_size: Some(900),
                ..SetSettingsParams::default()
            },
        ] {
            let err = settings_patch(&bad, defaults()).expect_err("out of range");
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        }
    }

    #[test]
    fn a_partial_auto_analysis_keeps_the_other_switches() {
        let current = kerf_core::AutoAnalysis {
            scenes: false,
            ..defaults()
        };
        let p = SetSettingsParams {
            auto_analysis: Some(AutoAnalysisParams {
                transcript: Some(false),
                ..AutoAnalysisParams::default()
            }),
            ..SetSettingsParams::default()
        };
        let patch = settings_patch(&p, current).unwrap();
        let merged: kerf_core::AutoAnalysis = serde_json::from_value(patch["auto_analysis"].clone()).unwrap();
        assert!(!merged.transcript, "the one asked for");
        assert!(!merged.scenes, "kept off");
        assert!(merged.silence && merged.enabled);
    }

    #[test]
    fn the_settings_tool_schema_offers_no_forbidden_field() {
        let schema = serde_json::to_value(schemars::schema_for!(SetSettingsParams)).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        for key in properties.keys() {
            assert!(
                WRITABLE.contains(&key.as_str()),
                "{key} is in the schema but not on the allow-list"
            );
        }
        assert_eq!(properties.len(), WRITABLE.len());
    }

    #[test]
    fn every_writable_setting_is_one_the_settings_file_reads_back() {
        let p = SetSettingsParams {
            cpu_percent: Some(60),
            auto_analysis: Some(AutoAnalysisParams {
                silence: Some(false),
                ..AutoAnalysisParams::default()
            }),
            proxy_size: Some(1080),
            preview_source: Some(PreviewSourceParam::Original),
            time_display: Some(TimeDisplayParam::Seconds),
        };
        let patch = settings_patch(&p, defaults()).unwrap();
        assert_eq!(patch.len(), WRITABLE.len(), "every writable key was exercised");
        let read: settings::Settings = serde_json::from_value(serde_json::Value::Object(patch)).expect("the file reads it");
        assert_eq!(read.cpu_percent, 60);
        assert!(!read.auto_analysis.silence);
        assert_eq!(read.proxy_size, kerf_core::ProxySize::W1080);
        assert_eq!(read.preview_source, kerf_core::PreviewSource::Original);
        assert_eq!(read.time_display, settings::TimeDisplay::Seconds);
        let seen = readable_settings(&settings::Settings::default());
        assert!(seen.get("time_display").is_some(), "the display mode is readable: {seen}");
    }
}
