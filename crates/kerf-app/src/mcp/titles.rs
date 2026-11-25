//! Titles: fonts, text overlays, SVG overlays and presets, title clips and their previews.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddOverlayParams {
    #[schemars(description = "The text to display")]
    text: String,
    #[schemars(description = "When the overlay appears, in timeline seconds")]
    start: f64,
    #[schemars(description = "When the overlay disappears, in timeline seconds")]
    end: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct UpdateOverlayParams {
    #[schemars(description = "UUID of the overlay")]
    overlay_id: String,
    #[schemars(description = "New text; omit to leave unchanged")]
    text: Option<String>,
    #[schemars(description = "New start time (seconds); omit to leave unchanged")]
    start: Option<f64>,
    #[schemars(description = "New end time (seconds); omit to leave unchanged")]
    end: Option<f64>,
    #[schemars(description = "Center X as a fraction of frame width (0–1); omit to leave unchanged")]
    pos_x: Option<f64>,
    #[schemars(description = "Center Y as a fraction of frame height (0–1, ~0.85 = lower third); omit to leave unchanged")]
    pos_y: Option<f64>,
    #[schemars(description = "Font height as a fraction of frame height (e.g. 0.06); omit to leave unchanged")]
    size: Option<f64>,
    #[schemars(description = "Text color (e.g. \"white\", \"#ffcc00\", \"yellow@0.9\"); omit to leave unchanged")]
    color: Option<String>,
    #[schemars(description = "Box color behind the text (e.g. \"black@0.5\"); empty string clears it; omit to leave unchanged")]
    bg: Option<String>,
    #[schemars(
        description = "System font family name (see list_fonts); empty string reverts to the default font; omit to leave unchanged"
    )]
    font: Option<String>,
    #[schemars(description = "Bold (thickened) text; omit to leave unchanged")]
    bold: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddSvgOverlayParams {
    #[schemars(
        description = "Your own SVG document: the text of an <svg ...>...</svg> file, animated with SMIL (see the tool description). Give this OR `preset`, not both"
    )]
    svg: Option<String>,
    #[schemars(description = "A ready-made animated title instead of `svg` (see list_svg_presets). Needs `params`")]
    preset: Option<SvgPresetKind>,
    #[schemars(
        description = "What fills the preset: `text` (required), and optionally `subtitle`, `color`, `accent` (any SVG colour like \"#ffffff\") and `font` (a system family from list_fonts). Only with `preset`"
    )]
    params: Option<SvgPresetParams>,
    #[schemars(description = "When the title appears, in timeline seconds")]
    start: f64,
    #[schemars(
        description = "When it disappears, in timeline seconds. Omit it to run for the SVG's own length when its animations end (clamped to 2–30 s), else 4 s"
    )]
    end: Option<f64>,
    #[schemars(description = "Left edge of the placement rectangle as a fraction of the frame width; default 0")]
    x: Option<f64>,
    #[schemars(description = "Top edge of the placement rectangle as a fraction of the frame height; default 0")]
    y: Option<f64>,
    #[schemars(description = "Width of the placement rectangle as a fraction of the frame width; default 1 (the whole frame)")]
    width: Option<f64>,
    #[schemars(description = "Height of the placement rectangle as a fraction of the frame height; default 1 (the whole frame)")]
    height: Option<f64>,
    #[schemars(description = "Opacity of the whole title, 0–1; default 1")]
    opacity: Option<f64>,
    #[schemars(description = "A label for the title (shown in the timeline); default is the preset's text")]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddTitleClipParams {
    #[schemars(
        description = "Your own SVG document, animated with SMIL as for add_svg_overlay (times are seconds from the clip's start). Drawn over the whole frame: give it a viewBox of the delivery frame's shape"
    )]
    svg: String,
    #[schemars(description = "Where the clip starts, in timeline seconds")]
    start: f64,
    #[schemars(
        description = "How long it runs, in seconds. Omit it to run as long as its animations (clamped to 2-30 s), else 4 s. Past the end of its animations it holds their last state"
    )]
    duration: Option<f64>,
    #[schemars(
        description = "UUID of the video track to put it on. Omit it for the lowest video track with nothing in the way over its span (an end card after the last shot lands beside it, ready for a transition), else a new video track on top"
    )]
    track_id: Option<String>,
    #[schemars(description = "A label for the title (shown on the clip)")]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct UpdateTitleClipParams {
    #[schemars(description = "UUID of the title clip (timeline_summary's `title_clips`)")]
    clip_id: String,
    #[schemars(description = "New label; omit to leave unchanged")]
    name: Option<String>,
    #[schemars(
        description = "Replace the artwork with your own SVG; a title from a template stops being one. Omit to leave unchanged"
    )]
    svg: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct UpdateSvgOverlayParams {
    #[schemars(description = "UUID of the SVG overlay (see timeline_summary)")]
    overlay_id: String,
    #[schemars(description = "New label; omit to leave unchanged")]
    name: Option<String>,
    #[schemars(
        description = "Replace the artwork with your own SVG (the title stops being a preset). Not together with `preset_params`; omit to leave unchanged"
    )]
    svg: Option<String>,
    #[schemars(
        description = "Re-fill a PRESET title: the full set of params (`text` required, optional `subtitle`, `color`, `accent`, `font`). Refused on a custom SVG title; omit to leave unchanged"
    )]
    preset_params: Option<SvgPresetParams>,
    #[schemars(
        description = "Switch a PRESET title to another kind (see list_svg_presets), keeping its id, name, timing, placement and place in the draw order; its params carry over unless `preset_params` is given too. Refused on a custom SVG title and together with `svg`; omit to leave unchanged"
    )]
    preset_kind: Option<SvgPresetKind>,
    #[schemars(description = "New start time (timeline seconds); omit to leave unchanged")]
    start: Option<f64>,
    #[schemars(description = "New end time (timeline seconds); omit to leave unchanged")]
    end: Option<f64>,
    #[schemars(description = "Left edge of the placement rectangle, fraction of the frame width; omit to leave unchanged")]
    x: Option<f64>,
    #[schemars(description = "Top edge of the placement rectangle, fraction of the frame height; omit to leave unchanged")]
    y: Option<f64>,
    #[schemars(description = "Width of the placement rectangle, fraction of the frame width; omit to leave unchanged")]
    width: Option<f64>,
    #[schemars(description = "Height of the placement rectangle, fraction of the frame height; omit to leave unchanged")]
    height: Option<f64>,
    #[schemars(description = "Opacity of the whole title, 0–1; omit to leave unchanged")]
    opacity: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ValidateSvgParams {
    #[schemars(description = "The SVG document to check: the text of an <svg ...>...</svg> file")]
    svg: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PreviewSvgOverlayParams {
    #[schemars(
        description = "UUID of an SVG title already on the timeline (see timeline_summary's `svg_overlays`, or a title clip from its `title_clips`). Give this OR `svg`, not both"
    )]
    overlay_id: Option<String>,
    #[schemars(
        description = "A draft SVG document to look at before adding it (the text of an <svg ...>...</svg> file). Give this OR `overlay_id`, not both"
    )]
    svg: Option<String>,
    #[schemars(
        description = "The moments to draw, in seconds AFTER THE TITLE APPEARS (0 = its first frame; for a title already on the timeline, a sliced one's offset is added for you). At most 36. Not together with `count`"
    )]
    times: Option<Vec<f64>>,
    #[schemars(
        description = "How many moments to draw, spread evenly from the first frame to the end of the title (default 6, at most 36). Not together with `times`"
    )]
    count: Option<u32>,
    #[schemars(description = "Width in pixels of each cell of the sheet (default 480, 64 to 960)")]
    max_width: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PreviewSvgPresetParams {
    #[schemars(description = "The ready-made title to draw (see list_svg_presets)")]
    preset: SvgPresetKind,
    #[schemars(description = "What fills it, as for add_svg_overlay (`text` is required)")]
    params: SvgPresetParams,
    #[schemars(description = "How long the title would run, in seconds (default 4): a preset times its animation to its length")]
    duration: Option<f64>,
    #[schemars(description = "The moments to draw, in seconds after the title appears. At most 36. Not together with `count`")]
    times: Option<Vec<f64>>,
    #[schemars(
        description = "How many moments to draw, spread evenly from the first frame to the end (default 6, at most 36). Not together with `times`"
    )]
    count: Option<u32>,
    #[schemars(description = "Width in pixels of each cell of the sheet (default 480, 64 to 960)")]
    max_width: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct OverlayIdParams {
    #[schemars(description = "UUID of the overlay")]
    overlay_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct OverlayKeyframesParams {
    #[schemars(description = "UUID of the overlay")]
    overlay_id: String,
    #[schemars(description = "Position/opacity keyframes (replaces the overlay's animation). Each: \
                       {\"time\":seconds_from_overlay_start,\"pos_x\":0.5,\"pos_y\":0.85,\"opacity\":1.0}. Pass [] to clear.")]
    keyframes: Vec<TextKeyframe>,
}

/// `svg` xor `preset` + `params`, as the one source `add_svg_overlay` takes.
pub(super) fn svg_source(
    svg: Option<String>,
    preset: Option<SvgPresetKind>,
    params: Option<SvgPresetParams>,
) -> Result<SvgSource, McpError> {
    let bad = |msg: &str| McpError::invalid_params(msg.to_string(), None);
    match (svg, preset, params) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(bad("pass either `svg` or `preset` with `params`, not both")),
        (Some(svg), None, None) => Ok(SvgSource::Custom(svg)),
        (None, Some(kind), Some(params)) => Ok(SvgSource::Preset(SvgPreset { kind, params })),
        (None, Some(_), None) => Err(bad("a preset needs `params` (at least `text`)")),
        (None, None, Some(_)) => Err(bad("`params` fill a `preset`; name one (see list_svg_presets)")),
        (None, None, None) => Err(bad("pass `svg` (your own document) or a `preset` with `params`")),
    }
}

/// What `validate_svg` answers. No source: the agent has it, and sanitising only removes things.
#[derive(Serialize)]
pub(super) struct SvgValidation {
    /// The size of the sanitised document, the part that is stored (limit 128 KiB).
    svg_bytes: usize,
    /// What the sanitiser or the SMIL reader would drop or could not honour.
    warnings: Vec<String>,
    /// When every animation has finished, in seconds after the title appears; null for a still
    /// or a document that loops forever.
    natural_duration: Option<f64>,
}

impl From<SvgCheck> for SvgValidation {
    fn from(c: SvgCheck) -> Self {
        Self {
            svg_bytes: c.svg.len(),
            warnings: c.warnings,
            natural_duration: c.natural_duration,
        }
    }
}

/// Which moments of a title a preview sheet shows.
#[derive(Debug, PartialEq)]
pub(super) enum SvgMoments {
    /// These seconds after the title appears.
    At(Vec<f64>),
    /// This many, evenly from the first frame to the end.
    Spread(usize),
}

/// `times` xor `count` as the moments of a preview sheet. A list the sheet cannot hold is refused
/// rather than cut short, because the caption would then not match what was asked for.
pub(super) fn svg_moments(times: Option<Vec<f64>>, count: Option<u32>) -> Result<SvgMoments, McpError> {
    let bad = |msg: String| McpError::invalid_params(msg, None);
    let most = kerf_core::svg::raster::MAX_SHEET_CELLS;
    match (times, count) {
        (Some(_), Some(_)) => Err(bad("pass either `times` or `count`, not both".to_string())),
        (Some(times), None) => {
            if times.is_empty() || times.len() > most {
                return Err(bad(format!("`times` takes 1 to {most} moments, got {}", times.len())));
            }
            if let Some(t) = times.iter().find(|t| !t.is_finite() || **t < 0.0) {
                return Err(bad(format!("`times` are seconds after the title appears, not {t}")));
            }
            Ok(SvgMoments::At(times))
        }
        (None, count) => Ok(SvgMoments::Spread((count.unwrap_or(6) as usize).clamp(1, most))),
    }
}

/// The moments of a title that runs `span` seconds, in seconds after it appears.
pub(super) fn svg_sheet_times(moments: &SvgMoments, span: f64) -> Vec<f64> {
    match moments {
        SvgMoments::At(times) => times.clone(),
        SvgMoments::Spread(1) => vec![span / 2.0],
        SvgMoments::Spread(n) => (0..*n).map(|i| span * i as f64 / (*n - 1) as f64).collect(),
    }
}

/// The text beside a preview sheet: what it shows and which moment each cell is.
pub(super) fn svg_sheet_caption(label: &str, times: &[f64], offset: f64, warnings: &[String]) -> String {
    let mut out = format!(
        "{label}: {} moment{} (row-major), cell -> seconds after it appears:",
        times.len(),
        if times.len() == 1 { "" } else { "s" }
    );
    for (i, t) in times.iter().enumerate() {
        out.push_str(&format!("\n  cell {}: {t:.2}s", i + 1));
    }
    if offset > 0.0 {
        out.push_str(&format!(
            "\nthis title was cut into, so its animation is {offset:.2}s further on than these times: the labels on the image are that clock"
        ));
    }
    if !warnings.is_empty() {
        out.push_str(&format!("\nwarnings: {}", warnings.join("; ")));
    }
    out
}

/// A title to look at: what the agent named, with how long it runs and how far in it starts.
pub(super) struct SvgPreviewTitle {
    pub(super) svg: String,
    /// How it is introduced in the caption.
    pub(super) label: String,
    /// Seconds it is on screen; `None` for a draft, which runs for the length `add_svg_overlay` would give it.
    pub(super) span: Option<f64>,
    /// Seconds of the document's own clock already elapsed when it appears.
    pub(super) offset: f64,
    /// A draft is read for the first time here, so what the sanitiser dropped is news.
    pub(super) report_warnings: bool,
}

/// Draw the contact sheet of `title` at `moments` into a `frame`-shaped cell `cell_w` wide:
/// the PNG and its caption. Reads the document (so it belongs on the blocking pool) and touches
/// no project.
pub(super) fn svg_preview(
    title: &SvgPreviewTitle,
    moments: &SvgMoments,
    cell_w: u32,
    frame: (u32, u32),
) -> Result<(Vec<u8>, String), McpError> {
    let doc = SvgDoc::parse(&title.svg).map_err(core_err)?;
    let span = title.span.unwrap_or_else(|| doc.default_span());
    let times = svg_sheet_times(moments, span);
    let clock: Vec<f64> = times.iter().map(|t| t + title.offset).collect();
    let png = kerf_core::svg::raster::contact_sheet(&doc, &clock, cell_w, frame).map_err(core_err)?;
    let warnings = if title.report_warnings { doc.warnings() } else { &[] };
    Ok((png, svg_sheet_caption(&title.label, &times, title.offset, warnings)))
}

#[tool_router(router = tool_router_titles, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "List the system font family names available for text overlays (pass one as `font` to add_overlay / update_overlay)"
    )]
    fn list_fonts(&self) -> Result<String, McpError> {
        json(&kerf_core::list_system_fonts())
    }

    #[tool(
        description = "Add a text overlay (title / lower-third / caption / watermark) drawn over the composited picture between start and end (timeline seconds). Returns the overlay; style or animate it with update_overlay / set_overlay_keyframes."
    )]
    fn add_overlay(&self, Parameters(p): Parameters<AddOverlayParams>) -> Result<String, McpError> {
        self.edit(|project| Self::add_overlay_op(project, p))
    }

    pub(super) fn add_overlay_op(project: &Project, p: AddOverlayParams) -> Result<String, McpError> {
        let out = project.add_overlay(p.text, p.start, p.end).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Update a text overlay's text, timing, position (pos_x / pos_y as 0–1 frame fractions), size (font height fraction), color, box background (bg, empty string clears), font (system font family from list_fonts, empty string clears) or bold. Omit a field to leave it unchanged."
    )]
    fn update_overlay(&self, Parameters(p): Parameters<UpdateOverlayParams>) -> Result<String, McpError> {
        self.edit(|project| Self::update_overlay_op(project, p))
    }

    pub(super) fn update_overlay_op(project: &Project, p: UpdateOverlayParams) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        let out = project
            .update_overlay(
                overlay_id, p.text, p.start, p.end, p.pos_x, p.pos_y, p.size, p.color, p.bg, p.font, p.bold,
            )
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(description = "Remove a text overlay")]
    fn remove_overlay(&self, Parameters(p): Parameters<OverlayIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::remove_overlay_op(project, p))
    }

    pub(super) fn remove_overlay_op(project: &Project, p: OverlayIdParams) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        project.remove_overlay(overlay_id).map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(
        description = "Add an animated SVG title (kinetic text, lower-third, logo sting, shape animation) drawn over the \
                       picture from start to end (timeline seconds), above the clips and below text overlays and \
                       captions. Pick a ready-made `preset` with `params` (list_svg_presets), or write your own `svg`. \
                       Your own SVG is animated with SMIL only: `animate`, `set`, `animateTransform` (translate / scale / \
                       rotate / skewX / skewY) and `animateColor`, with begin / dur / from / to / by / values / keyTimes / \
                       calcMode (linear, discrete, paced, spline + keySplines) / repeatCount / fill=\"freeze\", and \
                       `id.begin` / `id.end` chains. Times in the SVG are seconds RELATIVE TO THE OVERLAY'S START \
                       (begin=\"0s\" is the moment it appears, so the animation does not depend on where you place it). \
                       The document needs a viewBox (or width and height) and is scaled to fit, centred, inside the \
                       placement rectangle x / y / width / height (fractions of the frame; default the whole frame): for a \
                       full-frame title draw it in the delivery frame's own coordinates (viewBox=\"0 0 1920 1080\" for \
                       1080p; timeline_summary shows `delivery_format`), for a corner bug use a small rectangle. Dropped \
                       with a warning in the result: CSS animations and transitions, scripts and event handlers, \
                       foreignObject, links and external images or fonts (system fonts by name work; the only image that \
                       is kept is an <image> whose href is a data:image/png, data:image/jpeg, data:image/gif or \
                       data:image/webp URI), animateMotion, and `animate` on `transform` (use animateTransform). Limits: \
                       128 KiB of SVG, 50 SVG overlays. Returns the overlay (without its source) and `warnings`. Draft your \
                       own SVG with validate_svg and preview_svg_overlay (`svg`) first; once added, check it in the cut \
                       with preview_timeline at a few times between start and end."
    )]
    async fn add_svg_overlay(&self, Parameters(p): Parameters<AddSvgOverlayParams>) -> Result<String, McpError> {
        let source = svg_source(p.svg, p.preset, p.params)?;
        let layout = SvgLayout {
            name: p.name,
            x: p.x,
            y: p.y,
            width: p.width,
            height: p.height,
            opacity: p.opacity,
        };
        // Reading the document takes up to a second and a half: on the blocking pool, and before the
        // project lock, which then only places it.
        let checked = blocking(move || source.check().map_err(core_err)).await?;
        self.edit(|project| {
            let out = project
                .add_svg_overlay_parsed(checked, p.start, p.end, layout)
                .map_err(core_err)?;
            json(&SvgOverlayOutcome::from(out))
        })
    }

    #[tool(
        description = "Change an animated SVG title: its timing (start / end), placement (x / y / width / height as frame \
                       fractions), opacity or name, or its artwork. For a PRESET title pass `preset_params` (the full set; \
                       `text` required) to re-fill it and/or `preset_kind` to switch it to another preset (one edit; the \
                       title keeps its id, name, timing and place in the draw order); its animation re-times itself to \
                       the new length when you change start / end. To swap the artwork for your own, pass `svg` (the \
                       title then stops being a preset); `svg` is exclusive with `preset_params` and `preset_kind`, \
                       which a custom SVG title refuses. Omit a field to leave it unchanged. Returns the overlay \
                       (without its source) and `warnings`."
    )]
    async fn update_svg_overlay(&self, Parameters(p): Parameters<UpdateSvgOverlayParams>) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        let patch = SvgOverlayPatch {
            name: p.name,
            svg: p.svg,
            preset_params: p.preset_params,
            preset_kind: p.preset_kind,
            start: p.start,
            end: p.end,
            x: p.x,
            y: p.y,
            width: p.width,
            height: p.height,
            opacity: p.opacity,
        };
        // As in add_svg_overlay: read the artwork before the project lock is taken.
        let (patch, doc) = blocking(move || {
            let doc = patch.check().map_err(core_err)?;
            Ok((patch, doc))
        })
        .await?;
        self.edit(|project| {
            let out = project.update_svg_overlay_parsed(overlay_id, patch, doc).map_err(core_err)?;
            json(&SvgOverlayOutcome::from(out))
        })
    }

    #[tool(
        description = "Put your own animated SVG title on a VIDEO TRACK as a clip (a title clip), instead of floating it over \
                       the cut as add_svg_overlay does. As a clip it is part of the cut: it takes a transition_in \
                       (set_transition: dip_to_white, crossfade, slide ... from the shot before it on the same track), \
                       fades, transform and keyframes (an animated placement), colour and effects; trim, move, split, \
                       ripple and remove it with the ordinary clip tools; extending it past its animations holds their \
                       last state, so an end card can run as long as the cut needs. Its document is drawn over the \
                       whole frame (give it a viewBox of the delivery frame's shape; transparent where it draws \
                       nothing, so a title on a track above footage overlays it) and its SMIL times are seconds from \
                       the clip's start. It has no sound and plays at its own speed. Same SVG rules and limits as \
                       add_svg_overlay. Returns the title clip (its id, track and timing; no source) and `warnings`. \
                       To put a library template on a track, use apply_template with on_track=true."
    )]
    async fn add_title_clip(&self, Parameters(p): Parameters<AddTitleClipParams>) -> Result<String, McpError> {
        let placement = TitleClipPlacement {
            track_id: p.track_id.as_deref().map(|t| parse_arg("track_id", t)).transpose()?,
            name: p.name,
        };
        let svg = p.svg;
        // Read before the project lock, which then only places the clip.
        let doc = blocking(move || SvgDoc::parse(&svg).map_err(core_err)).await?;
        self.edit(|project| {
            let out = project
                .add_title_clip_parsed(doc, p.start, p.duration, placement)
                .map_err(core_err)?;
            json(&TitleClipOutcome::from(out))
        })
    }

    #[tool(
        description = "Rename a title clip or replace its artwork with your own `svg` (a title from a template then stops \
                       being one; to change a template's values use set_template_values). Its timing, track, \
                       transitions, fades and transform are the clip's: change them with the clip tools. Returns the \
                       title clip and `warnings`."
    )]
    async fn update_title_clip(&self, Parameters(p): Parameters<UpdateTitleClipParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let patch = TitleClipPatch {
            name: p.name,
            svg: p.svg,
        };
        let (patch, doc) = blocking(move || {
            let doc = patch.svg.as_deref().map(SvgDoc::parse).transpose().map_err(core_err)?;
            Ok((patch, doc))
        })
        .await?;
        self.edit(|project| {
            let out = project.update_title_clip_parsed(clip_id, patch, doc).map_err(core_err)?;
            json(&TitleClipOutcome::from(out))
        })
    }

    #[tool(description = "Remove an animated SVG title (its id is in timeline_summary's `svg_overlays`)")]
    fn remove_svg_overlay(&self, Parameters(p): Parameters<OverlayIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::remove_svg_overlay_op(project, p))
    }

    pub(super) fn remove_svg_overlay_op(project: &Project, p: OverlayIdParams) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        project.remove_svg_overlay(overlay_id).map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(
        description = "List the ready-made animated titles add_svg_overlay can make with `preset` + `params`, and what each \
                       takes (name, whether required, default). fade_up: the text rises and fades in, then fades out; \
                       typewriter: letters appear one by one; lower_third: an accent bar wipes in above a name (and \
                       subtitle) at the lower left; word_pop: one big word at a time pops in, centred — the social-video \
                       look. A preset is drawn for the delivery frame (set_delivery_format) and times its animation to \
                       the overlay's length, so it also follows a trim; the text is centred or left-anchored and long \
                       text shrinks to fit, never wraps."
    )]
    fn list_svg_presets(&self) -> Result<String, McpError> {
        json(&kerf_core::svg::preset_infos())
    }

    #[tool(
        description = "Check an SVG document you wrote for add_svg_overlay / update_svg_overlay without adding it: it is \
                       sanitised and its SMIL animations read, exactly as an add would. Returns `svg_bytes` (the size of \
                       the sanitised document; the limit is 128 KiB), `warnings` (what would be dropped or could not be \
                       honoured: CSS animations, scripts, external images, animateMotion, an unreadable value, ...) and \
                       `natural_duration` (seconds after the title appears when every animation has finished; null for \
                       a still or something that loops forever, which add_svg_overlay then gives 4 s). A document that \
                       cannot be used at all (not XML, no <svg> root, no viewBox or width and height, over a limit) is \
                       an error saying why. Nothing is changed and the source is not echoed back. To see how it \
                       looks, use preview_svg_overlay."
    )]
    async fn validate_svg(&self, Parameters(p): Parameters<ValidateSvgParams>) -> Result<String, McpError> {
        let check = blocking(move || kerf_core::check_svg(&p.svg).map_err(core_err)).await?;
        json(&SvgValidation::from(check))
    }

    #[tool(
        description = "See an animated SVG title without rendering the cut: a PNG contact sheet of it at several moments, \
                       each cell the title drawn over a dark grey frame of the delivery shape with its time written \
                       under it, plus a text index of cell -> seconds. Pass `svg` to look at a draft before you add \
                       it (its warnings come back too) or `overlay_id` for a title already on the timeline (reads the \
                       cut you are proposing, staged edits included). By default 6 moments spread from its first frame \
                       to its end (the last is usually the title gone or at rest); pass `times` (seconds after it \
                       appears) to look at exact moments, e.g. around a transition, or `count` for more or fewer. \
                       It shows the title alone, not over the footage: to judge placement and legibility against the \
                       picture, add it and use preview_timeline at a few times between its start and end."
    )]
    async fn preview_svg_overlay(&self, Parameters(p): Parameters<PreviewSvgOverlayParams>) -> Result<CallToolResult, McpError> {
        let overlay_id = p.overlay_id.as_deref().map(|s| parse_arg("overlay_id", s)).transpose()?;
        if overlay_id.is_some() == p.svg.is_some() {
            return Err(McpError::invalid_params(
                "pass either `overlay_id` (a title on the timeline) or `svg` (a draft), exactly one".to_string(),
                None,
            ));
        }
        let moments = svg_moments(p.times, p.count)?;
        let cell_w = p.max_width.unwrap_or(480).clamp(64, MAX_SVG_CELL_WIDTH);
        let (project, draft) = (self.project.clone(), p.svg);
        let (png, caption) = blocking(move || {
            // Resolve the title and the frame under the lock, draw with it released.
            let (title, frame) = {
                let project = lock_agent(&project);
                let timeline = project.working_timeline().map_err(core_err)?;
                let frame = timeline.preset_frame();
                let title = match (overlay_id, draft) {
                    (Some(id), _) if timeline.title_clip(id).is_some() => {
                        let (clip, t) = timeline.title_clip(id).expect("just found");
                        SvgPreviewTitle {
                            svg: t.svg.clone(),
                            label: format!("title clip \"{}\" ({id})", t.display_name()),
                            span: Some(clip.duration()),
                            offset: clip.source_in,
                            report_warnings: false,
                        }
                    }
                    (Some(id), _) => {
                        let o = timeline
                            .svg_overlay(id)
                            .ok_or_else(|| core_err(kerf_core::Error::SvgOverlayNotFound(id)))?;
                        SvgPreviewTitle {
                            svg: o.svg.clone(),
                            label: format!("SVG title \"{}\" ({id})", o.display_name()),
                            span: Some(o.end - o.start),
                            offset: o.offset,
                            report_warnings: false,
                        }
                    }
                    (None, svg) => SvgPreviewTitle {
                        svg: svg.unwrap_or_default(),
                        label: "draft SVG (not on the timeline)".to_string(),
                        span: None,
                        offset: 0.0,
                        report_warnings: true,
                    },
                };
                (title, frame)
            };
            svg_preview(&title, &moments, cell_w, frame)
        })
        .await?;
        Ok(png_result(caption, png))
    }

    #[tool(
        description = "See a ready-made animated title (list_svg_presets) filled with your `params` before adding it: a PNG \
                       contact sheet of it at several moments, drawn for the project's delivery frame, plus a text index \
                       of cell -> seconds. Like preview_svg_overlay but for a preset that is not on the timeline yet; \
                       nothing is added and nothing is staged. `duration` (default 4 s) is how long the title would run. \
                       Then add_svg_overlay with the same `preset` and `params`."
    )]
    async fn preview_svg_preset(&self, Parameters(p): Parameters<PreviewSvgPresetParams>) -> Result<CallToolResult, McpError> {
        kerf_core::svg::check_params(&p.params).map_err(core_err)?;
        let moments = svg_moments(p.times, p.count)?;
        let duration = p.duration.unwrap_or(4.0);
        if !duration.is_finite() || duration <= 0.0 || duration > kerf_core::MAX_SVG_SECONDS {
            return Err(McpError::invalid_params(
                format!(
                    "`duration` must be a positive number of seconds (at most {}), got {duration}",
                    kerf_core::MAX_SVG_SECONDS
                ),
                None,
            ));
        }
        let cell_w = p.max_width.unwrap_or(480).clamp(64, MAX_SVG_CELL_WIDTH);
        let (project, owner) = (self.project.clone(), self.owner());
        let (png, caption) = blocking(move || {
            // The frame the title would be drawn for, under the lock; the drawing with it released.
            let frame = lock_agent_as(&project, owner.as_deref())
                .working_timeline()
                .map_err(core_err)?
                .preset_frame();
            let title = SvgPreviewTitle {
                svg: kerf_core::render_preset(p.preset, &p.params, duration, frame),
                label: format!("preset {} (not on the timeline)", p.preset.as_str()),
                span: Some(duration),
                offset: 0.0,
                report_warnings: true,
            };
            svg_preview(&title, &moments, cell_w, frame)
        })
        .await?;
        Ok(png_result(caption, png))
    }

    #[tool(
        description = "Set or clear (empty list) a text overlay's position/opacity keyframes, to animate it over its lifetime (e.g. a title that slides in and fades out)"
    )]
    fn set_overlay_keyframes(&self, Parameters(p): Parameters<OverlayKeyframesParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_overlay_keyframes_op(project, p))
    }

    pub(super) fn set_overlay_keyframes_op(project: &Project, p: OverlayKeyframesParams) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        let out = project.set_overlay_keyframes(overlay_id, p.keyframes).map_err(core_err)?;
        json(&out)
    }
}
