//! Motion templates, title clips and their text, captions, interchange exports, music fit and the silence / transcript edits.

use crate::*;

// ---- motion templates --------------------------------------------------------

/// The user's template library (`KERF_TEMPLATES_DIR` or the data directory). Library calls need no
/// project, so none of them takes the lock.
fn template_library() -> CmdResult<TemplateLibrary> {
    TemplateLibrary::default_location().map_err(|e| e.to_string())
}

/// Only `.kerftemplate` files are read as templates, and the path is the one a file picker returned.
pub(crate) fn require_template_path(path: &str) -> CmdResult<()> {
    let is_template = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(kerf_core::template::EXTENSION));
    if is_template {
        Ok(())
    } else {
        Err(format!("{path} is not a .{} file", kerf_core::template::EXTENSION))
    }
}

/// What applying a template or changing the values of a template title hands back: the refreshed cut,
/// the title that was made or changed, and what the sanitiser or the machine could not honour (a font
/// that is not installed, ...).
#[derive(Serialize)]
pub(crate) struct TemplateEdit {
    timeline: Timeline,
    /// The floating title made or changed; absent when it is a title on a track (`clip_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    overlay_id: Option<Uuid>,
    /// The title clip made or changed, when the title is on a track.
    #[serde(skip_serializing_if = "Option::is_none")]
    clip_id: Option<Uuid>,
    warnings: Vec<String>,
}

impl TemplateEdit {
    fn of_clip(timeline: Timeline, done: TitleClipResult) -> Self {
        Self {
            timeline,
            overlay_id: None,
            clip_id: Some(done.clip.id),
            warnings: done.warnings,
        }
    }
}

/// Where a new title clip goes: a video track (by default the lowest one free over its span, else a
/// new one on top) and its name.
fn title_placement(track_id: Option<String>, name: Option<String>) -> CmdResult<TitleClipPlacement> {
    Ok(TitleClipPlacement {
        track_id: track_id.as_deref().map(|t| id(t, "track_id")).transpose()?,
        name,
    })
}

/// An element of a title that has an `id`, and the tag it is: what a control can aim at.
#[derive(Serialize)]
pub(crate) struct SvgElementId {
    pub(crate) id: String,
    pub(crate) tag: String,
}

/// Every template in the library (by name) and the files that did not read. Only manifests are read;
/// a poster is fetched on its own (`template_poster`).
#[tauri::command]
pub(crate) async fn list_templates() -> CmdResult<TemplateList> {
    blocking(|| template_library()?.list().map_err(|e| e.to_string())).await
}

/// A template's poster as a `data:image/png;base64,` URL (a PNG of at most 512 px), `null` when it has
/// none.
#[tauri::command]
pub(crate) async fn template_poster(template_id: String) -> CmdResult<Option<String>> {
    blocking(move || {
        let poster = template_library()?.poster(&template_id).map_err(|e| e.to_string())?;
        Ok(poster.map(|png| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )
        }))
    })
    .await
}

/// The `id`s of a title's elements and their tags, for the save dialog to offer as targets. Pure.
#[tauri::command(async)]
pub(crate) fn svg_element_ids(svg: String) -> CmdResult<Vec<SvgElementId>> {
    let ids = kerf_core::template::element_ids(&svg).map_err(|e| e.to_string())?;
    Ok(ids.into_iter().map(|(id, tag)| SvgElementId { id, tag }).collect())
}

/// An image file as the `data:` URL a template `image` value takes (PNG, JPEG, GIF or WebP).
#[tauri::command]
pub(crate) async fn template_image_value(path: String) -> CmdResult<String> {
    blocking(move || template_image_data_url(&path).map_err(|e| e.to_string())).await
}

/// Keep the title `overlay_id` (as the working cut has it) in the library as a template with the given
/// controls. The title is read under the lock; the fonts, the sanitiser and the poster run with it
/// released. A library write, not a timeline edit: nothing is undone or staged.
#[tauri::command]
pub(crate) async fn save_template(
    state: State<'_, AppState>,
    overlay_id: String,
    name: String,
    description: Option<String>,
    controls: Vec<TemplateControl>,
) -> CmdResult<TemplateSaved> {
    let oid = id(&overlay_id, "overlay_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let source = lock_user(&shared).template_source(oid).map_err(|e| e.to_string())?;
        let request = TemplateSaveRequest {
            name,
            description: description.unwrap_or_default(),
            controls,
        };
        template_library()?.save(&source, request).map_err(|e| e.to_string())
    })
    .await
}

/// Add a library template to the timeline as an animated title: `values` (control id to value, partial)
/// are spliced into its artwork and the result sanitised **before** the project lock is taken; the lock
/// only places the title and stores the template's fonts in the project. `end` defaults to `start` plus
/// the template's duration; `layout` places and names it like `add_svg_overlay`'s.
#[tauri::command]
pub(crate) async fn apply_template(
    state: State<'_, AppState>,
    template_id: String,
    values: std::collections::BTreeMap<String, String>,
    start: f64,
    end: Option<f64>,
    layout: Option<SvgLayout>,
) -> CmdResult<TemplateEdit> {
    let shared = state.project.clone();
    blocking(move || {
        let prepared = template_library()?
            .get(&template_id)
            .and_then(|t| t.prepare(&values))
            .map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project
            .apply_template_parsed(prepared, start, end, layout.unwrap_or_default())
            .map_err(|e| e.to_string())?;
        Ok(TemplateEdit {
            timeline: project.timeline().map_err(|e| e.to_string())?,
            overlay_id: Some(done.overlay.id),
            clip_id: None,
            warnings: done.warnings,
        })
    })
    .await
}

/// Add a library template as a title clip on a video track, so it takes transitions, fades and keys
/// like any clip: prepared before the lock, as `apply_template`. It runs from `start` for `duration`
/// seconds (the template's own length by default) on `track_id` (by default the lowest video track
/// free over its span, else a new one on top).
#[tauri::command]
pub(crate) async fn apply_template_clip(
    state: State<'_, AppState>,
    template_id: String,
    values: std::collections::BTreeMap<String, String>,
    start: f64,
    duration: Option<f64>,
    track_id: Option<String>,
    name: Option<String>,
) -> CmdResult<TemplateEdit> {
    let placement = title_placement(track_id, name)?;
    let shared = state.project.clone();
    blocking(move || {
        let prepared = template_library()?
            .get(&template_id)
            .and_then(|t| t.prepare(&values))
            .map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project
            .apply_template_clip_parsed(prepared, start, duration, placement)
            .map_err(|e| e.to_string())?;
        Ok(TemplateEdit::of_clip(project.timeline().map_err(|e| e.to_string())?, done))
    })
    .await
}

/// Add custom SVG artwork as a title clip on a video track (read before the lock). `duration` defaults
/// to how long its animations run (2–30 s, else 4).
#[tauri::command]
pub(crate) async fn add_title_clip(
    state: State<'_, AppState>,
    svg: String,
    start: f64,
    duration: Option<f64>,
    track_id: Option<String>,
    name: Option<String>,
) -> CmdResult<TemplateEdit> {
    let placement = title_placement(track_id, name)?;
    let shared = state.project.clone();
    blocking(move || {
        let doc = kerf_core::SvgDoc::parse(&svg).map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project
            .add_title_clip_parsed(doc, start, duration, placement)
            .map_err(|e| e.to_string())?;
        Ok(TemplateEdit::of_clip(project.timeline().map_err(|e| e.to_string())?, done))
    })
    .await
}

/// Rename a title clip or replace its artwork (`svg`, read before the lock; it ends a template). Its
/// timing, place and look are the clip's own edits.
#[tauri::command]
pub(crate) async fn update_title_clip(
    state: State<'_, AppState>,
    clip_id: String,
    patch: TitleClipPatch,
) -> CmdResult<TemplateEdit> {
    let cid = id(&clip_id, "clip_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let doc = patch
            .svg
            .as_deref()
            .map(kerf_core::SvgDoc::parse)
            .transpose()
            .map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project.update_title_clip_parsed(cid, patch, doc).map_err(|e| e.to_string())?;
        Ok(TemplateEdit::of_clip(project.timeline().map_err(|e| e.to_string())?, done))
    })
    .await
}

/// A change to a title's text made the way template values are: the title read under the lock, the new
/// artwork made with it released (`prepare`: the sanitiser, the text engine), stored if the title is still
/// the one it was made from. One edit called `label`.
fn title_text_edit(
    shared: &Mutex<Project>,
    title_id: Uuid,
    label: &str,
    prepare: impl FnOnce(&kerf_core::TitleTextInputs) -> kerf_core::Result<kerf_core::PreparedText>,
) -> CmdResult<TemplateEdit> {
    let inputs = lock_user(shared).title_text_inputs(title_id).map_err(|e| e.to_string())?;
    let prepared = prepare(&inputs).map_err(|e| e.to_string())?;
    let project = lock_user(shared);
    let done = project
        .set_title_text_parsed(title_id, prepared, label)
        .map_err(|e| e.to_string())?;
    let timeline = project.timeline().map_err(|e| e.to_string())?;
    Ok(match done {
        TitleEdited::Overlay(o) => TemplateEdit {
            timeline,
            overlay_id: Some(o.overlay.id),
            clip_id: None,
            warnings: o.warnings,
        },
        TitleEdited::Clip(c) => TemplateEdit::of_clip(timeline, *c),
    })
}

/// Give the `<text>` whose id is `element` in the title `title_id` (floating or on a track, not a preset)
/// a box: a width its lines wrap to or it shrinks to fit (`text_box.width` `null` takes it away). A box is
/// drawn only by glyph outlines, so the title's text is set as outlines again; a box for a text that
/// cannot be one is an error saying why.
#[tauri::command]
pub(crate) async fn set_title_text_box(
    state: State<'_, AppState>,
    title_id: String,
    element: String,
    text_box: kerf_core::svg::TextBox,
) -> CmdResult<TemplateEdit> {
    let tid = id(&title_id, "title_id")?;
    let shared = state.project.clone();
    blocking(move || title_text_edit(&shared, tid, "Set text box", |i| i.text_box(&element, &text_box))).await
}

/// Give the `<text>` whose id is `element` in the title `title_id` (floating or on a track, not a preset) an
/// animation: which units of it (characters, words, lines) a window slides over, and what each does as the window
/// reaches it (`animation` `null` takes it away). The animation is compiled into per-glyph SMIL when the text is
/// set as outlines, so the title's text is set as outlines again; a text that cannot be one is an error saying why.
#[tauri::command]
pub(crate) async fn set_title_text_animation(
    state: State<'_, AppState>,
    title_id: String,
    element: String,
    animation: Option<kerf_core::svg::TextAnimation>,
) -> CmdResult<TemplateEdit> {
    let tid = id(&title_id, "title_id")?;
    let shared = state.project.clone();
    blocking(move || {
        title_text_edit(&shared, tid, "Set text animation", |i| {
            i.text_animation(&element, animation.as_ref())
        })
    })
    .await
}

/// The text animations a user can start from (`rise`, `typewriter`, ...): each a complete `TextAnimation` to copy
/// onto a text with `set_title_text_animation`. Needs no project.
#[tauri::command(async)]
pub(crate) fn list_text_animation_presets() -> Vec<kerf_core::svg::TextAnimationPreset> {
    kerf_core::svg::text_animation_presets()
}

/// The curves a text animation's moving properties can be given (`ease_out`, `overshoot`, `spring`, ...): each a
/// `Curve` (`null` for the straight line) to put on an animation's `curves`. Needs no project.
#[tauri::command(async)]
pub(crate) fn list_text_animation_curves() -> Vec<kerf_core::svg::CurvePreset> {
    kerf_core::svg::curve_presets()
}

/// Draw the text of the title `title_id` as glyph outlines (the same on every machine, whatever fonts it
/// has) or with its fonts again. Each text that cannot be an outline says why in the warnings.
#[tauri::command]
pub(crate) async fn convert_title_text(
    state: State<'_, AppState>,
    title_id: String,
    form: kerf_core::TitleTextForm,
) -> CmdResult<TemplateEdit> {
    let tid = id(&title_id, "title_id")?;
    let shared = state.project.clone();
    let label = match form {
        kerf_core::TitleTextForm::Outlines => "Draw text as outlines",
        kerf_core::TitleTextForm::Text => "Draw text with its fonts",
    };
    blocking(move || title_text_edit(&shared, tid, label, |i| i.convert(form))).await
}

/// Change control values of a template title, floating or on a track (partial: only the named
/// controls; `overlay_id` is either's id). The title is read under the lock, the splice and the
/// sanitiser run with it released, and the result is stored only if the title is still the one it was
/// made from; one that changed meanwhile is an error saying to try again.
#[tauri::command]
pub(crate) async fn set_template_values(
    state: State<'_, AppState>,
    overlay_id: String,
    values: std::collections::BTreeMap<String, String>,
) -> CmdResult<TemplateEdit> {
    let oid = id(&overlay_id, "overlay_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let inputs = lock_user(&shared).template_edit_inputs(oid).map_err(|e| e.to_string())?;
        let prepared = inputs.prepare(&values).map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let done = project.set_title_values_parsed(oid, prepared).map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(match done {
            TitleEdited::Overlay(o) => TemplateEdit {
                timeline,
                overlay_id: Some(o.overlay.id),
                clip_id: None,
                warnings: o.warnings,
            },
            TitleEdited::Clip(c) => TemplateEdit::of_clip(timeline, *c),
        })
    })
    .await
}

/// Bring a `.kerftemplate` file into the library. Never overwrites: the same template is
/// `already_present`, another one with the same id is kept under a new id (`added_as_copy`). The
/// path is the one the file picker returned: an absolute path to a regular `.kerftemplate` file.
#[tauri::command]
pub(crate) async fn import_template(path: String) -> CmdResult<ImportOutcome> {
    require_template_path(&path)?;
    blocking(move || {
        let file = std::path::Path::new(&path);
        kerf_core::paths::validate_import_path(file).map_err(|e| e.to_string())?;
        template_library()?.import(file).map_err(|e| e.to_string())
    })
    .await
}

/// Write a library template to `path` (a directory gets `<name>.kerftemplate` in it, a path with no
/// extension gets `.kerftemplate`); resolves to where it went. The picker has asked about replacing.
/// Not onto a file the project still needs, into Kerf's own settings and logs, or into the library.
#[tauri::command]
pub(crate) async fn export_template(state: State<'_, AppState>, template_id: String, path: String) -> CmdResult<String> {
    require_user_output_path(&path)?;
    let shared = state.project.clone();
    blocking(move || {
        let library = template_library()?;
        let mut protected = lock_user(&shared).output_path_inputs().map_err(|e| e.to_string())?;
        protected
            .dirs
            .push((library.dir().to_path_buf(), "the template library".to_string()));
        check_output(std::path::Path::new(&path), &protected).map_err(|e| e.to_string())?;
        let written = library
            .export(&template_id, std::path::Path::new(&path))
            .map_err(|e| e.to_string())?;
        Ok(written.to_string_lossy().into_owned())
    })
    .await
}

/// Remove a template from the library. Titles already made from it keep working: they carry their own
/// artwork and fonts.
#[tauri::command]
pub(crate) async fn delete_template(template_id: String) -> CmdResult<()> {
    blocking(move || template_library()?.delete(&template_id).map_err(|e| e.to_string())).await
}

#[tauri::command(async)]
pub(crate) fn generate_captions(state: State<'_, AppState>, options: Option<CaptionOptions>) -> CmdResult<Timeline> {
    let project = state.project();
    project
        .generate_captions(options.unwrap_or_default())
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// What the cut says: every audible clip's cached transcript in timeline time, and the assets the cut
/// plays with no transcript yet. The cache is read under the lock and mapped with it released.
#[tauri::command]
pub(crate) async fn cut_transcript(state: State<'_, AppState>) -> CmdResult<kerf_core::CutTranscript> {
    let shared = state.project.clone();
    blocking(move || {
        let inputs = lock_user(&shared).cut_transcript_inputs().map_err(|e| e.to_string())?;
        Ok(inputs.read())
    })
    .await
}

#[tauri::command(async)]
pub(crate) fn clear_captions(state: State<'_, AppState>) -> CmdResult<Timeline> {
    let project = state.project();
    project.clear_captions().map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// What importing a subtitle file hands back: the refreshed cut, and what the
/// import did (cues read, placed, skipped, dropped) for the toast.
#[derive(Serialize)]
pub(crate) struct CaptionImport {
    timeline: Timeline,
    summary: ImportSummary,
}

/// The loose arguments every caption import takes — which clock the file's times
/// are on, the asset a `source` clock is about, the look, a time offset — as the
/// one request the engine works from. The combination rules are the engine's (the
/// MCP tool shares them); this only parses the asset id.
fn caption_request(
    base: Option<&str>,
    asset_id: Option<&str>,
    options: Option<CaptionOptions>,
    offset: Option<f64>,
) -> CmdResult<CaptionImportRequest> {
    let asset = opt_id(asset_id, "asset_id")?;
    Ok(CaptionImportRequest {
        base: CaptionTimeBase::resolve(base, asset).map_err(|e| e.to_string())?,
        options: options.unwrap_or_default(),
        offset: offset.unwrap_or(0.0),
    })
}

/// The part of an import that holds the project lock: place a file that was
/// already read and parsed with the lock released.
fn place_caption_file(
    shared: &Mutex<Project>,
    generation: u64,
    file: &CaptionFile,
    req: CaptionImportRequest,
) -> CmdResult<CaptionImport> {
    let project = lock_user(shared);
    require_generation(&project, generation, "captions were being imported")?;
    let summary = project.import_captions(file, req).map_err(|e| e.to_string())?;
    let timeline = project.timeline().map_err(|e| e.to_string())?;
    Ok(CaptionImport { timeline, summary })
}

/// Caption the cut from a `.srt` / `.ass` / `.ssa` file on disk. `base` is
/// `timeline` (the file is a subtitle track for the finished cut — the default)
/// or `source` with an `asset_id` (the file times that asset's own footage, and
/// is projected through its clips like a transcript); `options` is the same
/// caption look `generate_captions` takes; `offset` is seconds added to every cue
/// first (negative moves them earlier). The file is read **and parsed** before
/// the project lock is taken — only the placement holds it — and the import is
/// one `Import captions` revision that replaces the previous generated /
/// imported captions.
#[tauri::command]
pub(crate) async fn import_captions(
    state: State<'_, AppState>,
    path: String,
    base: Option<String>,
    asset_id: Option<String>,
    options: Option<CaptionOptions>,
    offset: Option<f64>,
) -> CmdResult<CaptionImport> {
    let req = caption_request(base.as_deref(), asset_id.as_deref(), options, offset)?;
    let shared = state.project.clone();
    blocking(move || {
        let generation = lock_user(&shared).generation();
        let text = kerf_core::read_caption_file(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
        let file = kerf_core::parse_captions(&text, None).map_err(|e| e.to_string())?;
        place_caption_file(&shared, generation, &file, req)
    })
    .await
}

/// [`import_captions`] for text the webview already holds (a file read through
/// an `<input type=file>`, or pasted). `format` is `srt` / `ass` or omitted to
/// guess from the text. Parsed on the blocking pool with the lock released, like
/// the file variant.
#[tauri::command]
pub(crate) async fn import_captions_text(
    state: State<'_, AppState>,
    text: String,
    format: Option<String>,
    base: Option<String>,
    asset_id: Option<String>,
    options: Option<CaptionOptions>,
    offset: Option<f64>,
) -> CmdResult<CaptionImport> {
    let req = caption_request(base.as_deref(), asset_id.as_deref(), options, offset)?;
    let format = CaptionFormat::from_arg(format.as_deref()).map_err(|e| e.to_string())?;
    let shared = state.project.clone();
    blocking(move || {
        let generation = lock_user(&shared).generation();
        let file = kerf_core::parse_captions(&text, format).map_err(|e| e.to_string())?;
        place_caption_file(&shared, generation, &file, req)
    })
    .await
}

#[tauri::command]
pub(crate) async fn export_srt(state: State<'_, AppState>, asset_id: String, output_path: String) -> CmdResult<String> {
    let id = id(&asset_id, "asset_id")?;
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    blocking(move || {
        let (protected, srt) = {
            let project = lock_user(&shared);
            (
                project.output_path_inputs().map_err(|e| e.to_string())?,
                project.transcript_srt(id).map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        std::fs::write(&output_path, srt).map_err(|e| e.to_string())?;
        Ok(output_path)
    })
    .await
}

/// Write the cut's captions (its generated overlays, as the export draws them)
/// to `output_path` as SubRip or WebVTT, by `format` or the path's extension.
#[tauri::command]
pub(crate) async fn export_captions(
    state: State<'_, AppState>,
    output_path: String,
    format: Option<kerf_core::SubtitleFormat>,
) -> CmdResult<String> {
    require_user_output_path(&output_path)?;
    let path = std::path::PathBuf::from(&output_path);
    let format = match format {
        Some(f) => f,
        None => kerf_core::SubtitleFormat::from_path(&path).map_err(|e| e.to_string())?,
    };
    let shared = state.project.clone();
    blocking(move || {
        let (timeline, protected) = {
            let project = lock_user(&shared);
            (
                project.working_timeline().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        check_output(&path, &protected).map_err(|e| e.to_string())?;
        let text = Project::captions_document(&timeline, format).map_err(|e| e.to_string())?;
        Project::write_document(&path, &text).map_err(|e| e.to_string())?;
        Ok(output_path)
    })
    .await
}

/// Write the cut as OpenTimelineIO or a CMX 3600 EDL (`video_track`: which video
/// track an EDL holds, 0 = V1), returning the path and what the format dropped.
#[tauri::command]
pub(crate) async fn export_interchange(
    state: State<'_, AppState>,
    output_path: String,
    format: kerf_core::InterchangeFormat,
    video_track: Option<usize>,
) -> CmdResult<kerf_core::ExportedFile> {
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    blocking(move || {
        let (inputs, protected) = {
            let project = lock_user(&shared);
            (
                project.delivery_inputs().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        let (text, report) =
            Project::interchange_document(&inputs, format, video_track.unwrap_or(0)).map_err(|e| e.to_string())?;
        Project::write_document(std::path::Path::new(&output_path), &text).map_err(|e| e.to_string())?;
        Ok(kerf_core::ExportedFile {
            path: output_path,
            report,
        })
    })
    .await
}

/// Write the markers as a CSV (formula-safe) or as YouTube chapter text.
#[tauri::command]
pub(crate) async fn export_markers(
    state: State<'_, AppState>,
    output_path: String,
    format: kerf_core::MarkerListFormat,
) -> CmdResult<kerf_core::ExportedFile> {
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    blocking(move || {
        let (inputs, protected) = {
            let project = lock_user(&shared);
            (
                project.delivery_inputs().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        let (text, report) = Project::markers_document(&inputs, format).map_err(|e| e.to_string())?;
        Project::write_document(std::path::Path::new(&output_path), &text).map_err(|e| e.to_string())?;
        Ok(kerf_core::ExportedFile {
            path: output_path,
            report,
        })
    })
    .await
}

/// Re-time an asset's cached transcript to the speech energy and label its speakers, without
/// re-transcribing ([`run_refine`]). Stopped by `cancel_analysis`, like an analysis pass.
#[tauri::command]
pub(crate) async fn refine_transcript(app: AppHandle, state: State<'_, AppState>, asset_id: String) -> CmdResult<AssetAnalysis> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    let cancel = state.begin_analysis();
    blocking(move || run_refine(&app, &|| lock_user(&shared), id, &|| cancel())).await
}

#[tauri::command(async)]
pub(crate) fn remove_silence(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id, "asset_id")?;
    let project = state.project();
    project.remove_silence(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn snap_to_beats(state: State<'_, AppState>, track_id: Option<String>, tolerance: Option<f64>) -> CmdResult<Timeline> {
    let track = opt_id(track_id.as_deref(), "track_id")?;
    let project = state.project();
    project.snap_to_beats(track, tolerance).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Plan "fit music to length" for a music clip without changing anything. `target`
/// defaults to the picture's length from the clip's start.
#[tauri::command(async)]
pub(crate) fn plan_music_fit(
    state: State<'_, AppState>,
    clip_id: String,
    target: Option<f64>,
) -> CmdResult<kerf_core::model::MusicFit> {
    let clip = id(&clip_id, "clip_id")?;
    state.project().plan_music_fit(clip, target).map_err(|e| e.to_string())
}

/// What fitting music did: the refreshed timeline and the report.
#[derive(serde::Serialize)]
pub(crate) struct MusicFitted {
    timeline: Timeline,
    report: kerf_core::model::MusicFitReport,
}

/// Fit a music clip to `target` (default: the picture's length from its start) with
/// bar-aligned phrase splices, fading out an overrun when `fade_out`.
#[tauri::command(async)]
pub(crate) fn fit_music(
    state: State<'_, AppState>,
    clip_id: String,
    target: Option<f64>,
    fade_out: bool,
) -> CmdResult<MusicFitted> {
    let clip = id(&clip_id, "clip_id")?;
    let project = state.project();
    let report = project.fit_music(clip, target, fade_out).map_err(|e| e.to_string())?;
    let timeline = project.timeline().map_err(|e| e.to_string())?;
    Ok(MusicFitted { timeline, report })
}

/// Give an asset's sound its own clip on an audio track, for every use of the asset
/// on a video track that still plays it: each has its sound **detached** (the picture
/// muted, an audio clip with the same span linked to it) so nothing sounds twice — one
/// revision. A clip on a locked track is skipped and reported; with nothing to detach
/// it is an error (`add_asset_audio` is the explicit way to append the whole audio).
#[tauri::command(async)]
pub(crate) fn extract_audio(state: State<'_, AppState>, asset_id: String) -> CmdResult<AudioDetached> {
    let id = id(&asset_id, "asset_id")?;
    let project = state.project();
    let done = project.extract_audio(id).map_err(|e| e.to_string())?;
    Ok(AudioDetached {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        detached: done.detached.len(),
        skipped: done.skipped,
    })
}

/// Append an asset's whole audio to the first audio track as a clip of its own. It
/// never touches a picture clip, so an asset that also plays its own sound from a
/// video track is heard twice where they overlap — `extract_audio` is for that.
#[tauri::command(async)]
pub(crate) fn add_asset_audio(state: State<'_, AppState>, asset_id: String) -> CmdResult<Timeline> {
    let id = id(&asset_id, "asset_id")?;
    let project = state.project();
    project.add_asset_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn concatenate(state: State<'_, AppState>, asset_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&asset_ids, "asset_ids")?;
    let project = state.project();
    project.concatenate(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}
