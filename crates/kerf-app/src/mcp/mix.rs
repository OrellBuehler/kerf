//! The mix: track and master levels, ducking and limiter, and a track's muted, solo and locked state.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetMasterDuckParams {
    #[schemars(
        description = "Speech-gate depth in dB (negative, clamped to -40..-1; -12 is a good bed under dialogue). Omit to go back to the sidechain compressor"
    )]
    depth_db: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTrackDuckParams {
    #[schemars(description = "UUID of the track to (un)duck")]
    track_id: String,
    #[schemars(description = "true to duck this track under the others on export, false to restore a flat mix")]
    duck: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTrackVolumeParams {
    #[schemars(description = "UUID of the track to set the level of")]
    track_id: String,
    #[schemars(description = "Track fader as a linear gain: 1.0 is unity, 0.5 is -6 dB, 0 is silent. Clamped to 0..4")]
    volume: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTrackPanParams {
    #[schemars(description = "UUID of the track to place")]
    track_id: String,
    #[schemars(description = "Stereo placement: -1 hard left, 0 centre, 1 hard right")]
    pan: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetMasterVolumeParams {
    #[schemars(
        description = "Master fader as a linear gain on the finished mix: 1.0 is unity, 0.5 is -6 dB, 0 is silent. Clamped to 0..4 (+12 dB)"
    )]
    volume: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetMasterLimiterParams {
    #[schemars(description = "true switches the limiter on, false switches it off")]
    enabled: bool,
    #[schemars(
        description = "Where the limiter stops the signal, in dBFS: -1.5 keeps the mix a decibel and a half under full scale. Clamped to -24..0. Omit to keep the current ceiling (-1.5 until one is chosen)"
    )]
    ceiling_db: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetMasterTruePeakParams {
    #[schemars(
        description = "true makes the master limiter hold the true peak (it runs 4x oversampled, so the peaks between samples stay under the ceiling too); false is the plain sample-peak limiter"
    )]
    enabled: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTrackMutedParams {
    #[schemars(description = "UUID of the track to mute or unmute")]
    track_id: String,
    #[schemars(description = "true to silence (audio) or hide (video) this track, false to restore it")]
    muted: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTrackSoloParams {
    #[schemars(description = "UUID of the track to solo or unsolo")]
    track_id: String,
    #[schemars(description = "true to solo this track, false to clear its solo")]
    solo: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetTrackLockedParams {
    #[schemars(description = "UUID of the track to lock or unlock")]
    track_id: String,
    #[schemars(description = "true to guard this track's clips against edits, false to unlock")]
    locked: bool,
}

#[tool_router(router = tool_router_mix, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Duck (or unduck) a track: on export its audio is sidechain-compressed under the \
                       other tracks, so e.g. a music bed dips automatically whenever dialogue plays"
    )]
    fn set_track_duck(&self, Parameters(p): Parameters<SetTrackDuckParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_track_duck_op(project, p))
    }

    pub(super) fn set_track_duck_op(project: &Project, p: SetTrackDuckParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let track = project.set_track_duck(track_id, p.duck).map_err(core_err)?;
        json(&reads::track_brief(&track))
    }

    #[tool(
        description = "Set a track's fader — one linear gain riding every clip on the track, applied after \
                       each clip's own volume and effects. This is how a music bed is balanced against a \
                       voiceover: put the music on its own track and pull it to roughly 0.25-0.4 under \
                       speech. Prefer it to editing every clip's volume, and to set_track_duck when the \
                       level should simply sit lower rather than dip and recover."
    )]
    fn set_track_volume(&self, Parameters(p): Parameters<SetTrackVolumeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_track_volume_op(project, p))
    }

    pub(super) fn set_track_volume_op(project: &Project, p: SetTrackVolumeParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let track = project.set_track_volume(track_id, p.volume).map_err(core_err)?;
        json(&reads::track_brief(&track))
    }

    #[tool(
        description = "Place a track in the stereo field, -1 hard left to 1 hard right. A balance, so a \
                       panned track never gets louder, and a no-op on a mono delivery. Use it sparingly — \
                       a hard-panned music bed sounds broken on a phone speaker, which is what most of \
                       this footage is watched on."
    )]
    fn set_track_pan(&self, Parameters(p): Parameters<SetTrackPanParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_track_pan_op(project, p))
    }

    pub(super) fn set_track_pan_op(project: &Project, p: SetTrackPanParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let track = project.set_track_pan(track_id, p.pan).map_err(core_err)?;
        json(&reads::track_brief(&track))
    }

    #[tool(
        description = "Set the master fader — one linear gain on the finished mix, after every track and the duck \
                       bus and before loudnorm. Use it to bring the whole cut down (or up) without touching a \
                       track: when get_levels says the mix is hot, this and set_master_limiter are the fixes, and \
                       a quiet cut is raised here or by exporting with loudnorm. Clamped to 0..4 (+12 dB). \
                       Returns the master bus."
    )]
    fn set_master_volume(&self, Parameters(p): Parameters<SetMasterVolumeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_master_volume_op(project, p))
    }

    pub(super) fn set_master_volume_op(project: &Project, p: SetMasterVolumeParams) -> Result<String, McpError> {
        let master = project.set_master_volume(p.volume).map_err(core_err)?;
        json(&master)
    }

    #[tool(
        description = "Switch the master limiter on or off, optionally moving its ceiling (dBFS, default -1.5). A \
                       lookahead limiter on the finished mix that holds the loudest peaks under the ceiling \
                       instead of letting them clip — turn it on for a cut destined for a platform, which asks for \
                       a true peak under -1 dBTP. It is a sample-peak ceiling unless set_master_true_peak is on: the \
                       true peak between samples can read above it (up to about 1 dB on a pure high tone), which is \
                       why the default sits half a decibel under that line. Confirm the true peak with get_levels, and \
                       if it is still over -1 dBTP with the limiter on, turn on set_master_true_peak or lower the \
                       ceiling by the overshoot plus half a decibel (the notes say to what) instead of switching it on \
                       again. A limiter has nothing to do on a mix \
                       already under its ceiling and leaves it alone. Returns the master bus."
    )]
    fn set_master_limiter(&self, Parameters(p): Parameters<SetMasterLimiterParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_master_limiter_op(project, p))
    }

    pub(super) fn set_master_limiter_op(project: &Project, p: SetMasterLimiterParams) -> Result<String, McpError> {
        let master = project.set_master_limiter(p.enabled, p.ceiling_db).map_err(core_err)?;
        json(&master)
    }

    #[tool(
        description = "Make the master limiter (set_master_limiter) hold the true peak instead of only the sample \
                       peak: it then runs on the mix upsampled four times, so the peaks between samples — what \
                       get_levels and the platforms measure as dBTP — stay under its ceiling too, and a ceiling of \
                       -1 really means -1 dBTP. Costs a little render time; does nothing while the limiter is off \
                       (the setting is kept). Returns the master bus."
    )]
    fn set_master_true_peak(&self, Parameters(p): Parameters<SetMasterTruePeakParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_master_true_peak_op(project, p))
    }

    pub(super) fn set_master_true_peak_op(project: &Project, p: SetMasterTruePeakParams) -> Result<String, McpError> {
        let master = project.set_master_true_peak(p.enabled).map_err(core_err)?;
        json(&master)
    }

    #[tool(
        description = "Choose how ducked tracks (set_track_duck) dip under the rest of the mix on export. With \
                       depth_db, a speech gate: whenever the other tracks speak the ducked tracks drop by exactly \
                       that much (ramping down in ~50 ms and back ~300 ms after the speech stops) — -12 is a good \
                       music bed under dialogue. Without it, the default sidechain compressor, which dips by how loud \
                       the speech is. Returns the master bus."
    )]
    fn set_master_duck(&self, Parameters(p): Parameters<SetMasterDuckParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_master_duck_op(project, p))
    }

    pub(super) fn set_master_duck_op(project: &Project, p: SetMasterDuckParams) -> Result<String, McpError> {
        let master = project.set_master_duck(p.depth_db).map_err(core_err)?;
        json(&master)
    }

    #[tool(
        description = "Mute (or unmute) a track: its clips stop rendering — silent for audio, hidden for \
                       video — while keeping their place on the timeline. Use this to audition a cut \
                       without a music bed, or to park B-roll without deleting it."
    )]
    fn set_track_muted(&self, Parameters(p): Parameters<SetTrackMutedParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_track_muted_op(project, p))
    }

    pub(super) fn set_track_muted_op(project: &Project, p: SetTrackMutedParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let track = project.set_track_muted(track_id, p.muted).map_err(core_err)?;
        json(&reads::track_brief(&track))
    }

    #[tool(
        description = "Solo (or unsolo) a track. While any track of a kind is soloed, the other tracks of \
                       that kind stop rendering; video and audio solo independently, so soloing a music \
                       bed does not blank the picture. Several tracks may be soloed at once."
    )]
    fn set_track_solo(&self, Parameters(p): Parameters<SetTrackSoloParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_track_solo_op(project, p))
    }

    pub(super) fn set_track_solo_op(project: &Project, p: SetTrackSoloParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let track = project.set_track_solo(track_id, p.solo).map_err(core_err)?;
        json(&reads::track_brief(&track))
    }

    #[tool(
        description = "Lock a track against editing. A locked track still renders, but no clip on it is moved, \
                       trimmed, split, cut, removed, retimed, disabled or reordered and nothing is added or pasted \
                       onto it (its clips' volume, fades, effects, grade, transform and keyframes stay editable). You \
                       may lock a track; only the user can unlock one — `locked: false` on a locked track is refused, \
                       so ask them. The answer is the track as it now holds for you: in a proposal, a track the user \
                       unlocked since it began cannot be locked again from it (the answer says `locked: false`)."
    )]
    fn set_track_locked(&self, Parameters(p): Parameters<SetTrackLockedParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_track_locked_op(project, p))
    }

    pub(super) fn set_track_locked_op(project: &Project, p: SetTrackLockedParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let track = project.set_track_locked(track_id, p.locked).map_err(core_err)?;
        json(&reads::track_brief(&track))
    }
}
