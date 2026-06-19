//! Kerf stdio MCP server.
//!
//! Exposes the Kerf timeline / media engine as MCP tools over stdio so an LLM
//! can inspect loaded media and assemble a non-destructive edit. All editing
//! tools mutate the in-memory `.kerf` timeline; `export` triggers the render.
//!
//! Usage:
//!   kerf-mcp [PATH_TO.kerf]
//! With no argument it serves a seeded in-memory sample project.

use std::sync::{Arc, Mutex};

use kerf_core::Project;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone)]
struct KerfMcp {
    project: Arc<Mutex<Project>>,
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

#[derive(Serialize)]
struct AssetMetadata {
    asset: kerf_core::Asset,
    analysis: Option<kerf_core::AssetAnalysis>,
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

    #[tool(
        description = "Analyze an asset (silence + scene detection, and transcription when configured) and cache the result"
    )]
    fn analyze_asset(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        json(&project.analyze_asset(id).map_err(core_err)?)
    }

    #[tool(description = "Cut [start, end) of an asset and append it to the matching track")]
    fn cut_clip(&self, Parameters(p): Parameters<CutClipParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        json(&project.cut_clip(id, p.start, p.end).map_err(core_err)?)
    }

    #[tool(description = "Add a clip referencing a source range of an asset to the timeline")]
    fn add_clip_to_timeline(&self, Parameters(p): Parameters<AddClipParams>) -> Result<String, McpError> {
        let asset_id = parse_id(&p.asset_id)?;
        let track_id = p.track_id.as_deref().map(parse_id).transpose()?;
        let project = self.lock();
        json(&project
            .add_clip_to_timeline(asset_id, track_id, p.source_in, p.source_out, p.timeline_start)
            .map_err(core_err)?)
    }

    #[tool(description = "Split a timeline clip at a timeline time into two adjacent clips")]
    fn split_at(&self, Parameters(p): Parameters<SplitParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let (left, right) = project.split_at(clip_id, p.at).map_err(core_err)?;
        json(&serde_json::json!({ "left": left, "right": right }))
    }

    #[tool(description = "Trim a clip's source in/out points (timeline position is preserved)")]
    fn trim(&self, Parameters(p): Parameters<TrimParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        json(&project.trim(clip_id, p.source_in, p.source_out).map_err(core_err)?)
    }

    #[tool(description = "Move a clip to a new index within its track (re-flows the track gaplessly)")]
    fn reorder(&self, Parameters(p): Parameters<ReorderParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        project.reorder(track_id, clip_id, p.new_index).map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(description = "Remove a clip from the timeline")]
    fn remove(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        project.remove(clip_id).map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(description = "Set the linear volume gain of a clip")]
    fn set_volume(&self, Parameters(p): Parameters<VolumeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        json(&project.set_volume(clip_id, p.volume).map_err(core_err)?)
    }

    #[tool(description = "Append the non-silent spans of an asset as clips, using cached analysis")]
    fn remove_silence(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        json(&project.remove_silence(id).map_err(core_err)?)
    }

    #[tool(description = "Append the full audio of an asset to the first audio track")]
    fn extract_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        json(&project.extract_audio(id).map_err(core_err)?)
    }

    #[tool(description = "Stitch the full length of several assets together in order")]
    fn concatenate(&self, Parameters(p): Parameters<ConcatParams>) -> Result<String, McpError> {
        let ids = p
            .asset_ids
            .iter()
            .map(|s| parse_id(s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let project = self.lock();
        json(&project.concatenate(&ids).map_err(core_err)?)
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
        json(&project.add_task(&p.prompt).map_err(core_err)?)
    }

    #[tool(
        description = "Claim the oldest queued task (marks it working) and return it; returns null when the queue is empty"
    )]
    fn claim_next_task(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.claim_next_task().map_err(core_err)?)
    }

    #[tool(description = "Mark a claimed task ready for the user to review, with a summary of the edits made")]
    fn complete_task(&self, Parameters(p): Parameters<CompleteTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        json(&project.complete_task(id, p.result).map_err(core_err)?)
    }

    #[tool(description = "Mark a task failed with an error message")]
    fn fail_task(&self, Parameters(p): Parameters<FailTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        json(&project.fail_task(id, &p.error).map_err(core_err)?)
    }
}

impl KerfMcp {
    fn new(project: Project) -> Self {
        Self {
            project: Arc::new(Mutex::new(project)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Project> {
        self.project.lock().expect("project mutex poisoned")
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
             cut/split/trim/add/reorder/remove tools. When finished call \
             complete_task with a short summary (or fail_task on error); the \
             user reviews and applies the staged edit. Call export to render."
                .to_string(),
        );
        info
    }
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr; stdout is reserved for the MCP transport.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let project = match std::env::args().nth(1) {
        Some(path) => {
            tracing::info!(%path, "opening project");
            Project::open(path)?
        }
        None => {
            tracing::info!("no project path given; serving seeded sample project");
            Project::sample()?
        }
    };

    let server = KerfMcp::new(project);
    tracing::info!("kerf-mcp serving on stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
