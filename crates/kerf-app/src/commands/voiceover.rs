//! Voiceover.

use crate::*;

// ---- voiceover ---------------------------------------------------------------

/// Whether voiceovers can be generated here, what the first one has to
/// download, and the voices on offer.
#[tauri::command(async)]
pub(crate) fn voiceover_status() -> CmdResult<kerf_core::VoiceoverStatus> {
    Ok(kerf_core::voiceover_status())
}

/// The error an abandoned voiceover returns, for the webview to stay quiet on.
const VOICEOVER_CANCELLED: &str = "voiceover cancelled";

fn voiceover_err(e: kerf_core::Error) -> String {
    match e {
        kerf_core::Error::Cancelled => VOICEOVER_CANCELLED.to_string(),
        other => other.to_string(),
    }
}

/// Download the voice runtime, model and `voice` ahead of time, streaming
/// `voiceover-progress`, so the first voiceover starts synthesizing at once.
#[tauri::command]
pub(crate) async fn prepare_voiceover(
    app: AppHandle,
    state: State<'_, AppState>,
    voice: String,
) -> CmdResult<kerf_core::VoiceoverStatus> {
    let cancel = state.voiceover_cancel.begin();
    blocking(move || {
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "voiceover-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        kerf_core::prepare_voiceover(&voice, &mut on_progress, &|| cancel()).map_err(voiceover_err)?;
        Ok(kerf_core::voiceover_status())
    })
    .await
}

#[derive(Serialize)]
pub(crate) struct VoiceoverResult {
    asset: Asset,
    timeline: Timeline,
}

/// Read `text` aloud and put it on the timeline — on the `VO` track unless
/// `track_id` names another audio track, at `timeline_start` or after what is
/// already there — then caption the cut when `captions` is given.
///
/// Synthesis (and a first-use model download) runs with the project lock
/// released, like an import; only landing the result takes it.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate_voiceover(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    voice: Option<String>,
    speed: Option<f64>,
    track_id: Option<String>,
    timeline_start: Option<f64>,
    captions: Option<CaptionOptions>,
) -> CmdResult<VoiceoverResult> {
    let track = opt_id(track_id.as_deref(), "track_id")?;
    let shared = state.project.clone();
    let cancel = state.voiceover_cancel.begin();
    blocking(move || {
        let generation = lock_user(&shared).generation();
        let voice = voice.unwrap_or_else(|| kerf_core::DEFAULT_VOICE.to_string());
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "voiceover-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        let asset =
            Project::synthesize_voiceover(&text, &voice, speed.unwrap_or(1.0), &mut on_progress, &stop).map_err(voiceover_err)?;
        let project = lock_user(&shared);
        // Spoken for a project that is no longer open: the quiet stop of a cancel, not a failure.
        if project.generation() != generation {
            return Err(VOICEOVER_CANCELLED.to_string());
        }
        let (asset, _) = project
            .place_voiceover(&asset, track, timeline_start)
            .map_err(|e| e.to_string())?;
        if let Some(options) = captions {
            project.generate_captions(options).map_err(|e| e.to_string())?;
        }
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(VoiceoverResult { asset, timeline })
    })
    .await
}

/// Request cancellation of the voiceover being generated (or its download).
#[tauri::command(async)]
pub(crate) fn cancel_voiceover(state: State<'_, AppState>) {
    state.voiceover_cancel.stop();
}
