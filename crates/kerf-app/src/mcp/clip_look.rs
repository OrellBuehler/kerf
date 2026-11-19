//! A clip's look and sound: volume, speed, transform, colour, effects, masks, blend, stabilisation and smart crop.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SmartCropParams {
    #[schemars(description = "UUID of the clip to reframe; every clip on an unlocked video track when omitted")]
    clip_id: Option<String>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StabilizeParams {
    #[schemars(description = "UUID of the clip to stabilize: a clip of moving footage on a video track")]
    clip_id: String,
    #[schemars(description = "false stops stabilizing the clip (default true)")]
    enabled: Option<bool>,
    #[schemars(
        description = "How much of the camera's movement is smoothed away, 0 to 1: the radius of the smoothing of the camera's path, up to a second of frames either side. 0.3 keeps a deliberate walk's sway, 0.5 makes a hand-held walk steady, 1 holds a slow pan nearly still. Omitted keeps the clip's current strength (0.5 for a clip not yet stabilized)."
    )]
    strength: Option<f64>,
    #[schemars(
        description = "Enlarge the picture by this many percent (0 to 30) to hide the edges the correction moves into view. A fixed small zoom keeps more of the frame but may show black at the edges on a strong shake. Omitted keeps the clip's current zoom (automatic for a clip not yet stabilized); auto_zoom: true goes back to automatic."
    )]
    zoom: Option<f64>,
    #[schemars(
        description = "true goes back to the automatic zoom: the least that keeps the moved edges out of view over the measured footage. Not with zoom."
    )]
    auto_zoom: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StabilizationStatusParams {
    #[schemars(description = "UUIDs of the clips to report on; every stabilized clip when omitted")]
    clip_ids: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetMaskParams {
    #[schemars(description = "UUID of the clip to mask")]
    clip_id: String,
    #[schemars(
        description = "Shape outline: \"rect\" or \"ellipse\". Omit to CLEAR the mask entirely. Every other field, when omitted, keeps the clip's current value (or its default on a fresh mask)"
    )]
    shape: Option<String>,
    #[schemars(description = "Centre of the shape across the frame, 0 = left edge, 1 = right (default 0.5)")]
    x: Option<f64>,
    #[schemars(description = "Centre of the shape down the frame, 0 = top, 1 = bottom (default 0.5)")]
    y: Option<f64>,
    #[schemars(description = "Full width of the shape as a fraction of the frame, not a radius (default 0.5)")]
    width: Option<f64>,
    #[schemars(description = "Full height of the shape as a fraction of the frame (default 0.5)")]
    height: Option<f64>,
    #[schemars(
        description = "Edge softness as a fraction of the shape's own half-size, 0 = hard (default 0.15). A hard-edged mask over a face reads as a sticker"
    )]
    feather: Option<f64>,
    #[schemars(description = "Keep what is OUTSIDE the shape instead of inside it (default false)")]
    inverted: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetBlendModeParams {
    #[schemars(description = "UUID of the clip (on a video track) to blend")]
    clip_id: String,
    #[schemars(
        description = "How the clip composites onto the tracks below: \"normal\" (plain, the default), \"multiply\", \"screen\", \"overlay\", \"hard_light\", \"add\" (linear dodge), \"darken\", \"lighten\", \"difference\" or \"exclusion\""
    )]
    mode: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct VolumeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Linear gain (1.0 = unchanged, 0.0 = muted)")]
    volume: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct FadeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Fade-in duration in seconds; omit to leave unchanged, 0 to clear")]
    fade_in: Option<f64>,
    #[schemars(description = "Fade-out duration in seconds; omit to leave unchanged, 0 to clear")]
    fade_out: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SpeedParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Playback rate: 1.0 = normal, 2.0 = 2x faster, 0.5 = half speed, negative = reverse")]
    speed: f64,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, the later clips on the same track follow the change in this clip's duration, keeping their gaps; false retimes the clip and leaves later clips where they are (a slowed clip then runs into its neighbour). Tracks ripple independently. Ripple is an attempt, not a guarantee: it never moves a locked track (and an edit to a clip ON a locked track is refused outright), and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners are retimed by the same ratio and re-placed about the clip you named (a sound that leads its picture still leads it, scaled); the linked clips after follow it. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SpeedRampParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "The ramp's keys, at most 16, each {time, speed, easing?}. `time` is seconds from the clip's START on the timeline (not source seconds), strictly increasing once sorted (keys are put in time order for you; two at one time are refused). `speed` is a magnitude from 0.01 to 100 (1 = normal, 0.3 = slow motion, 2 = fast); the clip's direction stays the sign of its own speed, so a reversed clip stays reversed. `easing` is how the speed moves from this key to the next: \"linear\" (default) changes steadily over timeline time, \"hold\" keeps this key's speed until the next key and then jumps. Before the first key the clip runs at the first key's speed, after the last at the last's. An empty list removes the ramp (the clip goes back to its own `speed`) and is an error when the clip has none."
    )]
    keys: Vec<SpeedKey>,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, the later clips on the same track follow the change in this clip's duration, keeping their gaps; false leaves later clips where they are (a slowed clip then runs into its neighbour). Tracks ripple independently. Ripple is an attempt, not a guarantee: it never moves a locked track (and an edit to a clip ON a locked track is refused outright), and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the one named — a picture and its detached sound (`link_id` in get_timeline_state shows the groups) — get the same ramp on the timeline (a key 3 s into the named clip lands 3 s into that moment of a partner that starts elsewhere) and are re-placed about it; the linked clips after follow. false ramps only the clip you named, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct TransformParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Uniform scale after fitting to the frame (1.0 = fit, < 1.0 = picture-in-picture); omit to leave unchanged"
    )]
    scale: Option<f64>,
    #[schemars(description = "Horizontal offset as a fraction of frame width (0.0 = centered); omit to leave unchanged")]
    pos_x: Option<f64>,
    #[schemars(description = "Vertical offset as a fraction of frame height (0.0 = centered); omit to leave unchanged")]
    pos_y: Option<f64>,
    #[schemars(description = "Clockwise rotation in degrees; omit to leave unchanged")]
    rotation: Option<f64>,
    #[schemars(description = "Opacity 0.0–1.0 (1.0 = opaque); omit to leave unchanged")]
    opacity: Option<f64>,
    #[schemars(description = "Fraction cropped from the left edge (0.0–1.0); omit to leave unchanged")]
    crop_left: Option<f64>,
    #[schemars(description = "Fraction cropped from the right edge (0.0–1.0); omit to leave unchanged")]
    crop_right: Option<f64>,
    #[schemars(description = "Fraction cropped from the top edge (0.0–1.0); omit to leave unchanged")]
    crop_top: Option<f64>,
    #[schemars(description = "Fraction cropped from the bottom edge (0.0–1.0); omit to leave unchanged")]
    crop_bottom: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct TransformAtParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Seconds from the clip's start: where an animated number gets its key")]
    time: f64,
    #[schemars(description = "Uniform scale after fitting to the frame (1.0 = fit); omit to leave unchanged")]
    scale: Option<f64>,
    #[schemars(description = "Horizontal offset as a fraction of frame width (0.0 = centered); omit to leave unchanged")]
    pos_x: Option<f64>,
    #[schemars(description = "Vertical offset as a fraction of frame height (0.0 = centered); omit to leave unchanged")]
    pos_y: Option<f64>,
    #[schemars(description = "Clockwise rotation in degrees; omit to leave unchanged")]
    rotation: Option<f64>,
    #[schemars(description = "Opacity 0.0–1.0 (1.0 = opaque); omit to leave unchanged")]
    opacity: Option<f64>,
    #[schemars(description = "Fraction cropped from the left edge (0.0–1.0, never animated); omit to leave unchanged")]
    crop_left: Option<f64>,
    #[schemars(description = "Fraction cropped from the right edge (0.0–1.0, never animated); omit to leave unchanged")]
    crop_right: Option<f64>,
    #[schemars(description = "Fraction cropped from the top edge (0.0–1.0, never animated); omit to leave unchanged")]
    crop_top: Option<f64>,
    #[schemars(description = "Fraction cropped from the bottom edge (0.0–1.0, never animated); omit to leave unchanged")]
    crop_bottom: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ColorParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Additive brightness -1.0–1.0 (0.0 = unchanged); omit to leave unchanged")]
    brightness: Option<f64>,
    #[schemars(description = "Contrast multiplier 0.0–4.0 (1.0 = unchanged); omit to leave unchanged")]
    contrast: Option<f64>,
    #[schemars(description = "Saturation multiplier 0.0–3.0 (1.0 = unchanged); omit to leave unchanged")]
    saturation: Option<f64>,
    #[schemars(description = "Gamma 0.1–10.0 (1.0 = unchanged); omit to leave unchanged")]
    gamma: Option<f64>,
    #[schemars(
        description = "Warm/cool shift -1.0–1.0 (0.0 = unchanged; positive warms, negative cools); omit to leave unchanged"
    )]
    temperature: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct TransitionParams {
    #[schemars(description = "UUID of the clip whose start blends with the clip before it on the same track")]
    clip_id: String,
    #[schemars(
        description = "Transition kind. Fades: \"crossfade\", \"dip_to_black\", \"dip_to_white\". Motion, named for the direction of travel: \"slide_left\" / \"slide_right\" / \"slide_up\" / \"slide_down\" brings the new shot in over the old one, \"push_left\" / \"push_right\" / \"push_up\" / \"push_down\" carries the old one out with it. Omit to clear the transition"
    )]
    kind: Option<String>,
    #[schemars(description = "Transition duration in seconds (required when a kind is given)")]
    duration: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct VideoEffectsParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Ordered list of video effects (replaces the clip's chain). Each is an object with a \"type\": \
                       {\"type\":\"blur\",\"sigma\":8}, {\"type\":\"sharpen\",\"amount\":1.0}, {\"type\":\"grayscale\"}, \
                       {\"type\":\"invert\"}, {\"type\":\"vignette\"}, or \
                       {\"type\":\"chroma_key\",\"color\":\"green\",\"similarity\":0.1,\"blend\":0.0} (keys a color to \
                       transparency so a lower track shows through). Pass [] to clear."
    )]
    effects: Vec<VideoEffect>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AudioEffectsParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Ordered list of audio effects (replaces the clip's chain). Each is an object with a \"type\": \
                       {\"type\":\"highpass\",\"hz\":80}, {\"type\":\"lowpass\",\"hz\":12000}, \
                       {\"type\":\"equalizer\",\"hz\":3000,\"width\":1000,\"gain_db\":3}, \
                       {\"type\":\"compressor\",\"threshold_db\":-18,\"ratio\":3,\"attack_ms\":20,\"release_ms\":250,\"makeup_db\":6}, \
                       {\"type\":\"gate\",\"threshold_db\":-40}, \
                       or {\"type\":\"denoise\",\"reduction_db\":12,\"floor_db\":-50,\"track_noise\":false} \
                       (spectral noise reduction: takes up to reduction_db, 0.01..97, off the hiss / hum under \
                       floor_db, -80..-20 dBFS; start at -50, raise it toward -40 / -35 for a noisy room, and back \
                       off if speech goes thin or watery — too high a floor eats the voice; track_noise lets the \
                       filter follow a floor that moves). Pass [] to clear."
    )]
    effects: Vec<AudioEffect>,
}

#[tool_router(router = tool_router_clip_look, vis = "pub(super)")]
impl KerfMcp {
    #[tool(description = "Cut a clip to a shape: inside the shape the clip is kept, outside it goes \
                       transparent and whatever is on a LOWER track shows through. Omit `shape` to \
                       clear the mask. This is the one masking primitive, and it composes with the \
                       track stack rather than replacing it — to blur a face, duplicate the shot onto \
                       the track above with add_clip at the same timeline_start, give the copy a blur \
                       with set_video_effects, then mask the copy to an ellipse over the face; to grade \
                       one region, do the same with set_color. Check the result with preview_timeline: \
                       positions are fractions of the frame, so you have to look to know you covered \
                       the right thing. To make it follow something, key its numbers with \
                       set_property_keyframes (mask_x, mask_y, mask_width, mask_height, mask_feather): a \
                       keyed number is read from its keys, not from this static value, and clearing the \
                       mask clears its keys.")]
    fn set_mask(&self, Parameters(p): Parameters<SetMaskParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_mask_op(project, p))
    }

    pub(super) fn set_mask_op(project: &Project, p: SetMaskParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let mask = match p.shape {
            None => None,
            Some(ref s) => {
                let shape = MaskShape::parse(s).ok_or_else(|| {
                    McpError::invalid_params(format!("invalid mask shape '{s}'; expected \"rect\" or \"ellipse\""), None)
                })?;
                // Omitted fields keep the clip's current mask, so nudging one
                // number (move the ellipse a little left) does not reset the
                // size and feather back to the defaults out from under it.
                // Deliberately not `.ok()…unwrap_or_default()`: a read that
                // failed is not the same answer as "this clip has no mask
                // yet", and collapsing the two silently resets every field
                // the caller did not name.
                let timeline = project.working_timeline().map_err(core_err)?;
                let (ti, ci) = timeline
                    .locate(clip_id)
                    .ok_or_else(|| core_err(kerf_core::Error::ClipNotFound(clip_id)))?;
                let d = timeline.tracks[ti].clips[ci].mask.unwrap_or_default();
                Some(Mask {
                    shape,
                    x: p.x.unwrap_or(d.x),
                    y: p.y.unwrap_or(d.y),
                    width: p.width.unwrap_or(d.width),
                    height: p.height.unwrap_or(d.height),
                    feather: p.feather.unwrap_or(d.feather),
                    inverted: p.inverted.unwrap_or(d.inverted),
                })
            }
        };
        let clip = project.set_mask(clip_id, mask).map_err(core_err)?;
        json(&clip)
    }

    #[tool(
        description = "Composite a clip onto the tracks BELOW it with a blend mode instead of laying it over \
                       them: its colour is mixed with what is underneath, per pixel. multiply darkens (white \
                       vanishes: paper textures, shadows, line art), screen lightens (black vanishes: light leaks, \
                       lens flares, fire and smoke shot on black), add sums the two (a hotter glow), overlay and \
                       hard_light add contrast from the clip, darken / lighten keep the darker / lighter of the \
                       two, difference and exclusion invert where the clip is bright. \"normal\" goes back to \
                       plain compositing. The clip's opacity, mask, chroma key, fades and dips still decide how much \
                       of the mix shows (a fade or dip eases the mix in and out rather than going through black or \
                       white), and outside its picture the lower tracks are untouched, letterbox bars included; a \
                       clip with nothing under it blends with black. Only a clip on a video track can blend. A \
                       blended clip costs the export several times a plain one's work per frame while it is on \
                       screen, and the GPU preview draws its frames through FFmpeg. Check the result with \
                       preview_timeline."
    )]
    fn set_blend_mode(&self, Parameters(p): Parameters<SetBlendModeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_blend_mode_op(project, p))
    }

    pub(super) fn set_blend_mode_op(project: &Project, p: SetBlendModeParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let mode = BlendMode::parse(&p.mode).ok_or_else(|| {
            let known: Vec<&str> = BlendMode::ALL.iter().map(|m| m.as_str()).collect();
            McpError::invalid_params(
                format!("invalid blend mode '{}'; expected one of {}", p.mode, known.join(", ")),
                None,
            )
        })?;
        let clip = project.set_blend_mode(clip_id, mode).map_err(core_err)?;
        json(&clip)
    }

    #[tool(
        description = "Set the linear volume gain of a clip. A picture whose sound was detached (`source_audio: \
                       false`) carries no sound, so its volume changes nothing audible — its linked audio clip \
                       (see `link_id`) is a separate clip to set. While the clip's volume is animated \
                       (set_property_keyframes with prop `volume`) this static value is not used: clear those \
                       keys (pass []) for it to apply."
    )]
    fn set_volume(&self, Parameters(p): Parameters<VolumeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_volume_op(project, p))
    }

    pub(super) fn set_volume_op(project: &Project, p: VolumeParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.set_volume(clip_id, p.volume).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Set a clip's fade-in / fade-out duration in seconds (omit a field to leave it unchanged, 0 to \
                       clear). On a picture whose sound was detached (`source_audio: false`) the fade only fades the \
                       picture — it carries no sound, so nothing audible changes; its linked audio clip (see \
                       `link_id`) is a separate clip to fade."
    )]
    fn set_fade(&self, Parameters(p): Parameters<FadeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_fade_op(project, p))
    }

    pub(super) fn set_fade_op(project: &Project, p: FadeParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.set_fade(clip_id, p.fade_in, p.fade_out).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Set a clip's playback speed (1.0 = unchanged, 2.0 = 2x faster, 0.5 = half, negative = reverse); this retimes the clip and changes its timeline duration. In ripple mode (get_ripple_mode, or pass `ripple`) the later clips on the track follow the change in duration. Linked partners are retimed by the same ratio so a picture and its sound stay in step (link=false: this clip only)."
    )]
    fn set_speed(&self, Parameters(p): Parameters<SpeedParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_speed_op(project, p))
    }

    pub(super) fn set_speed_op(project: &Project, p: SpeedParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| project.set_speed(clip_id, p.speed))
            })
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Give a clip a speed ramp: its playback rate changes over its length instead of holding one value — slow motion that eases in and out, a speed-up, a freeze-and-go. `keys` are {time, speed, easing?}: `time` is seconds from the clip's start on the TIMELINE (clip-local, like keyframes; trimming the clip's head carries the ramp with the footage), `speed` a magnitude from 0.01 to 100 (1 = normal; the direction stays the sign of the clip's own speed, so a reverse clip ramps in reverse). Between two keys the speed changes steadily over timeline time (`linear`, the default) or stays at the left key's value and jumps at the next key (`hold`); before the first key the clip runs at the first key's speed and after the last at the last's, so the clip's own `speed` is not read while a ramp is set. At most 16 keys, put in time order for you; two at one time are refused. Empty `keys` removes the ramp (an error when there is none). Example, slow to 0.3x for 1.5 s and back: [{\"time\":1,\"speed\":1},{\"time\":1.5,\"speed\":0.3},{\"time\":3,\"speed\":0.3},{\"time\":3.5,\"speed\":1}]. The source window stays and the clip's length changes (it covers the same footage in more or less time), so in ripple mode (get_ripple_mode, or pass `ripple`) the later clips on the track follow the change in duration. Linked partners get the same ramp so a picture and its sound stay in step (link=false: this clip only). set_speed replaces a ramp with one constant rate. A ramped clip's `speed_ramp` is listed with the clip in get_timeline_state."
    )]
    fn set_speed_ramp(&self, Parameters(p): Parameters<SpeedRampParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_speed_ramp_op(project, p))
    }

    pub(super) fn set_speed_ramp_op(project: &Project, p: SpeedRampParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| project.set_speed_ramp(clip_id, p.keys))
            })
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Set a clip's geometric transform — scale / position (pos_x, pos_y as fractions of the frame) / rotation / opacity / per-edge crop. Use a sub-1.0 scale with a position for picture-in-picture. Omit a field to leave it unchanged. A number that is animated (set_keyframes / add_keyframe, or set_property_keyframes) is read from its keys, not from this static value, which only applies once its keys are cleared; the crop edges are never animated."
    )]
    fn set_transform(&self, Parameters(p): Parameters<TransformParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_transform_op(project, p))
    }

    pub(super) fn set_transform_op(project: &Project, p: TransformParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .set_transform(
                clip_id,
                p.scale,
                p.pos_x,
                p.pos_y,
                p.rotation,
                p.opacity,
                p.crop_left,
                p.crop_right,
                p.crop_top,
                p.crop_bottom,
            )
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Set how a clip looks at one moment of it (`time`, seconds from the clip's start), in one edit, whether or not it is animated: each named number that has keyframes (set_keyframes / add_keyframe, or set_property_keyframes) gets a key at that time — the rest of the animation is kept — and every other named number, the crop edges always, is set as set_transform sets it. Use it to change a pose without first finding out which numbers are animated. Returns the clip."
    )]
    fn set_transform_at(&self, Parameters(p): Parameters<TransformAtParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_transform_at_op(project, p))
    }

    pub(super) fn set_transform_at_op(project: &Project, p: TransformAtParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let patch = kerf_core::TransformPatch {
            scale: p.scale,
            pos_x: p.pos_x,
            pos_y: p.pos_y,
            rotation: p.rotation,
            opacity: p.opacity,
            crop_left: p.crop_left,
            crop_right: p.crop_right,
            crop_top: p.crop_top,
            crop_bottom: p.crop_bottom,
        };
        let out = project.set_transform_at(clip_id, p.time, patch).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Set a clip's color correction — brightness / contrast / saturation / gamma / temperature (warm-cool). Omit a field to leave it unchanged. A number that is animated (set_property_keyframes) is read from its keys, not from this static value, which only applies once its keys are cleared (pass [])."
    )]
    fn set_color(&self, Parameters(p): Parameters<ColorParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_color_op(project, p))
    }

    pub(super) fn set_color_op(project: &Project, p: ColorParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .set_color(clip_id, p.brightness, p.contrast, p.saturation, p.gamma, p.temperature)
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Grade a clip beyond set_color: tone curves (master and per channel), shadows / midtones / highlights colour wheels, and a .cube 3D LUT with an intensity. Each part is optional and omitted parts are left as they are. They apply after set_color's numbers, in the order LUT, curves, wheels. Check the result with preview_timeline."
    )]
    fn set_grade(&self, Parameters(mut p): Parameters<GradeParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        // A new LUT file is read before the project is locked — an existing local file, not a URL,
        // an ffmpeg protocol or a network share (reading one would make this machine reach out).
        if let Some(path) = p.lut_path.as_deref().filter(|s| !s.is_empty()) {
            let path = self.path_arg(path)?;
            crate::require_local_input_path(&path).map_err(|e| McpError::invalid_params(e, None))?;
            kerf_core::paths::validate_import_path(std::path::Path::new(&path)).map_err(core_err)?;
            kerf_core::color_lut::inspect_lut(&path).map_err(core_err)?;
            p.lut_path = Some(path);
        }
        self.edit(|project| Self::set_grade_op(project, clip_id, p))
    }

    pub(super) fn set_grade_op(project: &Project, clip_id: Uuid, p: GradeParams) -> Result<String, McpError> {
        let timeline = project.working_timeline().map_err(core_err)?;
        let (ti, ci) = timeline
            .locate(clip_id)
            .ok_or_else(|| core_err(kerf_core::Error::ClipNotFound(clip_id)))?;
        let mut grade = timeline.tracks[ti].clips[ci].grade.clone();
        if let Some(curves) = p.curves.clone() {
            grade.curves = Some(curves.into());
        }
        if let Some(wheels) = p.wheels {
            grade.wheels = Some(wheels.into());
        }
        match (p.lut_path.as_deref(), p.lut_intensity) {
            (Some(""), _) => grade.lut = None,
            (Some(path), intensity) => {
                let kept = grade.lut.as_ref().filter(|l| l.path == path).map(|l| l.intensity);
                grade.lut = Some(ClipLut {
                    path: path.to_string(),
                    intensity: intensity.or(kept).unwrap_or(1.0),
                });
            }
            (None, Some(intensity)) => {
                let lut = grade
                    .lut
                    .as_mut()
                    .ok_or_else(|| McpError::invalid_params("the clip has no LUT whose intensity to change", None))?;
                lut.intensity = intensity;
            }
            (None, None) => {}
        }
        let out = project.set_grade(clip_id, grade).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Set or clear the transition blending a clip's start with the clip before it on the same track, with a duration in seconds; omit kind to clear. A dissolve or a motion transition plays both shots at once, so it borrows the outgoing clip's unused source — a clip trimmed to the very end of its footage has none to lend and the transition falls back to a hard cut. Dips need no handle. Reach for a fade between scenes and a motion transition between shots in a montage; a cut needs no transition at all."
    )]
    fn set_transition(&self, Parameters(p): Parameters<TransitionParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_transition_op(project, p))
    }

    pub(super) fn set_transition_op(project: &Project, p: TransitionParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let transition = parse_transition(p.kind, p.duration)?;
        let out = project.set_transition(clip_id, transition).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Replace a clip's video effect chain (applied in order at export): blur, sharpen, grayscale, invert, vignette, or chroma_key (key a color to transparency so footage on a lower track shows through). Pass an empty list to clear."
    )]
    fn set_video_effects(&self, Parameters(p): Parameters<VideoEffectsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_video_effects_op(project, p))
    }

    pub(super) fn set_video_effects_op(project: &Project, p: VideoEffectsParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.set_video_effects(clip_id, p.effects).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Replace a clip's audio effect chain (applied in order at export): highpass, lowpass, equalizer (parametric band), compressor (dynamics), gate (noise gate) or denoise (spectral noise reduction for hiss, hum and room tone: put it after a highpass and before a compressor, which would lift the noise). Pass an empty list to clear."
    )]
    fn set_audio_effects(&self, Parameters(p): Parameters<AudioEffectsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_audio_effects_op(project, p))
    }

    pub(super) fn set_audio_effects_op(project: &Project, p: AudioEffectsParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.set_audio_effects(clip_id, p.effects).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Smart crop: frame each shot for the delivery frame instead of centring it. Samples where a clip's content actually sits and writes the crop that keeps it, per clip — so 16:9 footage delivered at 9:16 keeps the subject rather than whatever happened to be in the middle. Set the delivery frame first (set_delivery_format); clips already that shape are left alone. The result is an ordinary transform crop the user can adjust or undo. Sampling takes a moment per clip (send a `progressToken`, cancel the request, or pass `background: true` to run it as a job). Returns how many clips were reframed and a summary of the timeline."
    )]
    async fn smart_crop(
        &self,
        Parameters(p): Parameters<SmartCropParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let clip = p.clip_id.as_deref().map(|s| parse_arg("clip_id", s)).transpose()?;
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on a smart crop ends an agent's too (`begin_smart_crop`).
        let gui_stop = self.app.state::<crate::AppState>().begin_smart_crop();
        self.long_tool("smart_crop", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let project = me.project.clone();
            let (moved, timeline) = blocking(move || {
                // Plan under the lock, sample with it released (one short ffmpeg decode per
                // clip), apply under it again — the same shape as analyze_asset, for the same
                // reason.
                let (plan, generation) = {
                    let project = lock_agent_as(&project, owner.as_deref());
                    (project.smart_crop_inputs(clip).map_err(core_err)?, project.generation())
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let crops = Project::sample_smart_crops_with(
                    &plan,
                    &mut |f| run.progress(Some(f64::from(f)), "sampling where each shot's content sits"),
                    &stop,
                )
                .map_err(core_err)?;
                let guard = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&guard, generation, "the shots were being framed")?;
                let moved = guard.apply_smart_crops(&crops).map_err(core_err)?;
                let timeline = guard.working_timeline().map_err(core_err)?;
                Ok((moved, timeline))
            })
            .await?;
            me.changed();
            json(&serde_json::json!({ "clips_reframed": moved, "timeline": reads::timeline_brief(&timeline) }))
        })
        .await
    }

    #[tool(
        description = "Stabilize a clip: take the shake of a hand-held camera out of its picture. Sets the clip's stabilization (an ordinary edit: it stages with your proposal and undoes like any other) and then measures the camera over the clip's footage (vid.stab's analysis of the original, a few seconds per second of 1080p; queued behind exports and proxies). Until the analysis lands the clip plays and exports as shot. The correction is per source frame, so speed, reverse and speed ramps keep it; trimming the clip within the footage measured keeps it, trimming past it needs this tool again (stabilization_status says `missing`). Where Kerf's FFmpeg has no vid.stab the clip is steadied by FFmpeg's simpler `deshake` instead, with no analysis (status `basic`). A still, a title, a clip on an audio track and 360 footage are refused. `enabled: false` stops stabilizing (no analysis). Pass `background: true` to run the analysis as a job. Returns the clip's `stabilize` setting and its `status`."
    )]
    async fn stabilize_clip(
        &self,
        Parameters(p): Parameters<StabilizeParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let auto_zoom = p.auto_zoom.unwrap_or(false);
        if auto_zoom && p.zoom.is_some() {
            return Err(McpError::invalid_params("give zoom or auto_zoom: true, not both", None));
        }
        // What is not named keeps what the clip has: a call that only changes the strength keeps a fixed zoom.
        let current = lock_agent_as(&self.project, self.owner().as_deref())
            .working_timeline()
            .map_err(core_err)?
            .clip(clip_id)
            .ok_or_else(|| core_err(kerf_core::Error::ClipNotFound(clip_id)))?
            .stabilize;
        let setting = p.enabled.unwrap_or(true).then(|| Stabilize {
            strength: p.strength.or(current.map(|c| c.strength)).unwrap_or(0.5),
            zoom: if auto_zoom {
                None
            } else {
                p.zoom.or(current.and_then(|c| c.zoom))
            },
        });
        // The setting is the edit, checked here: a bad number or a clip that cannot be steadied is an
        // error on the call, not a job that fails a moment later. The setting it has already is none.
        if setting != current {
            self.edit(|project| project.set_clip_stabilize(clip_id, setting).map_err(core_err))?;
        }
        if setting.is_none() {
            return json(&serde_json::json!({ "clip_id": clip_id, "stabilize": null, "status": StabilizeStatus::Off }));
        }
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on a stabilization analysis ends an agent's too (`begin_stabilize`).
        let gui_stop = self.app.state::<crate::AppState>().begin_stabilize();
        self.long_tool("stabilize_clip", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let project = me.project.clone();
            let status = blocking(move || {
                // Resolve under the lock, measure with it released (the clip's window of the original),
                // read the status under it again: the analysis is a cache, nothing is applied.
                let (job, generation) = {
                    let project = lock_agent_as(&project, owner.as_deref());
                    (project.stabilize_inputs(clip_id).map_err(core_err)?, project.generation())
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                Project::analyze_stabilization(&job, &mut |f| run.progress(Some(f), "measuring the camera"), &stop)
                    .map_err(core_err)?;
                let inputs = lock_agent_as(&project, owner.as_deref())
                    .stabilization_status_inputs(Some(&[clip_id]))
                    .map_err(core_err)?;
                Ok(Project::stabilization_statuses(&inputs).pop())
            })
            .await?;
            me.changed();
            // The Inspector hears where the clip stands now, as it does for its own analysis.
            if let Some(status) = &status {
                if let Err(e) = me.app.emit("stabilize-progress", status.clone()) {
                    tracing::warn!(error = %e, "failed to emit stabilize-progress");
                }
            }
            json(&serde_json::json!({
                "clip_id": clip_id,
                "stabilize": setting,
                "status": status.map(|s| s.status),
            }))
        })
        .await
    }

    #[tool(
        description = "Where each clip's stabilization stands (stabilize_clip): `off`, `unsupported` (with the reason), `basic` (steadied by FFmpeg's deshake: this FFmpeg has no vid.stab, no analysis needed), `missing` (not analysed, or trimmed past what was: it plays as shot until stabilize_clip runs again), `queued`, `analyzing` (with a fraction), `failed` (with the reason; it plays as shot) or `ready` (the preview and the export steady it). The named clips, or every stabilized clip of the timeline you see."
    )]
    async fn stabilization_status(&self, Parameters(p): Parameters<StabilizationStatusParams>) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .map(|ids| ids.iter().map(|s| parse_arg("clip_ids", s)).collect::<Result<Vec<_>, _>>())
            .transpose()?;
        let (project, owner) = (self.project.clone(), self.owner());
        let statuses = blocking(move || {
            let inputs = lock_agent_as(&project, owner.as_deref())
                .stabilization_status_inputs(ids.as_deref())
                .map_err(core_err)?;
            Ok(Project::stabilization_statuses(&inputs))
        })
        .await?;
        json(&statuses)
    }
}
