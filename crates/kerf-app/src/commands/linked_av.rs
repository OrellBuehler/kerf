//! Linked A/V and the per-clip edits that follow it: volume, speed, colour, effects, keyframes, reframe, tags, markers, overlays.

use crate::*;

// ---- linked A/V ------------------------------------------------------------

/// **Detach audio**: split a picture clip's own sound onto an audio track — a new
/// audio clip with the same span and position, linked to the picture, whose own
/// sound is muted. One revision.
#[tauri::command(async)]
pub(crate) fn detach_audio(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.detach_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// **Detach audio** from several picture clips in **one** revision. A clip that
/// cannot be detached is skipped and reported; an error only when none could be.
#[tauri::command(async)]
pub(crate) fn detach_audio_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<AudioDetached> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    let done = project.detach_audio_clips(&ids).map_err(|e| e.to_string())?;
    Ok(AudioDetached {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        detached: done.detached.len(),
        skipped: done.skipped,
    })
}

/// **Reattach audio**: delete the linked audio clip(s) carrying a picture's sound and
/// let the picture play its own again. Name either clip of the pair; refused when the
/// picture's sound is already playing from another audio clip (it would double). One
/// revision.
#[tauri::command(async)]
pub(crate) fn reattach_audio(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.reattach_audio(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// **Reattach audio** on several pictures in **one** revision, all or nothing: name
/// either clip of each pair; a pair that cannot be reattached refuses the lot.
#[tauri::command(async)]
pub(crate) fn reattach_audio_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.reattach_audio_clips(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Link clips (at least two, on different tracks) so an edit to one is carried to
/// the others. One revision.
#[tauri::command(async)]
pub(crate) fn link_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.link_clips(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Unlink clips; a group left with a single clip dissolves. One revision.
#[tauri::command(async)]
pub(crate) fn unlink_clips(state: State<'_, AppState>, clip_ids: Vec<String>) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.unlink_clips(&ids).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Paste clipboard clips. Takes clip *values*, not ids, so a cut-then-paste
/// works after the sources are gone.
#[tauri::command(async)]
pub(crate) fn insert_clips(state: State<'_, AppState>, placements: Vec<Placement>, at: f64) -> CmdResult<Timeline> {
    let items = placements
        .into_iter()
        .map(|p| id(&p.track_id, "placements[].track_id").map(|t| (t, p.clip)))
        .collect::<Result<Vec<_>, _>>()?;
    let project = state.project();
    project.insert_clips(&items, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn duplicate_clips(state: State<'_, AppState>, clip_ids: Vec<String>, at: f64) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.duplicate_clips(&ids, at).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Remove a clip — and its linked partners, unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn remove_clip(state: State<'_, AppState>, clip_id: String, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.with_links(link, |p| p.remove(id)).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Remove several clips in **one** revision. `ripple` forces ripple on or off
/// for the call; omitted, the project's ripple mode decides — so a multi-select
/// *ripple* delete is this with `ripple: true`. Linked partners of the named clips
/// go too, unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn remove_clips(
    state: State<'_, AppState>,
    clip_ids: Vec<String>,
    ripple: Option<bool>,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project
        .with_links(link, |p| p.with_ripple(ripple, |p| p.remove_clips(&ids)))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_volume(state: State<'_, AppState>, clip_id: String, volume: f32) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_volume(id, volume).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_fade(
    state: State<'_, AppState>,
    clip_id: String,
    fade_in: Option<f64>,
    fade_out: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_fade(id, fade_in, fade_out).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Retime a clip; its linked partners are retimed by the same ratio unless `link`
/// is `false`.
#[tauri::command(async)]
pub(crate) fn set_speed(state: State<'_, AppState>, clip_id: String, speed: f64, link: Option<bool>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.set_speed(id, speed))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Give a clip a speed ramp (keys in clip-local timeline seconds, empty clears it); its linked
/// partners get the same ramp unless `link` is `false`.
#[tauri::command(async)]
pub(crate) fn set_speed_ramp(
    state: State<'_, AppState>,
    clip_id: String,
    keys: Vec<SpeedKey>,
    link: Option<bool>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .with_links(link, |p| p.set_speed_ramp(id, keys))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_transform(
    state: State<'_, AppState>,
    clip_id: String,
    scale: Option<f64>,
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    rotation: Option<f64>,
    opacity: Option<f64>,
    crop_left: Option<f64>,
    crop_right: Option<f64>,
    crop_top: Option<f64>,
    crop_bottom: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .set_transform(
            id,
            scale,
            pos_x,
            pos_y,
            rotation,
            opacity,
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
        )
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// The transform as it should look `time` seconds into the clip, one revision: keyed numbers
/// get a key there, the rest (the crop always) their static value. A Preview drag writes it.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_transform_at(
    state: State<'_, AppState>,
    clip_id: String,
    time: f64,
    scale: Option<f64>,
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    rotation: Option<f64>,
    opacity: Option<f64>,
    crop_left: Option<f64>,
    crop_right: Option<f64>,
    crop_top: Option<f64>,
    crop_bottom: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    let patch = kerf_core::TransformPatch {
        scale,
        pos_x,
        pos_y,
        rotation,
        opacity,
        crop_left,
        crop_right,
        crop_top,
        crop_bottom,
    };
    project.set_transform_at(id, time, patch).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_color(
    state: State<'_, AppState>,
    clip_id: String,
    brightness: Option<f64>,
    contrast: Option<f64>,
    saturation: Option<f64>,
    gamma: Option<f64>,
    temperature: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .set_color(id, brightness, contrast, saturation, gamma, temperature)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_transition(
    state: State<'_, AppState>,
    clip_id: String,
    kind: Option<String>,
    duration: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let transition = parse_transition(kind, duration)?;
    let project = state.project();
    project.set_transition(id, transition).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Cut a clip to a shape (or clear it with `mask: null`), so a lower track shows
/// through outside it.
#[tauri::command(async)]
pub(crate) fn set_mask(state: State<'_, AppState>, clip_id: String, mask: Option<Mask>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_mask(id, mask).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Composite a clip onto the tracks below with a blend mode (`normal` is plain alpha-over).
#[tauri::command(async)]
pub(crate) fn set_blend_mode(state: State<'_, AppState>, clip_id: String, mode: BlendMode) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_blend_mode(id, mode).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Replace a clip's curves, colour wheels and LUT. A LUT file is read and checked first, with the
/// project unlocked.
#[tauri::command]
pub(crate) async fn set_grade(state: State<'_, AppState>, clip_id: String, grade: Grade) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Only a newly chosen LUT file is checked, so a curve or wheel edit still applies to a
        // clip whose LUT went missing.
        let before = {
            let timeline = lock_user(&shared).timeline().map_err(|e| e.to_string())?;
            timeline.clip(id).and_then(|c| c.grade.lut.as_ref().map(|l| l.path.clone()))
        };
        if grade.lut.as_ref().is_some_and(|l| Some(&l.path) != before.as_ref()) {
            kerf_core::color_lut::check_grade(&grade).map_err(|e| e.to_string())?;
        }
        let project = lock_user(&shared);
        project.set_grade(id, grade).map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

/// What a `.cube` file holds (its size and title), or why it cannot be used as a LUT.
#[tauri::command]
pub(crate) async fn inspect_lut(path: String) -> CmdResult<kerf_core::color_lut::LutInfo> {
    blocking(move || kerf_core::color_lut::inspect_lut(&path).map_err(|e| e.to_string())).await
}

#[tauri::command(async)]
pub(crate) fn set_video_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<VideoEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_video_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_audio_effects(state: State<'_, AppState>, clip_id: String, effects: Vec<AudioEffect>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_audio_effects(id, effects).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_keyframes(state: State<'_, AppState>, clip_id: String, keyframes: Vec<Keyframe>) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_keyframes(id, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_keyframe(
    state: State<'_, AppState>,
    clip_id: String,
    time: f64,
    scale: Option<f64>,
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    rotation: Option<f64>,
    opacity: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .add_keyframe(id, time, scale, pos_x, pos_y, rotation, opacity)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// With `prop`, one number's segment alone; without, the transform key at `time` (the
/// bundle's and every transform number's own track).
#[tauri::command(async)]
pub(crate) fn set_keyframe_easing(
    state: State<'_, AppState>,
    clip_id: String,
    time: f64,
    easing: kerf_core::Easing,
    prop: Option<Property>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    match prop {
        Some(prop) => project.set_property_easing(id, prop, time, easing),
        None => project.set_keyframe_easing(id, time, easing),
    }
    .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Replace the keys of one animatable number — a transform number, a colour number or the
/// clip's volume. No keys leaves it static.
#[tauri::command(async)]
pub(crate) fn set_property_keyframes(
    state: State<'_, AppState>,
    clip_id: String,
    prop: Property,
    keys: Vec<PropertyKey>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_property_keyframes(id, prop, keys).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Replace the keys of several animatable numbers of a clip in one edit (the dope sheet's
/// gestures).
#[tauri::command(async)]
pub(crate) fn set_property_tracks(
    state: State<'_, AppState>,
    clip_id: String,
    tracks: Vec<PropertyTrack>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_property_tracks(id, tracks).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Copy the animation of `props` (every keyed number when empty) from one clip to another,
/// `offset` seconds later.
#[tauri::command(async)]
pub(crate) fn copy_keyframes(
    state: State<'_, AppState>,
    from_clip_id: String,
    to_clip_id: String,
    props: Option<Vec<Property>>,
    offset: Option<f64>,
) -> CmdResult<Timeline> {
    let (from, to) = (id(&from_clip_id, "from_clip_id")?, id(&to_clip_id, "to_clip_id")?);
    let project = state.project();
    project
        .copy_keyframes(from, to, &props.unwrap_or_default(), offset.unwrap_or(0.0))
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// One clip a motion preset was written into, and the window it covers (seconds from the clip's start).
#[derive(serde::Serialize)]
struct MotionWindow {
    clip_id: Uuid,
    start: f64,
    end: f64,
}

/// What applying a motion preset did: the refreshed timeline and, per clip, the window used (a `duration`
/// longer than the clip is clamped to it).
#[derive(serde::Serialize)]
pub(crate) struct MotionApplication {
    timeline: Timeline,
    applied: Vec<MotionWindow>,
}

/// Write a motion preset (an entrance, an exit or an emphasis) into the animation of several clips as one
/// revision, all or nothing. `mode` defaults to `add`; `at`, `duration` and `intensity` to the preset's own.
#[tauri::command(async)]
pub(crate) fn apply_motion_preset(
    state: State<'_, AppState>,
    clip_ids: Vec<String>,
    preset: kerf_core::MotionPreset,
    mode: Option<kerf_core::MotionMode>,
    at: Option<f64>,
    duration: Option<f64>,
    intensity: Option<f64>,
) -> CmdResult<MotionApplication> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    let done = project
        .apply_motion_preset(&ids, preset, mode.unwrap_or_default(), at, duration, intensity)
        .map_err(|e| e.to_string())?;
    Ok(MotionApplication {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        applied: done
            .into_iter()
            .map(|m| MotionWindow {
                clip_id: m.clip.id,
                start: m.start,
                end: m.end,
            })
            .collect(),
    })
}

/// The motion presets a user can apply (`fade_in`, `slide_out_to_left`, `pulse`, ...), entrances first, then
/// exits, then emphases. Needs no project.
#[tauri::command(async)]
pub(crate) fn list_motion_presets() -> Vec<kerf_core::MotionPresetInfo> {
    kerf_core::motion_presets()
}

/// Replace the live behaviours (sway, breathe, spin, drift, shake) of several clips with one list as one
/// revision, all or nothing; an empty list clears them. They ride on top of the clip's pose and keys.
#[tauri::command(async)]
pub(crate) fn set_behaviors(
    state: State<'_, AppState>,
    clip_ids: Vec<String>,
    behaviors: Vec<kerf_core::Behavior>,
) -> CmdResult<Timeline> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    project.set_behaviors(&ids, behaviors).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// What baking behaviours into keys did to one clip: the numbers that now carry keys, how many each, and the
/// frame rate they sit on.
#[derive(serde::Serialize)]
struct BakeReport {
    clip_id: Uuid,
    properties: Vec<Property>,
    keys: usize,
    fps: f64,
}

/// What a bake did: the refreshed timeline and, per clip, what was written.
#[derive(serde::Serialize)]
pub(crate) struct BakeApplication {
    timeline: Timeline,
    baked: Vec<BakeReport>,
}

/// Sample the behaviours of several clips into keys of the numbers they move and clear them, as one
/// revision, all or nothing. `fps` defaults to the delivery's rate, else the footage's, else 30.
#[tauri::command(async)]
pub(crate) fn bake_behaviors(state: State<'_, AppState>, clip_ids: Vec<String>, fps: Option<f64>) -> CmdResult<BakeApplication> {
    let ids = ids(&clip_ids, "clip_ids")?;
    let project = state.project();
    let done = project.bake_behaviors(&ids, fps).map_err(|e| e.to_string())?;
    Ok(BakeApplication {
        timeline: project.timeline().map_err(|e| e.to_string())?,
        baked: done
            .into_iter()
            .map(|b| BakeReport {
                clip_id: b.clip.id,
                properties: b.baked.properties,
                keys: b.baked.keys,
                fps: b.baked.fps,
            })
            .collect(),
    })
}

#[tauri::command(async)]
pub(crate) fn clear_keyframes(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.clear_keyframes(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_reframe(
    state: State<'_, AppState>,
    clip_id: String,
    yaw: Option<f64>,
    pitch: Option<f64>,
    roll: Option<f64>,
    fov: Option<f64>,
    lens_fov: Option<f64>,
    input: Option<Projection>,
    output: Option<Projection>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .set_reframe(id, yaw, pitch, roll, fov, lens_fov, input, output)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Mark an asset as 360 footage (or clear the mark) for footage the probe could
/// not identify. Unlike `set_reframe` this sticks to the asset, so every clip cut
/// from it afterwards is reframed.
#[tauri::command]
pub(crate) async fn set_asset_projection(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_id: String,
    projection: Option<Projection>,
) -> CmdResult<Asset> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    // On the blocking pool: queueing the proxy runs the cached ffprobe that keys it.
    blocking(move || {
        let asset = lock_user(&shared)
            .set_asset_projection(id, projection)
            .map_err(|e| e.to_string())?;
        // 360 assets proxy at a different size, so the cached proxy no longer matches.
        spawn_proxy(&app, &asset);
        Ok(asset)
    })
    .await
}

/// Add, take off or replace tags on assets (`edit`: `add`, `remove`, `remove_keys`, applied in that
/// order after the keys go), all in one transaction. Returns the assets as they are now. Not an edit of the cut.
#[tauri::command(async)]
pub(crate) fn edit_asset_tags(state: State<'_, AppState>, asset_ids: Vec<String>, edit: TagEdit) -> CmdResult<Vec<Asset>> {
    let ids = ids(&asset_ids, "asset_ids")?;
    state.project().edit_asset_tags(&ids, &edit).map_err(|e| e.to_string())
}

/// Every tag in use across the library: the keys with their values and asset counts, and the plain tags.
#[tauri::command(async)]
pub(crate) fn list_tags(state: State<'_, AppState>) -> CmdResult<TagSummary> {
    state.project().list_tags().map_err(|e| e.to_string())
}

/// What [`assemble_by_tag`] made: the refreshed cut and the groups laid out in it.
#[derive(Serialize)]
pub(crate) struct AssembleEdit {
    timeline: Timeline,
    #[serde(flatten)]
    summary: AssembleSummary,
}

/// Lay the library out at the end of the cut grouped by the value of one tag key, a title per group. The
/// groups are resolved under the lock, the titles drawn and read with it released, and the clips placed
/// under it again.
#[tauri::command]
pub(crate) async fn assemble_by_tag(state: State<'_, AppState>, request: AssembleByTag) -> CmdResult<AssembleEdit> {
    let shared = state.project.clone();
    blocking(move || {
        let plan = lock_user(&shared)
            .assemble_by_tag_inputs(&request)
            .map_err(|e| e.to_string())?;
        let prepared = plan.prepare().map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        let summary = project.apply_assemble_by_tag(prepared).map_err(|e| e.to_string())?;
        Ok(AssembleEdit {
            timeline: project.timeline().map_err(|e| e.to_string())?,
            summary,
        })
    })
    .await
}

#[tauri::command(async)]
pub(crate) fn clear_reframe(state: State<'_, AppState>, clip_id: String) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.clear_reframe(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_reframe_keyframes(
    state: State<'_, AppState>,
    clip_id: String,
    keyframes: Vec<ReframeKeyframe>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project.set_reframe_keyframes(id, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn add_reframe_keyframe(
    state: State<'_, AppState>,
    clip_id: String,
    time: f64,
    yaw: Option<f64>,
    pitch: Option<f64>,
    roll: Option<f64>,
    fov: Option<f64>,
) -> CmdResult<Timeline> {
    let id = id(&clip_id, "clip_id")?;
    let project = state.project();
    project
        .add_reframe_keyframe(id, time, yaw, pitch, roll, fov)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn add_marker(state: State<'_, AppState>, time: f64, name: String, color: Option<String>) -> CmdResult<Timeline> {
    let project = state.project();
    project.add_marker(time, name, color).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_marker(
    state: State<'_, AppState>,
    marker_id: String,
    time: Option<f64>,
    name: Option<String>,
    color: Option<String>,
    duration: Option<f64>,
    comment: Option<String>,
    kind: Option<kerf_core::MarkerKind>,
) -> CmdResult<Timeline> {
    let id = id(&marker_id, "marker_id")?;
    let patch = kerf_core::MarkerPatch {
        time,
        name,
        color,
        duration,
        comment,
        kind,
    };
    let project = state.project();
    project.edit_marker(id, patch).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn remove_marker(state: State<'_, AppState>, marker_id: String) -> CmdResult<Timeline> {
    let id = id(&marker_id, "marker_id")?;
    let project = state.project();
    project.remove_marker(id).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn add_overlay(state: State<'_, AppState>, text: String, start: f64, end: f64) -> CmdResult<Timeline> {
    let project = state.project();
    project.add_overlay(text, start, end).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_overlay(
    state: State<'_, AppState>,
    overlay_id: String,
    text: Option<String>,
    start: Option<f64>,
    end: Option<f64>,
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    size: Option<f64>,
    color: Option<String>,
    bg: Option<String>,
    font: Option<String>,
    bold: Option<bool>,
) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project
        .update_overlay(oid, text, start, end, pos_x, pos_y, size, color, bg, font, bold)
        .map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn remove_overlay(state: State<'_, AppState>, overlay_id: String) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project.remove_overlay(oid).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub(crate) fn set_overlay_keyframes(
    state: State<'_, AppState>,
    overlay_id: String,
    keyframes: Vec<TextKeyframe>,
) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project.set_overlay_keyframes(oid, keyframes).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// Reading the artwork (sanitise + parse) takes up to a second and a half for a heavy document, so
/// it runs on the blocking pool *before* the project lock is taken; the lock then only places it.
#[tauri::command]
pub(crate) async fn add_svg_overlay(
    state: State<'_, AppState>,
    source: SvgSource,
    start: f64,
    end: Option<f64>,
    layout: Option<SvgLayout>,
) -> CmdResult<Timeline> {
    let shared = state.project.clone();
    blocking(move || {
        let checked = source.check().map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        project
            .add_svg_overlay_parsed(checked, start, end, layout.unwrap_or_default())
            .map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn update_svg_overlay(
    state: State<'_, AppState>,
    overlay_id: String,
    patch: SvgOverlayPatch,
) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let shared = state.project.clone();
    blocking(move || {
        let doc = patch.check().map_err(|e| e.to_string())?;
        let project = lock_user(&shared);
        project
            .update_svg_overlay_parsed(oid, patch, doc)
            .map_err(|e| e.to_string())?;
        project.timeline().map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command(async)]
pub(crate) fn remove_svg_overlay(state: State<'_, AppState>, overlay_id: String) -> CmdResult<Timeline> {
    let oid = id(&overlay_id, "overlay_id")?;
    let project = state.project();
    project.remove_svg_overlay(oid).map_err(|e| e.to_string())?;
    project.timeline().map_err(|e| e.to_string())
}

/// What `add_svg_overlay` / `update_svg_overlay` would make of this source: the sanitised
/// document, what the sanitiser or the SMIL reader could not honour, and how long it runs.
/// Needs no project, so it never waits for the lock, and parses on the blocking pool.
#[tauri::command]
pub(crate) async fn validate_svg(svg: String) -> CmdResult<SvgCheck> {
    blocking(move || kerf_core::check_svg(&svg).map_err(|e| e.to_string())).await
}

#[tauri::command(async)]
pub(crate) fn list_svg_presets() -> Vec<SvgPresetInfo> {
    kerf_core::svg::preset_infos()
}

/// The artwork a preset would make for the project's delivery frame, as SVG text (the page shows
/// it through an `<img>` data URL, never inline). `duration` defaults to 4 s.
#[tauri::command(async)]
pub(crate) fn svg_preset_preview(
    state: State<'_, AppState>,
    kind: SvgPresetKind,
    params: SvgPresetParams,
    duration: Option<f64>,
) -> CmdResult<String> {
    kerf_core::svg::check_params(&params).map_err(|e| e.to_string())?;
    let frame = state.project().timeline().map_err(|e| e.to_string())?.preset_frame();
    Ok(kerf_core::render_preset(kind, &params, duration.unwrap_or(4.0), frame))
}
