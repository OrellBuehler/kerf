//! The motion-template tools: the user's library of animated SVG titles that carry named controls
//! (a tagline, a brand colour, a logo), kept from a title on the cut and applied to it again.
//!
//! Applying a template and setting its values are edits of the cut, so they go through `edit()` and
//! stage like any agent edit; the zip, the fonts and the sanitiser all run on the blocking pool
//! **before** the lock is taken. The library calls (list, save, import, export, delete) are not
//! edits: they stage nothing, undo nothing, and tell the window the library changed instead. An agent
//! does not delete a template while the user reviews its edits — a template is something they made.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use kerf_core::{
    PreparedTemplate, PreparedValues, Project, SvgLayout, TemplateControl, TemplateKind, TemplateLibrary, TemplateSaveRequest,
    TemplateSaved, TemplateTarget, TitleClipPlacement, TitleEdited,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{schemars, tool, tool_router, ErrorData as McpError};
use uuid::Uuid;

use super::{
    blocking, check_outputs_now, core_err, json, lock_agent_as, parse_arg, refuse_overwrite, KerfMcp, SvgOverlayOutcome,
    TitleClipOutcome,
};

/// The event the window listens to for a change of the library made behind its back.
const LIBRARY_CHANGED: &str = "templates-changed";

/// The most image files one call may turn into values: a title is held to 128 KiB, so a handful of
/// pictures is all that can ever fit.
const MAX_IMAGE_FILES: usize = 8;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TemplateTargetArg {
    #[schemars(
        description = "The `id` attribute of an element in the title's SVG (exactly one element must carry it), e.g. \"tagline\" for <text id=\"tagline\">"
    )]
    element: String,
    #[schemars(
        description = "The attribute of that element the value is written to, spelled as in the SVG: `fill`, `stroke`, `stop-color`, `flood-color`, `lighting-color` or `color` for a colour; `font-family` for a font; `href` or `xlink:href` for an image; any attribute for a number. Leave it out for a `text` control"
    )]
    attribute: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TemplateControlArg {
    #[schemars(
        description = "A short identifier, unique in the template: a letter, then letters, digits, `-` or `_` (64 at most), e.g. \"tagline\". It is the key `values` use in apply_template / set_template_values"
    )]
    id: String,
    #[schemars(description = "What the user sees the control called, e.g. \"Tagline\" (80 characters at most)")]
    label: String,
    #[schemars(
        description = "What the control changes: `text` (an element's text content; aim it at a <text>, <tspan> or <textPath>), `color` (a colour attribute; values like #rgb, #rrggbb, #rrggbbaa, a colour name, none or currentColor, no rgb()), `number` (an attribute holding a plain unitless number, within `min` / `max`), `font` (a font-family: one family name) or `image` (an <image>'s href: a picture file given through `image_files`)"
    )]
    kind: TemplateKind,
    #[schemars(
        description = "Every place the one value is written (1 to 16): a colour can be set on three gradient stops at once. No two controls may write the same place"
    )]
    targets: Vec<TemplateTargetArg>,
    #[schemars(description = "The smallest value of a `number` control; only on a number")]
    min: Option<f64>,
    #[schemars(description = "The largest value of a `number` control; only on a number")]
    max: Option<f64>,
}

impl From<TemplateControlArg> for TemplateControl {
    fn from(c: TemplateControlArg) -> Self {
        Self {
            id: c.id,
            label: c.label,
            kind: c.kind,
            targets: c
                .targets
                .into_iter()
                .map(|t| TemplateTarget {
                    element: t.element,
                    attribute: t.attribute,
                })
                .collect(),
            min: c.min,
            max: c.max,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SaveTemplateParams {
    #[schemars(
        description = "UUID of the animated SVG title to keep as a template (see timeline_summary's `svg_overlays`, or a title clip from its `title_clips`; a title you staged counts)"
    )]
    overlay_id: String,
    #[schemars(description = "The template's name in the library (120 characters at most)")]
    name: String,
    #[schemars(description = "What it is for, shown beside the name (2000 characters at most); default none")]
    description: Option<String>,
    #[schemars(
        description = "The things that can be changed without touching the artwork, each aimed at element ids in the SVG. Leave it out or empty for a template that is used as it is"
    )]
    controls: Option<Vec<TemplateControlArg>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ApplyTemplateParams {
    #[schemars(description = "The template's `id` from list_templates")]
    template_id: String,
    #[schemars(
        description = "New values for some of the template's controls, by control id (a control left out keeps what the artwork holds). All values are strings: text as it should read; a colour like \"#e63946\"; a number like \"12.5\" with no unit; a font as one family name. An `image` control is set through `image_files` instead"
    )]
    values: Option<BTreeMap<String, String>>,
    #[schemars(
        description = "Pictures for `image` controls: control id -> absolute path of a PNG, JPEG, GIF or WebP file. A picture has to fit in the title (a little under 96 KB), so use a small one"
    )]
    image_files: Option<BTreeMap<String, String>>,
    #[schemars(description = "When the title appears, in timeline seconds")]
    start: f64,
    #[schemars(description = "When it disappears, in timeline seconds. Omit it to run for the template's own length")]
    end: Option<f64>,
    #[schemars(description = "Left edge of the placement rectangle as a fraction of the frame width; default 0")]
    x: Option<f64>,
    #[schemars(description = "Top edge of the placement rectangle as a fraction of the frame height; default 0")]
    y: Option<f64>,
    #[schemars(description = "Width of the placement rectangle as a fraction of the frame width; default 1 (the whole frame)")]
    width: Option<f64>,
    #[schemars(description = "Height of the placement rectangle as a fraction of the frame height; default 1 (the whole frame)")]
    height: Option<f64>,
    #[schemars(description = "Opacity of the whole title, 0-1; default 1")]
    opacity: Option<f64>,
    #[schemars(description = "A label for the title (shown in the timeline); default is the template's name")]
    name: Option<String>,
    #[schemars(
        description = "Put the title on a video track as a clip (a title clip) instead of floating it over the cut: it then takes a transition_in such as dip_to_white from the shot before it, fades, transform and keyframes, and is trimmed, moved and extended like any clip (past its animations it holds their last state). It is drawn over the whole frame, so x / y / width / height / opacity are refused with it: place it with set_transform. Default false"
    )]
    on_track: Option<bool>,
    #[schemars(
        description = "With on_track: UUID of the video track. Omit it for the lowest video track with nothing in the way over the title's span (an end card after the last shot lands beside it), else a new video track on top"
    )]
    track_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTitleTextBoxParams {
    #[schemars(
        description = "UUID of an animated title: a floating one (timeline_summary's `svg_overlays`) or a title clip (`title_clips`). Not a preset title, which is drawn from its parameters"
    )]
    title_id: String,
    #[schemars(
        description = "The `id` of the <text> element to give the box; get_timeline_state shows the title's source (a template's text controls name theirs)"
    )]
    element: String,
    #[schemars(
        description = "The box's width in the title's own units (its viewBox), from the text's x as text-anchor places it (start: rightward; middle: half each side). Omit or null to take the box away"
    )]
    width: Option<f64>,
    #[schemars(
        description = "\"wrap\" (default): the lines wrap at the width (and at a newline with white-space: pre-line). \"shrink\": the text stays on one line (one per newline) and is made smaller, never larger, until it fits — for a name or a tagline of any length"
    )]
    fit: Option<kerf_core::svg::TextFit>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTitleTextAnimationParams {
    #[schemars(
        description = "UUID of an animated title: a floating one (timeline_summary's `svg_overlays`) or a title clip (`title_clips`). Not a preset title, which is drawn from its parameters"
    )]
    title_id: String,
    #[schemars(
        description = "The `id` of the <text> element to animate; get_timeline_state shows the title's source (a template's text controls name theirs)"
    )]
    element: String,
    #[schemars(
        description = "The id of a ready-made animation (list_text_animation_presets): rise, fade_in, typewriter, pop, blur_in, spin_in, drop_in, wiggle. Exclusive with `animation`; omit both to take the animation away"
    )]
    preset: Option<String>,
    #[schemars(description = "Seconds the animation waits before it begins (0 to 30); changes a `preset`'s own")]
    delay: Option<f64>,
    #[schemars(description = "Seconds the animation takes (0.05 to 30); changes a `preset`'s own")]
    duration: Option<f64>,
    #[schemars(
        description = "The id of a ready-made curve (list_text_animation_curves): linear, ease_in, ease_out, ease_in_out, overshoot or spring. It shapes how the x, y, scale and rotation the animation changes travel to their place (an overshoot or a spring goes past it and comes back); opacity and blur stay as they are. Replaces the curves of those four that a `preset` or `animation` has; needs one of them, and an animation that moves, scales and turns nothing has nothing for a curve to shape"
    )]
    curve: Option<String>,
    #[schemars(
        description = "A whole animation of your own instead of a `preset`: `by` (character | word | line), `delay`, `duration`, `easing` (linear | ease_in | ease_out | ease_in_out), `range` {start, end, offset {from, to}, shape} (a window over the text, as fractions of it, slid from offset.from to offset.to; shape square | ramp_up | ramp_down | triangle | round | smooth: the amount each unit gets, 0 to 1), `wiggle` {rate, min, max, correlation, seed} (noise that multiplies the amount, or null), `properties` {x, y, scale, rotation, opacity, blur} (what a unit does at an amount of 1: x and y in em, rotation in degrees, blur in em; scale 1, opacity 1 and the rest 0 change nothing, and at least one must) and `curves` {x, y, scale, rotation, opacity, blur} (how each property travels there, each null for a straight line or {kind: \"spring\", bounce 0.05..0.8} or {kind: \"speed\", out_speed, out_influence, in_speed, in_influence}: After Effects' speed and influence, the speeds -5..5 times the average and the influences 0.01..1; see list_text_animation_curves). A curve is written for a unit settling into its place, as in every preset; a square window has nothing to shape. Every field has a default"
    )]
    animation: Option<kerf_core::svg::TextAnimation>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ConvertTitleTextParams {
    #[schemars(description = "UUID of an animated title: a floating one or a title clip, not a preset title")]
    title_id: String,
    #[schemars(
        description = "\"outlines\": draw the title's text as the paths of its glyphs (the same on every machine, whatever fonts it has; the only form in which a text box wraps or shrinks). \"text\": draw it with its fonts again (not for a template title, whose text is set as outlines whenever its values change)"
    )]
    to: kerf_core::TitleTextForm,
}

/// A title-text edit answered the way the other title edits are.
fn title_text_outcome(done: TitleEdited) -> Result<String, McpError> {
    match done {
        TitleEdited::Overlay(out) => json(&SvgOverlayOutcome::from(*out)),
        TitleEdited::Clip(out) => json(&TitleClipOutcome::from(*out)),
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTemplateValuesParams {
    #[schemars(
        description = "UUID of an animated title that came from a template: a floating one (its entry in timeline_summary's `svg_overlays` has a `template`) or a title clip (in `title_clips`)"
    )]
    overlay_id: String,
    #[schemars(
        description = "New values by control id; a control left out keeps its value. Strings: text as it should read; a colour like \"#e63946\"; a number like \"12.5\" with no unit; a font as one family name"
    )]
    values: Option<BTreeMap<String, String>>,
    #[schemars(
        description = "Pictures for `image` controls: control id -> absolute path of a PNG, JPEG, GIF or WebP file (a little under 96 KB)"
    )]
    image_files: Option<BTreeMap<String, String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ImportTemplateParams {
    #[schemars(description = "Absolute path of the .kerftemplate file to bring into the library")]
    path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ExportTemplateParams {
    #[schemars(description = "The template's `id` from list_templates")]
    template_id: String,
    #[schemars(description = "Absolute path of the .kerftemplate file to write")]
    output_path: String,
    #[schemars(description = "Replace the file if it already exists (default false: the call fails naming it)")]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct TemplateIdParams {
    #[schemars(description = "The template's `id` from list_templates")]
    template_id: String,
}

fn bad(message: impl Into<String>) -> McpError {
    McpError::invalid_params(message.into(), None)
}

/// The animation a `set_title_text_animation` call asks for: a preset or a whole animation (not both), `delay` and
/// `duration` overriding either's own, `curve` (a curve preset's id) put on the properties it moves, or none (the
/// animation taken away).
fn resolve_animation(
    preset: Option<&str>,
    animation: Option<kerf_core::svg::TextAnimation>,
    delay: Option<f64>,
    duration: Option<f64>,
    curve: Option<&str>,
) -> Result<Option<kerf_core::svg::TextAnimation>, McpError> {
    let timed = |mut a: kerf_core::svg::TextAnimation| {
        a.delay = delay.unwrap_or(a.delay);
        a.duration = duration.unwrap_or(a.duration);
        a
    };
    let resolved = match (preset, animation) {
        (Some(_), Some(_)) => return Err(bad("give a `preset` or an `animation`, not both")),
        (Some(id), None) => match kerf_core::svg::text_animation_preset(id) {
            Some(found) => Some(timed(found.animation)),
            None => {
                return Err(bad(format!(
                    "no text animation preset \u{201c}{id}\u{201d}; see list_text_animation_presets"
                )))
            }
        },
        (None, Some(a)) => Some(timed(a)),
        (None, None) => None,
    };
    let Some(id) = curve else { return Ok(resolved) };
    let Some(animation) = resolved else {
        return Err(bad(
            "a `curve` shapes the animation of a `preset` or an `animation`: give one",
        ));
    };
    let Some(found) = kerf_core::svg::curve_preset(id) else {
        return Err(bad(format!(
            "no text animation curve \u{201c}{id}\u{201d}; see list_text_animation_curves"
        )));
    };
    animation.with_motion_curve(found.curve).map(Some).map_err(core_err)
}

fn library() -> Result<TemplateLibrary, McpError> {
    TemplateLibrary::default_location().map_err(core_err)
}

/// A `.kerftemplate` path an agent may name: an extension check on top of the local-path rules the
/// caller has already applied.
fn template_path_arg(path: &str) -> Result<PathBuf, McpError> {
    let file = PathBuf::from(path);
    let is_template = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(kerf_core::template::EXTENSION));
    if !is_template {
        return Err(bad(format!(
            "{path} is not a template file: Kerf templates end in .{}",
            kerf_core::template::EXTENSION
        )));
    }
    Ok(file)
}

/// The control values a call carries: `values`, and each `image_files` entry read (bounded) as the
/// `data:` URL an image control takes. Reads files: for the blocking pool, before any lock.
pub(super) fn control_values(
    values: Option<BTreeMap<String, String>>,
    image_files: Option<BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, McpError> {
    let mut values = values.unwrap_or_default();
    let files = image_files.unwrap_or_default();
    if files.len() > MAX_IMAGE_FILES {
        return Err(bad(format!(
            "`image_files` names {} pictures; at most {MAX_IMAGE_FILES} fit in one title",
            files.len()
        )));
    }
    for (control, path) in files {
        if values.contains_key(&control) {
            return Err(bad(format!(
                "`{control}` is in both `values` and `image_files`; an image control takes a file"
            )));
        }
        crate::require_local_input_path(&path).map_err(bad)?;
        let url = crate::template_image_data_url(&path).map_err(core_err)?;
        values.insert(control, url);
    }
    Ok(values)
}

/// The template `template_id` with `values` spliced in and read. The slow part of an apply.
fn prepare_template(
    library: &TemplateLibrary,
    template_id: &str,
    values: Option<BTreeMap<String, String>>,
    image_files: Option<BTreeMap<String, String>>,
) -> Result<PreparedTemplate, McpError> {
    let values = control_values(values, image_files)?;
    library.get(template_id).and_then(|t| t.prepare(&values)).map_err(core_err)
}

/// The title `overlay_id` as the agent's working cut has it, with `values` spliced in and read. The
/// project is locked only to read the title.
fn prepare_values(
    project: &Mutex<Project>,
    owner: Option<&str>,
    overlay_id: Uuid,
    values: Option<BTreeMap<String, String>>,
    image_files: Option<BTreeMap<String, String>>,
) -> Result<PreparedValues, McpError> {
    // No values at all is the engine's to refuse (`TemplateEditInputs::prepare`): one answer for every surface.
    let values = control_values(values, image_files)?;
    let inputs = lock_agent_as(project, owner)
        .template_edit_inputs(overlay_id)
        .map_err(core_err)?;
    inputs.prepare(&values).map_err(core_err)
}

/// Keep the title `overlay_id` in `library`. The title is read under the lock; the fonts, the
/// sanitiser and the poster run with it released.
fn save_title(
    library: &TemplateLibrary,
    project: &Mutex<Project>,
    owner: Option<&str>,
    overlay_id: Uuid,
    request: TemplateSaveRequest,
) -> Result<TemplateSaved, McpError> {
    let source = lock_agent_as(project, owner).template_source(overlay_id).map_err(core_err)?;
    library.save(&source, request).map_err(core_err)
}

/// `import_template`'s work: the file looked at on disk, then read into the library. Blocking.
fn import_now(library: &TemplateLibrary, path: &std::path::Path) -> Result<kerf_core::ImportOutcome, McpError> {
    kerf_core::paths::validate_import_path(path).map_err(core_err)?;
    library.import(path).map_err(core_err)
}

/// `export_template`'s work: every look at the disk (does the file exist, where does the path really
/// lead) and the write. Blocking; the lock is taken only to read what the project protects.
fn export_now(
    project: &Mutex<Project>,
    owner: Option<&str>,
    library: &TemplateLibrary,
    template_id: &str,
    path: &std::path::Path,
    overwrite: bool,
) -> Result<PathBuf, McpError> {
    refuse_overwrite(path, overwrite)?;
    refuse_inside_library(library, path)?;
    // Not onto a file the project still needs, nor into Kerf's own settings and logs.
    check_outputs_now(project, owner, std::slice::from_ref(&path.to_path_buf()))?;
    library.export(template_id, path).map_err(core_err)
}

/// A template is the user's: while they review agent edits, only they remove one.
fn refuse_deleting_while_reviewing(review: bool) -> Result<(), McpError> {
    if review {
        return Err(bad(
            "the user keeps their template library while they review agent edits: ask them to remove the template in \
             Kerf's Titles panel, or leave it",
        ));
    }
    Ok(())
}

/// Refuse writing an export into the library itself, where it could replace another template.
fn refuse_inside_library(library: &TemplateLibrary, path: &std::path::Path) -> Result<(), McpError> {
    let protected = kerf_core::paths::Protected {
        files: Vec::new(),
        dirs: vec![(library.dir().to_path_buf(), "the template library".to_string())],
    };
    kerf_core::paths::check_output(path, &protected).map_err(core_err)
}

#[tool_router(router = tool_router_templates, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "List the motion templates in the user's library. A template is an animated SVG title kept with \
                       the fonts it needs and a few named controls (a tagline, a brand colour, a logo) that change it \
                       without touching the artwork. Returns `templates`, by name: `id`, `name`, `description`, \
                       `duration` (seconds it runs), `width` x `height` (its own shape in pixels), `controls` (each \
                       `id`, `label`, `kind` = text / color / number / font / image, `targets`, and `min` / `max` for a \
                       number), `fonts` (families it carries), `has_poster` and `size_bytes`; and `problems`, the files \
                       in the library that could not be read, with why. Put one on the cut with apply_template; make \
                       one from a title with save_template. An empty list is a library with nothing in it yet."
    )]
    async fn list_templates(&self) -> Result<String, McpError> {
        let list = blocking(|| library()?.list().map_err(core_err)).await?;
        json(&list)
    }

    #[tool(
        description = "Keep an animated SVG title that is on the cut (overlay_id; one you staged counts) in the user's \
                       template library under a new id, so it can be applied again in this or any other project. The \
                       SVG's elements need `id` attributes to be aimed at, so write them in when you author the title: \
                       author the SVG with ids, validate_svg, add_svg_overlay, check it with preview_svg_overlay, then \
                       save_template with `controls`. Each control names what can change: `kind` text (an element's \
                       text), color (fill / stroke / stop-color / ...), number (a plain number in an attribute, within \
                       min / max), font (a font-family) or image (an <image>'s href), and `targets` [{element: an id \
                       in the SVG, attribute}]. Its default is whatever its targets hold in the artwork now. A target \
                       that is missing, an attribute the kind cannot write, or two controls writing the same place is \
                       an error saying which. The fonts the artwork uses are embedded when their licence allows; \
                       `warnings` say which were left out. Returns `info` (the new template, with its `id`) and \
                       `warnings`. This writes the user's library, not the cut: it is not part of your staged proposal \
                       and cannot be undone (delete_template, which the user must do while they review your edits, \
                       removes it)."
    )]
    async fn save_template(&self, Parameters(p): Parameters<SaveTemplateParams>) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        let request = TemplateSaveRequest {
            name: p.name,
            description: p.description.unwrap_or_default(),
            controls: p
                .controls
                .unwrap_or_default()
                .into_iter()
                .map(TemplateControl::from)
                .collect(),
        };
        let (project, owner) = (self.project.clone(), self.owner());
        let saved = blocking(move || save_title(&library()?, &project, owner.as_deref(), overlay_id, request)).await?;
        self.notify(LIBRARY_CHANGED);
        json(&saved)
    }

    #[tool(
        description = "Put a library template (list_templates) on the cut as an animated title from start to end \
                       (timeline seconds), above the clips and below text overlays and captions, like add_svg_overlay. \
                       `values` sets some of its controls by control id (strings; a control left out keeps what the \
                       artwork holds); `image_files` gives an `image` control a picture file. The title carries its own \
                       copy of the artwork and fonts, so it keeps working if the template is deleted. Omit `end` to run \
                       for the template's own length. Placement x / y / width / height are fractions of the frame \
                       (default the whole frame): a template is drawn in its own shape and fitted inside it. Returns \
                       the overlay (its id, timing, and `template` with the controls and their current values; no \
                       source) and `warnings` (a font that is not installed, anything the sanitiser dropped). Change \
                       the values later with set_template_values; look at it with preview_svg_overlay (`overlay_id`) or \
                       preview_timeline. A staged edit like any other. With `on_track` true it is a title clip on a \
                       video track instead (see add_title_clip): it takes a transition_in such as dip_to_white from \
                       the shot before it, fades and keyframes, extends like any clip, and the answer is `title_clip`."
    )]
    async fn apply_template(&self, Parameters(p): Parameters<ApplyTemplateParams>) -> Result<String, McpError> {
        if p.on_track.unwrap_or(false) {
            return self.apply_template_on_track(p).await;
        }
        if p.track_id.is_some() {
            return Err(bad("`track_id` places a title clip: pass on_track=true with it"));
        }
        let layout = SvgLayout {
            name: p.name,
            x: p.x,
            y: p.y,
            width: p.width,
            height: p.height,
            opacity: p.opacity,
        };
        let (template_id, values, image_files) = (p.template_id, p.values, p.image_files);
        let prepared = blocking(move || prepare_template(&library()?, &template_id, values, image_files)).await?;
        self.edit(|project| {
            let out = project
                .apply_template_parsed(prepared, p.start, p.end, layout)
                .map_err(core_err)?;
            json(&SvgOverlayOutcome::from(out))
        })
    }

    #[tool(
        description = "Change control values of an animated title that came from a template (apply_template; its entry \
                       in timeline_summary's `svg_overlays` has a `template` listing the controls and their current \
                       values). Only the controls you name change; `values` are strings by control id, `image_files` \
                       gives an `image` control a picture file. One edit: the title keeps its id, timing and placement. \
                       A value the control cannot take (a colour that is not one, a number outside its min / max, a \
                       unit on a number) is an error saying which. Returns the overlay and `warnings`. A title whose \
                       artwork you replaced with update_svg_overlay `svg` is no longer a template. If the title changed \
                       while this ran, it says so: call again."
    )]
    async fn set_template_values(&self, Parameters(p): Parameters<SetTemplateValuesParams>) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        let (project, owner) = (self.project.clone(), self.owner());
        let prepared = blocking(move || prepare_values(&project, owner.as_deref(), overlay_id, p.values, p.image_files)).await?;
        self.edit(|project| title_text_outcome(project.set_title_values_parsed(overlay_id, prepared).map_err(core_err)?))
    }

    #[tool(
        description = "Give a <text> of an animated title (custom or a template, floating or a title clip; not a \
                       preset) a box: a `width` its lines wrap to, or with fit=\"shrink\" a width it stays on one line \
                       within, made smaller until it fits (a name of any length on a lower third). Omit `width` to take \
                       the box away. A box is drawn only when the text is glyph outlines, so the title's text is set as \
                       outlines (see convert_title_text); a text that cannot be one (a textPath, positioned tspans, ...) \
                       is an error saying why. update_svg_overlay / update_title_clip with new `svg` draw the text with \
                       its fonts again (a box written there is not drawn until convert_title_text). One edit; the title \
                       keeps its id, timing, placement and template values. \
                       If the title changed while this ran, it says so: call again."
    )]
    async fn set_title_text_box(&self, Parameters(p): Parameters<SetTitleTextBoxParams>) -> Result<String, McpError> {
        let title_id = parse_arg("title_id", &p.title_id)?;
        let text_box = kerf_core::svg::TextBox {
            width: p.width,
            fit: p.fit.unwrap_or_default(),
        };
        let (project, owner, element) = (self.project.clone(), self.owner(), p.element);
        let prepared = blocking(move || {
            let inputs = lock_agent_as(&project, owner.as_deref())
                .title_text_inputs(title_id)
                .map_err(core_err)?;
            inputs.text_box(&element, &text_box).map_err(core_err)
        })
        .await?;
        self.edit(|project| {
            title_text_outcome(
                project
                    .set_title_text_parsed(title_id, prepared, "Set text box")
                    .map_err(core_err)?,
            )
        })
    }

    #[tool(
        description = "List the text animations set_title_text_animation can start from by `preset` id, each with the \
                       complete `animation` it stands for (read them to write your own): rise (characters fade in \
                       one after another, rising into place), fade_in (words fade in), typewriter (characters appear \
                       one at a time), pop (characters grow from nothing), blur_in (characters come into focus), \
                       spin_in (characters turn and grow into place), drop_in (words drop in from above) and wiggle \
                       (every character jitters on its own, then settles)."
    )]
    fn list_text_animation_presets(&self) -> Result<String, McpError> {
        json(&kerf_core::svg::text_animation_presets())
    }

    #[tool(
        description = "List the curves set_title_text_animation can put on an animation by `curve` id, each with the \
                       curve it stands for (`null` for the straight line; read them to write your own `curves`): \
                       linear, ease_in, ease_out, ease_in_out (After Effects' speed and influence), overshoot (goes a \
                       little past its place, then comes back) and spring (swings past and back, settling). A curve is \
                       written for a unit settling into its place, as every preset does; a unit leaving it walks the \
                       same path backwards, and a `square` window has nothing to shape."
    )]
    fn list_text_animation_curves(&self) -> Result<String, McpError> {
        json(&kerf_core::svg::curve_presets())
    }

    #[tool(
        description = "Animate a <text> of an animated title (custom or a template, floating or a title clip; not a \
                       preset) letter by letter: pick a ready-made `preset` (list_text_animation_presets) or write an \
                       `animation` of your own, a window that slides over the characters, words or lines of the text \
                       (a typewriter, a stagger) and/or seeded noise (a wiggle), driving each unit's position, scale, \
                       rotation, opacity and blur in em, and a `curve` (list_text_animation_curves: ease_out, an \
                       overshoot, a spring) shapes how they travel. It runs from `delay` for `duration` seconds on \
                       the title's own clock; the title's default length follows it. Omit `preset` and `animation` to take the \
                       animation away. It is compiled to per-glyph SMIL when the text is glyph outlines, so the \
                       title's text is set as outlines (see convert_title_text); a text that cannot be one (a \
                       textPath, positioned tspans, ...) is an error saying why, and so is an animation with nothing \
                       to change or a number out of bounds. update_svg_overlay / update_title_clip with new `svg` draw \
                       the text with its fonts again (the animation does not play until convert_title_text). \
                       get_timeline_state shows it as data-kerf-animate on the <text>. One edit; the title keeps its \
                       id, timing, placement and template values. If the title changed while this ran, it says so: \
                       call again."
    )]
    async fn set_title_text_animation(&self, Parameters(p): Parameters<SetTitleTextAnimationParams>) -> Result<String, McpError> {
        let title_id = parse_arg("title_id", &p.title_id)?;
        let animation = resolve_animation(p.preset.as_deref(), p.animation, p.delay, p.duration, p.curve.as_deref())?;
        let (project, owner, element) = (self.project.clone(), self.owner(), p.element);
        let prepared = blocking(move || {
            let inputs = lock_agent_as(&project, owner.as_deref())
                .title_text_inputs(title_id)
                .map_err(core_err)?;
            inputs.text_animation(&element, animation.as_ref()).map_err(core_err)
        })
        .await?;
        self.edit(|project| {
            title_text_outcome(
                project
                    .set_title_text_parsed(title_id, prepared, "Set text animation")
                    .map_err(core_err)?,
            )
        })
    }

    #[tool(
        description = "Draw an animated title's text as glyph outlines (to=\"outlines\": the same on every machine \
                       whatever fonts it has, and what a text box needs) or with its fonts again (to=\"text\"). A \
                       template title's text is outlines already and cannot go back. Each text that cannot be an \
                       outline (a textPath, positioned tspans, right-to-left or vertical text, ...) stays text and \
                       `warnings` says why; a title none of whose text can is an error. get_timeline_state shows the \
                       source as text either way. One edit."
    )]
    async fn convert_title_text(&self, Parameters(p): Parameters<ConvertTitleTextParams>) -> Result<String, McpError> {
        let title_id = parse_arg("title_id", &p.title_id)?;
        let (project, owner, form) = (self.project.clone(), self.owner(), p.to);
        let prepared = blocking(move || {
            let inputs = lock_agent_as(&project, owner.as_deref())
                .title_text_inputs(title_id)
                .map_err(core_err)?;
            inputs.convert(form).map_err(core_err)
        })
        .await?;
        let label = match form {
            kerf_core::TitleTextForm::Outlines => "Draw text as outlines",
            kerf_core::TitleTextForm::Text => "Draw text with its fonts",
        };
        self.edit(|project| title_text_outcome(project.set_title_text_parsed(title_id, prepared, label).map_err(core_err)?))
    }

    #[tool(
        description = "Bring a .kerftemplate file (one the user exported, or another machine made) into the library. \
                       `path` is an absolute path to a file on this computer (a URL, a network share (UNC path), a \
                       relative path or a missing file is refused at once). The whole file is checked and re-read \
                       before anything is kept; one that fails is refused with why. It never overwrites: the same \
                       template is `already_present` (nothing written), another one with the same id is kept under a \
                       new id (`added_as_copy`). Returns `info`, `status` (added / already_present / added_as_copy) and \
                       `warnings`. Writes the user's library, not the cut: not part of your staged proposal."
    )]
    async fn import_template(&self, Parameters(p): Parameters<ImportTemplateParams>) -> Result<String, McpError> {
        crate::require_local_input_path(&p.path).map_err(bad)?;
        // The cheap check first: what is not named like a template is not looked at on disk at all.
        let path = template_path_arg(&p.path)?;
        let outcome = blocking(move || import_now(&library()?, &path)).await?;
        self.notify(LIBRARY_CHANGED);
        json(&outcome)
    }

    #[tool(
        description = "Write a library template to a .kerftemplate file, to share it or move it to another machine. \
                       `output_path` is an absolute path ending in .kerftemplate (a URL, a network share, a relative \
                       path or anywhere inside the template library or Kerf's own settings is refused); an existing \
                       file is refused unless `overwrite` is true. Returns the `path` written."
    )]
    async fn export_template(&self, Parameters(p): Parameters<ExportTemplateParams>) -> Result<String, McpError> {
        crate::require_local_output_path(&p.output_path).map_err(bad)?;
        let path = template_path_arg(&p.output_path)?;
        let (overwrite, template_id) = (p.overwrite.unwrap_or(false), p.template_id);
        let (project, owner) = (self.project.clone(), self.owner());
        // Every look at the disk (does the file exist, where does the path really lead) is on the blocking
        // pool, with the lock taken only for what the project needs.
        let written =
            blocking(move || export_now(&project, owner.as_deref(), &library()?, &template_id, &path, overwrite)).await?;
        json(&serde_json::json!({ "path": written.to_string_lossy() }))
    }

    #[tool(
        description = "Remove a template from the user's library for good (list_templates gives the `id`). Titles \
                       already made from it keep working: each carries its own artwork and fonts. It cannot be undone \
                       and it is not part of your proposal, so while the user reviews agent edits (the default) it is \
                       refused: ask them to remove it in Kerf."
    )]
    async fn delete_template(&self, Parameters(p): Parameters<TemplateIdParams>) -> Result<String, McpError> {
        refuse_deleting_while_reviewing(self.lock().agent_review())?;
        let template_id = p.template_id;
        blocking({
            let template_id = template_id.clone();
            move || library()?.delete(&template_id).map_err(core_err)
        })
        .await?;
        self.notify(LIBRARY_CHANGED);
        json(&serde_json::json!({ "deleted": template_id }))
    }
}

impl KerfMcp {
    /// `apply_template` with `on_track`: the template as a title clip.
    async fn apply_template_on_track(&self, p: ApplyTemplateParams) -> Result<String, McpError> {
        if [p.x, p.y, p.width, p.height, p.opacity].iter().any(Option::is_some) {
            return Err(bad(
                "a title on a track is drawn over the whole frame: leave out x / y / width / height / opacity and place it with set_transform",
            ));
        }
        let placement = TitleClipPlacement {
            track_id: p.track_id.as_deref().map(|t| parse_arg("track_id", t)).transpose()?,
            name: p.name,
        };
        let duration = p.end.map(|end| end - p.start);
        let (template_id, values, image_files) = (p.template_id, p.values, p.image_files);
        let prepared = blocking(move || prepare_template(&library()?, &template_id, values, image_files)).await?;
        self.edit(|project| {
            let out = project
                .apply_template_clip_parsed(prepared, p.start, duration, placement)
                .map_err(core_err)?;
            json(&TitleClipOutcome::from(out))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::{ImportStatus, SvgSource};
    use rmcp::model::ErrorCode;

    /// The process-wide embedded font set and the library directory are shared by every test in this
    /// crate that opens a project, so the ones here that apply templates take turns.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    const ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180">
<defs><linearGradient id="g"><stop id="bg-stop" offset="0" stop-color="#112233"/><stop offset="1" stop-color="#000000"/></linearGradient></defs>
<rect width="320" height="180" fill="url(#g)"/>
<text id="tagline" x="160" y="90" fill="#ffffff" text-anchor="middle" font-family="Geist">Hello</text>
<rect id="bar" x="0" y="170" width="10" height="10" fill="#e63946"><animate attributeName="width" from="0" to="320" dur="2s" fill="freeze"/></rect>
</svg>"##;

    fn controls() -> Vec<TemplateControl> {
        let control = |id: &str, kind: TemplateKind, element: &str, attribute: Option<&str>| TemplateControl {
            id: id.to_string(),
            label: id.to_string(),
            kind,
            targets: vec![TemplateTarget {
                element: element.to_string(),
                attribute: attribute.map(str::to_string),
            }],
            min: None,
            max: None,
        };
        vec![
            control("tagline", TemplateKind::Text, "tagline", None),
            control("brand", TemplateKind::Color, "bg-stop", Some("stop-color")),
        ]
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-mcp-templates-{tag}-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A project holding the title as the agent `session` staged it, and a library with it saved.
    fn saved(dir: &std::path::Path) -> (Mutex<Project>, TemplateLibrary, TemplateSaved, Uuid) {
        let shared = Mutex::new(Project::open_in_memory().unwrap());
        let library = TemplateLibrary::at(dir.join("library"));
        let checked = SvgSource::Custom(ART.to_string()).check().unwrap();
        let overlay = super::super::lock_agent_edit(&shared, Some("session-a"))
            .unwrap()
            .add_svg_overlay_parsed(checked, 1.0, Some(4.0), SvgLayout::default())
            .unwrap()
            .overlay;
        let request = TemplateSaveRequest {
            name: "End card".to_string(),
            description: "A brand end card".to_string(),
            controls: controls(),
        };
        let done = save_title(&library, &shared, Some("session-a"), overlay.id, request).unwrap();
        (shared, library, done, overlay.id)
    }

    fn values(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
        Some(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    fn invalid(e: McpError) -> String {
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS, "{}", e.message);
        e.message.to_string()
    }

    #[test]
    fn a_staged_title_is_kept_as_a_template_and_applied_again_with_other_values() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("roundtrip");
        let (shared, library, done, source_title) = saved(&dir);
        assert_eq!(done.info.name, "End card");
        assert_eq!(done.info.controls.len(), 2);
        assert_eq!(library.list().unwrap().templates.len(), 1);

        // Applied with new values it is one more staged title; the user's cut is untouched.
        let prepared = prepare_template(
            &library,
            &done.info.id,
            values(&[("tagline", "See you soon"), ("brand", "#3366ff")]),
            None,
        )
        .unwrap();
        let out = {
            let agent = super::super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            let out = agent
                .apply_template_parsed(prepared, 10.0, None, SvgLayout::default())
                .unwrap();
            assert!(agent.timeline().unwrap().svg_overlays.is_empty(), "staged, not applied");
            let mut working = agent.working_timeline().unwrap();
            assert_eq!(working.svg_overlays.len(), 2);
            // Stored as outlines; an agent's read shows the text it can set.
            assert!(working.svg_overlays.iter().any(|o| o.svg.contains("data-kerf-text")));
            super::super::restore_title_text(&mut working);
            assert!(working.svg_overlays.iter().all(|o| !o.svg.contains("data-kerf")));
            assert!(working.svg_overlays.iter().any(|o| o.svg.contains(">See you soon</text>")));
            out
        };
        // The size an agent is told is that of the source it reads.
        let text_form = kerf_core::svg::restore_text(&out.overlay.svg).unwrap();
        assert!(text_form.len() < out.overlay.svg.len());
        assert_eq!(
            serde_json::to_value(SvgOverlayOutcome::from(out.clone())).unwrap()["overlay"]["svg_bytes"],
            text_form.len()
        );
        let summary = serde_json::to_value(SvgOverlayOutcome::from(out)).unwrap();
        assert_eq!(summary["overlay"]["name"], "End card");
        assert_eq!(summary["overlay"]["start"], 10.0);
        assert_eq!(summary["overlay"]["template"]["template_id"], done.info.id);
        assert_eq!(summary["overlay"]["template"]["values"]["tagline"], "See you soon");
        assert_eq!(summary["overlay"]["template"]["values"]["brand"], "#3366ff");
        assert!(summary["overlay"].get("svg").is_none(), "{summary}");
        assert_ne!(summary["overlay"]["id"], source_title.to_string());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn values_are_set_on_the_working_cut_and_a_bad_one_names_the_control() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("values");
        let (shared, library, done, _) = saved(&dir);
        let prepared = prepare_template(&library, &done.info.id, None, None).unwrap();
        let title = {
            let agent = super::super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            agent
                .apply_template_parsed(prepared, 0.0, Some(3.0), SvgLayout::default())
                .unwrap()
                .overlay
                .id
        };

        let prepared = prepare_values(&shared, Some("session-a"), title, values(&[("tagline", "Bye")]), None).unwrap();
        let changed = {
            let agent = super::super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            agent.set_template_values_parsed(title, prepared).unwrap()
        };
        let template = changed.overlay.template.expect("still a template");
        assert_eq!(template.values["tagline"], "Bye");
        assert_eq!(template.values["brand"], "#112233", "a control left out keeps its value");

        let e = invalid(prepare_values(&shared, Some("session-a"), title, values(&[("brand", "rgb(1,2,3)")]), None).unwrap_err());
        assert!(e.contains("brand") || e.contains("colour") || e.contains("color"), "{e}");
        let e = invalid(prepare_values(&shared, Some("session-a"), title, values(&[("nope", "x")]), None).unwrap_err());
        assert!(e.contains("nope"), "{e}");
        let e = invalid(prepare_values(&shared, Some("session-a"), title, None, None).unwrap_err());
        assert!(e.contains("no values to set"), "{e}");
        // A title that did not come from a template has no values to set.
        let plain = {
            let agent = super::super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            let checked = SvgSource::Custom(ART.to_string()).check().unwrap();
            agent
                .add_svg_overlay_parsed(checked, 0.0, Some(1.0), SvgLayout::default())
                .unwrap()
                .overlay
                .id
        };
        let e = invalid(prepare_values(&shared, Some("session-a"), plain, values(&[("tagline", "x")]), None).unwrap_err());
        assert!(e.contains("not a template"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_template_on_a_track_is_a_staged_title_clip_whose_values_change_by_its_clip_id() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("on-track");
        let (shared, library, done, _) = saved(&dir);
        let prepared = prepare_template(&library, &done.info.id, values(&[("tagline", "End")]), None).unwrap();
        let out = {
            let agent = super::super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            let out = agent
                .apply_template_clip_parsed(prepared, 5.0, Some(3.0), TitleClipPlacement::default())
                .unwrap();
            assert_eq!(agent.timeline().unwrap().title_clips().count(), 0, "staged, not applied");
            out
        };
        let clip = out.clip.id;
        let summary = serde_json::to_value(TitleClipOutcome::from(out)).unwrap();
        assert_eq!(summary["title_clip"]["name"], "End card");
        assert_eq!(
            (summary["title_clip"]["start"].as_f64(), summary["title_clip"]["end"].as_f64()),
            (Some(5.0), Some(8.0))
        );
        assert_eq!(summary["title_clip"]["template"]["values"]["tagline"], "End");
        assert!(summary["title_clip"].get("svg").is_none(), "{summary}");

        let prepared = prepare_values(&shared, Some("session-a"), clip, values(&[("tagline", "Bye")]), None).unwrap();
        let agent = super::super::lock_agent_edit(&shared, Some("session-a")).unwrap();
        let TitleEdited::Clip(changed) = agent.set_title_values_parsed(clip, prepared).unwrap() else {
            panic!("the title clip was changed");
        };
        let title = changed.clip.title_clip().unwrap();
        assert_eq!(title.template.as_ref().unwrap().values["tagline"], "Bye");
        drop(agent);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_template_the_library_lacks_or_a_control_it_does_not_have_is_the_callers_to_fix() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("missing");
        let (_, library, done, _) = saved(&dir);
        let missing = invalid(prepare_template(&library, &Uuid::new_v4().to_string(), None, None).unwrap_err());
        assert!(missing.contains("no template"), "{missing}");
        let not_an_id = invalid(prepare_template(&library, "../../etc/passwd", None, None).unwrap_err());
        assert!(not_an_id.contains("UUID"), "{not_an_id}");
        let unknown = invalid(prepare_template(&library, &done.info.id, values(&[("nope", "x")]), None).unwrap_err());
        assert!(unknown.contains("nope"), "{unknown}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_a_title_that_is_not_there_or_a_control_aimed_at_nothing_is_refused() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("save");
        let (shared, library, _, title) = saved(&dir);
        let request = |controls: Vec<TemplateControl>| TemplateSaveRequest {
            name: "Again".to_string(),
            description: String::new(),
            controls,
        };
        let e = invalid(save_title(&library, &shared, Some("session-a"), Uuid::new_v4(), request(Vec::new())).unwrap_err());
        assert!(
            e.contains("not found") || e.contains("no animated title") || !e.is_empty(),
            "{e}"
        );
        let mut aimed_wrong = controls();
        aimed_wrong[0].targets[0].element = "nothing-here".to_string();
        let e = invalid(save_title(&library, &shared, Some("session-a"), title, request(aimed_wrong)).unwrap_err());
        assert!(e.contains("nothing-here"), "{e}");
        assert_eq!(library.list().unwrap().templates.len(), 1, "a refused save writes nothing");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_image_control_takes_a_file_the_caller_names_and_never_both_ways() {
        let dir = scratch("images");
        let png = dir.join("logo.png");
        {
            use base64::Engine as _;
            // A 4 x 4 red PNG.
            let bytes = base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAAEklEQVR4nGP4z8DwHxkzkC4AADxAH+HggXe0AAAAAElFTkSuQmCC")
                .unwrap();
            std::fs::write(&png, bytes).unwrap();
        }
        let image = png.to_string_lossy().into_owned();
        let merged = control_values(values(&[("tagline", "Hi")]), values(&[("logo", &image)])).unwrap();
        assert_eq!(merged["tagline"], "Hi");
        assert!(merged["logo"].starts_with("data:image/png;base64,"), "{}", merged["logo"]);

        let e = invalid(control_values(values(&[("logo", "data:x")]), values(&[("logo", &image)])).unwrap_err());
        assert!(e.contains("both"), "{e}");
        let e = invalid(control_values(None, values(&[("logo", "logo.png")])).unwrap_err());
        assert!(e.contains("absolute"), "{e}");
        let e = invalid(control_values(None, values(&[("logo", "\\\\host\\share\\logo.png")])).unwrap_err());
        assert!(e.contains("network"), "{e}");
        let text = dir.join("notes.txt");
        std::fs::write(&text, "not a picture").unwrap();
        let e = invalid(control_values(None, values(&[("logo", &text.to_string_lossy())])).unwrap_err());
        assert!(e.contains("PNG, JPEG, GIF or WebP"), "{e}");
        let many: BTreeMap<String, String> = (0..=MAX_IMAGE_FILES).map(|i| (format!("c{i}"), image.clone())).collect();
        let e = invalid(control_values(None, Some(many)).unwrap_err());
        assert!(e.contains("at most"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_template_file_round_trips_through_export_and_import_without_overwriting() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("share");
        let (_, library, done, _) = saved(&dir);
        let out = dir.join("card.kerftemplate");
        let written = library.export(&done.info.id, &out).unwrap();
        assert_eq!(written, out);

        // The same template is already there; into another library it is added.
        assert_eq!(library.import(&out).unwrap().status, ImportStatus::AlreadyPresent);
        let other = TemplateLibrary::at(dir.join("other"));
        let first = other.import(&out).unwrap();
        assert_eq!(first.status, ImportStatus::Added);
        assert_eq!(first.info.id, done.info.id);

        assert!(invalid(template_path_arg("/tmp/card.zip").unwrap_err()).contains(".kerftemplate"));
        assert!(template_path_arg("/tmp/CARD.KERFTEMPLATE").is_ok());
        let inside = library.dir().join("copy.kerftemplate");
        let e = invalid(refuse_inside_library(&library, &inside).unwrap_err());
        assert!(e.contains("template library"), "{e}");
        assert!(refuse_inside_library(&library, &dir.join("elsewhere.kerftemplate")).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_export_checks_the_disk_in_one_blocking_step_and_refuses_what_it_should() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("export");
        let (shared, library, done, _) = saved(&dir);
        let out = dir.join("card.kerftemplate");
        let export =
            |path: &std::path::Path, overwrite| export_now(&shared, Some("session-a"), &library, &done.info.id, path, overwrite);
        assert_eq!(export(&out, false).unwrap(), out);
        let e = invalid(export(&out, false).unwrap_err());
        assert!(e.contains("already exists") && e.contains("overwrite"), "{e}");
        assert_eq!(export(&out, true).unwrap(), out, "told to replace it");
        let e = invalid(export(&library.dir().join("copy.kerftemplate"), false).unwrap_err());
        assert!(e.contains("template library"), "{e}");
        // Even through a link into the library: where the path really leads is what is checked.
        #[cfg(unix)]
        {
            let link = dir.join("shortcut");
            std::os::unix::fs::symlink(library.dir(), &link).unwrap();
            let e = invalid(export(&link.join("copy.kerftemplate"), false).unwrap_err());
            assert!(e.contains("template library"), "{e}");
        }
        let e = invalid(
            export_now(
                &shared,
                None,
                &library,
                &Uuid::new_v4().to_string(),
                &dir.join("x.kerftemplate"),
                false,
            )
            .unwrap_err(),
        );
        assert!(e.contains("no template"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_import_is_named_like_a_template_before_the_disk_is_asked_about_it() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("import");
        // Not named like a template: refused by name, whatever is (or is not) at the path.
        let e = invalid(template_path_arg("/definitely/not/here/notes.txt").unwrap_err());
        assert!(e.contains("not a template file"), "{e}");
        let (_, library, done, _) = saved(&dir);
        let out = dir.join("card.kerftemplate");
        library.export(&done.info.id, &out).unwrap();
        let other = TemplateLibrary::at(dir.join("other"));
        assert_eq!(
            import_now(&other, &template_path_arg(out.to_str().unwrap()).unwrap())
                .unwrap()
                .status,
            ImportStatus::Added
        );
        // Named like one, but nothing there: the disk answers.
        let e = import_now(&other, &dir.join("nothing.kerftemplate")).unwrap_err();
        assert!(!e.message.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deleting_waits_for_the_user_while_they_review() {
        let e = invalid(refuse_deleting_while_reviewing(true).unwrap_err());
        assert!(e.contains("ask them"), "{e}");
        assert!(refuse_deleting_while_reviewing(false).is_ok());
    }

    /// The tools reach the library through `KERF_TEMPLATES_DIR`, read on every call.
    #[test]
    fn the_library_is_where_the_environment_says_and_starts_empty() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("env");
        let before = std::env::var_os("KERF_TEMPLATES_DIR");
        std::env::set_var("KERF_TEMPLATES_DIR", dir.join("mine"));
        let listed = library().and_then(|l| l.list().map_err(core_err));
        let located = library().map(|l| l.dir().to_path_buf());
        match before {
            Some(v) => std::env::set_var("KERF_TEMPLATES_DIR", v),
            None => std::env::remove_var("KERF_TEMPLATES_DIR"),
        }
        let listed = listed.unwrap();
        assert!(listed.templates.is_empty() && listed.problems.is_empty());
        assert_eq!(located.unwrap(), dir.join("mine"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_preset_or_an_animation_of_ones_own_with_its_timing_changed_or_none_at_all() {
        let rise = kerf_core::svg::text_animation_preset("rise").unwrap().animation;
        assert_eq!(
            resolve_animation(Some("rise"), None, None, None, None).unwrap(),
            Some(rise.clone())
        );
        let slower = resolve_animation(Some("rise"), None, Some(0.5), Some(3.0), None)
            .unwrap()
            .unwrap();
        assert_eq!((slower.delay, slower.duration), (0.5, 3.0));
        assert_eq!(slower.properties, rise.properties, "only the timing changed");
        let own = kerf_core::svg::TextAnimation { duration: 2.0, ..rise };
        assert_eq!(
            resolve_animation(None, Some(own.clone()), None, None, None).unwrap(),
            Some(own.clone())
        );
        assert_eq!(
            resolve_animation(None, Some(own), None, Some(4.0), None)
                .unwrap()
                .unwrap()
                .duration,
            4.0
        );
        assert_eq!(
            resolve_animation(None, None, Some(1.0), Some(1.0), None).unwrap(),
            None,
            "neither takes it away"
        );
        let both = resolve_animation(Some("rise"), Some(rise), None, None, None).unwrap_err();
        assert!(both.message.contains("not both"), "{}", both.message);
        let unknown = resolve_animation(Some("spin"), None, None, None, None).unwrap_err();
        assert!(
            unknown.message.contains("spin") && unknown.message.contains("list_text_animation_presets"),
            "{}",
            unknown.message
        );
    }

    #[test]
    fn a_curve_goes_on_what_the_animation_moves_or_is_refused_by_name() {
        let rise = kerf_core::svg::text_animation_preset("rise").unwrap().animation;
        let sprung = resolve_animation(Some("rise"), None, None, Some(2.0), Some("spring"))
            .unwrap()
            .unwrap();
        assert_eq!(sprung.duration, 2.0);
        assert_eq!(sprung.curves.y, kerf_core::svg::curve_preset("spring").unwrap().curve);
        assert_eq!((sprung.curves.x, sprung.curves.opacity), (None, None));
        assert_eq!(sprung.properties, rise.properties);
        // It also shapes an animation of one's own, and linear takes a curve off.
        let own = resolve_animation(None, Some(sprung), None, None, Some("linear"))
            .unwrap()
            .unwrap();
        assert_eq!(own.curves, Default::default());
        let unknown = resolve_animation(Some("rise"), None, None, None, Some("bouncy")).unwrap_err();
        assert!(
            unknown.message.contains("bouncy") && unknown.message.contains("list_text_animation_curves"),
            "{}",
            unknown.message
        );
        let alone = resolve_animation(None, None, None, None, Some("spring")).unwrap_err();
        assert!(alone.message.contains("preset"), "{}", alone.message);
        // Nothing to shape: a fade moves nothing.
        let fade = resolve_animation(Some("fade_in"), None, None, None, Some("spring")).unwrap_err();
        assert!(fade.message.contains("nothing to shape"), "{}", fade.message);
        assert!(resolve_animation(Some("fade_in"), None, None, None, Some("linear")).is_ok());
    }
}
