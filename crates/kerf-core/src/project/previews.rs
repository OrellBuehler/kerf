//! Reads of the media for a preview: frames, sheets, waveforms, proxies, levels and the checks on a cut.

use super::*;

impl Project {
    // ---- media extraction (preview frames, waveforms) ---------------------

    /// Decode a single frame of an asset at `time_secs` as PNG bytes, scaled to
    /// at most `max_width` px wide.
    pub fn frame_at(&self, asset_id: Uuid, time_secs: f64, max_width: u32) -> Result<Vec<u8>> {
        let asset = self.require_asset(asset_id)?;
        // A still image has one frame at t=0; seeking past it decodes nothing.
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        engine::frame_at(Path::new(&asset.path), time_secs, max_width)
    }

    /// Decode a single frame of an asset at `time_secs` as JPEG bytes (`quality`
    /// = ffmpeg `-q:v`, 2 = best … 31 = worst), scaled to at most `max_width` px
    /// wide. Smaller than [`frame_at`]'s PNG — for handing the frame to an LLM.
    pub fn frame_jpeg(&self, asset_id: Uuid, time_secs: f64, max_width: u32, quality: u8) -> Result<Vec<u8>> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_preview_frame(&asset, time_secs, max_width, quality, true)
    }

    /// Decode a preview frame for an already-resolved [`Asset`] as JPEG bytes,
    /// *without* needing `&self` — so the caller can release the project lock
    /// before the (potentially slow) ffmpeg decode runs, instead of freezing
    /// every other project op for its duration. `accurate = false` snaps to the
    /// nearest keyframe for fast scrubbing; a still decodes its one frame at t=0.
    /// [`Project::decode_preview_frame`] zoomed into `region` (fractions of
    /// the frame). Reads the **original** source rather than the 1280-wide
    /// proxy: a zoom is a request for the pixels the proxy threw away.
    pub fn decode_preview_region(
        asset: &Asset,
        time_secs: f64,
        region: engine::Region,
        max_width: u32,
        quality: u8,
    ) -> Result<Vec<u8>> {
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        engine::frame_jpeg_region(Path::new(&asset.path), time_secs, region, max_width, quality, true)
    }

    pub fn decode_preview_frame(asset: &Asset, time_secs: f64, max_width: u32, quality: u8, accurate: bool) -> Result<Vec<u8>> {
        // A still image has one frame at t=0; seeking past it decodes nothing.
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        // Decode from the all-intra proxy when one is ready (every frame a
        // keyframe → the seek decodes exactly one frame); export always reads the
        // original — only previews consult the proxy.
        let path = Self::preview_source(asset);
        engine::frame_jpeg(&path, time_secs, max_width, quality, accurate)
    }

    /// [`Project::decode_preview_frame`] for a caller that is a job someone can stop (an agent's
    /// request that was cancelled): `cancel` kills the decode and the answer is
    /// [`Error::Cancelled`]. The scrub keeps the plain one — a scrubbed frame is superseded, not
    /// cancelled.
    pub fn decode_preview_frame_cancellable(
        asset: &Asset,
        time_secs: f64,
        max_width: u32,
        quality: u8,
        accurate: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        let path = Self::preview_source(asset);
        engine::frame_jpeg_cancellable(&path, time_secs, max_width, quality, accurate, cancel)
    }

    /// [`Project::decode_preview_region`] that `cancel` can stop ([`Error::Cancelled`]).
    pub fn decode_preview_region_cancellable(
        asset: &Asset,
        time_secs: f64,
        region: engine::Region,
        max_width: u32,
        quality: u8,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        engine::frame_jpeg_region_cancellable(Path::new(&asset.path), time_secs, region, max_width, quality, true, cancel)
    }

    /// Decode a window of an asset's audio as mono s16le PCM at `sample_rate`,
    /// for the GUI's preview playback. Static like
    /// [`Project::decode_preview_frame`] so the caller can release the project
    /// lock before the ffmpeg decode runs. Always reads the original source —
    /// proxies are video-only.
    /// `effects` is the owning clip's audio chain, applied during the decode so
    /// the monitor hears its EQ / compressor / gate / denoise instead of the dry source.
    /// The chain runs *before* the Web Audio engine's volume and fade envelope,
    /// where the export runs it after the clip gain — a difference only a
    /// level-dependent effect (compressor, gate, denoise) can hear, and this is a preview
    /// monitor, not the export mix.
    pub fn decode_audio_pcm(
        asset: &Asset,
        start: f64,
        duration: f64,
        sample_rate: u32,
        effects: &[crate::model::AudioEffect],
    ) -> Result<Vec<u8>> {
        let filters = engine::audio_effects_filter(effects);
        engine::audio_pcm(Path::new(&asset.path), start, duration, sample_rate, filters.as_deref())
    }

    /// [`Project::decode_audio_pcm`] that `cancel` can stop ([`Error::Cancelled`]).
    pub fn decode_audio_pcm_cancellable(
        asset: &Asset,
        start: f64,
        duration: f64,
        sample_rate: u32,
        effects: &[crate::model::AudioEffect],
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let filters = engine::audio_effects_filter(effects);
        engine::audio_pcm_cancellable(
            Path::new(&asset.path),
            start,
            duration,
            sample_rate,
            filters.as_deref(),
            cancel,
        )
    }

    /// The media path a preview should decode for `asset`: its generated proxy
    /// when one is ready on disk, else the original source. Only the preview
    /// paths ([`Project::decode_preview_frame`] and the [`Project::timeline_frame`]
    /// compositor) consult this — export always uses the original `asset.path`.
    /// Stills and audio-only assets never get a proxy, so they resolve to the
    /// original. Falls back to the original whenever no proxy exists yet, so a
    /// preview never blocks waiting on generation.
    pub(crate) fn preview_source(asset: &Asset) -> PathBuf {
        resolve_preview_source(asset, crate::proxy::effective_preview_source(), || {
            engine::ready_proxy(Path::new(&asset.path), engine::proxy_width(asset.projection()))
        })
    }

    /// The proxy a whole-file analysis read of `asset` decodes instead of its original, when one
    /// is **already on disk** (never waited for), the preview source setting allows proxies and
    /// `faithful` accepts the file's [`engine::ProxyFidelity`]; `None` reads the original. A proxy
    /// answers `-ss T` with the frame the original does, so what a read finds *when* comes out
    /// the same, from a small all-intra decode in place of a 4K long-GOP one. Probes the file
    /// (cached), so not under a lock that other work waits on.
    fn analysis_proxy(asset: &Asset, faithful: impl FnOnce(engine::ProxyFidelity) -> bool) -> Option<PathBuf> {
        let original = Path::new(&asset.path);
        resolve_analysis_proxy(
            asset,
            crate::proxy::effective_preview_source(),
            || engine::ready_proxy(original, engine::proxy_width(asset.projection())),
            || faithful(engine::proxy_fidelity(original)),
        )
    }

    /// [`Project::analysis_proxy`] for scene detection, which scores frames: only where the
    /// proxy's scores are the original's (an SDR MP4 / MOV / Matroska source).
    pub(crate) fn scoring_proxy(asset: &Asset) -> Option<PathBuf> {
        Self::analysis_proxy(asset, |f| f.scores)
    }

    /// [`Project::analysis_proxy`] for a contact sheet, which picks frames by time: any source
    /// whose proxy keeps the original's times (an HDR one is tone-mapped the same either way).
    pub(crate) fn sheet_proxy(asset: &Asset) -> Option<PathBuf> {
        Self::analysis_proxy(asset, |f| f.times)
    }

    // ---- proxies ------------------------------------------------------------

    /// Whether the user deleted `asset_id`'s proxy on purpose (so it is not built
    /// again by itself until they rebuild it). Kept in the project, a list of ids.
    pub fn proxy_declined(&self, asset_id: Uuid) -> Result<bool> {
        Ok(self.proxy_declined_ids()?.contains(&asset_id))
    }

    fn proxy_declined_ids(&self) -> Result<std::collections::HashSet<Uuid>> {
        let stored = self.meta(PROXY_DECLINED_KEY)?.unwrap_or_default();
        let ids: Vec<Uuid> = serde_json::from_str(&stored).unwrap_or_default();
        Ok(ids.into_iter().collect())
    }

    /// Record that the user does (or no longer does) want `asset_id` to go without a
    /// proxy. Not an edit: no revision, and nothing in the cut moves.
    pub fn set_proxy_declined(&self, asset_id: Uuid, declined: bool) -> Result<()> {
        let mut ids = self.proxy_declined_ids()?;
        if declined == ids.contains(&asset_id) {
            return Ok(());
        }
        if declined {
            ids.insert(asset_id);
        } else {
            ids.remove(&asset_id);
        }
        let mut ids: Vec<Uuid> = ids.into_iter().collect();
        ids.sort();
        self.set_meta(PROXY_DECLINED_KEY, &serde_json::to_string(&ids)?)
    }

    /// What every asset's proxy status is worked out from — resolved under the
    /// project lock, then handed to [`crate::proxy::statuses`] with it released (the
    /// status touches the disk and may run the cached `ffprobe`).
    pub fn proxy_inputs(&self) -> Result<Vec<crate::proxy::ProxyInput>> {
        let declined = self.proxy_declined_ids()?;
        Ok(self
            .list_assets()?
            .into_iter()
            .map(|asset| crate::proxy::ProxyInput {
                declined: declined.contains(&asset.id),
                asset,
            })
            .collect())
    }

    /// [`Project::proxy_inputs`] for one asset.
    pub fn proxy_input(&self, asset_id: Uuid) -> Result<crate::proxy::ProxyInput> {
        Ok(crate::proxy::ProxyInput {
            asset: self.require_asset(asset_id)?,
            declined: self.proxy_declined(asset_id)?,
        })
    }

    /// The clips of the cut in `from..to` (to the end when `to` is `None`) that a
    /// preview could not be made of yet, because the preview source is **Proxy only** and
    /// their proxy is not ready. Empty under any other source. The GUI's preview
    /// commands refuse to decode the original for these; an agent's reads do not ask.
    pub fn proxy_waits(&self, from: f64, to: Option<f64>) -> Result<Vec<crate::proxy::ProxyWait>> {
        self.proxy_waits_under(crate::proxy::effective_preview_source(), from, to)
    }

    pub(super) fn proxy_waits_under(
        &self,
        source: crate::proxy::PreviewSource,
        from: f64,
        to: Option<f64>,
    ) -> Result<Vec<crate::proxy::ProxyWait>> {
        if source != crate::proxy::PreviewSource::ProxyOnly {
            return Ok(Vec::new());
        }
        let timeline = self.working_timeline()?;
        Self::proxy_waits_in(&timeline, source, from, to, |id| self.proxy_input(id))
    }

    /// [`Project::proxy_waits`] over a timeline and proxy inputs already in hand, so a caller
    /// holding both (the GPU preview's snapshot of a frame's inputs) asks without the project
    /// lock. `input` answers for an asset id; the status is read now, from the proxy queue.
    pub fn proxy_waits_in(
        timeline: &Timeline,
        source: crate::proxy::PreviewSource,
        from: f64,
        to: Option<f64>,
        mut input: impl FnMut(Uuid) -> Result<crate::proxy::ProxyInput>,
    ) -> Result<Vec<crate::proxy::ProxyWait>> {
        if source != crate::proxy::PreviewSource::ProxyOnly {
            return Ok(Vec::new());
        }
        let mut wanted: Vec<Uuid> = Vec::new();
        for track in timeline.tracks.iter().filter(|t| t.kind == StreamKind::Video) {
            for clip in &track.clips {
                let reaches = clip.timeline_end() > from && to.is_none_or(|to| clip.timeline_start <= to);
                if reaches && !clip.is_generated() && !wanted.contains(&clip.asset_id) {
                    wanted.push(clip.asset_id);
                }
            }
        }
        let mut waits = Vec::new();
        for id in wanted {
            let input = input(id)?;
            if crate::proxy::needs_proxy(&input.asset) && Self::preview_source(&input.asset) == Path::new(&input.asset.path) {
                waits.push(crate::proxy::ProxyWait {
                    asset_id: id,
                    name: input.asset.name.clone(),
                    status: crate::proxy::status(&input),
                });
            }
        }
        Ok(waits)
    }

    /// Build a `columns`×`rows` contact sheet of an asset — frames sampled evenly
    /// across `[start, end)` (defaulting to the whole asset) tiled into one JPEG,
    /// each cell `cell_width` px wide. Returns the montage bytes and the row-major
    /// per-cell timestamps, so an LLM can skim the footage and name good moments.
    #[allow(clippy::too_many_arguments)]
    pub fn skim_asset(
        &self,
        asset_id: Uuid,
        start: Option<f64>,
        end: Option<f64>,
        columns: u32,
        rows: u32,
        cell_width: u32,
        quality: u8,
    ) -> Result<(Vec<u8>, Vec<f64>)> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_contact_sheet(&asset, start, end, columns, rows, cell_width, quality)
    }

    /// Build the contact sheet for an already-resolved [`Asset`], *without*
    /// `&self` — so the caller can release the project lock before the
    /// (many-seek) ffmpeg sampling runs. See [`Project::skim_asset`]. Reads the asset's
    /// ready proxy when it has one ([`Project::sheet_proxy`]): the same moments, from a
    /// small all-intra decode instead of a 4K long-GOP one.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_contact_sheet(
        asset: &Asset,
        start: Option<f64>,
        end: Option<f64>,
        columns: u32,
        rows: u32,
        cell_width: u32,
        quality: u8,
    ) -> Result<(Vec<u8>, Vec<f64>)> {
        Self::decode_contact_sheet_cancellable(asset, start, end, columns, rows, cell_width, quality, &|| false)
    }

    /// [`Project::decode_contact_sheet`] that `cancel` can stop ([`Error::Cancelled`]): a request
    /// the agent dropped does not leave a many-seek sampling running, and a stop is not retried
    /// on the original.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_contact_sheet_cancellable(
        asset: &Asset,
        start: Option<f64>,
        end: Option<f64>,
        columns: u32,
        rows: u32,
        cell_width: u32,
        quality: u8,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(Vec<u8>, Vec<f64>)> {
        let start = start.unwrap_or(0.0).max(0.0);
        let end = end.unwrap_or(asset.duration).min(asset.duration).max(start);
        if let Some(proxy) = Self::sheet_proxy(asset) {
            match engine::contact_sheet_cancellable(&proxy, start, end, columns, rows, cell_width, quality, cancel) {
                Ok(sheet) => return Ok(sheet),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => tracing::warn!(error = %e, "contact sheet could not read the proxy; reading the original"),
            }
        }
        engine::contact_sheet_cancellable(Path::new(&asset.path), start, end, columns, rows, cell_width, quality, cancel)
    }

    /// Composite a single still of the current timeline at timeline time `t` as
    /// JPEG bytes (`quality` = ffmpeg `-q:v`), the canvas at most `max_width` px
    /// wide — what the edit looks like on screen at `t`, for an LLM to review.
    pub fn timeline_frame(&self, time_secs: f64, max_width: u32, quality: u8) -> Result<Vec<u8>> {
        let (timeline, assets) = self.timeline_frame_inputs()?;
        Self::composite_timeline_frame(&timeline, &assets, time_secs, max_width, quality)
    }

    /// The owned inputs the timeline-frame compositor needs (timeline + the
    /// proxy-swapped preview asset list), resolved together so a caller can pull
    /// them out under the project lock and then **drop the guard** before running
    /// the slow ffmpeg composite — see [`Project::composite_timeline_frame`].
    pub fn timeline_frame_inputs(&self) -> Result<(Timeline, Vec<Asset>)> {
        Ok((self.working_timeline()?, self.preview_assets()?))
    }

    /// Composite a timeline still from already-resolved inputs, **without**
    /// `&self` — so the GUI preview (which fetches frames continuously during
    /// playback) can release the shared project lock before this ffmpeg decode,
    /// instead of freezing every other op for its duration. Mirrors
    /// [`Project::decode_preview_frame`]'s lock-free shape for single frames.
    pub fn composite_timeline_frame(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        max_width: u32,
        quality: u8,
    ) -> Result<Vec<u8>> {
        Self::composite_timeline_frame_cancellable(timeline, assets, time_secs, max_width, quality, &|| false)
    }

    /// [`Project::composite_timeline_frame`] that `cancel` can stop ([`Error::Cancelled`]).
    pub fn composite_timeline_frame_cancellable(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        max_width: u32,
        quality: u8,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        // Hardware-accelerated decode like the single-frame path (with the same
        // learned software fallback inside the engine) — this runs continuously
        // while the user scrubs.
        let opts = engine::ExportOptions {
            hwaccel: engine::decode_hwaccel(),
            ..engine::ExportOptions::default()
        };
        engine::timeline_frame_cancellable(timeline, assets, &opts, time_secs, max_width, quality, cancel)
    }

    /// [`Project::composite_timeline_frame`] zoomed into `region` of the
    /// composited canvas — the same lock-free shape, for an agent checking a
    /// detail of the cut (a caption against the safe area, a mask edge).
    pub fn composite_timeline_region(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        region: engine::Region,
        max_width: u32,
        quality: u8,
    ) -> Result<Vec<u8>> {
        Self::composite_timeline_region_cancellable(timeline, assets, time_secs, region, max_width, quality, &|| false)
    }

    /// [`Project::composite_timeline_region`] that `cancel` can stop ([`Error::Cancelled`]).
    #[allow(clippy::too_many_arguments)]
    pub fn composite_timeline_region_cancellable(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        region: engine::Region,
        max_width: u32,
        quality: u8,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let opts = engine::ExportOptions {
            hwaccel: engine::decode_hwaccel(),
            ..engine::ExportOptions::default()
        };
        engine::timeline_frame_region_cancellable(timeline, assets, &opts, time_secs, region, max_width, quality, cancel)
    }

    /// The span of the cut a sheet of it covers: `[start, end)` clamped into `[0, duration]`,
    /// defaulting to the whole cut. [`Project::timeline_sheet_times`] and a cell of a sheet both
    /// read it, so a cell lands on the moment the sheet showed.
    pub fn timeline_sheet_window(timeline: &Timeline, start: Option<f64>, end: Option<f64>) -> (f64, f64) {
        let duration = timeline.duration();
        let start = start.unwrap_or(0.0).clamp(0.0, duration);
        let end = end.unwrap_or(duration).min(duration).max(start);
        (start, end)
    }

    /// The timeline time each cell of a `columns`×`rows` sheet of the cut shows, row-major
    /// ([`engine::contact_sheet_times`] over [`Project::timeline_sheet_window`]).
    pub fn timeline_sheet_times(timeline: &Timeline, start: Option<f64>, end: Option<f64>, columns: u32, rows: u32) -> Vec<f64> {
        let (start, end) = Self::timeline_sheet_window(timeline, start, end);
        engine::contact_sheet_times(start, end, columns, rows)
    }

    /// A contact sheet of the cut from already-resolved inputs ([`Project::timeline_frame_inputs`],
    /// so the preview's proxies), **without** `&self`: the composited still at each of
    /// `columns`×`rows` moments across `[start, end)` (the whole cut by default), `cell_width` px
    /// wide, tiled into one JPEG, with each cell's time and the clips on screen there. A gap is
    /// black. `cancel` stops it ([`Error::Cancelled`]).
    #[allow(clippy::too_many_arguments)]
    pub fn composite_timeline_sheet_cancellable(
        timeline: &Timeline,
        assets: &[Asset],
        start: Option<f64>,
        end: Option<f64>,
        columns: u32,
        rows: u32,
        cell_width: u32,
        quality: u8,
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> Result<(Vec<u8>, Vec<engine::SheetCell>)> {
        if timeline.duration() <= 0.0 {
            return Err(Error::InvalidArgument(
                "the timeline is empty: there is no cut to skim".to_string(),
            ));
        }
        let opts = engine::ExportOptions {
            hwaccel: engine::decode_hwaccel(),
            ..engine::ExportOptions::default()
        };
        let times = Self::timeline_sheet_times(timeline, start, end, columns, rows);
        let jpeg = engine::timeline_sheet_cancellable(timeline, assets, &opts, &times, columns, cell_width, quality, cancel)?;
        Ok((jpeg, engine::timeline_sheet_cells(timeline, assets, &opts, &times)))
    }

    /// The clips on screen at each of `times`, bottom to top: what a sheet's caption, or one
    /// cell's, names.
    pub fn timeline_sheet_cells(timeline: &Timeline, assets: &[Asset], times: &[f64]) -> Vec<engine::SheetCell> {
        engine::timeline_sheet_cells(timeline, assets, &engine::ExportOptions::default(), times)
    }

    /// How ready the current cut is for each publishing target — what a platform
    /// would reject, what it would accept and then quietly under-distribute, and
    /// what would simply be better — with the cut it was judged on.
    ///
    /// Reads the **working** timeline, so an agent assembling a cut sees the
    /// verdict on its own proposal rather than on the user's live one.
    /// `frame` overrides the shape the cut is judged at — what the export dialog
    /// passes when a render is about to resize away from the project frame — and
    /// `range` the span an export would write (the dialog's In → out), so a Reel
    /// cut from the middle of a long timeline is judged at the Reel's length.
    pub fn platform_report(
        &self,
        frame: Option<(u32, u32)>,
        range: Option<(f64, f64)>,
    ) -> Result<crate::platform::PlatformReport> {
        Ok(crate::platform::report(&self.cut_summary_in(frame, range)?))
    }

    /// What the readiness check needs to know about the current cut.
    pub fn cut_summary(&self, frame: Option<(u32, u32)>) -> Result<crate::platform::CutSummary> {
        self.cut_summary_in(frame, None)
    }

    /// [`Project::cut_summary`] of only `range` of the cut — the span an export of that range writes.
    /// Summarised on the sliced timeline the export builds its graph from (`Timeline::slice` over the
    /// same clamp, `Timeline::export_span`), so the length, the footage-derived frame, whether anything
    /// in the span has sound and whether a title is on screen in it all describe the file, not the cut
    /// around it.
    pub fn cut_summary_in(&self, frame: Option<(u32, u32)>, range: Option<(f64, f64)>) -> Result<crate::platform::CutSummary> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        // The same gate the export applies, so a muted track is as absent here
        // as it will be in the file.
        let rendered = timeline.for_render();
        let rendered = match rendered.export_span(range) {
            Some((start, end)) => rendered.slice(start, end),
            None => rendered,
        };
        let (width, height) = frame.unwrap_or_else(|| engine::delivery_frame(&rendered, &assets));
        // Audio-bearing means what the export means by it: any clip whose asset
        // carries an audio stream, on a video track as much as an audio one.
        // A picture clip whose sound was detached adds none (its audio clip does).
        let has_audio = rendered
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .any(|c| c.source_audio && assets.iter().find(|a| a.id == c.asset_id).is_some_and(|a| a.has_audio()));
        Ok(crate::platform::CutSummary {
            duration: rendered.duration(),
            width,
            height,
            has_audio,
            has_text: !rendered.overlays.is_empty(),
        })
    }

    /// What a [`crate::review::review`] of the cut reads: the **working** timeline, the assets and
    /// the cached analysis of each asset the timeline uses. Resolved together so the caller can drop
    /// the lock before reviewing.
    pub fn review_inputs(&self) -> Result<crate::review::ReviewInputs> {
        let timeline = self.working_timeline()?;
        let mut analyses = HashMap::new();
        for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
            if analyses.contains_key(&clip.asset_id) {
                continue;
            }
            if let Some(analysis) = self.get_analysis(clip.asset_id)? {
                analyses.insert(clip.asset_id, analysis);
            }
        }
        Ok(crate::review::ReviewInputs {
            stabilization: self.stabilization_status_inputs(None)?,
            timeline,
            assets: self.list_assets()?,
            analyses,
        })
    }

    /// Review the working cut for craft defects ([`crate::review`]): `checks` by name (none is every
    /// check; an unknown one is an [`Error::InvalidArgument`]), over `range` of the cut.
    pub fn review_cut(&self, range: Option<(f64, f64)>, checks: &[String]) -> Result<crate::review::CutReview> {
        let checks = crate::review::Check::parse_list(checks)?;
        Ok(crate::review::review(&self.review_inputs()?, range, &checks))
    }

    /// The owned inputs a levels measurement needs: the working timeline and the
    /// **original** assets — it measures what the export would write, so no proxy
    /// swap. Resolved together so the caller can drop the project lock before
    /// [`Project::measure_levels`] runs ffmpeg over the whole cut.
    pub fn levels_inputs(&self) -> Result<(Timeline, Vec<Asset>)> {
        Ok((self.working_timeline()?, self.list_assets()?))
    }

    /// Measure how loud the cut is — the finished mix and each track — over
    /// `range` (the whole cut when `None`), **without** `&self`: the lock-free
    /// half of [`Project::levels`].
    ///
    /// One ffmpeg pass over the export's own audio graph with `ebur128` meters
    /// on every track's strip and on the finished mix, so the master reading is
    /// what the file would contain: the master fader and limiter are in it, and
    /// `loudnorm` is when asked for. Whole-file work, so it holds the heavy-job
    /// lease; `cancel` is polled and kills the pass ([`Error::Cancelled`]).
    ///
    /// A track's reading is its strip output, ahead of the duck bus and the
    /// master. A track that is muted (or shadowed by a solo) has no reading and
    /// `heard: false`.
    pub fn measure_levels(
        timeline: &Timeline,
        assets: &[Asset],
        range: Option<TimeRange>,
        loudnorm: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Levels> {
        if let Some(r) = range {
            if !r.start.is_finite() || !r.end.is_finite() || r.start < 0.0 || r.end <= r.start {
                return Err(Error::InvalidArgument("range needs 0 <= start < end, in seconds".to_string()));
            }
        }
        // A span entirely past the end would fall back to measuring the whole cut
        // (that is what a range export does with one) — and answer a question that
        // was not asked. Say so instead.
        // An empty cut is measured as one (nothing to hear), not refused.
        if let Some(why) = range
            .filter(|_| timeline.untitled_duration() > 0.0)
            .and_then(|r| timeline.range_past_the_sound(r.start))
        {
            return Err(Error::InvalidArgument(why));
        }
        let measured = engine::mix_levels(timeline, assets, range, loudnorm, cancel)?;
        let tracks = timeline
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.clips
                    .iter()
                    .any(|c| assets.iter().any(|a| a.id == c.asset_id && a.has_audio()))
            })
            .map(|(i, t)| TrackLevels {
                track_id: t.id,
                name: t.name.clone(),
                kind: t.kind,
                ducked: t.duck,
                heard: timeline.track_renders(t),
                level: measured.tracks.iter().find(|(ti, _)| *ti == i).map(|(_, l)| *l),
            })
            .collect();
        Ok(Levels::new(
            measured.duration,
            measured.master,
            tracks,
            loudnorm,
            &timeline.master,
        ))
    }

    /// [`Project::measure_levels`] over the working timeline, holding `&self`
    /// for the whole pass. Callers that share the project behind a lock use
    /// [`Project::levels_inputs`] and release it first.
    pub fn levels(&self, range: Option<TimeRange>, loudnorm: bool) -> Result<Levels> {
        let (timeline, assets) = self.levels_inputs()?;
        Self::measure_levels(&timeline, &assets, range, loudnorm, &|| false)
    }

    /// The owned inputs a **cover frame** render needs: the working timeline and
    /// the **original** assets. Deliberately not the proxy-swapped preview list
    /// — a cover is a delivered image, so it comes off the same media the export
    /// reads. Resolved together so the caller can drop the project lock before
    /// [`Project::render_still`] runs ffmpeg.
    pub fn export_still_inputs(&self) -> Result<(Timeline, Vec<Asset>)> {
        Ok((self.working_timeline()?, self.list_assets()?))
    }

    /// Write the composited frame at `time_secs` to `path` as a cover image,
    /// **without** `&self` — the lock-free half of [`Project::export_still`].
    ///
    /// `format` defaults to whatever the path's extension asks for. The frame is
    /// rendered at the project's full delivery resolution through the export
    /// graph, so the cover is a real frame of the finished video.
    pub fn render_still(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        path: impl AsRef<Path>,
        format: Option<engine::ImageFormat>,
    ) -> Result<PathBuf> {
        Self::render_still_cancellable(timeline, assets, time_secs, path, format, &|| false)
    }

    /// [`Project::render_still`] that can be stopped: `cancel` is polled while ffmpeg runs
    /// (it is killed, and the cover it was writing removed, unless this run never touched an
    /// earlier file at `path`) and the result is [`Error::Cancelled`].
    pub fn render_still_cancellable(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        path: impl AsRef<Path>,
        format: Option<engine::ImageFormat>,
        cancel: &dyn Fn() -> bool,
    ) -> Result<PathBuf> {
        let path = path.as_ref();
        let format = format.unwrap_or_else(|| engine::ImageFormat::from_path(path));
        let opts = engine::ExportOptions {
            hwaccel: engine::decode_hwaccel(),
            ..engine::ExportOptions::default()
        };
        // `-q:v 2` is the highest useful JPEG quality; a cover is re-encoded by
        // whatever platform receives it, so there is nothing to gain by saving
        // bytes here and plenty to lose.
        engine::export_still_cancellable(timeline, assets, &opts, time_secs, path, format, 2, cancel)
    }

    /// Render a cover frame for the current timeline at `time_secs`.
    pub fn export_still(&self, time_secs: f64, path: impl AsRef<Path>, format: Option<engine::ImageFormat>) -> Result<PathBuf> {
        self.check_output_path(path.as_ref())?;
        let (timeline, assets) = self.export_still_inputs()?;
        Self::render_still(&timeline, &assets, time_secs, path, format)
    }

    /// [`Project::list_assets`], but with each eligible asset's `path` swapped to
    /// its ready proxy — the asset list the timeline-preview compositor decodes
    /// from. Stream metadata (resolution / fps) is kept from the original, so the
    /// composite geometry and source-time mapping match the export exactly; only
    /// the decoded pixels come from the lighter all-intra proxy. Export reads
    /// [`Project::list_assets`] (originals) and is unaffected.
    pub(super) fn preview_assets(&self) -> Result<Vec<Asset>> {
        Ok(Self::preview_assets_of(self.list_assets()?))
    }

    /// [`Project::preview_assets`] over assets already read: each one swapped for its ready
    /// proxy where there is one. Touches the disk (is the proxy there), not the project, so it
    /// belongs outside the project lock.
    pub fn preview_assets_of(mut assets: Vec<Asset>) -> Vec<Asset> {
        for asset in &mut assets {
            let source = Self::preview_source(asset);
            if source != Path::new(&asset.path) {
                // The proxy was tone-mapped when it was encoded, so the graph
                // must not convert it again.
                *asset = asset.as_sdr_proxy();
                asset.path = source.to_string_lossy().into_owned();
                // Only a padded proxy keeps the original's lead (`media::decoded_lead`).
                for s in asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
                    s.lead = crate::media::decoded_lead(&asset.path, true, s.lead, 0.0);
                }
            }
        }
        assets
    }

    /// Reduce an asset's first audio stream to `buckets` peak magnitudes in
    /// `0.0..=1.0` for waveform rendering.
    pub fn waveform(&self, asset_id: Uuid, buckets: usize) -> Result<Vec<f32>> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_waveform(&asset, buckets)
    }

    /// Waveform peaks for an already-resolved [`Asset`], *without* `&self` — so
    /// the caller can release the project lock before the whole-file ffmpeg
    /// decode. See [`Project::waveform`].
    pub fn decode_waveform(asset: &Asset, buckets: usize) -> Result<Vec<f32>> {
        engine::waveform(Path::new(&asset.path), buckets, 8_000)
    }

    /// [`Project::decode_waveform`] that `cancel` can stop ([`Error::Cancelled`]): an agent's
    /// request that was dropped does not leave a whole-file decode running.
    pub fn decode_waveform_cancellable(asset: &Asset, buckets: usize, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
        engine::waveform_cancellable(Path::new(&asset.path), buckets, 8_000, cancel)
    }

    /// `[start, end)` **source seconds** of an asset's audio as `buckets` min/max
    /// peak pairs per channel — what the timeline draws a clip's waveform from.
    /// Served from a cached peak pyramid (one decode per file, ever), so any
    /// window at any zoom is a slice read. `buckets` is capped at
    /// [`crate::MAX_WAVEFORM_BUCKETS`]; see [`crate::WaveformRange`] for the shape.
    /// An asset with no audio stream is an `InvalidArgument`.
    pub fn waveform_range(&self, asset_id: Uuid, start: f64, end: f64, buckets: usize) -> Result<engine::WaveformRange> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_waveform_range(&asset, start, end, buckets)
    }

    /// [`Project::waveform_range`] for an already-resolved [`Asset`], *without*
    /// `&self` — so the caller can release the project lock before the first
    /// call's whole-file decode (later calls are memoized and cheap). Ungated
    /// like [`Project::decode_waveform`]: a waveform appearing is not worth
    /// queueing behind an export.
    pub fn decode_waveform_range(asset: &Asset, start: f64, end: f64, buckets: usize) -> Result<engine::WaveformRange> {
        Self::decode_waveform_range_cancellable(asset, start, end, buckets, &|| false)
    }

    /// [`Project::decode_waveform_range`] that `cancel` can stop ([`Error::Cancelled`]) — while it
    /// waits for the file's decode, or while it runs it. A stopped decode caches nothing.
    pub fn decode_waveform_range_cancellable(
        asset: &Asset,
        start: f64,
        end: f64,
        buckets: usize,
        cancel: &dyn Fn() -> bool,
    ) -> Result<engine::WaveformRange> {
        // A video-only asset has nothing to draw; say so rather than spawn an
        // ffmpeg to fail on `-map 0:a:0`. (No stream info at all is tried anyway.)
        if !asset.streams.is_empty() && !asset.streams.iter().any(|s| s.kind == StreamKind::Audio) {
            return Err(Error::InvalidArgument(format!("asset {} has no audio stream", asset.id)));
        }
        engine::waveform_range_of_cancellable(Path::new(&asset.path), start, end, buckets, cancel)
    }

    /// The ready proxy a filmstrip should decode instead of `asset`'s original, if
    /// one has already been generated — never waited for: without one the original
    /// is decoded, and the result is the same cache entry either way. (A proxy's
    /// path depends on whether the source is HDR, which can take an `ffprobe`: that
    /// is why this is resolved inside [`Project::decode_filmstrip`], off the project
    /// lock, and only when the strip is not already cached.)
    pub(super) fn filmstrip_proxy(asset: &Asset) -> Option<PathBuf> {
        let source = Self::preview_source(asset);
        (source != Path::new(&asset.path)).then_some(source)
    }

    /// The thumbnail strip of an asset's video (see [`crate::Filmstrip`] for what a
    /// thumbnail is and how to place one), from the cache when it exists, else
    /// built with one decode. Holds `&self` for the decode — a surface resolves the
    /// asset with [`Project::require_asset`] under the lock and calls
    /// [`Project::decode_filmstrip`] with it released instead. An asset with no
    /// video stream is an `InvalidArgument`.
    pub fn filmstrip(&self, asset_id: Uuid) -> Result<std::sync::Arc<engine::Filmstrip>> {
        Self::decode_filmstrip(&self.require_asset(asset_id)?)
    }

    /// [`Project::filmstrip`] for an already-resolved [`Asset`], *without*
    /// `&self` — so the caller can release the project lock before anything slow
    /// runs: the asset's ready proxy is looked up here (decoded in preference to
    /// the original), and the first call's whole-file decode follows (later calls
    /// are cached and cheap). Ungated like [`Project::decode_waveform_range`]: the
    /// timeline draws from it, and a thumbnail appearing is not worth queueing
    /// behind an export — but, being a video decode, it keeps to a couple of niced
    /// threads at every CPU budget (see `engine::filmstrip`).
    pub fn decode_filmstrip(asset: &Asset) -> Result<std::sync::Arc<engine::Filmstrip>> {
        engine::filmstrip_for(asset, || Self::filmstrip_proxy(asset))
    }

    /// Reduce an asset's first audio stream to `buckets` RMS magnitudes in
    /// `0.0..=1.0` — a perceptual energy-over-time curve. Companion to
    /// [`Self::waveform`] (which returns peaks); RMS better reflects loudness.
    pub fn energy(&self, asset_id: Uuid, buckets: usize) -> Result<Vec<f32>> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_energy(&asset, buckets)
    }

    /// Energy envelope for an already-resolved [`Asset`], *without* `&self` —
    /// lock-free like [`Project::decode_waveform`].
    pub fn decode_energy(asset: &Asset, buckets: usize) -> Result<Vec<f32>> {
        engine::energy_envelope(Path::new(&asset.path), buckets, 8_000)
    }

    /// [`Project::decode_energy`] that `cancel` can stop ([`Error::Cancelled`]).
    pub fn decode_energy_cancellable(asset: &Asset, buckets: usize, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
        engine::energy_envelope_cancellable(Path::new(&asset.path), buckets, 8_000, cancel)
    }
}
