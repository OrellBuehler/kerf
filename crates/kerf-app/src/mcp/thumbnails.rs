//! The thumbnail tools: pictures designed beside the cut (a frame of it under a headline, a template, a
//! logo) that never appear in the video. Adding, changing and removing one are edits of the cut, so they
//! go through `edit()` and stage; the sanitiser, a template's zip and any picture file are read on the
//! blocking pool **before** the lock is taken. Drawing one reads the originals with the lock released.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kerf_core::{Project, SvgPresetParams, ThumbnailLayerSpec, ThumbnailResult, ThumbnailSpec, TimeRange};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{schemars, tool, tool_router, ErrorData as McpError};
use uuid::Uuid;

use super::templates::control_values;
use super::{blocking, core_err, json, lock_agent_as, parse_arg, png_result, refuse_overwrite, KerfMcp, MAX_PREVIEW_WIDTH};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct HeadlineArg {
    #[schemars(description = "The headline, set large and bold with a dark outline; a line per \\n, at most three")]
    text: String,
    #[schemars(description = "A smaller line under it, in the accent colour")]
    subtitle: Option<String>,
    #[schemars(description = "The headline's colour, an SVG colour like #ffffff (default white)")]
    color: Option<String>,
    #[schemars(description = "The subtitle's colour (default #ffb000)")]
    accent: Option<String>,
    #[schemars(description = "A font family (list_fonts); default the font Kerf ships")]
    font: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct LayerArg {
    #[schemars(
        description = "Keep this layer of the thumbnail (its id from the answer's `layers`): its artwork and placement stay unless given here; `values` changes a template layer's controls"
    )]
    layer_id: Option<String>,
    #[schemars(description = "A new headline layer (exactly one of text / template_id / svg / image_file for a new layer)")]
    text: Option<HeadlineArg>,
    #[schemars(description = "A new layer from a library template (list_templates), drawn still")]
    template_id: Option<String>,
    #[schemars(
        description = "Template control values by control id (strings, as apply_template takes them), for a new template layer or the kept one"
    )]
    values: Option<BTreeMap<String, String>>,
    #[schemars(
        description = "Pictures for the template's `image` controls: control id -> absolute path of a PNG, JPEG, GIF or WebP file"
    )]
    image_files: Option<BTreeMap<String, String>>,
    #[schemars(description = "A new layer of your own artwork: an SVG document with a viewBox (validate_svg checks one)")]
    svg: Option<String>,
    #[schemars(
        description = "A new picture layer (a logo): absolute path of a PNG, JPEG, GIF or WebP file, a little under 96 KB"
    )]
    image_file: Option<String>,
    #[schemars(description = "A label for the layer")]
    name: Option<String>,
    #[schemars(
        description = "The moment of an animated artwork to draw, in seconds of its own clock. Default: the moment it shows most (its settled state, before any animation out); a negative number goes back to that"
    )]
    at: Option<f64>,
    #[schemars(description = "Left edge of the rectangle the artwork is fitted into, as a fraction of the thumbnail's width")]
    x: Option<f64>,
    #[schemars(description = "Top edge, as a fraction of the thumbnail's height")]
    y: Option<f64>,
    #[schemars(
        description = "Width of the rectangle, a fraction of the thumbnail's width. A new headline defaults to x 0.05, y 0.62, width 0.9, height 0.3 (a band across the lower part); anything else to the whole picture"
    )]
    width: Option<f64>,
    #[schemars(description = "Height of the rectangle, a fraction of the thumbnail's height")]
    height: Option<f64>,
    #[schemars(description = "0-1, default 1")]
    opacity: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ThumbnailArgs {
    #[schemars(description = "A label, e.g. \"YouTube\"")]
    name: Option<String>,
    #[schemars(description = "The background frame, in timeline seconds (suggest_thumbnail_frames proposes good ones)")]
    time_secs: Option<f64>,
    #[schemars(
        description = "youtube (1280x720), vertical (1080x1920), square (1080x1080), 4:5 (1080x1350), 16:9 (1920x1080) or WxH (16-4096 a side). A new thumbnail defaults to the delivery frame's shape (youtube for a landscape one)"
    )]
    size: Option<String>,
    #[schemars(
        description = "Where the crop is centred when the frame has another shape than the thumbnail, as a fraction of the frame's width (0.5 the middle; the crop stays inside the frame)"
    )]
    focus_x: Option<f64>,
    #[schemars(description = "The same, as a fraction of the frame's height")]
    focus_y: Option<f64>,
    #[schemars(
        description = "Which of the video's own graphics the background keeps: none (the footage alone), titles (its titles and text, no captions; the default) or all"
    )]
    graphics: Option<kerf_core::VideoGraphics>,
    #[schemars(description = "The whole stack of layers, bottom first, replacing the one it has. Omit to keep the layers")]
    layers: Option<Vec<LayerArg>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddThumbnailParams {
    #[serde(flatten)]
    args: ThumbnailArgs,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct UpdateThumbnailParams {
    #[schemars(description = "UUID of the thumbnail (timeline_summary's `thumbnails`)")]
    thumbnail_id: String,
    #[serde(flatten)]
    args: ThumbnailArgs,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ThumbnailIdParams {
    #[schemars(description = "UUID of the thumbnail (timeline_summary's `thumbnails`)")]
    thumbnail_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct PreviewThumbnailParams {
    #[schemars(description = "UUID of the thumbnail (timeline_summary's `thumbnails`)")]
    thumbnail_id: String,
    #[schemars(description = "Width of the image in pixels (default 640, at most the thumbnail's own)")]
    max_width: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ExportThumbnailParams {
    #[schemars(description = "UUID of the thumbnail. Omit to write every thumbnail of the project")]
    thumbnail_id: Option<String>,
    #[schemars(
        description = "Absolute local output path ending .png, .jpg / .jpeg or .webp (the format). With several thumbnails, each is written beside it as <stem>-<thumbnail name>.<ext>"
    )]
    output_path: String,
    #[schemars(description = "Overwrite existing files (default false — the call fails naming the path)")]
    overwrite: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once; poll job_status for the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false"
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SuggestFramesParams {
    #[schemars(description = "How many frames to suggest (default 6, at most 12)")]
    count: Option<usize>,
    #[schemars(description = "Look only inside this span of the cut, {start, end} in timeline seconds. Omit for the whole cut")]
    range: Option<TimeRange>,
}

fn bad(message: impl Into<String>) -> McpError {
    McpError::invalid_params(message.into(), None)
}

/// The core spec a call's arguments stand for, every file it names read (bounded) into a `data:` URL.
/// Reads files: for the blocking pool, before any lock.
fn spec_of(args: ThumbnailArgs) -> Result<ThumbnailSpec, McpError> {
    let layers = match args.layers {
        None => None,
        Some(layers) => Some(
            layers
                .into_iter()
                .map(|l| {
                    let image = match l.image_file {
                        Some(path) => {
                            crate::require_local_input_path(&path).map_err(bad)?;
                            Some(crate::template_image_data_url(&path).map_err(core_err)?)
                        }
                        None => None,
                    };
                    let values = (l.values.is_some() || l.image_files.is_some())
                        .then(|| control_values(l.values, l.image_files))
                        .transpose()?;
                    Ok(ThumbnailLayerSpec {
                        layer_id: l.layer_id,
                        text: l.text.map(|h| SvgPresetParams {
                            subtitle: h.subtitle,
                            color: h.color.unwrap_or_else(|| "#ffffff".into()),
                            accent: h.accent.unwrap_or_else(|| "#ffb000".into()),
                            font: h.font,
                            ..SvgPresetParams::new(h.text)
                        }),
                        template_id: l.template_id,
                        values,
                        svg: l.svg,
                        image,
                        name: l.name,
                        at: l.at,
                        x: l.x,
                        y: l.y,
                        width: l.width,
                        height: l.height,
                        opacity: l.opacity,
                    })
                })
                .collect::<Result<Vec<_>, McpError>>()?,
        ),
    };
    Ok(ThumbnailSpec {
        name: args.name,
        time: args.time_secs,
        size: args.size,
        focus_x: args.focus_x,
        focus_y: args.focus_y,
        graphics: args.graphics,
        layers,
    })
}

/// A thumbnail as an agent reads it: everything but its layers' SVG sources.
pub(super) fn thumbnail_view(t: &kerf_core::Thumbnail) -> serde_json::Value {
    let mut layers = Vec::new();
    for l in &t.layers {
        let mut v = serde_json::json!({
            "id": l.id,
            "name": l.name,
            "kind": l.kind(),
            "x": l.x,
            "y": l.y,
            "width": l.width,
            "height": l.height,
            "opacity": l.opacity,
        });
        if let Some(at) = l.at {
            v["at"] = at.into();
        }
        if let Some(text) = &l.text {
            v["text"] = serde_json::to_value(text).unwrap_or_default();
        }
        if let Some(tpl) = &l.template {
            v["template"] = serde_json::json!({
                "template_id": tpl.template_id,
                "values": tpl.values,
                "controls": tpl.controls.iter().map(|c| &c.id).collect::<Vec<_>>(),
            });
        }
        layers.push(v);
    }
    serde_json::json!({
        "id": t.id,
        "name": t.name,
        "time_secs": t.time,
        "size": format!("{}x{}", t.width, t.height),
        "focus_x": t.focus_x,
        "focus_y": t.focus_y,
        "graphics": t.graphics,
        "layers": layers,
    })
}

fn outcome(done: ThumbnailResult) -> Result<String, McpError> {
    json(&serde_json::json!({
        "thumbnail": thumbnail_view(&done.thumbnail),
        "warnings": done.warnings,
    }))
}

/// Build the thumbnail (`id`, or a new one) off the lock: read what it starts from under it, then
/// prepare with it released.
fn prepare(
    project: &Mutex<Project>,
    owner: Option<&str>,
    id: Option<Uuid>,
    args: ThumbnailArgs,
) -> Result<kerf_core::project::PreparedThumbnail, McpError> {
    let uses_templates = args
        .layers
        .as_ref()
        .is_some_and(|ls| ls.iter().any(|l| l.template_id.is_some()));
    let spec = spec_of(args)?;
    let library = uses_templates
        .then(kerf_core::TemplateLibrary::default_location)
        .transpose()
        .map_err(core_err)?;
    let base = lock_agent_as(project, owner).thumbnail_base(id).map_err(core_err)?;
    kerf_core::project::prepare_thumbnail(base, &spec, library.as_ref()).map_err(core_err)
}

/// Where each thumbnail of an export goes: `output` for one, else beside it, named by the thumbnail.
fn output_paths(output: &Path, names: &[String]) -> Vec<PathBuf> {
    if names.len() == 1 {
        return vec![output.to_path_buf()];
    }
    let stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("thumbnail");
    let ext = output.extension().and_then(|s| s.to_str()).unwrap_or("png");
    let mut used = std::collections::HashSet::new();
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let slug: String = name
                .chars()
                .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            let mut slug = if slug.is_empty() { format!("{}", i + 1) } else { slug };
            if !used.insert(slug.clone()) {
                slug = format!("{slug}-{}", i + 1);
            }
            output.with_file_name(format!("{stem}-{slug}.{ext}"))
        })
        .collect()
}

#[tool_router(router = tool_router_thumbnails, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Design a thumbnail: a picture that fronts the video but never appears in it — a frame of the cut \
                       (`time_secs`), cropped to `size` around a focus point, under layers of its own: a headline \
                       (`text`), a library template with control values, your own SVG, or a picture file. Unlike \
                       export_cover (a plain frame of the video, its own titles and all) its graphics are its own. \
                       Returns the thumbnail (its id and layers, no SVG sources) and `warnings`. A staged edit; look \
                       at it with preview_thumbnail, write it with export_thumbnail."
    )]
    async fn add_thumbnail(&self, Parameters(p): Parameters<AddThumbnailParams>) -> Result<String, McpError> {
        let (project, owner) = (self.project.clone(), self.owner());
        let prepared = blocking(move || prepare(&project, owner.as_deref(), None, p.args)).await?;
        self.edit(|project| outcome(project.apply_thumbnail_parsed(prepared).map_err(core_err)?))
    }

    #[tool(
        description = "Change a thumbnail (add_thumbnail): only what you give changes. `layers` is the whole new \
                       stack, bottom first; keep a layer by its `layer_id` (with new placement, `at` or template \
                       `values` if you like) and add new ones beside it. One staged edit. If the thumbnail changed \
                       while this ran, it says so: call again."
    )]
    async fn update_thumbnail(&self, Parameters(p): Parameters<UpdateThumbnailParams>) -> Result<String, McpError> {
        let id = parse_arg("thumbnail_id", &p.thumbnail_id)?;
        let (project, owner) = (self.project.clone(), self.owner());
        let prepared = blocking(move || prepare(&project, owner.as_deref(), Some(id), p.args)).await?;
        self.edit(|project| outcome(project.apply_thumbnail_parsed(prepared).map_err(core_err)?))
    }

    #[tool(description = "Remove a thumbnail. A staged edit.")]
    fn remove_thumbnail(&self, Parameters(p): Parameters<ThumbnailIdParams>) -> Result<String, McpError> {
        let id = parse_arg("thumbnail_id", &p.thumbnail_id)?;
        self.edit(|project| {
            project.remove_thumbnail(id).map_err(core_err)?;
            json(&serde_json::json!({ "removed": id }))
        })
    }

    #[tool(
        description = "Look at a thumbnail as it will be written: its frame (proxies for speed), cropped, with its \
                       layers. Returns the image, `max_width` wide (default 640)."
    )]
    async fn preview_thumbnail(
        &self,
        Parameters(p): Parameters<PreviewThumbnailParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_arg("thumbnail_id", &p.thumbnail_id)?;
        let (project, owner) = (self.project.clone(), self.owner());
        let width = p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH);
        let cancel = context.ct.clone();
        let (png, caption) = blocking(move || {
            let (t, timeline, assets) = lock_agent_as(&project, owner.as_deref())
                .thumbnail_render_inputs(id)
                .map_err(core_err)?;
            let assets = Project::preview_assets_of(assets);
            let picture =
                kerf_core::thumbnail::render(&t, &timeline, &assets, width, &|| cancel.is_cancelled()).map_err(core_err)?;
            let png = kerf_core::svg::raster::encode_png(&picture).map_err(core_err)?;
            Ok((
                png,
                format!("thumbnail “{}” ({}x{}) @ {:.2}s", t.name, t.width, t.height, t.time),
            ))
        })
        .await?;
        Ok(png_result(caption, png))
    }

    #[tool(
        description = "Write a thumbnail (or, with no thumbnail_id, every one) to an image file at its full size, the \
                       frame read from the original media: .png, .jpg or .webp by the extension. Refuses to overwrite \
                       an existing file unless overwrite is true. Returns `outputs`; with background=true it runs as a \
                       background job (job_status)."
    )]
    async fn export_thumbnail(
        &self,
        Parameters(p): Parameters<ExportThumbnailParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        crate::require_local_output_path(&p.output_path).map_err(bad)?;
        let output = PathBuf::from(&p.output_path);
        kerf_core::thumbnail::ThumbnailFormat::from_path(&output).map_err(core_err)?;
        let id = p.thumbnail_id.as_deref().map(|s| parse_arg("thumbnail_id", s)).transpose()?;
        let thumbs: Vec<(Uuid, String)> = {
            let timeline = self.lock().working_timeline().map_err(core_err)?;
            match id {
                Some(id) => {
                    let t = timeline.thumbnail(id).ok_or_else(|| bad(format!("no thumbnail {id}")))?;
                    vec![(t.id, t.name.clone())]
                }
                None => timeline.thumbnails.iter().map(|t| (t.id, t.name.clone())).collect(),
            }
        };
        if thumbs.is_empty() {
            return Err(bad("the project has no thumbnails yet: make one with add_thumbnail"));
        }
        let names: Vec<String> = thumbs.iter().map(|(_, n)| n.clone()).collect();
        let paths = output_paths(&output, &names);
        for path in &paths {
            refuse_overwrite(path, p.overwrite.unwrap_or(false))?;
        }
        self.check_outputs(&paths).await?;
        let (project, owner) = (self.project.clone(), self.owner());
        self.long_tool("export_thumbnail", p.background, &context, move |run| async move {
            let written = blocking(move || {
                let mut out = Vec::new();
                for (i, ((id, name), path)) in thumbs.iter().zip(&paths).enumerate() {
                    run.progress(Some(i as f64 / thumbs.len() as f64), format!("drawing “{name}”"));
                    let (t, timeline, assets) = lock_agent_as(&project, owner.as_deref())
                        .thumbnail_render_inputs(*id)
                        .map_err(core_err)?;
                    let written =
                        kerf_core::thumbnail::write(&t, &timeline, &assets, path, &|| run.cancelled()).map_err(core_err)?;
                    out.push(serde_json::json!({
                        "thumbnail_id": id,
                        "output": written.to_string_lossy(),
                        "size": format!("{}x{}", t.width, t.height),
                    }));
                }
                run.progress(Some(1.0), "written");
                Ok(out)
            })
            .await?;
            json(&serde_json::json!({ "outputs": written }))
        })
        .await
    }

    #[tool(
        description = "Suggest frames worth a thumbnail: the middle of each shot (split at analyzed scene changes, else \
                       spread evenly), away from fades and transitions, each measured on a small decode without the \
                       video's graphics — black, faded and flat frames dropped, the rest scored on sharpness, exposure \
                       and contrast. Returns `frames` (best first: `time`, `score` and its parts) and a contact sheet \
                       of them, numbered, to choose from by eye."
    )]
    async fn suggest_thumbnail_frames(
        &self,
        Parameters(p): Parameters<SuggestFramesParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let range = match p.range {
            Some(r) if !(r.start.is_finite() && r.end.is_finite() && r.end > r.start) => {
                return Err(bad("range needs a start before its end"));
            }
            r => r.map(|r| (r.start.max(0.0), r.end)),
        };
        let count = p.count.unwrap_or(6);
        let (project, owner) = (self.project.clone(), self.owner());
        let cancel = context.ct.clone();
        let found = blocking(move || {
            let (timeline, assets, scenes) = lock_agent_as(&project, owner.as_deref())
                .frame_suggestion_inputs()
                .map_err(core_err)?;
            let assets = Project::preview_assets_of(assets);
            kerf_core::thumbnail::suggest_frames(&timeline, &assets, &scenes, count, range, &|| cancel.is_cancelled())
                .map_err(core_err)
        })
        .await?;
        let caption = serde_json::to_string(&serde_json::json!({
            "frames": found.frames,
            "looked_at": found.looked_at,
        }))
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(png_result(caption, found.sheet_png))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn several_thumbnails_are_written_beside_the_path_by_name() {
        let out = Path::new("/out/cover.png");
        assert_eq!(output_paths(out, &["Main".into()]), vec![PathBuf::from("/out/cover.png")]);
        assert_eq!(
            output_paths(out, &["YouTube 16:9".into(), "".into(), "youtube 16 9".into()]),
            vec![
                PathBuf::from("/out/cover-youtube-16-9.png"),
                PathBuf::from("/out/cover-2.png"),
                PathBuf::from("/out/cover-youtube-16-9-3.png"),
            ]
        );
    }

    #[test]
    fn a_layer_argument_becomes_the_core_spec() {
        let args: ThumbnailArgs = serde_json::from_value(serde_json::json!({
            "time_secs": 2.0,
            "size": "youtube",
            "layers": [{"text": {"text": "Hi", "color": "#ff0000"}, "y": 0.1}, {"layer_id": "x", "at": -1}],
        }))
        .unwrap();
        let spec = spec_of(args).unwrap();
        let layers = spec.layers.unwrap();
        let text = layers[0].text.as_ref().unwrap();
        assert_eq!(
            (text.text.as_str(), text.color.as_str(), text.accent.as_str()),
            ("Hi", "#ff0000", "#ffb000")
        );
        assert_eq!(layers[0].y, Some(0.1));
        assert_eq!(layers[1].layer_id.as_deref(), Some("x"));
        assert_eq!(spec.time, Some(2.0));
        let unknown = serde_json::from_value::<ThumbnailArgs>(serde_json::json!({"layers": [{"txt": "x"}]}));
        assert!(unknown.is_err());
        let relative =
            serde_json::from_value::<ThumbnailArgs>(serde_json::json!({"layers": [{"image_file": "logo.png"}]})).unwrap();
        assert!(spec_of(relative).is_err());
    }
}
