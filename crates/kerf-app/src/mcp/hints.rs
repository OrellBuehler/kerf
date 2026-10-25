//! Every tool's title and behaviour hints (`readOnlyHint`, `destructiveHint`,
//! `idempotentHint`, `openWorldHint`), which a client uses to decide what to run
//! without asking and what to confirm first.
//!
//! An agent's timeline edits stage (or land as an undoable revision), so they are
//! not destructive. What is: applying or discarding a proposal, undo / redo /
//! revert on the live cut, a file written outside the project, data deleted.
//! Open world means the tool may download a model.
//!
//! The last column is the tool set that lists it for a client that asks for only some sets
//! (`toolsets.rs`).

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::ToolAnnotations;

use super::toolsets::ToolSet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Effect {
    /// Reads the project or the media and changes nothing.
    Read,
    /// A staged (or undoable) edit; running it twice does it twice.
    Edit,
    /// A staged (or undoable) edit to an absolute value; running it twice changes nothing more.
    Set,
    /// Writes caches or settings, and may download a model first.
    Fetch,
    /// Adds generated media, and may download a model first.
    Generate,
    /// Replaces or removes something that is not staged: the user's cut, a file, a task.
    Destroy,
    /// The same, but a second call has nothing left to remove.
    Erase,
}

impl Effect {
    fn annotations(self) -> ToolAnnotations {
        let (read_only, destructive, idempotent, open_world) = match self {
            Effect::Read => (true, false, true, false),
            Effect::Edit => (false, false, false, false),
            Effect::Set => (false, false, true, false),
            Effect::Fetch => (false, false, true, true),
            Effect::Generate => (false, false, false, true),
            Effect::Destroy => (false, true, false, false),
            Effect::Erase => (false, true, true, false),
        };
        ToolAnnotations::new()
            .read_only(read_only)
            .destructive(destructive)
            .idempotent(idempotent)
            .open_world(open_world)
    }
}

use Effect::*;
use ToolSet::*;

pub(super) const TOOLS: &[(&str, &str, Effect, ToolSet)] = &[
    ("list_fonts", "List fonts", Read, Titles),
    ("import_asset", "Import media", Set, Core),
    ("list_assets", "List assets", Read, Core),
    ("get_asset_metadata", "Asset metadata", Read, Core),
    ("get_scopes", "Scopes of a frame", Read, Analysis),
    ("get_transcript", "Transcript", Read, Analysis),
    ("get_cut_transcript", "Transcript of the cut", Read, Core),
    ("search_transcripts", "Search transcripts", Read, Analysis),
    ("speaker_ranges", "Speaker time ranges", Read, Analysis),
    ("find_filler_words", "Find filler words", Read, Analysis),
    ("refine_transcript", "Refine transcript", Set, Analysis),
    ("proxy_status", "Proxy status", Read, Analysis),
    ("analysis_status", "Analysis status", Read, Analysis),
    ("rebuild_proxy", "Rebuild proxy", Set, Analysis),
    ("delete_proxy", "Delete proxy", Erase, Analysis),
    ("get_timeline_state", "Timeline state", Read, Core),
    ("get_editor_context", "Editor context", Read, Core),
    ("get_editing_guide", "Editing guide", Read, Core),
    ("transcription_status", "Transcription status", Read, Analysis),
    ("download_speech_model", "Download speech model", Fetch, Analysis),
    ("analyze_asset", "Analyze asset", Fetch, Core),
    ("set_speech_model", "Set speech model", Set, Analysis),
    ("cut_clip", "Cut clip", Edit, Core),
    ("add_clip_to_timeline", "Add clip", Edit, Core),
    ("split_at", "Split clip", Edit, Core),
    ("trim", "Trim clip", Set, Core),
    ("reorder", "Reorder clip", Set, Core),
    ("move_clip", "Move clip", Set, Core),
    ("move_clips", "Move clips", Set, Core),
    ("ripple_delete", "Ripple delete", Edit, Core),
    ("cut_clip_range", "Cut range from clip", Edit, Core),
    ("roll_edit", "Roll edit", Edit, Core),
    ("slip_clip", "Slip clip", Edit, Core),
    ("slide_clip", "Slide clip", Edit, Core),
    ("split_remove", "Split and remove", Edit, Core),
    ("split_remove_clips", "Split and remove clips", Edit, Core),
    ("apply_edits", "Apply several edits", Edit, Core),
    ("add_track", "Add track", Edit, Core),
    ("remove_track", "Remove track", Edit, Core),
    ("set_track_duck", "Duck track", Set, Audio),
    ("set_mask", "Set mask", Set, Clips),
    ("set_track_volume", "Track volume", Set, Audio),
    ("set_track_pan", "Track pan", Set, Audio),
    ("set_master_volume", "Master volume", Set, Audio),
    ("set_master_limiter", "Master limiter", Set, Audio),
    ("set_master_duck", "Master duck", Set, Audio),
    ("set_master_true_peak", "Master true peak", Set, Audio),
    ("sync_clips_by_audio", "Sync clips by audio", Set, Analysis),
    ("set_delivery_format", "Delivery format", Set, Delivery),
    ("add_marker", "Add marker", Edit, Titles),
    ("update_marker", "Update marker", Set, Titles),
    ("remove_marker", "Remove marker", Edit, Titles),
    ("set_track_muted", "Mute track", Set, Audio),
    ("set_track_solo", "Solo track", Set, Audio),
    ("set_track_locked", "Lock track", Set, Core),
    ("set_clip_enabled", "Enable clip", Set, Clips),
    ("duplicate_clips", "Duplicate clips", Edit, Core),
    ("remove", "Remove clip", Edit, Core),
    ("remove_clips", "Remove clips", Edit, Core),
    ("get_ripple_mode", "Ripple mode", Read, Core),
    ("set_ripple_mode", "Set ripple mode", Set, Core),
    ("set_volume", "Clip volume", Set, Core),
    ("set_fade", "Clip fades", Set, Core),
    ("set_speed", "Clip speed", Set, Clips),
    ("set_speed_ramp", "Clip speed ramp", Set, Clips),
    ("set_transform", "Clip transform", Set, Clips),
    ("set_transform_at", "Clip transform at a moment", Set, Clips),
    ("set_color", "Clip colour", Set, Clips),
    ("set_grade", "Clip grade", Set, Clips),
    ("set_transition", "Set transition", Set, Core),
    ("set_video_effects", "Video effects", Set, Clips),
    ("set_audio_effects", "Audio effects", Set, Clips),
    ("set_keyframes", "Set keyframes", Set, Keyframes),
    ("add_keyframe", "Add keyframe", Set, Keyframes),
    ("set_keyframe_easing", "Keyframe easing", Set, Keyframes),
    ("set_property_keyframes", "Property keyframes", Set, Keyframes),
    ("set_property_tracks", "Property tracks", Set, Keyframes),
    ("copy_keyframes", "Copy keyframes", Set, Keyframes),
    ("clear_keyframes", "Clear keyframes", Set, Keyframes),
    ("set_reframe", "Aim 360 camera", Set, Keyframes),
    ("set_asset_projection", "Asset projection", Set, Keyframes),
    ("clear_reframe", "Clear 360 reframe", Set, Keyframes),
    ("set_reframe_keyframes", "360 camera keyframes", Set, Keyframes),
    ("add_reframe_keyframe", "Add 360 camera keyframe", Set, Keyframes),
    ("add_overlay", "Add text overlay", Edit, Titles),
    ("update_overlay", "Update text overlay", Set, Titles),
    ("remove_overlay", "Remove text overlay", Edit, Titles),
    ("set_overlay_keyframes", "Overlay keyframes", Set, Titles),
    ("add_svg_overlay", "Add SVG title", Edit, Titles),
    ("update_svg_overlay", "Update SVG title", Set, Titles),
    ("remove_svg_overlay", "Remove SVG title", Edit, Titles),
    ("add_title_clip", "Add title clip", Edit, Titles),
    ("update_title_clip", "Update title clip", Set, Titles),
    ("list_svg_presets", "List SVG title presets", Read, Titles),
    ("validate_svg", "Validate SVG", Read, Titles),
    ("preview_svg_overlay", "Preview SVG title", Read, Titles),
    ("list_templates", "List templates", Read, Titles),
    ("save_template", "Save template", Edit, Titles),
    ("apply_template", "Apply template", Edit, Titles),
    ("set_template_values", "Set template values", Set, Titles),
    ("set_title_text_box", "Set title text box", Set, Titles),
    ("set_title_text_animation", "Set title text animation", Set, Titles),
    ("list_text_animation_presets", "List text animation presets", Read, Titles),
    ("list_text_animation_curves", "List text animation curves", Read, Titles),
    ("convert_title_text", "Convert title text", Set, Titles),
    ("import_template", "Import template", Set, Titles),
    ("export_template", "Export template", Destroy, Titles),
    ("delete_template", "Delete template", Erase, Titles),
    ("generate_captions", "Generate captions", Set, Titles),
    ("import_captions", "Import captions", Set, Titles),
    ("voiceover_status", "Voiceover status", Read, Audio),
    ("generate_voiceover", "Generate voiceover", Generate, Audio),
    ("music_status", "Music status", Read, Music),
    ("compose_music", "Compose music", Generate, Music),
    ("stems_status", "Stem separation status", Read, Music),
    ("separate_stems", "Separate stems", Generate, Music),
    ("clear_captions", "Clear captions", Set, Titles),
    ("export_srt", "Export subtitles", Destroy, Delivery),
    ("export_captions", "Export captions", Destroy, Delivery),
    ("export_interchange", "Export for another editor", Destroy, Delivery),
    ("export_markers", "Export markers", Destroy, Delivery),
    ("remove_silence", "Remove silence", Edit, Core),
    ("smart_crop", "Smart crop", Set, Delivery),
    ("snap_to_beats", "Snap cuts to beats", Edit, Music),
    ("get_music_structure", "Music structure", Read, Music),
    ("plan_music_fit", "Plan music fit", Read, Music),
    ("fit_music", "Fit music", Edit, Music),
    ("extract_audio", "Extract audio", Edit, Clips),
    ("add_asset_audio", "Add asset audio", Edit, Clips),
    ("detach_audio", "Detach audio", Edit, Clips),
    ("detach_audio_clips", "Detach audio of clips", Edit, Clips),
    ("reattach_audio", "Reattach audio", Edit, Clips),
    ("reattach_audio_clips", "Reattach audio of clips", Edit, Clips),
    ("link_clips", "Link clips", Set, Clips),
    ("unlink_clips", "Unlink clips", Set, Clips),
    ("concatenate", "Concatenate assets", Edit, Core),
    ("stage_edits", "Stage edits", Set, Core),
    ("staged_diff", "Staged diff", Read, Core),
    ("apply_staged_edits", "Apply staged edits", Destroy, Core),
    ("discard_staged_edits", "Discard staged edits", Erase, Core),
    ("revision_diff", "Revision diff", Read, Core),
    ("history", "Edit history", Read, Core),
    ("undo", "Undo", Destroy, Core),
    ("redo", "Redo", Destroy, Core),
    ("revert_to", "Revert to revision", Destroy, Core),
    ("export_capabilities", "Export capabilities", Read, Delivery),
    ("export", "Export", Destroy, Delivery),
    ("export_variants", "Export variants", Destroy, Delivery),
    ("platform_check", "Platform check", Read, Core),
    ("review_cut", "Review the cut", Read, Core),
    ("export_cover", "Export cover", Destroy, Delivery),
    ("add_thumbnail", "Add thumbnail", Edit, Delivery),
    ("update_thumbnail", "Update thumbnail", Set, Delivery),
    ("remove_thumbnail", "Remove thumbnail", Edit, Delivery),
    ("preview_thumbnail", "Preview thumbnail", Read, Delivery),
    ("export_thumbnail", "Export thumbnail", Destroy, Delivery),
    ("suggest_thumbnail_frames", "Suggest thumbnail frames", Read, Delivery),
    ("list_tasks", "List tasks", Read, Tasks),
    ("add_task", "Add task", Edit, Tasks),
    ("claim_next_task", "Claim next task", Edit, Tasks),
    ("complete_task", "Complete task", Edit, Tasks),
    ("fail_task", "Fail task", Edit, Tasks),
    ("resolve_task", "Resolve task", Destroy, Tasks),
    ("remove_task", "Remove task", Erase, Tasks),
    ("get_waveform", "Waveform", Read, Analysis),
    ("get_waveform_range", "Waveform range", Read, Analysis),
    ("get_energy", "Energy envelope", Read, Analysis),
    ("get_levels", "Measure levels", Read, Audio),
    ("get_band_balance", "Band balance", Read, Audio),
    ("get_frame", "Get frame", Read, Core),
    ("skim_asset", "Skim asset", Read, Core),
    ("skim_timeline", "Skim timeline", Read, Core),
    ("preview_timeline", "Preview timeline", Read, Core),
    ("timeline_summary", "Timeline summary", Read, Core),
    ("preview_svg_preset", "Preview SVG title preset", Read, Titles),
    ("missing_assets", "Missing media", Read, Project),
    ("get_analysis_detail", "Analysis detail", Read, Analysis),
    ("undo_staged_edit", "Undo staged step", Destroy, Core),
    ("restore_discarded_edits", "Restore discarded edits", Edit, Core),
    ("apply_staged_subset", "Apply part of staged edits", Destroy, Core),
    ("job_status", "Job status", Read, Core),
    ("cancel_job", "Cancel job", Erase, Core),
    ("list_jobs", "List jobs", Read, Core),
    ("project_info", "Project info", Read, Core),
    ("save_project", "Save project", Set, Core),
    ("save_project_as", "Save project as", Destroy, Project),
    ("open_project", "Open project", Destroy, Project),
    ("new_project", "New project", Destroy, Project),
    ("get_settings", "Agent-visible settings", Read, Project),
    ("set_settings", "Change settings", Set, Project),
];

/// Give every route its title and hints. A tool missing from [`TOOLS`] is left
/// bare, which `every_tool_is_titled_and_annotated` refuses.
pub(super) fn annotate<S>(router: &mut ToolRouter<S>) {
    for route in router.map.values_mut() {
        if let Some((_, title, effect, _)) = TOOLS.iter().find(|(name, ..)| *name == route.attr.name) {
            route.attr.title = Some(title.to_string());
            route.attr.annotations = Some(effect.annotations());
        }
    }
}
