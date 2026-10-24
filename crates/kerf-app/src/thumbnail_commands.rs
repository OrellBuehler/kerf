//! The webview's side of thumbnails (the agent's is `mcp/thumbnails.rs`): the same core calls, each
//! prepared or drawn with the project lock released.

use kerf_core::thumbnail::SuggestedFrame;
use kerf_core::{Project, ThumbnailSpec, TimeRange, Timeline};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::{blocking, check_output, id, lock_user, opt_id, require_user_output_path, AppState, CmdResult};

/// What a thumbnail edit answers: the timeline it left, the thumbnail and what could not be honoured.
#[derive(Serialize)]
pub(crate) struct ThumbnailEdit {
    timeline: Timeline,
    thumbnail_id: Uuid,
    warnings: Vec<String>,
}

/// Add a thumbnail (`thumbnail_id` absent) or change one to `spec`: built before the lock (templates,
/// the sanitiser), stored under it.
#[tauri::command]
pub(crate) async fn set_thumbnail(
    state: State<'_, AppState>,
    thumbnail_id: Option<String>,
    spec: ThumbnailSpec,
) -> CmdResult<ThumbnailEdit> {
    let thumbnail = opt_id(thumbnail_id.as_deref(), "thumbnail_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let uses_templates = spec
            .layers
            .as_ref()
            .is_some_and(|ls| ls.iter().any(|l| l.template_id.is_some()));
        let library = uses_templates
            .then(kerf_core::TemplateLibrary::default_location)
            .transpose()
            .map_err(|e| e.to_string())?;
        let base = lock_user(&shared).thumbnail_base(thumbnail).map_err(|e| e.to_string())?;
        let prepared = kerf_core::project::prepare_thumbnail(base, &spec, library.as_ref()).map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project.apply_thumbnail_parsed(prepared).map_err(|e| e.to_string())?;
        Ok(ThumbnailEdit {
            timeline: project.timeline().map_err(|e| e.to_string())?,
            thumbnail_id: done.thumbnail.id,
            warnings: done.warnings,
        })
    })
    .await
}

#[tauri::command(async)]
pub(crate) fn remove_thumbnail(state: State<'_, AppState>, thumbnail_id: String) -> CmdResult<Timeline> {
    let thumbnail = id(&thumbnail_id, "thumbnail_id")?;
    let project = state.project();
    project.remove_thumbnail(thumbnail).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// The thumbnail drawn `width` pixels wide from the proxies, as PNG bytes.
#[tauri::command]
pub(crate) async fn thumbnail_preview(
    state: State<'_, AppState>,
    thumbnail_id: String,
    width: u32,
) -> CmdResult<tauri::ipc::Response> {
    let thumbnail = id(&thumbnail_id, "thumbnail_id")?;
    let shared = state.project.clone();
    let png = blocking(move || {
        let (t, timeline, assets) = lock_user(&shared)
            .thumbnail_render_inputs(thumbnail)
            .map_err(|e| e.to_string())?;
        let assets = Project::preview_assets_of(assets);
        let picture = kerf_core::thumbnail::render(&t, &timeline, &assets, width, &|| false).map_err(|e| e.to_string())?;
        kerf_core::svg::raster::encode_png(&picture).map_err(|e| e.to_string())
    })
    .await?;
    Ok(tauri::ipc::Response::new(png))
}

/// Write the thumbnail at full size to `output_path` (`.png`, `.jpg` or `.webp`), from the originals.
/// The cover's Cancel stops it.
#[tauri::command]
pub(crate) async fn export_thumbnail(state: State<'_, AppState>, thumbnail_id: String, output_path: String) -> CmdResult<String> {
    require_user_output_path(&output_path)?;
    let thumbnail = id(&thumbnail_id, "thumbnail_id")?;
    let shared = state.project.clone();
    let cancel = state.cover_cancel.begin();
    blocking(move || {
        let ((t, timeline, assets), protected) = {
            let project = lock_user(&shared);
            (
                project.thumbnail_render_inputs(thumbnail).map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        let path = std::path::Path::new(&output_path);
        check_output(path, &protected).map_err(|e| e.to_string())?;
        let written = kerf_core::thumbnail::write(&t, &timeline, &assets, path, &|| cancel()).map_err(|e| match e {
            kerf_core::Error::Cancelled => crate::COVER_CANCELLED.to_string(),
            other => other.to_string(),
        })?;
        Ok(written.to_string_lossy().into_owned())
    })
    .await
}

/// The best frames of the cut (or of `range`) for a thumbnail, best first.
#[tauri::command]
pub(crate) async fn suggest_thumbnail_frames(
    state: State<'_, AppState>,
    count: Option<usize>,
    range: Option<TimeRange>,
) -> CmdResult<Vec<SuggestedFrame>> {
    let shared = state.project.clone();
    blocking(move || {
        let (timeline, assets, scenes) = lock_user(&shared).frame_suggestion_inputs().map_err(|e| e.to_string())?;
        let assets = Project::preview_assets_of(assets);
        let range = range.map(|r| (r.start, r.end));
        kerf_core::thumbnail::suggest_frames(&timeline, &assets, &scenes, count.unwrap_or(6), range, &|| false)
            .map(|found| found.frames)
            .map_err(|e| e.to_string())
    })
    .await
}
