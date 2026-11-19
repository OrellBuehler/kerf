//! Music.

use crate::*;

// ---- music -------------------------------------------------------------------

/// Whether music can be composed here: the SoundFont's state, compositions in flight and the styles.
#[tauri::command(async)]
pub(crate) fn music_status() -> CmdResult<kerf_core::MusicStatus> {
    Ok(kerf_core::music_status())
}

/// The error an abandoned composition returns, for the webview to stay quiet on.
const MUSIC_CANCELLED: &str = "music cancelled";

fn music_err(e: kerf_core::Error) -> String {
    match e {
        kerf_core::Error::Cancelled => MUSIC_CANCELLED.to_string(),
        other => other.to_string(),
    }
}

/// Download the SoundFont ahead of time, streaming `music-progress` (`download_soundfont`), so the
/// first composition starts rendering at once.
#[tauri::command]
pub(crate) async fn prepare_music(app: AppHandle, state: State<'_, AppState>) -> CmdResult<kerf_core::MusicStatus> {
    let cancel = state.music_cancel.begin();
    blocking(move || {
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "music-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        kerf_core::prepare_soundfont(&mut on_progress, &|| cancel()).map_err(music_err)?;
        Ok(kerf_core::music_status())
    })
    .await
}

/// A composition landed: what [`kerf_core::Composed`] says, and the refreshed timeline.
#[derive(Serialize)]
pub(crate) struct ComposeResult {
    #[serde(flatten)]
    composed: kerf_core::Composed,
    timeline: Timeline,
}

/// Compose music from a score and put it on the timeline: on the `M` track unless `request.track_id`
/// names another audio track, at `request.at` or after what is already there — or, with
/// `request.replace_clip_id`, swapped into that composed clip. With `request.align` the score is
/// first fitted to the cut's picture (tempo nudged, hits added).
///
/// The request is checked and aligned under the project lock, rendering (and a first-use SoundFont
/// download) runs with it released, streaming `music-progress` (`download_soundfont`, `waiting`,
/// `render`); only landing the result takes it again.
#[tauri::command]
pub(crate) async fn compose_music(
    app: AppHandle,
    state: State<'_, AppState>,
    request: kerf_core::ComposeRequest,
) -> CmdResult<ComposeResult> {
    let shared = state.project.clone();
    let cancel = state.music_cancel.begin();
    blocking(move || {
        let (plan, generation) = {
            let project = lock_user(&shared);
            (
                project.compose_music_inputs(&request).map_err(music_err)?,
                project.generation(),
            )
        };
        let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
            let _ = app.emit(
                "music-progress",
                VoiceoverProgressEvent {
                    stage: stage.to_string(),
                    fraction,
                    detail,
                },
            );
        };
        let stop = stop_or_replaced(cancel, &shared, generation);
        let asset = Project::synthesize_music(&plan.score, &mut on_progress, &stop).map_err(music_err)?;
        let project = lock_user(&shared);
        // Composed for a project that is no longer open: the quiet stop of a cancel, not a failure.
        if project.generation() != generation {
            return Err(MUSIC_CANCELLED.to_string());
        }
        let (asset, clip) = project
            .place_music(&asset, plan.track_id, plan.at, plan.replace_clip_id)
            .map_err(|e| e.to_string())?;
        let timeline = project.timeline().map_err(|e| e.to_string())?;
        Ok(ComposeResult {
            composed: kerf_core::Composed {
                asset,
                clip,
                score: plan.score,
                align: plan.align,
                replaced: plan.replaces,
            },
            timeline,
        })
    })
    .await
}

/// Request cancellation of the music being composed (or its SoundFont download).
#[tauri::command(async)]
pub(crate) fn cancel_music(state: State<'_, AppState>) {
    state.music_cancel.stop();
}
