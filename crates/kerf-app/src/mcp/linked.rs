//! Linked picture and sound: extracting, detaching, reattaching and linking audio.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct LinkClipsParams {
    #[schemars(
        description = "UUIDs of the clips to link — at least two, each on a DIFFERENT track (a picture and its sound), none on a locked track"
    )]
    clip_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct DetachClipsParams {
    #[schemars(
        description = "UUIDs of the picture clips whose own sound to detach (on video tracks); one that cannot be is skipped and reported"
    )]
    clip_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ReattachClipsParams {
    #[schemars(
        description = "UUIDs of the clips whose detached sound to reattach — each names a picture or its sound (a pair named twice counts once); all or nothing: one that cannot be reattached refuses the lot"
    )]
    clip_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct UnlinkClipsParams {
    #[schemars(
        description = "UUIDs of the clips to unlink; each leaves its group, and a group left with a single clip dissolves (so naming either half of a pair unlinks the pair)"
    )]
    clip_ids: Vec<String>,
}

#[tool_router(router = tool_router_linked, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Give an asset's sound its own clip on an audio track, for every clip of the asset on a video \
                       track that still plays it: each is DETACHED (see detach_audio: an audio clip with the same span \
                       and position, linked to the picture, whose own sound is muted) so nothing is heard twice — in \
                       one revision. A clip that cannot be detached (its track is locked) is skipped and listed in \
                       `skipped` with the reason. Errors when there is nothing to extract (no clip of the asset on a \
                       video track, or its sound already detached) and does NOT append the asset's audio as a \
                       fallback: for the asset's whole audio as a clip of its own, use add_asset_audio. Returns \
                       `detached` (each: the audio `clip`, its `track_id`, `created_track`) and `skipped`."
    )]
    fn extract_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::extract_audio_op(project, p))
    }

    pub(super) fn extract_audio_op(project: &Project, p: AssetIdParams) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let out = project.extract_audio(id).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Add an asset's WHOLE audio, from its start, as a new clip at the end of the first UNLOCKED audio \
                       track (a new one is made when there is none, or every one is locked). It touches no picture clip, so footage that is also on a video \
                       track playing its own sound is heard twice where the two overlap — to give a clip already cut its \
                       sound as an audio clip use extract_audio / detach_audio, which mute the picture. For music and \
                       other audio-only assets this is how they reach the timeline. One revision. Returns the clip."
    )]
    fn add_asset_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::add_asset_audio_op(project, p))
    }

    pub(super) fn add_asset_audio_op(project: &Project, p: AssetIdParams) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let out = project.add_asset_audio(id).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "DETACH a picture clip's own sound onto an audio track: a new audio clip with the SAME source \
                       span, speed and timeline position goes on the audio track at the picture's own position (V1 → \
                       A1) when it has room, else the first that does, else a new one; it is LINKED to the picture \
                       (so they move, trim, split and delete together — see link_clips) and the picture's own sound \
                       is muted, so the sound is heard once, from the audio track. The LEVEL is kept: the picture \
                       track's fader is folded into the new clip's volume (volume × picture fader ÷ audio fader) — \
                       except under a compressor or gate, which react to level: such a clip goes to an audio \
                       track whose fader EQUALS the picture track's (an existing one with room, else a new \
                       track at that fader) and its volume is left alone. \
                       What cannot be kept is the rest of the audio track's strip — its pan, duck and mute/solo \
                       decide the mix now, not the picture track's. Audio effects, fades and the transition's sound \
                       fade go with the audio clip. One revision. Errors if the clip is not on a video track, its \
                       asset has no audio, its sound is already detached, or a track involved is locked. Returns \
                       `clip` (the audio clip), `track_id` and `created_track`. reattach_audio undoes it; \
                       detach_audio_clips does several in one revision."
    )]
    fn detach_audio(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::detach_audio_op(project, p))
    }

    pub(super) fn detach_audio_op(project: &Project, p: ClipIdParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.detach_audio(clip_id).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "DETACH the own sound of SEVERAL picture clips in ONE revision (one undo step) — detach_audio for \
                       each. A clip that cannot be (not a video clip, no audio, already detached, a locked track) is \
                       skipped and listed in `skipped` with the reason; errors only when none could be. Returns \
                       `detached` (each: the audio `clip`, `track_id`, `created_track`) and `skipped`."
    )]
    fn detach_audio_clips(&self, Parameters(p): Parameters<DetachClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::detach_audio_clips_op(project, p))
    }

    pub(super) fn detach_audio_clips_op(project: &Project, p: DetachClipsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let out = project.detach_audio_clips(&ids).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "REATTACH detached sound: delete the linked audio clip(s) carrying a picture's sound and let the \
                       picture play its own again, as it was before detach_audio (edits made to the audio clip are \
                       not carried back). Name either clip of the pair. A picture whose audio clip is already gone \
                       is just unmuted — unless another audio clip is already playing the same footage in step with \
                       it (unmuting would double the sound), which is refused. One revision. Returns the picture clip. \
                       reattach_audio_clips does several in one revision."
    )]
    fn reattach_audio(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::reattach_audio_op(project, p))
    }

    pub(super) fn reattach_audio_op(project: &Project, p: ClipIdParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project.reattach_audio(clip_id).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "REATTACH the detached sound of SEVERAL pictures in ONE revision (one undo step) — reattach_audio \
                       for each; name either clip of each pair (a pair named twice counts once). ALL OR NOTHING, unlike \
                       detach_audio_clips: a pair that cannot be reattached (a locked track, or the picture's sound \
                       already playing from another audio clip, which would double it) refuses the whole call and the \
                       error names the clip, changing nothing. Returns the picture clips."
    )]
    fn reattach_audio_clips(&self, Parameters(p): Parameters<ReattachClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::reattach_audio_clips_op(project, p))
    }

    pub(super) fn reattach_audio_clips_op(project: &Project, p: ReattachClipsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let out = project.reattach_audio_clips(&ids).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "LINK clips into one group — a picture and the sound that goes with it, or any clips that must \
                       stay together. From then on move_clip(s) / trim / split_at / remove(_clips) / ripple_delete / \
                       cut_clip_range / set_speed / set_speed_ramp / roll_edit / slip_clip / slide_clip / split_remove(_clips) carry the \
                       edit to the others (pass link=false on those to edit one alone) and any ripple, cut or delete \
                       that moves a linked clip moves its partners by the same amount — a sound that leads or trails \
                       its picture (a J- or L-cut) stays that far ahead or behind. Only linked clips follow: an \
                       unlinked clip on a partner's track stays where it was, and one in the way refuses the edit \
                       (as does a locked partner, a PICTURE a linked clip would run into or start before 0 — pictures are \
                       never trimmed to fit — or a clip that would be left under 0.05 s) with a reason; a linked SOUND \
                       in the way is trimmed back instead, and the revision label says so (\"… (trimmed sound on \
                       A2)\"). Two linked clips you name and move apart yourself are refused as \"out of step\" — \
                       unlink them first (naming both and cutting them at the same moment, as a trim to the \
                       playhead does, is not moving them apart). \
                       At least two clips, on different tracks, none on a locked track. detach_audio links for you. One revision. Returns the group's `link_id`."
    )]
    fn link_clips(&self, Parameters(p): Parameters<LinkClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::link_clips_op(project, p))
    }

    pub(super) fn link_clips_op(project: &Project, p: LinkClipsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let group = project.link_clips(&ids).map_err(core_err)?;
        json(&serde_json::json!({ "link_id": group, "clips": ids.len() }))
    }

    #[tool(
        description = "UNLINK clips: each leaves its group, and a group left with a single clip dissolves (naming either \
                       half of a pair unlinks the pair). Errors when none of them was linked. One revision. Returns \
                       how many of the named clips were linked."
    )]
    fn unlink_clips(&self, Parameters(p): Parameters<UnlinkClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::unlink_clips_op(project, p))
    }

    pub(super) fn unlink_clips_op(project: &Project, p: UnlinkClipsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let unlinked = project.unlink_clips(&ids).map_err(core_err)?;
        json(&serde_json::json!({ "unlinked": unlinked }))
    }
}
