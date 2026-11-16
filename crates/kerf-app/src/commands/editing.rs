//! Ripple mode and the timeline edits, each of which returns the refreshed timeline.

use crate::*;

// ---- ripple mode -------------------------------------------------------------

/// Whether the project edits in ripple mode (off for a project that never said).
#[tauri::command(async)]
pub(crate) fn get_ripple_mode(state: State<'_, AppState>) -> CmdResult<bool> {
    state.project().ripple_mode().map_err(|e| e.to_string())
}

/// Turn ripple mode on or off for the project and answer with what is stored.
/// A setting, not an edit: no revision, and the timeline does not move, so
/// nothing is returned but the flag.
#[tauri::command(async)]
pub(crate) fn set_ripple_mode(state: State<'_, AppState>, on: bool) -> CmdResult<bool> {
    let project = state.project();
    project.set_ripple_mode(on).map_err(|e| e.to_string())?;
    project.ripple_mode().map_err(|e| e.to_string())
}

// ---- timeline editing (each returns the refreshed timeline) ----------------

#[tauri::command(async)]
pub(crate) fn cut_clip(state: State<'_, AppState>, asset_id: String, start: f64, end: f64) -> CmdResult<Timeline> {
    let id = id(&asset_id, "asset_id")?;
    let project = state.project();
    project.cut_clip(id, start, end).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn add_clip(
    state: State<'_, AppState>,
    asset_id: String,
    track_id: Option<String>,
    source_in: f64,
    source_out: f64,
    timeline_start: Option<f64>,
) -> CmdResult<Timeline> {
    let asset = id(&asset_id, "asset_id")?;
    let track = opt_id(track_id.as_deref(), "track_id")?;
    let project = state.project();
    project
        .add_clip_to_timeline(asset, track, source_in, source_out, timeline_start)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Split a clip at timeline time `at`. A clip's linked partners (a picture and its
/// detached sound) are split at the same moment unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn split_clip(state: State<'_, AppState>, clip_id: String, at: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.with_links(link, |p| p.split_at(id, at)).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn trim_clip(
    state: State<'_, AppState>,
    clip_id: String,
    source_in: Option<f64>,
    source_out: Option<f64>,
    timeline_start: Option<f64>,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.trim(id, source_in, source_out, timeline_start))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn reorder_clip(
    state: State<'_, AppState>,
    track_id: String,
    clip_id: String,
    new_index: usize,
) -> CmdResult<Timeline> {
    let track = id(&track_id, "track_id")?;
    let clip = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.reorder(track, clip, new_index).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn move_clip(
    state: State<'_, AppState>,
    clip_id: String,
    timeline_start: f64,
    track_id: Option<String>,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let clip = id(&clip_id, "clip_id")?;
    let track = opt_id(track_id.as_deref(), "track_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.move_clip(clip, timeline_start, track))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Move several clips in **one** revision — a marquee selection dragged
/// together. Each move names a clip, its absolute start and optionally another
/// track of the same kind; the whole group is checked as a group and an illegal
/// one changes nothing. Never ripples. Linked partners of the named clips move
/// with them (by the same Δt, on their own tracks) unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn move_clips(state: State<'_, AppState>, moves: Vec<ClipMove>, link: Option<bool>) -> CmdResult<Timeline> {
    let project = state.project();
    project
        .with_links(link, |p| p.move_clips(&moves))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn ripple_delete(state: State<'_, AppState>, clip_id: String, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.with_links(link, |p| p.ripple_delete(id)).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn cut_clip_range(
    state: State<'_, AppState>,
    clip_id: String,
    from: f64,
    to: f64,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.cut_clip_range(id, from, to))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Roll the cut between two adjacent clips of one track: `clip_a`'s end and
/// `clip_b`'s start move together by `delta` seconds (positive is later), clamped
/// to each clip's footage and a 0.05 s floor. Never ripples. The cuts of linked
/// partner pairs roll with it unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn roll_edit(
    state: State<'_, AppState>,
    clip_a: String,
    clip_b: String,
    delta: f64,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let (a, b) = (id(&clip_a, "clip_a")?, id(&clip_b, "clip_b")?);
    let project = state.project();
    project
        .with_links(link, |p| p.roll_edit(a, b, delta))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Slip a clip: show a different part of its footage in the same place and for
/// the same length. `delta` is in **source** seconds, positive = starts later in
/// its own footage (mirrored window for a reversed clip). Clamped to the footage.
/// Never ripples. Linked partners slip by the same moment of footage unless `link`
/// is `false`.
#[tauri::command(async)]
pub(crate) fn slip_clip(state: State<'_, AppState>, clip_id: String, delta: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.slip_clip(id, delta))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Slide a clip along its track by `delta` timeline seconds, the neighbours that
/// touch it giving way. Clamped to their footage and a 0.05 s floor. Never ripples.
/// Linked partners slide with it unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn slide_clip(state: State<'_, AppState>, clip_id: String, delta: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.slide_clip(id, delta))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Split a clip at timeline time `at` and remove one half (`side` is `"left"` or
/// `"right"`): trim the start / end to the playhead. Follows ripple mode. Linked
/// partners that span `at` are trimmed with it unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn split_remove(
    state: State<'_, AppState>,
    clip_id: String,
    at: f64,
    side: String,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let side = SplitSide::parse(&side).ok_or_else(|| format!("invalid side '{side}'; expected \"left\" or \"right\""))?;
    let project = state.project();
    project
        .with_links(link, |p| p.split_remove(id, at, side))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Split and remove on several clips as **one** revision — the playhead trim of a
/// selection, V1 and its A1 partner together, undone in one step. Each cut names a
/// clip and its time; `side` is the same for all. All or nothing, one clip per track;
/// follows ripple mode, each track on its own. Linked partners of a named clip that
/// are not named are cut with it unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn split_remove_clips(
    state: State<'_, AppState>,
    cuts: Vec<ClipCut>,
    side: String,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let side = SplitSide::parse(&side).ok_or_else(|| format!("invalid side '{side}'; expected \"left\" or \"right\""))?;
    let project = state.project();
    project
        .with_links(link, |p| p.split_remove_clips(&cuts, side))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn add_track(state: State<'_, AppState>, kind: String, name: Option<String>) -> CmdResult<Timeline> {
    let kind = self::kind(&kind)?;
    let project = state.project();
    project.add_track(kind, name).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn remove_track(state: State<'_, AppState>, track_id: String) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.remove_track(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_track_duck(state: State<'_, AppState>, track_id: String, duck: bool) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_duck(id, duck).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set a track's fader — the gain riding every clip on the track.
#[tauri::command(async)]
pub(crate) fn set_track_volume(state: State<'_, AppState>, track_id: String, volume: f32) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_volume(id, volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set a track's stereo placement, -1 (hard left) to 1 (hard right).
#[tauri::command(async)]
pub(crate) fn set_track_pan(state: State<'_, AppState>, track_id: String, pan: f32) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_pan(id, pan).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Set the master fader — the linear gain on the finished mix, after every
/// track and the duck bus and before loudness normalisation.
#[tauri::command(async)]
pub(crate) fn set_master_volume(state: State<'_, AppState>, volume: f64) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_volume(volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Switch the master limiter on or off, optionally moving its ceiling (dBFS).
/// Omitting `ceiling_db` keeps the one it had.
#[tauri::command(async)]
pub(crate) fn set_master_limiter(state: State<'_, AppState>, enabled: bool, ceiling_db: Option<f64>) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_limiter(enabled, ceiling_db).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Make the master limiter hold the true peak (4x oversampled) or the sample peak.
#[tauri::command(async)]
pub(crate) fn set_master_true_peak(state: State<'_, AppState>, enabled: bool) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_true_peak(enabled).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Choose how ducked tracks dip: `depth_db` (negative dB) for the speech gate, omitted
/// for the sidechain compressor.
#[tauri::command(async)]
pub(crate) fn set_master_duck(state: State<'_, AppState>, depth_db: Option<f64>) -> CmdResult<Timeline> {
    let project = state.project();
    project.set_master_duck(depth_db).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Measure how loud the cut is — the finished mix and each track — in one pass
/// over the audio the export would render. Whole-file work, so it resolves its
/// inputs under the project lock and runs ffmpeg with it released. `range` is a
/// `{start, end}` span of the cut (default all of it); `loudnorm` measures the
/// mix as an export with normalisation on would write it. `cancel_levels` stops
/// the pass, which then rejects with `"levels cancelled"`.
#[tauri::command]
pub(crate) async fn get_levels(
    state: State<'_, AppState>,
    range: Option<TimeRange>,
    loudnorm: Option<bool>,
) -> CmdResult<Levels> {
    let shared = state.project.clone();
    // Fresh cancel flag for this pass; `cancel_levels` flips it from the UI.
    let cancel = state.levels_cancel.begin();
    blocking(move || {
        let ((timeline, assets), generation) = {
            let project = lock_user(&shared);
            (project.levels_inputs().map_err(|e| e.to_string())?, project.generation())
        };
        let levels =
            Project::measure_levels(&timeline, &assets, range, loudnorm.unwrap_or(false), &|| cancel()).map_err(|e| match e {
                kerf_core::Error::Cancelled => LEVELS_CANCELLED.to_string(),
                other => other.to_string(),
            })?;
        // The levels of a cut that is no longer the open one are of no use to the one that is.
        if lock_user(&shared).generation() != generation {
            return Err(LEVELS_CANCELLED.to_string());
        }
        Ok(levels)
    })
    .await
}

/// The error a stopped measurement returns, for the webview to stay quiet on.
const LEVELS_CANCELLED: &str = "levels cancelled";

/// Request cancellation of the loudness measurement in flight (if any). The running
/// [`get_levels`] polls the flag while ffmpeg works, kills it and rejects with
/// `"levels cancelled"`.
#[tauri::command(async)]
pub(crate) fn cancel_levels(state: State<'_, AppState>) {
    state.levels_cancel.stop();
}

/// Set the frame the project is cut for, or clear it back to the source shape.
/// The preview, the scrubbed still and the export all read it, so the vertical
/// crop is visible while cutting instead of only in the rendered file. `fps` is the
/// frame rate every export, playback and still defaults to (1 to 240; `None` follows the
/// footage), and it is set with the frame: a call without one clears it.
#[tauri::command(async)]
pub(crate) fn set_delivery_format(
    state: State<'_, AppState>,
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    fps: Option<f64>,
) -> CmdResult<Timeline> {
    let format = match (width, height) {
        (Some(w), Some(h)) => Some(
            Delivery::new(w, h, fit.unwrap_or(Fit::Cover))
                .with_fps(fps)
                .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    state.project().set_delivery_format(format).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_track_muted(state: State<'_, AppState>, track_id: String, muted: bool) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_muted(id, muted).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_track_solo(state: State<'_, AppState>, track_id: String, solo: bool) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_solo(id, solo).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_track_locked(state: State<'_, AppState>, track_id: String, locked: bool) -> CmdResult<Timeline> {
    let id = id(&track_id, "track_id")?;
    let project = state.project();
    project.set_track_locked(id, locked).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_clip_enabled(state: State<'_, AppState>, clip_id: String, enabled: bool) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_clip_enabled(id, enabled).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}
