//! Captions, caption files and markers.

use super::*;

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct CaptionParams {
    #[schemars(
        description = "Look: `lines` (default) holds a few words as a subtitle line; `word_punch` puts one large word on screen at a time, the social-video style. Everything below is an override on top of the style — omit them to get the whole look."
    )]
    style: Option<CaptionStyle>,
    #[schemars(description = "Most words on one caption line (lines: 4, word_punch: 1)")]
    max_words: Option<usize>,
    #[schemars(description = "Most characters on one caption line (default 28); the tighter of the two limits wins")]
    max_chars: Option<usize>,
    #[schemars(description = "Vertical position as a fraction of frame height, 0 = top (lines: 0.88, word_punch: 0.72)")]
    pos_y: Option<f64>,
    #[schemars(description = "Font height as a fraction of frame height (lines: 0.05, word_punch: 0.11)")]
    size: Option<f64>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct ImportCaptionsParams {
    #[schemars(
        description = "Absolute path of a .srt, .vtt, .ass or .ssa subtitle file (at most 5 MiB). Give this or `text`, not both."
    )]
    path: Option<String>,
    #[schemars(
        description = "The subtitle file's text itself, for captions you wrote rather than found on disk (SubRip is the easy one to write). Give this or `path`, not both."
    )]
    text: Option<String>,
    #[schemars(description = "`srt`, `vtt` or `ass`. Omit to detect it from the content, which is almost always right.")]
    format: Option<String>,
    #[schemars(
        description = "Which clock the cue times are on. `timeline` (default): the file is a subtitle track for the finished cut, so a cue at 0:12 shows at 0:12 of the timeline (cues after the cut ends are dropped). `source`: the file times one asset's own footage — a transcript, or subtitles for the uncut recording — and needs asset_id; each cue is projected through every clip of that asset, so it follows trims, reorders and speed changes and a cue whose footage was cut gets no caption. Giving asset_id alone implies `source`."
    )]
    base: Option<String>,
    #[schemars(description = "The asset whose footage the cue times belong to; only with base `source`")]
    asset_id: Option<String>,
    #[schemars(
        description = "Look: `lines` (default) holds a few words as a subtitle line; `word_punch` puts one large word on screen at a time. Long cues are split into readable lines either way. Everything below is an override on top of the style."
    )]
    style: Option<CaptionStyle>,
    #[schemars(
        description = "Most words on one caption line (lines: 4, word_punch: 1). Raise it, with max_chars, to keep each cue of a professionally timed file as one caption."
    )]
    max_words: Option<usize>,
    #[schemars(description = "Most characters on one caption line (default 28); the tighter of the two limits wins")]
    max_chars: Option<usize>,
    #[schemars(description = "Vertical position as a fraction of frame height, 0 = top (lines: 0.88, word_punch: 0.72)")]
    pos_y: Option<f64>,
    #[schemars(description = "Font height as a fraction of frame height (lines: 0.05, word_punch: 0.11)")]
    size: Option<f64>,
    #[schemars(
        description = "Seconds added to every cue before it is placed; negative moves them earlier. For a file whose clock does not start where the picture does — a broadcast .srt that begins at 01:00:00 wants -3600. Default 0."
    )]
    offset: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddMarkerParams {
    #[schemars(description = "Timeline time in seconds to mark")]
    time: f64,
    #[schemars(description = "Short label for the marker, e.g. 'best laugh' or 'chapter 2'")]
    name: String,
    #[schemars(description = "Optional CSS color for the ruler chip")]
    #[serde(default)]
    color: Option<String>,
    #[schemars(description = "How long the marked span runs from `time`, seconds; 0 is a point marker")]
    #[serde(default)]
    duration: Option<f64>,
    #[schemars(description = "A free-form note (a review comment, a chapter description); an empty string clears it")]
    #[serde(default)]
    comment: Option<String>,
    #[schemars(
        description = "`comment` (the default: a note on the cut) or `chapter` (starts a chapter: written into the export's chapter list and the YouTube chapter text)"
    )]
    #[serde(default)]
    kind: Option<kerf_core::MarkerKind>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct UpdateMarkerParams {
    #[schemars(description = "UUID of the marker to update")]
    marker_id: String,
    #[schemars(description = "New timeline time in seconds; omit to leave it put")]
    #[serde(default)]
    time: Option<f64>,
    #[schemars(description = "New label; omit to leave it alone")]
    #[serde(default)]
    name: Option<String>,
    #[schemars(description = "New CSS color, or an empty string to clear it")]
    #[serde(default)]
    color: Option<String>,
    #[schemars(description = "How long the marked span runs from `time`, seconds; 0 is a point marker")]
    #[serde(default)]
    duration: Option<f64>,
    #[schemars(description = "A free-form note (a review comment, a chapter description); an empty string clears it")]
    #[serde(default)]
    comment: Option<String>,
    #[schemars(
        description = "`comment` (the default: a note on the cut) or `chapter` (starts a chapter: written into the export's chapter list and the YouTube chapter text)"
    )]
    #[serde(default)]
    kind: Option<kerf_core::MarkerKind>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct MarkerIdParams {
    #[schemars(description = "UUID of the marker to remove")]
    marker_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportCaptionsParams {
    #[schemars(description = "Absolute local .srt or .vtt file path to write — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(description = "`srt` or `vtt`; omit to follow the path's extension")]
    #[serde(default)]
    format: Option<kerf_core::SubtitleFormat>,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportSrtParams {
    #[schemars(description = "UUID of the asset whose transcript to export")]
    asset_id: String,
    #[schemars(description = "Absolute local .srt file path to write — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[tool_router(router = tool_router_captions, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Drop a named marker on the timeline. Markers render nothing — they are shared \
                       vocabulary for places in the cut, so 'the laugh at 01:12' survives as something \
                       the user can see and jump to. Good for reporting findings from skim_asset."
    )]
    fn add_marker(&self, Parameters(p): Parameters<AddMarkerParams>) -> Result<String, McpError> {
        self.edit(|project| Self::add_marker_op(project, p))
    }

    pub(super) fn add_marker_op(project: &Project, p: AddMarkerParams) -> Result<String, McpError> {
        let patch = kerf_core::MarkerPatch {
            color: p.color,
            duration: p.duration,
            comment: p.comment,
            kind: p.kind,
            ..kerf_core::MarkerPatch::default()
        };
        let marker = project.add_marker_with(p.time, p.name, patch).map_err(core_err)?;
        json(&marker)
    }

    #[tool(
        description = "Move, rename, recolor or re-kind a marker, or set its duration or comment; omitted fields are left alone"
    )]
    fn update_marker(&self, Parameters(p): Parameters<UpdateMarkerParams>) -> Result<String, McpError> {
        self.edit(|project| Self::update_marker_op(project, p))
    }

    pub(super) fn update_marker_op(project: &Project, p: UpdateMarkerParams) -> Result<String, McpError> {
        let marker_id = parse_arg("marker_id", &p.marker_id)?;
        let patch = kerf_core::MarkerPatch {
            time: p.time,
            name: p.name,
            color: p.color,
            duration: p.duration,
            comment: p.comment,
            kind: p.kind,
        };
        let marker = project.edit_marker(marker_id, patch).map_err(core_err)?;
        json(&marker)
    }

    #[tool(description = "Remove a marker from the timeline")]
    fn remove_marker(&self, Parameters(p): Parameters<MarkerIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::remove_marker_op(project, p))
    }

    pub(super) fn remove_marker_op(project: &Project, p: MarkerIdParams) -> Result<String, McpError> {
        let marker_id = parse_arg("marker_id", &p.marker_id)?;
        project.remove_marker(marker_id).map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(
        description = "Caption the cut: project every clip's cached transcript (run analyze_asset first) through the current edit and write the result as text overlays, replacing any previously generated (or imported) set. Captions are placed in TIMELINE time, so they follow trims, reorders, speed changes and removed silences, and words that were cut out get no caption. Long sentences are split into readable lines. Pick the look with `style`: `lines` for subtitles, `word_punch` for one big word at a time (what social captions usually look like — prefer it for a vertical cut). Hand-made titles and lower-thirds are left alone. Returns how many captions were written, the first few in full and the ids of the first hundred (get_timeline_state's `captions` window pages through all of them)."
    )]
    fn generate_captions(&self, Parameters(p): Parameters<CaptionParams>) -> Result<String, McpError> {
        let opts = CaptionOptions {
            style: p.style.unwrap_or_default(),
            max_words: p.max_words,
            max_chars: p.max_chars,
            pos_y: p.pos_y,
            size: p.size,
        };
        self.edit(|project| {
            let out = project.generate_captions(opts).map_err(core_err)?;
            json(&reads::caption_result(&out))
        })
    }

    #[tool(
        description = "Caption the cut from a subtitle file: SubRip (.srt), WebVTT (.vtt) or ASS/SSA (.ass, .ssa), by path or as text you pass in. Cue times are taken as TIMELINE time by default (a subtitle track made for the finished cut); pass base=source with asset_id when the file instead times one asset's own footage (a transcript, subtitles for the uncut recording) and each cue is projected through that asset's clips like a transcript, following trims, reorders and speed changes. Long cues are split into readable lines, sized to fit the delivery frame, never two on screen at once — the same machinery as generate_captions, in the `style` you pick (`word_punch` for a vertical cut). Markup, styles and positions in the file are ignored; unreadable entries are skipped and counted. The captions REPLACE any previously generated or imported ones (a later generate_captions replaces these too — captions are one lane of text), and hand-made titles are left alone. Caption last, like generate_captions: a later edit does not move them. Pass offset (seconds, may be negative) when the file's clock does not start where the picture does. Returns a summary: cues read, placed, skipped_lines, dropped_outside (not on the cut at all: past its end or before its start, or footage that was cut out), dropped_short (on the cut but too brief to read), dropped_overlap (lost its slot to another cue), captions written and replaced. At most 10,000 cues / 100,000 words are read and 20,000 captions written per import — a refused import changes nothing."
    )]
    async fn import_captions(&self, Parameters(p): Parameters<ImportCaptionsParams>) -> Result<String, McpError> {
        let asset = p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?;
        let base = CaptionTimeBase::resolve(p.base.as_deref(), asset).map_err(core_err)?;
        let format = CaptionFormat::from_arg(p.format.as_deref()).map_err(core_err)?;
        let req = CaptionImportRequest {
            base,
            options: CaptionOptions {
                style: p.style.unwrap_or_default(),
                max_words: p.max_words,
                max_chars: p.max_chars,
                pos_y: p.pos_y,
                size: p.size,
            },
            offset: p.offset.unwrap_or(0.0),
        };
        let (project, owner) = (self.project.clone(), self.owner());
        let (path, text) = (p.path.as_deref().map(|path| self.path_arg(path)).transpose()?, p.text);
        let summary = blocking(move || {
            // Read, decode and parse with the project lock released — a few MB of
            // text is milliseconds, but never milliseconds the user's edits wait
            // on — and take it only to place the result.
            let text = caption_source(path.as_deref(), text)?;
            let file = kerf_core::parse_captions(&text, format).map_err(core_err)?;
            lock_agent_edit(&project, owner.as_deref())?
                .import_captions(&file, req)
                .map_err(core_err)
        })
        .await?;
        self.changed();
        json(&summary)
    }

    #[tool(
        description = "Remove the captions generate_captions or import_captions wrote, leaving hand-made titles and lower-thirds alone. Returns how many were removed."
    )]
    fn clear_captions(&self) -> Result<String, McpError> {
        self.edit(Self::clear_captions_op)
    }

    pub(super) fn clear_captions_op(project: &Project) -> Result<String, McpError> {
        let removed = project.clear_captions().map_err(core_err)?;
        Ok(format!("removed {removed} generated caption(s)"))
    }

    #[tool(
        description = "Write an asset's cached transcript to a SubRip (.srt) subtitle file (run analyze_asset first). \
                       Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_srt(&self, Parameters(p): Parameters<ExportSrtParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let output = self.path_arg(&p.output_path)?;
        crate::require_local_output_path(&output).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(output);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let srt = self.lock().transcript_srt(id).map_err(core_err)?;
        blocking(move || std::fs::write(&path, srt).map_err(|e| McpError::internal_error(e.to_string(), None))).await?;
        Ok(format!("wrote {}", p.output_path))
    }

    #[tool(
        description = "Write the cut's captions (the generated / imported caption overlays, timed as the export draws them) to a SubRip (.srt) or WebVTT (.vtt) file. Fails when the cut has no captions. Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_captions(&self, Parameters(p): Parameters<ExportCaptionsParams>) -> Result<String, McpError> {
        let output = self.path_arg(&p.output_path)?;
        crate::require_local_output_path(&output).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(output);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let format = match p.format {
            Some(f) => f,
            None => kerf_core::SubtitleFormat::from_path(&path).map_err(core_err)?,
        };
        let timeline = self.lock().working_timeline().map_err(core_err)?;
        blocking(move || {
            let text = Project::captions_document(&timeline, format).map_err(core_err)?;
            Project::write_document(&path, &text).map_err(core_err)
        })
        .await?;
        Ok(format!("wrote {}", p.output_path))
    }
}
