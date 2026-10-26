//! `apply_edits`: several timeline edits as one revision (or one staged step), all or nothing.
//!
//! Each op names a quick, pure edit tool and takes exactly that tool's arguments — the same
//! parameter struct, the same strict refusal of a key its schema does not list, the same body
//! (`*_op`), so `ripple` and `link` mean what they mean on the tool. The ops run in order inside
//! [`Project::batch`]; a string `"$2.right.id"` in an op's arguments is op 2's answer at that path.

use kerf_core::{Batch, Project};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::JsonObject;
use rmcp::{schemars, tool, tool_router, ErrorData as McpError};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{args, core_err, json, lock_agent_edit, parse_arg, reads, router, KerfMcp};

/// The most ops one call runs.
const MAX_OPS: usize = 200;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ApplyEditsParams {
    /// The edits, run in order. Each is `{tool, args}`: `tool` one of the edit tools listed under it,
    /// `args` exactly the arguments that tool takes (its `ripple` / `link` included).
    /// A string of the form `"$K.path"` anywhere in `args` is replaced by the value at `path` in op
    /// K's answer (K counts from 0 and must be an earlier op; path segments are object keys or array
    /// indices): `"$0.right.id"` is the right half of a split_at at op 0, `"$1.0.id"` the first clip a
    /// move_clips at op 1 returned. Write `"$$…"` for a literal string starting with `$`.
    pub ops: Vec<EditOp>,
    /// Run the edits and report what they would change (the per-op answers, the timeline after and
    /// a diff against the cut now), keeping nothing. Default false.
    pub dry_run: Option<bool>,
    /// The name of the one revision or staged step the edits make (history, staged_diff). Omitted,
    /// it is the edit's own label for one op, else a count and the kinds of edit.
    pub label: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditOp {
    /// The edit tool to run, one of: cut_clip, add_clip_to_timeline, split_at, trim, reorder, move_clip,
    /// move_clips, ripple_delete, cut_clip_range, roll_edit, slip_clip, slide_clip, split_remove,
    /// split_remove_clips, remove, remove_clips, duplicate_clips, concatenate, remove_silence, snap_to_beats,
    /// add_track, remove_track, extract_audio, add_asset_audio, detach_audio, detach_audio_clips,
    /// reattach_audio, reattach_audio_clips, link_clips, unlink_clips, set_volume, set_fade, set_speed,
    /// set_speed_ramp, set_transform, set_transform_at, set_color, set_grade (without a lut_path), set_transition,
    /// set_video_effects, set_audio_effects, set_mask, set_blend_mode, set_clip_enabled, set_keyframes, add_keyframe,
    /// set_keyframe_easing, set_property_keyframes, set_property_tracks, copy_keyframes, clear_keyframes,
    /// set_reframe, clear_reframe, set_reframe_keyframes, add_reframe_keyframe, set_track_volume,
    /// set_track_pan, set_track_duck, set_track_muted, set_track_solo, set_track_locked, set_master_volume,
    /// set_master_limiter, set_master_true_peak, set_master_duck, fit_music, add_overlay, update_overlay,
    /// remove_overlay, set_overlay_keyframes, remove_svg_overlay, clear_captions, add_marker, update_marker,
    /// remove_marker, set_delivery_format.
    pub tool: String,
    /// Its arguments, as the tool takes them on its own.
    #[serde(default)]
    pub args: JsonObject,
}

/// clear_captions takes no arguments.
#[derive(serde::Deserialize)]
struct NoArgs {}

/// The edit tools an op may name, each with the body the tool itself runs. Only quick edits that
/// change the cut and nothing else: no long or blocking job, no file, no library, settings, staging
/// or history.
macro_rules! batchable {
    ($($tool:ident => $op:expr),* $(,)?) => {
        pub(super) const BATCHABLE: &[&str] = &[$(stringify!($tool)),*];

        fn dispatch(project: &Project, tool: &str, args: JsonObject) -> Option<Result<String, McpError>> {
            match tool {
                $(stringify!($tool) => Some(op(project, args, $op)),)*
                _ => None,
            }
        }
    };
}

batchable! {
    cut_clip => KerfMcp::cut_clip_op,
    add_clip_to_timeline => KerfMcp::add_clip_to_timeline_op,
    split_at => KerfMcp::split_at_op,
    trim => KerfMcp::trim_op,
    reorder => KerfMcp::reorder_op,
    move_clip => KerfMcp::move_clip_op,
    move_clips => KerfMcp::move_clips_op,
    ripple_delete => KerfMcp::ripple_delete_op,
    cut_clip_range => KerfMcp::cut_clip_range_op,
    roll_edit => KerfMcp::roll_edit_op,
    slip_clip => KerfMcp::slip_clip_op,
    slide_clip => KerfMcp::slide_clip_op,
    split_remove => KerfMcp::split_remove_op,
    split_remove_clips => KerfMcp::split_remove_clips_op,
    remove => KerfMcp::remove_op,
    remove_clips => KerfMcp::remove_clips_op,
    duplicate_clips => KerfMcp::duplicate_clips_op,
    concatenate => KerfMcp::concatenate_op,
    remove_silence => KerfMcp::remove_silence_op,
    snap_to_beats => KerfMcp::snap_to_beats_op,
    add_track => KerfMcp::add_track_op,
    remove_track => KerfMcp::remove_track_op,
    extract_audio => KerfMcp::extract_audio_op,
    add_asset_audio => KerfMcp::add_asset_audio_op,
    detach_audio => KerfMcp::detach_audio_op,
    detach_audio_clips => KerfMcp::detach_audio_clips_op,
    reattach_audio => KerfMcp::reattach_audio_op,
    reattach_audio_clips => KerfMcp::reattach_audio_clips_op,
    link_clips => KerfMcp::link_clips_op,
    unlink_clips => KerfMcp::unlink_clips_op,
    set_volume => KerfMcp::set_volume_op,
    set_fade => KerfMcp::set_fade_op,
    set_speed => KerfMcp::set_speed_op,
    set_speed_ramp => KerfMcp::set_speed_ramp_op,
    set_transform => KerfMcp::set_transform_op,
    set_transform_at => KerfMcp::set_transform_at_op,
    set_color => KerfMcp::set_color_op,
    set_grade => set_grade,
    set_transition => KerfMcp::set_transition_op,
    set_video_effects => KerfMcp::set_video_effects_op,
    set_audio_effects => KerfMcp::set_audio_effects_op,
    set_mask => KerfMcp::set_mask_op,
    set_blend_mode => KerfMcp::set_blend_mode_op,
    set_clip_enabled => KerfMcp::set_clip_enabled_op,
    set_keyframes => KerfMcp::set_keyframes_op,
    add_keyframe => KerfMcp::add_keyframe_op,
    set_keyframe_easing => KerfMcp::set_keyframe_easing_op,
    set_property_keyframes => KerfMcp::set_property_keyframes_op,
    set_property_tracks => KerfMcp::set_property_tracks_op,
    copy_keyframes => KerfMcp::copy_keyframes_op,
    clear_keyframes => KerfMcp::clear_keyframes_op,
    set_reframe => KerfMcp::set_reframe_op,
    clear_reframe => KerfMcp::clear_reframe_op,
    set_reframe_keyframes => KerfMcp::set_reframe_keyframes_op,
    add_reframe_keyframe => KerfMcp::add_reframe_keyframe_op,
    set_track_volume => KerfMcp::set_track_volume_op,
    set_track_pan => KerfMcp::set_track_pan_op,
    set_track_duck => KerfMcp::set_track_duck_op,
    set_track_muted => KerfMcp::set_track_muted_op,
    set_track_solo => KerfMcp::set_track_solo_op,
    set_track_locked => KerfMcp::set_track_locked_op,
    set_master_volume => KerfMcp::set_master_volume_op,
    set_master_limiter => KerfMcp::set_master_limiter_op,
    set_master_true_peak => KerfMcp::set_master_true_peak_op,
    set_master_duck => KerfMcp::set_master_duck_op,
    fit_music => KerfMcp::fit_music_op,
    add_overlay => KerfMcp::add_overlay_op,
    update_overlay => KerfMcp::update_overlay_op,
    remove_overlay => KerfMcp::remove_overlay_op,
    set_overlay_keyframes => KerfMcp::set_overlay_keyframes_op,
    remove_svg_overlay => KerfMcp::remove_svg_overlay_op,
    clear_captions => |project: &Project, _: NoArgs| KerfMcp::clear_captions_op(project),
    add_marker => KerfMcp::add_marker_op,
    update_marker => KerfMcp::update_marker_op,
    remove_marker => KerfMcp::remove_marker_op,
    set_delivery_format => KerfMcp::set_delivery_format_op,
}

/// Run one op's body on its arguments, read into the tool's own parameter struct.
fn op<P: DeserializeOwned>(
    project: &Project,
    args: JsonObject,
    body: impl FnOnce(&Project, P) -> Result<String, McpError>,
) -> Result<String, McpError> {
    let params = serde_json::from_value(Value::Object(args))
        .map_err(|e| McpError::invalid_params(format!("failed to deserialize parameters: {e}"), None))?;
    body(project, params)
}

/// set_grade without a LUT file: reading one is the tool's own, before its lock (`check`).
fn set_grade(project: &Project, p: super::GradeParams) -> Result<String, McpError> {
    let clip_id = parse_arg("clip_id", &p.clip_id)?;
    KerfMcp::set_grade_op(project, clip_id, p)
}

#[tool_router(router = tool_router_batch, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Run several timeline edits as ONE edit — one revision (one undo step), or one staged step while \
                       your edits are held for review — all or nothing: if op K fails nothing is applied and the error \
                       names op K, its tool and why. Each op is {tool, args}: a quick edit tool (the list is in `ops`) \
                       with exactly the arguments it takes on its own, `ripple` and `link` included. Later ops can use \
                       what earlier ones made (\"$0.right.id\"; see `ops`). `dry_run` reports what the edits would \
                       change and keeps nothing. Imports, analysis, captions, SVG titles, voiceover, music, smart_crop, \
                       exports, staging, history and settings are refused: call them on their own. Returns each op's \
                       answer (a list or pair of clips cut down to their places), the timeline afterwards in brief and, for a dry run, \
                       the diff."
    )]
    fn apply_edits(&self, Parameters(p): Parameters<ApplyEditsParams>) -> Result<String, McpError> {
        check(&p.ops)?;
        let dry_run = p.dry_run.unwrap_or(false);
        let owner = self.owner();
        let out = {
            let project = lock_agent_edit(&self.project, owner.as_deref())?;
            run(&project, p)?
        };
        if !dry_run {
            self.changed();
        }
        Ok(out)
    }
}

/// Refuse what can be told without the project: no ops, too many, a tool that is not a batchable
/// edit, an argument the tool does not take, a LUT file.
fn check(ops: &[EditOp]) -> Result<(), McpError> {
    if ops.is_empty() {
        return Err(McpError::invalid_params("`ops` must list at least one edit", None));
    }
    if ops.len() > MAX_OPS {
        return Err(McpError::invalid_params(
            format!("`ops` lists {} edits; one call runs at most {MAX_OPS}", ops.len()),
            None,
        ));
    }
    for (i, op) in ops.iter().enumerate() {
        let fail = |why: String| McpError::invalid_params(format!("op {i} ({}): {why}", op.tool), None);
        if !BATCHABLE.contains(&op.tool.as_str()) {
            return Err(fail(match router().get(&op.tool) {
                Some(_) => "not an edit apply_edits runs (long jobs, files, imports, captions, titles from SVG, \
                            staging, history and settings run on their own); call it by itself"
                    .to_string(),
                None => "no such tool".to_string(),
            }));
        }
        if let Some(why) = router()
            .get(&op.tool)
            .and_then(|tool| args::unknown_argument(tool, Some(&op.args)))
        {
            return Err(fail(why));
        }
        if op.tool == "set_grade" && op.args.get("lut_path").is_some_and(|v| v != "") {
            return Err(fail(
                "a lut_path is a file to read; call set_grade on its own for that (\"\" to remove a LUT is fine)".to_string(),
            ));
        }
    }
    Ok(())
}

struct Failed(McpError);

impl From<kerf_core::Error> for Failed {
    fn from(e: kerf_core::Error) -> Self {
        Failed(core_err(e))
    }
}

/// Run the ops as one batch on the locked project and answer.
fn run(project: &Project, p: ApplyEditsParams) -> Result<String, McpError> {
    let dry_run = p.dry_run.unwrap_or(false);
    let done: Batch<Vec<Value>> = project
        .batch(p.label.as_deref(), dry_run, |project| {
            let mut answers: Vec<Value> = Vec::with_capacity(p.ops.len());
            for (i, EditOp { tool, mut args }) in p.ops.into_iter().enumerate() {
                let fail = |e: McpError| Failed(McpError::new(e.code, format!("op {i} ({tool}): {}", e.message), e.data));
                for value in args.values_mut() {
                    resolve(value, &answers).map_err(|why| fail(McpError::invalid_params(why, None)))?;
                }
                let answer = dispatch(project, &tool, args)
                    .unwrap_or_else(|| Err(McpError::invalid_params("not an edit apply_edits runs", None)))
                    .map_err(fail)?;
                answers.push(serde_json::from_str(&answer).unwrap_or(Value::String(answer)));
            }
            Ok::<_, Failed>(answers)
        })
        .map_err(|Failed(e)| e)?;
    let results: Vec<Value> = done.result.iter().map(compact_answer).collect();
    let mut out = serde_json::json!({
        "applied": !dry_run,
        "ops": results.len(),
        "results": results,
        "timeline": reads::timeline_brief(&done.after),
    });
    if let Some(label) = &done.label {
        out["label"] = label.as_str().into();
    }
    if dry_run {
        out["diff"] = reads::diff_page(&done.diff(), None, None);
    } else {
        out["staged"] = project.staged_timeline().map_err(core_err)?.is_some().into();
    }
    json(&out)
}

/// Replace every `"$K.path"` string in `value` by that value of answer K.
fn resolve(value: &mut Value, answers: &[Value]) -> Result<(), String> {
    match value {
        Value::String(s) => {
            if let Some(literal) = s.strip_prefix("$$") {
                *s = format!("${literal}");
            } else if let Some(found) = reference(s, answers)? {
                *value = found;
            }
        }
        Value::Array(items) => {
            for item in items {
                resolve(item, answers)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                resolve(item, answers)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The value `s` refers to, `None` when it is no reference (`$` then digits, then `.segment`s).
fn reference(s: &str, answers: &[Value]) -> Result<Option<Value>, String> {
    let Some(rest) = s.strip_prefix('$') else { return Ok(None) };
    let mut parts = rest.split('.');
    let Some(index) = parts.next().and_then(|k| k.parse::<usize>().ok()) else {
        return Ok(None);
    };
    let path: Vec<&str> = parts.collect();
    if path.iter().any(|seg| seg.is_empty() || seg.contains(char::is_whitespace)) {
        return Ok(None);
    }
    let Some(mut node) = answers.get(index) else {
        return Err(format!(
            "\"{s}\" refers to op {index}, which has not run yet (an op can only use earlier ops' answers)"
        ));
    };
    for seg in &path {
        node = match node {
            Value::Object(map) => map.get(*seg),
            Value::Array(items) => seg.parse::<usize>().ok().and_then(|k| items.get(k)),
            _ => None,
        }
        .ok_or_else(|| format!("\"{s}\": op {index}'s answer has nothing at `{}`", path.join(".")))?;
    }
    Ok(Some(node.clone()))
}

/// An op's answer as the tool gives it when it is the one clip the op edited, else with every clip in it
/// cut down to its place (`compact`).
fn compact_answer(value: &Value) -> Value {
    if is_clip(value) {
        return value.clone();
    }
    compact(value)
}

fn is_clip(value: &Value) -> bool {
    value.get("asset_id").is_some() && value.get("timeline_start").is_some()
}

/// `value` with every clip in it cut to where it is: its id, asset, place and span.
fn compact(value: &Value) -> Value {
    match value {
        Value::Object(_) if is_clip(value) => match serde_json::from_value::<kerf_core::Clip>(value.clone()) {
            Ok(clip) => {
                let mut out = serde_json::json!({
                    "id": clip.id,
                    "asset_id": clip.asset_id,
                    "timeline_start": clip.timeline_start,
                    "timeline_end": clip.timeline_end(),
                    "source_in": clip.source_in,
                    "source_out": clip.source_out,
                });
                if let Some(link) = clip.link_id {
                    out["link_id"] = serde_json::json!(link);
                }
                out
            }
            Err(_) => value.clone(),
        },
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), compact(v))).collect()),
        Value::Array(items) => Value::Array(items.iter().map(compact).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{check, run, ApplyEditsParams, BATCHABLE};
    use crate::mcp::{lock_agent_edit, router};
    use kerf_core::Project;
    use serde_json::{json, Value};
    use std::sync::Mutex;

    fn params(v: Value) -> ApplyEditsParams {
        serde_json::from_value(v).unwrap()
    }

    /// Check and run as the tool does, as the agent.
    fn apply(project: &Mutex<Project>, v: Value) -> Result<Value, rmcp::ErrorData> {
        let p = params(v);
        check(&p.ops)?;
        let guard = lock_agent_edit(project, Some("s1"))?;
        guard.set_agent_review(true);
        run(&guard, p).map(|s| serde_json::from_str(&s).unwrap())
    }

    fn sample() -> (Mutex<Project>, Vec<String>) {
        let project = Project::sample().unwrap();
        let ids = project.timeline().unwrap().tracks[0]
            .clips
            .iter()
            .map(|c| c.id.to_string())
            .collect();
        (Mutex::new(project), ids)
    }

    fn proposal_steps(project: &Mutex<Project>) -> Option<usize> {
        project.lock().unwrap().staged().unwrap().map(|s| s.edits.len())
    }

    #[test]
    fn every_batchable_tool_is_a_registered_edit_named_in_the_schema() {
        let schema = serde_json::to_string(&router().get("apply_edits").unwrap().input_schema)
            .unwrap()
            .replace("\\n", " ");
        let words: Vec<&str> = schema.split(|c: char| !(c.is_ascii_lowercase() || c == '_')).collect();
        for name in BATCHABLE {
            assert!(words.contains(name), "{name} is not listed under `tool`");
            let tool = router().get(name).unwrap_or_else(|| panic!("{name} is not a tool"));
            let read_only = tool.annotations.as_ref().and_then(|a| a.read_only_hint).unwrap_or(false);
            assert!(!read_only, "{name} is read-only");
        }
    }

    #[test]
    fn ops_that_are_not_quick_edits_or_misspell_an_argument_are_refused_before_the_lock() {
        for (op, said) in [
            (json!({"tool": "export", "args": {}}), "op 0 (export): not an edit"),
            (json!({"tool": "undo"}), "op 0 (undo): not an edit"),
            (json!({"tool": "generate_voiceover"}), "not an edit"),
            (json!({"tool": "frobnicate"}), "no such tool"),
            (
                json!({"tool": "trim", "args": {"clip_id": "x", "rippel": true}}),
                "unknown argument \"rippel\" for trim",
            ),
            (
                json!({"tool": "set_grade", "args": {"clip_id": "x", "lut_path": "/a.cube"}}),
                "lut_path",
            ),
        ] {
            let err = check(&params(json!({ "ops": [op] })).ops).unwrap_err();
            assert!(err.message.contains(said), "{}", err.message);
        }
        assert!(check(&params(json!({"ops": []})).ops).is_err());
        assert!(serde_json::from_value::<ApplyEditsParams>(json!({"ops": [{"tool": "trim", "arg": {}}]})).is_err());
    }

    #[test]
    fn the_ops_stage_as_one_step_and_later_ops_use_what_earlier_ones_made() {
        let (project, ids) = sample();
        let out = apply(
            &project,
            json!({"ops": [
                {"tool": "split_at", "args": {"clip_id": ids[0], "at": 1.0}},
                {"tool": "set_volume", "args": {"clip_id": "$0.right.id", "volume": 0.5}},
                {"tool": "add_marker", "args": {"time": 0.5, "name": "$$5 shot"}},
            ], "label": "Split the open"}),
        )
        .unwrap();
        assert_eq!(out["applied"], true);
        assert_eq!(out["label"], "Split the open");
        assert_eq!(out["staged"], true);
        assert_eq!(out["results"][1]["volume"], 0.5);
        assert!(
            out["results"][0]["right"]["timeline_end"].is_number(),
            "clips are compact: {out}"
        );
        assert!(out["results"][0]["right"].get("transform").is_none());
        assert_eq!(out["results"][2]["name"], "$5 shot");
        assert_eq!(proposal_steps(&project), Some(1));
        let staged = project.lock().unwrap().staged().unwrap().unwrap();
        assert_eq!(staged.edits, ["Split the open"]);
    }

    #[test]
    fn a_failing_op_applies_nothing_and_names_itself() {
        let (project, ids) = sample();
        let before = project.lock().unwrap().timeline().unwrap().duration();
        let err = apply(
            &project,
            json!({"ops": [
                {"tool": "set_volume", "args": {"clip_id": ids[0], "volume": 0.5}},
                {"tool": "trim", "args": {"clip_id": "not-a-uuid", "source_in": 1.0}},
            ]}),
        )
        .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            err.message.starts_with("op 1 (trim): `clip_id` must be a UUID"),
            "{}",
            err.message
        );
        assert_eq!(proposal_steps(&project), None, "nothing staged, not even the proposal");
        assert_eq!(project.lock().unwrap().timeline().unwrap().duration(), before);

        let err = apply(
            &project,
            json!({"ops": [{"tool": "set_volume", "args": {"clip_id": "$1.id", "volume": 0.5}}]}),
        )
        .unwrap_err();
        assert!(err.message.contains("has not run yet"), "{}", err.message);
    }

    #[test]
    fn a_blend_mode_batches_and_an_unknown_one_is_the_callers_to_fix() {
        let (project, ids) = sample();
        let out = apply(
            &project,
            json!({"ops": [{"tool": "set_blend_mode", "args": {"clip_id": ids[0], "mode": "screen"}}]}),
        )
        .unwrap();
        assert_eq!(out["results"][0]["blend_mode"], "screen", "{out}");
        let out = apply(
            &project,
            json!({"ops": [{"tool": "set_blend_mode", "args": {"clip_id": ids[0], "mode": "Hard light"}}]}),
        )
        .unwrap();
        assert_eq!(
            out["results"][0]["blend_mode"], "hard_light",
            "a common spelling reads: {out}"
        );
        let out = apply(
            &project,
            json!({"ops": [{"tool": "set_blend_mode", "args": {"clip_id": ids[0], "mode": "normal"}}]}),
        )
        .unwrap();
        assert!(out["results"][0].get("blend_mode").is_none(), "normal is the default: {out}");
        let err = apply(
            &project,
            json!({"ops": [{"tool": "set_blend_mode", "args": {"clip_id": ids[0], "mode": "soft_light"}}]}),
        )
        .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{}", err.message);
        assert!(
            err.message.starts_with("op 0 (set_blend_mode)") && err.message.contains("multiply"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_speed_ramp_batches_and_a_bad_ramp_is_the_callers_to_fix() {
        let (project, ids) = sample();
        let slow_mo = json!([
            {"time": 0.0, "speed": 1.0},
            {"time": 0.5, "speed": 0.3},
            {"time": 1.5, "speed": 0.3, "easing": "hold"},
            {"time": 2.0, "speed": 1.0},
        ]);
        let out = apply(
            &project,
            json!({"ops": [{"tool": "set_speed_ramp", "args": {"clip_id": ids[0], "keys": slow_mo}}]}),
        )
        .unwrap();
        let clip = &out["results"][0];
        assert_eq!(clip["speed_ramp"].as_array().map(Vec::len), Some(4), "{clip}");
        assert_eq!(clip["speed_ramp"][2]["easing"], "hold");
        assert!(clip["speed_ramp"][0].get("easing").is_none(), "linear is the default: {clip}");

        let out = apply(
            &project,
            json!({"ops": [{"tool": "set_speed_ramp", "args": {"clip_id": ids[0], "keys": []}}]}),
        )
        .unwrap();
        assert!(
            out["results"][0].get("speed_ramp").is_none(),
            "empty keys clear the ramp: {out}"
        );

        for (keys, said) in [
            (
                json!([{"time": 1.0, "speed": 1.0}, {"time": 1.0, "speed": 2.0}]),
                "op 0 (set_speed_ramp)",
            ),
            (json!([{"time": 0.0, "speed": 500.0}]), "op 0 (set_speed_ramp)"),
            (
                json!([{"time": 0.0, "speed": 1.0, "easing": "ease"}]),
                "op 0 (set_speed_ramp)",
            ),
        ] {
            let err = apply(
                &project,
                json!({"ops": [{"tool": "set_speed_ramp", "args": {"clip_id": ids[0], "keys": keys}}]}),
            )
            .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{}", err.message);
            assert!(err.message.starts_with(said), "{}", err.message);
        }
    }

    #[test]
    fn a_dry_run_answers_the_diff_and_keeps_nothing() {
        let (project, ids) = sample();
        let out = apply(
            &project,
            json!({"ops": [{"tool": "remove", "args": {"clip_id": ids[0]}}], "dry_run": true}),
        )
        .unwrap();
        assert_eq!(out["applied"], false);
        assert!(out.get("label").is_none());
        assert!(out["diff"]["entries"]["total"].as_u64().unwrap() >= 1, "{out}");
        assert_eq!(proposal_steps(&project), None);
    }
}
