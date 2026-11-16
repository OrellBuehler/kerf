//! Sound effects, band balance and stems.

use crate::*;

// ---- sound effects -----------------------------------------------------------

/// The error a sound effect abandoned because its project was replaced returns, for the webview to
/// stay quiet on.
const SFX_CANCELLED: &str = "sound effect cancelled";

fn sfx_err(e: kerf_core::Error) -> String {
    match e {
        kerf_core::Error::Cancelled => SFX_CANCELLED.to_string(),
        other => other.to_string(),
    }
}

/// Every sound effect Kerf can make, one row per kind: its label, category, what it sounds like, the
/// lengths it may have, where its hit point is and how loud it sits.
#[tauri::command(async)]
pub(crate) fn sfx_catalogue() -> Vec<kerf_core::SfxPreset> {
    kerf_core::sfx_catalogue().to_vec()
}

/// `spec` rendered for auditioning, as the bytes of its 48 kHz stereo 16-bit WAV (the page decodes
/// them with Web Audio: the CSP has no `media-src`, so a path the page could load does not exist).
/// No edit and no asset; the same spec is the same audio that placing it would use, in a file of its own
/// in the cache folder (which is pruned), not among the project's effects. A render takes milliseconds and
/// never touches the project. The GUI's alone: an agent does not hear, so there is no MCP tool for it.
#[tauri::command]
pub(crate) async fn preview_sound_effect(spec: kerf_core::SfxSpec) -> CmdResult<tauri::ipc::Response> {
    let wav = blocking(move || {
        let path = Project::preview_sound_effect(&spec, &|| false).map_err(sfx_err)?;
        std::fs::read(&path).map_err(|e| format!("could not read the rendered sound effect: {e}"))
    })
    .await?;
    Ok(tauri::ipc::Response::new(wav))
}

/// Sound effects placed: the clips in the order they were asked for, and the refreshed timeline.
#[derive(Serialize)]
pub(crate) struct SfxAdded {
    clips: Vec<kerf_core::Clip>,
    timeline: Timeline,
}

/// Synthesize `placements` and put them on the timeline as **one** revision: each clip's hit point at
/// its `at`, on the first `FX` lane with room unless it names a track. `ripple` forces ripple on or
/// off for the call; omitted, the project's mode decides.
///
/// The batch is checked against the cut (the placement is rehearsed) under the project lock, rendered
/// with it released, and only landing the result takes it again.
#[tauri::command]
pub(crate) async fn add_sound_effects(
    state: State<'_, AppState>,
    placements: Vec<kerf_core::SfxPlacement>,
    ripple: Option<bool>,
) -> CmdResult<SfxAdded> {
    let shared = state.project.clone();
    blocking(move || {
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project
                    .with_ripple(ripple, |project| project.sound_effects_inputs(&placements))
                    .map_err(sfx_err)?,
                project.generation(),
            )
        };
        let stop = stop_or_replaced(|| false, &shared, generation);
        let assets = Project::synthesize_sound_effects(&plan, &stop).map_err(sfx_err)?;
        let project = lock_user(&shared);
        if project.generation() != generation {
            return Err(SFX_CANCELLED.to_string());
        }
        let clips = project
            .with_ripple(ripple, |project| project.place_sound_effects(&assets, &plan))
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|placed| placed.clip)
            .collect();
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(SfxAdded { clips, timeline })
    })
    .await
}

/// A sound effect reshaped: its clip as it stands, and the refreshed timeline.
#[derive(Serialize)]
pub(crate) struct SfxUpdated {
    clip: kerf_core::Clip,
    timeline: Timeline,
}

/// Render the effect on `clip_id` again from `spec` (the whole spec: what it leaves out is back at its
/// default) and swap the clip's audio, keeping its id, gain (unless `volume_db` says another), fades,
/// effects and the timeline position of its **hit point**. One revision; refused for a clip that is
/// not a synthesized sound effect.
#[tauri::command]
pub(crate) async fn update_sound_effect(
    state: State<'_, AppState>,
    clip_id: String,
    spec: kerf_core::SfxSpec,
    volume_db: Option<f64>,
    ripple: Option<bool>,
) -> CmdResult<SfxUpdated> {
    let clip_id = id(&clip_id, "clip_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let (update, generation) = {
            let project = lock_user(&shared);
            (
                project
                    .with_ripple(ripple, |project| {
                        project.update_sound_effect_inputs(clip_id, &spec, volume_db)
                    })
                    .map_err(sfx_err)?,
                project.generation(),
            )
        };
        let stop = stop_or_replaced(|| false, &shared, generation);
        let asset = Project::synthesize_sound_effect(&update.spec, &stop).map_err(sfx_err)?;
        let project = lock_user(&shared);
        if project.generation() != generation {
            return Err(SFX_CANCELLED.to_string());
        }
        let clip = project
            .with_ripple(ripple, |project| project.apply_sound_effect_update(&asset, &update))
            .map_err(|e| e.to_string())?
            .clip;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(SfxUpdated { clip, timeline })
    })
    .await
}

/// Where the working cut would take a sound effect — on its cuts, transitions, titles, punch-ins,
/// drops in the music and `sfx` markers — as placements `add_sound_effects` takes as they are. A read:
/// nothing is analysed or changed, and footage that has not been analysed simply suggests less.
#[tauri::command]
pub(crate) async fn suggest_sound_effects(
    state: State<'_, AppState>,
    request: kerf_core::SfxSuggestRequest,
) -> CmdResult<Vec<kerf_core::SfxSuggestion>> {
    let shared = state.project.clone();
    blocking(move || {
        request.check().map_err(|e| e.to_string())?;
        let inputs = lock_user(&shared).suggest_sound_effects_inputs().map_err(|e| e.to_string())?;
        kerf_core::suggest_sound_effects(&inputs, &request).map_err(|e| e.to_string())
    })
    .await
}

/// The error a stopped band-balance measurement returns, for the webview to stay quiet on.
const BAND_BALANCE_CANCELLED: &str = "band balance cancelled";

/// Where a stretch of sound keeps its energy — six bands per window, with RMS and loudness — for one
/// asset, one clip's source range or a range of the mix. The target and windows are resolved under
/// the project lock; the decode runs with it released and `cancel_band_balance` stops it.
#[tauri::command]
pub(crate) async fn get_band_balance(
    state: State<'_, AppState>,
    request: kerf_core::BandBalanceRequest,
) -> CmdResult<kerf_core::BandBalance> {
    let shared = state.project.clone();
    let cancel = state.bands_cancel.begin();
    blocking(move || {
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.band_balance_inputs(&request).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        Project::measure_band_balance(&plan, &stop).map_err(|e| match e {
            kerf_core::Error::Cancelled => BAND_BALANCE_CANCELLED.to_string(),
            other => other.to_string(),
        })
    })
    .await
}

/// Request cancellation of the band-balance measurement in flight (if any).
#[tauri::command(async)]
pub(crate) fn cancel_band_balance(state: State<'_, AppState>) {
    state.bands_cancel.stop();
}

/// Whether stem separation would still download its runtime or model.
#[tauri::command(async)]
pub(crate) fn stems_status() -> CmdResult<kerf_core::StemsStatus> {
    Ok(kerf_core::stems_status())
}

/// The error an abandoned separation returns, for the webview to stay quiet on.
const STEMS_CANCELLED: &str = "stems cancelled";

#[derive(Serialize)]
pub(crate) struct StemsResult {
    placed: kerf_core::StemsPlaced,
    timeline: Timeline,
}

/// Split an asset's sound into drums / bass / other / vocals, streaming
/// `stems-progress` (`download_runtime`, `download_model`, `separate`, `encode`), and
/// add them to the library — laid under `clip_id` on new tracks when given. The
/// separation runs with the project lock released; only landing it takes the lock.
#[tauri::command]
pub(crate) async fn separate_stems(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    clip_id: Option<String>,
) -> CmdResult<StemsResult> {
    let asset_id = id(&asset_id, "asset_id")?;
    let clip = opt_id(clip_id.as_deref(), "clip_id")?;
    let shared = state.project.clone();
    let cancel = state.stems_cancel.begin();
    blocking(move || {
        let (asset, generation) = {
            let project = lock_user(&shared);
            (
                project.require_asset(asset_id).map_err(|e| e.to_string())?,
                project.generation(),
            )
        };
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "stems-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        let stems = Project::separate_stems_media(&asset, &mut on_progress, &stop).map_err(|e| match e {
            kerf_core::Error::Cancelled => STEMS_CANCELLED.to_string(),
            other => other.to_string(),
        })?;
        let project = lock_user(&shared);
        // Separated for a project that is no longer open: the quiet stop of a cancel.
        if project.generation() != generation {
            return Err(STEMS_CANCELLED.to_string());
        }
        let placed = project.place_stems(asset_id, &stems, clip).map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(StemsResult { placed, timeline })
    })
    .await
}

/// Request cancellation of the running stem separation (or its download).
#[tauri::command(async)]
pub(crate) fn cancel_stems(state: State<'_, AppState>) {
    state.stems_cancel.stop();
}
