//! Embedded MCP server — the app *is* the MCP server.
//!
//! Hosts the Kerf timeline / media engine as MCP tools over a streamable-HTTP
//! endpoint (`/mcp`) on localhost, sharing the **same** `Project` the Tauri
//! commands edit. A connected LLM thus operates on the project the user has
//! open; after every mutation we emit a `project-changed` Tauri event so the
//! webview re-fetches and the edit shows up live in the GUI.
//!
//! Edits made through these tools are attributed to [`EditSource::Agent`]; the
//! actor is set per-operation under the shared lock (the GUI sets
//! [`EditSource::User`] the same way), so attribution stays correct even though
//! both front doors share one `Project`.

use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine as _;
use kerf_core::{EditSource, Project};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{session::local::LocalSessionManager, StreamableHttpService};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Default localhost bind address for the MCP endpoint; override with `KERF_MCP_ADDR`.
const DEFAULT_ADDR: &str = "127.0.0.1:7777";

#[derive(Clone)]
pub struct KerfMcp {
    project: Arc<Mutex<Project>>,
    app: AppHandle,
}

// ---- tool parameter schemas ------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AssetIdParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CutClipParams {
    #[schemars(description = "UUID of the source asset")]
    asset_id: String,
    #[schemars(description = "In-point in the source asset (seconds)")]
    start: f64,
    #[schemars(description = "Out-point in the source asset (seconds)")]
    end: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddClipParams {
    #[schemars(description = "UUID of the source asset")]
    asset_id: String,
    #[schemars(description = "Target track UUID; omit to auto-select by asset kind")]
    track_id: Option<String>,
    #[schemars(description = "In-point in the source asset (seconds)")]
    source_in: f64,
    #[schemars(description = "Out-point in the source asset (seconds)")]
    source_out: f64,
    #[schemars(description = "Timeline position (seconds); omit to append")]
    timeline_start: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SplitParams {
    #[schemars(description = "UUID of the clip to split")]
    clip_id: String,
    #[schemars(description = "Timeline time at which to split (seconds)")]
    at: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TrimParams {
    #[schemars(description = "UUID of the clip to trim")]
    clip_id: String,
    #[schemars(description = "New source in-point (seconds)")]
    source_in: Option<f64>,
    #[schemars(description = "New source out-point (seconds)")]
    source_out: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ReorderParams {
    #[schemars(description = "UUID of the track containing the clip")]
    track_id: String,
    #[schemars(description = "UUID of the clip to move")]
    clip_id: String,
    #[schemars(description = "New zero-based index within the track")]
    new_index: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ClipIdParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct VolumeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Linear gain (1.0 = unchanged, 0.0 = muted)")]
    volume: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ConcatParams {
    #[schemars(description = "Ordered list of asset UUIDs to stitch together")]
    asset_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RevertParams {
    #[schemars(description = "Revision seq to jump the timeline back to (see history)")]
    seq: i64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportParams {
    #[schemars(description = "Output file path for the rendered result")]
    output_path: String,
    #[schemars(description = "Container/format hint, e.g. \"mp4\"")]
    format: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddTaskParams {
    #[schemars(description = "What the task should accomplish, in plain language")]
    prompt: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CompleteTaskParams {
    #[schemars(description = "UUID of the task to complete")]
    task_id: String,
    #[schemars(description = "Short summary of the edits made, shown to the user")]
    result: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FailTaskParams {
    #[schemars(description = "UUID of the task that could not be completed")]
    task_id: String,
    #[schemars(description = "Why the task failed")]
    error: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TaskIdParams {
    #[schemars(description = "UUID of the task")]
    task_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WaveformParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Number of peak-magnitude buckets to return")]
    buckets: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FrameParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Time in the source asset to decode (seconds)")]
    time_secs: f64,
    #[schemars(description = "Maximum output width in pixels (default 960)")]
    max_width: Option<u32>,
}

#[derive(Serialize)]
struct AssetMetadata {
    asset: kerf_core::Asset,
    analysis: Option<kerf_core::AssetAnalysis>,
}

#[derive(Serialize)]
struct TrackSummary {
    id: String,
    name: String,
    kind: String,
    clip_count: usize,
    duration_secs: f64,
}

#[derive(Serialize)]
struct TimelineSummary {
    total_duration_secs: f64,
    track_count: usize,
    total_clip_count: usize,
    tracks: Vec<TrackSummary>,
}

// ---- tools -----------------------------------------------------------------

#[tool_router]
impl KerfMcp {
    #[tool(description = "List all media assets in the project")]
    fn list_assets(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.list_assets().map_err(core_err)?)
    }

    #[tool(description = "Get an asset's probed metadata and cached analysis (silence, scenes, transcript)")]
    fn get_asset_metadata(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let asset = project.require_asset(id).map_err(core_err)?;
        let analysis = project.get_analysis(id).map_err(core_err)?;
        json(&AssetMetadata { asset, analysis })
    }

    #[tool(description = "Get the full non-destructive timeline state (tracks and clips)")]
    fn get_timeline_state(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.timeline().map_err(core_err)?)
    }

    #[tool(description = "Analyze an asset (silence + scene detection, and transcription when configured) and cache the result")]
    fn analyze_asset(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let analysis = project.analyze_asset(id).map_err(core_err)?;
        self.changed();
        json(&analysis)
    }

    #[tool(description = "Cut [start, end) of an asset and append it to the matching track")]
    fn cut_clip(&self, Parameters(p): Parameters<CutClipParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let out = project.cut_clip(id, p.start, p.end).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Add a clip referencing a source range of an asset to the timeline")]
    fn add_clip_to_timeline(&self, Parameters(p): Parameters<AddClipParams>) -> Result<String, McpError> {
        let asset_id = parse_id(&p.asset_id)?;
        let track_id = p.track_id.as_deref().map(parse_id).transpose()?;
        let project = self.lock();
        let out = project
            .add_clip_to_timeline(asset_id, track_id, p.source_in, p.source_out, p.timeline_start)
            .map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Split a timeline clip at a timeline time into two adjacent clips")]
    fn split_at(&self, Parameters(p): Parameters<SplitParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let (left, right) = project.split_at(clip_id, p.at).map_err(core_err)?;
        self.changed();
        json(&serde_json::json!({ "left": left, "right": right }))
    }

    #[tool(description = "Trim a clip's source in/out points (timeline position is preserved)")]
    fn trim(&self, Parameters(p): Parameters<TrimParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.trim(clip_id, p.source_in, p.source_out).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Move a clip to a new index within its track (re-flows the track gaplessly)")]
    fn reorder(&self, Parameters(p): Parameters<ReorderParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        project.reorder(track_id, clip_id, p.new_index).map_err(core_err)?;
        self.changed();
        Ok("ok".to_string())
    }

    #[tool(description = "Remove a clip from the timeline")]
    fn remove(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        project.remove(clip_id).map_err(core_err)?;
        self.changed();
        Ok("ok".to_string())
    }

    #[tool(description = "Set the linear volume gain of a clip")]
    fn set_volume(&self, Parameters(p): Parameters<VolumeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.set_volume(clip_id, p.volume).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Append the non-silent spans of an asset as clips, using cached analysis")]
    fn remove_silence(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let out = project.remove_silence(id).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Append the full audio of an asset to the first audio track")]
    fn extract_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let out = project.extract_audio(id).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Stitch the full length of several assets together in order")]
    fn concatenate(&self, Parameters(p): Parameters<ConcatParams>) -> Result<String, McpError> {
        let ids = p.asset_ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<Uuid>, _>>()?;
        let project = self.lock();
        let out = project.concatenate(&ids).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "List the timeline edit history (revisions, newest changes have higher seq; the current one is marked)")]
    fn history(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.history().map_err(core_err)?)
    }

    #[tool(description = "Undo the last timeline edit, returning the restored timeline")]
    fn undo(&self) -> Result<String, McpError> {
        let project = self.lock();
        let out = project.undo().map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Redo the next timeline edit, returning the restored timeline")]
    fn redo(&self) -> Result<String, McpError> {
        let project = self.lock();
        let out = project.redo().map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Revert the timeline to a specific revision seq (see history), returning the restored timeline")]
    fn revert_to(&self, Parameters(p): Parameters<RevertParams>) -> Result<String, McpError> {
        let project = self.lock();
        let out = project.revert_to(p.seq).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Render the timeline to a file (requires the ffmpeg feature)")]
    fn export(&self, Parameters(p): Parameters<ExportParams>) -> Result<String, McpError> {
        let project = self.lock();
        let output = project.export(&p.output_path, &p.format).map_err(core_err)?;
        json(&serde_json::json!({ "output": output.to_string_lossy() }))
    }

    // ---- agent task queue --------------------------------------------------

    #[tool(description = "List the agent task queue with each task's status (queued/working/ready/done/failed)")]
    fn list_tasks(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.list_tasks().map_err(core_err)?)
    }

    #[tool(description = "Enqueue a new task (status: queued) for an agent to claim")]
    fn add_task(&self, Parameters(p): Parameters<AddTaskParams>) -> Result<String, McpError> {
        let project = self.lock();
        let task = project.add_task(&p.prompt).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Claim the oldest queued task (marks it working) and return it; returns null when the queue is empty")]
    fn claim_next_task(&self) -> Result<String, McpError> {
        let project = self.lock();
        let task = project.claim_next_task().map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Mark a claimed task ready for the user to review, with a summary of the edits made")]
    fn complete_task(&self, Parameters(p): Parameters<CompleteTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        let task = project.complete_task(id, p.result).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Mark a task failed with an error message")]
    fn fail_task(&self, Parameters(p): Parameters<FailTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        let task = project.fail_task(id, &p.error).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Mark a task done (user accepted the staged edit), returning the updated task")]
    fn resolve_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        let task = project.resolve_task(id).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Remove a task from the queue permanently, returning the updated task list")]
    fn remove_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        project.remove_task(id).map_err(core_err)?;
        self.changed();
        json(&project.list_tasks().map_err(core_err)?)
    }

    #[tool(description = "Get peak-magnitude waveform data (0.0–1.0) for an asset's first audio stream")]
    fn get_waveform(&self, Parameters(p): Parameters<WaveformParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let buckets = project.waveform(id, p.buckets).map_err(core_err)?;
        json(&buckets)
    }

    #[tool(description = "Decode a single frame from an asset and return it as a base64 PNG data URL")]
    fn get_frame(&self, Parameters(p): Parameters<FrameParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let png = project
            .frame_at(id, p.time_secs, p.max_width.unwrap_or(960))
            .map_err(core_err)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        Ok(format!("data:image/png;base64,{b64}"))
    }

    #[tool(description = "Summarise the timeline: total duration, track count, clips per track, and any per-track gaps")]
    fn timeline_summary(&self) -> Result<String, McpError> {
        let project = self.lock();
        let timeline = project.timeline().map_err(core_err)?;
        let tracks: Vec<TrackSummary> = timeline
            .tracks
            .iter()
            .map(|t| TrackSummary {
                id: t.id.to_string(),
                name: t.name.clone(),
                kind: format!("{:?}", t.kind).to_lowercase(),
                clip_count: t.clips.len(),
                duration_secs: t.end(),
            })
            .collect();
        let total_clip_count = tracks.iter().map(|t| t.clip_count).sum();
        let summary = TimelineSummary {
            total_duration_secs: timeline.duration(),
            track_count: tracks.len(),
            total_clip_count,
            tracks,
        };
        json(&summary)
    }
}

impl KerfMcp {
    fn new(project: Arc<Mutex<Project>>, app: AppHandle) -> Self {
        Self { project, app }
    }

    /// Lock the shared project, attributing any edits made under this guard to
    /// the agent. The GUI sets `User` the same way, and the mutex keeps the two
    /// from interleaving within a single operation.
    fn lock(&self) -> MutexGuard<'_, Project> {
        let mut guard = self.project.lock().expect("project mutex poisoned");
        guard.set_actor(EditSource::Agent);
        guard
    }

    /// Tell the webview the project changed so it re-fetches and renders live.
    fn changed(&self) {
        if let Err(e) = self.app.emit("project-changed", ()) {
            tracing::warn!(error = %e, "failed to emit project-changed");
        }
    }
}

#[tool_handler]
impl ServerHandler for KerfMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Kerf MCP server. The user queues editing tasks in the desktop app; \
             call claim_next_task to take the oldest one (or list_tasks to see \
             the whole queue). To work a task, inspect loaded media with \
             list_assets / get_asset_metadata / get_timeline_state, run \
             analyze_asset to populate silence / scene / transcript metadata, \
             then assemble a non-destructive edit with the \
             cut/split/trim/add/reorder/remove tools. Every edit is tracked: use \
             history to list revisions and undo / redo / revert_to to roll \
             changes back. When finished call complete_task with a short summary \
             (or fail_task on error); the user reviews and applies the staged \
             edit. Call export to render."
                .to_string(),
        );
        info
    }
}

// ---- server ----------------------------------------------------------------

/// Serve the MCP tools over streamable HTTP at `/mcp`, sharing `project` with
/// the Tauri commands. Runs until the process exits.
pub async fn serve(project: Arc<Mutex<Project>>, app: AppHandle) -> anyhow::Result<()> {
    let addr = std::env::var("KERF_MCP_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());

    let service = StreamableHttpService::new(
        move || Ok(KerfMcp::new(project.clone(), app.clone())),
        LocalSessionManager::default().into(),
        Default::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "kerf MCP server listening on http://{addr}/mcp");
    axum::serve(listener, router).await?;
    Ok(())
}

// ---- helpers ---------------------------------------------------------------

fn parse_id(s: &str) -> Result<Uuid, McpError> {
    Uuid::parse_str(s).map_err(|e| McpError::invalid_params(format!("invalid uuid '{s}': {e}"), None))
}

fn core_err(e: kerf_core::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn json<T: Serialize>(value: &T) -> Result<String, McpError> {
    serde_json::to_string_pretty(value).map_err(|e| McpError::internal_error(e.to_string(), None))
}
