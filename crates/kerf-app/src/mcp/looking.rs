//! Looking and measuring: frames, contact sheets, waveforms, levels and scopes.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GetLevelsParams {
    #[schemars(description = "Measure only this span of the cut, {start, end} in timeline seconds. Omit for the whole cut")]
    range: Option<TimeRange>,
    #[schemars(
        description = "Measure the mix as an export with loudnorm=true would write it (default false: the mix as it stands, which is what tells you whether to turn loudnorm on)"
    )]
    loudnorm: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct BandBalanceParams {
    #[schemars(description = "UUID of an asset: measure its whole sound. Times are the asset's own seconds.")]
    asset_id: Option<String>,
    #[schemars(
        description = "UUID of a clip: measure its source range at the clip's own volume (its fader and effects are not applied). Times are source seconds."
    )]
    clip_id: Option<String>,
    #[schemars(
        description = "Measure the finished mix of the cut over this span, {start, end} in timeline seconds — the audio an export would render, faders, ducking and master bus included (what get_levels measures)."
    )]
    range: Option<TimeRange>,
    #[schemars(
        description = "The stretches to compare, [{start, end, label?}] in the target's seconds (at most 100, each at least 0.25 s). Omitted: a composed asset's score sections, else the four-bar phrases of analyzed music, else eight equal windows; `windows_from` in the answer says which."
    )]
    sections: Option<Vec<kerf_core::BandWindow>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WaveformParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Number of buckets to return (1–4096; a few hundred is plenty to read a shape from)")]
    buckets: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WaveformRangeParams {
    #[schemars(description = "UUID of the asset (it must have an audio stream)")]
    asset_id: String,
    #[schemars(
        description = "Start of the window in SOURCE seconds — time within the asset's own audio, from 0 at the start of the file, not timeline time. Part of the window before 0 reads as silence."
    )]
    start: f64,
    #[schemars(
        description = "End of the window in SOURCE seconds; must be greater than start. Part of the window past the end of the audio reads as silence."
    )]
    end: f64,
    #[schemars(
        description = "Number of buckets to split the window into (1–4096, capped). A few hundred shows the shape of a whole file; to see exactly where a word or beat begins, ask for a short window instead of more buckets."
    )]
    buckets: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FrameParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Time in the source asset to decode (seconds)")]
    time_secs: f64,
    #[schemars(description = "Maximum output width in pixels (default 640, capped at 1920)")]
    max_width: Option<u32>,
    #[schemars(
        description = "Zoom into part of the frame instead of seeing all of it: a rectangle in fractions of the full frame (0..1), `left`/`top` its top-left corner, `width`/`height` its size. The region is cropped out first and then scaled to max_width, so a quarter of the frame shows four times the detail for the same image cost — use it to check a face, on-screen text, a caption, a mask edge. Omit for the whole frame."
    )]
    region: Option<RegionParams>,
}

/// A region of a frame to zoom into, as the schema hands it to a model: the
/// same fractions [`Region`] takes, kept as a separate type so the tool schema
/// documents each edge.
#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RegionParams {
    #[schemars(description = "Left edge of the region as a fraction of the frame width (0 = left edge)")]
    left: f64,
    #[schemars(description = "Top edge of the region as a fraction of the frame height (0 = top edge)")]
    top: f64,
    #[schemars(description = "Width of the region as a fraction of the frame width")]
    width: f64,
    #[schemars(description = "Height of the region as a fraction of the frame height")]
    height: f64,
}

impl RegionParams {
    /// The engine region, pulled into the frame — a model's fractions are a
    /// request, not a proof.
    fn region(self) -> Region {
        Region {
            left: self.left,
            top: self.top,
            width: self.width,
            height: self.height,
        }
        .normalized()
    }

    /// The region as the caption echoes it back, after normalization, so the
    /// model's next crop is in the coordinates that were actually used.
    fn describe(self) -> String {
        let r = self.region();
        format!(
            "region left={:.3} top={:.3} width={:.3} height={:.3} of the full frame",
            r.left, r.top, r.width, r.height
        )
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SkimParams {
    #[schemars(description = "UUID of the asset to skim")]
    asset_id: String,
    #[schemars(description = "Start of the time range in seconds (default 0 = start of asset)")]
    start: Option<f64>,
    #[schemars(description = "End of the time range in seconds (default the asset's full duration)")]
    end: Option<f64>,
    #[schemars(description = "Grid columns (default 4, max 8)")]
    columns: Option<u32>,
    #[schemars(description = "Grid rows (default 4, max 8)")]
    rows: Option<u32>,
    #[schemars(description = "Width of each grid cell in pixels (default 240)")]
    cell_width: Option<u32>,
    #[schemars(
        description = "Zoom into one cell of the sheet instead of building it: the 1-based, row-major cell number from a previous skim with the same range and grid. Returns that cell's moment as a single full-detail frame (max_width wide, from the original source) — the shortcut from 'cell 7 looks promising' to seeing it properly."
    )]
    cell: Option<u32>,
    #[schemars(description = "Width of the zoomed cell frame in pixels when `cell` is given (default 640, capped at 1920)")]
    max_width: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TimelineFrameParams {
    #[schemars(description = "Timeline position to render (seconds)")]
    time_secs: f64,
    #[schemars(description = "Maximum output width in pixels (default 640, capped at 1920)")]
    max_width: Option<u32>,
    #[schemars(
        description = "Zoom into part of the frame instead of seeing all of it: a rectangle in fractions of the full frame (0..1), `left`/`top` its top-left corner, `width`/`height` its size. The region is cropped out first and then scaled to max_width, so a quarter of the frame shows four times the detail for the same image cost — use it to check a face, on-screen text, a caption, a mask edge. Omit for the whole frame."
    )]
    region: Option<RegionParams>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SkimTimelineParams {
    #[schemars(description = "Start of the span of the cut in timeline seconds (default 0)")]
    pub(super) start: Option<f64>,
    #[schemars(description = "End of the span in timeline seconds (default the end of the cut)")]
    end: Option<f64>,
    #[schemars(description = "Grid columns (default 4, max 8)")]
    columns: Option<u32>,
    #[schemars(description = "Grid rows (default 4, max 8)")]
    rows: Option<u32>,
    #[schemars(description = "Width of each grid cell in pixels (default 240, 80 to 640)")]
    cell_width: Option<u32>,
    #[schemars(
        description = "Open one cell of the sheet instead of building it: the 1-based, row-major cell number from a previous skim_timeline with the same span and grid. Returns the cut at that cell's moment as one frame, max_width wide (what preview_timeline shows there)."
    )]
    pub(super) cell: Option<u32>,
    #[schemars(description = "Width of the cell frame in pixels when `cell` is given (default 640, capped at 1920)")]
    max_width: Option<u32>,
    #[schemars(
        description = "With `cell`: zoom into part of that frame, a rectangle in fractions of the frame (0..1), as preview_timeline's `region`. Ignored without `cell`."
    )]
    pub(super) region: Option<RegionParams>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ScopesParams {
    #[schemars(description = "Timeline position to measure (seconds)")]
    time_secs: f64,
    #[schemars(
        description = "Grids to return besides the numeric summary: any of `waveform` (luma), `parade` (red, green, blue), \
                       `vectorscope`, `histogram`. Omit for the summary alone, which is usually all you need."
    )]
    grids: Option<Vec<kerf_core::scopes::ScopeKind>>,
    #[schemars(
        description = "Grid size, each field optional: `columns` / `rows` of the waveform and parade (default 64 x 64, \
                       at most 512 x 256), `vector` the vectorscope's side (default 48, at most 256)"
    )]
    pub(super) grid_size: Option<GridSizeParams>,
    #[schemars(description = "Width to measure the frame at (default the delivery's, at most 3840)")]
    max_width: Option<u32>,
    #[schemars(
        description = "The Y'CbCr matrix to read the frame with: `auto` (default: the delivery's, BT.709 for HD and up, \
                       BT.601 for SD), `bt601`, `bt709`, `bt2020`"
    )]
    matrix: Option<kerf_core::scopes::ScopeMatrix>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct GridSizeParams {
    columns: Option<u32>,
    rows: Option<u32>,
    vector: Option<u32>,
}

impl GridSizeParams {
    pub(super) fn resolve(self) -> kerf_core::scopes::GridSize {
        kerf_core::scopes::GridSize {
            columns: self.columns.unwrap_or(64),
            rows: self.rows.unwrap_or(64),
            vector: self.vector.unwrap_or(48),
        }
        .clamped()
    }
}

#[tool_router(router = tool_router_looking, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Measure the picture at a timeline time without looking at it: video scopes of the composited \
                       frame the export would write (originals, full delivery resolution). Returns a summary: per channel \
                       (red, green, blue, luma) min / max / mean in 0..1 and how many pixels are clipped at 0 and at 1 \
                       (`clipped_low` / `clipped_high`, counted over every pixel, so a single blown pixel shows), the mean \
                       chroma (`cb`, `cr` in -0.5..0.5; `hue_deg` counter-clockwise from +Cb, so red is ~103, skin tones \
                       ~123, blue ~355; `saturation` of the mean — a colour cast — and `mean_saturation`, how colourful \
                       the frame is, both as |CbCr| / 0.5) and the densest vectorscope spots. Use it to check exposure, \
                       clipping, a colour cast or a grade's effect after an edit. Grids are optional (counts, row 0 the \
                       lowest level)."
    )]
    async fn get_scopes(
        &self,
        Parameters(p): Parameters<ScopesParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        if !p.time_secs.is_finite() {
            return Err(McpError::invalid_params("time_secs must be a number", None));
        }
        let (project, owner) = (self.project.clone(), self.owner());
        let cancel = context.ct.clone();
        let req = kerf_core::scopes::ScopeRequest {
            time_secs: p.time_secs,
            max_width: p.max_width.map(|w| w.clamp(64, kerf_core::scopes::MAX_SCOPE_WIDTH)),
            matrix: p.matrix.unwrap_or_default(),
            options: kerf_core::scopes::ScopeOptions {
                grids: p.grids.unwrap_or_default(),
                size: p.grid_size.unwrap_or_default().resolve(),
            },
        };
        let scopes = blocking(move || {
            let (timeline, assets) = lock_agent_as(&project, owner.as_deref())
                .export_still_inputs()
                .map_err(core_err)?;
            kerf_core::scopes::timeline_scopes_cancellable(&timeline, &assets, &req, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&scopes)
    }

    #[tool(
        description = "Peak magnitude per bucket (0.0–1.0, unsigned) over the WHOLE of an asset's first audio stream, as a bare JSON array of `buckets` numbers (1–4096; a few hundred shows the shape). To look at a window, with a sign and per channel, use get_waveform_range; for how loud each slice FEELS use get_energy."
    )]
    async fn get_waveform(
        &self,
        Parameters(p): Parameters<WaveformParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let buckets = blocking(move || {
            // Resolve under the lock, decode the whole audio stream with it
            // released — bucketing a long source takes seconds and must not
            // stall the GUI's commands on the shared mutex. A dropped request
            // stops the decode.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_waveform_cancellable(&asset, count, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&buckets)
    }

    #[tool(
        description = "Read what an asset's audio looks like over a window: the lowest and highest sample value in each of `buckets` equal slices of source time [start, end), per channel. Unlike get_waveform (one unsigned magnitude per bucket across the whole file) it is signed, one lane per channel (stereo gives two, mono one) and zoomable: ask for the whole asset in a few hundred buckets to find the loud, quiet and silent stretches, then ask again for a short window around one of them to find the exact moment a word, beat or click starts — the way to pick a cut point. Returns compact JSON {channels, buckets, duration, peaks_per_second, min, max}. `min` and `max` are each an array of `channels` arrays of `buckets` numbers in -1.0..1.0; bucket j covers source time start + j * (end - start) / buckets. `duration` is the length of the audio in seconds: a bucket before 0 or past it reads 0.0 / 0.0, so use `duration` to tell where the audio ends from silence. A value of exactly 1.0 or -1.0 is a clipped sample, and a run of them is distortion already baked into the source. `peaks_per_second` is the stored resolution the buckets were read from (10, 25, 100 or 500); when it is below buckets / (end - start) the request was finer than the stored 500 per second and neighbouring buckets repeat, so a shorter window will not reveal more. An asset with no audio stream (video-only footage, a still image) is rejected. The first call for a long file decodes it once, which can take seconds; every later window of that file is instant."
    )]
    async fn get_waveform_range(
        &self,
        Parameters(p): Parameters<WaveformRangeParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        require_window(p.start, p.end)?;
        let (start, end) = (p.start, p.end);
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let range = blocking(move || {
            // Resolve under the lock, read the pyramid with it released — the
            // first call for a file decodes all of its audio, which must not
            // stall the GUI's commands on the shared mutex. A dropped request
            // stops that decode (and caches nothing of it).
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_waveform_range_cancellable(&asset, start, end, count, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&range)
    }

    #[tool(
        description = "Get an RMS energy envelope (0.0–1.0 per bucket) for an asset's first audio stream — a perceptual loudness-over-time curve. Unlike the peak waveform, it tracks how loud each slice feels, so use it to find quiet/loud passages and match cut pacing to musical energy."
    )]
    async fn get_energy(
        &self,
        Parameters(p): Parameters<WaveformParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let energy = blocking(move || {
            // Same lock-free, cancellable decode shape as `get_waveform`.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_energy_cancellable(&asset, count, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&energy)
    }

    #[tool(
        description = "Measure how loud the cut is: the integrated loudness (LUFS), loudness range (LU), \
                       short-term maximum (LUFS), sample peak (dBFS) and true peak (dBTP), for the mix and \
                       for each track that feeds it, in one pass over the audio the \
                       export would render (so the master reading includes the master fader and limiter, and \
                       loudnorm when asked for). A track's reading is its strip: after its fader and pan, before \
                       the duck bus and the master. Platforms play social video at about -14 LUFS and ask for a \
                       true peak under -1 dBTP; `notes` says in words what is off and which tool fixes it \
                       (set_master_volume, set_master_limiter, set_track_volume, or exporting with loudnorm). \
                       Run it before you report a cut finished. A muted track is `heard: false` with no level, \
                       and a value of null means there was nothing to measure (silence; a short-term maximum \
                       needs a span of 3 seconds). It reads the whole span, so it takes a few seconds for a long \
                       cut; cancel the request to stop it. Reads the cut you are proposing, staged edits included."
    )]
    async fn get_levels(
        &self,
        Parameters(p): Parameters<GetLevelsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let levels = blocking(move || {
            // Resolve under the lock, run ffmpeg over the whole span with it
            // released — a long cut takes seconds and must not stall the GUI.
            let (timeline, assets) = lock_agent(&project).levels_inputs().map_err(core_err)?;
            Project::measure_levels(&timeline, &assets, p.range, p.loudnorm.unwrap_or(false), &|| {
                cancel.is_cancelled()
            })
            .map_err(core_err)
        })
        .await?;
        json(&levels)
    }

    #[tool(
        description = "Measure where sound keeps its energy: six frequency bands — sub 20-60 Hz, bass 60-250, low_mid 250-500, mid 500-2k, high_mid 2k-6k, air 6k-20k — per window, each as an energy level (dBFS) and a `share_pct` of the window's energy across the bands, plus the window's RMS (dBFS), K-weighted loudness (LUFS, ungated, so a window reads like a loudness meter) and `dominant` band. \
                       Name exactly one target: `asset_id` (the asset's whole sound, in its own seconds), `clip_id` (that clip's source range at the clip's volume, in source seconds) or `range` {start, end} (the finished mix of the cut over that span of the timeline — faders, ducking, master bus and all, the audio get_levels measures; it reads the cut you are proposing, staged edits included). \
                       Windows: `sections` [{start, end, label?}] in the target's seconds; omitted, a composed asset's score sections (intro, drop 1, …), else the four-bar phrases of analyzed music, else eight equal windows — `windows_from` says which. `overall` is the whole stretch as one window, and `notes` says in words what stands out: a heavy low end, a thin mid range (500 Hz-6 kHz, where speech lives), a dull top, a loudness swing between sections, a section that is bassier than the whole. \
                       Use it after compose_music to see whether the drop is much louder than the intro, and under a voiceover: music with much in mid and high_mid fights speech, and a big sub / bass share eats headroom and vanishes on a phone. Fix with set_audio_effects (a highpass or EQ on the music clip), set_track_volume or set_track_duck, or compose again with another style / energy. Decodes the audio, so it takes a few seconds for a long one; cancel the request to stop it."
    )]
    async fn get_band_balance(
        &self,
        Parameters(p): Parameters<BandBalanceParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let request = kerf_core::BandBalanceRequest {
            asset_id: p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?,
            clip_id: p.clip_id.as_deref().map(|s| parse_arg("clip_id", s)).transpose()?,
            range: p.range,
            sections: p.sections,
        };
        let project = self.project.clone();
        let owner = self.owner();
        let cancel = context.ct.clone();
        let balance = blocking(move || {
            // Resolve under the lock, decode with it released — a long cut takes seconds and must
            // not stall the GUI. A dropped request, or a project replaced meanwhile, stops it.
            let (plan, generation) = {
                let project = lock_agent_as(&project, owner.as_deref());
                (project.band_balance_inputs(&request).map_err(core_err)?, project.generation())
            };
            let stop = crate::stop_or_replaced(|| cancel.is_cancelled(), &project, generation);
            Project::measure_band_balance(&plan, &stop).map_err(core_err)
        })
        .await?;
        json(&balance)
    }

    #[tool(
        description = "Decode a single frame from an asset at a source time and return it as a low-res image the model can actually see. Use to drill into a specific moment (e.g. one cell flagged by skim_asset) before cutting."
    )]
    async fn get_frame(
        &self,
        Parameters(p): Parameters<FrameParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let (time_secs, max_width) = (p.time_secs, p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH));
        let region = p.region;
        let jpeg = blocking(move || {
            // Resolve under the lock, decode with it released — mirrors the GUI's
            // `get_frame` so an agent drill-in can't stall the user's edits.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            match region {
                // A zoom is a request for detail, so it decodes the original at
                // the best JPEG quality rather than the proxy at the preview one.
                Some(r) => {
                    Project::decode_preview_region_cancellable(&asset, time_secs, r.region(), max_width, ZOOM_QUALITY, &|| {
                        cancel.is_cancelled()
                    })
                }
                None => {
                    Project::decode_preview_frame_cancellable(&asset, time_secs, max_width, 4, true, &|| cancel.is_cancelled())
                }
            }
            .map_err(core_err)
        })
        .await?;
        let mut caption = format!("asset {} @ {}", p.asset_id, fmt_ts(p.time_secs.max(0.0)));
        if let Some(r) = region {
            caption.push_str(&format!(", {}", r.describe()));
        }
        Ok(image_result(caption, jpeg))
    }

    #[tool(
        description = "Skim an asset: sample frames evenly across a time range (default the whole asset) into one contact-sheet image, plus a text index of which source timestamp each grid cell shows. The cheap way to survey footage and find the good parts; then pass `cell` (same range and grid) to see one cell at full detail, or call get_frame — with a `region` to zoom — to inspect a promising moment, and add_clip_to_timeline / cut_clip to use it."
    )]
    async fn skim_asset(
        &self,
        Parameters(p): Parameters<SkimParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let (columns, rows, cell_width) = sheet_grid(p.columns, p.rows, p.cell_width);
        let project = self.project.clone();
        if let Some(cell) = p.cell {
            return self.skim_cell(id, &p, (columns, rows, cell), context).await;
        }
        let cancel = context.ct.clone();
        let (jpeg, times) = blocking(move || {
            // Resolve under the lock, sample the (columns × rows) frames with it
            // released — a contact sheet is many seeks and would otherwise freeze
            // the GUI's commands on the shared mutex for its whole build. A dropped
            // request stops the sampling.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_contact_sheet_cancellable(&asset, p.start, p.end, columns, rows, cell_width, 5, &|| {
                cancel.is_cancelled()
            })
            .map_err(core_err)
        })
        .await?;
        let index = times
            .iter()
            .enumerate()
            .map(|(i, t)| format!("  cell {}: {}", i + 1, fmt_ts(*t)))
            .collect::<Vec<_>>()
            .join("\n");
        let caption = format!("contact sheet {columns}x{rows} (row-major), cell -> source time:\n{index}");
        Ok(image_result(caption, jpeg))
    }

    /// The `cell` form of [`Self::skim_asset`]: the moment one cell of a sheet
    /// showed, decoded on its own at full detail. The sheet is never rebuilt —
    /// the cell's timestamp is pure arithmetic over the same range and grid.
    async fn skim_cell(
        &self,
        id: uuid::Uuid,
        p: &SkimParams,
        (columns, rows, cell): (u32, u32, u32),
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let cells = columns * rows;
        if cell < 1 || cell > cells {
            return Err(McpError::invalid_params(
                format!("cell must be 1..={cells} for a {columns}x{rows} sheet, got {cell}"),
                None,
            ));
        }
        let max_width = p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH);
        let (start, end) = (p.start, p.end);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let (jpeg, time) = blocking(move || {
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            // The same range defaults `decode_contact_sheet` applies, so the cell
            // lands on the frame the sheet showed.
            let start = start.unwrap_or(0.0).max(0.0);
            let end = end.unwrap_or(asset.duration).min(asset.duration).max(start);
            let time = kerf_core::contact_sheet_times(start, end, columns, rows)[(cell - 1) as usize];
            let jpeg = Project::decode_preview_region_cancellable(&asset, time, Region::FULL, max_width, ZOOM_QUALITY, &|| {
                cancel.is_cancelled()
            })
            .map_err(core_err)?;
            Ok::<_, McpError>((jpeg, time))
        })
        .await?;
        Ok(image_result(
            format!(
                "cell {cell} of {columns}x{rows} sheet, asset {} @ {}",
                p.asset_id,
                fmt_ts(time)
            ),
            jpeg,
        ))
    }

    #[tool(
        description = "Render the assembled timeline at a timeline time into one composite image the model can see — the actual cut on screen at that moment (footage layered in track order, picture-in-picture placement, crop, color; gaps render black). Use to verify an edit you just made; pass `region` to zoom into a detail of it. A moment inside a fade, dip, dissolve, slide or push is drawn as the export draws it: the outgoing clip under the incoming one's ramp, the dip's fade, the clips part-way across the frame."
    )]
    async fn preview_timeline(
        &self,
        Parameters(p): Parameters<TimelineFrameParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let (time_secs, max_width) = (p.time_secs, p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH));
        let region = p.region;
        let jpeg = blocking(move || {
            // Snapshot the inputs under the lock, composite with it released —
            // mirrors the GUI's `get_timeline_frame`. A dropped request stops the render.
            let (timeline, assets) = lock_agent(&project).timeline_frame_inputs().map_err(core_err)?;
            match region {
                Some(r) => Project::composite_timeline_region_cancellable(
                    &timeline,
                    &assets,
                    time_secs,
                    r.region(),
                    max_width,
                    ZOOM_QUALITY,
                    &|| cancel.is_cancelled(),
                ),
                None => Project::composite_timeline_frame_cancellable(&timeline, &assets, time_secs, max_width, 4, &|| {
                    cancel.is_cancelled()
                }),
            }
            .map_err(core_err)
        })
        .await?;
        let mut caption = format!("timeline composite @ {}", fmt_ts(p.time_secs.max(0.0)));
        if let Some(r) = region {
            caption.push_str(&format!(", {}", r.describe()));
        }
        Ok(image_result(caption, jpeg))
    }

    #[tool(
        description = "Skim the cut you have assembled: the composited timeline at moments spread evenly across a span (default the whole cut) tiled into one contact-sheet image, plus a text index of each cell's timeline time and the clips on screen there (clip id and name, bottom track first). The one-call check of an assembled cut: pacing, black frames (a gap renders black), the same shot twice, the flow from shot to shot. Reads the cut you are proposing, staged edits included. Pass `cell` (same span and grid) to see one cell as a full frame, with an optional `region` to zoom, or call preview_timeline at any time."
    )]
    async fn skim_timeline(
        &self,
        Parameters(p): Parameters<SkimTimelineParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (columns, rows, cell_width) = sheet_grid(p.columns, p.rows, p.cell_width);
        if let Some(cell) = p.cell {
            return self.skim_timeline_cell(&p, (columns, rows, cell), context).await;
        }
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let (start, end) = (p.start, p.end);
        let (jpeg, cells, window) = blocking(move || {
            // Snapshot the inputs under the lock, composite every cell with it released. A dropped
            // request kills the stills in flight.
            let (timeline, assets) = lock_agent(&project).timeline_frame_inputs().map_err(core_err)?;
            let window = Project::timeline_sheet_window(&timeline, start, end);
            let (jpeg, cells) = Project::composite_timeline_sheet_cancellable(
                &timeline,
                &assets,
                start,
                end,
                columns,
                rows,
                cell_width,
                5,
                &|| cancel.is_cancelled(),
            )
            .map_err(core_err)?;
            Ok::<_, McpError>((jpeg, cells, window))
        })
        .await?;
        Ok(image_result(timeline_sheet_caption(columns, rows, window, &cells), jpeg))
    }

    /// The `cell` form of [`Self::skim_timeline`]: the cut at the moment one cell showed, drawn
    /// as `preview_timeline` draws it. The sheet is never rebuilt.
    async fn skim_timeline_cell(
        &self,
        p: &SkimTimelineParams,
        (columns, rows, cell): (u32, u32, u32),
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let cells = columns * rows;
        if cell < 1 || cell > cells {
            return Err(McpError::invalid_params(
                format!("cell must be 1..={cells} for a {columns}x{rows} sheet, got {cell}"),
                None,
            ));
        }
        let max_width = p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH);
        let (start, end, region) = (p.start, p.end, p.region);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let (jpeg, on_screen) = blocking(move || {
            let (timeline, assets) = lock_agent(&project).timeline_frame_inputs().map_err(core_err)?;
            if timeline.duration() <= 0.0 {
                return Err(core_err(kerf_core::Error::InvalidArgument(
                    "the timeline is empty: there is no cut to skim".to_string(),
                )));
            }
            let time = Project::timeline_sheet_times(&timeline, start, end, columns, rows)[(cell - 1) as usize];
            let jpeg = match region {
                Some(r) => Project::composite_timeline_region_cancellable(
                    &timeline,
                    &assets,
                    time,
                    r.region(),
                    max_width,
                    ZOOM_QUALITY,
                    &|| cancel.is_cancelled(),
                ),
                None => Project::composite_timeline_frame_cancellable(&timeline, &assets, time, max_width, 4, &|| {
                    cancel.is_cancelled()
                }),
            }
            .map_err(core_err)?;
            let on_screen = Project::timeline_sheet_cells(&timeline, &assets, &[time]).remove(0);
            Ok::<_, McpError>((jpeg, on_screen))
        })
        .await?;
        let mut caption = format!(
            "cell {cell} of {columns}x{rows} sheet, timeline composite @ {}: {}",
            fmt_ts(on_screen.time),
            on_screen_clips(&on_screen.clips)
        );
        if let Some(r) = p.region {
            caption.push_str(&format!(", {}", r.describe()));
        }
        Ok(image_result(caption, jpeg))
    }
}
