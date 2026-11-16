use super::{
    caption_source, clip_moves, core_err, elide_large_svg, export_fps_of, export_message, fmt_ts, image_result, log_tool_error,
    png_result, refuse_overwrite, reject_unknown_arguments, require_window, router, server_identity, sheet_grid, svg_moments,
    svg_preview, svg_sheet_caption, svg_sheet_times, svg_source, timeline_sheet_caption, track_gaps, variant_output,
    variants_message, with_frame, AtomicI64, ClipMoveParams, EditAssetTagsParams, KerfMcp, Ordering, Parameters, Shell,
    SkimTimelineParams, SvgMoments, SvgOverlaySummary, SvgPreviewTitle, SvgValidation, TagArg,
};

/// The resources read what the polling tools answer: the project's brief, the queue's first
/// page, the jobs, one job; a job the registry does not know is not found.
#[test]
fn a_resource_reads_what_its_polling_tool_answers() {
    use super::{activity, jobs, resource_text};
    let project = std::sync::Mutex::new(kerf_core::Project::sample().unwrap());
    let read = |uri: &str| -> serde_json::Value { serde_json::from_str(&resource_text(&project, None, uri).unwrap()).unwrap() };
    let brief = read(activity::PROJECT_URI);
    let generation = project.lock().unwrap().generation();
    assert_eq!(brief["generation"], generation);
    assert!(brief["timeline"].is_object(), "{brief}");
    assert!(brief["proposal"].is_null(), "no proposal staged: {brief}");

    project.lock().unwrap().add_task("tighten the intro").unwrap();
    let tasks = read(activity::TASKS_URI);
    assert!(tasks["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["prompt"] == "tighten the intro"));
    assert!(tasks["total"].as_u64().unwrap() >= 1);

    let (id, _run) = jobs::jobs().begin("export").unwrap();
    let job = read(&format!("{}{id}", activity::JOB_URI_PREFIX));
    assert_eq!(job["job_id"], id.as_str());
    assert!(read(activity::JOBS_URI)
        .as_array()
        .unwrap()
        .iter()
        .any(|j| j["job_id"] == id.as_str()));
    jobs::jobs().finish(&id, Ok("{}".to_string()));

    for missing in ["kerf://jobs/job-00000000", "kerf://jobs/nope", "kerf://timeline"] {
        let err = resource_text(&project, None, missing).unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND, "{missing}");
    }
}

#[test]
fn set_grade_refuses_a_misspelt_key_inside_the_curves_or_wheels() {
    let parse = |v: serde_json::Value| serde_json::from_value::<super::GradeParams>(v);
    let ok = parse(serde_json::json!({
        "clip_id": "c",
        "curves": {"master": [{"x": 0, "y": 1}, {"x": 1, "y": 0}]},
        "wheels": {"shadows": {"x": 0.2}}
    }))
    .unwrap();
    let curves = kerf_core::Curves::from(ok.curves.unwrap());
    assert_eq!(curves.master.len(), 2);
    assert_eq!(kerf_core::Wheels::from(ok.wheels.unwrap()).shadows.x, 0.2);
    for bad in [
        serde_json::json!({"clip_id": "c", "curves": {"mastre": []}}),
        serde_json::json!({"clip_id": "c", "curves": {"master": [{"x": 0, "y": 1, "z": 2}]}}),
        serde_json::json!({"clip_id": "c", "wheels": {"midtone": {}}}),
        serde_json::json!({"clip_id": "c", "wheels": {"shadows": {"lightnes": 0.1}}}),
    ] {
        assert!(parse(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn set_property_tracks_refuses_a_track_without_its_keys() {
    let parse = |v: serde_json::Value| serde_json::from_value::<super::PropertyTracksParams>(v);
    let ok = parse(serde_json::json!({
        "clip_id": "c",
        "tracks": [{"prop": "opacity", "keys": []}, {"prop": "volume", "keys": [{"time": 0, "value": 1}]}]
    }))
    .unwrap();
    let tracks: Vec<kerf_core::PropertyTrack> = ok.tracks.into_iter().map(Into::into).collect();
    assert_eq!(tracks[1].keys.len(), 1);
    for bad in [
        serde_json::json!({"clip_id": "c", "tracks": [{"prop": "opacity"}]}),
        serde_json::json!({"clip_id": "c", "tracks": [{"prop": "opacity", "key": [{"time": 0, "value": 1}]}]}),
        serde_json::json!({"clip_id": "c", "tracks": [{"prop": "opacity", "keys": [], "easing": "hold"}]}),
    ] {
        assert!(parse(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn log_tool_error_handles_both_kinds() {
    log_tool_error("t", &rmcp::ErrorData::invalid_params("bad id", None));
    log_tool_error("t", &rmcp::ErrorData::internal_error("boom", None));
}

#[test]
fn refuse_overwrite_blocks_an_existing_file_unless_opted_in() {
    let path = std::env::temp_dir().join(format!("kerf-refuse-overwrite-test-{}.tmp", std::process::id()));
    std::fs::write(&path, b"existing").unwrap();

    let err = refuse_overwrite(&path, false).expect_err("must refuse an existing file by default");
    assert!(err.message.contains("overwrite=true"), "{}", err.message);

    assert!(refuse_overwrite(&path, true).is_ok(), "overwrite=true must let it through");

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn refuse_overwrite_allows_a_path_that_does_not_exist() {
    let path = std::env::temp_dir().join(format!("kerf-refuse-overwrite-missing-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&path);
    assert!(refuse_overwrite(&path, false).is_ok());
}

#[test]
fn fmt_ts_carries_at_minute_boundaries() {
    assert_eq!(fmt_ts(0.0), "00:00.000");
    assert_eq!(fmt_ts(12.5), "00:12.500");
    assert_eq!(fmt_ts(-3.0), "00:00.000");
    // Just under a minute must carry into minutes, not render ":60.000".
    assert_eq!(fmt_ts(59.9999), "01:00.000");
    assert_eq!(fmt_ts(119.9997), "02:00.000");
    assert_eq!(fmt_ts(59.9994), "00:59.999");
    assert_eq!(fmt_ts(125.25), "02:05.250");
}

fn export_progress(
    fraction: f64,
    eta: Option<f64>,
    waiting: Option<&'static str>,
    stage: Option<kerf_core::ExportStage>,
) -> kerf_core::ExportProgress {
    kerf_core::ExportProgress {
        fraction,
        elapsed_secs: 1.0,
        eta_secs: eta,
        waiting,
        stage,
        part: None,
    }
}

/// A job's `message` said "rendering" at progress 0.0 for a queue, a metering pass and the
/// seconds ffmpeg spent opening the footage alike; each says what it is.
#[test]
fn an_export_job_says_what_it_is_doing_not_just_rendering() {
    use kerf_core::ExportStage as S;
    let line = |p: kerf_core::ExportProgress| export_message(&p);
    assert_eq!(
        line(export_progress(0.0, None, Some("waiting for the preview proxy"), None)),
        "waiting for the preview proxy"
    );
    assert_eq!(
        line(export_progress(0.03, None, None, Some(S::Metering))),
        "metering loudness"
    );
    assert_eq!(
        line(export_progress(0.08, None, None, Some(S::Starting))),
        "starting ffmpeg and seeking the footage"
    );
    assert_eq!(line(export_progress(0.5, None, None, Some(S::Rendering))), "rendering");
    assert_eq!(
        line(export_progress(0.5, Some(65.0), None, Some(S::Rendering))),
        "rendering, 01:05.000 to go"
    );
    assert_eq!(
        line(export_progress(0.99, Some(0.0), None, Some(S::Finishing))),
        "finishing the file"
    );
    assert_eq!(
        line(export_progress(0.98, None, None, Some(S::Measuring))),
        "measuring the written file"
    );
    // A report from before stages existed is a render.
    assert_eq!(line(export_progress(0.5, None, None, None)), "rendering");
}

/// An export rendered in parts says which part in place of "rendering", and keeps the part beside
/// what a stage says (the seconds each part spends opening its footage).
#[test]
fn an_export_job_in_parts_says_which_part() {
    use kerf_core::ExportStage as S;
    let part = |index, encoding| kerf_core::ExportPart {
        index,
        total: 5,
        encoding,
    };
    let line = |p: kerf_core::ExportProgress, part| export_message(&kerf_core::ExportProgress { part: Some(part), ..p });
    assert_eq!(
        line(export_progress(0.2, None, None, Some(S::Rendering)), part(2, false)),
        "rendering part 2 of 5"
    );
    assert_eq!(
        line(export_progress(0.2, Some(65.0), None, Some(S::Rendering)), part(2, false)),
        "rendering part 2 of 5, 01:05.000 to go"
    );
    assert_eq!(
        line(export_progress(0.2, None, None, Some(S::Starting)), part(2, false)),
        "starting ffmpeg and seeking the footage (rendering part 2 of 5)"
    );
    assert_eq!(
        line(export_progress(0.7, None, None, Some(S::Rendering)), part(5, true)),
        "encoding"
    );
    // A queue is a queue, whatever it will render in.
    assert_eq!(
        line(
            export_progress(0.0, None, Some("waiting for another job"), None),
            part(1, false)
        ),
        "waiting for another job"
    );
}

#[test]
fn a_multi_format_job_names_the_file_and_the_stage() {
    use kerf_core::ExportStage as S;
    let labels = vec!["9:16".to_string(), "1:1".to_string()];
    let at = |variant, together, waiting, stage, eta| kerf_core::VariantProgress {
        variant,
        total: 2,
        fraction: 0.5,
        elapsed_secs: 1.0,
        eta_secs: eta,
        waiting,
        stage,
        part: None,
        together,
    };
    assert_eq!(
        variants_message(&at(0, false, None, Some(S::Rendering), None), &labels),
        "rendering 1 of 2 (9:16)"
    );
    assert_eq!(
        variants_message(&at(1, false, None, Some(S::Rendering), Some(12.0)), &labels),
        "rendering 2 of 2 (1:1), 00:12.000 to go"
    );
    assert_eq!(
        variants_message(&at(0, true, None, Some(S::Rendering), None), &labels),
        "rendering 2 together (9:16, 1:1)"
    );
    assert_eq!(
        variants_message(&at(0, true, None, Some(S::Metering), None), &labels),
        "metering loudness"
    );
    assert_eq!(
        variants_message(&at(0, false, Some("waiting for another job to finish"), None, None), &labels),
        "waiting for another job to finish"
    );
}

#[test]
fn a_caption_import_takes_exactly_one_of_a_path_or_text() {
    let invalid = |r: Result<String, rmcp::ErrorData>| {
        let err = r.expect_err("must be refused");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{}", err.message);
        err.message.to_string()
    };
    // Inline text passes straight through.
    assert_eq!(caption_source(None, Some("1\n".into())).unwrap(), "1\n");
    // Neither, or both.
    assert!(invalid(caption_source(None, None)).contains("exactly one"));
    assert!(invalid(caption_source(Some("/a/b.srt"), Some("x".into()))).contains("exactly one"));
    // A path is absolute, and the engine's guards still apply to it.
    assert!(invalid(caption_source(Some("subs/movie.srt"), None)).contains("absolute"));
    let dir = std::env::temp_dir().join(format!("kerf-mcp-caption-source-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let srt = dir.join("cues.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
    assert!(caption_source(srt.to_str(), None).unwrap().contains("Hi"));
    let txt = dir.join("cues.txt");
    std::fs::write(&txt, "x").unwrap();
    assert!(invalid(caption_source(txt.to_str(), None)).contains(".srt, .vtt, .ass or .ssa"));
    assert!(invalid(caption_source(dir.join("missing.srt").to_str(), None)).contains("cannot read"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The import tool takes a file *or* text and neither is mandatory: the
/// schema must not claim one is required, or a caller sending the other is
/// rejected before the tool can say which it wanted.
#[test]
fn caption_import_parameters_are_all_optional() {
    let tools = router().list_all();
    let tool = tools
        .iter()
        .find(|t| t.name == "import_captions")
        .expect("import_captions is registered");
    let required = tool.input_schema.get("required").and_then(|r| r.as_array());
    assert!(required.is_none_or(|r| r.is_empty()), "{required:?}");
    let props = tool
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("properties");
    for name in ["path", "text", "format", "base", "asset_id", "style"] {
        assert!(props.contains_key(name), "{name} is missing from the schema");
    }
}

/// The transcript tools are registered and need only the asset: words, speaker, gap and
/// fillers are all optional.
#[test]
fn transcript_tools_take_only_the_asset() {
    let tools = router().list_all();
    for name in ["get_transcript", "speaker_ranges", "find_filler_words", "refine_transcript"] {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"));
        let required: Vec<&str> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(required, ["asset_id"], "{name}");
    }
}

/// `analyze_asset` runs only the steps it is asked for, and an agent that names none gets
/// the user's own choice: the schema keeps `steps` optional, names every kind, and a bad name
/// is refused before anything runs. The proxy tools an agent steers a cache with exist.
#[test]
fn analyze_asset_takes_optional_steps_and_the_proxy_tools_are_registered() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let analyze = tool("analyze_asset");
    let required: Vec<&str> = analyze
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, ["asset_id"], "`steps` must stay optional");
    let schema = serde_json::to_string(&*analyze.input_schema).unwrap();
    for kind in ["silence", "scenes", "loudness", "rhythm", "transcript", "all"] {
        assert!(schema.contains(kind), "`steps` does not name `{kind}`: {schema}");
    }
    for name in ["proxy_status", "rebuild_proxy", "delete_proxy", "analysis_status"] {
        let tool = tool(name);
        assert!(!tool.description.as_deref().unwrap_or_default().is_empty(), "{name}");
    }
    let status_required = tool("proxy_status")
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.len());
    assert!(
        status_required.is_none_or(|n| n == 0),
        "proxy_status takes no required argument"
    );
    assert!(tool("get_asset_metadata")
        .description
        .as_deref()
        .unwrap()
        .contains("analysis_status"));

    // The names an agent sends are parsed the way the tool parses them.
    let parsed = crate::parse_steps(Some(vec!["transcript".into(), "silence".into()]))
        .unwrap()
        .unwrap();
    assert_eq!(
        parsed,
        [kerf_core::AnalysisKind::Silence, kerf_core::AnalysisKind::Transcript]
    );
    assert!(crate::parse_steps(None).unwrap().is_none(), "no steps means the default set");
    assert!(crate::parse_steps(Some(vec!["colour".into()]))
        .unwrap_err()
        .contains("colour"));
    assert!(
        crate::parse_steps(Some(Vec::new())).is_err(),
        "an empty list is a mistake, not a no-op"
    );
}

/// Every tool the agent can call must reach it with a description and an
/// object input schema. The router is built by the `tool_router` macro, so
/// this is what catches an rmcp upgrade silently changing how the tool
/// surface is generated.
#[test]
fn every_tool_has_a_description_and_object_schema() {
    let tools = router().list_all();
    assert!(tools.len() > 50, "expected the full tool surface, got {}", tools.len());

    for tool in &tools {
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(!description.is_empty(), "tool `{}` has no description", tool.name);
        assert_eq!(
            tool.input_schema.get("type").and_then(|t| t.as_str()),
            Some("object"),
            "tool `{}` input schema is not an object",
            tool.name
        );
    }
}

/// `Option<T>` parameters must stay out of `required` — a great many tools
/// document a field as "omit to …", and a schema generator that started
/// requiring them would break that contract without failing to compile.
#[test]
fn optional_parameters_are_not_required() {
    let tools = router().list_all();
    let add_clip = tools
        .iter()
        .find(|t| t.name == "add_clip_to_timeline")
        .expect("add_clip_to_timeline is registered");

    let required: Vec<&str> = add_clip
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    // Documented as required by the tool.
    assert!(required.contains(&"asset_id"), "asset_id should be required: {required:?}");
    assert!(required.contains(&"source_in"), "source_in should be required: {required:?}");
    // Documented as "omit to auto-select" / "omit to append".
    assert!(!required.contains(&"track_id"), "track_id must stay optional: {required:?}");
    assert!(
        !required.contains(&"timeline_start"),
        "timeline_start must stay optional: {required:?}"
    );

    let properties = add_clip
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("input schema has properties");
    for field in ["asset_id", "track_id", "source_in", "source_out", "timeline_start"] {
        assert!(properties.contains_key(field), "`{field}` missing from schema");
    }
}

/// The edit modes are documented in the units a model can act on: deltas in
/// seconds, and the half to remove a closed set — a typo in `side` is the
/// caller's to fix at the schema, not a silent default.
#[test]
fn the_edit_mode_tools_take_a_delta_and_a_closed_side() {
    let tools = router().list_all();
    let required = |name: &str| {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"));
        let mut required: Vec<String> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        required.sort();
        (required, serde_json::to_string(&*tool.input_schema).unwrap())
    };
    assert_eq!(required("roll_edit").0, ["clip_a", "clip_b", "delta"]);
    assert_eq!(required("slip_clip").0, ["clip_id", "delta"]);
    assert_eq!(required("slide_clip").0, ["clip_id", "delta"]);
    assert_eq!(required("split_remove_clips").0, ["cuts", "side"]);
    let (split, schema) = required("split_remove");
    assert_eq!(split, ["at", "clip_id", "side"]);
    assert!(schema.contains("left") && schema.contains("right"), "{schema}");

    let ok = |side: &str| {
        serde_json::from_value::<super::SplitRemoveParams>(serde_json::json!({ "clip_id": "x", "at": 1.0, "side": side }))
    };
    assert!(ok("left").is_ok() && ok("right").is_ok());
    assert!(ok("middle").is_err());
}

/// A detached picture carries no sound, so the property edits an agent would try on it
/// (volume, fade, disabling) change nothing audible: the tools say so, and where its
/// sound is instead.
#[test]
fn the_property_tools_say_a_detached_picture_carries_no_sound() {
    let tools = router().list_all();
    for name in ["set_volume", "set_fade", "set_clip_enabled"] {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"));
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.contains("source_audio: false") && description.contains("linked audio clip"),
            "`{name}` should warn about a detached picture: {description}"
        );
    }
}

/// A keyed number ignores its static value, and `set_keyframes` clears the whole-transform
/// keys only: the tools an agent would reach for say so, rather than the agent finding out by
/// a clip that is still moving after "clearing" it.
#[test]
fn the_static_setters_and_set_keyframes_say_what_an_animated_number_does_to_them() {
    let tools = router().list_all();
    let text = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
            .description
            .as_deref()
            .unwrap_or_default()
            .to_string()
    };
    for name in ["set_volume", "set_color", "set_transform"] {
        let d = text(name);
        assert!(
            d.contains("set_property_keyframes") && (d.contains("not used") || d.contains("not from this static value")),
            "`{name}` should say its static value is ignored while the number is animated: {d}"
        );
    }
    let d = text("set_keyframes");
    assert!(
        d.contains("only") && d.contains("keeps them") && d.contains("clear_keyframes"),
        "`set_keyframes` clears the bundle only: {d}"
    );
    let d = text("clear_keyframes");
    assert!(d.contains("of their own") && d.contains("Colour and volume keys stay"), "{d}");
}

/// The per-call `ripple` override belongs on the edits that follow the
/// project's ripple mode and on none that decide their own layout — an
/// override on `move_clip` or `ripple_delete` would promise something the
/// core never does. Optional everywhere: omitted means "follow the mode".
#[test]
fn ripple_is_an_optional_argument_on_exactly_the_edits_that_follow_the_mode() {
    let tools = router().list_all();
    let schema = |name: &str| {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"));
        let properties = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default();
        let required: Vec<String> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        (properties, required)
    };

    for name in [
        "trim",
        "set_speed",
        "set_speed_ramp",
        "remove",
        "remove_clips",
        "add_clip_to_timeline",
        "split_at",
        "split_remove",
        "split_remove_clips",
        "generate_voiceover",
        "compose_music",
        "add_sound_effects",
        "update_sound_effect",
    ] {
        let (properties, required) = schema(name);
        assert!(properties.contains_key("ripple"), "`{name}` should take `ripple`");
        assert!(!required.iter().any(|r| r == "ripple"), "`{name}`: ripple must stay optional");
    }
    for name in [
        "ripple_delete",
        "cut_clip_range",
        "snap_to_beats",
        "move_clip",
        "move_clips",
        "roll_edit",
        "slip_clip",
        "slide_clip",
        "reorder",
        "duplicate_clips",
    ] {
        let (properties, _) = schema(name);
        assert!(
            !properties.contains_key("ripple"),
            "`{name}` never ripples, so takes no `ripple`"
        );
    }

    // The mode itself, and the group edits, are on the surface.
    assert!(schema("get_ripple_mode").0.is_empty());
    assert_eq!(schema("set_ripple_mode").1, ["on"]);
    assert_eq!(schema("move_clips").1, ["moves"]);
    assert_eq!(schema("remove_clips").1, ["clip_ids"]);
}

/// The per-call `link` override belongs on the edits that carry linked clips
/// along and on none that do not — an override on `add_clip_to_timeline` or
/// `set_volume` would promise a propagation the core never does. Optional
/// everywhere: omitted means "edit the group".
#[test]
fn link_is_an_optional_argument_on_exactly_the_edits_that_carry_linked_clips() {
    let tools = router().list_all();
    let schema = |name: &str| {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"));
        let properties = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default();
        let required: Vec<String> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        (properties, required)
    };
    for name in [
        "move_clip",
        "move_clips",
        "trim",
        "split_at",
        "remove",
        "remove_clips",
        "ripple_delete",
        "cut_clip_range",
        "set_speed",
        "set_speed_ramp",
        "roll_edit",
        "slip_clip",
        "slide_clip",
        "split_remove",
        "split_remove_clips",
    ] {
        let (properties, required) = schema(name);
        assert!(properties.contains_key("link"), "`{name}` should take `link`");
        assert!(!required.iter().any(|r| r == "link"), "`{name}`: link must stay optional");
    }
    for name in [
        "add_clip_to_timeline",
        "cut_clip",
        "reorder",
        "set_volume",
        "set_fade",
        "snap_to_beats",
        "duplicate_clips",
        "extract_audio",
        "add_asset_audio",
        "detach_audio",
        "detach_audio_clips",
        "reattach_audio",
        "reattach_audio_clips",
        "link_clips",
        "unlink_clips",
    ] {
        let (properties, _) = schema(name);
        assert!(
            !properties.contains_key("link"),
            "`{name}` does not follow links, so takes no `link`"
        );
    }
    assert_eq!(schema("detach_audio").1, ["clip_id"]);
    assert_eq!(schema("detach_audio_clips").1, ["clip_ids"]);
    assert_eq!(schema("add_asset_audio").1, ["asset_id"]);
    assert_eq!(schema("reattach_audio").1, ["clip_id"]);
    assert_eq!(schema("reattach_audio_clips").1, ["clip_ids"]);
    assert_eq!(schema("link_clips").1, ["clip_ids"]);
    assert_eq!(schema("unlink_clips").1, ["clip_ids"]);
}

/// A group move reaches the core with every id parsed; one mistyped uuid
/// refuses the lot and is the caller's to fix.
#[test]
fn a_group_move_parses_its_ids_and_names_the_bad_one() {
    use rmcp::model::ErrorCode;
    let (clip, track) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let item = |clip_id: String, track_id: Option<String>| ClipMoveParams {
        clip_id,
        timeline_start: 2.5,
        track_id,
    };

    let moves = clip_moves(&[item(clip.to_string(), None), item(clip.to_string(), Some(track.to_string()))]).unwrap();
    assert_eq!(moves.len(), 2);
    assert_eq!(
        (moves[0].clip_id, moves[0].timeline_start, moves[0].track_id),
        (clip, 2.5, None)
    );
    assert_eq!(moves[1].track_id, Some(track));

    let e = clip_moves(&[item(clip.to_string(), None), item("not-a-uuid".to_string(), None)]).unwrap_err();
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    assert!(e.message.contains("not-a-uuid"), "{}", e.message);
    let e = clip_moves(&[item(clip.to_string(), Some("nope".to_string()))]).unwrap_err();
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
}

/// The group cut reaches the core with every id parsed; one mistyped uuid refuses
/// the lot and is the caller's to fix.
#[test]
fn a_group_cut_parses_its_ids_and_names_the_bad_one() {
    use super::{clip_cuts, ClipCutParams};
    use rmcp::model::ErrorCode;
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let item = |clip_id: String, at: f64| ClipCutParams { clip_id, at };
    let cuts = clip_cuts(&[item(a.to_string(), 2.5), item(b.to_string(), 3.0)]).unwrap();
    assert_eq!(
        cuts.iter().map(|c| (c.clip_id, c.at)).collect::<Vec<_>>(),
        vec![(a, 2.5), (b, 3.0)]
    );
    let e = clip_cuts(&[item(a.to_string(), 1.0), item("not-a-uuid".to_string(), 1.0)]).unwrap_err();
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    assert!(e.message.contains("not-a-uuid"), "{}", e.message);
}

/// Clients list a server by its `serverInfo`, and rmcp's default fills that
/// in from its own crate identity — so the app has to name itself.
#[test]
fn the_server_introduces_itself_as_kerf() {
    let info = server_identity();
    assert_eq!(info.name, "kerf");
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    assert_ne!(info.name, "rmcp", "server_info must not be rmcp's own identity");
}

/// The visual tools hand the model a caption plus a raw-base64 image block.
/// This asserts the shape that actually goes over the wire — rmcp wants
/// bare base64 under `data` with a sibling `mimeType`, never a `data:` URL,
/// and getting that wrong degrades silently into an image the model cannot
/// see rather than into a build failure.
#[test]
fn a_sheet_of_the_cut_indexes_each_cell_with_its_time_and_the_clips_on_screen() {
    use kerf_core::{SheetCell, SheetClip};
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let clip = |clip_id, name: &str| SheetClip {
        clip_id,
        name: name.to_string(),
    };
    let cells = [
        SheetCell {
            time: 0.0,
            clips: vec![clip(a, "intro.mp4")],
        },
        SheetCell {
            time: 2.5,
            clips: vec![clip(a, "intro.mp4"), clip(b, "logo.png")],
        },
        SheetCell {
            time: 5.0,
            clips: Vec::new(),
        },
    ];
    let caption = timeline_sheet_caption(3, 1, (0.0, 7.5), &cells);
    assert!(
        caption.starts_with(&format!(
            "contact sheet 3x1 (row-major) of the cut {} - {}",
            fmt_ts(0.0),
            fmt_ts(7.5)
        )),
        "{caption}"
    );
    for line in [
        format!("  cell 1: {}  clip {a} \"intro.mp4\"", fmt_ts(0.0)),
        format!("  cell 2: {}  clip {a} \"intro.mp4\", clip {b} \"logo.png\"", fmt_ts(2.5)),
        format!("  cell 3: {}  no clip (gap)", fmt_ts(5.0)),
    ] {
        assert!(caption.lines().any(|l| l == line), "{line}\n{caption}");
    }
}

#[test]
fn a_sheet_grid_is_clamped_like_skim_assets() {
    assert_eq!(sheet_grid(None, None, None), (4, 4, 240));
    assert_eq!(sheet_grid(Some(0), Some(99), Some(10_000)), (1, 8, 640));
    assert_eq!(sheet_grid(Some(3), Some(2), Some(1)), (3, 2, 80));
    let p: SkimTimelineParams = serde_json::from_value(serde_json::json!({})).expect("every field is optional");
    assert!(p.start.is_none() && p.cell.is_none() && p.region.is_none());
}

#[test]
fn image_results_serialize_as_caption_plus_bare_base64() {
    let jpeg = [0xFF, 0xD8, 0xFF, 0xD9];
    let value = serde_json::to_value(image_result("at 00:02.000".to_string(), jpeg.to_vec())).expect("CallToolResult serializes");

    let content = value["content"].as_array().expect("content is an array");
    assert_eq!(content.len(), 2, "expected caption + image: {content:?}");

    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "at 00:02.000");

    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/jpeg");
    let data = content[1]["data"].as_str().expect("image data is a string");
    assert_eq!(data, "/9j/2Q==");
    assert!(!data.starts_with("data:"), "must be bare base64, not a data: URL");

    assert_eq!(value["isError"], false);
}

/// `timeline_summary` promises the agent "any per-track gaps", and a gap is
/// a real defect in a cut — the picture goes black there. The head of the
/// track counts: a cut that opens two seconds late opens on black.
#[test]
fn track_gaps_finds_holes_including_the_one_at_the_head() {
    use kerf_core::{Clip, StreamKind, Track};
    let asset = uuid::Uuid::new_v4();
    let mut track = Track::new(StreamKind::Video, "V1");
    // Deliberately out of start order — clips are not stored sorted.
    track.clips.push(Clip::new(asset, 0.0, 1.0, 6.0));
    track.clips.push(Clip::new(asset, 0.0, 2.0, 2.0));

    let gaps = track_gaps(&track);
    assert_eq!(gaps.len(), 2, "head gap + the hole between the clips");
    assert_eq!((gaps[0].start_secs, gaps[0].end_secs), (0.0, 2.0));
    assert_eq!((gaps[1].start_secs, gaps[1].end_secs), (4.0, 6.0));
}

/// A gapless track reports nothing, and float noise from a retrim is not a
/// gap — otherwise every summary would be a wall of sub-millisecond holes.
#[test]
fn track_gaps_ignores_a_gapless_track_and_float_noise() {
    use kerf_core::{Clip, StreamKind, Track};
    let asset = uuid::Uuid::new_v4();
    let mut track = Track::new(StreamKind::Video, "V1");
    track.clips.push(Clip::new(asset, 0.0, 2.0, 0.0));
    track.clips.push(Clip::new(asset, 0.0, 2.0, 2.000_04));
    assert!(
        track_gaps(&track).is_empty(),
        "40 microseconds of drift is not a gap, got {:?}",
        track_gaps(&track).len()
    );

    // An overlap must not be read as a gap by the clip that follows it.
    let mut overlapping = Track::new(StreamKind::Video, "V1");
    overlapping.clips.push(Clip::new(asset, 0.0, 5.0, 0.0));
    overlapping.clips.push(Clip::new(asset, 0.0, 1.0, 1.0));
    assert!(track_gaps(&overlapping).is_empty());
}

/// A stale id or an out-of-range value is the caller's mistake, and the
/// model can only act on that if it is told so: `invalid_params` means "fix
/// the arguments", `internal_error` means "the server is broken".
#[test]
fn caller_mistakes_are_reported_as_invalid_params() {
    use rmcp::model::ErrorCode;
    let id = uuid::Uuid::new_v4();
    for e in [
        kerf_core::Error::ClipNotFound(id),
        kerf_core::Error::AssetNotFound(id),
        kerf_core::Error::TrackNotFound(id),
        kerf_core::Error::SvgOverlayNotFound(id),
        kerf_core::Error::InvalidArgument("marker time must be >= 0".to_string()),
        kerf_core::Error::StagedEditStale,
    ] {
        let rendered = e.to_string();
        assert_eq!(core_err(e).code, ErrorCode::INVALID_PARAMS, "{rendered}");
    }
    // Ours, not theirs.
    assert_eq!(
        core_err(kerf_core::Error::Engine("ffmpeg exited 1".to_string())).code,
        ErrorCode::INTERNAL_ERROR
    );
}

/// The router is fixed at compile time but the `#[tool_handler]` default
/// rebuilds it per request. This pins that it is built once — a regression
/// here is invisible except as latency on every single tool call.
#[test]
fn the_tool_router_is_built_once() {
    assert!(std::ptr::eq(router(), router()));
}

/// `export` takes a `RequestContext` alongside its `Parameters` so it can
/// report progress and honour cancellation. That second argument must stay
/// out of the *input schema* — a context extractor leaking in would ask the
/// model to invent a request context, and the tool would be uncallable.
#[test]
fn the_context_extractor_stays_out_of_the_export_schema() {
    let tools = router().list_all();
    let export = tools.iter().find(|t| t.name == "export").expect("export is registered");

    let properties = export
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("input schema has properties");
    assert!(properties.contains_key("output_path"), "{properties:?}");
    assert!(properties.contains_key("options"), "{properties:?}");
    assert!(properties.contains_key("overwrite"), "{properties:?}");
    assert!(properties.contains_key("background"), "{properties:?}");
    assert_eq!(properties.len(), 4, "only the declared params belong here: {properties:?}");

    let required: Vec<&str> = export
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, ["output_path"], "options and overwrite are optional");
}

/// The mixer's three tools: the master fader and limiter are staged edits like
/// any other, and `get_levels` is a read that takes a `RequestContext` (so a
/// long measurement can be cancelled) which must stay out of its schema.
#[test]
fn the_master_and_levels_tools_are_on_the_surface() {
    let tools = router().list_all();
    let schema = |name: &str| {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"));
        let mut properties: Vec<String> = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        properties.sort();
        let required: Vec<String> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        (properties, required)
    };
    assert_eq!(
        schema("set_master_volume"),
        (vec!["volume".to_string()], vec!["volume".to_string()])
    );
    assert_eq!(
        schema("set_master_limiter"),
        (
            vec!["ceiling_db".to_string(), "enabled".to_string()],
            vec!["enabled".to_string()]
        )
    );
    assert_eq!(
        schema("set_master_duck"),
        (vec!["depth_db".to_string()], Vec::<String>::new())
    );
    assert_eq!(
        schema("fit_music"),
        (
            vec!["clip_id".to_string(), "fade_out".to_string(), "target".to_string()],
            vec!["clip_id".to_string()]
        )
    );
    assert_eq!(
        schema("plan_music_fit"),
        (vec!["clip_id".to_string(), "target".to_string()], vec!["clip_id".to_string()])
    );
    assert_eq!(
        schema("get_music_structure"),
        (vec!["asset_id".to_string()], vec!["asset_id".to_string()])
    );
    // Both optional, and no request context leaking into the schema.
    assert_eq!(
        schema("get_levels"),
        (vec!["loudnorm".to_string(), "range".to_string()], Vec::<String>::new())
    );
    // The check takes an optional range and frame and nothing else: a bare call still judges the whole cut.
    assert_eq!(
        schema("platform_check"),
        (
            vec!["height".to_string(), "range".to_string(), "width".to_string()],
            Vec::<String>::new()
        )
    );
    assert_eq!(
        schema("review_cut"),
        (vec!["checks".to_string(), "range".to_string()], Vec::<String>::new())
    );
}

/// The lint is part of finishing a cut, beside the platform check, and names every check it takes.
#[test]
fn review_cut_is_a_finishing_step_and_lists_its_checks() {
    let guide = super::EDITING_GUIDE;
    let finish = guide.find("Before you report a cut finished").unwrap();
    let step = &guide[finish..finish + 700];
    assert!(step.contains("platform_check") && step.contains("review_cut"), "{step}");
    assert!(super::INSTRUCTIONS.contains("review_cut"));
    let text = router()
        .list_all()
        .into_iter()
        .find(|t| t.name == "review_cut")
        .and_then(|t| t.description.map(|d| d.to_string()))
        .unwrap_or_default();
    for check in kerf_core::ReviewCheck::ALL {
        assert!(text.contains(check.name()), "{check:?}: {text}");
    }
    assert_eq!(
        kerf_core::ReviewCheck::parse_list(&["flash"])
            .map_err(super::core_err)
            .unwrap_err()
            .code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}

/// The words an agent reads have to name the ceiling the engine actually defaults to
/// (it moved from -1 to -1.5 dBFS once the limiter was measured to overshoot).
#[test]
fn the_limiter_tool_names_the_default_ceiling_it_really_has() {
    let default = format!("{}", kerf_core::MASTER_DEFAULT_CEILING_DB);
    let tools = router().list_all();
    let limiter = tools.iter().find(|t| t.name == "set_master_limiter").expect("registered");
    let text = limiter.description.as_deref().unwrap_or_default();
    assert!(text.contains(&format!("default {default}")), "{text}");
    let param = limiter.input_schema["properties"]["ceiling_db"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(param.contains(&format!("({default} until")), "{param}");
}

/// A Reel written from the middle of a long timeline is judged at the Reel's length: the per-file
/// verdict `export_variants` reports reads the range the export was given, not the whole cut.
#[test]
fn an_exported_variant_is_judged_over_the_span_it_wrote() {
    let project = kerf_core::Project::sample().unwrap();
    let interview = project
        .list_assets()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "interview.mp4")
        .unwrap();
    // 20.5 s of sample cut, then two more full interviews: 260.5 s, past the 3 min Shorts cap.
    project
        .add_clip_to_timeline(interview.id, None, 0.0, 120.0, Some(20.5))
        .unwrap();
    project
        .add_clip_to_timeline(interview.id, None, 0.0, 120.0, Some(140.5))
        .unwrap();
    let reel = kerf_core::ExportVariant::beside(
        std::path::Path::new("/renders/cut.mp4"),
        kerf_core::Delivery::parse("9:16").unwrap(),
    );

    let whole = variant_output(&project, &reel, None).unwrap();
    let issues = whole["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["target"] == "YouTube Shorts" && i["severity"] == "error"),
        "{whole}"
    );

    // Over the 20 s span the three vertical feeds are ready; what remains is the other platforms'
    // shape, never a length.
    let span = variant_output(&project, &reel, Some((10.0, 30.0))).unwrap();
    let ready: Vec<&str> = span["ready_for"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(ready, ["Instagram Reels", "YouTube Shorts", "TikTok"]);
    assert!(
        span["issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| !ready.contains(&i["target"].as_str().unwrap())),
        "{span}"
    );
    assert_eq!(span["format"], "9:16");
}

/// Claude Code shows a server's instructions up to 4096 characters and drops the rest.
#[test]
fn the_server_instructions_fit_a_client_cap_and_route_editing_here() {
    let text = super::INSTRUCTIONS;
    assert!(text.len() <= 3500, "{} characters", text.len());
    for needle in ["instead of running ffmpeg", "get_editing_guide", "STAGED", "complete_task"] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    assert!(router().list_all().iter().any(|t| t.name == "get_editing_guide"));
}

/// A server confined to a workspace says so in its instructions, within the same budget; one that
/// is not says nothing new.
#[test]
fn the_workspace_rule_is_in_the_instructions_only_when_there_is_one() {
    use rmcp::ServerHandler;
    let (server, _) = headless_server();
    assert_eq!(server.get_info().instructions.as_deref(), Some(super::INSTRUCTIONS));
    for (from, _) in super::WORKSPACE_WORDING {
        assert!(super::INSTRUCTIONS.contains(from), "{from}");
    }
    let dir = std::env::temp_dir().join(format!("kerf-ws-info-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let ws = super::Workspace::open(&dir).unwrap();
    let confined = KerfMcp {
        workspace: Some(std::sync::Arc::new(ws)),
        ..server
    };
    let text = confined.get_info().instructions.unwrap();
    assert_eq!(
        text,
        super::instructions_for(confined.workspace.as_deref().map(super::Workspace::root))
    );
    std::fs::remove_dir_all(&dir).ok();

    let root = format!("/srv/kerf/{}", "x".repeat(54));
    assert_eq!(root.len(), 64);
    let text = super::instructions_for(Some(&root));
    assert!(text.len() <= super::INSTRUCTIONS_BUDGET, "{} characters", text.len());
    assert!(
        text.ends_with(&format!(" Workspace: {root}.")),
        "a 64-character root fits: {text}"
    );
    assert!(text.contains("relative to it, even where a tool says absolute"), "{text}");
    assert!(
        !text.contains("absolute path"),
        "nothing says a path must be absolute: {text}"
    );
    // A root too long for the budget is left out, never the budget broken.
    let text = super::instructions_for(Some(&format!("/srv/{}", "y".repeat(250))));
    assert!(
        text.len() <= super::INSTRUCTIONS_BUDGET && !text.contains("Workspace: "),
        "{text}"
    );
}

/// `set_delivery_format` takes a frame rate, and the words an agent reads say what leaving it out
/// does and where exports now get their rate from.
#[test]
fn next_delivery_keeps_the_rate_through_a_reshape_and_sets_it_alone() {
    use kerf_core::{Delivery, Fit};
    let ask = |width, height, fit, fps| super::SetDeliveryFormatParams { width, height, fit, fps };
    let vertical = Delivery::new(1080, 1920, Fit::Cover).with_fps(Some(50.0)).unwrap();
    let next = |p, current| super::next_delivery(&p, current).unwrap();
    // A reshape with no rate keeps the one the project has; a stated one replaces it, 0 clears it.
    let square = next(ask(Some(1080), Some(1080), None, None), Some(vertical)).unwrap();
    assert_eq!((square.width, square.height, square.fps), (1080, 1080, Some(50.0)));
    let square = next(ask(Some(1080), Some(1080), None, Some(25.0)), Some(vertical)).unwrap();
    assert_eq!(square.fps, Some(25.0));
    let square = next(ask(Some(1080), Some(1080), None, Some(0.0)), Some(vertical)).unwrap();
    assert_eq!(square.fps, None);
    // The short NTSC decimals are made exact, as the core does for every caller.
    let ntsc = next(ask(Some(1080), Some(1920), None, Some(29.97)), None).unwrap();
    assert_eq!(ntsc.fps, Some(30_000.0 / 1001.0));
    // A rate alone is the frame's rate, its shape and fit as they were.
    let rated = next(ask(None, None, None, Some(30.0)), Some(vertical)).unwrap();
    assert_eq!(
        (rated.width, rated.height, rated.fit, rated.fps),
        (1080, 1920, Fit::Cover, Some(30.0))
    );
    assert_eq!(next(ask(None, None, None, Some(0.0)), Some(vertical)).unwrap().fps, None);
    // ...which there has to be one of; and nothing at all still clears the frame.
    assert!(super::next_delivery(&ask(None, None, None, Some(30.0)), None).is_err());
    assert_eq!(next(ask(None, None, None, Some(0.0)), None), None);
    assert_eq!(next(ask(None, None, None, None), Some(vertical)), None);
    // Not a rate: refused whichever way it arrives.
    for bad in [-1.0, 0.5, 241.0, f64::NAN] {
        assert!(
            super::next_delivery(&ask(Some(1080), Some(1920), None, Some(bad)), None).is_err(),
            "{bad}"
        );
        assert!(
            super::next_delivery(&ask(None, None, None, Some(bad)), Some(vertical)).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn the_delivery_tool_takes_a_frame_rate_and_says_what_it_decides() {
    let tools = router().list_all();
    let tool = tools.iter().find(|t| t.name == "set_delivery_format").expect("registered");
    let props = tool.input_schema["properties"].as_object().unwrap();
    let mut names: Vec<&str> = props.keys().map(String::as_str).collect();
    names.sort();
    assert_eq!(names, ["fit", "fps", "height", "width"]);
    assert!(tool
        .input_schema
        .get("required")
        .is_none_or(|r| r.as_array().is_some_and(|r| r.is_empty())));
    let text = tool.description.as_deref().unwrap_or_default();
    assert!(text.contains("FIRST clip"), "{text}");
    assert!(text.contains("options.fps"), "{text}");
    let param = props["fps"]["description"].as_str().unwrap_or_default();
    assert!(param.contains("1 to 240") && param.contains("0 clears"), "{param}");

    for name in ["export", "export_variants"] {
        let tool = tools.iter().find(|t| t.name == name).expect("registered");
        let text = tool.description.as_deref().unwrap_or_default();
        assert!(text.contains("set_delivery_format"), "{name}: {text}");
        assert!(text.contains("fps"), "{name}: {text}");
    }
}

#[test]
fn an_export_result_carries_the_frame_it_wrote() {
    let frame = kerf_core::ExportedFrame {
        width: 1080,
        height: 1920,
        fps: 50.0,
        fps_source: kerf_core::FpsSource::Delivery,
    };
    let said = with_frame(serde_json::json!({ "output": "/out/cut.mp4" }), Some(frame));
    assert_eq!(
        said,
        serde_json::json!({
            "output": "/out/cut.mp4", "width": 1080, "height": 1920, "fps": 50.0, "fps_source": "delivery"
        })
    );
    // A render with no picture says nothing of one.
    let audio = serde_json::json!({ "output": "/out/cut.mp3" });
    assert_eq!(with_frame(audio.clone(), None), audio);
}

/// The summary says what an export would write, so a default taken from the footage is no surprise.
#[test]
fn the_summary_names_the_rate_an_export_defaults_to_and_where_it_comes_from() {
    let project = kerf_core::Project::sample().unwrap();
    let assets = project.list_assets().unwrap();
    let timeline = project.timeline().unwrap();
    let footage = export_fps_of(&timeline, &assets).expect("the sample has footage");
    assert_eq!(footage.from, kerf_core::FpsSource::Footage);

    let delivered = project
        .set_delivery_format(Some(
            kerf_core::Delivery::new(1080, 1920, kerf_core::Fit::Cover)
                .with_fps(Some(50.0))
                .unwrap(),
        ))
        .unwrap();
    let said = serde_json::to_value(export_fps_of(&delivered, &assets)).unwrap();
    assert_eq!(said, serde_json::json!({ "fps": 50.0, "from": "delivery" }));

    // Nothing to follow: no clip with a picture and no delivery rate, so nothing is claimed.
    assert!(export_fps_of(&kerf_core::Timeline::default(), &[]).is_none());
}

#[test]
fn the_server_instructions_point_at_the_editor_context() {
    let text = super::INSTRUCTIONS;
    assert!(text.contains("get_editor_context"), "{text}");
    assert!(text.contains("\"this clip\""), "{text}");
}

/// The agent is told what a lock refuses and what it leaves, in the words the refusal uses.
#[test]
fn the_server_instructions_say_what_a_locked_track_refuses() {
    let text = super::INSTRUCTIONS;
    assert!(text.contains("A LOCKED track"), "{text}");
    assert!(text.contains("`track V1 is locked`"), "{text}");
    assert!(text.contains("first unlocked one"), "{text}");
    assert!(text.contains("You may lock a track but not unlock one"), "{text}");
}

fn call(name: &'static str, args: serde_json::Value) -> rmcp::model::CallToolRequestParams {
    rmcp::model::CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone())
}

/// serde drops a key a parameter struct does not name, so a misspelt override
/// would run the edit without it and report success.
#[test]
fn an_unknown_argument_is_refused_naming_the_accepted_ones() {
    use rmcp::model::ErrorCode;
    let e = reject_unknown_arguments(&call("trim", serde_json::json!({"clip_id": "x", "rippel": false})))
        .expect_err("a misspelt ripple must not be dropped silently");
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(e.code.0, -32602);
    for name in ["\"rippel\"", "trim", "ripple", "clip_id", "source_in", "link"] {
        assert!(e.message.contains(name), "{name}: {}", e.message);
    }

    let e = reject_unknown_arguments(&call("get_editor_context", serde_json::json!({"verbose": true}))).unwrap_err();
    assert!(e.message.contains("takes no arguments"), "{}", e.message);

    assert!(reject_unknown_arguments(&call("trim", serde_json::json!({"clip_id": "x", "ripple": false}))).is_ok());
    assert!(reject_unknown_arguments(&call("get_editor_context", serde_json::json!({}))).is_ok());
    assert!(reject_unknown_arguments(&rmcp::model::CallToolRequestParams::new("get_editor_context")).is_ok());
    assert!(
        reject_unknown_arguments(&call("no_such_tool", serde_json::json!({"x": 1}))).is_ok(),
        "an unknown tool is the router's to report"
    );
}

/// The strict-argument check accepts what the schema lists, and schemars does not
/// list a `#[serde(alias)]` name, so an alias on a parameter would be refused.
#[test]
fn mcp_parameters_take_no_serde_alias() {
    // The tools' parameter structs live in this file and in the adapter's modules.
    for (file, source) in [
        ("mcp.rs", include_str!("../mcp.rs")),
        ("mcp/jobs.rs", include_str!("jobs.rs")),
        ("mcp/project_tools.rs", include_str!("project_tools.rs")),
        ("mcp/reads.rs", include_str!("reads.rs")),
        ("mcp/server.rs", include_str!("server.rs")),
        ("mcp/templates.rs", include_str!("templates.rs")),
    ] {
        assert!(
            !source.replace(' ', "").contains(concat!("alias", "=\"")),
            "an MCP parameter in {file} uses #[serde(alias)]"
        );
    }
}

/// Every argument a tool's schema lists is accepted — the check reads the same
/// schema the client was given, so it can only refuse what the client was not offered —
/// and no tool admits a key it does not list.
#[test]
fn every_listed_argument_is_accepted_and_nothing_else() {
    for tool in router().list_all() {
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default();
        let args: serde_json::Map<String, serde_json::Value> =
            props.keys().map(|k| (k.clone(), serde_json::Value::Null)).collect();
        let mut request = rmcp::model::CallToolRequestParams::new(tool.name.clone()).with_arguments(args);
        assert!(reject_unknown_arguments(&request).is_ok(), "{}", tool.name);
        request
            .arguments
            .as_mut()
            .unwrap()
            .insert("not_an_argument".into(), serde_json::Value::Null);
        assert!(reject_unknown_arguments(&request).is_err(), "{} admits any key", tool.name);
    }
}

/// A client decides from these hints what to run unasked and what to confirm,
/// so a tool without them is a tool it has to guess about.
/// Each tool is listed by exactly one tool set (its row in `hints::TOOLS`), each set lists
/// something, and the essentials every selection keeps are tools.
#[test]
fn every_tool_is_in_one_tool_set() {
    let names = tool_names();
    let unset: Vec<&String> = names.iter().filter(|n| super::toolsets::set_of(n).is_none()).collect();
    assert!(
        unset.is_empty(),
        "tools in no tool set (add a row to hints::TOOLS): {unset:?}"
    );
    for set in super::toolsets::ToolSet::ALL {
        let tools: Vec<&str> = set.tools().collect();
        assert!(!tools.is_empty(), "{} lists nothing", set.name());
        for tool in tools {
            assert!(
                names.iter().any(|n| n == tool),
                "{} names {tool}, which is no tool",
                set.name()
            );
        }
    }
    for tool in super::toolsets::ESSENTIALS {
        assert!(names.iter().any(|n| n == tool), "essential {tool} is no tool");
    }
}

#[test]
fn the_server_offers_prompts_tools_and_resources() {
    let caps = super::capabilities();
    assert!(caps.prompts.is_some() && caps.tools.is_some() && caps.resources.is_some());
}

#[test]
fn every_tool_is_titled_and_annotated() {
    let tools = router().list_all();
    for tool in &tools {
        assert!(
            tool.title.as_deref().is_some_and(|t| !t.is_empty()),
            "{} has no title",
            tool.name
        );
        let a = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
        for (hint, value) in [
            ("readOnlyHint", a.read_only_hint),
            ("destructiveHint", a.destructive_hint),
            ("idempotentHint", a.idempotent_hint),
            ("openWorldHint", a.open_world_hint),
        ] {
            assert!(value.is_some(), "{} lacks {hint}", tool.name);
        }
    }
    for (name, ..) in super::hints::TOOLS {
        assert!(tools.iter().any(|t| t.name == *name), "hints name {name}, which is no tool");
    }
    let hint = |name: &str| tools.iter().find(|t| t.name == name).unwrap().annotations.clone().unwrap();
    for read in [
        "get_timeline_state",
        "preview_timeline",
        "skim_timeline",
        "staged_diff",
        "get_editor_context",
        "get_levels",
        "list_templates",
    ] {
        assert_eq!(hint(read).read_only_hint, Some(true), "{read}");
    }
    for staged in [
        "trim",
        "remove_clips",
        "remove_track",
        "set_volume",
        "stage_edits",
        "apply_template",
        "set_template_values",
        "save_template",
    ] {
        let a = hint(staged);
        assert_eq!((a.read_only_hint, a.destructive_hint), (Some(false), Some(false)), "{staged}");
    }
    for destructive in [
        "apply_staged_edits",
        "discard_staged_edits",
        "revert_to",
        "export",
        "export_variants",
        "export_cover",
        "export_srt",
        "export_template",
        "delete_template",
        "delete_proxy",
        "remove_task",
    ] {
        assert_eq!(hint(destructive).destructive_hint, Some(true), "{destructive}");
    }
    for downloads in [
        "download_speech_model",
        "generate_voiceover",
        "compose_music",
        "separate_stems",
    ] {
        assert_eq!(hint(downloads).open_world_hint, Some(true), "{downloads}");
    }
}

#[test]
fn the_editing_guide_send_a_social_cut_through_get_levels() {
    let text = super::EDITING_GUIDE;
    assert!(text.contains("get_levels"), "{text}");
    assert!(text.contains("-14 LUFS") && text.contains("loudnorm"), "{text}");
    assert!(text.contains("set_master_limiter"), "{text}");
}

/// An agent that has never seen the GUI writes the score from the tool alone, so its description
/// and schema have to carry every style, every section kind, the chord syntax (names and roman
/// numerals), the hits and how to align or recompose. The description stays short: a client
/// shows it up front, and the schema only once the tool is loaded.
#[test]
fn compose_music_describes_the_score_and_the_music_tools_are_on_the_surface() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let text = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
    let compose = text("compose_music");
    assert!(
        compose.len() <= 3000,
        "compose_music's description is {} characters",
        compose.len()
    );
    let compose = compose + &serde_json::to_string(&tool("compose_music").input_schema).unwrap();
    for style in kerf_core::MusicStyle::ALL {
        assert!(compose.contains(style.name()), "the tool names the style {}", style.name());
    }
    for needle in [
        "intro",
        "verse",
        "build",
        "drop",
        "breakdown",
        "outro",
        "roman numerals",
        "F#m7",
        "D/F#",
        "bVII",
        "at_s",
        "impact",
        "riser",
        "dropout",
        "whoosh",
        "align",
        "replace_clip_id",
        "get_band_balance",
        "set_master_duck",
        "fit_music",
        "ONE per bar",
        "progressToken",
        "9007199254740991",
        "WHOLE bed",
        "unlink_clips",
        "replaced",
        "half a bar",
        "build",
        "256 hits",
        "misspelled",
    ] {
        assert!(compose.contains(needle), "the tool should mention {needle}");
    }

    let schema = &tool("compose_music").input_schema;
    assert_eq!(schema["required"], serde_json::json!(["score"]));
    for field in ["score", "track_id", "at", "align", "replace_clip_id", "ripple", "background"] {
        assert!(schema["properties"].get(field).is_some(), "compose_music takes {field}");
    }
    let shown = serde_json::to_string(schema).unwrap();
    for needle in [
        "tropical_house",
        "breakdown",
        "chords",
        "energy",
        "max_tempo_change",
        "markers",
    ] {
        assert!(shown.contains(needle), "the schema should carry {needle}");
    }

    let balance = tool("get_band_balance");
    for field in ["asset_id", "clip_id", "range", "sections"] {
        assert!(
            balance.input_schema["properties"].get(field).is_some(),
            "get_band_balance takes {field}"
        );
    }
    assert!(
        balance
            .input_schema
            .get("required")
            .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)),
        "the target is one of three, so none is required by the schema"
    );
    let balance = text("get_band_balance");
    for needle in ["sub", "high_mid", "share_pct", "LUFS", "windows_from", "notes"] {
        assert!(balance.contains(needle), "get_band_balance should mention {needle}");
    }
    assert!(tool("music_status").input_schema["properties"]
        .as_object()
        .is_none_or(|p| p.is_empty()));
}

/// What a model sends for a score parses, and a bad one is the caller's mistake.
#[test]
fn a_compose_music_call_parses_and_a_bad_score_is_the_callers() {
    let p: super::ComposeMusicParams = serde_json::from_value(serde_json::json!({
        "score": {
            "bpm": 110, "key": "Am", "style": "tropical_house",
            "sections": [
                {"kind": "intro", "bars": 4, "chords": ["i", "VI", "III", "VII"]},
                {"kind": "drop", "bars": 8, "chords": ["Am", "F", "C", "G"], "energy": 0.9}
            ],
            "hits": [{"at_s": 8.7, "kind": "impact"}]
        },
        "align": {"to": "cuts", "max_tempo_change": 0.05},
        "at": 2.0
    }))
    .unwrap();
    p.score.validate().unwrap();
    assert_eq!(p.align.unwrap().to, kerf_core::AlignTo::Cuts);

    let mut bad = p.score;
    bad.seed = kerf_core::MAX_SEED + 1;
    let e = super::core_err(bad.validate().unwrap_err());
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(e.message.contains("seed must be 0..=9007199254740991"), "{}", e.message);
    bad.seed = 0;
    bad.sections[1].chords = vec!["Hm".to_string()];
    let e = super::core_err(bad.validate().unwrap_err());
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(e.message.contains("sections[1]") && e.message.contains("Hm"), "{}", e.message);
}

/// A misspelled field is an error, not a default: in the score, in its sections and hits, and in
/// the align block.
#[test]
fn a_misspelled_compose_music_field_is_refused() {
    let call = |score: serde_json::Value, align: serde_json::Value| {
        serde_json::from_value::<super::ComposeMusicParams>(serde_json::json!({ "score": score, "align": align }))
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let score = |section: serde_json::Value, extra: serde_json::Value| {
        let mut s = serde_json::json!({"bpm": 100, "key": "C", "style": "pop", "sections": [section]});
        if let (Some(s), Some(extra)) = (s.as_object_mut(), extra.as_object()) {
            s.extend(extra.clone());
        }
        s
    };
    let section = serde_json::json!({"kind": "drop", "bars": 4, "chords": ["C"]});
    let ok_align = serde_json::json!({"to": "cuts"});
    call(score(section.clone(), serde_json::json!({})), ok_align.clone()).unwrap();
    let typo = serde_json::json!({"kind": "drop", "bars": 4, "chords": ["C"], "enrgy": 0.9});
    assert!(call(score(typo, serde_json::json!({})), ok_align.clone())
        .unwrap_err()
        .contains("unknown field `enrgy`"));
    assert!(call(score(section.clone(), serde_json::json!({"tempo": 90})), ok_align)
        .unwrap_err()
        .contains("unknown field `tempo`"));
    let e = call(
        score(section, serde_json::json!({})),
        serde_json::json!({"to": "cuts", "max_tempo": 0.1}),
    )
    .unwrap_err();
    assert!(e.contains("unknown field `max_tempo`"), "{e}");
}

/// A music request routes compose (aligned to the cuts) → fit → duck → measure.
#[test]
fn the_editing_guide_route_a_music_request_through_compose_music() {
    let text = super::EDITING_GUIDE;
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("the instructions should mention {needle}"))
    };
    let compose = at("compose_music takes a score");
    let fit = text[compose..]
        .find("fit_music")
        .map(|i| i + compose)
        .expect("fit_music after compose_music");
    let duck = text[compose..]
        .find("set_master_duck")
        .map(|i| i + compose)
        .expect("set_master_duck after compose_music");
    let levels = text[compose..]
        .find("get_levels")
        .map(|i| i + compose)
        .expect("get_levels after compose_music");
    let bands = text[compose..]
        .find("get_band_balance")
        .map(|i| i + compose)
        .expect("get_band_balance after compose_music");
    assert!(
        compose < fit && fit < duck && duck < levels && levels < bands,
        "the order of the music flow"
    );
    assert!(
        text.contains("align {to: \"cuts\"}") && text.contains("replace_clip_id"),
        "{text}"
    );
    let long = text
        .split("Long tools (")
        .nth(1)
        .and_then(|t| t.split(')').next())
        .unwrap_or_default();
    assert!(long.contains("compose_music"), "it is a long tool: {long}");
}

// ---- sound effects ---------------------------------------------------------------------

const SFX_TOOLS: [&str; 4] = [
    "list_sound_effects",
    "add_sound_effects",
    "update_sound_effect",
    "suggest_sound_effects",
];

/// An agent that has never seen the GUI picks sounds, times, gains and lanes from the tools alone:
/// the descriptions and schemas carry the kinds, the anchor rule, density, the FX lanes, staging and
/// the limits, and the tools are on the surface in the `audio` set with the hints a client acts on.
#[test]
fn the_sound_effect_tools_say_what_a_model_needs_and_are_on_the_surface() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let text = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
    let both = |name: &str| text(name) + &serde_json::to_string(&tool(name).input_schema).unwrap();
    for name in SFX_TOOLS {
        assert_eq!(super::toolsets::set_of(name).map(|s| s.name()), Some("audio"), "{name}");
        assert!(
            text(name).len() <= 3000,
            "{name}'s description is {} characters",
            text(name).len()
        );
    }

    let list = text("list_sound_effects");
    // The count the text states is the catalogue's.
    let stated = |text: &str, noun: &str| -> usize {
        let at = text
            .find(&format!(" {noun}"))
            .unwrap_or_else(|| panic!("no `{noun}` in {text}"));
        text[..at]
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap_or_else(|_| panic!("no count before `{noun}`"))
    };
    assert_eq!(stated(&list, "kinds"), kerf_core::SfxKind::ALL.len());
    for kind in kerf_core::SfxKind::ALL {
        assert!(list.contains(kind.name()), "list_sound_effects should name {}", kind.name());
    }
    for category in kerf_core::SfxCategory::ALL {
        assert!(list.to_lowercase().contains(category.name()), "{}", category.name());
    }
    for needle in ["hit", "anchor", "level_db", "duration_s", "reverse", "no licence"] {
        assert!(list.contains(needle), "list_sound_effects should mention {needle}");
    }

    let add = both("add_sound_effects");
    for needle in [
        "HIT",
        "`at`",
        "peak",
        "END of a riser",
        "suggest_sound_effects",
        "list_sound_effects",
        "FX 2",
        "new lane",
        "STAGED",
        "ONE revision",
        "256",
        "effects[2]",
        "volume_db",
        "-8",
        "seed",
        "9007199254740991",
        "pitch",
        "intensity",
        "tail",
        "reverse",
        "track_id",
        "ripple",
        "get_levels",
        "update_sound_effect",
        "misspelled",
        "20 ms",
    ] {
        assert!(add.contains(needle), "add_sound_effects should mention {needle}");
    }
    assert_eq!(
        tool("add_sound_effects").input_schema["required"],
        serde_json::json!(["effects"])
    );
    assert!(tool("add_sound_effects").input_schema["properties"].get("ripple").is_some());
    assert!(
        tool("add_sound_effects").input_schema["properties"]
            .get("background")
            .is_none(),
        "rendering takes milliseconds: no background job"
    );

    let update = tool("update_sound_effect");
    let properties = update.input_schema["properties"].as_object().unwrap();
    for field in [
        "clip_id",
        "kind",
        "duration_s",
        "pitch",
        "intensity",
        "tail",
        "reverse",
        "seed",
        "volume_db",
        "ripple",
    ] {
        assert!(properties.contains_key(field), "update_sound_effect takes {field}");
    }
    let required: Vec<&str> = update.input_schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(required.contains(&"clip_id") && required.contains(&"kind"), "{required:?}");
    let update = both("update_sound_effect");
    for needle in [
        "WHOLE spec",
        "HIT",
        "default",
        "not a synthesized sound effect",
        "ripple",
        "STAGED",
    ] {
        assert!(update.contains(needle), "update_sound_effect should mention {needle}");
    }

    let suggest = both("suggest_sound_effects");
    for needle in [
        "subtle",
        "balanced",
        "punchy",
        "placement",
        "exactly an element of add_sound_effects",
        "no track",
        "0.25 s",
        "sfx:<kind>",
        "punch_in",
        "music_drop",
        "-8 dB",
        "adds nothing",
        "range",
        "categories",
        "kinds",
    ] {
        assert!(suggest.contains(needle), "suggest_sound_effects should mention {needle}");
    }
    assert!(tool("suggest_sound_effects")
        .input_schema
        .get("required")
        .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)));
    for field in ["range", "density", "categories", "kinds"] {
        assert!(
            tool("suggest_sound_effects").input_schema["properties"].get(field).is_some(),
            "suggest_sound_effects takes {field}"
        );
    }

    let hints = |name: &str| tool(name).annotations.clone().unwrap();
    for read in ["list_sound_effects", "suggest_sound_effects"] {
        assert_eq!(hints(read).read_only_hint, Some(true), "{read}");
    }
    for edit in ["add_sound_effects", "update_sound_effect"] {
        let a = hints(edit);
        assert_eq!((a.read_only_hint, a.destructive_hint), (Some(false), Some(false)), "{edit}");
    }
    assert_eq!(hints("update_sound_effect").idempotent_hint, Some(true));
    assert_eq!(hints("add_sound_effects").idempotent_hint, Some(false));
}

/// What a model sends for a placement parses, a misspelled field is an error that names it, and
/// the arguments of a reshape are flat like a placement's.
#[test]
fn sound_effect_calls_parse_and_a_misspelled_field_is_refused() {
    let p: super::AddSoundEffectsParams = serde_json::from_value(serde_json::json!({
        "effects": [
            {"kind": "whoosh", "at": 12.5},
            {"kind": "riser", "at": 20, "duration_s": 3, "pitch": -2, "intensity": 0.5, "tail": 0.1,
             "reverse": false, "seed": 9007199254740991u64, "volume_db": -6, "track_id": uuid::Uuid::nil()},
        ],
        "ripple": false,
    }))
    .unwrap();
    assert_eq!((p.effects.len(), p.ripple), (2, Some(false)));
    assert_eq!(p.effects[0].spec, kerf_core::SfxSpec::new(kerf_core::SfxKind::Whoosh));
    assert_eq!(
        (p.effects[1].volume_db, p.effects[1].spec.seed),
        (Some(-6.0), 9007199254740991)
    );

    let typo = |effect: serde_json::Value| {
        serde_json::from_value::<super::AddSoundEffectsParams>(serde_json::json!({ "effects": [effect] }))
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    assert!(typo(serde_json::json!({"kind": "whoosh", "at": 1, "pitc": 2}))
        .unwrap_err()
        .contains("unknown field `pitc`"));
    assert!(typo(serde_json::json!({"kind": "zap", "at": 1})).is_err());
    assert!(typo(serde_json::json!({"kind": "whoosh"})).unwrap_err().contains("at"));

    let id = uuid::Uuid::new_v4().to_string();
    let u: super::UpdateSoundEffectParams = serde_json::from_value(serde_json::json!({
        "clip_id": id, "kind": "swoosh", "pitch": 2, "reverse": true, "seed": 5, "volume_db": -4, "ripple": true,
    }))
    .unwrap();
    assert_eq!(
        (u.spec.kind, u.spec.pitch, u.spec.reverse, u.spec.seed),
        (kerf_core::SfxKind::Swoosh, 2.0, true, 5)
    );
    assert_eq!((u.volume_db, u.ripple, u.spec.intensity), (Some(-4.0), Some(true), 0.7));
    assert!(serde_json::from_value::<super::UpdateSoundEffectParams>(
        serde_json::json!({"clip_id": id, "kind": "swoosh", "pitc": 2})
    )
    .is_err());
    assert!(serde_json::from_value::<super::UpdateSoundEffectParams>(serde_json::json!({"clip_id": id})).is_err());

    // The server's own check, on the schema, names the argument too.
    let e = reject_unknown_arguments(&call(
        "update_sound_effect",
        serde_json::json!({"clip_id": id, "kind": "swoosh", "pitc": 2}),
    ))
    .unwrap_err();
    assert!(e.message.contains("pitc"), "{}", e.message);
    assert!(reject_unknown_arguments(&call(
        "update_sound_effect",
        serde_json::json!({"clip_id": id, "kind": "swoosh", "pitch": 2, "volume_db": -3})
    ))
    .is_ok());
    let e = reject_unknown_arguments(&call(
        "suggest_sound_effects",
        serde_json::json!({"density": "punchy", "dense": 1}),
    ))
    .unwrap_err();
    assert!(e.message.contains("dense"), "{}", e.message);
    let bad_density = serde_json::from_value::<kerf_core::SfxSuggestRequest>(serde_json::json!({"density": "loud"}));
    assert!(bad_density.is_err());
}

/// A caller's mistake in a sound effect is the caller's: `invalid_params`, naming the effect.
#[test]
fn a_bad_sound_effect_is_the_callers_mistake() {
    let (_server, shared) = headless_server();
    let project = shared.lock().unwrap();
    let bad = |effect: serde_json::Value| {
        let p: super::AddSoundEffectsParams = serde_json::from_value(serde_json::json!({ "effects": [effect] })).unwrap();
        core_err(project.sound_effects_inputs(&p.effects).unwrap_err())
    };
    let e = bad(serde_json::json!({"kind": "riser", "at": 4, "pitch": 40}));
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        e.message.contains("effects[0] (riser)") && e.message.contains("pitch"),
        "{}",
        e.message
    );
    let e = bad(serde_json::json!({"kind": "pop", "at": -1}));
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    let e = bad(serde_json::json!({"kind": "pop", "at": 1, "track_id": uuid::Uuid::new_v4()}));
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    let none = project.sound_effects_inputs(&[]).unwrap_err();
    assert_eq!(core_err(none).code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

/// `list_sound_effects` answers the whole catalogue with the hit point in words.
#[test]
fn list_sound_effects_answers_the_catalogue_with_its_hit_points() {
    let (server, _) = headless_server();
    let list = answer(server.list_sound_effects().unwrap());
    let rows = list["effects"].as_array().unwrap();
    assert_eq!(rows.len(), kerf_core::SfxKind::ALL.len());
    for (row, kind) in rows.iter().zip(kerf_core::SfxKind::ALL) {
        assert_eq!(row["kind"], kind.name());
        for field in [
            "label",
            "category",
            "description",
            "default_duration_s",
            "min_duration_s",
            "max_duration_s",
            "anchor",
            "hit",
            "level_db",
        ] {
            assert!(row.get(field).is_some(), "{} lacks {field}", kind.name());
        }
    }
    let whoosh = &rows[0];
    assert_eq!(whoosh["anchor"], serde_json::json!({"type": "peak", "fraction": 0.55}));
    assert!(whoosh["hit"].as_str().unwrap().contains("55 %"), "{whoosh}");
    let riser = rows.iter().find(|r| r["kind"] == "riser").unwrap();
    assert_eq!(riser["anchor"], serde_json::json!({"type": "end"}));
    assert!(riser["hit"].as_str().unwrap().contains("end"), "{riser}");
    let impact = rows.iter().find(|r| r["kind"] == "impact").unwrap();
    assert!(impact["hit"].as_str().unwrap().contains("4 ms"), "{impact}");
    assert_eq!(list["max_pitch_semitones"], kerf_core::MAX_SFX_PITCH);
    assert_eq!(list["defaults"]["intensity"], 0.7);
    assert_eq!(list["defaults"]["tail"], 0.3);
}

/// The suggestions an agent gets are read from its working cut, and their placements go to
/// `add_sound_effects` as they are.
#[tokio::test(flavor = "multi_thread")]
async fn suggestions_come_from_the_cut_and_their_placements_are_what_add_takes() {
    let (server, shared) = headless_server();
    {
        let project = shared.lock().unwrap();
        let a = footage(&project, "a.mp4", 10.0, "2026-01-01T00:00:00Z");
        let b = footage(&project, "b.mp4", 8.0, "2026-01-01T00:00:01Z");
        project.cut_clip(a.id, 0.0, 6.0).unwrap();
        project.cut_clip(b.id, 0.0, 4.0).unwrap();
    }
    let suggested = answer(
        server
            .suggest_sound_effects(Parameters(kerf_core::SfxSuggestRequest::default()))
            .await
            .unwrap(),
    );
    assert_eq!(suggested["density"], "balanced");
    let list = suggested["suggestions"].as_array().unwrap();
    assert_eq!(suggested["count"], list.len());
    let cut = list
        .iter()
        .find(|s| s["source"] == "cut")
        .unwrap_or_else(|| panic!("a cut at 6 s is suggested: {suggested}"));
    assert_eq!(cut["placement"]["at"], 6.0);
    assert!(cut["reason"].as_str().unwrap().contains("a.mp4"), "{cut}");
    assert!(suggested["next"].as_str().unwrap().contains("add_sound_effects"));

    // Every placement is an element of `effects` as it stands, and the plan checks.
    let effects: Vec<serde_json::Value> = list.iter().map(|s| s["placement"].clone()).collect();
    let p: super::AddSoundEffectsParams = serde_json::from_value(serde_json::json!({ "effects": effects })).unwrap();
    let plan = shared.lock().unwrap().sound_effects_inputs(&p.effects).unwrap();
    assert_eq!(plan.effects.len(), list.len());

    // A range narrows it, and a bad one is the caller's mistake.
    let after = answer(
        server
            .suggest_sound_effects(Parameters(kerf_core::SfxSuggestRequest {
                range: Some(kerf_core::TimeRange { start: 7.0, end: 9.0 }),
                ..Default::default()
            }))
            .await
            .unwrap(),
    );
    assert_eq!(after["count"], 0, "{after}");
    let e = server
        .suggest_sound_effects(Parameters(kerf_core::SfxSuggestRequest {
            range: Some(kerf_core::TimeRange { start: 5.0, end: 2.0 }),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

/// The guide routes a sound-effect request suggest → add → levels, and the instructions name it.
#[test]
fn the_guide_and_the_prompt_route_a_sound_effect_request_through_suggest_and_add() {
    let text = super::EDITING_GUIDE;
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("the guide should mention {needle}"))
    };
    let suggest = at("suggest_sound_effects reads it");
    let add = text[suggest..]
        .find("add_sound_effects takes")
        .map(|i| i + suggest)
        .expect("add after suggest");
    let list = text[suggest..]
        .find("list_sound_effects says")
        .map(|i| i + suggest)
        .expect("list");
    let levels = text[suggest..]
        .find("get_levels")
        .map(|i| i + suggest)
        .expect("get_levels after the effects");
    let update = text[suggest..]
        .find("update_sound_effect")
        .map(|i| i + suggest)
        .expect("update");
    assert!(
        suggest < add && add < list && list < levels && levels < update,
        "the order of the flow"
    );
    for needle in ["HIT", "ENDS on it", "FX, FX 2", "staged", "sfx:<kind>", "ONE call", "-8 dB"] {
        assert!(text.contains(needle), "the guide should mention {needle}");
    }
    let said = text
        .split("each of the ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|n| n.parse::<usize>().ok());
    assert_eq!(
        said,
        Some(kerf_core::SfxKind::ALL.len()),
        "the guide's count of sounds is the catalogue's"
    );
    assert!(super::INSTRUCTIONS.contains("sound effects") && super::INSTRUCTIONS.contains("sound_design"));
    assert!(text.contains("sound_design"));
}

/// The add answer is read from what core stored and placed: an effect asked for a few milliseconds before 0
/// is heard that much later and says so, and the same render put on twice is one asset whose id is the
/// stored one, not the fresh copy the second call rendered.
#[test]
fn the_add_answer_reports_the_hit_it_has_and_the_assets_as_stored() {
    let (_server, shared) = headless_server();
    let project = shared.lock().unwrap();
    let stand_in = |spec: &kerf_core::SfxSpec| -> kerf_core::Asset {
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(),
            "path": format!("/fx/{}-{}.wav", spec.kind.name(), spec.seed),
            "name": spec.title(),
            "duration": spec.resolved_duration_s(),
            "streams": [{ "index": 0, "kind": "audio", "codec": "pcm_s16le", "sample_rate": 48000, "channels": 2 }],
            "imported_at": "2026-01-01T00:00:00Z",
            "sound_effect": spec,
        }))
        .unwrap()
    };
    let add = |effects: serde_json::Value| {
        let p: super::AddSoundEffectsParams = serde_json::from_value(serde_json::json!({ "effects": effects })).unwrap();
        let plan = project.sound_effects_inputs(&p.effects).unwrap();
        let assets: Vec<kerf_core::Asset> = plan.effects.iter().map(|e| stand_in(&e.spec)).collect();
        let placed = project.place_sound_effects(&assets, &plan).unwrap();
        let timeline = project.working_timeline().unwrap();
        super::reads::sfx_answer(&plan, &placed, &timeline)
    };

    let first = add(serde_json::json!([
        {"kind": "impact", "at": 0},
        {"kind": "whoosh", "at": 0.2},
        {"kind": "pop", "at": 5}
    ]));
    let at = |answer: &serde_json::Value, i: usize| answer["clips"][i]["at"].as_f64().unwrap();
    assert_eq!(
        at(&first, 0),
        0.004,
        "an impact asked for at 0 starts there and hits 4 ms later"
    );
    assert_eq!(at(&first, 1), 0.2, "a cut head still hits where it was asked");
    assert_eq!(at(&first, 2), 5.0);
    assert_eq!(first["clips"][0]["start"].as_f64(), Some(0.0));

    // The same pop twice, and once more in a second call: one asset, and it is the stored one.
    let again = add(serde_json::json!([{"kind": "pop", "at": 8}, {"kind": "pop", "at": 9}]));
    let stored = project.list_assets().unwrap();
    for answer in [&first, &again] {
        let used: Vec<&str> = answer["clips"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["asset_id"].as_str().unwrap())
            .collect();
        for asset in answer["assets"].as_array().unwrap() {
            let id = asset["id"].as_str().unwrap();
            assert!(used.contains(&id), "{id} is no clip's asset: {answer}");
            assert!(
                stored.iter().any(|a| a.id.to_string() == id),
                "{id} is not in the library: {answer}"
            );
        }
        assert!(used.iter().all(|id| stored.iter().any(|a| a.id.to_string() == *id)));
    }
    assert_eq!(again["assets"].as_array().unwrap().len(), 1, "{again}");
    assert_eq!(
        again["assets"][0]["id"], first["clips"][2]["asset_id"],
        "the pop stored by the first call"
    );
    assert_eq!(again["clips"][0]["asset_id"], again["clips"][1]["asset_id"]);
    assert_eq!(stored.len(), 3);
}

/// Everything wrong with a sound-effect call is found before anything renders: the caller's
/// mistake, naming the effect, with nothing staged or stored and the webview not told.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sound_effect_call_changes_nothing_and_tells_no_one() {
    use rmcp::model::ErrorCode;
    let (server, shared) = headless_server();
    let changed = std::sync::Arc::new(AtomicI64::new(0));
    {
        let changed = changed.clone();
        server.app.listen("project-changed", move |_| {
            changed.fetch_add(1, Ordering::Relaxed);
        });
    }
    let params = |effects: serde_json::Value| {
        serde_json::from_value::<super::AddSoundEffectsParams>(serde_json::json!({ "effects": effects })).unwrap()
    };
    let e = server
        .add_sound_effects_now(
            params(serde_json::json!([{"kind": "whoosh", "at": 3}, {"kind": "riser", "at": -2}])),
            || false,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    assert!(e.message.contains("effects[1] (riser)"), "{}", e.message);
    let e = server
        .add_sound_effects_now(params(serde_json::json!([])), || false)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    let e = server
        .update_sound_effect_now(
            serde_json::from_value(serde_json::json!({"clip_id": uuid::Uuid::new_v4(), "kind": "pop"})).unwrap(),
            || false,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    let e = server
        .update_sound_effect_now(
            serde_json::from_value(serde_json::json!({"clip_id": "nope", "kind": "pop"})).unwrap(),
            || false,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    assert!(e.message.contains("clip_id"), "{}", e.message);
    assert_eq!(changed.load(Ordering::Relaxed), 0);
    let user = crate::lock_user(&shared);
    assert!(user.staged().unwrap().is_none() && user.list_assets().unwrap().is_empty());
    assert!(user.timeline().unwrap().tracks.iter().all(|t| t.clips.is_empty()));
}

/// The real thing, through the tools an agent calls: effects are rendered, probed and staged on
/// the FX lanes with their hits where asked (a riser and the impact it builds to overlap, so they
/// take two lanes), the webview is told once per edit, the live cut is untouched, and a reshape keeps
/// the hit where it was. `cargo test -p kerf-app --no-default-features -- --ignored sound_effects`.
#[ignore = "renders with the real engine and probes with ffprobe"]
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_adds_and_reshapes_sound_effects_under_review_with_the_real_engine() {
    let (server, shared) = headless_server();
    let changed = std::sync::Arc::new(AtomicI64::new(0));
    {
        let changed = changed.clone();
        server.app.listen("project-changed", move |_| {
            changed.fetch_add(1, Ordering::Relaxed);
        });
    }
    {
        let project = shared.lock().unwrap();
        let a = footage(&project, "a.mp4", 10.0, "2026-01-01T00:00:00Z");
        let b = footage(&project, "b.mp4", 8.0, "2026-01-01T00:00:01Z");
        project.cut_clip(a.id, 0.0, 6.0).unwrap();
        project.cut_clip(b.id, 0.0, 6.0).unwrap();
    }
    let suggested = answer(
        server
            .suggest_sound_effects(Parameters(kerf_core::SfxSuggestRequest {
                density: kerf_core::SfxDensity::Punchy,
                ..Default::default()
            }))
            .await
            .unwrap(),
    );
    let effects: Vec<serde_json::Value> = suggested["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["placement"].clone())
        .collect();
    assert!(effects.len() >= 2, "the opening and the cut: {suggested}");
    let mut effects = effects;
    effects.push(serde_json::json!({"kind": "riser", "at": 6.0, "duration_s": 2.0}));
    effects.push(serde_json::json!({"kind": "impact", "at": 6.0, "volume_db": -3}));

    let done = answer(
        server
            .add_sound_effects_now(
                serde_json::from_value(serde_json::json!({ "effects": effects })).unwrap(),
                || false,
            )
            .await
            .unwrap(),
    );
    assert_eq!(changed.load(Ordering::Relaxed), 1, "the webview is told once for the batch");
    let rows = done["clips"].as_array().unwrap();
    assert_eq!(rows.len(), effects.len());
    let riser = rows.iter().find(|r| r["kind"] == "riser").unwrap();
    let impact = rows.iter().rev().find(|r| r["kind"] == "impact").unwrap();
    assert_eq!((riser["at"].as_f64(), impact["at"].as_f64()), (Some(6.0), Some(6.0)));
    assert!(
        (riser["end"].as_f64().unwrap() - 6.0).abs() < 1e-6,
        "a riser ends on its hit: {riser}"
    );
    assert_ne!(riser["track_name"], impact["track_name"], "they overlap: two lanes");
    assert!(rows.iter().all(|r| r["track_name"].as_str().unwrap().starts_with("FX")));
    assert!(done["assets"].as_array().unwrap().len() >= 2);

    {
        let user = crate::lock_user(&shared);
        assert!(
            user.timeline().unwrap().tracks.iter().all(|t| !t.name.starts_with("FX")),
            "staged, not live"
        );
        let proposed = user.staged_timeline().unwrap().expect("the effects are in the proposal");
        assert_eq!(
            proposed
                .tracks
                .iter()
                .filter(|t| t.name.starts_with("FX"))
                .flat_map(|t| &t.clips)
                .count(),
            effects.len()
        );
    }
    let library = shared.lock().unwrap().list_assets().unwrap();
    let rendered = library
        .iter()
        .find(|a| {
            a.sound_effect
                .as_ref()
                .is_some_and(|spec| spec.kind == kerf_core::SfxKind::Riser)
        })
        .expect("the riser is in the library");
    assert_eq!(rendered.name, "SFX: Riser");
    assert_eq!(rendered.primary_kind(), kerf_core::StreamKind::Audio);
    assert!((rendered.duration - 2.0).abs() < 0.01, "{}", rendered.duration);

    // Reshaped: a longer, lower riser still ends on 6 s.
    let reshaped = answer(
        server
            .update_sound_effect_now(
                serde_json::from_value(serde_json::json!({
                    "clip_id": riser["clip_id"], "kind": "riser", "duration_s": 3.0, "pitch": -5, "volume_db": -9,
                }))
                .unwrap(),
                || false,
            )
            .await
            .unwrap(),
    );
    assert_eq!(changed.load(Ordering::Relaxed), 2);
    assert!((reshaped["at"].as_f64().unwrap() - 6.0).abs() < 1e-6, "{reshaped}");
    assert!((reshaped["end"].as_f64().unwrap() - 6.0).abs() < 1e-6 && reshaped["start"].as_f64().unwrap() < 3.1);
    assert_eq!(reshaped["clip_id"], riser["clip_id"]);
    assert_eq!(reshaped["volume_db"].as_f64(), Some(-9.0));
    assert_eq!(reshaped["spec"]["pitch"].as_f64(), Some(-5.0));

    // Not an effect: refused, and nothing more is said to the webview.
    let video_clip = {
        let user = crate::lock_user(&shared);
        user.staged_timeline().unwrap().unwrap().tracks[0].clips[0].id
    };
    let e = server
        .update_sound_effect_now(
            serde_json::from_value(serde_json::json!({"clip_id": video_clip, "kind": "pop"})).unwrap(),
            || false,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(changed.load(Ordering::Relaxed), 2);

    // An impact asked for at 0 starts there and is heard 4 ms later; the same effect added twice is one stored
    // asset, and every id the answer names is the clips' and the library's.
    let twice = answer(
        server
            .add_sound_effects_now(
                serde_json::from_value(serde_json::json!({
                    "effects": [{"kind": "impact", "at": 0, "seed": 41}, {"kind": "impact", "at": 3, "seed": 41}]
                }))
                .unwrap(),
                || false,
            )
            .await
            .unwrap(),
    );
    assert_eq!(twice["clips"][0]["at"].as_f64(), Some(0.004), "{twice}");
    assert_eq!(twice["clips"][1]["at"].as_f64(), Some(3.0), "{twice}");
    assert_eq!(twice["assets"].as_array().unwrap().len(), 1, "{twice}");
    assert_eq!(twice["assets"][0]["id"], twice["clips"][0]["asset_id"]);
    assert_eq!(twice["clips"][0]["asset_id"], twice["clips"][1]["asset_id"]);
    let library = shared.lock().unwrap().list_assets().unwrap();
    assert!(library.iter().any(|a| serde_json::json!(a.id) == twice["assets"][0]["id"]));
    // Again, in another call: the stored asset comes back, not a fresh id.
    let thrice = answer(
        server
            .add_sound_effects_now(
                serde_json::from_value(serde_json::json!({"effects": [{"kind": "impact", "at": 6, "seed": 41}]})).unwrap(),
                || false,
            )
            .await
            .unwrap(),
    );
    assert_eq!(thrice["assets"][0]["id"], twice["assets"][0]["id"], "{thrice}");
}

/// An agent with no way to load media can only rearrange what it was handed.
#[test]
fn media_can_be_imported_over_mcp() {
    let tools = router().list_all();
    let import = tools
        .iter()
        .find(|t| t.name == "import_asset")
        .expect("import_asset is registered");
    let required: Vec<&str> = import
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, ["path"]);
}

/// The window is the whole point of `get_waveform_range`, so all four
/// arguments are required and the schema has to say what they are in terms a
/// model that has never seen the timeline can act on (source seconds, not
/// timeline time).
#[test]
fn the_waveform_range_tool_takes_a_source_window() {
    let tools = router().list_all();
    let tool = tools
        .iter()
        .find(|t| t.name == "get_waveform_range")
        .expect("get_waveform_range is registered");
    let mut required: Vec<&str> = tool
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    required.sort_unstable();
    assert_eq!(required, ["asset_id", "buckets", "end", "start"]);

    let properties = tool
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("input schema has properties");
    for name in ["start", "end"] {
        let described = properties[name]
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_default();
        assert!(described.contains("SOURCE seconds"), "{name}: {described}");
    }
}

/// A swapped or empty window is a mistake the model can fix, not a silent
/// stretch of audio — the engine's answer to one is a row of zeros.
#[test]
fn a_waveform_window_must_run_forwards() {
    use rmcp::model::ErrorCode;
    assert!(require_window(0.0, 1.0).is_ok());
    assert!(require_window(-2.0, 0.5).is_ok(), "a window may begin before the media");
    for (start, end) in [(5.0, 1.0), (3.0, 3.0), (0.0, f64::NAN), (f64::INFINITY, 1.0)] {
        let e = require_window(start, end).expect_err("not a forward window");
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS, "{start}..{end}");
    }
}

#[test]
fn the_editing_guide_describe_the_svg_subset() {
    let text = super::EDITING_GUIDE;
    for needle in [
        "add_svg_overlay",
        "list_svg_presets",
        "SMIL",
        "animateTransform",
        "viewBox",
        "relative to the overlay's start",
        "animateMotion",
        "update_svg_overlay",
        "remove_svg_overlay",
        "validate_svg",
        "preview_svg_overlay",
    ] {
        assert!(text.contains(needle), "the instructions should mention `{needle}`: {text}");
    }
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("`{needle}` in the instructions"));
    assert!(
        at("validate_svg") < at("preview_svg_overlay")
            && at("preview_svg_overlay") < at("then add_svg_overlay, then")
            && at("then add_svg_overlay, then") < at("preview_timeline at a few moments between its start"),
        "the loop runs validate, preview, add, then look at the cut: {text}"
    );
    let tools = router().list_all();
    for name in [
        "add_svg_overlay",
        "update_svg_overlay",
        "remove_svg_overlay",
        "list_svg_presets",
        "validate_svg",
        "preview_svg_overlay",
    ] {
        assert!(tools.iter().any(|t| t.name == name), "`{name}` is registered");
    }
}

#[test]
fn the_editing_guide_send_a_reusable_title_through_a_template() {
    let text = super::EDITING_GUIDE;
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("`{needle}` in the instructions"));
    assert!(
        at("preview_timeline at a few moments between its start") < at("To reuse a title")
            && at("To reuse a title") < at("save_template with `controls`")
            && at("save_template with `controls`") < at("apply_template puts one on the cut")
            && at("apply_template puts one on the cut") < at("set_template_values changes them later")
            && at("set_template_values changes them later") < at("To narrate a cut"),
        "the template paragraph follows the SVG one: author with ids, validate, add, save, apply, set: {text}"
    );
    for needle in [
        "`id` attributes",
        "list_templates",
        "ends the template",
        "delete_template is refused while the user reviews",
    ] {
        assert!(text.contains(needle), "the instructions should mention `{needle}`: {text}");
    }
}

#[test]
fn the_template_tools_ask_for_little_and_say_what_they_take() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let required = |name: &str| -> Vec<String> {
        let mut r: Vec<String> = tool(name)
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        r.sort();
        r
    };
    assert!(required("list_templates").is_empty());
    assert_eq!(required("save_template"), ["name", "overlay_id"]);
    assert_eq!(required("apply_template"), ["start", "template_id"]);
    assert_eq!(required("set_template_values"), ["overlay_id"]);
    assert_eq!(required("set_title_text_box"), ["element", "title_id"]);
    assert_eq!(required("set_title_text_animation"), ["element", "title_id"]);
    assert!(required("list_text_animation_presets").is_empty());
    assert!(required("list_text_animation_curves").is_empty());
    assert_eq!(required("convert_title_text"), ["title_id", "to"]);
    assert_eq!(required("import_template"), ["path"]);
    assert_eq!(required("export_template"), ["output_path", "template_id"]);
    assert_eq!(required("delete_template"), ["template_id"]);
    let description = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
    for (name, needles) in [
        (
            "save_template",
            &[
                "`id` attributes",
                "controls",
                "validate_svg",
                "not part of your staged proposal",
            ][..],
        ),
        (
            "apply_template",
            &["list_templates", "image_files", "staged edit", "set_template_values"][..],
        ),
        (
            "set_template_values",
            &["control", "timeline_summary", "update_svg_overlay"][..],
        ),
        (
            "set_title_text_animation",
            &[
                "preset",
                "data-kerf-animate",
                "convert_title_text",
                "per-glyph",
                "list_text_animation_curves",
            ][..],
        ),
        ("list_text_animation_presets", &["typewriter", "wiggle", "`animation`"][..]),
        (
            "list_text_animation_curves",
            &["spring", "overshoot", "`curves`", "`null`"][..],
        ),
        ("import_template", &["already_present", "added_as_copy", "network share"][..]),
        ("export_template", &[".kerftemplate", "overwrite"][..]),
        ("delete_template", &["cannot be undone", "review"][..]),
    ] {
        for needle in needles {
            assert!(
                description(name).contains(needle),
                "{name} should say `{needle}`: {}",
                description(name)
            );
        }
    }
}

#[test]
fn a_control_in_save_template_refuses_a_key_it_does_not_name() {
    let parse = |controls: serde_json::Value| {
        serde_json::from_value::<super::templates::SaveTemplateParams>(serde_json::json!({
            "overlay_id": "x", "name": "n", "controls": controls
        }))
    };
    let ok = parse(serde_json::json!([
        {"id": "tagline", "label": "Tagline", "kind": "text", "targets": [{"element": "tagline"}]},
        {"id": "size", "label": "Size", "kind": "number", "min": 1, "max": 9,
         "targets": [{"element": "t", "attribute": "font-size"}]}
    ]));
    assert!(ok.is_ok(), "{ok:?}");
    for bad in [
        serde_json::json!([{"id": "a", "label": "A", "kind": "text", "target": [{"element": "t"}], "targets": []}]),
        serde_json::json!([{"id": "a", "label": "A", "kind": "text", "targets": [{"element": "t", "attr": "fill"}]}]),
        serde_json::json!([{"id": "a", "label": "A", "kind": "paragraph", "targets": [{"element": "t"}]}]),
    ] {
        assert!(parse(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn an_agent_reads_a_template_title_with_its_controls_and_values_but_not_its_source() {
    use kerf_core::{SvgOverlay, TemplateControl, TemplateInstance, TemplateKind, TemplateTarget};
    let o = SvgOverlay {
        id: uuid::Uuid::new_v4(),
        name: "End card".to_string(),
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".to_string(),
        start: 0.0,
        end: 4.0,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        opacity: 1.0,
        offset: 0.0,
        preset: None,
        template: Some(TemplateInstance {
            template_id: "0b8c2a52-6d7c-4a52-9d3c-1d3a6f3b6d11".to_string(),
            name: "Kitefun end card".to_string(),
            controls: vec![TemplateControl {
                id: "tagline".to_string(),
                label: "Tagline".to_string(),
                kind: TemplateKind::Text,
                targets: vec![TemplateTarget {
                    element: "tagline".to_string(),
                    attribute: None,
                }],
                min: None,
                max: None,
            }],
            values: [("tagline".to_string(), "See you".to_string())].into(),
        }),
    };
    let v = serde_json::to_value(SvgOverlaySummary::from(&o)).unwrap();
    assert_eq!(v["kind"], "custom");
    assert_eq!(v["template"]["template_id"], "0b8c2a52-6d7c-4a52-9d3c-1d3a6f3b6d11");
    assert_eq!(v["template"]["controls"][0]["id"], "tagline");
    assert_eq!(v["template"]["controls"][0]["kind"], "text");
    assert_eq!(v["template"]["values"]["tagline"], "See you");
    assert!(v["template"]["controls"][0].get("targets").is_none(), "{v}");
    assert!(v.get("svg").is_none(), "{v}");
}

#[test]
fn get_scopes_asks_only_for_a_time() {
    let tools = router().list_all();
    let tool = tools
        .iter()
        .find(|t| t.name == "get_scopes")
        .expect("`get_scopes` is registered");
    let required = tool
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(required, [serde_json::json!("time_secs")]);
    let description = tool.description.as_deref().unwrap_or_default();
    assert!(
        description.contains("clipped_high") && description.contains("hue_deg"),
        "{description}"
    );
    let p: crate::mcp::ScopesParams =
        serde_json::from_value(serde_json::json!({ "time_secs": 1.0, "grid_size": { "vector": 4096 } })).unwrap();
    let size = p.grid_size.unwrap().resolve();
    assert_eq!((size.columns, size.rows), (64, 64));
    assert_eq!(
        size.vector,
        kerf_core::scopes::GridSize { vector: 4096, ..size }.clamped().vector
    );
}

#[test]
fn the_svg_tools_ask_for_little_and_say_what_the_svg_may_do() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let required = |name: &str| -> Vec<String> {
        let mut r: Vec<String> = tool(name)
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        r.sort();
        r
    };
    assert_eq!(required("add_svg_overlay"), ["start"]);
    assert_eq!(required("update_svg_overlay"), ["overlay_id"]);
    assert_eq!(required("remove_svg_overlay"), ["overlay_id"]);
    let description = tool("add_svg_overlay").description.as_deref().unwrap_or_default().to_string();
    for needle in [
        "SMIL",
        "viewBox",
        "RELATIVE TO THE OVERLAY'S START",
        "animateMotion",
        "preview_timeline",
    ] {
        assert!(
            description.contains(needle),
            "add_svg_overlay should say `{needle}`: {description}"
        );
    }
}

#[test]
fn a_title_comes_from_svg_or_from_a_preset_never_both_or_neither() {
    use kerf_core::{SvgPresetKind, SvgPresetParams, SvgSource};
    use rmcp::model::ErrorCode;
    let params = || Some(SvgPresetParams::new("Hello"));
    let kind = Some(SvgPresetKind::FadeUp);
    assert_eq!(
        svg_source(Some("<svg/>".to_string()), None, None).unwrap(),
        SvgSource::Custom("<svg/>".to_string())
    );
    assert!(matches!(
        svg_source(None, kind, params()).unwrap(),
        SvgSource::Preset(p) if p.kind == SvgPresetKind::FadeUp && p.params.text == "Hello"
    ));
    for (svg, kind, params) in [
        (Some("<svg/>".to_string()), kind, params()),
        (Some("<svg/>".to_string()), kind, None),
        (Some("<svg/>".to_string()), None, params()),
        (None, kind, None),
        (None, None, params()),
        (None, None, None),
    ] {
        let e = svg_source(svg, kind, params).expect_err("not one source");
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    }
}

/// The source of a title can be 128 KiB: the agent reads timing and placement, not markup.
#[test]
fn an_agent_reads_a_title_without_its_source() {
    use kerf_core::{SvgOverlay, SvgPreset, SvgPresetKind, SvgPresetParams};
    let mut o = SvgOverlay {
        id: uuid::Uuid::new_v4(),
        name: String::new(),
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".to_string(),
        start: 1.0,
        end: 4.0,
        x: 0.1,
        y: 0.2,
        width: 0.5,
        height: 0.25,
        opacity: 0.8,
        offset: 0.0,
        preset: None,
        template: None,
    };
    let custom = serde_json::to_value(SvgOverlaySummary::from(&o)).unwrap();
    assert_eq!(custom["kind"], "custom");
    assert_eq!(custom["name"], "SVG");
    assert_eq!(custom["svg_bytes"], o.svg.len());
    assert!(custom.get("svg").is_none() && custom.get("params").is_none(), "{custom}");

    o.preset = Some(SvgPreset {
        kind: SvgPresetKind::LowerThird,
        params: SvgPresetParams::new("Ada Lovelace"),
    });
    let preset = serde_json::to_value(SvgOverlaySummary::from(&o)).unwrap();
    assert_eq!(preset["kind"], "lower_third");
    assert_eq!(preset["name"], "Ada Lovelace");
    assert_eq!(preset["params"]["text"], "Ada Lovelace");
    assert!(preset.get("svg").is_none(), "{preset}");
}

fn svg_text(extra: &str) -> String {
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 160 90\"><rect width=\"20\" height=\"20\" fill=\"red\"><animate attributeName=\"x\" from=\"0\" to=\"100\" dur=\"3s\" fill=\"freeze\"/></rect>{extra}</svg>")
}

#[test]
fn png_results_serialize_as_caption_plus_a_png_block() {
    let value = serde_json::to_value(png_result("cells".to_string(), vec![0x89, b'P', b'N', b'G'])).unwrap();
    let content = value["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{content:?}");
    assert_eq!(
        (content[0]["type"].as_str(), content[0]["text"].as_str()),
        (Some("text"), Some("cells"))
    );
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/png");
    assert_eq!(content[1]["data"], "iVBORw==");
}

#[test]
fn a_preview_asks_for_moments_by_times_or_by_count_never_both() {
    use rmcp::model::ErrorCode;
    assert_eq!(svg_moments(None, None).unwrap(), SvgMoments::Spread(6), "six by default");
    assert_eq!(svg_moments(None, Some(0)).unwrap(), SvgMoments::Spread(1));
    assert_eq!(
        svg_moments(None, Some(500)).unwrap(),
        SvgMoments::Spread(36),
        "a count is clamped"
    );
    assert_eq!(
        svg_moments(Some(vec![0.0, 1.5]), None).unwrap(),
        SvgMoments::At(vec![0.0, 1.5])
    );
    for (times, count) in [
        (Some(vec![1.0]), Some(3)),
        (Some(vec![]), None),
        (Some(vec![0.5; 37]), None),
        (Some(vec![f64::NAN]), None),
        (Some(vec![-1.0]), None),
    ] {
        let e = svg_moments(times.clone(), count).expect_err("a mistake the caller can fix");
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS, "{times:?} {count:?}");
    }
}

#[test]
fn a_spread_runs_from_the_first_frame_to_the_end_of_the_title() {
    assert_eq!(svg_sheet_times(&SvgMoments::Spread(5), 4.0), [0.0, 1.0, 2.0, 3.0, 4.0]);
    assert_eq!(svg_sheet_times(&SvgMoments::Spread(2), 3.0), [0.0, 3.0]);
    assert_eq!(
        svg_sheet_times(&SvgMoments::Spread(1), 3.0),
        [1.5],
        "a single cell is the middle"
    );
    assert_eq!(
        svg_sheet_times(&SvgMoments::At(vec![0.5, 9.0]), 3.0),
        [0.5, 9.0],
        "asked-for times are kept"
    );
}

#[test]
fn a_sheet_caption_indexes_the_cells_and_flags_a_slice_and_warnings() {
    let plain = svg_sheet_caption("draft", &[0.0, 1.5], 0.0, &[]);
    assert_eq!(
        plain,
        "draft: 2 moments (row-major), cell -> seconds after it appears:\n  cell 1: 0.00s\n  cell 2: 1.50s"
    );
    let full = svg_sheet_caption("t", &[2.0], 1.0, &["scripts were removed".to_string()]);
    assert!(full.starts_with("t: 1 moment (row-major)"), "{full}");
    assert!(full.contains("1.00s further on"), "{full}");
    assert!(full.ends_with("warnings: scripts were removed"), "{full}");
}

#[test]
fn a_preview_draws_a_png_sheet_and_offsets_a_sliced_title() {
    let title = SvgPreviewTitle {
        svg: svg_text("<script>1</script>"),
        label: "draft".to_string(),
        span: None,
        offset: 0.0,
        report_warnings: true,
    };
    let (png, caption) = svg_preview(&title, &SvgMoments::Spread(3), 96, (160, 90)).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert!(
        caption.contains("cell 3: 3.00s"),
        "a draft runs for its animation's length: {caption}"
    );
    assert!(caption.contains("warnings:") && caption.contains("script"), "{caption}");

    let on_timeline = SvgPreviewTitle {
        span: Some(2.0),
        offset: 1.0,
        report_warnings: false,
        label: "SVG title".to_string(),
        ..title
    };
    let (png, caption) = svg_preview(&on_timeline, &SvgMoments::At(vec![0.0, 2.0]), 96, (160, 90)).unwrap();
    assert_eq!(&png[..4], b"\x89PNG");
    assert!(
        caption.contains("cell 2: 2.00s") && caption.contains("1.00s further on"),
        "{caption}"
    );
    assert!(!caption.contains("warnings"), "{caption}");

    let broken = SvgPreviewTitle {
        svg: "<nope/>".to_string(),
        label: "draft".to_string(),
        span: None,
        offset: 0.0,
        report_warnings: true,
    };
    let e = svg_preview(&broken, &SvgMoments::Spread(1), 96, (160, 90)).expect_err("not an svg");
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn validate_svg_answers_size_warnings_and_duration_without_the_source() {
    let check = kerf_core::check_svg(&svg_text("<script>1</script>")).unwrap();
    let sanitized = check.svg.len();
    let value = serde_json::to_value(SvgValidation::from(check)).unwrap();
    assert_eq!(value["svg_bytes"], sanitized);
    assert_eq!(value["natural_duration"], 3.0);
    assert!(value["warnings"][0].as_str().unwrap().contains("script"), "{value}");
    assert!(value.get("svg").is_none(), "{value}");
    let still = kerf_core::check_svg("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"/>").unwrap();
    assert!(serde_json::to_value(SvgValidation::from(still)).unwrap()["natural_duration"].is_null());
}

#[test]
fn a_timeline_read_leaves_out_large_svg_sources_and_says_how_to_get_them() {
    use kerf_core::{SvgOverlay, Timeline};
    let title = |bytes: usize| SvgOverlay {
        id: uuid::Uuid::new_v4(),
        name: String::new(),
        svg: "x".repeat(bytes),
        start: 0.0,
        end: 1.0,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        opacity: 1.0,
        offset: 0.0,
        preset: None,
        template: None,
    };
    let mut timeline = Timeline::new();
    timeline.svg_overlays = vec![title(4096), title(4097), title(120_000)];
    elide_large_svg(&mut timeline);
    assert_eq!(timeline.svg_overlays[0].svg.len(), 4096, "up to 4 KiB is kept");
    for (o, bytes) in timeline.svg_overlays[1..].iter().zip([4097, 120_000]) {
        assert!(
            o.svg.contains(&format!("{bytes} bytes")) && o.svg.contains("full_svg=true"),
            "{}",
            o.svg
        );
        assert!(o.svg.len() < 200);
    }
}

#[test]
fn the_svg_authoring_tools_are_described_for_a_model_that_has_not_seen_the_gui() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let description = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
    let required = |name: &str| -> Vec<String> {
        tool(name)
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let property = |name: &str, field: &str| tool(name).input_schema["properties"].get(field).cloned();
    assert_eq!(required("validate_svg"), ["svg"]);
    assert!(
        required("preview_svg_overlay").is_empty(),
        "everything is optional but one source"
    );
    assert!(required("get_timeline_state").is_empty());
    for field in ["overlay_id", "svg", "times", "count", "max_width"] {
        assert!(property("preview_svg_overlay", field).is_some(), "{field}");
    }
    assert!(property("update_svg_overlay", "preset_kind").is_some());
    assert!(property("get_timeline_state", "full_svg").is_some());
    // A title on a track: its own tools, and a template put on one.
    let mut needs = required("add_title_clip");
    needs.sort();
    assert_eq!(needs, ["start", "svg"]);
    assert_eq!(required("update_title_clip"), ["clip_id"]);
    assert!(property("apply_template", "on_track").is_some() && property("apply_template", "track_id").is_some());
    assert!(description("add_title_clip").contains("dip_to_white"));
    assert!(description("apply_template").contains("on_track"));
    assert!(description("update_svg_overlay").contains("preset_kind"));
    assert!(
        description("preview_svg_overlay").contains("PNG") && description("preview_svg_overlay").contains("preview_timeline")
    );
    assert!(description("validate_svg").contains("natural_duration"));
    assert!(description("get_timeline_state").contains("full_svg"));

    let presets = description("list_svg_presets");
    assert!(
        presets.contains("bar wipes in above a name"),
        "the lower_third bar sits above the name: {presets}"
    );
    let add = description("add_svg_overlay");
    for needle in ["data:image/png", "data:image/jpeg", "data:image/gif", "data:image/webp"] {
        assert!(add.contains(needle), "only these embedded images are kept: {add}");
    }
    assert!(!add.contains("`data:` images are fine"), "{add}");
}

// ---- review, ownership, jobs, paging: the surface this adapter adds ----------------------

fn tool_names() -> Vec<String> {
    router().list_all().iter().map(|t| t.name.to_string()).collect()
}

/// The numbers and lists the README and the site print are the surface this build serves: a
/// tool added without them fails here, not in a reader's head.
#[test]
fn the_readme_and_the_site_describe_the_tools_the_server_has() {
    const README: &str = include_str!("../../../../README.md");
    const SITE: &str = include_str!("../../../../site/hugo.toml");
    let names = tool_names();
    let count = names.len();
    let heading = README
        .lines()
        .find_map(|line| line.strip_prefix("### The tools (")?.strip_suffix(')'))
        .expect("the README has a `### The tools (N)` heading");
    assert_eq!(heading.parse::<usize>().ok(), Some(count), "README heading: {heading}");
    let shown = SITE
        .lines()
        .find_map(|line| line.trim().strip_prefix("toolCount = "))
        .expect("site/hugo.toml has a toolCount");
    assert_eq!(shown.trim().parse::<usize>().ok(), Some(count), "site toolCount: {shown}");
    let section = README
        .split("### The tools")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the tools section");
    let unlisted: Vec<&String> = names.iter().filter(|name| !section.contains(&format!("`{name}`"))).collect();
    assert!(unlisted.is_empty(), "tools the README's lists leave out: {unlisted:?}");
}

#[test]
fn the_new_tools_are_on_the_surface() {
    let names = tool_names();
    for tool in [
        "project_info",
        "save_project",
        "save_project_as",
        "open_project",
        "new_project",
        "get_settings",
        "set_settings",
        "job_status",
        "cancel_job",
        "list_jobs",
        "get_transcript",
        "get_analysis_detail",
        "preview_svg_preset",
        "undo_staged_edit",
        "restore_discarded_edits",
        "apply_staged_subset",
        "list_assets",
        "get_asset_metadata",
        "missing_assets",
    ] {
        assert!(names.iter().any(|n| n == tool), "`{tool}` is not registered");
    }
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "a tool is registered twice");
}

/// Stabilizing names the clip and nothing else is required; reading where it stands changes nothing.
#[test]
fn stabilize_clip_takes_a_clip_and_optional_numbers_and_its_status_is_a_read() {
    let tools = router().list_all();
    let tool = |name: &str| tools.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("{name}"));
    let stabilize = tool("stabilize_clip");
    assert_eq!(stabilize.input_schema["required"], serde_json::json!(["clip_id"]));
    for arg in ["enabled", "strength", "zoom", "auto_zoom", "background"] {
        assert!(stabilize.input_schema["properties"].get(arg).is_some(), "{arg}");
    }
    let hints = stabilize.annotations.as_ref().unwrap();
    assert_eq!((hints.read_only_hint, hints.destructive_hint), (Some(false), Some(false)));
    let status = tool("stabilization_status");
    assert_eq!(status.annotations.as_ref().unwrap().read_only_hint, Some(true));
    assert!(status
        .input_schema
        .get("required")
        .is_none_or(|r| r.as_array().is_some_and(|r| r.is_empty())));
}

#[test]
fn exactly_the_long_tools_take_background() {
    let tools = router().list_all();
    let mut with: Vec<String> = tools
        .iter()
        .filter(|t| t.input_schema["properties"].get("background").is_some())
        .map(|t| t.name.to_string())
        .collect();
    with.sort();
    let mut long = vec![
        "analyze_asset",
        "compose_music",
        "download_speech_model",
        "export",
        "export_thumbnail",
        "export_variants",
        "generate_voiceover",
        "import_asset",
        "import_url",
        "refine_transcript",
        "separate_stems",
        "smart_crop",
        "stabilize_clip",
        "sync_clips_by_audio",
    ];
    long.sort_unstable();
    assert_eq!(with, long);
    for name in &with {
        let tool = tools.iter().find(|t| t.name == name.as_str()).unwrap();
        let required: Vec<&str> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(!required.contains(&"background"), "`background` is optional on {name}");
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.contains("background") && description.contains("job"),
            "{name} should say a background job is possible: {description}"
        );
    }
    let job = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name}"))
            .input_schema
            .clone()
    };
    for name in ["job_status", "cancel_job"] {
        assert_eq!(job(name)["required"], serde_json::json!(["job_id"]), "{name}");
    }
    assert!(job("list_jobs")["properties"].as_object().is_none_or(|p| p.is_empty()));
}

/// A big library is listed in short lines, and what an import added is asked for by time: both
/// arguments are in the schema (so the strict-argument check lets them through), optional, and
/// explained to a model that has not seen the window; the tool stays read-only.
#[test]
fn list_assets_takes_a_compact_view_and_an_import_time_and_stays_read_only() {
    let tools = router().list_all();
    let tool = tools.iter().find(|t| t.name == "list_assets").unwrap();
    for field in ["offset", "limit", "fields", "imported_after"] {
        assert!(
            tool.input_schema["properties"].get(field).is_some(),
            "list_assets takes {field}"
        );
    }
    assert!(tool
        .input_schema
        .get("required")
        .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)));
    let fields = serde_json::to_string(&tool.input_schema["properties"]["fields"]).unwrap();
    assert!(fields.contains("compact") && fields.contains("full"), "{fields}");
    let text = tool.description.as_deref().unwrap_or_default();
    for needle in ["compact", "imported_after", "newest_imported_at", "next_offset", "20,000"] {
        assert!(text.contains(needle), "the description does not say `{needle}`: {text}");
    }
    assert_eq!(tool.annotations.as_ref().unwrap().read_only_hint, Some(true));

    let accepted = serde_json::json!({"fields": "compact", "imported_after": "2026-10-22T09:30:00Z", "limit": 20});
    assert!(reject_unknown_arguments(&call("list_assets", accepted)).is_ok());
    let e = reject_unknown_arguments(&call("list_assets", serde_json::json!({"compact": true}))).unwrap_err();
    assert!(
        e.message.contains("\"compact\"") && e.message.contains("fields") && e.message.contains("imported_after"),
        "{}",
        e.message
    );
    assert!(super::EDITING_GUIDE.contains("compact") && super::EDITING_GUIDE.contains("imported_after"));
}

#[test]
fn paging_arguments_are_optional_on_history_and_the_task_list() {
    let tools = router().list_all();
    // The listings and diffs that used to answer everything page the same way.
    for name in ["list_assets", "staged_diff"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        for field in ["offset", "limit"] {
            assert!(tool.input_schema["properties"].get(field).is_some(), "{name} takes {field}");
        }
        assert!(tool
            .input_schema
            .get("required")
            .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)));
    }
    let revision = tools.iter().find(|t| t.name == "revision_diff").unwrap();
    assert_eq!(revision.input_schema["required"], serde_json::json!(["seq"]));
    for field in ["offset", "limit"] {
        assert!(
            revision.input_schema["properties"].get(field).is_some(),
            "revision_diff takes {field}"
        );
    }
    let state = tools.iter().find(|t| t.name == "get_timeline_state").unwrap();
    for field in ["clips_offset", "clips_limit", "captions_offset", "captions_limit"] {
        assert!(
            state.input_schema["properties"].get(field).is_some(),
            "get_timeline_state takes {field}"
        );
    }
    for name in ["history", "list_tasks"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        for field in ["offset", "limit"] {
            assert!(tool.input_schema["properties"].get(field).is_some(), "{name} takes {field}");
        }
        assert!(tool
            .input_schema
            .get("required")
            .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)));
    }
    let transcript = tools.iter().find(|t| t.name == "get_transcript").unwrap();
    for field in ["asset_id", "from", "to", "offset", "limit"] {
        assert!(
            transcript.input_schema["properties"].get(field).is_some(),
            "get_transcript takes {field}"
        );
    }
    assert_eq!(transcript.input_schema["required"], serde_json::json!(["asset_id"]));
    let search = tools.iter().find(|t| t.name == "search_transcripts").unwrap();
    for field in ["query", "asset_ids", "speaker", "mode", "offset", "limit"] {
        assert!(
            search.input_schema["properties"].get(field).is_some(),
            "search_transcripts takes {field}"
        );
    }
    assert_eq!(search.input_schema["required"], serde_json::json!(["query"]));
    let detail = tools.iter().find(|t| t.name == "get_analysis_detail").unwrap();
    assert_eq!(detail.input_schema["required"], serde_json::json!(["asset_id", "kind"]));
}

#[test]
fn the_review_tools_say_what_the_user_alone_can_do() {
    let tools = router().list_all();
    let description = |name: &str| -> String {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default()
            .to_string()
    };
    for name in ["apply_staged_edits", "apply_staged_subset"] {
        let text = description(name);
        assert!(text.contains("review") && text.contains("refused"), "{name}: {text}");
    }
    for name in ["undo", "redo", "revert_to"] {
        let text = description(name);
        assert!(text.contains("LIVE") && text.contains("review"), "{name}: {text}");
    }
    assert!(description("undo").contains("undo_staged_edit"));
    assert!(description("discard_staged_edits").contains("restore_discarded_edits"));
    assert!(description("resolve_task").contains("complete_task"));
    assert!(description("claim_next_task").contains("proposal"));
}

/// With review off a claimed task's work lands live, so the tools that move a task along say so
/// rather than promise a proposal, and `stage_edits` says it is the way to hold a change back.
#[test]
fn the_task_tools_say_what_review_off_means_for_task_work() {
    let tools = router().list_all();
    let description = |name: &str| -> String {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default()
            .to_string()
    };
    let claim = description("claim_next_task");
    assert!(
        claim.contains("With review off no proposal is opened") && claim.contains("live cut"),
        "{claim}"
    );
    assert!(!claim.contains("whether or not the user has review switched on"), "{claim}");
    let complete = description("complete_task");
    assert!(complete.contains("ready") && complete.contains("marked done"), "{complete}");
    assert!(description("fail_task").contains("undo steps them back"));
    let stage = description("stage_edits");
    assert!(
        stage.contains("review off") && stage.contains("task work included"),
        "{stage}"
    );
    assert!(description("apply_staged_edits").contains("stage_edits"));
    assert!(super::INSTRUCTIONS.contains("With review off your edits, task work included"));
    for text in [super::INSTRUCTIONS, super::EDITING_GUIDE] {
        assert!(!text.contains("claim_next_task always opens"), "{text}");
    }
    assert!(super::EDITING_GUIDE.contains("With review OFF nothing is staged"));
    assert!(super::EDITING_GUIDE.contains("already open when the user switches"));
}

#[test]
fn the_descriptions_the_review_called_thin_now_say_what_they_do() {
    let tools = router().list_all();
    let description = |name: &str| -> String {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default()
            .to_string()
    };
    let silence = description("remove_silence");
    assert!(
        silence.contains("analyze_asset") && silence.contains("silence") && silence.contains("first unlocked video track"),
        "{silence}"
    );
    let cut = description("cut_clip");
    assert!(
        cut.contains("SOURCE") && cut.contains("add_clip_to_timeline") && cut.contains("first UNLOCKED"),
        "{cut}"
    );
    let concat = description("concatenate");
    assert!(
        concat.contains("FULL") && concat.contains("in order") && concat.contains("one revision"),
        "{concat}"
    );
    let wave = description("get_waveform");
    assert!(wave.contains("WHOLE") && wave.contains("get_waveform_range"), "{wave}");
    assert!(description("analyze_asset").contains("SUMMARY"));
    assert!(description("list_assets").contains("proxy") && description("list_assets").contains("source_missing"));
    assert!(description("timeline_summary").contains("media_notes"));
    assert!(description("platform_check").contains("width"));
    assert!(description("generate_voiceover").contains("warning"));
}

/// An agent verifying an AAC true-peak problem had to run ffmpeg's ebur128 on the file itself:
/// the export tells what it wrote, and says where it differs from `get_levels`.
#[test]
fn the_export_descriptions_say_what_the_written_file_is_measured_for() {
    let tools = router().list_all();
    let description = |name: &str| -> String {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default()
            .to_string()
    };
    let export = description("export");
    for needle in [
        "report",
        "get_levels",
        "integrated_lufs",
        "true_peak_dbtp",
        "true_peak_over_db",
        "report.problem",
        "metering loudness",
        "starting ffmpeg",
    ] {
        assert!(export.contains(needle), "export: {needle} in {export}");
    }
    let variants = description("export_variants");
    assert!(
        variants.contains("report") && variants.contains("true_peak_over_db"),
        "{variants}"
    );
}

#[test]
fn the_editing_guide_cover_review_jobs_paging_and_proxies() {
    let text = super::EDITING_GUIDE;
    for needle in [
        "STAGED",
        "agent_review",
        "undo_staged_edit",
        "restore_discarded_edits",
        "apply_staged_subset",
        "background: true",
        "job_status",
        "get_transcript",
        "get_analysis_detail",
        "captions_offset",
        "media_notes",
        "discard_unsaved",
    ] {
        assert!(text.contains(needle), "the instructions should mention {needle}");
    }
    assert!(text.find("complete_task").is_some());
}

#[test]
fn a_mistyped_id_names_the_argument() {
    let e = super::parse_arg("clip_id", "not-a-uuid").unwrap_err();
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        e.message.contains("`clip_id`") && e.message.contains("not-a-uuid"),
        "{}",
        e.message
    );
    assert!(super::parse_arg("clip_id", &uuid::Uuid::new_v4().to_string()).is_ok());
    // Through the helpers that parse several ids, it is the field of the item that is named.
    let item = |id: &str| ClipMoveParams {
        clip_id: id.to_string(),
        timeline_start: 0.0,
        track_id: Some("nope".to_string()),
    };
    let e = clip_moves(&[item(&uuid::Uuid::new_v4().to_string())]).unwrap_err();
    assert!(e.message.contains("track_id"), "{}", e.message);
}

#[test]
fn the_new_core_refusals_are_the_callers_to_fix() {
    use rmcp::model::ErrorCode;
    for e in [
        kerf_core::Error::StagedEditOwnedByOther,
        kerf_core::Error::AgentReviewRequired,
        kerf_core::Error::NoDiscardedEdit,
        kerf_core::Error::AssetInUse {
            asset: uuid::Uuid::new_v4(),
            clips: 2,
        },
    ] {
        let rendered = e.to_string();
        let mapped = core_err(e);
        assert_eq!(mapped.code, ErrorCode::INVALID_PARAMS, "{rendered}");
        assert_eq!(mapped.message, rendered, "the message reaches the model as the core wrote it");
    }
    assert!(core_err(kerf_core::Error::AgentReviewRequired)
        .message
        .contains("ask them to review"));
}

#[test]
fn an_import_path_is_refused_on_the_call_not_in_a_job() {
    for bad in [
        "clip.mp4",
        "https://example.com/a.mp4",
        "concat:a|b",
        "/definitely/not/here.mp4",
    ] {
        let e = core_err(kerf_core::paths::validate_import_path(std::path::Path::new(bad)).unwrap_err());
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{bad}");
    }
}

#[test]
fn a_network_or_device_path_is_refused_before_any_filesystem_call() {
    for bad in [
        "\\\\server\\share\\clip.mp4",
        "\\\\?\\UNC\\server\\share\\clip.mp4",
        "\\\\.\\pipe\\kerf",
        "\\/server/share/clip.mp4",
    ] {
        let why = crate::require_local_input_path(bad).expect_err(bad);
        assert!(why.contains("network"), "{bad}: {why}");
        let e = caption_source(Some(bad), None).unwrap_err();
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{bad}");
        assert!(e.message.contains("network"), "{bad}: {}", e.message);
    }
    for bad in ["//server/share/clip.mp4", "//./pipe/kerf"] {
        assert!(crate::check_input_path(bad, true).is_err(), "{bad}");
    }
    for ok in [
        "/home/user/clip.mp4",
        "//tmp/clip.mp4",
        "C:\\Users\\me\\clip.mp4",
        "\\\\?\\C:\\Users\\me\\clip.mp4",
    ] {
        assert!(crate::check_input_path(ok, false).is_ok(), "{ok}");
    }
    assert!(
        crate::check_input_path("//tmp/clip.mp4", true).is_err(),
        "//host/share is a share on Windows"
    );
}

#[test]
fn a_platform_check_frame_is_both_sides_or_neither() {
    use super::check_frame;
    assert_eq!(check_frame(None, None).unwrap(), None);
    assert_eq!(check_frame(Some(1080), Some(1920)).unwrap(), Some((1080, 1920)));
    for (w, h) in [
        (Some(1080), None),
        (None, Some(1920)),
        (Some(0), Some(1920)),
        (Some(1080), Some(99_999)),
    ] {
        assert_eq!(
            check_frame(w, h).unwrap_err().code,
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "{w:?} {h:?}"
        );
    }
}

#[test]
fn the_session_is_the_header_rmcp_stamps_on_the_request() {
    use super::session_from_parts;
    let parts = |header: Option<&str>| {
        let mut request = axum::http::Request::builder().uri("/mcp");
        if let Some(h) = header {
            request = request.header("mcp-session-id", h);
        }
        request.body(()).unwrap().into_parts().0
    };
    assert_eq!(session_from_parts(&parts(Some("abc-123"))), Some("abc-123".to_string()));
    assert_eq!(
        session_from_parts(&parts(None)),
        None,
        "a client with no session has no owner"
    );
    assert_eq!(session_from_parts(&parts(Some(""))), None);
}

#[test]
fn a_project_swapped_mid_job_is_noticed() {
    let mut project = kerf_core::Project::open_in_memory().unwrap();
    let before = project.generation();
    assert!(super::require_generation(&project, before, "it ran").is_ok());
    project.replace_with(kerf_core::Project::open_in_memory().unwrap()).close();
    let e = super::require_generation(&project, before, "it ran").unwrap_err();
    assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        e.message.contains("replaced") && e.message.contains("Run it again"),
        "{}",
        e.message
    );
}

/// One agent's proposal is closed to another connected at the same time, whichever door it
/// comes through — here the one every edit tool takes.
#[test]
fn a_second_session_is_refused_while_the_first_owns_the_proposal() {
    let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
    {
        let first = super::lock_agent_edit(&shared, Some("session-a")).expect("the first edit opens the proposal");
        first
            .add_overlay("Title".to_string(), 0.0, 2.0)
            .expect("it goes into the proposal");
        assert!(
            first.staged().unwrap().is_some(),
            "review is on by default, so the edit was staged"
        );
        assert!(first.timeline().unwrap().overlays.is_empty(), "the user's cut is untouched");
    }
    // The same session carries on.
    assert!(super::lock_agent_edit(&shared, Some("session-a")).is_ok());
    // Another is turned away, with words for the model.
    let refused = super::lock_agent_edit(&shared, Some("session-b")).err().expect("refused");
    assert_eq!(refused.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(refused.message.contains("another agent"), "{}", refused.message);
    // So is a client that names no session at all.
    assert!(super::lock_agent_edit(&shared, None).is_err());
    // It cannot throw the proposal away either: a session that merely declares itself is
    // checked against the owner.
    let stranger = super::lock_agent_as(&shared, Some("session-b"));
    let e = stranger.discard_staged().map(|_| ()).unwrap_err();
    assert!(matches!(e, kerf_core::Error::StagedEditOwnedByOther), "{e}");
    let e = stranger.undo_staged().map(|_| ()).unwrap_err();
    assert!(matches!(e, kerf_core::Error::StagedEditOwnedByOther), "{e}");
}

/// Through the door every edit tool takes, an agent's edit to a clip on a track the user locked is
/// refused in the words the instructions promise — and leaves no proposal behind — and the agent
/// cannot unlock the track either.
#[test]
fn an_agent_session_is_refused_a_locked_track_and_cannot_unlock_it() {
    let project = kerf_core::Project::open_in_memory().unwrap();
    let asset: kerf_core::Asset = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "path": "/footage/a.mp4",
        "name": "a.mp4",
        "duration": 10.0,
        "streams": [{ "index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080, "fps": 30.0 }],
        "imported_at": "2025-01-01T00:00:00Z",
    }))
    .unwrap();
    project.insert_or_get_asset(&asset).unwrap();
    let clip = project.add_clip_to_timeline(asset.id, None, 0.0, 4.0, Some(0.0)).unwrap();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_track_locked(v1, true).unwrap();
    let shared = std::sync::Mutex::new(project);

    {
        let agent = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
        assert!(
            agent.working_timeline().unwrap().track(v1).unwrap().locked,
            "the agent sees the lock"
        );
        let refused = agent.split_at(clip.id, 1.0).map_err(super::core_err).unwrap_err();
        assert_eq!(refused.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            refused.message.contains("track V1 is locked"),
            "the refusal names the locked track"
        );
        assert!(agent.staged().unwrap().is_none(), "a refused edit opens no proposal");
    }
    let agent = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
    let refused = agent.set_track_locked(v1, false).map_err(super::core_err).unwrap_err();
    assert!(
        refused.message.contains("an agent cannot unlock a track"),
        "the refusal says an agent cannot unlock"
    );
    assert!(agent.timeline().unwrap().track(v1).unwrap().locked);
}

/// A task removed by an agent that does not own the proposal under it stays in the queue; the
/// owner may remove it, and the user always may.
#[test]
fn a_task_is_not_removed_by_an_agent_that_does_not_own_its_proposal() {
    let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
    let task = super::lock_agent_as(&shared, Some("session-a")).add_task("tighten").unwrap();
    {
        let owner = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
        owner.claim_next_task(Some("session-a")).unwrap();
        owner.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
    }
    let stranger = super::lock_agent_as(&shared, Some("session-b"));
    let e = stranger.remove_task(task.id).unwrap_err();
    assert!(matches!(e, kerf_core::Error::StagedEditOwnedByOther), "{e}");
    assert!(
        stranger.get_task(task.id).unwrap().is_some(),
        "the refusal left the task in the queue"
    );
    drop(stranger);
    let owner = super::lock_agent_as(&shared, Some("session-a"));
    owner.remove_task(task.id).unwrap();
    assert!(owner.get_task(task.id).unwrap().is_none());
}

/// What the user discarded, an agent does not bring back; what it discarded itself it may.
#[test]
fn an_agent_cannot_restore_a_proposal_the_user_discarded() {
    let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
    {
        let guard = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
        guard.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
    }
    crate::lock_user(&shared).discard_staged().unwrap();
    let agent = super::lock_agent_as(&shared, Some("session-a"));
    let e = agent.restore_discarded().unwrap_err();
    assert!(e.to_string().contains("only they can bring it back"), "{e}");
    assert!(agent.staged().unwrap().is_none());
    drop(agent);
    assert!(crate::lock_user(&shared).restore_discarded().is_ok(), "the user can");
}

/// With review on, the agent leaves the user's ripple switch alone.
#[test]
fn an_agent_does_not_flip_the_ripple_switch_while_review_is_on() {
    let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
    let agent = super::lock_agent_as(&shared, Some("session-a"));
    assert!(agent.agent_review());
    let e = agent.set_ripple_mode(true).unwrap_err();
    assert_eq!(super::core_err(e).code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(!agent.ripple_mode().unwrap());
}

/// With review on, an agent cannot accept its own proposal — `force` or not — nor step the
/// user's live history.
#[test]
fn an_agent_cannot_approve_its_own_work_while_review_is_on() {
    let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
    {
        let guard = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
        guard.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
    }
    let agent = super::lock_agent_as(&shared, Some("session-a"));
    assert!(agent.agent_review(), "on by default");
    for force in [false, true] {
        let e = agent.apply_staged(force).map(|_| ()).unwrap_err();
        assert!(matches!(e, kerf_core::Error::AgentReviewRequired), "force={force}: {e}");
    }
    let e = agent.apply_staged_subset(&["x".to_string()], true).map(|_| ()).unwrap_err();
    assert!(matches!(e, kerf_core::Error::AgentReviewRequired), "{e}");
    assert!(agent.staged().unwrap().is_some(), "nothing was applied");
    // Its own steps it may take back, and bring a discarded proposal back.
    let (label, _) = agent.undo_staged().unwrap();
    assert!(!label.is_empty());
    agent.discard_staged().unwrap();
}

/// `mcp.md` lists every tool by name; a tool added without a line there fails here, so the
/// index cannot fall behind the surface again.
#[test]
fn the_docs_name_every_tool() {
    let docs = include_str!("../../../../.claude/docs/mcp.md");
    let missing: Vec<String> = tool_names()
        .into_iter()
        .filter(|name| !docs.contains(&format!("`{name}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "add these to the tool index in .claude/docs/mcp.md: {missing:?}"
    );
}

/// An export, cover or subtitle file is never written over something the project still needs.
#[tokio::test(flavor = "multi_thread")]
async fn an_output_onto_a_source_file_is_refused_before_anything_renders() {
    let dir = std::env::temp_dir().join(format!("kerf-mcp-out-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("clip.mp4");
    std::fs::write(&source, b"not really a video").unwrap();
    let project = kerf_core::Project::open_in_memory().unwrap();
    let asset: kerf_core::Asset = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "path": source.to_string_lossy(),
        "name": "clip.mp4",
        "duration": 2.0,
        "streams": [],
        "imported_at": "2025-01-01T00:00:00Z",
    }))
    .unwrap();
    project.insert_or_get_asset(&asset).unwrap();
    let shared = std::sync::Arc::new(std::sync::Mutex::new(project));

    let refused = super::check_output_paths(shared.clone(), None, vec![dir.join("out.mp4"), source.clone()])
        .await
        .expect_err("the source is not a place to render to");
    assert_eq!(refused.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(refused.message.contains("clip.mp4"), "{}", refused.message);
    // The same file by another way in.
    let detour = dir.join("..").join(dir.file_name().unwrap()).join("clip.mp4");
    assert!(super::check_output_paths(shared.clone(), None, vec![detour]).await.is_err());
    // Anywhere else is fine.
    super::check_output_paths(shared, None, vec![dir.join("out.mp4")])
        .await
        .expect("a fresh path");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A call that fails leaves no empty proposal owned by the agent behind.
#[test]
fn a_failed_first_edit_opens_no_proposal() {
    let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
    {
        let guard = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
        assert!(guard.remove_overlay(uuid::Uuid::new_v4()).is_err(), "no such title");
        assert!(guard.staged().unwrap().is_none(), "nothing was staged, so nothing is open");
    }
    // And so a second session is not locked out by a mistake of the first.
    let second = super::lock_agent_edit(&shared, Some("session-b")).unwrap();
    second.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
    assert!(second.staged().unwrap().is_some());
}

/// The tag tools, the filter on `list_assets` and the way they are described.
#[test]
fn the_tag_tools_are_on_the_surface_and_described_for_a_model() {
    let tools = router().list_all();
    let tool = |name: &str| tools.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("{name}"));
    let description = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
    let hint = |name: &str| tool(name).annotations.clone().unwrap();
    let required = |name: &str| -> Vec<String> {
        let mut required: Vec<String> = tool(name)
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        required.sort();
        required
    };
    assert_eq!(hint("list_tags").read_only_hint, Some(true));
    assert!(required("list_tags").is_empty());
    let edit = hint("edit_asset_tags");
    assert_eq!(
        (edit.read_only_hint, edit.destructive_hint, edit.idempotent_hint),
        (Some(false), Some(false), Some(true))
    );
    assert_eq!(hint("assemble_by_tag").read_only_hint, Some(false));
    assert_eq!(hint("assemble_by_tag").destructive_hint, Some(false));
    assert_eq!(hint("assemble_by_tag").idempotent_hint, Some(false), "it appends every time");

    assert_eq!(required("edit_asset_tags"), ["asset_ids"]);
    for field in ["add", "remove", "remove_keys"] {
        assert!(
            tool("edit_asset_tags").input_schema["properties"].get(field).is_some(),
            "{field}"
        );
    }
    assert_eq!(required("assemble_by_tag"), ["key"]);
    for field in [
        "key",
        "order",
        "filter",
        "include_untagged",
        "title",
        "title_seconds",
        "sort_within",
    ] {
        assert!(
            tool("assemble_by_tag").input_schema["properties"].get(field).is_some(),
            "{field}"
        );
    }
    assert!(tool("list_assets").input_schema["properties"].get("tags").is_some());

    let edit = description("edit_asset_tags");
    for needle in ["remove_keys", "location", "list_tags", "review", "whole call"] {
        assert!(edit.contains(needle), "edit_asset_tags does not say `{needle}`: {edit}");
    }
    let assemble = description("assemble_by_tag");
    for needle in [
        "location",
        "overlay",
        "card",
        "include_untagged",
        "ONE edit",
        "appends",
        "unmatched",
    ] {
        assert!(
            assemble.contains(needle),
            "assemble_by_tag does not say `{needle}`: {assemble}"
        );
    }
    assert!(description("list_tags").contains("assemble_by_tag") && description("list_tags").contains("plain"));
    assert!(description("list_assets").contains("`tags`"));

    let guide = super::EDITING_GUIDE;
    for needle in ["TAGS", "list_tags", "edit_asset_tags", "assemble_by_tag"] {
        assert!(guide.contains(needle), "the guide does not say `{needle}`");
    }
    let accepted = serde_json::json!({"tags": [{"key": "location", "value": "Cumbuco"}, {"value": "jump"}]});
    assert!(reject_unknown_arguments(&call("list_assets", accepted)).is_ok());
    let typo = serde_json::json!({"key": "location", "tittle": "card"});
    let e = reject_unknown_arguments(&call("assemble_by_tag", typo)).unwrap_err();
    assert!(
        e.message.contains("tittle") && e.message.contains("title_seconds"),
        "{}",
        e.message
    );
    assert!(
        serde_json::from_value::<kerf_core::AssembleByTag>(serde_json::json!({"key": "location", "tittle": "card"})).is_err()
    );
}

fn headless_server() -> (KerfMcp, std::sync::Arc<std::sync::Mutex<kerf_core::Project>>) {
    let shared = std::sync::Arc::new(std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap()));
    let state = crate::AppState::new(
        shared.clone(),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        crate::LaunchSlot::default(),
        std::sync::Arc::new(kerf_core::ScrubFrames::new(kerf_core::ScrubConfig::default())),
    );
    let shell = Shell::Headless(std::sync::Arc::new(crate::shell::Headless::new(state, None, None)));
    (KerfMcp::new(shared.clone(), shell), shared)
}

fn footage(project: &kerf_core::Project, name: &str, seconds: f64, imported: &str) -> kerf_core::Asset {
    let asset: kerf_core::Asset = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "path": format!("/footage/{name}"),
        "name": name,
        "duration": seconds,
        "streams": [{ "index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080, "fps": 30.0 }],
        "imported_at": imported,
    }))
    .unwrap();
    project.insert_or_get_asset(&asset).unwrap();
    asset
}

fn tag(key: Option<&str>, value: &str) -> TagArg {
    TagArg {
        key: key.map(str::to_string),
        value: value.to_string(),
    }
}

fn answer(text: String) -> serde_json::Value {
    serde_json::from_str(&text).unwrap()
}

/// A tag `edit_asset_tags` is given names only `key` and `value`: a misspelt member is the caller's mistake, not a
/// plain tag that quietly lost its key.
#[test]
fn a_misspelt_tag_member_is_refused() {
    let call = |add: serde_json::Value| {
        serde_json::from_value::<EditAssetTagsParams>(serde_json::json!({
            "asset_ids": ["00000000-0000-0000-0000-000000000001"],
            "add": add,
            "remove": add,
        }))
        .map(|p| (p.add.map_or(0, |a| a.len()), p.remove.map_or(0, |a| a.len())))
        .map_err(|e| e.to_string())
    };
    assert_eq!(
        call(serde_json::json!([{"key": "location", "value": "X"}, {"value": "jump"}])),
        Ok((2, 2))
    );
    let typo = call(serde_json::json!([{"keys": "location", "value": "X"}])).unwrap_err();
    assert!(typo.contains("unknown field `keys`"), "{typo}");
    let missing = call(serde_json::json!([{"key": "location"}])).unwrap_err();
    assert!(missing.contains("missing field `value`"), "{missing}");
    let extra = serde_json::from_value::<EditAssetTagsParams>(serde_json::json!({
        "asset_ids": [], "add": [{"value": "x", "colour": "red"}]
    }))
    .unwrap_err()
    .to_string();
    assert!(extra.contains("unknown field `colour`"), "{extra}");
}

/// The agent tags assets and builds a cut from them: with review on both are held back with its
/// proposal (the user's library and cut are untouched), the agent's own reads see them, and the
/// webview is told after each. A refused call changes nothing and says so as the caller's mistake.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_tags_assets_and_assembles_a_cut_by_them_under_review() {
    use rmcp::model::ErrorCode;
    let (server, shared) = headless_server();
    let (a, b, c) = {
        let project = shared.lock().unwrap();
        (
            footage(&project, "a.mp4", 10.0, "2026-01-01T00:00:00Z"),
            footage(&project, "b.mp4", 6.0, "2026-01-01T00:00:01Z"),
            footage(&project, "c.mp4", 4.0, "2026-01-01T00:00:02Z"),
        )
    };
    let changed = std::sync::Arc::new(AtomicI64::new(0));
    {
        let changed = changed.clone();
        server.app.listen("project-changed", move |_| {
            changed.fetch_add(1, Ordering::Relaxed);
        });
    }
    let ids = |assets: &[&kerf_core::Asset]| assets.iter().map(|a| a.id.to_string()).collect::<Vec<_>>();

    let done = answer(
        server
            .edit_asset_tags(Parameters(EditAssetTagsParams {
                asset_ids: ids(&[&a, &c]),
                add: Some(vec![tag(Some(" Location "), "Jeri"), tag(None, "jump")]),
                remove: None,
                remove_keys: None,
            }))
            .unwrap(),
    );
    assert_eq!(done["assets"][0]["name"], "a.mp4");
    assert_eq!(done["assets"][0]["tags"], serde_json::json!(["location: Jeri", "jump"]));
    server
        .edit_asset_tags(Parameters(EditAssetTagsParams {
            asset_ids: ids(&[&b]),
            add: Some(vec![tag(Some("location"), "Cumbuco")]),
            remove: None,
            remove_keys: None,
        }))
        .unwrap();
    assert_eq!(changed.load(Ordering::Relaxed), 2, "the webview is told after each");

    {
        let user = crate::lock_user(&shared);
        assert!(
            user.get_asset(a.id).unwrap().unwrap().tags.is_empty(),
            "the library is the user's until they accept"
        );
        let staged = user.staged().unwrap().expect("the tags are held back as a proposal");
        assert_eq!(staged.asset_changes.len(), 3);
        assert!(staged
            .asset_changes
            .iter()
            .all(|c| c.id.starts_with("asset_tags:") && c.tag_edit.is_some()));
    }

    let vocabulary = answer(server.list_tags().unwrap());
    assert_eq!(vocabulary["keys"][0]["key"], "location");
    assert_eq!(vocabulary["keys"][0]["assets"], 3);
    assert_eq!(
        vocabulary["keys"][0]["values"],
        serde_json::json!([{"value": "Cumbuco", "assets": 1}, {"value": "Jeri", "assets": 2}])
    );
    assert_eq!(vocabulary["plain"], serde_json::json!([{"value": "jump", "assets": 2}]));

    // Cumbuco first, then Jeri; a title over the start of each section.
    let mut req = kerf_core::AssembleByTag::new("location");
    req.order = Some(vec!["cumbuco".to_string()]);
    let built = answer(server.assemble_by_tag(Parameters(req)).await.unwrap());
    let groups = built["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(
        (groups[0]["value"].as_str(), groups[1]["value"].as_str()),
        (Some("Cumbuco"), Some("Jeri"))
    );
    assert_eq!(groups[0]["asset_ids"], serde_json::json!([b.id]));
    assert_eq!(groups[1]["asset_ids"], serde_json::json!([a.id, c.id]));
    assert!(groups.iter().all(|g| g["title_clip_id"].is_string()));
    assert_eq!(built["clip_ids"].as_array().unwrap().len(), 3);
    assert_eq!(
        (groups[0]["start"].as_f64(), groups[1]["start"].as_f64()),
        (Some(0.0), Some(6.0))
    );
    assert_eq!(changed.load(Ordering::Relaxed), 3);
    {
        let user = crate::lock_user(&shared);
        assert!(
            user.timeline().unwrap().tracks.iter().all(|t| t.clips.is_empty()),
            "the cut is staged, not live"
        );
        let proposed = user.staged_timeline().unwrap().expect("the cut is in the proposal");
        assert_eq!(
            proposed.tracks.iter().flat_map(|t| &t.clips).count(),
            5,
            "three clips and two titles"
        );
    }

    // Mistakes are the caller's, and leave nothing behind.
    let before = changed.load(Ordering::Relaxed);
    let unknown = server
        .edit_asset_tags(Parameters(EditAssetTagsParams {
            asset_ids: vec![a.id.to_string(), uuid::Uuid::new_v4().to_string()],
            add: Some(vec![tag(None, "wave")]),
            remove: None,
            remove_keys: None,
        }))
        .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::INVALID_PARAMS);
    let empty = server
        .edit_asset_tags(Parameters(EditAssetTagsParams {
            asset_ids: ids(&[&a]),
            add: Some(vec![tag(None, "  ")]),
            remove: None,
            remove_keys: None,
        }))
        .unwrap_err();
    assert_eq!(empty.code, ErrorCode::INVALID_PARAMS);
    let nothing = server
        .edit_asset_tags(Parameters(EditAssetTagsParams {
            asset_ids: ids(&[&a]),
            add: None,
            remove: None,
            remove_keys: None,
        }))
        .unwrap_err();
    assert_eq!(nothing.code, ErrorCode::INVALID_PARAMS);
    let not_an_id = server
        .edit_asset_tags(Parameters(EditAssetTagsParams {
            asset_ids: vec!["a.mp4".to_string()],
            add: Some(vec![tag(None, "wave")]),
            remove: None,
            remove_keys: None,
        }))
        .unwrap_err();
    assert_eq!(not_an_id.code, ErrorCode::INVALID_PARAMS);
    let no_match = server
        .assemble_by_tag(Parameters(kerf_core::AssembleByTag::new("rider")))
        .await
        .unwrap_err();
    assert_eq!(no_match.code, ErrorCode::INVALID_PARAMS);
    let too_long = {
        let mut req = kerf_core::AssembleByTag::new("location");
        req.title_seconds = 600.0;
        server.assemble_by_tag(Parameters(req)).await.unwrap_err()
    };
    assert_eq!(too_long.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        changed.load(Ordering::Relaxed),
        before,
        "a refused call tells the webview nothing"
    );
    assert_eq!(
        crate::lock_user(&shared).staged().unwrap().unwrap().asset_changes.len(),
        3,
        "and holds nothing back"
    );
}

#[test]
fn the_motion_tools_say_what_they_take_and_name_every_preset() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let required = |name: &str| -> Vec<String> {
        let mut r: Vec<String> = tool(name)
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        r.sort();
        r
    };
    assert_eq!(required("apply_motion_preset"), ["clip_ids", "preset"]);
    assert!(required("list_motion_presets").is_empty());

    let description = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
    let apply = description("apply_motion_preset");
    let (_, rest) = apply.split_once("ENTRANCES:").expect("the presets are listed by kind");
    let (entrances, rest) = rest.split_once("EXITS:").expect("the presets are listed by kind");
    let (exits, rest) = rest.split_once("EMPHASES:").expect("the presets are listed by kind");
    let (emphases, _) = rest
        .split_once("A direction names")
        .expect("the list ends where the rules begin");
    for info in kerf_core::motion_presets() {
        for (listed, kind) in [
            (entrances, kerf_core::MotionKind::Entrance),
            (exits, kerf_core::MotionKind::Exit),
            (emphases, kerf_core::MotionKind::Emphasis),
        ] {
            assert_eq!(
                listed.contains(info.id.as_str()),
                info.kind == kind,
                "`{}` is listed under the wrong kind (or not at all): {apply}",
                info.id.as_str()
            );
        }
    }
    let both = apply.clone() + &description("list_motion_presets");
    for needle in [
        "own pose",
        "`add`",
        "`replace`",
        "clamped",
        "all or nothing",
        "audio track",
        "`start` / `end`",
        "default_duration",
    ] {
        assert!(both.contains(needle), "the motion tools do not say `{needle}`: {both}");
    }
    let schema = serde_json::to_string(&tool("apply_motion_preset").input_schema).unwrap();
    for needle in [
        "slide_in_from_left",
        "drop_out",
        "replace",
        "0.25 to 2",
        "clamped",
        "list_motion_presets",
    ] {
        assert!(schema.contains(needle), "the schema does not say `{needle}`: {schema}");
    }

    let hint = |name: &str| tool(name).annotations.clone().unwrap();
    assert_eq!(hint("list_motion_presets").read_only_hint, Some(true));
    let apply_hint = hint("apply_motion_preset");
    assert_eq!(
        (
            apply_hint.read_only_hint,
            apply_hint.destructive_hint,
            apply_hint.idempotent_hint
        ),
        (Some(false), Some(false), Some(true)),
        "a staged, undoable edit that applying twice does not repeat"
    );
    for name in ["apply_motion_preset", "list_motion_presets"] {
        assert_eq!(
            super::toolsets::set_of(name),
            Some(super::toolsets::ToolSet::Keyframes),
            "{name}"
        );
    }

    assert!(super::INSTRUCTIONS.contains("apply_motion_preset"));
    assert!(super::INSTRUCTIONS.len() <= 3500, "{} characters", super::INSTRUCTIONS.len());
    for needle in ["apply_motion_preset", "list_motion_presets", "own pose", "mode add"] {
        assert!(super::EDITING_GUIDE.contains(needle), "the guide does not say `{needle}`");
    }

    let parse = |v: serde_json::Value| serde_json::from_value::<super::MotionPresetParams>(v);
    let ok = parse(serde_json::json!({"clip_ids": ["c"], "preset": "slide_out_to_left", "mode": "replace"})).unwrap();
    assert_eq!(ok.preset, kerf_core::MotionPreset::SlideOutToLeft);
    assert_eq!(ok.mode, Some(kerf_core::MotionMode::Replace));
    for bad in [
        serde_json::json!({"clip_ids": ["c"], "preset": "slide_in_left"}),
        serde_json::json!({"clip_ids": ["c"], "preset": "fade_in", "mode": "merge"}),
        serde_json::json!({"preset": "fade_in"}),
    ] {
        assert!(parse(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn list_motion_presets_answers_what_the_picker_lists() {
    let (server, _) = headless_server();
    let listed = answer(server.list_motion_presets().unwrap());
    let list = listed.as_array().unwrap();
    assert_eq!(list.len(), kerf_core::MotionPreset::ALL.len());
    assert_eq!(list[0]["id"], "fade_in");
    assert_eq!(list[0]["kind"], "entrance");
    assert_eq!(list[6]["default_duration"], 0.5);
    let push = list.iter().find(|p| p["id"] == "push_in").unwrap();
    assert!(push.get("default_duration").is_none());
    assert_eq!(
        listed,
        serde_json::to_value(kerf_core::motion_presets()).unwrap(),
        "the tool and the Tauri command list the same presets"
    );
}

/// The agent animates clips with a preset: with review on it is held back as a proposal (the live cut
/// is untouched), the agent's own reads see it, the webview is told once per edit, the window each clip
/// got comes back clamped, and a refused call changes nothing and says so as the caller's mistake.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_applies_a_motion_preset_and_it_stages() {
    use rmcp::model::ErrorCode;
    let (server, shared) = headless_server();
    let (long, short, voice) = {
        let project = shared.lock().unwrap();
        let video = footage(&project, "a.mp4", 60.0, "2026-01-01T00:00:00Z");
        let long = project.add_clip_to_timeline(video.id, None, 0.0, 10.0, Some(0.0)).unwrap();
        let short = project.add_clip_to_timeline(video.id, None, 20.0, 20.25, Some(10.0)).unwrap();
        let sound: kerf_core::Asset = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "path": "/footage/voice.wav", "name": "voice.wav", "duration": 5.0,
            "streams": [{ "index": 0, "kind": "audio", "codec": "pcm_s16le", "sample_rate": 48000, "channels": 2 }],
            "imported_at": "2026-01-01T00:00:02Z",
        }))
        .unwrap();
        project.insert_or_get_asset(&sound).unwrap();
        let voice = project.add_clip_to_timeline(sound.id, None, 0.0, 5.0, Some(0.0)).unwrap();
        (long, short, voice)
    };
    let changed = std::sync::Arc::new(AtomicI64::new(0));
    {
        let changed = changed.clone();
        server.app.listen("project-changed", move |_| {
            changed.fetch_add(1, Ordering::Relaxed);
        });
    }
    let params = |ids: Vec<String>, preset, mode, at, duration, intensity| super::MotionPresetParams {
        clip_ids: ids,
        preset,
        mode,
        at,
        duration,
        intensity,
    };
    let ids = vec![long.id.to_string(), short.id.to_string()];

    let done = answer(
        server
            .apply_motion_preset(Parameters(params(
                ids.clone(),
                kerf_core::MotionPreset::FadeIn,
                None,
                None,
                None,
                None,
            )))
            .unwrap(),
    );
    assert_eq!(
        (done["preset"].as_str(), done["mode"].as_str()),
        (Some("fade_in"), Some("add"))
    );
    let windows = done["applied"].as_array().unwrap();
    assert_eq!(windows[0]["clip_id"], long.id.to_string());
    assert_eq!(
        (windows[0]["start"].as_f64(), windows[0]["end"].as_f64()),
        (Some(0.0), Some(0.5))
    );
    assert_eq!(
        (windows[1]["start"].as_f64(), windows[1]["end"].as_f64()),
        (Some(0.0), Some(0.25)),
        "the half second fade is clamped to the quarter second clip, and the result says so"
    );
    assert_eq!(changed.load(Ordering::Relaxed), 1, "the webview is told after the edit");

    // The exit reads the proposal: it merges into the entrance the first call staged.
    server
        .apply_motion_preset(Parameters(params(
            vec![long.id.to_string()],
            kerf_core::MotionPreset::FadeOut,
            Some(kerf_core::MotionMode::Add),
            None,
            Some(1.0),
            None,
        )))
        .unwrap();
    {
        let user = crate::lock_user(&shared);
        let live = user.timeline().unwrap();
        assert!(
            live.clip(long.id).unwrap().channels.is_empty(),
            "the live cut is the user's until they accept"
        );
        let proposed = user
            .staged_timeline()
            .unwrap()
            .expect("the preset is held back as a proposal");
        let keys: Vec<(f64, f64)> = proposed
            .clip(long.id)
            .unwrap()
            .property_keys(kerf_core::Property::Opacity)
            .iter()
            .map(|k| (k.time, k.value))
            .collect();
        assert_eq!(keys, [(0.0, 0.0), (0.5, 1.0), (9.0, 1.0), (10.0, 0.0)]);
        assert!(user.staged().unwrap().is_some());
    }
    assert_eq!(changed.load(Ordering::Relaxed), 2);

    // Mistakes are the caller's, and leave nothing behind.
    let before = changed.load(Ordering::Relaxed);
    let staged_before = serde_json::to_value(crate::lock_user(&shared).staged_timeline().unwrap()).unwrap();
    for (name, call) in [
        (
            "not an id",
            params(vec!["a.mp4".into()], kerf_core::MotionPreset::Pulse, None, None, None, None),
        ),
        (
            "a clip that is not there",
            params(
                vec![long.id.to_string(), uuid::Uuid::new_v4().to_string()],
                kerf_core::MotionPreset::Pulse,
                None,
                None,
                None,
                None,
            ),
        ),
        (
            "nothing named",
            params(vec![], kerf_core::MotionPreset::Pulse, None, None, None, None),
        ),
        (
            "a sound",
            params(
                vec![long.id.to_string(), voice.id.to_string()],
                kerf_core::MotionPreset::Pulse,
                None,
                None,
                None,
                None,
            ),
        ),
        (
            "a window of nothing",
            params(ids.clone(), kerf_core::MotionPreset::Pulse, None, None, Some(0.0), None),
        ),
        (
            "an intensity past the top",
            params(ids.clone(), kerf_core::MotionPreset::Pulse, None, None, None, Some(5.0)),
        ),
        (
            "a start past the clip",
            params(ids, kerf_core::MotionPreset::Pulse, None, Some(99.0), None, None),
        ),
    ] {
        let err = server.apply_motion_preset(Parameters(call)).unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS, "{name}: {}", err.message);
    }
    assert_eq!(
        changed.load(Ordering::Relaxed),
        before,
        "a refused call tells the webview nothing"
    );
    assert_eq!(
        serde_json::to_value(crate::lock_user(&shared).staged_timeline().unwrap()).unwrap(),
        staged_before,
        "and changes nothing in the proposal"
    );
}

#[test]
fn the_behavior_tools_say_what_they_take_and_how_far() {
    let tools = router().list_all();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("`{name}` is registered"))
    };
    let required = |name: &str| -> Vec<String> {
        let mut r: Vec<String> = tool(name)
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        r.sort();
        r
    };
    assert_eq!(required("set_behaviors"), ["behaviors", "clip_ids"]);
    assert_eq!(required("bake_behaviors"), ["clip_ids"]);

    let description = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
    let set = description("set_behaviors");
    for kind in kerf_core::BehaviorKind::wire_names() {
        assert!(set.contains(kind), "the description does not name `{kind}`: {set}");
    }
    for needle in [
        "on top of the clip's pose",
        "REPLACES",
        "at most 8",
        "[] clears",
        "never opacity",
        "export exactly",
        "GPU preview falls back to FFmpeg",
        "all or nothing",
        "audio track",
        "bake_behaviors",
        "get_timeline_state",
    ] {
        assert!(set.contains(needle), "set_behaviors does not say `{needle}`: {set}");
    }
    let bake = description("bake_behaviors");
    for needle in [
        "keyframes",
        "every frame",
        "1000 keys",
        "split it first",
        "no behaviours is refused",
        "all or nothing",
    ] {
        assert!(bake.contains(needle), "bake_behaviors does not say `{needle}`: {bake}");
    }
    // The schema carries every limit the core enforces, so a model need not learn them from a refusal.
    let schema = serde_json::to_string(&tool("set_behaviors").input_schema).unwrap();
    for limit in [
        kerf_core::MAX_SWAY_DEGREES,
        kerf_core::MAX_BEHAVIOR_HZ,
        kerf_core::MAX_BREATHE,
        kerf_core::MAX_SPIN_TURNS,
        kerf_core::MAX_DRIFT,
        kerf_core::MAX_SHAKE,
    ] {
        let said = format!("{limit}");
        assert!(
            schema.contains(&format!("..{said}")) || schema.contains(&format!(" {said}")),
            "the schema does not say the limit {said}: {schema}"
        );
    }
    assert!(schema.contains(&format!("at most {}", kerf_core::MAX_BEHAVIORS)), "{schema}");
    assert!(schema.contains("90 is straight down"), "{schema}");
    assert!(schema.contains("same seed is the same shake"), "{schema}");

    let hint = |name: &str| tool(name).annotations.clone().unwrap();
    let set_hint = hint("set_behaviors");
    assert_eq!(
        (set_hint.read_only_hint, set_hint.destructive_hint, set_hint.idempotent_hint),
        (Some(false), Some(false), Some(true)),
        "a list replaced by the same list is the same cut"
    );
    let bake_hint = hint("bake_behaviors");
    assert_eq!(
        (
            bake_hint.read_only_hint,
            bake_hint.destructive_hint,
            bake_hint.idempotent_hint
        ),
        (Some(false), Some(false), Some(false)),
        "a staged, undoable edit that does not claim to repeat harmlessly"
    );
    for name in ["set_behaviors", "bake_behaviors"] {
        assert_eq!(
            super::toolsets::set_of(name),
            Some(super::toolsets::ToolSet::Keyframes),
            "{name}"
        );
        assert!(super::batch::BATCHABLE.contains(&name), "{name} batches in apply_edits");
    }

    assert!(super::INSTRUCTIONS.contains("set_behaviors"));
    assert!(super::INSTRUCTIONS.len() <= 3500, "{} characters", super::INSTRUCTIONS.len());
    for needle in [
        "set_behaviors",
        "bake_behaviors",
        "REPLACES",
        "Key edits write the pose without them",
        "GPU preview draws a clip with them through FFmpeg",
    ] {
        assert!(super::EDITING_GUIDE.contains(needle), "the guide does not say `{needle}`");
    }
    assert!(description("get_timeline_state").contains("`behaviors`"));

    let parse = |v: serde_json::Value| serde_json::from_value::<super::SetBehaviorsParams>(v);
    let ok = parse(serde_json::json!({
        "clip_ids": ["c"],
        "behaviors": [
            {"kind": "sway", "amount": 5, "speed": 0.5},
            {"kind": "shake", "amount": 0.01, "speed": 8, "seed": 7},
            {"kind": "drift", "speed": 0.02},
        ],
    }))
    .unwrap();
    assert_eq!(ok.behaviors.len(), 3);
    assert_eq!(
        ok.behaviors[2],
        kerf_core::Behavior::Drift {
            speed: 0.02,
            angle: 0.0,
            phase: 0.0
        },
        "angle and phase default to 0"
    );
    for bad in [
        serde_json::json!({"clip_ids": ["c"], "behaviors": [{"kind": "wobble", "amount": 1}]}),
        serde_json::json!({"clip_ids": ["c"], "behaviors": [{"kind": "sway", "speed": 1}]}),
        serde_json::json!({"clip_ids": ["c"]}),
        serde_json::json!({"behaviors": []}),
    ] {
        assert!(parse(bad.clone()).is_err(), "{bad}");
    }
}

/// The agent gives a clip behaviours: with review on they are held back as a proposal (the live cut is
/// untouched), the agent's own reads show them on the clip, the webview is told once per edit, and a
/// refused call changes nothing and says so as the caller's mistake. A bake turns them into keys.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_sets_and_bakes_behaviors_and_it_stages() {
    use rmcp::model::ErrorCode;
    let (server, shared) = headless_server();
    let (clip, other, long, voice) = {
        let project = shared.lock().unwrap();
        let video = footage(&project, "a.mp4", 120.0, "2026-01-01T00:00:00Z");
        let clip = project.add_clip_to_timeline(video.id, None, 0.0, 10.0, Some(0.0)).unwrap();
        let other = project.add_clip_to_timeline(video.id, None, 20.0, 24.0, Some(10.0)).unwrap();
        let long = project.add_clip_to_timeline(video.id, None, 30.0, 80.0, Some(14.0)).unwrap();
        let sound: kerf_core::Asset = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "path": "/footage/voice.wav", "name": "voice.wav", "duration": 5.0,
            "streams": [{ "index": 0, "kind": "audio", "codec": "pcm_s16le", "sample_rate": 48000, "channels": 2 }],
            "imported_at": "2026-01-01T00:00:02Z",
        }))
        .unwrap();
        project.insert_or_get_asset(&sound).unwrap();
        let voice = project.add_clip_to_timeline(sound.id, None, 0.0, 5.0, Some(0.0)).unwrap();
        (clip, other, long, voice)
    };
    let changed = std::sync::Arc::new(AtomicI64::new(0));
    {
        let changed = changed.clone();
        server.app.listen("project-changed", move |_| {
            changed.fetch_add(1, Ordering::Relaxed);
        });
    }
    let set = |ids: Vec<String>, behaviors: serde_json::Value| super::SetBehaviorsParams {
        clip_ids: ids,
        behaviors: serde_json::from_value(behaviors).unwrap(),
    };
    let sway = serde_json::json!({"kind": "sway", "amount": 5.0, "speed": 0.5});
    let shake = serde_json::json!({"kind": "shake", "amount": 0.01, "speed": 8.0, "seed": 7});

    let done = answer(
        server
            .set_behaviors(Parameters(set(
                vec![clip.id.to_string(), other.id.to_string()],
                serde_json::json!([sway, shake]),
            )))
            .unwrap(),
    );
    let clips = done["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0]["clip_id"], clip.id.to_string());
    assert_eq!(
        clips[0]["behaviors"][0],
        serde_json::json!({"kind": "sway", "amount": 5.0, "speed": 0.5, "phase": 0.0})
    );
    assert_eq!(clips[0]["behaviors"][1]["seed"], 7);
    assert_eq!(changed.load(Ordering::Relaxed), 1, "the webview is told after the edit");
    {
        let user = crate::lock_user(&shared);
        assert!(
            user.timeline().unwrap().clip(clip.id).unwrap().behaviors.is_empty(),
            "the live cut is the user's until they accept"
        );
        let proposed = user
            .staged_timeline()
            .unwrap()
            .expect("the behaviours are held back as a proposal");
        assert_eq!(proposed.clip(clip.id).unwrap().behaviors.len(), 2);
        assert_eq!(proposed.clip(other.id).unwrap().behaviors.len(), 2);
    }

    // The agent's own read of the cut shows them on the clip, which is how it reads the list it replaces.
    let state = answer(
        server
            .get_timeline_state(Parameters(
                serde_json::from_value::<super::TimelineStateParams>(serde_json::json!({})).unwrap(),
            ))
            .unwrap(),
    );
    let seen = state["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["clips"].as_array().unwrap().iter())
        .find(|c| c["id"] == clip.id.to_string())
        .expect("the clip is in the state");
    assert_eq!(seen["behaviors"][0]["kind"], "sway");
    assert_eq!(seen["behaviors"][1]["kind"], "shake");

    // A shorter list replaces the longer; [] clears.
    server
        .set_behaviors(Parameters(set(vec![other.id.to_string()], serde_json::json!([sway]))))
        .unwrap();
    server
        .set_behaviors(Parameters(set(vec![other.id.to_string()], serde_json::json!([]))))
        .unwrap();
    {
        let user = crate::lock_user(&shared);
        let proposed = user.staged_timeline().unwrap().unwrap();
        assert!(proposed.clip(other.id).unwrap().behaviors.is_empty());
        assert_eq!(proposed.clip(clip.id).unwrap().behaviors.len(), 2);
    }
    assert_eq!(changed.load(Ordering::Relaxed), 3);

    // Mistakes are the caller's, and leave nothing behind.
    let before = changed.load(Ordering::Relaxed);
    let staged_before = serde_json::to_value(crate::lock_user(&shared).staged_timeline().unwrap()).unwrap();
    let nine = serde_json::Value::Array(vec![sway.clone(); kerf_core::MAX_BEHAVIORS + 1]);
    for (name, call) in [
        ("not an id", set(vec!["a.mp4".into()], serde_json::json!([sway]))),
        (
            "a clip that is not there",
            set(
                vec![clip.id.to_string(), uuid::Uuid::new_v4().to_string()],
                serde_json::json!([sway]),
            ),
        ),
        ("nothing named", set(vec![], serde_json::json!([sway]))),
        (
            "a sound",
            set(vec![clip.id.to_string(), voice.id.to_string()], serde_json::json!([sway])),
        ),
        (
            "a sway past its angle",
            set(
                vec![clip.id.to_string()],
                serde_json::json!([{"kind": "sway", "amount": 181.0, "speed": 1.0}]),
            ),
        ),
        (
            "a breath that would reach nothing",
            set(
                vec![clip.id.to_string()],
                serde_json::json!([{"kind": "breathe", "amount": 0.95, "speed": 1.0}]),
            ),
        ),
        (
            "a speed past thirty hertz",
            set(
                vec![clip.id.to_string()],
                serde_json::json!([{"kind": "shake", "amount": 0.01, "speed": 31.0}]),
            ),
        ),
        ("more than a clip carries", set(vec![clip.id.to_string()], nine)),
    ] {
        let err = server.set_behaviors(Parameters(call)).unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS, "{name}: {}", err.message);
    }
    assert_eq!(
        changed.load(Ordering::Relaxed),
        before,
        "a refused call tells the webview nothing"
    );
    assert_eq!(
        serde_json::to_value(crate::lock_user(&shared).staged_timeline().unwrap()).unwrap(),
        staged_before,
        "and changes nothing in the proposal"
    );

    // Baking: the motion becomes keys on the numbers it moved, at the frame grid, and the behaviours go.
    let bake = |ids: Vec<String>, fps| super::BakeBehaviorsParams { clip_ids: ids, fps };
    let baked = answer(
        server
            .bake_behaviors(Parameters(bake(vec![clip.id.to_string()], Some(24.0))))
            .unwrap(),
    );
    let report = &baked["baked"][0];
    assert_eq!(report["clip_id"], clip.id.to_string());
    assert_eq!(report["fps"], 24.0);
    assert_eq!(
        report["properties"],
        serde_json::json!(["pos_x", "pos_y", "rotation"]),
        "a sway turns it, a shake moves it"
    );
    assert_eq!(report["keys"], 241, "a key a frame of a 10 s clip at 24 fps, and its end");
    {
        let user = crate::lock_user(&shared);
        let proposed = user.staged_timeline().unwrap().unwrap();
        let c = proposed.clip(clip.id).unwrap();
        assert!(c.behaviors.is_empty());
        assert_eq!(c.property_keys(kerf_core::Property::Rotation).len(), 241);
    }
    for (name, call) in [
        ("a clip with none left", bake(vec![clip.id.to_string()], None)),
        ("a rate past the top", bake(vec![clip.id.to_string()], Some(5000.0))),
    ] {
        let err = server.bake_behaviors(Parameters(call)).unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS, "{name}: {}", err.message);
    }
    server
        .set_behaviors(Parameters(set(vec![long.id.to_string()], serde_json::json!([sway]))))
        .unwrap();
    let err = server
        .bake_behaviors(Parameters(bake(vec![long.id.to_string()], Some(30.0))))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    assert!(
        err.message.contains("split the clip"),
        "the refusal says what to do about it: {}",
        err.message
    );
    assert_eq!(
        crate::lock_user(&shared)
            .staged_timeline()
            .unwrap()
            .unwrap()
            .clip(long.id)
            .unwrap()
            .behaviors
            .len(),
        1,
        "a refused bake keeps the behaviours"
    );
}
