//! Cutting and arranging the timeline: clips, tracks, ripple mode, silence removal and concatenation.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct CutClipParams {
    #[schemars(description = "UUID of the source asset")]
    asset_id: String,
    #[schemars(description = "In-point in the source asset (seconds)")]
    start: f64,
    #[schemars(description = "Out-point in the source asset (seconds)")]
    end: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddClipParams {
    #[schemars(description = "UUID of the source asset")]
    asset_id: String,
    #[schemars(
        description = "Target track UUID; omit to auto-select the first unlocked track of the asset's main kind. A named track must suit the clip: a video track needs media with a picture, an audio track media with sound (footage with both may go on either); an unsuitable or locked track refuses"
    )]
    track_id: Option<String>,
    #[schemars(description = "In-point in the source asset (seconds)")]
    source_in: f64,
    #[schemars(description = "Out-point in the source asset (seconds)")]
    source_out: f64,
    #[schemars(description = "Timeline position (seconds); omit to append")]
    timeline_start: Option<f64>,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, a clip dropped onto footage that is already there pushes that footage and everything after it later by the clip's length; appending, or filling a gap it fits in, moves nothing either way. false places the clip exactly as asked. Tracks ripple independently. A clip dropped INSIDE an existing clip would need that clip split, so ripple cannot make room: it leaves the two overlapping — place clips on the boundary between two, or split first, and check get_timeline_state. More generally ripple is an attempt, not a guarantee: it never moves a locked track (and placing ON one is refused), and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SplitParams {
    #[schemars(description = "UUID of the clip to split")]
    clip_id: String,
    #[schemars(description = "Timeline time at which to split (seconds)")]
    at: f64,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). A split changes no length, so ripple never moves anything here; the parameter exists so a call can state its intent either way."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner that has the split time inside it is split at the same moment; the clips on each side of the cut form a link group of their own (a partner that lies wholly after the cut goes with the right half). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct TrimParams {
    #[schemars(description = "UUID of the clip to trim")]
    clip_id: String,
    #[schemars(description = "New source in-point (seconds)")]
    source_in: Option<f64>,
    #[schemars(description = "New source out-point (seconds)")]
    source_out: Option<f64>,
    #[schemars(
        description = "New timeline start (seconds) applied in the same edit — pass alongside source_in \
                       when trimming the left edge so the clip's right edge stays put"
    )]
    timeline_start: Option<f64>,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, the later clips on the same track follow the change in this clip's length, keeping their gaps, and a left-edge trim keeps the clip's start (so timeline_start is not needed). false trims in place and leaves later clips where they are. Tracks ripple independently (trimming on V1 never moves A1). Ripple is an attempt, not a guarantee: it never moves a locked track (and an edit to a clip ON a locked track is refused outright), and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: a partner that shares the edge you move (within 1 ms) has that edge moved by the same amount, clamped to its own footage; a pure move carries it by the same time; a partner that would then land on a clip outside its group refuses the call (ripple may make room). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ReorderParams {
    #[schemars(description = "UUID of the track containing the clip")]
    track_id: String,
    #[schemars(description = "UUID of the clip to move")]
    clip_id: String,
    #[schemars(description = "New zero-based index within the track")]
    new_index: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RippleDeleteParams {
    #[schemars(description = "UUID of the clip to remove")]
    clip_id: String,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner is removed too, the named clip's track closes the gap by that clip's length, and the linked clips after it follow by the same amount on their own tracks (unlinked clips on a partner's track stay put). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RemoveClipParams {
    #[schemars(description = "UUID of the clip to remove")]
    clip_id: String,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). true closes the gap the clip leaves: the later clips on its track shift left by its length, gaps kept (ripple_delete is the same thing as a tool of its own). false leaves the gap. Tracks ripple independently. Ripple is an attempt, not a guarantee: it never moves a locked track (and an edit to a clip ON a locked track is refused outright), and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: they are removed too (and gaps close per ripple). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RemoveClipsParams {
    #[schemars(description = "UUIDs of the clips to remove, on any tracks; a clip named twice is removed once")]
    clip_ids: Vec<String>,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). true is a multi-clip ripple delete: every track closes up behind what it lost, each by its own removed length, gaps kept. false leaves the gaps. Tracks ripple independently. The result's `rippled` / `clips_shifted` say whether anything actually moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: every partner of a named clip is removed with it and counted in `removed`. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct MoveClipsParams {
    #[schemars(
        description = "The clips to move and where each lands. Every clip may appear once. The group is checked as a group: clips moving together may pass through the places they are leaving, but may not overlap each other or a clip that stays."
    )]
    moves: Vec<ClipMoveParams>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner of a named clip moves by the same time on its OWN track — a track change you ask for applies to the clip you named only. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetRippleModeParams {
    #[schemars(description = "true to turn ripple mode on for the project, false to turn it off")]
    on: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct MoveClipParams {
    #[schemars(description = "UUID of the clip to move")]
    clip_id: String,
    #[schemars(description = "New timeline position in seconds (clamped to >= 0)")]
    timeline_start: f64,
    #[schemars(description = "Destination track UUID (must be the same kind); omit to keep the clip on its current track")]
    track_id: Option<String>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner moves by the same time on its OWN track — a track change you ask for applies to the clip you named only. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddTrackParams {
    #[schemars(description = "Track kind: \"video\" or \"audio\"")]
    kind: String,
    #[schemars(description = "Optional track name; auto-named (V2/A2/…) when omitted")]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct TrackIdParams {
    #[schemars(description = "UUID of the track")]
    track_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct CutClipRangeParams {
    #[schemars(description = "UUID of the clip to cut")]
    clip_id: String,
    #[schemars(description = "Source-time start of the span to remove (seconds, e.g. a transcript segment's or a word's start)")]
    from: f64,
    #[schemars(description = "Source-time end of the span to remove (seconds)")]
    to: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: the stretch of TIMELINE the cut removes is taken out of every partner it overlaps too — a partner whose start was inside the stretch resumes at the cut, one spanning it is cut in two — and the linked clips after it follow by the same amount. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RollEditParams {
    #[schemars(description = "UUID of the EARLIER clip of the pair — the one whose end is the cut")]
    clip_a: String,
    #[schemars(
        description = "UUID of the LATER clip of the pair, on the same track and touching clip_a — the one whose start is the cut"
    )]
    clip_b: String,
    #[schemars(
        description = "Seconds to move the cut: positive moves it later (clip_a gains footage at its end, clip_b loses it at its start), negative earlier. Clamped to the footage each clip has left and to 0.05s for the clip that shrinks; the reply's `applied` is how far it really went."
    )]
    delta: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each pair of partners that share the cut rolls by the same amount, and the roll clamps to the tightest pair. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SlipClipParams {
    #[schemars(description = "UUID of the clip to slip")]
    clip_id: String,
    #[schemars(
        description = "Source-footage seconds (NOT timeline seconds — at 2x speed 1 second of slip moves the picture half a second): positive makes the clip start later in its own footage, so you see material that comes later; negative earlier. A reversed clip is mirrored for you (positive moves its source window down). Clamped to the asset's footage; the reply's `applied` is how far it really went."
    )]
    delta: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners slip by the same moment of footage (a still has none and is skipped), clamped to the tightest. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SlideClipParams {
    #[schemars(description = "UUID of the clip to slide")]
    clip_id: String,
    #[schemars(
        description = "Timeline seconds to move the clip: positive later, negative earlier. A neighbour that touches the clip gives way (previous clip's end and next clip's start move by the same amount); one across a gap is left alone and the clip stops where it would meet it. Clamped to the neighbours' footage and a 0.05s floor, and to 0 at the start of the track; the reply's `applied` is how far it really went."
    )]
    delta: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners slide by the same time, their own neighbours giving way, clamped to the tightest. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SplitRemoveParams {
    #[schemars(description = "UUID of the clip to cut")]
    clip_id: String,
    #[schemars(
        description = "Timeline time to cut at (seconds); must lie inside the clip and leave at least 0.05s of the half you keep"
    )]
    at: f64,
    #[schemars(description = "Which half to REMOVE: \"left\" (everything before `at`) or \"right\" (everything after it)")]
    side: SplitSide,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, the later clips on the track close the gap the removed half leaves (a left removal keeps the clip's own start and pulls the rest in); false leaves the gap, as after a plain split and delete. Ripple is an attempt, not a guarantee: it never moves a locked track (and an edit to a clip ON a locked track is refused outright), and a lane it would leave overlapping is kept as the edit made it."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners that span `at` are trimmed to it as well. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SplitRemoveClipsParams {
    #[schemars(
        description = "The clips to cut and where. At most one clip per track, each clip once; different tracks may be cut at different times (usually the same time: a picture and its sound)."
    )]
    cuts: Vec<ClipCutParams>,
    #[schemars(
        description = "Which half to REMOVE from every clip: \"left\" (everything before its `at`) or \"right\" (everything after it)"
    )]
    side: SplitSide,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, each track closes the gap its own clip leaves (a left removal keeps the clip's own start); tracks ripple independently. false leaves the gaps."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: a partner of a named clip that you did not name is cut at the same time if the time is inside it. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SplitAllTracksParams {
    #[schemars(description = "Timeline time to cut every targeted track at (seconds)")]
    at: f64,
    #[schemars(
        description = "The tracks to cut (UUIDs from get_timeline_state). Omitted, EVERY UNLOCKED track is targeted; a locked track with a clip under `at` is left alone and listed in the reply's `skipped`. Naming tracks cuts only those, plus the linked partners of the clips it cuts (unless link=false). A named track must exist and not be locked; an empty list is refused (leave it out for all tracks)."
    )]
    track_ids: Option<Vec<String>>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to a clip that is cut — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are cut with it at the same moment even on a track you did not name, and the halves on each side of the cut form a link group of their own. false cuts only the clips on the targeted tracks and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RangeEditParams {
    #[schemars(description = "Timeline time the range starts at (seconds); below 0 is held at 0")]
    from: f64,
    #[schemars(description = "Timeline time the range ends at (seconds); it must be after `from`")]
    to: f64,
    #[schemars(
        description = "The tracks to take the range from (UUIDs from get_timeline_state). Omitted, EVERY UNLOCKED track is targeted; a locked track the range reaches is left alone and listed in the reply's `skipped`. Naming tracks targets only those: a clip on another track is not cut, but a clip LINKED to one that is cut is cut (and, for extract_range, moved) with it, wherever it sits, unless link=false. A named track must exist and not be locked; an empty list is refused (leave it out for all tracks)."
    )]
    track_ids: Option<Vec<String>>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones the range cuts — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are cut with them, each group falls in two by side (the clips before the range keep the link, the ones after it get a new one) and, for extract_range, the later linked clips move together. false edits only the clips on the targeted tracks and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track that the range reaches refuses the whole call, and so does a moved linked sound that would land on a clip it is not linked to (the error names the lane)."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RippleDeleteGapParams {
    #[schemars(description = "UUID of the track that holds the gap")]
    track_id: String,
    #[schemars(
        description = "A timeline time inside the gap (seconds); the gap's own start and end count as inside it. A time inside a clip, with no gap there (clips touching) or with nothing after the gap is refused."
    )]
    at: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, the clips linked to the ones that moved up follow them by the same amount on their own tracks (a picture's detached sound goes with its picture); a moved linked clip that would land on a clip it is not linked to, or on a locked track, refuses the call and names the lane. false moves only this track's clips and ignores links, which can leave a picture and its sound out of sync."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct DuplicateClipsParams {
    #[schemars(description = "UUIDs of the clips to copy")]
    clip_ids: Vec<String>,
    #[schemars(description = "Timeline time the earliest copy should land at, in seconds")]
    at: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SetClipEnabledParams {
    #[schemars(description = "UUID of the clip to enable or disable")]
    clip_id: String,
    #[schemars(description = "false to drop this clip from the render while leaving it on the timeline")]
    enabled: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ConcatParams {
    #[schemars(description = "Ordered list of asset UUIDs to stitch together")]
    asset_ids: Vec<String>,
}

#[tool_router(router = tool_router_editing, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Cut [start, end) of an asset (SOURCE seconds) and append it to the first UNLOCKED track of the asset's main kind — the first unlocked video track for footage, the first unlocked audio track for audio-only media — after the last clip already there (every one of them locked refuses). It does not take a track or a position: add_clip_to_timeline does. Returns the clip."
    )]
    fn cut_clip(&self, Parameters(p): Parameters<CutClipParams>) -> Result<String, McpError> {
        self.edit(|project| Self::cut_clip_op(project, p))
    }

    pub(super) fn cut_clip_op(project: &Project, p: CutClipParams) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let out = project.cut_clip(id, p.start, p.end).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Add a clip referencing a source range of an asset to the timeline. Appended after the \
                       track's last clip unless timeline_start says where. A track you name must suit the clip: \
                       a video track takes media with a picture, an audio track media with sound (footage with \
                       both may go on either), and a locked track refuses; omit track_id to land on the first \
                       unlocked track of the asset's main kind. In ripple mode (get_ripple_mode, or pass \
                       `ripple`) a clip dropped onto footage that is already there pushes it later."
    )]
    fn add_clip_to_timeline(&self, Parameters(p): Parameters<AddClipParams>) -> Result<String, McpError> {
        self.edit(|project| Self::add_clip_to_timeline_op(project, p))
    }

    pub(super) fn add_clip_to_timeline_op(project: &Project, p: AddClipParams) -> Result<String, McpError> {
        let asset_id = parse_arg("asset_id", &p.asset_id)?;
        let track_id = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        let out = project
            .with_ripple(p.ripple, |project| {
                project.add_clip_to_timeline(asset_id, track_id, p.source_in, p.source_out, p.timeline_start)
            })
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Split a timeline clip at a timeline time into two adjacent clips. Linked clips are split \
                       with it (a picture's detached sound is cut at the same moment, and the new halves are \
                       linked to each other); pass link=false to split only this clip."
    )]
    fn split_at(&self, Parameters(p): Parameters<SplitParams>) -> Result<String, McpError> {
        self.edit(|project| Self::split_at_op(project, p))
    }

    pub(super) fn split_at_op(project: &Project, p: SplitParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let (left, right) = project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| project.split_at(clip_id, p.at))
            })
            .map_err(core_err)?;
        json(&serde_json::json!({ "left": left, "right": right }))
    }

    #[tool(
        description = "Trim a clip's source in/out points (timeline position preserved unless timeline_start is \
                       passed). In ripple mode (get_ripple_mode, or pass `ripple`) the later clips on the track \
                       follow the change in the clip's length and a left-edge trim keeps the clip's start. \
                       A linked partner that shares the edge you move (its detached sound or picture) has \
                       that edge moved with it — and the trim is refused if that would put the partner on a clip \
                       outside its group (as move_clip refuses), unless ripple makes room; pass link=false to \
                       trim only this clip. Returns the clip as it ended up."
    )]
    fn trim(&self, Parameters(p): Parameters<TrimParams>) -> Result<String, McpError> {
        self.edit(|project| Self::trim_op(project, p))
    }

    pub(super) fn trim_op(project: &Project, p: TrimParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| {
                    project.trim(clip_id, p.source_in, p.source_out, p.timeline_start)
                })
            })
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Move a clip to a new index within its track (re-flows the track gaplessly). The re-flow \
                       re-lays this one track and the clips linked to what it moved follow (a picture's sound goes where \
                       its picture went), unless an unlinked clip on their track is in the way, which refuses it; \
                       link=false is not offered here — use move_clip to reposition one clip."
    )]
    fn reorder(&self, Parameters(p): Parameters<ReorderParams>) -> Result<String, McpError> {
        self.edit(|project| Self::reorder_op(project, p))
    }

    pub(super) fn reorder_op(project: &Project, p: ReorderParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        project.reorder(track_id, clip_id, p.new_index).map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(
        description = "Move a clip to a new timeline position (free positioning, gaps allowed), optionally onto another \
                       same-kind track; rejects overlaps. Linked clips move with it by the same time, each on its \
                       own track (a picture's detached sound follows it; a track change applies to the clip you \
                       named only); pass link=false to move only this clip. All or nothing — a partner that would \
                       start before 0, land on another clip or sit on a locked track refuses the move."
    )]
    fn move_clip(&self, Parameters(p): Parameters<MoveClipParams>) -> Result<String, McpError> {
        self.edit(|project| Self::move_clip_op(project, p))
    }

    pub(super) fn move_clip_op(project: &Project, p: MoveClipParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let track_id = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        let out = project
            .with_links(p.link, |project| project.move_clip(clip_id, p.timeline_start, track_id))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Move several clips in ONE edit (one revision, one undo step): each entry names a clip, \
                       where it starts afterwards (absolute timeline seconds, not an offset) and optionally \
                       another track of the same kind. All or nothing — an unknown clip, a start before 0, a \
                       clip named twice, a locked track or a landing spot that overlaps a clip that stays (or \
                       another clip in the group) refuses the whole call and changes nothing. Clips moving \
                       together may pass through the places they are leaving, so nudging a run of abutting \
                       clips by a second is fine, and two clips can swap. Never ripples, whatever the ripple \
                       mode. Linked clips of the ones named move with them (by the same time, on their own \
                       tracks) unless link=false, and are checked in the same group. Returns the moved clips in \
                       the order given, then the linked partners carried along."
    )]
    fn move_clips(&self, Parameters(p): Parameters<MoveClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::move_clips_op(project, p))
    }

    pub(super) fn move_clips_op(project: &Project, p: MoveClipsParams) -> Result<String, McpError> {
        let moves = clip_moves(&p.moves)?;
        let out = project
            .with_links(p.link, |project| project.move_clips(&moves))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Remove a clip and close the gap: later clips on the same track shift left by its duration \
                       (ripple delete). Linked clips are removed too, each closing the gap on its own track; pass \
                       link=false to remove only this clip."
    )]
    fn ripple_delete(&self, Parameters(p): Parameters<RippleDeleteParams>) -> Result<String, McpError> {
        self.edit(|project| Self::ripple_delete_op(project, p))
    }

    pub(super) fn ripple_delete_op(project: &Project, p: RippleDeleteParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        project
            .with_links(p.link, |project| project.ripple_delete(clip_id))
            .map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(
        description = "Cut a source-time range out of a clip and close the gap (split + ripple in one edit) — \
                       the transcript-editing primitive: pass a transcript segment's start/end to delete that \
                       sentence from the cut, or a word's (get_transcript, get_cut_transcript, find_filler_words) to delete one word. Linked clips lose the same stretch of timeline (a picture's \
                       detached sound is cut with it); pass link=false to cut only this clip."
    )]
    fn cut_clip_range(&self, Parameters(p): Parameters<CutClipRangeParams>) -> Result<String, McpError> {
        self.edit(|project| Self::cut_clip_range_op(project, p))
    }

    pub(super) fn cut_clip_range_op(project: &Project, p: CutClipRangeParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let pieces = project
            .with_links(p.link, |project| project.cut_clip_range(clip_id, p.from, p.to))
            .map_err(core_err)?;
        json(&pieces)
    }

    #[tool(
        description = "ROLL an edit: move the cut between two adjacent clips of one track (clip_a ends where clip_b \
                       begins) by `delta` seconds, positive = later. Both clips' footage shifts with it — clip_a's end \
                       and clip_b's start move together — so the pair covers the same stretch of timeline and nothing \
                       after it moves. Use it to adjust where a cut falls without touching the length of the cut. \
                       Clamps (does not fail) to the footage each clip has left and to a 0.05s floor for the clip that \
                       shrinks; `applied` in the reply says how far it went and `clamped` whether that is short of \
                       `delta`. Errors if the clips are not adjacent, on different tracks, in the wrong order, or the \
                       track is locked. One revision. Never ripples, whatever the ripple mode. The cut of each \
                       linked partner pair that shares this one rolls too (clamped to the tightest) unless link=false."
    )]
    fn roll_edit(&self, Parameters(p): Parameters<RollEditParams>) -> Result<String, McpError> {
        self.edit(|project| Self::roll_edit_op(project, p))
    }

    pub(super) fn roll_edit_op(project: &Project, p: RollEditParams) -> Result<String, McpError> {
        let (a, b) = (parse_arg("clip_a", &p.clip_a)?, parse_arg("clip_b", &p.clip_b)?);
        let out = project
            .with_links(p.link, |project| project.roll_edit(a, b, p.delta))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "SLIP a clip: show a different part of its footage without moving it or changing its length — \
                       the clip keeps its place and duration on the timeline and only its source window shifts. \
                       `delta` is in SOURCE seconds; positive makes the clip start later in its own footage, negative \
                       earlier (a reversed clip is mirrored for you). Clamps to the asset's footage — `applied` says how \
                       far it went — and errors when there is none left, for a still image (nothing to slip) or a \
                       locked track. Keyframes and fades are timed to the clip, so they stay put. One revision. Never \
                       ripples. Linked partners slip by the same moment of footage unless link=false."
    )]
    fn slip_clip(&self, Parameters(p): Parameters<SlipClipParams>) -> Result<String, McpError> {
        self.edit(|project| Self::slip_clip_op(project, p))
    }

    pub(super) fn slip_clip_op(project: &Project, p: SlipClipParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .with_links(p.link, |project| project.slip_clip(clip_id, p.delta))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "SLIDE a clip along its track, keeping its content: it moves by `delta` timeline seconds \
                       (positive = later) while the clips touching it give way — the previous clip's end and the next \
                       clip's start move by the same amount, so the clip's footage and the overall length of the three \
                       are unchanged. A neighbour across a gap is not touched: with a gap on one side only the touching \
                       neighbour is trimmed (or extended), and the clip stops where it would meet a clip it is not \
                       touching. The last clip of a track has no next clip to give way, so sliding it later extends the \
                       track. Clamps (does not fail) to the neighbours' footage and a 0.05s floor, and to 0 at the start \
                       of the track; `applied` says how far it went. Errors if nothing can move or the track is locked. \
                       One revision. Never ripples. Linked partners slide with it, each with its own neighbours \
                       giving way and the whole slide clamped to the tightest, unless link=false."
    )]
    fn slide_clip(&self, Parameters(p): Parameters<SlideClipParams>) -> Result<String, McpError> {
        self.edit(|project| Self::slide_clip_op(project, p))
    }

    pub(super) fn slide_clip_op(project: &Project, p: SlideClipParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .with_links(p.link, |project| project.slide_clip(clip_id, p.delta))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Split a clip at a timeline time and REMOVE one half in a single edit — trim the start (side \
                       \"left\") or the end (side \"right\") to that time. The half that stays keeps the clip's id, and \
                       loses what belonged to the removed half: removing the left drops fade_in and the transition into \
                       it, removing the right drops fade_out. In ripple mode (get_ripple_mode, or pass `ripple`) the \
                       later clips on the track close the gap; otherwise the gap stays. Linked partners that span \
                       `at` are trimmed to it as well (link=false: this clip only) — but only the named clip comes \
                       back: read the partners from get_timeline_state, or use split_remove_clips, which returns \
                       every clip it cut. Returns the clip that remains. \
                       Errors if `at` is not inside the clip or would leave under 0.05s, or the track is locked."
    )]
    fn split_remove(&self, Parameters(p): Parameters<SplitRemoveParams>) -> Result<String, McpError> {
        self.edit(|project| Self::split_remove_op(project, p))
    }

    pub(super) fn split_remove_op(project: &Project, p: SplitRemoveParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let out = project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| project.split_remove(clip_id, p.at, p.side))
            })
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Split and REMOVE one half of SEVERAL clips as ONE edit (one revision, one undo step) — trim \
                       the starts (side \"left\") or the ends (side \"right\") of, say, a video clip and its audio \
                       to the same time. Each cut is split_remove's: the half that stays keeps its clip's id and \
                       loses what belonged to the removed half. All or nothing — an unknown clip, a locked track, a \
                       cut outside its clip or one that would leave under 0.05s refuses the whole call and changes \
                       nothing — and at most one clip per track. In ripple mode (get_ripple_mode, or pass `ripple`) \
                       each track closes its own gap. A linked partner of a named clip that you did not name is cut \
                       at the same time when the time is inside it (link=false: exactly what you named). Returns \
                       the clips that remain, in the order given, then the partners'."
    )]
    fn split_remove_clips(&self, Parameters(p): Parameters<SplitRemoveClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::split_remove_clips_op(project, p))
    }

    pub(super) fn split_remove_clips_op(project: &Project, p: SplitRemoveClipsParams) -> Result<String, McpError> {
        let cuts = clip_cuts(&p.cuts)?;
        let out = project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| project.split_remove_clips(&cuts, p.side))
            })
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "CUT EVERY TRACK at one timeline time — Premiere's \"Add Edit to All Tracks\", Resolve's \"Split Clip\" on all \
                       tracks: each clip with `at` strictly inside it is split in two there, on EVERY UNLOCKED track by default \
                       (or only on `track_ids`). Nothing is removed and nothing moves, so ripple mode makes no difference. A track \
                       with no clip under `at`, or whose clip already has an edge at `at`, is passed over; a time where nothing \
                       at all can be cut is refused and the error says why. Linked clips are cut with the clip they are linked to \
                       (a picture's detached sound, even on a track you did not name; link=false: only the targeted tracks' \
                       clips) and the halves are linked by side. Locked tracks are left alone and listed under `skipped` (a \
                       locked LINKED partner of a clip that is cut refuses the whole call). Returns `cuts`: per clip cut, its \
                       `track_id` and the `left` half (keeps the clip's id) and the `right` half (a new clip), in track order. \
                       One revision, so one undo step."
    )]
    pub(super) fn split_all_tracks(&self, Parameters(p): Parameters<SplitAllTracksParams>) -> Result<String, McpError> {
        self.edit(|project| Self::split_all_tracks_op(project, p))
    }

    pub(super) fn split_all_tracks_op(project: &Project, p: SplitAllTracksParams) -> Result<String, McpError> {
        let tracks = track_ids(p.track_ids.as_deref())?;
        let out = project
            .with_links(p.link, |project| project.split_all_tracks(p.at, tracks.as_deref()))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "LIFT the timeline range [from, to) off the targeted tracks and LEAVE THE GAP — Premiere's Lift, Resolve's \
                       delete with ripple off. The targeted tracks are `track_ids`, or by default EVERY UNLOCKED track. A clip \
                       wholly inside the range is removed; one the range overlaps at one end is shortened at that end; one the \
                       range sits in the middle of is cut in two around it (the right half is a new clip). Footage, speed ramps, \
                       keyframes, fades and transitions at the new edges are what a split and a trim to the playhead make of them. \
                       Everything after the range stays exactly where it is, so what was after it no longer follows what was \
                       before: use extract_range to take the stretch out AND close the gap. Ignores ripple mode (a lift that \
                       rippled would be an extract). Linked clips the range reaches are cut with it (link=false: only the \
                       targeted tracks' clips); a title clip on a track is a clip and is cut like one, while text overlays, floating animated \
                       titles (add_svg_overlay) and markers are not touched. Refused, changing nothing: an empty, reversed or \
                       non-finite range, a range with nothing in it on the targeted tracks, a locked or unknown track you named, a locked linked partner the range reaches. Locked tracks \
                       left alone are listed under `skipped`. Returns what was `removed`, `trimmed` and `split`, and the `tracks` \
                       that changed. One revision, so one undo step."
    )]
    pub(super) fn lift_range(&self, Parameters(p): Parameters<RangeEditParams>) -> Result<String, McpError> {
        self.edit(|project| Self::lift_range_op(project, p))
    }

    pub(super) fn lift_range_op(project: &Project, p: RangeEditParams) -> Result<String, McpError> {
        let tracks = track_ids(p.track_ids.as_deref())?;
        let out = project
            .with_links(p.link, |project| project.lift_range(p.from, p.to, tracks.as_deref()))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "EXTRACT the timeline range [from, to) from the targeted tracks and CLOSE THE GAP — Premiere's Extract, \
                       Resolve's ripple delete of a range. The targeted tracks are `track_ids`, or by default EVERY UNLOCKED \
                       track. It is lift_range, then every clip of those tracks that starts at or after `to` moves earlier by \
                       `to - from`, each track on its own (the clips linked to them move with them). Clips of a track you did not \
                       target, if they are not linked to a clip that moved, stay where they are — and so do TEXT OVERLAYS, \
                       floating animated titles (add_svg_overlay) and MARKERS, which do NOT move on an extract (as in every \
                       ripple edit), so they can end up out of place against the picture afterwards: re-place them \
                       (get_timeline_state, update_overlay, update_marker), and generate captions only after the cut is settled. Ignores ripple mode: it closes the gap \
                       itself, once. The range may hold no clip when something follows it on a targeted track — then it just \
                       closes that stretch up. Refused, changing nothing: an empty, reversed or non-finite range, a range with \
                       nothing in it and nothing after it, a locked or unknown track you named, a locked linked partner the range \
                       reaches, a moved linked sound that would land on a clip it is not linked to (the error names the lane). \
                       Locked tracks left alone are listed under `skipped`. Returns what was `removed`, `trimmed`, `split` and \
                       `moved`, how far (`closed`, seconds) and the `tracks` that changed. One revision, so one undo step."
    )]
    pub(super) fn extract_range(&self, Parameters(p): Parameters<RangeEditParams>) -> Result<String, McpError> {
        self.edit(|project| Self::extract_range_op(project, p))
    }

    pub(super) fn extract_range_op(project: &Project, p: RangeEditParams) -> Result<String, McpError> {
        let tracks = track_ids(p.track_ids.as_deref())?;
        let out = project
            .with_links(p.link, |project| project.extract_range(p.from, p.to, tracks.as_deref()))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "RIPPLE DELETE A GAP: close the empty stretch of ONE track that `at` falls in (get_timeline_state / \
                       timeline_summary list each track's gaps) — every later clip of that track moves earlier by the gap's \
                       length, from the end of the clip before the gap (0 at the head of the track) to the start of the clip \
                       after it. The clips linked to the ones that moved follow them (a picture's detached sound), unless \
                       link=false; unlinked clips of other tracks, text overlays and markers stay where they are, so a V1 gap \
                       closed this way leaves an unrelated music track untouched. Ignores ripple mode (this is the ripple). \
                       Refused, changing nothing: a locked or unknown track, a time inside a clip, a time with no gap \
                       (touching clips), a gap with nothing after it, a moved linked clip that would land on a clip it is not \
                       linked to (the error names the lane). Returns the gap's `from` and `to` and how many clips `moved`. One \
                       revision, so one undo step."
    )]
    pub(super) fn ripple_delete_gap(&self, Parameters(p): Parameters<RippleDeleteGapParams>) -> Result<String, McpError> {
        self.edit(|project| Self::ripple_delete_gap_op(project, p))
    }

    pub(super) fn ripple_delete_gap_op(project: &Project, p: RippleDeleteGapParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let out = project
            .with_links(p.link, |project| project.ripple_delete_gap(track_id, p.at))
            .map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Add a new empty track (\"video\" or \"audio\"), e.g. a B-roll lane above the interview; later video tracks composite on top at export"
    )]
    fn add_track(&self, Parameters(p): Parameters<AddTrackParams>) -> Result<String, McpError> {
        self.edit(|project| Self::add_track_op(project, p))
    }

    pub(super) fn add_track_op(project: &Project, p: AddTrackParams) -> Result<String, McpError> {
        let kind = parse_kind(&p.kind)?;
        let track = project.add_track(kind, p.name).map_err(core_err)?;
        json(&track)
    }

    #[tool(description = "Remove a track and all of its clips (refuses to remove the last track)")]
    fn remove_track(&self, Parameters(p): Parameters<TrackIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::remove_track_op(project, p))
    }

    pub(super) fn remove_track_op(project: &Project, p: TrackIdParams) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        project.remove_track(track_id).map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(
        description = "Enable or disable one clip. A disabled clip keeps its position, trims, effects and \
                       keyframes but drops out of the render — the reversible way to try a cut without it. \
                       A picture whose sound was detached (`source_audio: false`) carries no sound: disabling it \
                       leaves its linked audio clip playing, so disable that clip too."
    )]
    fn set_clip_enabled(&self, Parameters(p): Parameters<SetClipEnabledParams>) -> Result<String, McpError> {
        self.edit(|project| Self::set_clip_enabled_op(project, p))
    }

    pub(super) fn set_clip_enabled_op(project: &Project, p: SetClipEnabledParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let clip = project.set_clip_enabled(clip_id, p.enabled).map_err(core_err)?;
        json(&clip)
    }

    #[tool(
        description = "Copy clips and insert the copies so the earliest lands at `at`, keeping the \
                       relative offsets between them and each one's track. Everything comes along — \
                       trims, speed, transform, color, effects, keyframes, reframe — which \
                       add_clip_to_timeline cannot do, since that builds a fresh clip. Rejected \
                       outright if any copy would overlap, so a partial paste never lands."
    )]
    fn duplicate_clips(&self, Parameters(p): Parameters<DuplicateClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::duplicate_clips_op(project, p))
    }

    pub(super) fn duplicate_clips_op(project: &Project, p: DuplicateClipsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<_>, _>>()?;
        let clips = project.duplicate_clips(&ids, p.at).map_err(core_err)?;
        json(&clips)
    }

    #[tool(
        description = "Remove a clip from the timeline. Leaves a gap where it was, unless the project is in ripple \
                       mode (get_ripple_mode) or `ripple` is true, which closes it. Linked clips are removed with \
                       it (a picture's detached sound goes too) unless link=false."
    )]
    fn remove(&self, Parameters(p): Parameters<RemoveClipParams>) -> Result<String, McpError> {
        self.edit(|project| Self::remove_op(project, p))
    }

    pub(super) fn remove_op(project: &Project, p: RemoveClipParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| project.remove(clip_id))
            })
            .map_err(core_err)?;
        Ok("ok".to_string())
    }

    #[tool(
        description = "Remove several clips in ONE edit (one revision, one undo step) — a multi-select delete. \
                       All or nothing: an unknown clip, or one on a locked track, refuses the whole call. Leaves \
                       gaps, unless the project is in ripple mode (get_ripple_mode) or `ripple` is true, which \
                       closes every track up behind what it lost (a multi-clip ripple delete). Linked partners of \
                       the named clips are removed too unless link=false. Returns \
                       `removed` (how many clips, partners included), `ripple_active` (whether ripple applied to this call) and, \
                       measured rather than assumed, `rippled` / `clips_shifted` (whether any later clip actually \
                       moved, and how many — ripple on with nothing after the removed clips moves nothing)."
    )]
    fn remove_clips(&self, Parameters(p): Parameters<RemoveClipsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::remove_clips_op(project, p))
    }

    pub(super) fn remove_clips_op(project: &Project, p: RemoveClipsParams) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<_>, _>>()?;
        // What the agent sees is the cut it is building: the proposal while it has
        // one staged, the live timeline otherwise.
        let before = project.working_timeline().map_err(core_err)?;
        let (removed, ripple) = project
            .with_links(p.link, |project| {
                project.with_ripple(p.ripple, |project| {
                    let ripple = project.ripple_active()?;
                    project.remove_clips(&ids).map(|removed| (removed, ripple))
                })
            })
            .map_err(core_err)?;
        // Ripple on is a promise to try, not a result: a locked track never moves
        // and a lane the shift would leave overlapping is declined, silently.
        // So say what happened.
        let shifted = project.working_timeline().map_err(core_err)?.clips_moved_since(&before);
        json(&serde_json::json!({
            "removed": removed,
            "ripple_active": ripple,
            "rippled": shifted > 0,
            "clips_shifted": shifted,
        }))
    }

    #[tool(
        description = "Whether the project is in ripple mode. In ripple mode an edit that changes how much footage \
                       sits ahead of a clip — a trim, a speed change, a remove, an add onto footage that is there, \
                       a voiceover placed over footage — carries the later clips on the same track along, keeping \
                       their gaps. Tracks ripple on their own, except that a clip the ripple moved takes its \
                       LINKED partners along by the same amount (a picture's detached sound keeps with it, a \
                       J/L-cut offset included; other clips on that audio track stay put), a locked track never \
                       moves (and a lane the shift would leave overlapping is left as the edit made it), and titles \
                       and markers stay put. Off by default; it is the \
                       user's toolbar setting and is saved with the project. Check it before trimming or \
                       removing, and pass `ripple` on those tools to override it for one call."
    )]
    fn get_ripple_mode(&self) -> Result<String, McpError> {
        let on = self.lock().ripple_mode().map_err(core_err)?;
        json(&serde_json::json!({ "ripple_mode": on }))
    }

    #[tool(
        description = "Turn ripple mode on or off for the project (see get_ripple_mode). This flips the USER's \
                       setting, which shows in their toolbar and stays until changed again, and it applies at \
                       once — also to your staged edits. It is not an edit: no revision, and the timeline does \
                       not move. While the user has agent review switched on (the default) it is refused: pass \
                       `ripple` to trim / set_speed / set_speed_ramp / remove / remove_clips / add_clip_to_timeline to ripple (or not) \
                       for one call, or ask the user to switch modes."
    )]
    fn set_ripple_mode(&self, Parameters(p): Parameters<SetRippleModeParams>) -> Result<String, McpError> {
        let on = {
            let project = self.lock();
            project.set_ripple_mode(p.on).map_err(core_err)?;
            project.ripple_mode().map_err(core_err)?
        };
        // The toolbar toggle reads the flag when a project loads, so it has to be
        // told. Not `project-changed`: nothing it re-fetches moved.
        self.notify("ripple-mode-changed");
        json(&serde_json::json!({ "ripple_mode": on }))
    }

    #[tool(
        description = "Append the non-silent spans of an asset as clips, end to end after what the first UNLOCKED track of the asset's main kind already holds (the first unlocked video track for footage, the first unlocked audio track for audio-only media; every one of them locked refuses), as one revision. It needs the asset's `silence` analysis (analyze_asset with steps [\"silence\"]): with no analysis at all it fails, and if the silence step was never run it keeps the whole file. The silent spans are dropped, not rippled: nothing already on the track moves. Returns the clips."
    )]
    fn remove_silence(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        self.edit(|project| Self::remove_silence_op(project, p))
    }

    pub(super) fn remove_silence_op(project: &Project, p: AssetIdParams) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let out = project.remove_silence(id).map_err(core_err)?;
        json(&out)
    }

    #[tool(
        description = "Append the FULL length of each asset in `asset_ids`, in order, to the end of the first UNLOCKED track of its kind (footage on the first unlocked video track, audio-only media on the first unlocked audio track; every one of a kind locked refuses), as one revision — so a video clip and an audio-only clip do not play in sequence with each other, each kind has its own run. For part of an asset use cut_clip or add_clip_to_timeline. Returns the clips."
    )]
    fn concatenate(&self, Parameters(p): Parameters<ConcatParams>) -> Result<String, McpError> {
        self.edit(|project| Self::concatenate_op(project, p))
    }

    pub(super) fn concatenate_op(project: &Project, p: ConcatParams) -> Result<String, McpError> {
        let ids = p
            .asset_ids
            .iter()
            .map(|s| parse_arg("asset_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let out = project.concatenate(&ids).map_err(core_err)?;
        json(&out)
    }
}
