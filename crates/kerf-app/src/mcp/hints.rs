//! Every tool's title and behaviour hints (`readOnlyHint`, `destructiveHint`,
//! `idempotentHint`, `openWorldHint`), which a client uses to decide what to run
//! without asking and what to confirm first.
//!
//! An agent's timeline edits stage (or land as an undoable revision), so they are
//! not destructive. What is: applying or discarding a proposal, undo / redo /
//! revert on the live cut, a file written outside the project, data deleted.
//! Open world means the tool may download a model.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::ToolAnnotations;

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

pub(super) const TOOLS: &[(&str, &str, Effect)] = &[
    ("list_fonts", "List fonts", Read),
    ("import_asset", "Import media", Set),
    ("list_assets", "List assets", Read),
    ("get_asset_metadata", "Asset metadata", Read),
    ("get_scopes", "Scopes of a frame", Read),
    ("get_transcript", "Transcript", Read),
    ("speaker_ranges", "Speaker time ranges", Read),
    ("find_filler_words", "Find filler words", Read),
    ("refine_transcript", "Refine transcript", Set),
    ("proxy_status", "Proxy status", Read),
    ("analysis_status", "Analysis status", Read),
    ("rebuild_proxy", "Rebuild proxy", Set),
    ("delete_proxy", "Delete proxy", Erase),
    ("get_timeline_state", "Timeline state", Read),
    ("get_editor_context", "Editor context", Read),
    ("transcription_status", "Transcription status", Read),
    ("download_speech_model", "Download speech model", Fetch),
    ("analyze_asset", "Analyze asset", Fetch),
    ("set_speech_model", "Set speech model", Set),
    ("cut_clip", "Cut clip", Edit),
    ("add_clip_to_timeline", "Add clip", Edit),
    ("split_at", "Split clip", Edit),
    ("trim", "Trim clip", Set),
    ("reorder", "Reorder clip", Set),
    ("move_clip", "Move clip", Set),
    ("move_clips", "Move clips", Set),
    ("ripple_delete", "Ripple delete", Edit),
    ("cut_clip_range", "Cut range from clip", Edit),
    ("roll_edit", "Roll edit", Edit),
    ("slip_clip", "Slip clip", Edit),
    ("slide_clip", "Slide clip", Edit),
    ("split_remove", "Split and remove", Edit),
    ("split_remove_clips", "Split and remove clips", Edit),
    ("add_track", "Add track", Edit),
    ("remove_track", "Remove track", Edit),
    ("set_track_duck", "Duck track", Set),
    ("set_mask", "Set mask", Set),
    ("set_track_volume", "Track volume", Set),
    ("set_track_pan", "Track pan", Set),
    ("set_master_volume", "Master volume", Set),
    ("set_master_limiter", "Master limiter", Set),
    ("set_master_duck", "Master duck", Set),
    ("set_master_true_peak", "Master true peak", Set),
    ("sync_clips_by_audio", "Sync clips by audio", Set),
    ("set_delivery_format", "Delivery format", Set),
    ("add_marker", "Add marker", Edit),
    ("update_marker", "Update marker", Set),
    ("remove_marker", "Remove marker", Edit),
    ("set_track_muted", "Mute track", Set),
    ("set_track_solo", "Solo track", Set),
    ("set_track_locked", "Lock track", Set),
    ("set_clip_enabled", "Enable clip", Set),
    ("duplicate_clips", "Duplicate clips", Edit),
    ("remove", "Remove clip", Edit),
    ("remove_clips", "Remove clips", Edit),
    ("get_ripple_mode", "Ripple mode", Read),
    ("set_ripple_mode", "Set ripple mode", Set),
    ("set_volume", "Clip volume", Set),
    ("set_fade", "Clip fades", Set),
    ("set_speed", "Clip speed", Set),
    ("set_transform", "Clip transform", Set),
    ("set_transform_at", "Clip transform at a moment", Set),
    ("set_color", "Clip colour", Set),
    ("set_grade", "Clip grade", Set),
    ("set_transition", "Set transition", Set),
    ("set_video_effects", "Video effects", Set),
    ("set_audio_effects", "Audio effects", Set),
    ("set_keyframes", "Set keyframes", Set),
    ("add_keyframe", "Add keyframe", Set),
    ("set_keyframe_easing", "Keyframe easing", Set),
    ("set_property_keyframes", "Property keyframes", Set),
    ("set_property_tracks", "Property tracks", Set),
    ("copy_keyframes", "Copy keyframes", Set),
    ("clear_keyframes", "Clear keyframes", Set),
    ("set_reframe", "Aim 360 camera", Set),
    ("set_asset_projection", "Asset projection", Set),
    ("clear_reframe", "Clear 360 reframe", Set),
    ("set_reframe_keyframes", "360 camera keyframes", Set),
    ("add_reframe_keyframe", "Add 360 camera keyframe", Set),
    ("add_overlay", "Add text overlay", Edit),
    ("update_overlay", "Update text overlay", Set),
    ("remove_overlay", "Remove text overlay", Edit),
    ("set_overlay_keyframes", "Overlay keyframes", Set),
    ("add_svg_overlay", "Add SVG title", Edit),
    ("update_svg_overlay", "Update SVG title", Set),
    ("remove_svg_overlay", "Remove SVG title", Edit),
    ("add_title_clip", "Add title clip", Edit),
    ("update_title_clip", "Update title clip", Set),
    ("list_svg_presets", "List SVG title presets", Read),
    ("validate_svg", "Validate SVG", Read),
    ("preview_svg_overlay", "Preview SVG title", Read),
    ("list_templates", "List templates", Read),
    ("save_template", "Save template", Edit),
    ("apply_template", "Apply template", Edit),
    ("set_template_values", "Set template values", Set),
    ("set_title_text_box", "Set title text box", Set),
    ("convert_title_text", "Convert title text", Set),
    ("import_template", "Import template", Set),
    ("export_template", "Export template", Destroy),
    ("delete_template", "Delete template", Erase),
    ("generate_captions", "Generate captions", Set),
    ("import_captions", "Import captions", Set),
    ("voiceover_status", "Voiceover status", Read),
    ("generate_voiceover", "Generate voiceover", Generate),
    ("music_status", "Music status", Read),
    ("compose_music", "Compose music", Generate),
    ("stems_status", "Stem separation status", Read),
    ("separate_stems", "Separate stems", Generate),
    ("clear_captions", "Clear captions", Set),
    ("export_srt", "Export subtitles", Destroy),
    ("export_captions", "Export captions", Destroy),
    ("export_interchange", "Export for another editor", Destroy),
    ("export_markers", "Export markers", Destroy),
    ("remove_silence", "Remove silence", Edit),
    ("smart_crop", "Smart crop", Set),
    ("snap_to_beats", "Snap cuts to beats", Edit),
    ("get_music_structure", "Music structure", Read),
    ("plan_music_fit", "Plan music fit", Read),
    ("fit_music", "Fit music", Edit),
    ("extract_audio", "Extract audio", Edit),
    ("add_asset_audio", "Add asset audio", Edit),
    ("detach_audio", "Detach audio", Edit),
    ("detach_audio_clips", "Detach audio of clips", Edit),
    ("reattach_audio", "Reattach audio", Edit),
    ("reattach_audio_clips", "Reattach audio of clips", Edit),
    ("link_clips", "Link clips", Set),
    ("unlink_clips", "Unlink clips", Set),
    ("concatenate", "Concatenate assets", Edit),
    ("stage_edits", "Stage edits", Set),
    ("staged_diff", "Staged diff", Read),
    ("apply_staged_edits", "Apply staged edits", Destroy),
    ("discard_staged_edits", "Discard staged edits", Erase),
    ("revision_diff", "Revision diff", Read),
    ("history", "Edit history", Read),
    ("undo", "Undo", Destroy),
    ("redo", "Redo", Destroy),
    ("revert_to", "Revert to revision", Destroy),
    ("export_capabilities", "Export capabilities", Read),
    ("export", "Export", Destroy),
    ("export_variants", "Export variants", Destroy),
    ("platform_check", "Platform check", Read),
    ("export_cover", "Export cover", Destroy),
    ("list_tasks", "List tasks", Read),
    ("add_task", "Add task", Edit),
    ("claim_next_task", "Claim next task", Edit),
    ("complete_task", "Complete task", Edit),
    ("fail_task", "Fail task", Edit),
    ("resolve_task", "Resolve task", Destroy),
    ("remove_task", "Remove task", Erase),
    ("get_waveform", "Waveform", Read),
    ("get_waveform_range", "Waveform range", Read),
    ("get_energy", "Energy envelope", Read),
    ("get_levels", "Measure levels", Read),
    ("get_band_balance", "Band balance", Read),
    ("get_frame", "Get frame", Read),
    ("skim_asset", "Skim asset", Read),
    ("preview_timeline", "Preview timeline", Read),
    ("timeline_summary", "Timeline summary", Read),
    ("preview_svg_preset", "Preview SVG title preset", Read),
    ("missing_assets", "Missing media", Read),
    ("get_analysis_detail", "Analysis detail", Read),
    ("undo_staged_edit", "Undo staged step", Destroy),
    ("restore_discarded_edits", "Restore discarded edits", Edit),
    ("apply_staged_subset", "Apply part of staged edits", Destroy),
    ("job_status", "Job status", Read),
    ("cancel_job", "Cancel job", Erase),
    ("list_jobs", "List jobs", Read),
    ("project_info", "Project info", Read),
    ("save_project", "Save project", Set),
    ("save_project_as", "Save project as", Destroy),
    ("open_project", "Open project", Destroy),
    ("new_project", "New project", Destroy),
    ("get_settings", "Agent-visible settings", Read),
    ("set_settings", "Change settings", Set),
];

/// Give every route its title and hints. A tool missing from [`TOOLS`] is left
/// bare, which `every_tool_is_titled_and_annotated` refuses.
pub(super) fn annotate<S>(router: &mut ToolRouter<S>) {
    for route in router.map.values_mut() {
        if let Some((_, title, effect)) = TOOLS.iter().find(|(name, ..)| *name == route.attr.name) {
            route.attr.title = Some(title.to_string());
            route.attr.annotations = Some(effect.annotations());
        }
    }
}
