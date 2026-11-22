//! Ripple mode and the timeline edits, each of which returns the refreshed timeline.

use crate::mcp::batch::{self, EditOp};
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
/// follows ripple mode, each track on its own — `ripple` forces it on (Premiere's
/// ripple trim to the playhead) or off for this call, omitted the project's mode
/// decides. Linked partners of a named clip that are not named are cut with it unless
/// `link` is `false`.
#[tauri::command(async)]
pub(crate) fn split_remove_clips(
    state: State<'_, AppState>,
    cuts: Vec<ClipCut>,
    side: String,
    link: Option<bool>,
    ripple: Option<bool>,
) -> CmdResult<Timeline> {
    let side = SplitSide::parse(&side).ok_or_else(|| format!("invalid side '{side}'; expected \"left\" or \"right\""))?;
    let project = state.project();
    project
        .with_links(link, |p| p.with_ripple(ripple, |p| p.split_remove_clips(&cuts, side)))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

// ---- range edits: cut every track, lift, extract, delete a gap ---------------
// Each is one revision that names a time (or a range) and the tracks it is read on. They decide
// their own layout, so none takes `ripple`; each answers what it did beside the refreshed timeline.

/// `track_ids` as the core takes them: `None` is every unlocked track.
fn track_ids_arg(track_ids: Option<&[String]>) -> CmdResult<Option<Vec<Uuid>>> {
    track_ids.map(|list| ids(list, "track_ids")).transpose()
}

/// **Cut every track** at timeline time `at` (Premiere's Add Edit to All Tracks): every clip
/// with `at` inside it is split there on every unlocked track, or only on `track_ids`. Nothing
/// moves, so ripple mode is no part of it. A partner linked to a cut clip is cut with it unless
/// `link` is `false`. Answers the cuts and the locked tracks it left alone.
#[tauri::command(async)]
pub(crate) fn split_all_tracks(
    state: State<'_, AppState>,
    at: f64,
    track_ids: Option<Vec<String>>,
    link: Option<bool>,
) -> CmdResult<EditDone<CutAll>> {
    let tracks = track_ids_arg(track_ids.as_deref())?;
    let project = state.project();
    let result = project
        .with_links(link, |p| p.split_all_tracks(at, tracks.as_deref()))
        .map_err(|e| e.to_string())?;
    Ok(EditDone {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        result,
    })
}

/// **Lift** `[from, to)` off the targeted tracks (every unlocked one, or `track_ids`) and leave
/// the gap — Premiere's Lift. Never ripples. A linked partner the range reaches is cut with its
/// clip unless `link` is `false`. Answers what was removed, shortened and split.
#[tauri::command(async)]
pub(crate) fn lift_range(
    state: State<'_, AppState>,
    from: f64,
    to: f64,
    track_ids: Option<Vec<String>>,
    link: Option<bool>,
) -> CmdResult<EditDone<RangeEdit>> {
    let tracks = track_ids_arg(track_ids.as_deref())?;
    let project = state.project();
    let result = project
        .with_links(link, |p| p.lift_range(from, to, tracks.as_deref()))
        .map_err(|e| e.to_string())?;
    Ok(EditDone {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        result,
    })
}

/// **Extract** `[from, to)` from the targeted tracks and close the gap — Premiere's Extract.
/// Never rippled again, it closes the gap itself; text overlays and markers stay where they
/// are. Same arguments and answer as `lift_range`.
#[tauri::command(async)]
pub(crate) fn extract_range(
    state: State<'_, AppState>,
    from: f64,
    to: f64,
    track_ids: Option<Vec<String>>,
    link: Option<bool>,
) -> CmdResult<EditDone<RangeEdit>> {
    let tracks = track_ids_arg(track_ids.as_deref())?;
    let project = state.project();
    let result = project
        .with_links(link, |p| p.extract_range(from, to, tracks.as_deref()))
        .map_err(|e| e.to_string())?;
    Ok(EditDone {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        result,
    })
}

/// **Ripple delete a gap**: close the empty stretch of `track_id` that `at` is in, moving every
/// later clip of that track up by its length (Premiere's Ripple Delete on an empty space). The
/// linked partners of the clips that moved follow unless `link` is `false`. Never rippled again.
#[tauri::command(async)]
pub(crate) fn ripple_delete_gap(
    state: State<'_, AppState>,
    track_id: String,
    at: f64,
    link: Option<bool>,
) -> CmdResult<EditDone<GapClosed>> {
    let track = id(&track_id, "track_id")?;
    let project = state.project();
    let result = project
        .with_links(link, |p| p.ripple_delete_gap(track, at))
        .map_err(|e| e.to_string())?;
    Ok(EditDone {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        result,
    })
}

/// What [`apply_edits`] did: the timeline after the edits (the refreshed cut, or for a dry run the cut they would
/// make), each op's answer (the clip it made, the pieces a cut left, ... as the MCP tool of the same name answers,
/// every clip in a larger answer cut down to its place), whether they were kept, and the name of the one revision —
/// absent for a dry run and when the edits recorded none.
#[derive(serde::Serialize)]
pub(crate) struct AppliedEdits {
    timeline: Timeline,
    results: Vec<serde_json::Value>,
    applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

/// Several timeline edits as **one** revision — one undo step, all or nothing: an op that fails undoes the ones
/// before it and records nothing, and the error names it (`op 3 (trim): ...`). Each op is `{tool, args}`: one of the
/// quick edit tools the MCP `apply_edits` runs (`mcp/batch.rs`'s `BATCHABLE`) with that tool's own arguments
/// (snake_case, ids as strings, `ripple` and `link` per op, as the tool takes them). A string `"$K.path"` in `args`
/// is op K's answer at that path. `label` names the revision; blank, it is the edit's own label for one op, else a
/// count and the kinds of edit. The edits are the user's: they land live, never staged (an open proposal goes
/// stale, as for any edit of the GUI). `dry_run` runs them and keeps nothing: the answer is the cut they would make.
#[tauri::command]
pub(crate) async fn apply_edits(
    state: State<'_, AppState>,
    ops: Vec<EditOp>,
    label: Option<String>,
    dry_run: Option<bool>,
) -> CmdResult<AppliedEdits> {
    // Seconds under the project lock, so on the blocking pool and not on a worker the MCP server shares.
    let shared = state.project.clone();
    blocking(move || apply_edits_now(&shared, ops, label.as_deref(), dry_run.unwrap_or(false))).await
}

/// [`apply_edits`] on the shared project: the ops are checked before the lock is taken, the batch runs under it for
/// at most [`batch::USER_DEADLINE`], and nothing is emitted (the page takes the timeline from the answer, as it does
/// from every edit command).
pub(crate) fn apply_edits_now(
    project: &Mutex<Project>,
    ops: Vec<EditOp>,
    label: Option<&str>,
    dry_run: bool,
) -> CmdResult<AppliedEdits> {
    apply_edits_within(project, ops, label, dry_run, Some(batch::USER_DEADLINE))
}

/// [`apply_edits_now`] with the time the batch may hold the lock given.
fn apply_edits_within(
    project: &Mutex<Project>,
    ops: Vec<EditOp>,
    label: Option<&str>,
    dry_run: bool,
    deadline: Option<std::time::Duration>,
) -> CmdResult<AppliedEdits> {
    batch::check(&ops, batch::MAX_USER_OPS).map_err(|e| e.message.into_owned())?;
    let project = lock_user(project);
    let done = batch::run_ops(&project, ops, label, dry_run, deadline).map_err(|e| e.message.into_owned())?;
    Ok(AppliedEdits {
        timeline: done.after,
        results: done.result,
        applied: !dry_run,
        label: done.label,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::EditSource;
    use serde_json::{json, Value};

    /// The sample cut: V1 `c1` (interview, 0..12.5 s) and `c2` (b-roll, 12.5..20.5 s), A1 the interview's sound.
    fn sample() -> (Mutex<Project>, Vec<String>) {
        let project = Project::sample().unwrap();
        let ids = project.timeline().unwrap().tracks[0]
            .clips
            .iter()
            .map(|c| c.id.to_string())
            .collect();
        (Mutex::new(project), ids)
    }

    fn edits(v: Value) -> Vec<EditOp> {
        serde_json::from_value(v).unwrap()
    }

    fn apply(project: &Mutex<Project>, ops: Vec<EditOp>, label: Option<&str>) -> CmdResult<AppliedEdits> {
        apply_edits_now(project, ops, label, false)
    }

    fn snap(project: &Mutex<Project>) -> String {
        serde_json::to_string(&project.lock().unwrap().timeline().unwrap()).unwrap()
    }

    fn history(project: &Mutex<Project>) -> Vec<kerf_core::Revision> {
        project.lock().unwrap().history().unwrap()
    }

    #[test]
    fn several_edits_are_one_user_revision_and_one_undo_takes_them_all_back() {
        let (project, ids) = sample();
        let (before, revisions) = (snap(&project), history(&project).len());
        let done = apply(
            &project,
            edits(json!([
                {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}},
                {"tool": "trim", "args": {"clip_id": ids[1], "source_out": 5.0, "ripple": false}},
                {"tool": "set_keyframes", "args": {"clip_id": ids[0], "keyframes": [
                    {"time": 0.0, "scale": 1.0, "pos_x": 0.0, "pos_y": 0.0, "rotation": 0.0, "opacity": 1.0},
                    {"time": 2.0, "scale": 1.5, "pos_x": 0.0, "pos_y": 0.0, "rotation": 0.0, "opacity": 1.0},
                ]}},
                {"tool": "set_property_tracks", "args": {"clip_id": ids[0], "tracks": [
                    {"prop": "brightness", "keys": [{"time": 0.0, "value": 0.0}, {"time": 1.0, "value": 0.2}]},
                ]}},
            ])),
            Some("Tidy the open"),
        )
        .unwrap();
        let after = history(&project);
        assert_eq!(after.len(), revisions + 1, "one revision for the four edits");
        let last = after.last().unwrap();
        assert_eq!(
            (last.label.as_str(), last.source, last.current),
            ("Tidy the open", EditSource::User, true)
        );
        assert_eq!(done.label.as_deref(), Some("Tidy the open"));
        assert_eq!(done.results.len(), 4);
        let clip = |c: &Value| c["id"].as_str().map(str::to_string);
        assert_eq!(
            clip(&done.results[0]).as_deref(),
            Some(ids[0].as_str()),
            "an op answers the clip it made"
        );
        let c1 = done.timeline.clip(ids[0].parse().unwrap()).unwrap();
        assert_eq!(c1.volume, 0.5);
        assert_eq!(c1.keyframes.len(), 2);
        assert_eq!(done.timeline.clip(ids[1].parse().unwrap()).unwrap().source_out, 5.0);
        assert_eq!(
            serde_json::to_string(&done.timeline).unwrap(),
            snap(&project),
            "the timeline answered is the stored one"
        );

        project.lock().unwrap().undo().unwrap();
        assert_eq!(snap(&project), before, "one undo restores all four");
    }

    #[test]
    fn a_failing_op_applies_nothing_records_nothing_and_names_itself() {
        let (project, ids) = sample();
        let (before, revisions) = (snap(&project), history(&project).len());
        let err = apply(
            &project,
            edits(json!([
                {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}},
                {"tool": "split_at", "args": {"clip_id": ids[1], "at": 14.0}},
                {"tool": "trim", "args": {"clip_id": "not-a-uuid", "source_in": 1.0}},
            ])),
            Some("never"),
        )
        .map(|_| ())
        .unwrap_err();
        assert!(err.starts_with("op 2 (trim): `clip_id` must be a UUID"), "{err}");
        assert_eq!(snap(&project), before, "the two edits before it are undone");
        assert_eq!(history(&project).len(), revisions);

        // A refusal from the core, mid-batch, the same way.
        let err = apply(
            &project,
            edits(json!([
                {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}},
                {"tool": "set_volume", "args": {"clip_id": uuid::Uuid::new_v4().to_string(), "volume": 0.5}},
            ])),
            None,
        )
        .map(|_| ())
        .unwrap_err();
        assert!(err.starts_with("op 1 (set_volume)"), "{err}");
        assert_eq!(snap(&project), before);
        assert_eq!(history(&project).len(), revisions);

        // The project is usable afterwards, and the next batch is an ordinary revision.
        apply(
            &project,
            edits(json!([{"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.7}}])),
            None,
        )
        .unwrap();
        assert_eq!(history(&project).len(), revisions + 1);
    }

    #[test]
    fn the_label_is_the_one_given_else_the_edits_own() {
        let (project, ids) = sample();
        let one = json!([{"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}}]);
        let two = json!([
            {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.4}},
            {"tool": "set_volume", "args": {"clip_id": ids[1], "volume": 0.4}},
            {"tool": "set_clip_enabled", "args": {"clip_id": ids[1], "enabled": false}},
        ]);
        let label = |project: &Mutex<Project>| history(project).last().unwrap().label.clone();
        let done = apply(&project, edits(one.clone()), None).unwrap();
        assert_eq!(done.label.as_deref(), Some(label(&project).as_str()));
        let single = label(&project);
        assert!(
            !single.starts_with("1 edit"),
            "one op is named as the edit itself is: {single}"
        );
        let done = apply(&project, edits(two.clone()), None).unwrap();
        assert!(label(&project).starts_with("3 edits: "), "{}", label(&project));
        assert_eq!(done.label, Some(label(&project)));
        for blank in ["", "   "] {
            apply(&project, edits(two.clone()), Some(blank)).unwrap();
            assert!(label(&project).starts_with("3 edits: "), "a blank label is no label");
        }
        apply(&project, edits(one), Some("  Quieter  ")).unwrap();
        assert_eq!(label(&project), "Quieter");
    }

    #[test]
    fn ops_are_checked_before_anything_runs() {
        let (project, ids) = sample();
        let (before, revisions) = (snap(&project), history(&project).len());
        let run = |v: Value| apply(&project, edits(v), None).map(|_| ()).unwrap_err();
        assert!(run(json!([])).contains("at least one edit"));
        assert!(run(json!([{"tool": "export", "args": {}}])).starts_with("op 0 (export): not an edit"));
        assert!(run(json!([{"tool": "frobnicate"}])).contains("no such tool"));
        assert!(run(json!([
            {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}},
            {"tool": "trim", "args": {"clip_id": ids[0], "rippel": true}},
        ]))
        .starts_with("op 1 (trim): unknown argument \"rippel\" for trim"));
        assert!(run(json!([{"tool": "set_grade", "args": {"clip_id": ids[0], "lut_path": "/a.cube"}}])).contains("lut_path"));
        let many: Vec<Value> = (0..=batch::MAX_USER_OPS)
            .map(|_| json!({"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}}))
            .collect();
        assert!(run(Value::Array(many)).contains("at most"));
        assert!(serde_json::from_value::<Vec<EditOp>>(json!([{"tool": "trim", "arg": {}}])).is_err());
        // `args` is a map when it is given: `null` is none, here as in the page's harness.
        assert!(serde_json::from_value::<Vec<EditOp>>(json!([{"tool": "trim", "args": null}])).is_err());
        assert_eq!((snap(&project), history(&project).len()), (before, revisions));
    }

    #[test]
    fn the_ops_the_gui_waits_for_all_run_in_one_step() {
        let (project, ids) = sample();
        let (c1, c2) = (ids[0].as_str(), ids[1].as_str());
        let revisions = history(&project).len();
        let done = apply(
            &project,
            edits(json!([
                // The b-roll's head trimmed to 14 s, then the transcript editor's cuts: two ranges of one clip, the
                // later first so the earlier keeps its id.
                {"tool": "split_remove_clips", "args": {"cuts": [{"clip_id": c2, "at": 14.0}], "side": "left", "link": false}},
                {"tool": "cut_clip_range", "args": {"clip_id": c1, "from": 8.0, "to": 9.0, "link": false}},
                {"tool": "cut_clip_range", "args": {"clip_id": c1, "from": 2.0, "to": 3.0, "link": false}},
                {"tool": "set_video_effects", "args": {"clip_id": c1, "effects": [{"type": "grayscale"}]}},
                {"tool": "set_audio_effects", "args": {"clip_id": c1, "effects": [{"type": "highpass", "hz": 90.0}]}},
                {"tool": "set_color", "args": {"clip_id": c1, "brightness": 0.1, "saturation": 1.2}},
                {"tool": "set_grade", "args": {"clip_id": c1, "wheels": {"shadows": {"x": -0.2, "y": 0.0, "lightness": 0.0}}}},
                {"tool": "set_keyframes", "args": {"clip_id": c1, "keyframes": [
                    {"time": 0.0, "scale": 1.0, "pos_x": 0.0, "pos_y": 0.0, "rotation": 0.0, "opacity": 1.0},
                    {"time": 1.0, "scale": 1.2, "pos_x": 0.0, "pos_y": 0.0, "rotation": 0.0, "opacity": 1.0},
                ]}},
                {"tool": "set_property_tracks", "args": {"clip_id": c1, "tracks": [
                    {"prop": "contrast", "keys": [{"time": 0.0, "value": 1.0}, {"time": 1.0, "value": 1.3}]},
                ]}},
                {"tool": "clear_keyframes", "args": {"clip_id": c1}},
            ])),
            None,
        )
        .unwrap();
        assert_eq!(history(&project).len(), revisions + 1, "ten edits, one step");
        assert_eq!(done.results.len(), 10);
        let kept = done.results[0].as_array().unwrap();
        assert_eq!(kept.len(), 1, "a group trim answers the clip that remains: {kept:?}");
        let pieces = done.results[1].as_array().unwrap();
        assert!(
            pieces.len() >= 2 && pieces[0]["timeline_end"].is_number(),
            "a cut answers its pieces: {pieces:?}"
        );
        let c1 = done.timeline.clip(c1.parse().unwrap()).unwrap();
        assert_eq!(c1.effects.len(), 1);
        assert_eq!(c1.audio.len(), 1);
        assert_eq!(c1.color.brightness, 0.1);
        assert!(c1.grade.wheels.is_some());
        assert!(c1.keyframes.is_empty(), "the transform keys are cleared last");
        assert_eq!(
            c1.property_keys(kerf_core::Property::Contrast).len(),
            2,
            "a colour number's keys stay"
        );
        assert_eq!(done.timeline.clip(c2.parse().unwrap()).unwrap().source_in, 1.5);
        assert!(done.timeline.tracks[0].clips.len() > 2, "the cuts left pieces");
    }

    #[test]
    fn a_span_through_several_clips_is_one_range_edit_among_the_others() {
        let (project, ids) = sample();
        let revisions = history(&project).len();
        let done = apply(
            &project,
            edits(json!([
                {"tool": "extract_range", "args": {"from": 10.0, "to": 14.0}},
                {"tool": "set_volume", "args": {"clip_id": ids[1], "volume": 0.5}},
            ])),
            Some("Cut the span"),
        )
        .unwrap();
        assert_eq!(history(&project).len(), revisions + 1);
        assert_eq!(done.results[0]["from"], 10.0);
        assert_eq!(done.results[0]["closed"], 4.0);
        assert_eq!(done.timeline.clip(ids[1].parse().unwrap()).unwrap().volume, 0.5);
        // The single tool's own answer, for the same range on the same cut (its tracks have ids of their own).
        let (single, _) = sample();
        let mut alone = single
            .lock()
            .unwrap()
            .extract_range(10.0, 14.0, None)
            .map(|r| serde_json::to_value(r).unwrap())
            .unwrap();
        let mut batched = done.results[0].clone();
        for answer in [&mut alone, &mut batched] {
            answer["tracks"] = json!(answer["tracks"].as_array().map(Vec::len));
        }
        assert_eq!(batched, alone);
    }

    #[test]
    fn exactly_the_cap_passes_the_check_and_runs_and_one_more_does_not() {
        let (project, ids) = sample();
        let many = |n: usize| -> Vec<Value> {
            (0..n)
                .map(|i| json!({"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5 + (i % 2) as f64 / 4.0}}))
                .collect()
        };
        batch::check(&edits(Value::Array(many(batch::MAX_USER_OPS))), batch::MAX_USER_OPS).unwrap();
        let err = batch::check(&edits(Value::Array(many(batch::MAX_USER_OPS + 1))), batch::MAX_USER_OPS).unwrap_err();
        assert!(
            err.message.contains(&format!("at most {}", batch::MAX_USER_OPS)),
            "{}",
            err.message
        );
        let revisions = history(&project).len();
        let done = apply(&project, edits(Value::Array(many(batch::MAX_USER_OPS))), None).unwrap();
        assert_eq!(done.results.len(), batch::MAX_USER_OPS);
        assert_eq!(history(&project).len(), revisions + 1, "all of them, one revision");
    }

    #[test]
    fn a_failing_batch_keeps_a_redo_branch_that_was_there() {
        let (project, ids) = sample();
        apply(
            &project,
            edits(json!([{"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}}])),
            None,
        )
        .unwrap();
        project.lock().unwrap().undo().unwrap();
        let (before, revisions) = (snap(&project), history(&project).len());
        apply(
            &project,
            edits(json!([
                {"tool": "set_volume", "args": {"clip_id": ids[1], "volume": 0.25}},
                {"tool": "trim", "args": {"clip_id": "not-a-uuid", "source_in": 1.0}},
            ])),
            None,
        )
        .map(|_| ())
        .unwrap_err();
        assert_eq!((snap(&project), history(&project).len()), (before, revisions));
        let redone = project.lock().unwrap().redo().unwrap();
        assert_eq!(
            redone.clip(ids[0].parse().unwrap()).unwrap().volume,
            0.5,
            "the redo still goes forward"
        );
    }

    #[test]
    fn a_batch_that_holds_the_lock_past_its_deadline_is_refused_whole() {
        let (project, ids) = sample();
        let (before, revisions) = (snap(&project), history(&project).len());
        let ops = json!([
            {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}},
            {"tool": "set_volume", "args": {"clip_id": ids[1], "volume": 0.5}},
            {"tool": "set_clip_enabled", "args": {"clip_id": ids[1], "enabled": false}},
        ]);
        let err = apply_edits_within(&project, edits(ops.clone()), None, false, Some(std::time::Duration::ZERO))
            .map(|_| ())
            .unwrap_err();
        assert!(
            err.starts_with("op 1 (set_volume): the 1 edit before it took more than 0 s under the project lock"),
            "{err}"
        );
        assert!(err.contains("nothing was applied, send fewer edits"), "{err}");
        assert_eq!(
            (snap(&project), history(&project).len()),
            (before, revisions),
            "the first edit is undone"
        );
        // One op can never be late, and a deadline that is not reached is no refusal.
        apply_edits_within(
            &project,
            edits(ops.clone()),
            None,
            false,
            Some(std::time::Duration::from_secs(60)),
        )
        .unwrap();
        apply_edits_within(&project, edits(ops), None, false, None).unwrap();
        // The GUI's own deadline is the one `apply_edits_now` runs under.
        assert_eq!(batch::USER_DEADLINE, std::time::Duration::from_secs(4));
    }

    #[test]
    fn the_answers_a_batch_copies_into_later_ops_are_capped() {
        let (project, ids) = sample();
        let keys: Vec<Value> = (0..1000).map(|i| json!({"time": i as f64 * 0.01, "value": 0.5})).collect();
        let copies = |n: usize| -> Vec<EditOp> {
            let mut ops =
                vec![json!({"tool": "set_property_keyframes", "args": {"clip_id": ids[0], "prop": "opacity", "keys": keys}})];
            ops.extend((0..n).map(|_| {
                json!({"tool": "set_property_keyframes", "args": {"clip_id": ids[0], "prop": "opacity", "keys": "$0.channels.0.keys"}})
            }));
            edits(Value::Array(ops))
        };
        apply(&project, copies(5), None).unwrap();
        project.lock().unwrap().undo().unwrap();
        let (before, revisions) = (snap(&project), history(&project).len());
        let err = apply(&project, copies(80), None).map(|_| ()).unwrap_err();
        assert!(err.starts_with("op "), "{err}");
        assert!(
            err.contains("bytes of earlier answers") && err.contains("send fewer edits"),
            "{err}"
        );
        assert_eq!(
            (snap(&project), history(&project).len()),
            (before, revisions),
            "nothing applied, nothing recorded"
        );
    }

    #[test]
    fn the_name_of_a_revision_is_capped_and_a_text_that_starts_with_a_dollar_is_taken_literally() {
        let (project, ids) = sample();
        let name = |project: &Mutex<Project>| history(project).last().unwrap().label.clone();
        let volume = |v: f64| edits(json!([{"tool": "set_volume", "args": {"clip_id": ids[0], "volume": v}}]));
        apply(&project, volume(0.5), Some(&"a".repeat(250))).unwrap();
        assert_eq!(name(&project), format!("{}…", "a".repeat(199)));
        apply(&project, volume(0.6), Some(&"é".repeat(200))).unwrap();
        assert_eq!(
            name(&project),
            "é".repeat(200),
            "exactly the cap is kept whole, counted in characters"
        );
        apply(&project, volume(0.7), Some(&format!("  {}  ", "é".repeat(201)))).unwrap();
        assert_eq!(name(&project), format!("{}…", "é".repeat(199)));
        // A marker called `$100` goes in as `$$100`, which the batch reads as `$100` (`literal` in batch-edits.ts).
        let done = apply(
            &project,
            edits(json!([{"tool": "add_marker", "args": {"time": 1.0, "name": "$$100"}}])),
            None,
        )
        .unwrap();
        assert_eq!(done.results[0]["name"], "$100");
        assert!(project
            .lock()
            .unwrap()
            .timeline()
            .unwrap()
            .markers
            .iter()
            .any(|m| m.name == "$100"));
    }

    #[test]
    fn later_ops_use_what_earlier_ones_made() {
        let (project, ids) = sample();
        let done = apply(
            &project,
            edits(json!([
                {"tool": "split_at", "args": {"clip_id": ids[1], "at": 14.0, "link": false}},
                {"tool": "set_volume", "args": {"clip_id": "$0.right.id", "volume": 0.25}},
            ])),
            None,
        )
        .unwrap();
        let right: Uuid = done.results[0]["right"]["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(done.timeline.clip(right).unwrap().volume, 0.25);
        assert_eq!(done.timeline.clip(ids[1].parse().unwrap()).unwrap().volume, 1.0);
    }

    #[test]
    fn the_users_batch_lands_live_while_an_agents_proposal_stays_open_and_goes_stale() {
        let (project, ids) = sample();
        {
            let mut agent = project.lock().unwrap();
            agent.set_actor(EditSource::Agent);
            agent.set_agent_review(true);
            agent.set_volume(ids[1].parse().unwrap(), 0.3).unwrap();
            assert!(!agent.staged().unwrap().unwrap().stale);
        }
        let live = snap(&project);
        let revisions = history(&project).len();
        let done = apply(
            &project,
            edits(json!([
                {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}},
                {"tool": "set_clip_enabled", "args": {"clip_id": ids[0], "enabled": false}},
            ])),
            Some("Mute the open"),
        )
        .unwrap();
        let after = history(&project);
        assert_eq!(after.len(), revisions + 1, "a live revision, not a staged step");
        assert_eq!(after.last().unwrap().source, EditSource::User);
        assert_ne!(snap(&project), live);
        assert_eq!(done.timeline.clip(ids[0].parse().unwrap()).unwrap().volume, 0.5);
        let staged = project.lock().unwrap().staged().unwrap().unwrap();
        assert_eq!(staged.edits.len(), 1, "the proposal holds only the agent's own step");
        assert!(staged.stale, "and it knows the cut moved under it");
    }

    #[test]
    fn a_dry_run_answers_the_cut_the_edits_would_make_and_keeps_nothing() {
        let (project, ids) = sample();
        let (before, revisions) = (snap(&project), history(&project).len());
        let done = apply_edits_now(
            &project,
            edits(json!([{"tool": "remove", "args": {"clip_id": ids[0], "ripple": false}}])),
            Some("Drop the open"),
            true,
        )
        .unwrap();
        assert!(!done.applied && done.label.is_none());
        assert!(done.timeline.clip(ids[0].parse().unwrap()).is_none(), "the cut it would make");
        assert_eq!((snap(&project), history(&project).len()), (before, revisions));
        let applied = apply(
            &project,
            edits(json!([{"tool": "remove", "args": {"clip_id": ids[0]}}])),
            None,
        )
        .unwrap();
        assert!(applied.applied);
    }
}
