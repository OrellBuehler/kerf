//! Export, delivery variants, cover, scopes, smart crop, stabilisation, audio sync and platform checks.

use crate::*;

// ---- export ----------------------------------------------------------------

/// The hardware (GPU) video encoders this machine's ffmpeg can actually use —
/// verified once per process with a tiny test encode. The export dialog merges
/// them into its codec choices; empty means software encoders only.
#[tauri::command]
pub(crate) async fn hw_encoders() -> CmdResult<Vec<String>> {
    // First call probes by spawning ffmpeg, so keep it off the async workers.
    blocking(|| Ok(kerf_core::hw_encoders().to_vec())).await
}

/// Render the timeline to `output_path`, streaming `export-progress`, and answer an `ExportReport`:
/// what the written file measures (container, picture, sound, and — with sound — its own integrated
/// loudness, true peak and loudness range, flagged when the true peak is over the `loudness`
/// target's ceiling). A measurement that fails says why in the report and does not fail the export.
#[tauri::command]
pub(crate) async fn export_timeline(
    app: AppHandle,
    state: State<'_, AppState>,
    output_path: String,
    options: ExportOptions,
) -> CmdResult<kerf_core::ExportReport> {
    require_user_output_path(&output_path)?;
    // Snapshot the timeline + assets under the lock, then release it before the
    // (seconds-to-minutes) ffmpeg render. Otherwise the export would hold the
    // shared Project mutex for its whole duration and freeze every other GUI
    // command and the MCP agent until it finished. What the output must not be (a source
    // file, a proxy, the project itself) is read under it too; the check, which
    // canonicalises and stats, runs without it.
    let (timeline, assets, protected) = {
        let project = state.project();
        (
            project.timeline().map_err(|e| e.to_string())?,
            project.list_assets().map_err(|e| e.to_string())?,
            project.output_path_inputs().map_err(|e| e.to_string())?,
        )
    };

    // Fresh cancel flag for this run; `cancel_export` flips it from another thread.
    let cancel = state.export_cancel.begin();
    let shared = state.project.clone();

    blocking(move || {
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        // Stream `export-progress` events ({ fraction, elapsed_secs, eta_secs }) so
        // the UI can show a bar + ETA. ffmpeg emits ~2/sec, no extra throttle needed.
        let mut on_progress = |p: kerf_core::ExportProgress| {
            let _ = app.emit("export-progress", p);
        };
        // A render that fails or is cancelled removes what it wrote (`kerf_core::discard_partial`,
        // only if this run touched the file), so no earlier export at the path is lost to it.
        // A completed one is read back (the file's streams and its own loudness): the same job, the
        // same Stop, and a measurement that fails says so in the report instead of failing the export.
        let (status, report) = kerf_core::render_measured(
            &timeline,
            &assets,
            std::path::Path::new(&output_path),
            &options,
            &mut on_progress,
            &|| cancel(),
        )
        .map_err(|e| e.to_string())?;

        // The ticks the render backfilled are kept (the lock is taken only to write them).
        if status == kerf_core::RenderStatus::Completed {
            keep_time_bases(&timeline, &assets, &options, || lock_user(&shared));
        }
        match (status, report) {
            (kerf_core::RenderStatus::Completed, Some(report)) => Ok(report),
            (kerf_core::RenderStatus::Completed, None) => Err("the export finished without a report".to_string()),
            (kerf_core::RenderStatus::Cancelled, _) => Err("export cancelled".to_string()),
        }
    })
    .await
}

/// Progress of the shot-framing pass of a multi-format export, which runs before the first
/// file renders: the same event as the render's, flagged `waiting` so the bar does not sit at 0%.
#[derive(Serialize, Clone)]
struct FramingProgress {
    variant: usize,
    total: usize,
    fraction: f64,
    elapsed_secs: f64,
    eta_secs: Option<f64>,
    waiting: &'static str,
}

/// Render the cut once per delivery frame — one file per shape beside
/// `output_path`, named by shape (`cut-9x16.mp4`). With `smart_crop`, every
/// shot is framed for every shape first (one revision, reused by later
/// exports); the project frame's own crop is never touched. `single_pass`
/// (default true) writes every file from one decode of the cut where the encode
/// allows; false renders them one after another. Streams the same
/// `export-progress` event as a single export, with `variant` / `total` (and
/// `together` for a single pass) added. Answers one `ExportReport` per file (what
/// each turned out to be, read back from the file), in the order of `formats`.
#[tauri::command]
pub(crate) async fn export_variants(
    app: AppHandle,
    state: State<'_, AppState>,
    output_path: String,
    formats: Vec<Delivery>,
    smart_crop: bool,
    single_pass: Option<bool>,
    options: ExportOptions,
) -> CmdResult<Vec<kerf_core::ExportReport>> {
    let single_pass = single_pass.unwrap_or(true);
    if formats.is_empty() {
        return Err("pick at least one delivery frame".to_string());
    }
    require_user_output_path(&output_path)?;
    let base = std::path::PathBuf::from(&output_path);
    let mut deliveries: Vec<Delivery> = Vec::new();
    for d in formats {
        let d = Delivery::new(d.width, d.height, d.fit);
        if !deliveries.contains(&d) {
            deliveries.push(d);
        }
    }
    let variants: Vec<kerf_core::ExportVariant> = deliveries
        .iter()
        .map(|d| kerf_core::ExportVariant::beside(&base, *d))
        .collect();
    for variant in &variants {
        kerf_core::check_caption_sidecars(&variant.output, &options).map_err(|e| e.to_string())?;
    }
    let shared = state.project.clone();
    let cancel = state.export_cancel.begin();

    blocking(move || {
        // None of the files may be a source, a proxy or the project itself.
        let protected = lock_user(&shared).output_path_inputs().map_err(|e| e.to_string())?;
        for variant in &variants {
            check_output(&variant.output, &protected).map_err(|e| e.to_string())?;
        }
        // Frame first — plan under the lock, sample without it, apply under it
        // again — then snapshot and render with the lock released, like a
        // single export.
        if smart_crop {
            let (plan, generation) = {
                let project = lock_user(&shared);
                (
                    project.framing_inputs(&deliveries).map_err(|e| e.to_string())?,
                    project.generation(),
                )
            };
            if !plan.jobs.is_empty() {
                let (total, started) = (variants.len(), std::time::Instant::now());
                let mut on_framed = |fraction: f32| {
                    let _ = app.emit(
                        "export-progress",
                        FramingProgress {
                            variant: 0,
                            total,
                            fraction: f64::from(fraction),
                            elapsed_secs: started.elapsed().as_secs_f64(),
                            eta_secs: None,
                            waiting: "framing the shots",
                        },
                    );
                };
                let stop = stop_or_replaced(&cancel, &shared, generation);
                let framings = Project::sample_framings_with(&plan, &mut on_framed, &stop).map_err(|e| match e {
                    kerf_core::Error::Cancelled => "export cancelled".to_string(),
                    other => other.to_string(),
                })?;
                let framed = {
                    let project = lock_user(&shared);
                    require_generation(&project, generation, "the shots were being framed")?;
                    project.apply_framings(&framings).map_err(|e| e.to_string())?
                };
                if framed > 0 {
                    let _ = app.emit("project-changed", ());
                }
            }
        }
        let (timeline, assets) = {
            let project = lock_user(&shared);
            (
                project.timeline().map_err(|e| e.to_string())?,
                project.list_assets().map_err(|e| e.to_string())?,
            )
        };
        // `render_variants` writes the files in one run (or one after another) and removes what
        // failed or was cancelled (only if this run touched it); one after another, the finished
        // ones stay. Its progress is the event the webview already reads: which file, of how
        // many (or all of them together), and the whole.
        let mut on_progress = |p: kerf_core::VariantProgress| {
            let _ = app.emit("export-progress", p);
        };
        // Each finished file is read back too (one report per file, in order).
        match kerf_core::render_variants_measured(
            &timeline,
            &assets,
            &variants,
            &options,
            single_pass,
            &mut on_progress,
            &|| cancel(),
        ) {
            Ok((run, reports)) if run.status == kerf_core::RenderStatus::Completed => {
                keep_time_bases(&timeline, &assets, &options, || lock_user(&shared));
                Ok(reports)
            }
            // A stopped single pass took every file with it; the dialog says so.
            Ok((run, _)) if run.together => Err(format!("export cancelled; all {} files were removed", variants.len())),
            Ok(_) => Err("export cancelled".to_string()),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
}

/// Write the composited frame at `time_secs` to `output_path` as a **cover
/// image** — full delivery resolution, decoded from the original media rather
/// than a preview proxy. `format` follows the file extension when omitted.
/// `cancel_cover_export` stops it, rejecting with `"cover export cancelled"`.
#[tauri::command]
pub(crate) async fn export_cover(
    state: State<'_, AppState>,
    time_secs: f64,
    output_path: String,
    format: Option<kerf_core::ImageFormat>,
) -> CmdResult<String> {
    require_user_output_path(&output_path)?;
    let shared = state.project.clone();
    let cancel = state.cover_cancel.begin();
    blocking(move || {
        // Same shape as every heavy command: resolve under the lock, render
        // without it. A 4K still is a real decode.
        let ((timeline, assets), protected) = {
            let project = lock_user(&shared);
            (
                project.export_still_inputs().map_err(|e| e.to_string())?,
                project.output_path_inputs().map_err(|e| e.to_string())?,
            )
        };
        check_output(std::path::Path::new(&output_path), &protected).map_err(|e| e.to_string())?;
        let path = Project::render_still_cancellable(&timeline, &assets, time_secs, &output_path, format, &|| cancel()).map_err(
            |e| match e {
                kerf_core::Error::Cancelled => COVER_CANCELLED.to_string(),
                other => other.to_string(),
            },
        )?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}

/// The video scopes of the composited frame at `time_secs`: the export's still of the cut (from
/// the originals) read with the delivery's matrix. The summary always; the grids in `grids`.
#[tauri::command]
pub(crate) async fn get_scopes(
    state: State<'_, AppState>,
    time_secs: f64,
    grids: Option<Vec<kerf_core::scopes::ScopeKind>>,
    matrix: Option<kerf_core::scopes::ScopeMatrix>,
    size: Option<kerf_core::scopes::GridSize>,
    max_width: Option<u32>,
) -> CmdResult<kerf_core::scopes::Scopes> {
    let shared = state.project.clone();
    blocking(move || {
        let (timeline, assets) = lock_user(&shared).export_still_inputs().map_err(|e| e.to_string())?;
        let req = kerf_core::scopes::ScopeRequest {
            time_secs,
            max_width: max_width.map(|w| w.clamp(64, kerf_core::scopes::MAX_SCOPE_WIDTH)),
            matrix: matrix.unwrap_or_default(),
            options: kerf_core::scopes::ScopeOptions {
                grids: grids.unwrap_or_default(),
                size: size.unwrap_or_default(),
            },
        };
        kerf_core::scopes::timeline_scopes(&timeline, &assets, &req).map_err(|e| e.to_string())
    })
    .await
}

/// Stop the cover frame being written (if any).
#[tauri::command(async)]
pub(crate) fn cancel_cover_export(state: State<'_, AppState>) {
    state.cover_cancel.stop();
}

/// The error an abandoned smart crop returns, for the webview to stay quiet on.
const SMART_CROP_CANCELLED: &str = "smart crop cancelled";

/// `smart-crop-progress`: how much of the shots have been sampled.
#[derive(Serialize, Clone)]
struct SmartCropProgress {
    fraction: f64,
}

/// Frame every shot for the delivery frame instead of centring it blindly —
/// samples where each clip's content sits and writes the crop that keeps it.
/// One clip when `clip_id` is given, otherwise every clip on an unlocked video
/// track. The result is an ordinary transform crop, so the inspector's sliders
/// still have the last word. Streams `smart-crop-progress` (the fraction of clips
/// sampled); `cancel_smart_crop` stops it, rejecting with `"smart crop cancelled"`.
#[tauri::command]
pub(crate) async fn smart_crop(app: AppHandle, state: State<'_, AppState>, clip_id: Option<String>) -> CmdResult<Timeline> {
    let clip = opt_id(clip_id.as_deref(), "clip_id")?;
    let shared = state.project.clone();
    let cancel = state.smart_crop_cancel.begin();
    blocking(move || {
        // The usual shape for a heavy command: plan under the lock, decode
        // without it (one short ffmpeg pass per clip), apply under it again.
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.smart_crop_inputs(clip).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let mut on_progress = |fraction: f32| {
            let _ = app.emit(
                "smart-crop-progress",
                SmartCropProgress {
                    fraction: f64::from(fraction),
                },
            );
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        let crops = Project::sample_smart_crops_with(&plan, &mut on_progress, &stop).map_err(|e| match e {
            kerf_core::Error::Cancelled => SMART_CROP_CANCELLED.to_string(),
            other => other.to_string(),
        })?;
        let project = lock_user(&shared);
        require_generation(&project, generation, "the shots were being framed")?;
        project.apply_smart_crops(&crops).map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

/// Stop the smart crop in flight (if any).
#[tauri::command(async)]
pub(crate) fn cancel_smart_crop(state: State<'_, AppState>) {
    state.smart_crop_cancel.stop();
}

/// The error an abandoned stabilization analysis returns, for the webview to stay quiet on.
const STABILIZE_CANCELLED: &str = "stabilization cancelled";

/// `stabilize-progress`: where a clip's stabilization stands while it is analysed (its status, the
/// fraction measured in it), and once more when the analysis ends.
#[derive(Serialize, Clone)]
struct StabilizeProgress {
    #[serde(flatten)]
    status: ClipStabilizeStatus,
}

/// Stabilize a clip (`stabilize`: strength and optional zoom) or stop (`null`), as one revision.
/// A still, a title, a clip of sound and 360 footage are refused. The clip plays as shot until its
/// analysis lands (`analyze_clip_stabilization`, which the Inspector starts).
#[tauri::command(async)]
pub(crate) fn set_clip_stabilize(
    state: State<'_, AppState>,
    clip_id: String,
    stabilize: Option<Stabilize>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_clip_stabilize(id, stabilize).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Measure a stabilized clip's camera (vid.stab's first pass over its window of the original, in the
/// background lane of the heavy-job queue) and keep it beside the footage; the preview and the export
/// steady the clip from then on. Nothing to do where an analysis already covers it or this FFmpeg has
/// no vid.stab (`deshake` needs none). Streams `stabilize-progress`; `cancel_stabilization` stops it,
/// rejecting with `"stabilization cancelled"`. Answers the clip's status afterwards.
#[tauri::command]
pub(crate) async fn analyze_clip_stabilization(
    app: AppHandle,
    state: State<'_, AppState>,
    clip_id: String,
) -> CmdResult<ClipStabilizeStatus> {
    let clip = id(&clip_id, "clip_id")?;
    let shared = state.project.clone();
    let cancel = state.stabilize_cancel.begin();
    blocking(move || {
        let (job, generation) = {
            let project = lock_user(&shared);
            (
                project.stabilize_inputs(clip).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let emit = |status: StabilizeStatus| {
            let _ = app.emit(
                "stabilize-progress",
                StabilizeProgress {
                    status: ClipStabilizeStatus { clip_id: clip, status },
                },
            );
        };
        emit(StabilizeStatus::Queued);
        let stop = stop_or_replaced(cancel, &shared, generation);
        let ran = Project::analyze_stabilization(&job, &mut |fraction| emit(StabilizeStatus::Analyzing { fraction }), &stop);
        let inputs = lock_user(&shared)
            .stabilization_status_inputs(Some(&[clip]))
            .map_err(|e| e.to_string())?;
        let status = Project::stabilization_statuses(&inputs).pop().ok_or("the clip is gone")?;
        emit(status.status.clone());
        match ran {
            Ok(_) => Ok(status),
            Err(kerf_core::Error::Cancelled) => Err(STABILIZE_CANCELLED.to_string()),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
}

/// Stop the stabilization analyses in flight (if any).
#[tauri::command(async)]
pub(crate) fn cancel_stabilization(state: State<'_, AppState>) {
    state.stabilize_cancel.stop();
}

/// Where each clip's stabilization stands: the named clips, or every stabilized one (`off`,
/// `unsupported`, `basic` for `deshake`, `missing`, `queued`, `analyzing`, `failed`, `ready`). Reads the
/// analyses kept on disk with the lock released.
#[tauri::command]
pub(crate) async fn clip_stabilization_status(
    state: State<'_, AppState>,
    clip_ids: Option<Vec<String>>,
) -> CmdResult<Vec<ClipStabilizeStatus>> {
    let ids = clip_ids
        .map(|ids| ids.iter().map(|s| id(s, "clip_ids")).collect::<CmdResult<Vec<_>>>())
        .transpose()?;
    let shared = state.project.clone();
    blocking(move || {
        let inputs = lock_user(&shared)
            .stabilization_status_inputs(ids.as_deref())
            .map_err(|e| e.to_string())?;
        Ok(Project::stabilization_statuses(&inputs))
    })
    .await
}

/// What syncing a clip by its sound found, and the timeline after it.
#[derive(serde::Serialize)]
pub(crate) struct SyncedByAudio {
    sync: kerf_core::AudioSync,
    timeline: Timeline,
}

/// Move `clip_id` so its sound lines up with `reference_id`'s (linked partners follow).
/// An unreliable match is reported without moving anything unless `force`; a forced sync of
/// the pair just measured (the same decode) applies that measurement instead of decoding
/// again. A wait for the heavy-job slot is told through `audio-sync-progress`;
/// `cancel_audio_sync` stops it, which then rejects with `"audio sync cancelled"`.
#[tauri::command]
pub(crate) async fn sync_clips_by_audio(
    app: AppHandle,
    state: State<'_, AppState>,
    reference_id: String,
    clip_id: String,
    force: Option<bool>,
) -> CmdResult<SyncedByAudio> {
    let (reference, clip) = (id(&reference_id, "reference_id")?, id(&clip_id, "clip_id")?);
    let force = force.unwrap_or(false);
    let shared = state.project.clone();
    let cancel = state.sync_cancel.begin();
    let last = state.last_sync.clone();
    blocking(move || {
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.audio_sync_inputs(reference, clip).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let cached = last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(p, _)| force && *p == plan)
            .map(|(_, m)| *m);
        let measured = match cached {
            Some(m) => m,
            None => {
                let stop = stop_or_replaced(cancel, &shared, generation);
                let m = Project::measure_audio_sync(
                    &plan,
                    &mut |w| {
                        let _ = app.emit(
                            "audio-sync-progress",
                            VoiceoverProgressEvent {
                                stage: "waiting".to_string(),
                                fraction: None,
                                detail: Some(w.message().to_string()),
                            },
                        );
                    },
                    &stop,
                )
                .map_err(|e| match e {
                    kerf_core::Error::Cancelled => AUDIO_SYNC_CANCELLED.to_string(),
                    other => other.to_string(),
                })?;
                *last.lock().unwrap_or_else(|e| e.into_inner()) = Some((plan, m));
                m
            }
        };
        let project = lock_user(&shared);
        require_generation(&project, generation, "the clips were being synced")?;
        let sync = project.apply_audio_sync(&measured, force).map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(SyncedByAudio { sync, timeline })
    })
    .await
}

/// The error a stopped sync returns, for the webview to stay quiet on.
const AUDIO_SYNC_CANCELLED: &str = "audio sync cancelled";

/// Stop the sync by audio in flight (if any): it gives up while it waits for the slot or
/// between its decodes, and [`sync_clips_by_audio`] rejects with `"audio sync cancelled"`.
#[tauri::command(async)]
pub(crate) fn cancel_audio_sync(state: State<'_, AppState>) {
    state.sync_cancel.stop();
}

/// Every publishing target Kerf knows about, with its frame and length limits.
#[tauri::command(async)]
pub(crate) fn platform_targets() -> Vec<kerf_core::PlatformTarget> {
    kerf_core::PLATFORM_TARGETS.to_vec()
}

/// How ready the current cut is for each target — what would be rejected, what
/// would be accepted and then under-distributed, and what would just be better.
#[tauri::command(async)]
pub(crate) fn platform_check(
    state: State<'_, AppState>,
    width: Option<u32>,
    height: Option<u32>,
    range: Option<kerf_core::TimeRange>,
) -> CmdResult<kerf_core::PlatformReport> {
    // The export dialog can resize away from the project frame and write only
    // the marked span; when it does it passes the frame and range it is actually
    // about to render, so the verdict is about the file that will exist rather
    // than the one the project defaults to. The answer carries the cut it judged
    // so the dialog can say its frame and length in words.
    let frame = width.zip(height);
    let range = range.map(|r| (r.start, r.end));
    state.project().platform_report(frame, range).map_err(|e| e.to_string())
}

/// The working cut's editing mistakes (`kerf_core::review`): flash frames, cuts inside a word,
/// sound cut into sound, jump cuts, stale captions and black gaps. `checks` by name, none for all;
/// `range` keeps what falls in that span. Inputs resolve under the lock; the review runs without it.
#[tauri::command(async)]
pub(crate) fn review_cut(
    state: State<'_, AppState>,
    range: Option<kerf_core::TimeRange>,
    checks: Option<Vec<String>>,
) -> CmdResult<kerf_core::CutReview> {
    let checks = kerf_core::ReviewCheck::parse_list(&checks.unwrap_or_default()).map_err(|e| e.to_string())?;
    let inputs = state.project().review_inputs().map_err(|e| e.to_string())?;
    Ok(kerf_core::review::review(&inputs, range.map(|r| (r.start, r.end)), &checks))
}

/// Show a file in the OS file manager. The last step of an export: the render
/// finished somewhere, and "somewhere" is not much use on its own.
#[tauri::command(async)]
pub(crate) fn reveal_path(app: AppHandle, path: String) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    // Open the containing folder, not the file — opening the file would launch
    // a player, which is not what "show me where it went" means.
    let target = std::path::Path::new(&path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(&path));
    app.opener()
        .open_path(target.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Request cancellation of the in-flight analysis pass (if any). The running
/// [`analyze_asset`] observes the flag between steps — and, during
/// transcription, about once a second — then gives up and caches nothing.
#[tauri::command(async)]
pub(crate) fn cancel_analysis(state: State<'_, AppState>) {
    state.analysis_cancel.stop();
}

/// Request cancellation of the in-flight export (if any). The running
/// [`export_timeline`] observes the flag on its next progress poll, stops
/// ffmpeg, and returns the `"export cancelled"` error.
#[tauri::command(async)]
pub(crate) fn cancel_export(state: State<'_, AppState>) {
    state.export_cancel.stop();
}
