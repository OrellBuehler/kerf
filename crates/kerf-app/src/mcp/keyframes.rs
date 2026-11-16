//! Keyframes, per-property tracks, motion presets, behaviours and 360 reframing.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct KeyframesParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Whole-transform keyframes (replaces the clip's bundle of them). Each: {\"time\":seconds_from_clip_start, \
                       \"scale\":1.0,\"pos_x\":0.0,\"pos_y\":0.0,\"rotation\":0.0,\"opacity\":1.0}. Two or more animate \
                       the clip; pass [] to clear these keyframes (a number with keys of its own, from \
                       set_property_keyframes, keeps them — clear_keyframes clears everything of the transform)."
    )]
    keyframes: Vec<Keyframe>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ReframeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Camera heading in degrees, wrapping at ±180 (0 = the source's forward direction, 90 = to the \
                       right); omit to leave unchanged"
    )]
    yaw: Option<f64>,
    #[schemars(description = "Camera elevation in degrees, -90 (straight down) to 90 (straight up); omit to leave unchanged")]
    pitch: Option<f64>,
    #[schemars(description = "Horizon tilt in degrees; omit to leave unchanged")]
    roll: Option<f64>,
    #[schemars(
        description = "Diagonal field of view in degrees, 1–359. ~100 is a natural-looking shot, lower zooms in, \
                       above ~150 goes fisheye-wide; omit to leave unchanged"
    )]
    fov: Option<f64>,
    #[schemars(
        description = "Field of view of each physical lens in degrees, for a dual_fisheye source only (default 190). \
                       Tuning this moves the stitch seam; omit to leave unchanged"
    )]
    lens_fov: Option<f64>,
    #[schemars(
        description = "Override the source's projection when detection got it wrong: \"equirect\", \"dual_fisheye\" or \
                       \"fisheye\". Required to reframe an asset Kerf did not detect as 360"
    )]
    input: Option<Projection>,
    #[schemars(
        description = "What to render: \"flat\" (default — an ordinary rectilinear shot) or \"equirect\" (stitch a \
                       dual-fisheye source without choosing a direction)"
    )]
    output: Option<Projection>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ReframeKeyframesParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Camera keyframes (replaces the clip's animation). Each: {\"time\":seconds_from_clip_start, \
                       \"yaw\":0.0,\"pitch\":0.0,\"roll\":0.0,\"fov\":100.0}. Two or more pan the virtual camera over \
                       the clip; pass [] to clear and hold the static pose."
    )]
    keyframes: Vec<ReframeKeyframe>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddReframeKeyframeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Keyframe time in seconds from the clip's start")]
    time: f64,
    #[schemars(description = "Camera heading in degrees at this time; omit to capture the current value")]
    yaw: Option<f64>,
    #[schemars(description = "Camera elevation in degrees (-90..90) at this time; omit to capture current")]
    pitch: Option<f64>,
    #[schemars(description = "Horizon tilt in degrees at this time; omit to capture current")]
    roll: Option<f64>,
    #[schemars(description = "Diagonal field of view in degrees at this time; omit to capture current")]
    fov: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddKeyframeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Keyframe time in seconds from the clip's start")]
    time: f64,
    #[schemars(description = "Scale at this time (1.0 = fit); omit to capture the current value")]
    scale: Option<f64>,
    #[schemars(description = "Horizontal position as a frame-width fraction (0 = centered); omit to capture current")]
    pos_x: Option<f64>,
    #[schemars(description = "Vertical position as a frame-height fraction (0 = centered); omit to capture current")]
    pos_y: Option<f64>,
    #[schemars(description = "Rotation in degrees; omit to capture current")]
    rotation: Option<f64>,
    #[schemars(description = "Opacity 0.0–1.0; omit to capture current")]
    opacity: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct PropertyKeyframesParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "The number to animate: scale, pos_x, pos_y, rotation, opacity (the transform), brightness, \
                       contrast, saturation, gamma, temperature (the colour grade), volume (the clip's own gain), \
                       mask_x, mask_y, mask_width, mask_height, mask_feather (the clip's mask — it must have one, \
                       set_mask) or crop_zoom, crop_x, crop_y (the animated crop: a zoom into the picture and where \
                       its window sits)"
    )]
    prop: Property,
    #[schemars(
        description = "The keys (replaces this number's animation; pass [] to make it static again). Each: \
                       {\"time\":seconds_from_clip_start, \"value\":number, \"easing\":optional, same values as \
                       set_keyframe_easing}. A key's easing shapes the segment that leaves it. Ranges are the static \
                       setters': opacity 0–1, brightness and temperature -1–1, contrast 0–4, saturation 0–3, gamma \
                       0.1–10, volume 0–4 (linear gain), scale > 0, mask_x / mask_y / mask_feather 0–1, mask_width / \
                       mask_height 0.01–2 (fractions of the frame), crop_zoom 1–10 (1 = the whole picture, 2 = half \
                       of each side), crop_x / crop_y -1–1 (-1 = the window against the left / top edge, 0 = centred, \
                       1 = right / bottom). At most 1000 keys."
    )]
    keys: Vec<PropertyKey>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct PropertyTracksParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "The numbers to re-key, each {\"prop\":name, \"keys\":[...]} exactly as set_property_keyframes \
                       takes its prop and keys (pass [] keys to make that number static). Each number at most once; \
                       numbers not listed are untouched."
    )]
    pub(super) tracks: Vec<PropertyTrackArg>,
}

/// One number's keys for `set_property_tracks`: the model's [`PropertyTrack`] with `keys` required
/// and a key it does not name (`key`) refused — the persisted type reads either as no keys, which
/// would silently make the number static.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PropertyTrackArg {
    prop: Property,
    keys: Vec<PropertyKey>,
}

impl From<PropertyTrackArg> for PropertyTrack {
    fn from(a: PropertyTrackArg) -> Self {
        Self {
            prop: a.prop,
            keys: a.keys,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct CopyKeyframesParams {
    #[schemars(description = "UUID of the clip to copy the animation from")]
    from_clip_id: String,
    #[schemars(description = "UUID of the clip to give it to (its own keys for the copied numbers are replaced)")]
    to_clip_id: String,
    #[schemars(
        description = "The numbers to copy (any set_property_keyframes takes: scale, pos_x, ..., volume, mask_x, ..., \
                       crop_zoom, crop_x, crop_y); omit or [] for every number the source animates"
    )]
    props: Option<Vec<Property>>,
    #[schemars(
        description = "Seconds to shift the keys later (negative: earlier, cutting off what falls before the \
                       destination's start and pinning the pose it opens on). Default 0."
    )]
    offset: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct MotionPresetParams {
    #[schemars(
        description = "UUIDs of the picture clips to animate (a clip on an audio track refuses the whole call); a \
                       clip named twice counts once"
    )]
    pub(super) clip_ids: Vec<String>,
    #[schemars(description = "The preset, by id (list_motion_presets says what each does and how long it runs)")]
    pub(super) preset: kerf_core::MotionPreset,
    #[schemars(
        description = "\"add\" (default) merges into the clip's animation: its keys of the numbers the preset \
                       drives that fall inside the window are replaced, the ones outside stay, so an entrance, an \
                       exit and a mid-clip pulse can coexist (where two windows overlap the one applied last wins). \
                       \"replace\" gives those numbers exactly the preset's keys; the clip's other numbers are \
                       untouched either way"
    )]
    pub(super) mode: Option<kerf_core::MotionMode>,
    #[schemars(
        description = "Where the window starts, in seconds from the clip's start. Default: 0 for an entrance or an \
                       emphasis; an exit ends at the clip's end (it starts `duration` before it). Give it to put an \
                       emphasis or an entrance mid-clip"
    )]
    pub(super) at: Option<f64>,
    #[schemars(
        description = "How long the window is, in seconds (at least 0.01). Default: the preset's own (push_in and \
                       pull_out: the rest of the clip). Longer than the clip allows, it is clamped to what is left, \
                       not refused: a 0.5 s fade on a 0.3 s clip is a 0.3 s fade"
    )]
    pub(super) duration: Option<f64>,
    #[schemars(
        description = "How far the motion departs from the clip's own pose, 0.25 to 2 (default 1): 0.5 slides, \
                       turns or grows half as far, 2 twice as far. A scale never goes to nothing and an opacity \
                       stays in 0..1"
    )]
    pub(super) intensity: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetBehaviorsParams {
    #[schemars(
        description = "UUIDs of the picture clips to give these behaviours (a clip on an audio track refuses the \
                       whole call); a clip named twice counts once"
    )]
    pub(super) clip_ids: Vec<String>,
    #[schemars(
        description = "The clip's whole list of behaviours, at most 8, replacing what each clip has; [] clears them. \
                       Each is tagged by `kind`: sway {amount: degrees 0..180 each way, speed: Hz 0..30}; breathe \
                       {amount: fraction of the zoom 0..0.9, speed: Hz 0..30}; spin {speed: turns a second -10..10, \
                       negative turns the other way}; drift {speed: frame widths (heights) a second -5..5, angle: \
                       degrees from the right, 90 is straight down (default 0)}; shake {amount: frame fraction \
                       0..0.25, speed: Hz 0..30, seed: integer (default 0), the same seed is the same shake}. Every \
                       kind takes `phase`, seconds added to the clip's clock (default 0). A number outside its range \
                       refuses the call; one at 0 (an amount or a speed) is kept but moves nothing"
    )]
    pub(super) behaviors: Vec<kerf_core::Behavior>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct BakeBehaviorsParams {
    #[schemars(
        description = "UUIDs of the picture clips to bake (a clip with no behaviours, or one a bake would put too many \
                       keys on, refuses the whole call and says which); a clip named twice counts once"
    )]
    pub(super) clip_ids: Vec<String>,
    #[schemars(
        description = "The frame rate to sample at, 1 to 1000. Default: the delivery format's rate, else the first \
                       picture's footage, else 30. Match the rate you export at: a bake is exact on those frames and a \
                       straight line between them"
    )]
    pub(super) fps: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetKeyframeEasingParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Time of the keyframe, in seconds from the clip's start (within a millisecond)")]
    time: f64,
    #[schemars(
        description = "One number whose segment to shape (see set_property_keyframes); omit to shape the transform \
                       keyframe at that time (the whole-transform keys and any transform number keyed on its own)"
    )]
    prop: Option<Property>,
    #[schemars(
        description = "How the clip travels from this keyframe's pose to the next: \"linear\" (default), \"hold\" \
                       (stay, then jump at the next key), \"ease_in\", \"ease_out\", \"ease_in_out\", or \
                       {\"bezier\": {\"x1\", \"y1\", \"x2\", \"y2\"}} with every control point in 0..=1 (CSS cubic-bezier, \
                       no overshoot)"
    )]
    easing: kerf_core::Easing,
}

#[tool_router(router = tool_router_keyframes, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Replace a clip's whole-transform keyframes — the keys that animate scale / position / rotation / opacity together over time. Two or more keyframes animate the clip (e.g. a Ken Burns zoom, a moving picture-in-picture). An empty list clears these keyframes only: a number that has keys of its own (set_property_keyframes) keeps them, so the clip can still be animated afterwards — clear_keyframes makes the whole transform static. Colour and volume keys are never touched."
    )]
    fn set_keyframes(&self, Parameters(p): Parameters<KeyframesParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_keyframes_op(project, p))
    }

    pub(super) fn set_keyframes_op(project: &Project, p: KeyframesParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.set_keyframes(clip_id, p.keyframes).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Add (or replace) one transform keyframe at a time offset from the clip's start; unspecified channels capture the clip's current pose there. Use two calls to animate between two poses. A key added inside an eased segment splits it (a hold stays held, a curve stays the same curve); re-keying a moment keeps how it leaves."
    )]
    fn add_keyframe(&self, Parameters(p): Parameters<AddKeyframeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::add_keyframe_op(project, p))
    }

    pub(super) fn add_keyframe_op(project: &Project, p: AddKeyframeParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .add_keyframe(clip_id, p.time, p.scale, p.pos_x, p.pos_y, p.rotation, p.opacity)
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Set the easing of the segment leaving one keyframe: how the clip moves from that pose \
                       to the next (ease in / out / in-out, a hold, or a custom cubic bezier). Keys are linear until set. \
                       Without prop it shapes the transform keyframe at that time; with prop, that one number's key \
                       (a colour number, the volume, or a transform number). Returns the clip."
    )]
    fn set_keyframe_easing(&self, Parameters(p): Parameters<SetKeyframeEasingParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_keyframe_easing_op(project, p))
    }

    pub(super) fn set_keyframe_easing_op(project: &Project, p: SetKeyframeEasingParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = match p.prop {
            Some(prop) => project.set_property_easing(clip_id, prop, p.time, p.easing),
            None => project.set_keyframe_easing(clip_id, p.time, p.easing),
        }
        .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Animate ONE number of a clip with keys of its own: a transform number (scale, pos_x, pos_y, \
                       rotation, opacity), a colour number (brightness, contrast, saturation, gamma, temperature), \
                       the clip's volume (a linear gain — a swell, a duck, an audio fade that is not at the clip's ends), \
                       a number of its mask (a mask that follows a face, grows or softens; the clip needs a mask first) \
                       or the animated crop (crop_zoom / crop_x / crop_y: a punch-in or a pan across the shot that \
                       stays inside the picture; on top of any static crop edges). Numbers are independent: key the opacity at other moments than the zoom, grade a shot that \
                       warms over its length, ride the dialogue's level. Replaces that number's keys (pass [] to make \
                       it static); the clip's other numbers are untouched, and a number that set_keyframes was \
                       animating is taken over by its own keys while the rest of the transform keeps them. Each key: \
                       time (seconds from the clip's start), value, and the easing of the segment leaving it. While a \
                       number is animated its static value (set_color, set_volume, set_transform) is not used. \
                       Returns the clip."
    )]
    fn set_property_keyframes(&self, Parameters(p): Parameters<PropertyKeyframesParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_property_keyframes_op(project, p))
    }

    pub(super) fn set_property_keyframes_op(project: &Project, p: PropertyKeyframesParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.set_property_keyframes(clip_id, p.prop, p.keys).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Re-key SEVERAL numbers of one clip in a single edit — the same keys set_property_keyframes \
                       takes, for each number listed: retime a move and its fade together, or set a whole look's \
                       colour keys at once, as one undo step. Numbers not listed are untouched. Returns the clip."
    )]
    fn set_property_tracks(&self, Parameters(p): Parameters<PropertyTracksParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_property_tracks_op(project, p))
    }

    pub(super) fn set_property_tracks_op(project: &Project, p: PropertyTracksParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let tracks = p.tracks.into_iter().map(PropertyTrack::from).collect();
        let out = project.set_property_tracks(clip_id, tracks).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Copy the animation of a clip's numbers (all of them, or the props listed) to another clip, \
                       shifted by offset seconds — give a second shot the same push-in, the same warm-up, the same \
                       volume swell. The destination's own keys for those numbers are replaced. Returns the \
                       destination clip."
    )]
    fn copy_keyframes(&self, Parameters(p): Parameters<CopyKeyframesParams>) -> Result<String, McpError> {
        self.edit(|project| Self::copy_keyframes_op(project, p))
    }

    pub(super) fn copy_keyframes_op(project: &Project, p: CopyKeyframesParams) -> Result<String, McpError> {
        let (from, to) = (
            parse_arg("from_clip_id", &p.from_clip_id)?,
            parse_arg("to_clip_id", &p.to_clip_id)?,
        );
        let out = project
            .copy_keyframes(from, to, &p.props.unwrap_or_default(), p.offset.unwrap_or(0.0))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "List the motion presets apply_motion_preset takes by `id`, in the order entrances, exits, \
                       emphases: each with its label, `kind`, `default_duration` in seconds (absent for push_in and \
                       pull_out, which fill the rest of the clip), a `hint` saying what it looks like and the \
                       `properties` (scale, pos_x, pos_y, rotation, opacity) it keys."
    )]
    pub(super) fn list_motion_presets(&self) -> Result<String, McpError> {
        json(&kerf_core::motion_presets())
    }

    #[tool(
        description = "Give clips a ready-made motion in one call, written as eased keyframes on scale / position / \
                       rotation / opacity. ENTRANCES: fade_in, slide_in_from_left, slide_in_from_right, \
                       slide_in_from_top, slide_in_from_bottom, zoom_in, pop_in, spin_in, drop_in. EXITS: fade_out, \
                       slide_out_to_left, slide_out_to_right, slide_out_to_top, slide_out_to_bottom, zoom_out, \
                       pop_out, spin_out, drop_out. EMPHASES: pulse, wiggle, bounce, push_in (a slow zoom in over the \
                       whole clip), pull_out. A direction names where the picture comes from (slide_in_from_left) or \
                       goes to (slide_out_to_left). It is relative to the clip's own pose: an entrance ends on the \
                       clip's own scale, position, rotation and opacity, an exit starts from them, an emphasis rides \
                       on top, so it suits a clip that is already zoomed or positioned. Entrances and emphases start at \
                       the clip's start, exits end at its end (`at` moves the window). mode `add` (default) merges into \
                       the clip's existing keys inside the window and keeps the ones outside, so a fade_in, a fade_out \
                       and a mid-clip pulse coexist; `replace` replaces those numbers' keys. `duration` is clamped to \
                       the clip, so check the window each clip got in the result. One revision for all clips, all or \
                       nothing; a clip on an audio track is refused. Returns `applied`: per clip, its `clip_id` and the \
                       `start` / `end` of the window used, in seconds from the clip's start."
    )]
    pub(super) fn apply_motion_preset(&self, Parameters(p): Parameters<MotionPresetParams>) -> Result<String, McpError> {
        self.edit(|project| Self::apply_motion_preset_op(project, p))
    }

    pub(super) fn apply_motion_preset_op(project: &Project, p: MotionPresetParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let mode = p.mode.unwrap_or_default();
        let done = project
            .apply_motion_preset(&ids, p.preset, mode, p.at, p.duration, p.intensity)
            .map_err(core_err)?;
        let applied: Vec<serde_json::Value> = done
            .iter()
            .map(|m| serde_json::json!({"clip_id": m.clip.id, "start": m.start, "end": m.end}))
            .collect();
        json(&serde_json::json!({"preset": p.preset, "mode": mode, "applied": applied}))
    }

    #[tool(
        description = "Give clips live behaviours: procedural motion that rides on top of the clip's pose and its \
                       keyframes and never has to be keyed. sway rocks the picture (rotation, `amount` degrees each way \
                       at `speed` Hz); breathe pulses the zoom (`amount` as a fraction of the scale, at most 0.9, \
                       `speed` Hz); spin turns it steadily (`speed` turns a second, negative the other way); drift \
                       slides it steadily (`speed` frame widths / heights a second along `angle` degrees from the \
                       right, 90 is down); shake is a handheld jitter (`amount` of the frame, at most 0.25, at `speed` \
                       Hz, `seed` picks which shake and the same seed is the same shake everywhere). Each takes `phase`, \
                       seconds added to the clip's clock. The list REPLACES what each clip has (read it first with \
                       get_timeline_state: `behaviors` on the clip), at most 8, and [] clears them; several of a kind add \
                       up. Only scale, position and rotation move, never opacity, colour, mask or crop. Behaviours \
                       export exactly as shown (a short expression, not keys), a split or a range export carries the motion \
                       on from where it was, and a later key edit (set_transform_at, add_keyframe, a motion \
                       preset) writes the pose without them. The GPU preview falls back to FFmpeg for a clip with \
                       them. One revision for all clips, all or nothing; a clip on an audio track is refused. To make \
                       the motion editable as keys, bake_behaviors. Returns each clip's `clip_id` and `behaviors`."
    )]
    pub(super) fn set_behaviors(&self, Parameters(p): Parameters<SetBehaviorsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_behaviors_op(project, p))
    }

    pub(super) fn set_behaviors_op(project: &Project, p: SetBehaviorsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let done = project.set_behaviors(&ids, p.behaviors).map_err(core_err)?;
        let clips: Vec<serde_json::Value> = done
            .iter()
            .map(|c| serde_json::json!({"clip_id": c.id, "behaviors": c.behaviors}))
            .collect();
        json(&serde_json::json!({ "clips": clips }))
    }

    #[tool(
        description = "Turn clips' live behaviours (set_behaviors) into ordinary keyframes and clear the behaviours: \
                       the combined motion (the clip's own keys with the behaviours on top) is sampled at every frame of \
                       an `fps` grid into linear keys on scale, position and rotation, so it can then be edited, eased or \
                       retimed like any animation. Exact at those frames and a straight line between them, so bake at \
                       the rate you export at. A clip holds at most 1000 keys a number (about 33 s at 30 fps): a longer \
                       clip is refused with a message saying how long a piece can be (split it first) or keep the \
                       behaviour live, which costs the export nothing. A clip with no behaviours is refused. One \
                       revision for all clips, all or nothing. Returns per clip its `clip_id`, the `properties` that now \
                       carry keys, the `keys` written to each and the `fps`."
    )]
    pub(super) fn bake_behaviors(&self, Parameters(p): Parameters<BakeBehaviorsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::bake_behaviors_op(project, p))
    }

    pub(super) fn bake_behaviors_op(project: &Project, p: BakeBehaviorsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let done = project.bake_behaviors(&ids, p.fps).map_err(core_err)?;
        let baked: Vec<serde_json::Value> = done
            .iter()
            .map(|b| {
                serde_json::json!({
                    "clip_id": b.clip.id,
                    "properties": b.baked.properties,
                    "keys": b.baked.keys,
                    "fps": b.baked.fps,
                })
            })
            .collect();
        json(&serde_json::json!({ "baked": baked }))
    }

    #[tool(
        description = "Remove all transform keyframes from a clip (back to its static transform): the whole-transform keys and any scale / position / rotation / opacity keys of their own. Colour and volume keys stay — clear those with set_property_keyframes and an empty list."
    )]
    fn clear_keyframes(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::clear_keyframes_op(project, p))
    }

    pub(super) fn clear_keyframes_op(project: &Project, p: ClipIdParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.clear_keyframes(clip_id).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Aim a 360 clip's virtual camera — yaw / pitch / roll / field of view — to render an ordinary \
                       rectilinear shot out of spherical footage. Clips cut from 360 assets are reframed by default, \
                       so this adjusts where the camera points. Call preview_timeline first to see the whole sphere \
                       and decide where the subject is. Omit a field to leave it unchanged."
    )]
    fn set_reframe(&self, Parameters(p): Parameters<ReframeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_reframe_op(project, p))
    }

    pub(super) fn set_reframe_op(project: &Project, p: ReframeParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .set_reframe(clip_id, p.yaw, p.pitch, p.roll, p.fov, p.lens_fov, p.input, p.output)
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Stop reprojecting a 360 clip, leaving its raw spherical picture (equirect or dual-fisheye) on \
                       the timeline."
    )]
    fn clear_reframe(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::clear_reframe_op(project, p))
    }

    pub(super) fn clear_reframe_op(project: &Project, p: ClipIdParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.clear_reframe(clip_id).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Replace a 360 clip's camera keyframes to pan the virtual camera over the clip — following a \
                       subject, whipping between two points of interest, or pushing in by narrowing the field of \
                       view. Pass an empty list to hold a static pose."
    )]
    fn set_reframe_keyframes(&self, Parameters(p): Parameters<ReframeKeyframesParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_reframe_keyframes_op(project, p))
    }

    pub(super) fn set_reframe_keyframes_op(project: &Project, p: ReframeKeyframesParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.set_reframe_keyframes(clip_id, p.keyframes).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Add (or replace) one 360 camera keyframe at a time offset from the clip's start; unspecified \
                       channels capture the camera's current pose there. Two calls pan between two directions."
    )]
    fn add_reframe_keyframe(&self, Parameters(p): Parameters<AddReframeKeyframeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::add_reframe_keyframe_op(project, p))
    }

    pub(super) fn add_reframe_keyframe_op(project: &Project, p: AddReframeKeyframeParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .add_reframe_keyframe(clip_id, p.time, p.yaw, p.pitch, p.roll, p.fov)
            .map_err(core_err)?;
        json(&out)
    }
}
