//! The server's prompts: the guide's workflows as short step lists a user starts by name (Claude
//! Code shows them as `/mcp__kerf__<name>`). Each names its tools in backticks, and a test checks every
//! such name is a registered tool, so a renamed tool cannot leave a prompt pointing at nothing.

use std::collections::HashMap;

use rmcp::model::{GetPromptResult, Prompt, PromptArgument, PromptMessage, Role};
use rmcp::ErrorData as McpError;

struct Arg {
    name: &'static str,
    description: &'static str,
    required: bool,
}

struct Def {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    args: &'static [Arg],
    text: fn(&Args) -> String,
}

/// A prompt's arguments as the client sent them, blank ones dropped.
struct Args(HashMap<String, String>);

impl Args {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    fn or<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        self.get(name).unwrap_or(default)
    }
}

const PROMPTS: &[Def] = &[
    Def {
        name: "rough_cut_from_transcript",
        title: "Rough cut from a transcript",
        description: "Transcribe, choose the lines that tell the story, assemble them and review the cut.",
        args: &[
            Arg {
                name: "assets",
                description: "Asset ids to cut from, comma-separated; omitted, the asset selected in Kerf's library",
                required: false,
            },
            Arg {
                name: "target_length",
                description: "How long the cut should run, e.g. 60 seconds",
                required: false,
            },
            Arg {
                name: "brief",
                description: "What the cut is for or should say",
                required: false,
            },
        ],
        text: rough_cut,
    },
    Def {
        name: "podcast_cleanup",
        title: "Podcast cleanup",
        description: "Remove silences and filler words, even out the voices and deliver at a loudness target.",
        args: &[
            Arg {
                name: "asset",
                description: "The recording's asset id; omitted, the asset selected in Kerf's library",
                required: false,
            },
            Arg {
                name: "loudness",
                description:
                    "Export loudness preset: spotify or youtube (-14 LUFS), apple (-16) or broadcast (-23); default spotify",
                required: false,
            },
        ],
        text: podcast_cleanup,
    },
    Def {
        name: "social_reel",
        title: "Social reel",
        description:
            "A vertical (or square) cut for a platform: delivery frame, smart crop, captions last, platform check, export.",
        args: &[
            Arg {
                name: "platform",
                description: "Where it is going, e.g. Instagram Reels, TikTok or YouTube Shorts",
                required: true,
            },
            Arg {
                name: "length",
                description: "How long it should run; default under 60 seconds",
                required: false,
            },
            Arg {
                name: "aspect",
                description: "The frame shape: 9:16 (default), 1:1, 4:5 or 16:9",
                required: false,
            },
        ],
        text: social_reel,
    },
    Def {
        name: "music_bed",
        title: "Music bed",
        description: "Fit a music track (or compose one) under the cut, duck it under speech and check the levels.",
        args: &[
            Arg {
                name: "music",
                description: "The asset id of a music track to use; omitted, compose one",
                required: false,
            },
            Arg {
                name: "style",
                description: "A style to compose in (music_status lists them); ignored with music",
                required: false,
            },
        ],
        text: music_bed,
    },
    Def {
        name: "sound_design",
        title: "Sound design",
        description: "Put whooshes on the cuts, pops on titles and risers into drops: suggest where, review, add, check the mix.",
        args: &[
            Arg {
                name: "density",
                description: "How many: subtle (transitions, titles, drops), balanced (adds the cuts; default) or punchy (every cut and the opening)",
                required: false,
            },
            Arg {
                name: "range",
                description: "Only part of the cut, as start-end in seconds, e.g. 0-30; omitted, the whole cut",
                required: false,
            },
        ],
        text: sound_design,
    },
    Def {
        name: "deliver",
        title: "Deliver",
        description: "Final checks on the cut (gaps, frame rate, platform, loudness, captions), then export.",
        args: &[
            Arg {
                name: "output",
                description: "The absolute path to export to; omitted, ask the user",
                required: false,
            },
            Arg {
                name: "platform",
                description: "Where it is going, e.g. YouTube or Instagram Reels",
                required: false,
            },
        ],
        text: deliver,
    },
];

const GUIDE_FIRST: &str = "Call `get_editing_guide` first if you have not this session, and check `get_ripple_mode` (pass \
     ripple: false on an edit that must not push later clips).";

fn which_assets(args: &Args, name: &str) -> String {
    match args.get(name) {
        Some(ids) => format!("The media: {ids} (`get_asset_metadata` says how long each is and what is analysed)."),
        None => "Find the media: `get_editor_context` gives the asset selected in the library (selected_asset_id); \
                 otherwise `list_assets` with fields: \"compact\", and ask the user when it is not clear which."
            .to_string(),
    }
}

fn rough_cut(args: &Args) -> String {
    let length = args.or("target_length", "the length the material supports");
    let brief = args.get("brief").map(|b| format!(" The brief: {b}.")).unwrap_or_default();
    format!(
        "Make a rough cut from the transcript, about {length} long.{brief}\n\n\
         1. {GUIDE_FIRST}\n\
         2. {assets}\n\
         3. `analyze_asset` with steps [\"transcript\", \"silence\"] where the transcript is not done yet (`list_assets` \
         shows the analysis per kind). Transcribing is slow: pass background: true and follow `job_status`.\n\
         4. Read it with `get_transcript` (page with offset / next_offset; each line has its speaker). For one voice of an \
         interview, `speaker_ranges` gives that speaker's source ranges.\n\
         5. Choose the lines that tell the story in about {length}: note each line's source start and end, in the order \
         the cut will play.\n\
         6. Assemble: `cut_clip` appends each source range to the first unlocked track, in order \
         (`add_clip_to_timeline` places one elsewhere). Tighten with `trim` and `cut_clip_range` (a word's start/end from \
         get_transcript with words: true; cut the latest first).\n\
         7. Review: `timeline_summary` (the length, and gaps, which play as black), `preview_timeline` at a few cuts, \
         `staged_diff` for what you hand over.\n\
         8. Captions, if wanted, come last: `generate_captions`.\n\
         9. Say what you kept and why. With review on your edits are a proposal the user accepts in Kerf; finish a \
         claimed task with `complete_task`.",
        assets = which_assets(args, "assets"),
    )
}

fn podcast_cleanup(args: &Args) -> String {
    let loudness = args.or("loudness", "spotify");
    format!(
        "Clean up a podcast recording: drop the silences and filler words, even out the voices, deliver at the \
         {loudness} loudness preset.\n\n\
         1. {GUIDE_FIRST}\n\
         2. {asset}\n\
         3. `analyze_asset` with steps [\"silence\", \"transcript\", \"loudness\"] (background: true and `job_status` for \
         a long recording).\n\
         4. Silences: `remove_silence` appends the asset's non-silent spans as clips after what its track already holds, \
         as one revision, so clear the recording's old clip off that track first (`remove_clips`).\n\
         5. Filler words: `find_filler_words` gives each hit's source start/end; cut each with `cut_clip_range` on the \
         clip that shows it (`get_timeline_state` has the clips' source ranges), latest first so earlier times do not \
         shift. Speech models often leave fillers out, so few hits proves nothing.\n\
         6. Voices: `get_levels` measures the mix and each track. Give each voice clip one chain with \
         `set_audio_effects`, in this order: a highpass (~80 Hz) for rumble, a denoise for hiss and room tone \
         (reduction_db ~12, floor_db -50, raised toward -40 / -35 for a noisy room — too high eats speech — or \
         track_noise true when the noise moves), then a compressor for an uneven voice and a gate last; \
         `set_track_volume` balances speakers on separate tracks.\n\
         7. Loudness: `set_master_limiter` on and `set_master_true_peak`, then `get_levels` again; `set_master_volume` \
         fixes a mix that is still hot or quiet.\n\
         8. `timeline_summary` for gaps, `staged_diff` for what you hand over, then `export` to an absolute path with \
         options.loudness \"{loudness}\" and read the report it returns.",
        asset = which_assets(args, "asset"),
    )
}

fn social_reel(args: &Args) -> String {
    let platform = args.or("platform", "social");
    let length = args.or("length", "under 60 seconds");
    let aspect = args.or("aspect", "9:16");
    let frame = match aspect {
        "9:16" => "1080x1920",
        "1:1" => "1080x1080",
        "4:5" => "1080x1350",
        "16:9" => "1920x1080",
        _ => "that shape",
    };
    format!(
        "Cut a {aspect} piece for {platform}, {length}.\n\n\
         1. {GUIDE_FIRST}\n\
         2. `set_delivery_format` first ({frame}, with an fps such as 30), so every preview shows what ships.\n\
         3. Find the moments with `skim_asset` and build the cut (`cut_clip`, `trim`, `ripple_delete`) to {length}, the \
         strongest moment in the first seconds.\n\
         4. `smart_crop` frames each shot for the frame; check with `preview_timeline` and move a shot with \
         `set_transform`.\n\
         5. Sound: music under it if wanted (the music_bed prompt), then `get_levels`: about -14 LUFS and a true peak \
         under -1 dBTP (`set_master_limiter`, `set_master_true_peak`).\n\
         6. Captions LAST, once the cut is settled: `generate_captions` with style word_punch; re-run it after any later \
         edit.\n\
         7. `platform_check` (with range when the piece is part of a longer timeline), and fix its errors and warnings.\n\
         8. `export_cover` for the thumbnail, then `export` to an absolute path, or `export_variants` with formats such \
         as [\"9:16\", \"1:1\", \"16:9\"] when it goes to several places: one pass, each shot framed per shape, each file \
         reported with what it is ready for."
    )
}

fn music_bed(args: &Args) -> String {
    let source = match args.get("music") {
        Some(id) => format!(
            "2. The track {id}: `analyze_asset` it with steps [\"rhythm\"] (the beat grid), put it on an audio track with \
             `add_asset_audio`, then `plan_music_fit` and `fit_music` fit it to the picture on bar lines \
             (`get_music_structure` shows the phrases)."
        ),
        None => format!(
            "2. Compose it{style}: `music_status` lists the styles. Write a score and `compose_music` it with align \
             {{to: \"cuts\"}} so its tempo and hits land on the cuts; `fit_music` if it runs the wrong length. To change \
             it, edit the score (`list_assets` shows it as music_score) and compose again with replace_clip_id.",
            style = args.get("style").map(|s| format!(" in the {s} style")).unwrap_or_default(),
        ),
    };
    format!(
        "Put a music bed under the cut.\n\n\
         1. Cut the picture first: the music is fitted to it. {GUIDE_FIRST}\n\
         {source}\n\
         3. Mix: `set_track_volume` about 0.3 on the music track under speech, and `set_track_duck` on it plus \
         `set_master_duck` (depth_db -12) to dip it under dialogue.\n\
         4. Check: `get_levels` (about -14 LUFS for social, a true peak under -1 dBTP) and `get_band_balance` over a \
         range with speech: music heavy in mid / high_mid fights the voice; fix with `set_audio_effects` or a lower \
         fader.\n\
         5. `staged_diff` shows what you hand over."
    )
}

fn sound_design(args: &Args) -> String {
    let density = args.or("density", "balanced");
    let range = match args.get("range") {
        Some(r) => format!(" Only {r} seconds of the cut: pass range {{start, end}} to `suggest_sound_effects`."),
        None => String::new(),
    };
    format!(
        "Add sound effects to the cut: whooshes on the cuts, pops on titles, a riser into a drop, at {density} \
         density.{range}\n\n\
         1. Settle the picture first, since a later edit moves the cuts. {GUIDE_FIRST}\n\
         2. `analyze_asset` the footage where it is not analysed yet (steps scenes, transcript and rhythm): speech lowers \
         the effects under it and music drops get a riser. `list_sound_effects` says what each of the sounds is for.\n\
         3. `suggest_sound_effects` with density \"{density}\" (and range, categories or kinds to narrow it). It adds \
         nothing; each suggestion says what the moment is and why.\n\
         4. Review the list: keep the moments that help the story, drop the ones that crowd a line of speech or repeat, \
         and change a kind, a gain (volume_db) or a seed where the first sound is not right. Each placement is \
         exactly what `add_sound_effects` takes, and its \"at\" is where the HIT lands, not where the clip starts.\n\
         5. `add_sound_effects` with the placements you kept: one call, one edit, on the FX tracks.\n\
         6. Check the mix: `get_levels` (about -14 LUFS for social, true peak under -1 dBTP) and, over a stretch of \
         speech, `get_band_balance`; effects that cover a word get a lower fader with `set_volume`, or a new \
         spec with `update_sound_effect`. `preview_timeline` at a few effects shows the picture they sit on.\n\
         7. `staged_diff` shows what you hand over; say which moments got a sound and why."
    )
}

fn deliver(args: &Args) -> String {
    let platform = args.get("platform");
    let output = args.or("output", "an absolute path the user names (ask)");
    format!(
        "Run the final checks on the cut and export it{to}.\n\n\
         1. `staged_diff`: export renders the cut you are proposing, staged edits included; with review on, tell the \
         user the proposal still waits for their accept in Kerf.\n\
         2. `timeline_summary`: no unintended gaps (black picture), no media_notes with a missing source, and \
         export_fps — set the rate with `set_delivery_format` (fps) when the footage mixes rates.\n\
         3. `platform_check`{check} and fix what it reports.\n\
         4. `get_levels`: about -14 LUFS and a true peak under -1 dBTP for social; fix with `set_master_volume`, \
         `set_master_limiter`, `set_master_true_peak` or an export loudness preset.\n\
         5. If the cut changed after its captions were made, run `generate_captions` (or `import_captions`) again.\n\
         6. `preview_timeline` at the first frame, the last and a few cuts.\n\
         7. `export` to {output} (background: true and `job_status` for a long render), and check the width, height, \
         fps and report it answers. `export_cover` writes the thumbnail, `export_captions` a subtitle file.\n\
         8. Say where the files are; finish a claimed task with `complete_task`.",
        to = platform.map(|p| format!(" for {p}")).unwrap_or_default(),
        check = platform.map(|p| format!(" (for {p})")).unwrap_or_default(),
    )
}

/// Every prompt, for `prompts/list`.
pub(super) fn list() -> Vec<Prompt> {
    PROMPTS
        .iter()
        .map(|def| {
            let args = def
                .args
                .iter()
                .map(|a| {
                    PromptArgument::new(a.name)
                        .with_description(a.description)
                        .with_required(a.required)
                })
                .collect();
            Prompt::new(def.name, Some(def.description), Some(args)).with_title(def.title)
        })
        .collect()
}

/// `prompts/get`: an unknown prompt, an argument it does not take or a missing required one is
/// invalid params.
pub(super) fn get(name: &str, arguments: Option<&rmcp::model::JsonObject>) -> Result<GetPromptResult, McpError> {
    let def = PROMPTS.iter().find(|d| d.name == name).ok_or_else(|| {
        let names: Vec<&str> = PROMPTS.iter().map(|d| d.name).collect();
        McpError::invalid_params(format!("no prompt `{name}`; the prompts are {}", names.join(", ")), None)
    })?;
    let mut args = HashMap::new();
    for (key, value) in arguments.into_iter().flatten() {
        if !def.args.iter().any(|a| a.name == key) {
            let takes: Vec<&str> = def.args.iter().map(|a| a.name).collect();
            return Err(McpError::invalid_params(
                format!("`{name}` takes no argument `{key}`; it takes {}", takes.join(", ")),
                None,
            ));
        }
        let value = match value {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        if !value.is_empty() {
            args.insert(key.clone(), value);
        }
    }
    if let Some(missing) = def.args.iter().find(|a| a.required && !args.contains_key(a.name)) {
        return Err(McpError::invalid_params(
            format!("`{name}` needs `{}`: {}", missing.name, missing.description),
            None,
        ));
    }
    let text = (def.text)(&Args(args));
    Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).with_description(def.description))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{ContentBlock, ErrorCode};

    fn object(pairs: &[(&str, &str)]) -> rmcp::model::JsonObject {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    fn text_of(result: &GetPromptResult) -> String {
        match &result.messages[0].content {
            ContentBlock::Text(t) => t.text.clone(),
            other => panic!("not text: {other:?}"),
        }
    }

    /// The names a prompt puts in backticks.
    fn tools_named(text: &str) -> Vec<&str> {
        text.split('`')
            .skip(1)
            .step_by(2)
            .filter(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
            .collect()
    }

    /// Every way a prompt renders: with only its required arguments and with all of them.
    fn renders() -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        for def in PROMPTS {
            let required: Vec<(&str, &str)> = def.args.iter().filter(|a| a.required).map(|a| (a.name, "x")).collect();
            let all: Vec<(&str, &str)> = def.args.iter().map(|a| (a.name, "x")).collect();
            for args in [required, all] {
                out.push((def.name, text_of(&get(def.name, Some(&object(&args))).unwrap())));
            }
        }
        out
    }

    #[test]
    fn every_tool_a_prompt_names_is_registered() {
        let registered: Vec<String> = super::super::router().list_all().iter().map(|t| t.name.to_string()).collect();
        for (prompt, text) in renders() {
            let named = tools_named(&text);
            assert!(named.len() >= 5, "{prompt} names too few tools: {named:?}");
            for tool in named {
                assert!(
                    registered.iter().any(|t| t == tool),
                    "{prompt} names `{tool}`, which is no tool"
                );
            }
            assert!(text.len() < 2400, "{prompt} is {} characters", text.len());
        }
    }

    #[test]
    fn the_list_has_every_prompt_with_its_arguments() {
        let listed = list();
        let names: Vec<&str> = listed.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "rough_cut_from_transcript",
                "podcast_cleanup",
                "social_reel",
                "music_bed",
                "sound_design",
                "deliver"
            ]
        );
        let reel = &listed[2];
        let platform = reel
            .arguments
            .as_ref()
            .unwrap()
            .iter()
            .find(|a| a.name == "platform")
            .unwrap();
        assert_eq!(platform.required, Some(true));
        assert!(listed.iter().all(|p| p.title.is_some() && p.description.is_some()));
    }

    #[test]
    fn arguments_fill_the_text() {
        let reel = text_of(&get("social_reel", Some(&object(&[("platform", "TikTok"), ("aspect", "1:1")]))).unwrap());
        assert!(reel.contains("for TikTok") && reel.contains("1080x1080") && reel.contains("under 60 seconds"));
        let bed = text_of(&get("music_bed", Some(&object(&[("music", "abc")]))).unwrap());
        assert!(bed.contains("The track abc") && !bed.contains("compose_music"));
        let composed = text_of(&get("music_bed", None).unwrap());
        assert!(composed.contains("compose_music") && composed.contains("align {to: \"cuts\"}"));
        let social = text_of(&get("social_reel", Some(&object(&[("platform", "Reels")]))).unwrap());
        let captions = social.find("generate_captions").unwrap();
        assert!(captions > social.find("smart_crop").unwrap() && captions < social.find("platform_check").unwrap());
    }

    #[test]
    fn sound_design_suggests_then_adds_then_checks_the_mix() {
        let design = text_of(&get("sound_design", Some(&object(&[("density", "punchy"), ("range", "0-30")]))).unwrap());
        assert!(design.contains("punchy density") && design.contains("\"punchy\"") && design.contains("0-30"));
        let whole = text_of(&get("sound_design", None).unwrap());
        assert!(whole.contains("balanced") && !whole.contains("Only "), "{whole}");
        let at = |needle: &str| whole.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let (suggest, add, levels) = (at("`suggest_sound_effects`"), at("`add_sound_effects`"), at("`get_levels`"));
        assert!(suggest < add && add < levels, "suggest, review, add, check the mix");
        assert!(whole.contains("HIT"));
    }

    #[test]
    fn an_unknown_prompt_or_argument_and_a_missing_required_one_are_invalid_params() {
        let unknown = get("make_it_good", None).unwrap_err();
        assert_eq!(unknown.code, ErrorCode::INVALID_PARAMS);
        assert!(unknown.message.contains("social_reel"), "{}", unknown.message);
        let missing = get("social_reel", None).unwrap_err();
        assert_eq!(missing.code, ErrorCode::INVALID_PARAMS);
        assert!(missing.message.contains("`platform`"), "{}", missing.message);
        let blank = get("social_reel", Some(&object(&[("platform", "  ")]))).unwrap_err();
        assert_eq!(blank.code, ErrorCode::INVALID_PARAMS);
        let extra = get("deliver", Some(&object(&[("out_file", "/tmp/a.mp4")]))).unwrap_err();
        assert_eq!(extra.code, ErrorCode::INVALID_PARAMS);
        assert!(
            extra.message.contains("`out_file`") && extra.message.contains("output"),
            "{}",
            extra.message
        );
    }
}
