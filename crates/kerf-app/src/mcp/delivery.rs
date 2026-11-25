//! Delivery: the delivery format, export and its variants, cover, platform check, review cut and interchange files.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PlatformCheckParams {
    #[schemars(
        description = "Judge only this span of the cut, {start, end} in timeline seconds — what an export with the same `range` would write. Omit for the whole cut"
    )]
    range: Option<TimeRange>,
    #[schemars(
        description = "Judge the cut at this frame width in pixels instead of the project's own — the size an export with `options.resolution` would write. Give it with `height`. Omit both to use the project's frame"
    )]
    width: Option<u32>,
    #[schemars(description = "Frame height in pixels; see `width`")]
    height: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ReviewCutParams {
    #[schemars(
        description = "Report only what falls in this span of the cut, {start, end} in timeline seconds. Omit for the whole cut"
    )]
    range: Option<TimeRange>,
    #[schemars(
        description = "Run only these checks: flash_frame, mid_word_cut, hard_audio_cut, jump_cut, stale_captions, black_gap. Omit for all"
    )]
    checks: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetDeliveryFormatParams {
    #[schemars(description = "Delivery frame width in pixels, e.g. 1080. Omit (with height) to clear the \
                              format and go back to following the footage's shape.")]
    pub(super) width: Option<u32>,
    #[schemars(description = "Delivery frame height in pixels, e.g. 1920 for a 9:16 vertical cut")]
    pub(super) height: Option<u32>,
    #[schemars(description = "How footage of a different shape meets the frame: \"cover\" (the default) \
                              fills and crops, \"contain\" scales the whole picture in and letterboxes, \
                              \"blur\" scales the whole picture in over a blurred, enlarged copy of itself \
                              that fills the bars")]
    pub(super) fit: Option<Fit>,
    #[schemars(
        description = "The frame rate the cut is delivered at, 1 to 240 (24, 25, 30, 50, 60, or the \
                              NTSC rates 23.976 / 29.97 / 59.94). An export, the preview and the stills use it \
                              whenever `options.fps` names none. Omit to keep the rate the project already \
                              has (none: exports follow the FIRST clip's rate, which in footage of mixed \
                              rates is whichever comes first); 0 clears it back to following the footage."
    )]
    pub(super) fps: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportInterchangeParams {
    #[schemars(description = "Absolute local output path (.otio or .edl) — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(description = "`otio` (OpenTimelineIO, every track) or `edl` (CMX 3600, one video track)")]
    format: kerf_core::InterchangeFormat,
    #[schemars(description = "EDL only: which video track to write, 0 = the first (V1). Default 0.")]
    #[serde(default)]
    video_track: Option<usize>,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportMarkersParams {
    #[schemars(description = "Absolute local output path (.csv or .txt) — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(
        description = "`csv` (every marker as a spreadsheet row, formula-safe) or `youtube` (chapter lines for a video description, from the chapter markers — every marker when none is a chapter)"
    )]
    format: kerf_core::MarkerListFormat,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportParams {
    #[schemars(
        description = "Absolute local output file path for the rendered result — no URLs or ffmpeg protocol \
                       sinks. Its extension should match the chosen container."
    )]
    output_path: String,
    #[schemars(description = "Optional encode settings. Omit for the safe default (H.264 + AAC MP4). \
                       Key fields: container (mp4/mov/mkv/webm/gif/mp3/m4a/wav/flac); video_codec \
                       (libx264/libx265/libvpx-vp9/libsvtav1/prores_ks/gif, plus GPU encoders \
                       h264_nvenc/hevc_nvenc/av1_nvenc/h264_qsv/hevc_qsv/av1_qsv/h264_videotoolbox/\
                       hevc_videotoolbox/h264_amf/hevc_amf/av1_amf/h264_vaapi/hevc_vaapi/av1_vaapi — \
                       far faster, crf still applies; use only the ones export_capabilities lists — \
                       a VAAPI codec on a machine nothing has probed yet waits up to 10 s for the probe \
                       to find the render node); \
                       audio_codec (aac/libmp3lame/libopus/flac/alac/pcm_s16le/pcm_s24le); \
                       rate_control (crf/bitrate/two_pass/lossless); crf; video_bitrate (\"8M\"); \
                       preset; hwaccel (\"auto\"/\"cuda\"/\"vaapi\"/\"videotoolbox\"/\"qsv\" — GPU \
                       decode, composes with any encoder); resolution ([w,h]); fps (default: the delivery \
                       frame's rate from set_delivery_format, else the first clip's); audio_bitrate \
                       (\"192k\"); include_audio; faststart; range ({start,end} timeline seconds — \
                       render only that span); embed_captions (mux the cut's captions in as a soft \
                       subtitle stream — mp4/mov/mkv/webm only, not shown by default); \
                       skip_burned_captions (leave the captions out of the picture); caption_sidecars \
                       ([\"srt\",\"vtt\"] — caption files beside the output, written once the render \
                       completes; an existing one is refused unless overwrite is true). Chapter markers \
                       become the file's chapters.")]
    #[serde(default)]
    options: Option<ExportOptions>,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportVariantsParams {
    #[schemars(
        description = "Absolute local base output path (no URLs or ffmpeg protocol sinks); each delivery lands \
                              beside it with its shape in the name — `/renders/cut.mp4` at 9:16 and 1:1 writes \
                              `cut-9x16.mp4` and `cut-1x1.mp4`."
    )]
    output_path: String,
    #[schemars(
        description = "The delivery frames to render, one file each: \"9:16\" (1080x1920, Reels / Shorts / \
                              TikTok), \"1:1\" (1080x1080, feed), \"4:5\" (1080x1350, Instagram portrait), \"16:9\" \
                              (1920x1080, YouTube), or an explicit \"WxH\"."
    )]
    formats: Vec<String>,
    #[schemars(
        description = "Frame each shot for every shape first (default true): samples where each clip's \
                              content sits and keeps a crop per shape on the clip, so a 9:16 and a 1:1 delivery \
                              each keep the subject rather than the middle. The project frame's own crop is never \
                              touched. false renders whatever crop each clip already has."
    )]
    smart_crop: Option<bool>,
    #[schemars(description = "Encode settings shared by every variant — the same fields as `export`. Its \
                              resolution and fit are replaced per variant by the delivery frame.")]
    #[serde(default)]
    options: Option<ExportOptions>,
    #[schemars(
        description = "Overwrite any variant file that already exists (default false — the call fails naming the \
                              first existing path so you can pick a new base path or opt in)"
    )]
    overwrite: Option<bool>,
    #[schemars(
        description = "Render every file in one pass (default true): the sources are decoded and the mix is built \
                              once, then split into each frame — about one export's decode for all of them. Falls back \
                              to one file after another where one pass cannot (two-pass or hardware encoders, an \
                              audio-only container). A cancelled or failed single pass removes every file. false \
                              renders the files one after another, keeping the finished ones if a later one is cancelled."
    )]
    single_pass: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CoverParams {
    #[schemars(description = "Timeline position to capture (seconds)")]
    time_secs: f64,
    #[schemars(
        description = "Absolute local output image path (no URLs or ffmpeg protocol sinks). A .png \
                       extension writes PNG, anything else JPEG."
    )]
    output_path: String,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

/// The delivery frame a `set_delivery_format` call asks for, given the one the project has now (`None`
/// clears it). A width and height set the frame; its rate is `fps` (0 clears it), else the one the
/// project already delivers at, so reshaping does not lose it. `fps` alone changes the rate of the
/// frame the project has (and refuses when there is none); nothing at all clears the frame.
pub(super) fn next_delivery(p: &SetDeliveryFormatParams, current: Option<Delivery>) -> kerf_core::Result<Option<Delivery>> {
    let rate = match p.fps {
        Some(0.0) => None,
        Some(fps) => Some(fps),
        None => current.and_then(|d| d.rate()),
    };
    match (p.width, p.height, p.fps, current) {
        (Some(w), Some(h), _, _) => Delivery::new(w, h, p.fit.unwrap_or(Fit::Cover)).with_fps(rate).map(Some),
        (None, None, Some(_), Some(frame)) => Delivery {
            fit: p.fit.unwrap_or(frame.fit),
            ..frame
        }
        .with_fps(rate)
        .map(Some),
        (None, None, Some(0.0), None) => Ok(None),
        (None, None, Some(_), None) => Err(kerf_core::Error::InvalidArgument(
            "no delivery frame is set, so there is no frame to give a rate: pass width and height too".to_string(),
        )),
        _ => Ok(None),
    }
}

/// `frame` (what an export wrote: its size and rate, and where the rate came from) put on a result
/// object. A render with no picture (an audio file) has none and leaves the object as it is.
pub(super) fn with_frame(mut result: serde_json::Value, frame: Option<kerf_core::ExportedFrame>) -> serde_json::Value {
    if let (Some(frame), Some(map)) = (frame, result.as_object_mut()) {
        map.insert("width".into(), frame.width.into());
        map.insert("height".into(), frame.height.into());
        map.insert("fps".into(), frame.fps.into());
        map.insert("fps_source".into(), serde_json::json!(frame.fps_source));
    }
    result
}

#[tool_router(router = tool_router_delivery, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Set the frame this project is cut for — the shape and frame rate of the delivered video, e.g. \
                       1080x1920 at 30 fps for a vertical Reel or 1080x1080 for a square feed post. Everything \
                       that renders a picture (preview_timeline, get_frame and the export) then uses \
                       it, so what you look at while cutting is what ships. With \"cover\" (the default) \
                       a 16:9 shot is filled and cropped to the frame, which means the crop decides what \
                       survives: check preview_timeline and use set_transform's position to slide the \
                       important part of a shot back into frame. With \"blur\" the whole shot shows, the \
                       bars filled with a blurred copy of it (a clip moved or scaled by its transform gets \
                       plain bars). `fps` is the frame rate every export defaults to: without one an export \
                       takes the rate of the FIRST clip with a picture, which in footage of mixed rates \
                       (100, 24 and 29.97 fps) is whichever happens to come first — set it whenever the \
                       footage is mixed or the user has a rate in mind; an explicit `options.fps` on the \
                       export still wins. Omit `fps` to keep the project's current rate, pass 0 to clear it; \
                       `fps` alone (no width and height) changes just the rate of the frame the project has. \
                       Omit all three to clear the whole frame, rate included. Returns the frame and \
                       the rate an export will use (`export_fps`, and where it comes from)."
    )]
    fn set_delivery_format(&self, Parameters(p): Parameters<SetDeliveryFormatParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_delivery_format_op(project, p))
    }

    pub(super) fn set_delivery_format_op(project: &Project, p: SetDeliveryFormatParams) -> Result<String, McpError> {
        let current = project.working_timeline().map_err(core_err)?.format;
        let format = next_delivery(&p, current).map_err(core_err)?;
        let timeline = project.set_delivery_format(format).map_err(core_err)?;
        let assets = project.list_assets().map_err(core_err)?;
        json(&serde_json::json!({
            "format": timeline.format,
            "export_fps": export_fps_of(&timeline, &assets),
            "timeline": reads::timeline_brief(&timeline),
        }))
    }

    #[tool(
        description = "Hand the cut to another editor: write it as OpenTimelineIO (.otio: every track, clips, gaps, transitions, speed as LinearTimeWarp, markers, media references to the original files, each clip's full Kerf state in metadata.kerf) or a CMX 3600 EDL (.edl: one video track, frame-quantized at the timeline rate, 8-character reels, dissolves, M2 speed lines, drop-frame timecode at 29.97/59.94). Returns the path and a report of everything the format dropped or approximated (merged, with counts). Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_interchange(&self, Parameters(p): Parameters<ExportInterchangeParams>) -> Result<String, McpError> {
        let output = self.path_arg(&p.output_path)?;
        crate::require_local_output_path(&output).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(output);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let inputs = self.lock().delivery_inputs().map_err(core_err)?;
        let (format, track) = (p.format, p.video_track.unwrap_or(0));
        let report = blocking(move || {
            let (text, report) = Project::interchange_document(&inputs, format, track).map_err(core_err)?;
            Project::write_document(&path, &text).map_err(core_err)?;
            Ok(report)
        })
        .await?;
        json(&kerf_core::ExportedFile {
            path: p.output_path,
            report,
        })
    }

    #[tool(
        description = "Write the markers as a CSV (every marker: name, kind, start/end timecode, seconds, comment, colour; cells that look like spreadsheet formulas are defused) or as YouTube chapter text (`0:00 Intro` lines from the chapter markers; the report says when YouTube would refuse them). Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_markers(&self, Parameters(p): Parameters<ExportMarkersParams>) -> Result<String, McpError> {
        let output = self.path_arg(&p.output_path)?;
        crate::require_local_output_path(&output).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(output);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let inputs = self.lock().delivery_inputs().map_err(core_err)?;
        let format = p.format;
        let report = blocking(move || {
            let (text, report) = Project::markers_document(&inputs, format).map_err(core_err)?;
            Project::write_document(&path, &text).map_err(core_err)?;
            Ok(report)
        })
        .await?;
        json(&kerf_core::ExportedFile {
            path: p.output_path,
            report,
        })
    }

    #[tool(
        description = "List the hardware (GPU) video encoders this machine's ffmpeg can actually use (verified by a \
                       test encode) — pass one as export's video_codec for a much faster render. Empty means software \
                       encoders only. libx264 stays the safe default: a hardware encoder trades file size for speed."
    )]
    async fn export_capabilities(&self) -> Result<String, McpError> {
        let encoders = blocking(|| Ok(kerf_core::hw_encoders().to_vec())).await?;
        json(&serde_json::json!({ "hw_encoders": encoders }))
    }

    #[tool(
        description = "Render the timeline to a file with full ffmpeg encode control (container, video/audio codec, \
                       rate control, resolution, fps, bitrate, faststart, gif, audio-only …). Omit `options` for the \
                       safe H.264/AAC MP4 default. Frame size and rate: `options.resolution` and `options.fps` win; \
                       without them the file takes the project's delivery frame (set_delivery_format: its size and \
                       its `fps`), and without that the FIRST clip with a picture, which in footage of mixed frame \
                       rates is whichever comes first. The result reports the `width`, `height` and `fps` the file \
                       was written at and where the rate came from (`fps_source`: options / delivery / footage); \
                       check it against what was asked for. `options.loudness` normalizes the mix to a platform target: \
                       youtube / spotify (-14 LUFS), apple (-16), broadcast (EBU R128, -23), all at -1 dBTP; add \
                       `options.loudness_static: true` to get there with one measured gain and a true-peak limiter \
                       (the mix's dynamics untouched; the render measures it first) instead of loudnorm's riding gain. A render takes minutes: send a `progressToken` in the request's \
                       `_meta` to receive progress notifications while it runs, and cancel the request \
                       (`notifications/cancelled`) to stop it — a cancelled render deletes its half-written file \
                       rather than leaving a broken one behind. Or pass `background: true` to get a job_id at once \
                       and poll job_status (cancel_job stops it); its `message` says the stage — `metering loudness`, \
                       `starting ffmpeg and seeking the footage` (nothing is written while it opens a long recording and \
                       seeks a range's start), `rendering, … to go`, `finishing the file`, `measuring the written file` — \
                       and `progress` is over the whole export. Refuses to overwrite an existing file unless \
                       overwrite is true, and refuses to write over a file the project still uses (a source, a \
                       proxy, the project itself). The result is `{output, report}`: `report` is what the written FILE \
                       measures (not the timeline mix `get_levels` reads) — `container`, `duration_secs`, `size_bytes`, overall \
                       `bit_rate`, `video` (codec, width, height, fps, pix_fmt, bit_rate), `audio` (codec, sample_rate, \
                       channels, bit_rate) and, when it has sound, `loudness` (integrated_lufs, true_peak_dbtp, \
                       loudness_range_lu, peak_dbfs: the file's own, after the lossy encode), with the `target` of \
                       `options.loudness` and `true_peak_over_db` when the true peak is over that target's ceiling. A \
                       measurement that fails (or is stopped by cancel) says why in `report.problem` and does not fail \
                       the export. `warnings` (when there are any) names each stabilized clip that exported as shot, \
                       its camera never measured or the measuring failed (stabilize_clip measures it)."
    )]
    async fn export(
        &self,
        Parameters(p): Parameters<ExportParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let output = self.path_arg(&p.output_path)?;
        crate::require_local_output_path(&output).map_err(|e| McpError::invalid_params(e, None))?;
        let output = std::path::PathBuf::from(output);
        refuse_overwrite(&output, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&output)).await?;
        let opts = ExportOptions {
            overwrite_sidecars: p.overwrite.unwrap_or(false),
            ..p.options.unwrap_or_default()
        };
        self.check_beside(opts.caption_sidecar_paths(&output).iter().map(|p| p.as_path()))?;
        let (project, owner) = (self.project.clone(), self.owner());
        // A render runs for minutes. Without progress an agent cannot tell a slow export from a
        // hung one, and without cancellation it cannot abandon a render whose settings it already
        // knows are wrong. Both ride the protocol (`progressToken`, `notifications/cancelled`) —
        // or, as a background job, `job_status` and `cancel_job`.
        // ...and the GUI's Stop on an export ends this render too (`begin_export`).
        let gui_stop = self.app.state::<crate::AppState>().begin_export();
        self.long_tool("export", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let status = blocking(move || {
                // Snapshot the timeline + assets under the lock, then render with the
                // lock released so a long export doesn't freeze the GUI (or block other
                // agent tools) for its whole duration.
                let (timeline, assets, steady) = {
                    let project = lock_agent_as(&project, owner.as_deref());
                    (
                        project.working_timeline().map_err(core_err)?,
                        project.list_assets().map_err(core_err)?,
                        project.stabilization_status_inputs(None).map_err(core_err)?,
                    )
                };
                // A stabilized clip whose camera was never measured exports as shot: not a reason to
                // refuse the export, but said in its answer (read with the lock released).
                let warnings: Vec<String> = kerf_core::unsteady_clips(
                    &timeline,
                    &Project::stabilization_statuses(&steady),
                    timeline.export_span(opts.range.map(|r| (r.start, r.end))),
                )
                .iter()
                .map(kerf_core::Unsteady::message)
                .collect();
                let mut on_progress = |progress: kerf_core::ExportProgress| {
                    run.progress(Some(progress.fraction), export_message(&progress));
                };
                let before = kerf_core::file_mtime(&output);
                // The picture the file will be, said back: the size and rate the options and the
                // delivery frame made of the footage's own.
                let frame = kerf_core::exported_frame(&timeline, &assets, &opts);
                // A completed render is read back in the same job (`report`: the file's streams and
                // its own loudness), stopped by the same cancel; a measurement that fails says so in
                // the report and does not fail the export.
                let (status, report) =
                    kerf_core::render_measured(&timeline, &assets, &output, &opts, &mut on_progress, &|| run.cancelled())
                        .map_err(|e| {
                            // A failed render must not leave a truncated file that reads as a
                            // finished one — and must not touch an earlier export it never wrote over.
                            kerf_core::discard_partial(&output, before);
                            core_err(e)
                        })?;
                if status == kerf_core::RenderStatus::Cancelled {
                    // Done *here*, not after the await — cancelling the request can drop this
                    // handler's future, while a blocking job runs to completion whether or not
                    // anyone is still holding its handle.
                    kerf_core::discard_partial(&output, before);
                } else {
                    // The ticks the render backfilled are kept, the lock taken only to write them.
                    crate::keep_time_bases(&timeline, &assets, &opts, || lock_agent_as(&project, owner.as_deref()));
                }
                Ok((status, output, frame, report, warnings))
            })
            .await?;
            match status {
                (kerf_core::RenderStatus::Completed, output, frame, report, warnings) => {
                    let mut answer = serde_json::json!({ "output": output, "report": report });
                    if !warnings.is_empty() {
                        answer["warnings"] = serde_json::json!(warnings);
                    }
                    json(&with_frame(answer, frame))
                }
                (kerf_core::RenderStatus::Cancelled, output, ..) => Err(McpError::internal_error(
                    format!("export cancelled; the partial file at {} was removed", output.display()),
                    None,
                )),
            }
        })
        .await
    }

    #[tool(
        description = "Export the same cut at several delivery frames in one call — one file per shape, each shot \
                       framed for each: a 9:16 Reel, a 1:1 post and a 16:9 upload from one timeline. Shots are \
                       smart-cropped per shape first (unless smart_crop is false), which is recorded on the clips as \
                       one revision and reused by later exports; generated captions are re-fit to each frame. The \
                       project's own frame is untouched. Files land beside output_path named by shape \
                       (`cut-9x16.mp4`) and keep the project's delivery frame rate (set_delivery_format's `fps`; \
                       `options.fps` wins). Reports each file with the size and frame rate it was written at, \
                       the platforms it is ready for and any issue, so there is no need to run platform_check per variant afterwards. Progress and cancellation \
                       work as in `export` (and `background: true` returns a job_id). The files render in one pass by \
                       default (single_pass): one decode of the sources split into every frame, so cancelling removes \
                       them all; with single_pass false they render one after another and cancelling keeps the files \
                       already finished and deletes the one in flight. Refuses to overwrite any existing variant file unless overwrite is true. \
                       Each entry of `outputs` carries a `report` of what that written file measures (container, picture, \
                       sound, and its own integrated loudness / true peak / loudness range, with `true_peak_over_db` when it \
                       is over the `options.loudness` target's ceiling), as `export` does."
    )]
    async fn export_variants(
        &self,
        Parameters(p): Parameters<ExportVariantsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        if p.formats.is_empty() {
            return Err(McpError::invalid_params(
                "formats must name at least one delivery frame",
                None,
            ));
        }
        let output = self.path_arg(&p.output_path)?;
        crate::require_local_output_path(&output).map_err(|e| McpError::invalid_params(e, None))?;
        let mut deliveries: Vec<Delivery> = Vec::new();
        for name in &p.formats {
            let d = Delivery::parse(name).ok_or_else(|| {
                McpError::invalid_params(
                    format!("unknown delivery format {name:?}: expected 9:16, 1:1, 4:5, 16:9 or WxH"),
                    None,
                )
            })?;
            if !deliveries.contains(&d) {
                deliveries.push(d);
            }
        }
        let opts = p.options.unwrap_or_default();
        let base = std::path::PathBuf::from(output);
        let variants: Vec<kerf_core::ExportVariant> = deliveries
            .iter()
            .map(|d| kerf_core::ExportVariant::beside(&base, *d))
            .collect();
        let overwrite = p.overwrite.unwrap_or(false);
        let opts = ExportOptions {
            overwrite_sidecars: overwrite,
            ..opts
        };
        let sidecars: Vec<std::path::PathBuf> = variants.iter().flat_map(|v| opts.caption_sidecar_paths(&v.output)).collect();
        self.check_beside(
            variants
                .iter()
                .map(|v| v.output.as_path())
                .chain(sidecars.iter().map(|p| p.as_path())),
        )?;
        for variant in &variants {
            refuse_overwrite(&variant.output, overwrite)?;
            kerf_core::check_caption_sidecars(&variant.output, &opts).map_err(core_err)?;
        }
        self.check_outputs(&variants.iter().map(|v| v.output.clone()).collect::<Vec<_>>())
            .await?;
        let smart_crop = p.smart_crop.unwrap_or(true);
        let single_pass = p.single_pass.unwrap_or(true);
        let (me, owner) = (self.clone(), self.owner());
        let generation = self.lock().generation();
        // The GUI's Stop on an export ends the framing pass and every file of this one too.
        let gui_stop = self.app.state::<crate::AppState>().begin_export();
        self.long_tool("export_variants", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let project = me.project.clone();
            // The framing pass is the one write this tool makes. It is announced whether or not the
            // render after it succeeds: the proposal changed either way.
            let framed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let wrote = framed.clone();
            let labels: Vec<String> = deliveries.iter().map(Delivery::ratio_label).collect();
            let render_variants = variants.clone();
            let result = blocking(move || {
                // Frame first: plan under the lock, sample with it released (one
                // short decode per clip, shared by every shape), apply under it again.
                // The framing pass owns the first tenth of the job's progress when it runs; the
                // render takes the rest (it reports its own 0..1 over every file).
                let mut rendering_from = 0.0;
                if smart_crop {
                    let plan = lock_agent_as(&project, owner.as_deref())
                        .framing_inputs(&deliveries)
                        .map_err(core_err)?;
                    if !plan.jobs.is_empty() {
                        let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                        let framings = Project::sample_framings_with(
                            &plan,
                            &mut |f| run.progress(Some(f64::from(f) * FRAMING_SHARE), "framing each shot"),
                            &stop,
                        )
                        .map_err(core_err)?;
                        let guard = lock_agent_edit(&project, owner.as_deref())?;
                        require_generation(&guard, generation, "the shots were being framed")?;
                        wrote.store(guard.apply_framings(&framings).map_err(core_err)?, Ordering::SeqCst);
                        rendering_from = FRAMING_SHARE;
                    }
                }
                let (timeline, assets) = {
                    let project = lock_agent_as(&project, owner.as_deref());
                    (
                        project.working_timeline().map_err(core_err)?,
                        project.list_assets().map_err(core_err)?,
                    )
                };
                let mut on_progress = |progress: kerf_core::VariantProgress| {
                    run.progress(
                        Some(rendering_from + (1.0 - rendering_from) * progress.fraction),
                        variants_message(&progress, &labels),
                    );
                };
                // Core removes the file of a variant that failed or was cancelled in flight.
                // A completed run reads every file back (`reports`, one per file in order: the
                // file's streams and its own loudness); a stopped one has none to give.
                let (ended, reports) = kerf_core::render_variants_measured(
                    &timeline,
                    &assets,
                    &render_variants,
                    &opts,
                    single_pass,
                    &mut on_progress,
                    &|| run.cancelled(),
                )
                .map_err(core_err)?;
                if ended.status == kerf_core::RenderStatus::Completed {
                    // The ticks the render backfilled are kept, before the lock the answer below holds.
                    crate::keep_time_bases(&timeline, &assets, &opts, || lock_agent_as(&project, owner.as_deref()));
                }
                // Judged per file, at the frame that file actually is and over the span it writes.
                let range = opts.range.map(|r| (r.start, r.end));
                let mut outputs = Vec::new();
                let project = lock_agent_as(&project, owner.as_deref());
                for (i, v) in render_variants.iter().enumerate().take(ended.completed) {
                    // The size and rate that file was written at, not the delivery's nominal ones: the
                    // options may have named their own, and the project's frame rate rides along.
                    let frame = kerf_core::variant_frame(&timeline, &assets, v, &opts);
                    let mut output = with_frame(variant_output(&project, v, range)?, frame);
                    output["report"] = serde_json::to_value(reports.get(i)).unwrap_or(serde_json::Value::Null);
                    outputs.push(output);
                }
                Ok((ended.status, outputs, ended.together))
            })
            .await;
            let framed = framed.load(Ordering::SeqCst);
            if framed > 0 {
                me.changed();
            }
            let (status, outputs, together) = result?;
            match status {
                kerf_core::RenderStatus::Completed => json(&serde_json::json!({
                    "clips_framed": framed,
                    "single_pass": together,
                    "outputs": outputs,
                })),
                kerf_core::RenderStatus::Cancelled if together => Err(McpError::internal_error(
                    format!(
                        "export cancelled; the {} files rendering together were removed",
                        variants.len()
                    ),
                    None,
                )),
                kerf_core::RenderStatus::Cancelled => Err(McpError::internal_error(
                    format!(
                        "export cancelled after {} of {} files; the one in flight was removed",
                        outputs.len(),
                        variants.len()
                    ),
                    None,
                )),
            }
        })
        .await
    }

    #[tool(
        description = "Check the assembled cut against each publishing target (Instagram Reels / YouTube Shorts / \
                       TikTok / Instagram feed / YouTube): length limits, frame shape, and the reach limits a platform \
                       enforces silently — e.g. a Reel over 3 minutes uploads fine and is then shown only to existing \
                       followers. Returns errors (would be rejected), warnings (accepted but under-distributed or \
                       letterboxed) and tips. Advisory: export is never blocked. Pass `range` to judge only the span an \
                       export of that range would write (a Reel cut from the middle of a long timeline is judged at \
                       the Reel's length, frame and sound, not the timeline's), and `width` + `height` to judge it at the frame an export with a different `options.resolution` would write. Use before reporting a cut finished."
    )]
    fn platform_check(&self, Parameters(p): Parameters<PlatformCheckParams>) -> Result<String, McpError> {
        let frame = check_frame(p.width, p.height)?;
        // One summary, not two: `Project::platform_report` resolves its own, and
        // each resolve deserializes the whole timeline and re-reads every asset
        // row to answer the identical question.
        let summary = self
            .lock()
            .cut_summary_in(frame, p.range.map(|r| (r.start, r.end)))
            .map_err(core_err)?;
        let targets = kerf_core::platform::check_all(&summary);
        json(&serde_json::json!({
            "cut": {
                "duration_secs": summary.duration,
                "frame": format!("{}x{}", summary.width, summary.height),
                "has_audio": summary.has_audio,
                "has_text": summary.has_text,
            },
            "targets": targets,
        }))
    }

    #[tool(
        description = "Lint the cut for editing mistakes an agent cannot see without watching it: flash_frame (a visible \
                       clip under 3 frames at the export rate), mid_word_cut (a sounding clip's edge inside a transcript \
                       word; needs a transcript), hard_audio_cut (sound meeting sound with no fade or crossfade, a likely \
                       click; needs silence analysis), jump_cut (the same shot resuming a moment later; a tip, it can be \
                       deliberate), stale_captions (generated captions no longer over the words spoken, or speech left \
                       uncaptioned, after a later edit), black_gap (no picture while the cut plays) and unstabilized (a \
                       stabilized clip whose camera was never measured, or whose measuring failed: it exports as shot). Reads the cut you \
                       are building and the cached analysis; answers {issues: [{kind, severity, time, end?, clip_ids, \
                       message, fix}], counts_by_kind, checked, skipped: [{check, reason}]}, each `fix` naming the tools \
                       that address it. A picture and its linked sound report once. Run it with platform_check before \
                       reporting a cut finished."
    )]
    async fn review_cut(&self, Parameters(p): Parameters<ReviewCutParams>) -> Result<String, McpError> {
        let checks = kerf_core::ReviewCheck::parse_list(&p.checks.unwrap_or_default()).map_err(core_err)?;
        let range = p.range.map(|r| (r.start, r.end));
        let (project, owner) = (self.project.clone(), self.owner());
        let review = blocking(move || {
            let inputs = lock_agent_as(&project, owner.as_deref()).review_inputs().map_err(core_err)?;
            Ok(kerf_core::review::review(&inputs, range, &checks))
        })
        .await?;
        json(&review)
    }

    #[tool(
        description = "Write the composited timeline at a given time to an image file as a cover / thumbnail — full \
                       delivery resolution, rendered through the export graph, so it is a real frame of the finished \
                       video at the shape the project is cut for. Refuses to overwrite an existing file unless \
                       overwrite is true."
    )]
    async fn export_cover(
        &self,
        Parameters(p): Parameters<CoverParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let output = self.path_arg(&p.output_path)?;
        crate::require_local_output_path(&output).map_err(|e| McpError::invalid_params(e, None))?;
        let output = std::path::PathBuf::from(output);
        refuse_overwrite(&output, p.overwrite.unwrap_or(false))?;
        self.check_outputs(std::slice::from_ref(&output)).await?;
        let (project, owner) = (self.project.clone(), self.owner());
        let time_secs = p.time_secs;
        // The request's own cancellation, and the GUI's Cancel on a cover; progress rides the
        // request's `progressToken` in the stages a one-frame render has.
        let gui_stop = self.app.state::<crate::AppState>().begin_cover_export();
        let (run, relay) = jobs::foreground(&context);
        let run = run.also_cancelled_by(gui_stop);
        let written = blocking(move || {
            run.progress(Some(0.1), "reading the cut");
            let (timeline, assets) = lock_agent_as(&project, owner.as_deref())
                .export_still_inputs()
                .map_err(core_err)?;
            run.progress(Some(0.2), "rendering the cover frame");
            let written = Project::render_still_cancellable(&timeline, &assets, time_secs, &output, None, &|| run.cancelled())
                .map_err(core_err)?;
            run.progress(Some(1.0), "cover written");
            Ok(written)
        })
        .await;
        relay.finish().await;
        let written = written?;
        json(&serde_json::json!({ "output": written.to_string_lossy() }))
    }
}
