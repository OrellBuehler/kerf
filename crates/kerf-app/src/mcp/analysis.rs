//! Analysis, transcripts and the speech model, and syncing clips by their audio.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AnalyzeParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(
        description = "Which analyses to run: any of `silence`, `scenes`, `loudness`, `rhythm` (onsets, tempo, \
                       speech/music class), `transcript`, or `all`. Only these run, and their results are merged \
                       into what is already cached — the other kinds are left as they were. Omit it for the \
                       analyses the user has left switched on in Settings › Analysis (so an agent does not fetch \
                       a speech model for someone who turned transcription off); name `transcript` yourself to \
                       transcribe anyway. Run only what you need: `silence` before remove_silence, `rhythm` \
                       before snap_to_beats, `transcript` before generate_captions."
    )]
    steps: Option<Vec<String>>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SpeechModelParams {
    #[schemars(
        description = "whisper.cpp model name (tiny, base, small, medium, large-v3-turbo, or an `.en` variant); omit for the default"
    )]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct DownloadModelParams {
    #[schemars(
        description = "whisper.cpp model name (tiny, base, small, medium, large-v3-turbo, or an `.en` variant); omit for the default"
    )]
    name: Option<String>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SyncByAudioParams {
    #[schemars(description = "UUID of the clip that stays put: the timing reference")]
    reference_clip_id: String,
    #[schemars(description = "UUID of the clip to move into sync with the reference (its linked partners move with it)")]
    clip_id: String,
    #[schemars(
        description = "Move the clip even when the match is not reliable (confidence under 8 or distinct under 1.5). Default false: an unreliable match is reported and nothing moves"
    )]
    force: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the sync is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RefineTranscriptParams {
    #[schemars(description = "UUID of the asset whose cached transcript to re-time")]
    asset_id: String,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once; poll job_status for the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until it is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SpeakerRangesParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Only this speaker's ranges (e.g. \"S1\"); omit for every speaker")]
    speaker: Option<String>,
    #[schemars(description = "Words of one speaker closer than this many seconds are one range (default 0.75, at most 10)")]
    join_gap: Option<f64>,
    #[schemars(
        description = "Skip this many of each speaker's ranges (default 0), to page on; `more` in the answer says there are more"
    )]
    offset: Option<usize>,
    #[schemars(description = "Most ranges to return per speaker (default 200, at most 2000)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FillerWordsParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(
        description = "Words or short phrases to find, case and punctuation ignored (default: um, uh, umm, uhm, erm, er, ah, hmm, mm, you know, i mean). At most 100."
    )]
    fillers: Option<Vec<String>>,
    #[schemars(description = "Skip this many hits (default 0), to page on; `next_offset` in the answer is the next page")]
    offset: Option<usize>,
    #[schemars(description = "Most hits to return (default 200, at most 1000)")]
    limit: Option<usize>,
}

/// What `analyze_asset` returns: a summary of the cached analysis (counts, not lists — see
/// `get_transcript` / `get_analysis_detail`) with what the run did beside it.
#[derive(Serialize)]
struct AnalyzeResult {
    #[serde(flatten)]
    analysis: reads::AnalysisSummary,
    analysis_status: kerf_core::AnalysisStatus,
    /// Steps that failed, with why; the others still landed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failed: Vec<FailedStep>,
}

#[derive(Serialize)]
struct FailedStep {
    step: kerf_core::AnalysisKind,
    reason: String,
}

#[tool_router(router = tool_router_analysis, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Which analyses have run for one asset (asset_id) or for every asset: per kind (silence, scenes, \
                       loudness, rhythm, transcript) `state` is done, not_run, running, failed (with the `reason`) or \
                       off (switched off in Settings › Analysis, or no speech-to-text backend). Use it to see what \
                       analyze_asset still has to do before an edit that reads the result."
    )]
    async fn analysis_status(&self, Parameters(p): Parameters<ProxyStatusParams>) -> Result<String, McpError> {
        let wanted = p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?;
        let (project, owner) = (self.project.clone(), self.owner());
        let statuses = blocking(move || {
            // The transcription backend may need an ffmpeg start the first time it is asked: before
            // the lock, never inside it.
            crate::warm_transcription_backend();
            let project = lock_agent_as(&project, owner.as_deref());
            match wanted {
                Some(id) => {
                    project.require_asset(id).map_err(core_err)?;
                    Ok(vec![project.analysis_status(id).map_err(core_err)?])
                }
                None => project.analysis_statuses().map_err(core_err),
            }
        })
        .await?;
        json(&statuses)
    }

    #[tool(
        description = "Which speech-to-text backend this build of Kerf will use for transcription, and whether its model is downloaded yet. Call this when an asset's transcript comes back empty: it says whether transcription is unavailable (and why), or simply that the first run still has to fetch a model."
    )]
    fn transcription_status(&self) -> Result<String, McpError> {
        json(&kerf_core::transcription_status())
    }

    #[tool(
        description = "Download a speech-to-text model (whisper.cpp ggml) into Kerf's cache, so the next analyze_asset does not wait for it. Names, smallest first: tiny, tiny.en, base, base.en, small, small.en, medium, medium.en, large-v3-turbo — the plain names are multilingual, `.en` ones are English-only but more accurate on English. Bigger is slower and more accurate; `base` is the default. This only fills the cache: transcription keeps using whichever model is *selected*, so to transcribe with a model you name here, call set_speech_model too. The download is 75 MB to 1.6 GB and a no-op if the model is already cached; send a `progressToken` for progress (Kerf's own download bar moves too), cancel the request to stop it, or pass `background: true` and poll job_status."
    )]
    async fn download_speech_model(
        &self,
        Parameters(p): Parameters<DownloadModelParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let name = p.name.unwrap_or_else(|| kerf_core::DEFAULT_SPEECH_MODEL.to_string());
        let known = kerf_core::transcription_status().models;
        if !known.iter().any(|m| m.name == name) {
            return Err(McpError::invalid_params(
                format!(
                    "unknown speech model '{name}'; expected one of: {}",
                    known.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
                ),
                None,
            ));
        }
        let app = self.app.clone();
        // The GUI's Cancel on the model download ends an agent's download too.
        let gui_stop = app.state::<crate::AppState>().begin_model_download();
        self.long_tool("download_speech_model", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let model = name.clone();
            let path = blocking(move || {
                let mut on_progress = |d: kerf_core::DownloadProgress| {
                    let fraction = d.fraction();
                    // The same event the GUI's own download streams, so its progress bar moves too.
                    let _ = app.emit(
                        "model-progress",
                        serde_json::json!({
                            "model": model,
                            "downloaded_bytes": d.downloaded,
                            "total_bytes": d.total,
                            "fraction": fraction,
                        }),
                    );
                    let mb = |bytes: u64| bytes as f64 / 1_048_576.0;
                    run.progress(
                        fraction,
                        match d.total {
                            Some(total) => format!("downloading {model}: {:.0} of {:.0} MB", mb(d.downloaded), mb(total)),
                            None => format!("downloading {model}: {:.0} MB", mb(d.downloaded)),
                        },
                    );
                };
                kerf_core::download_speech_model_cancellable(&model, &mut on_progress, &|| run.cancelled()).map_err(core_err)
            })
            .await?;
            json(&serde_json::json!({ "model": name, "path": path.to_string_lossy() }))
        })
        .await
    }

    #[tool(
        description = "Analyze an asset and cache the result: silence, scene changes, EBU R128 loudness, rhythm \
                       (onsets, tempo/beat grid, speech-vs-music class) and speech-to-text transcript. Pass `steps` to \
                       run only some of them — the results are merged into what is cached and the other kinds are left \
                       alone, so run just what the next edit needs. Omit `steps` for the analyses the user has \
                       switched on in Settings. A cancelled or failed step caches nothing; a step that fails does not \
                       stop the others (see `failed` and `analysis_status` in the result). Transcribing downloads a \
                       speech model on first use (~148 MB) and then runs for a good fraction of the media's duration, so \
                       expect that step to be slow; see transcription_status if the transcript comes back empty. A \
                       preview proxy that is queued is built first. Send a `progressToken` for progress (the step and how \
                       far it is), cancel the request — or press Stop in Kerf — to stop it, or pass `background: true` and \
                       poll job_status. The result is a SUMMARY (counts, durations, the head of the transcript), not the \
                       lists: read those with get_transcript and get_analysis_detail."
    )]
    async fn analyze_asset(
        &self,
        Parameters(p): Parameters<AnalyzeParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let steps = crate::parse_steps(p.steps).map_err(|e| McpError::invalid_params(e, None))?;
        // Nothing to run (everything switched off, a silent file asked for its speech) is a bad
        // request, said so before anything starts.
        let generation = {
            let project = self.lock();
            let asset = project.require_asset(id).map_err(core_err)?;
            kerf_core::resolve_steps(&asset, steps.as_deref()).map_err(core_err)?;
            project.generation()
        };
        // The GUI's Stop button stops an agent's analysis too: this run polls the same stop the
        // GUI's own does (`begin_analysis`), and a Stop pressed before it began does not end it.
        let gui_stop = self.app.state::<crate::AppState>().begin_analysis();
        let (me, owner) = (self.clone(), self.owner());
        self.long_tool("analyze_asset", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let outcome = blocking(move || {
                // Heavy ffmpeg analysis with the lock released, re-locking only to merge each
                // finished step into the cache (`run_analysis`) — so analysis doesn't freeze the
                // GUI or stall other tools for its whole (multi-second) duration.
                let _progress = AnalysisProgressRelay::new(&app, id, &run);
                let outcome = crate::run_analysis(
                    &app,
                    &|| lock_agent_as(&project, owner.as_deref()),
                    id,
                    steps.as_deref(),
                    &|| run.cancelled(),
                )
                .map_err(|e| McpError::internal_error(e, None))?;
                // Whatever finished was merged as it did; a project swapped in meanwhile is not
                // told it was analysed.
                require_generation(
                    &lock_agent_as(&project, owner.as_deref()),
                    generation,
                    "it was being analysed",
                )?;
                Ok(outcome)
            })
            .await?;
            me.changed();
            json(&AnalyzeResult {
                analysis: reads::AnalysisSummary::of(&outcome.analysis),
                analysis_status: outcome.status,
                failed: outcome
                    .failed
                    .into_iter()
                    .map(|(step, reason)| FailedStep { step, reason })
                    .collect(),
            })
        })
        .await
    }

    #[tool(
        description = "Pick the speech-to-text model transcription uses, remembered in the project (omit the name to \
                       go back to the default). Same names as download_speech_model. Selecting a model does not \
                       download it — the next transcription fetches it if the cache is cold, or call \
                       download_speech_model first to get that wait out of the way. Returns the resulting \
                       transcription status."
    )]
    fn set_speech_model(&self, Parameters(p): Parameters<SpeechModelParams>) -> Result<String, McpError> {
        let name = p.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
        // A name the engine reads as a model file would ask the disk about a path outside the workspace.
        if let (Some(ws), Some(name)) = (&self.workspace, name) {
            ws.check_speech_model(name).map_err(|e| McpError::invalid_params(e, None))?;
        }
        kerf_core::set_speech_model(name);
        // Both writes the GUI's picker makes: the process setting transcription
        // reads, and the project meta that restores the choice on reopen.
        self.lock()
            .set_meta(crate::SPEECH_MODEL_KEY, name.unwrap_or(""))
            .map_err(core_err)?;
        // The GUI reads the transcription status once, at launch, so it has to be
        // told the choice moved or its picker shows the old model until the next
        // start. Deliberately not `project-changed`: that re-fetches the timeline,
        // the history and the task queue, and none of those moved.
        self.notify("speech-model-changed");
        json(&kerf_core::transcription_status())
    }

    #[tool(
        description = "When each speaker talks in an asset, from its speaker-labelled transcript: per speaker (S1, S2, … \
                       in order of first appearance) the SOURCE-time ranges of their words, joined across pauses \
                       shorter than join_gap, and their total speaking time. Use it to cut to one speaker (keep or \
                       remove their ranges with cut_clip_range / add_clip, which take source times) or to find who \
                       talks most. Ranges go by each word's own label, so a line get_transcript labels S1 (most of \
                       its words) can still lend a word to S2's ranges. Labels come from clustering voices without a \
                       model: distinct voices in clean turns separate well; overlapping speech or similar voices may \
                       not. At most `limit` ranges per speaker (default 200) from `offset`; `total_ranges` says how many \
                       there are and `more` whether any speaker has more. Errors if the transcript has no speaker \
                       labels (refine_transcript adds them)."
    )]
    fn speaker_ranges(&self, Parameters(p): Parameters<SpeakerRangesParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let join_gap = p.join_gap.filter(|g| g.is_finite()).unwrap_or(0.75).clamp(0.0, 10.0);
        let speaker = p.speaker.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let (offset, limit) = reads::page_of(p.offset, p.limit, 200, 2000);
        let ranges = self.lock().speaker_ranges(id, speaker, join_gap).map_err(core_err)?;
        // Each speaker's ranges are windowed alike: a long recording joins into hundreds.
        let more = ranges.iter().any(|s| offset.saturating_add(limit) < s.ranges.len());
        let speakers: Vec<serde_json::Value> = ranges
            .iter()
            .map(|s| {
                serde_json::json!({
                    "speaker": s.speaker,
                    "speaking": s.speaking,
                    "total_ranges": s.ranges.len(),
                    "ranges": s.ranges.iter().skip(offset).take(limit).collect::<Vec<_>>(),
                })
            })
            .collect();
        json(&serde_json::json!({ "speakers": speakers, "offset": offset, "limit": limit, "more": more }))
    }

    #[tool(
        description = "Find filler words in an asset's transcript by their word bounds: each hit's text, SOURCE-time \
                       start/end, the index of its line, and `estimated: true` when the word times were estimated \
                       rather than reported. Speech models often leave fillers out of a transcript, so few hits does \
                       not prove the speech has none. At most `limit` hits (default 200) from `offset`, with `total` \
                       and `next_offset`. Remove one with cut_clip_range (from/to = its start/end) on the \
                       clip that shows it; cut the latest first so earlier times do not shift."
    )]
    fn find_filler_words(&self, Parameters(p): Parameters<FillerWordsParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let fillers = p.fillers.unwrap_or_default();
        if fillers.len() > 100 {
            return Err(McpError::invalid_params("at most 100 fillers".to_string(), None));
        }
        let (offset, limit) = reads::page_of(p.offset, p.limit, 200, 1000);
        let found = self.lock().filler_words(id, &fillers).map_err(core_err)?;
        let items: Vec<_> = found.iter().skip(offset).take(limit).collect();
        let end = offset.saturating_add(items.len());
        json(&serde_json::json!({
            "total": found.len(),
            "offset": offset,
            "returned": items.len(),
            "next_offset": (end < found.len()).then_some(end),
            "fillers": items,
        }))
    }

    #[tool(
        description = "Re-time the words of an asset's cached transcript to the speech energy and label its speakers \
                       (S1, S2, …), without re-transcribing — for a transcript made before Kerf did this; a \
                       transcription already does it. No model is involved: it decodes the asset's audio once, as \
                       background work that waits behind exports and preview proxies, and the user can stop it like \
                       an analysis (or cancel the request, or pass `background: true` for a job_id). Refused if a \
                       newer transcript landed while it ran. Returns how many lines, words and speakers the \
                       transcript has now."
    )]
    async fn refine_transcript(
        &self,
        Parameters(p): Parameters<RefineTranscriptParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on an analysis ends this too (`begin_analysis`).
        let gui_stop = self.app.state::<crate::AppState>().begin_analysis();
        self.long_tool("refine_transcript", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let summary = blocking(move || {
                let stored = crate::run_refine(&app, &|| lock_agent_as(&project, owner.as_deref()), id, &|| run.cancelled())
                    .map_err(|e| McpError::internal_error(e, None))?;
                let mut speakers: Vec<&str> = stored.transcript.iter().filter_map(|s| s.speaker.as_deref()).collect();
                speakers.sort_unstable();
                speakers.dedup();
                Ok(serde_json::json!({
                    "segments": stored.transcript.len(),
                    "words": stored.transcript.iter().map(|s| s.words.len()).sum::<usize>(),
                    "speakers": speakers,
                }))
            })
            .await?;
            json(&summary)
        })
        .await
    }

    #[tool(
        description = "Sync a clip to another by their sound: decodes both clips' audio, finds the offset where \
                       they match (GCC-PHAT cross-correlation, to a fraction of a millisecond) and moves the clip \
                       so the same moment lands at the same timeline time — a camera's scratch audio against the \
                       recorder, two cameras on one stage. Only each clip's used source window (source_in..source_out, \
                       at most 20 minutes) is compared, so trim both to the stretch they share for a fast, clean \
                       match. Both clips need sound and normal forward speed, must not be linked to each other or \
                       share a track, and the moving clip's track must be unlocked — checked before anything is \
                       decoded. Send a `progressToken` to hear when it waits for another job; cancelling the request \
                       stops it, or pass `background: true` for a job. Returns `offset` (seconds the clip moved; positive is later), \
                       `timeline_start`, `confidence` (peak over noise, >= 8 is clear), `distinct` (peak over the \
                       next-best match, >= 1.5 is unambiguous — music can match at several offsets; null when nothing \
                       else matched at all), `reliable`, \
                       `inverted` (one recording has flipped polarity) and `applied`. An unreliable match does not \
                       move anything unless force is true."
    )]
    async fn sync_clips_by_audio(
        &self,
        Parameters(p): Parameters<SyncByAudioParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let (reference, clip) = (
            parse_arg("reference_clip_id", &p.reference_clip_id)?,
            parse_arg("clip_id", &p.clip_id)?,
        );
        let force = p.force.unwrap_or(false);
        let (me, owner) = (self.clone(), self.owner());
        self.long_tool("sync_clips_by_audio", p.background, &context, move |run| async move {
            let project = me.project.clone();
            let sync = blocking(move || {
                // Resolve under the lock, decode and correlate without it, move under it again.
                let (plan, generation) = {
                    let guard = lock_agent_as(&project, owner.as_deref());
                    (
                        guard.audio_sync_inputs(reference, clip).map_err(core_err)?,
                        guard.generation(),
                    )
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let measured =
                    Project::measure_audio_sync(&plan, &mut |w| run.progress(None, w.message()), &stop).map_err(core_err)?;
                // The sync moves a clip: it stages like any agent edit.
                let guard = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&guard, generation, "the clips were being synced")?;
                guard.apply_audio_sync(&measured, force).map_err(core_err)
            })
            .await?;
            if sync.applied {
                me.changed();
            }
            json(&sync)
        })
        .await
    }
}

/// Forwards the `analysis-progress` events the GUI's bin listens to — for one asset — to a [`Run`],
/// so an agent hears what the user sees: the step and how far it is. Stops listening when dropped.
struct AnalysisProgressRelay {
    app: Shell,
    id: tauri::EventId,
}

impl AnalysisProgressRelay {
    fn new(app: &Shell, asset: Uuid, run: &Run) -> Self {
        let (run, asset) = (run.clone(), asset.to_string());
        let id = app.listen("analysis-progress", move |payload| {
            let Ok(p) = serde_json::from_str::<serde_json::Value>(payload) else {
                return;
            };
            if p["asset_id"] != asset.as_str() {
                return;
            }
            let stage = p["stage"].as_str().unwrap_or("analysing");
            let message = match p["detail"].as_str() {
                Some(detail) => format!("{stage}: {detail}"),
                None => stage.to_string(),
            };
            run.progress(p["fraction"].as_f64(), message);
        });
        Self { app: app.clone(), id }
    }
}

impl Drop for AnalysisProgressRelay {
    fn drop(&mut self) {
        self.app.unlisten(self.id);
    }
}
