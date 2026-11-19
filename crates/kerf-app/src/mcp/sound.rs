//! Sound the project did not have: voiceover and sound effects.

use super::*;

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct VoiceoverParams {
    #[schemars(
        description = "The script to read aloud, in English. Sentences are read one at a time; a blank line is a longer pause (a paragraph break)."
    )]
    text: String,
    #[schemars(
        description = "Kokoro voice id (default af_heart). American: af_heart, af_bella, af_nicole, af_sarah, af_sky, af_nova, am_michael, am_fenrir, am_puck, am_adam, am_eric, am_liam, am_onyx; British: bf_emma, bf_isabella, bf_alice, bf_lily, bm_george, bm_fable, bm_lewis, bm_daniel. voiceover_status lists them all."
    )]
    voice: Option<String>,
    #[schemars(description = "Speaking rate, 0.5–2.0 (default 1.0)")]
    speed: Option<f64>,
    #[schemars(description = "Audio track to place it on; omitted, it goes on the `VO` track (created on first use)")]
    track_id: Option<String>,
    #[schemars(
        description = "Timeline time to start at, in seconds; omitted, it is appended after what the track already holds"
    )]
    timeline_start: Option<f64>,
    #[schemars(
        description = "Caption the cut afterwards in this style (`lines` or `word_punch`), the same as generate_captions. Omit to leave captions alone."
    )]
    caption_style: Option<CaptionStyle>,
    #[schemars(
        description = "Ripple override for placing the narration. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, narration placed over footage already on the track pushes that footage and everything after it later by its length; appending, or filling free space, moves nothing either way. false places it without moving anything. Tracks ripple independently. Ripple is an attempt, not a guarantee: it never moves a locked track (and an edit to a clip ON a locked track is refused outright), and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "With caption_style: most words on one caption line (lines: 4, word_punch: 1); see generate_captions"
    )]
    caption_max_words: Option<usize>,
    #[schemars(description = "With caption_style: most characters on one caption line (default 28)")]
    caption_max_chars: Option<usize>,
    #[schemars(
        description = "With caption_style: vertical position as a fraction of frame height, 0 = top (lines: 0.88, word_punch: 0.72)"
    )]
    caption_pos_y: Option<f64>,
    #[schemars(description = "With caption_style: font height as a fraction of frame height (lines: 0.05, word_punch: 0.11)")]
    caption_size: Option<f64>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AddSoundEffectsParams {
    #[schemars(
        description = "The effects to add, 1 to 256, each {kind, at, ...}: `kind` is a sound from list_sound_effects; `at` is the timeline second the effect's HIT lands on — a whoosh's peak (about halfway through it), an impact's first transient, the END of a riser — so put `at` on the picture event (the cut, the title's first frame, the drop) and the clip starts earlier by itself (a start less than 10 ms before 0 is put at 0 and the hit lands that late; a deeper one has its head cut off, with a 4 ms fade-in; an effect that would keep under 20 ms of sound is refused with the earliest time it can land). Optional per effect: duration_s (clamped to the kind's range), pitch (semitones, -12 to 12), intensity (0-1, default 0.7: brightness and aggression), tail (0-1, default 0.3: reverb / echo after the sound), reverse (play it backwards; the hit mirrors to the end), seed (a different integer is a different take of the same sound, 0 to 9007199254740991 — vary it down a run of cuts so they do not repeat), volume_db (the clip's gain, -60 to 12, default 0; levels are already balanced per kind, so a click sits quieter than a boom; under speech about -8, in the clear about -3), track_id. Without a track_id an effect goes on the first unlocked audio track named FX (then FX 2, FX 3, …) where it overlaps no clip, and a new lane is made when none fits, so effects that overlap (a riser into its impact) are fine. With track_id it must be an unlocked audio track with room for it. An unknown or misspelled field is an error naming it. suggest_sound_effects' `placement` objects are exactly this shape: pass them as they are."
    )]
    pub(super) effects: Vec<kerf_core::SfxPlacement>,
    #[schemars(
        description = "Ripple override for placing the clips, and for the check made before anything renders. Omitted, the project's ripple mode applies (get_ripple_mode). It matters only for an effect placed on a named track (track_id) on top of other clips: with ripple on they are pushed later (a start inside a clip is still refused, ripple never splits a clip); with it off the request is refused. Effects on the FX lanes never need it: a lane with no room is skipped and a new one made. Ripple is an attempt: it never moves a locked track (placing ON one is refused)."
    )]
    pub(super) ripple: Option<bool>,
}

#[derive(Debug, schemars::JsonSchema)]
pub(super) struct UpdateSoundEffectParams {
    #[schemars(
        description = "UUID of the effect's clip on the timeline (add_sound_effects' answer has it as `clip_id`). Refused for a clip that is not a synthesized sound effect, or on a locked or video track."
    )]
    clip_id: String,
    #[serde(flatten)]
    pub(super) spec: kerf_core::SfxSpec,
    #[schemars(
        description = "The clip's new gain in dB, -60 to 12. Omitted, the clip keeps the gain it has (the fader, not the render, so a changed kind does not change how loud it sits)."
    )]
    pub(super) volume_db: Option<f64>,
    #[schemars(
        description = "Ripple override. Omitted, the project's ripple mode applies (get_ripple_mode). It matters when the new render is longer and would run into the next clip of its track: with ripple on the later clips are pushed, with it off the request is refused before anything is rendered or changed."
    )]
    pub(super) ripple: Option<bool>,
}

impl<'de> serde::Deserialize<'de> for UpdateSoundEffectParams {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // `deny_unknown_fields` does not work through `flatten`, so the spec's own fields are handed
        // to `SfxSpec` (which refuses an unknown one, naming it) once the call's are taken, as
        // `SfxPlacement` does.
        #[derive(serde::Deserialize)]
        struct Wire {
            clip_id: String,
            #[serde(default)]
            volume_db: Option<f64>,
            #[serde(default)]
            ripple: Option<bool>,
            #[serde(flatten)]
            spec: serde_json::Map<String, serde_json::Value>,
        }
        let wire = Wire::deserialize(de)?;
        let spec = kerf_core::SfxSpec::deserialize(serde_json::Value::Object(wire.spec)).map_err(serde::de::Error::custom)?;
        Ok(Self {
            clip_id: wire.clip_id,
            spec,
            volume_db: wire.volume_db,
            ripple: wire.ripple,
        })
    }
}

#[tool_router(router = tool_router_sound, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Whether voiceovers can be generated on this machine, whether the voice model is downloaded yet (the first voiceover fetches ~100 MB), and the voices on offer with their accent and gender."
    )]
    fn voiceover_status(&self) -> Result<String, McpError> {
        json(&kerf_core::voiceover_status())
    }

    #[tool(
        description = "Read a script aloud with the Kokoro text-to-speech model and put the narration on the timeline: on the `VO` audio track unless track_id names another, appended after what is already there unless timeline_start says where. The audio is English only. The first call downloads the voice model (~100 MB) and each voice is fetched on first use; synthesis then takes a few seconds per sentence — send a `progressToken` for progress and cancel the request to stop it. Each sentence is synthesized on its own, so the voiceover's transcript is its script with exact timings: pass caption_style to caption the cut straight away (or run generate_captions later), and never analyze_asset it for a transcript. The generated audio lands for the user immediately like an import; placing it on the timeline (and the captions) is an ordinary staged edit. `caption_style` plus `caption_max_words` / `caption_max_chars` / `caption_pos_y` / `caption_size` set the look of those captions as in generate_captions; if the narration is placed but the captions fail, the result still succeeds with a `warning`. `track_id` is checked before anything is synthesized. Pass `background: true` for a job_id. Returns the asset (without its per-sentence timings: get_transcript has them) and the clip."
    )]
    async fn generate_voiceover(
        &self,
        Parameters(p): Parameters<VoiceoverParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let track = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        let generation = {
            let project = self.lock();
            // Said before a minute of synthesis, not after it: the track the narration is for must
            // exist, and be an audio track.
            if let Some(t) = track {
                let timeline = project.working_timeline().map_err(core_err)?;
                match timeline.track(t) {
                    None => return Err(core_err(kerf_core::Error::TrackNotFound(t))),
                    Some(found) if found.kind != StreamKind::Audio => {
                        return Err(McpError::invalid_params(
                            format!("`track_id` {t} is a {:?} track; narration goes on an audio track", found.kind)
                                .to_lowercase(),
                            None,
                        ))
                    }
                    Some(_) => {}
                }
            }
            project.generation()
        };
        let voice = p.voice.unwrap_or_else(|| kerf_core::DEFAULT_VOICE.to_string());
        // Any caption option asks for captions; the style alone is the shorthand.
        let captioned = p.caption_style.is_some()
            || p.caption_max_words.is_some()
            || p.caption_max_chars.is_some()
            || p.caption_pos_y.is_some()
            || p.caption_size.is_some();
        let captions = captioned.then(|| CaptionOptions {
            style: p.caption_style.unwrap_or_default(),
            max_words: p.caption_max_words,
            max_chars: p.caption_max_chars,
            pos_y: p.caption_pos_y,
            size: p.caption_size,
        });
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on a voiceover ends an agent's too (`begin_voiceover`).
        let gui_stop = self.app.state::<crate::AppState>().begin_voiceover();
        self.long_tool("generate_voiceover", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let placed = blocking(move || {
                let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
                    let message = match &detail {
                        Some(d) => format!("{stage}: {d}"),
                        None => stage.to_string(),
                    };
                    run.progress(fraction, message);
                    // The GUI shows the same progress it shows for its own voiceovers.
                    let _ = app.emit(
                        "voiceover-progress",
                        crate::VoiceoverProgressEvent {
                            stage: stage.to_string(),
                            fraction,
                            detail,
                        },
                    );
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let asset = Project::synthesize_voiceover(&p.text, &voice, p.speed.unwrap_or(1.0), &mut on_progress, &stop)
                    .map_err(core_err)?;
                // Landing it is an edit of the cut, so it goes into the proposal.
                let project = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&project, generation, "the narration was being made")?;
                let (asset, clip) = project
                    .with_ripple(p.ripple, |project| project.place_voiceover(&asset, track, p.timeline_start))
                    .map_err(core_err)?;
                // The narration is down. Captions failing after that is a warning on a success,
                // not an error that leaves the agent thinking nothing was placed.
                let captions = captions.map(|opts| project.generate_captions(opts).map(|c| c.len()));
                Ok((asset, clip, captions))
            })
            .await;
            // A synthesis that fails places nothing; one that got as far as placing it has
            // changed the proposal whatever came after.
            let (asset, clip, captions) = placed?;
            me.changed();
            let mut answer = serde_json::json!({ "asset": reads::compact_asset(&asset), "clip": clip });
            match captions {
                Some(Ok(count)) => answer["captions"] = serde_json::json!(count),
                Some(Err(e)) => {
                    answer["captions"] = serde_json::json!(0);
                    answer["warning"] = serde_json::json!(format!(
                        "the narration is on the timeline, but captioning it failed: {e}. Fix that and run generate_captions."
                    ));
                }
                None => {}
            }
            json(&answer)
        })
        .await
    }

    #[tool(
        description = "The sound effects Kerf can make, for add_sound_effects: 21 kinds in five categories, each with what it sounds like and where it belongs in an edit, the lengths it may have (`duration_s` is clamped to min..max), where its HIT is and how loud it sits. Transition (whoosh, swoosh, swipe): the move between two shots. Impact (impact, boom, thud, punch): a hit on a moment. Build (riser, downlifter, reverse_swell): tension that climbs to a moment or falls away from one. Ui (pop, click, ding, notify, bubble, typewriter): small sounds for a title or an on-screen thing appearing. Texture (glitch, tape_stop, record_scratch, shutter, heartbeat): colour and character. Every effect has a hit point (`anchor`, and `hit` says it in words): the moment that should land on the picture event — a whoosh peaks part-way through, an impact hits within milliseconds of its start, a riser builds to its very end; `reverse` mirrors it. Kerf synthesizes them itself (nothing to download, no licence, rendered in milliseconds), so the same spec is always the same audio. `level_db` is how far below the others' common peak the kind is meant to sit. Also answers the defaults of the spec fields every effect takes."
    )]
    pub(super) fn list_sound_effects(&self) -> Result<String, McpError> {
        let effects: Vec<serde_json::Value> = kerf_core::sfx_catalogue()
            .iter()
            .map(|preset| {
                let mut row = serde_json::to_value(preset).unwrap_or_default();
                row["hit"] = serde_json::json!(reads::sfx_hit_words(&preset.anchor));
                row
            })
            .collect();
        json(&serde_json::json!({
            "effects": effects,
            "defaults": {
                "pitch": 0.0,
                "intensity": kerf_core::DEFAULT_SFX_INTENSITY,
                "tail": kerf_core::DEFAULT_SFX_TAIL,
                "reverse": false,
                "seed": 0,
                "volume_db": 0.0,
            },
            "max_pitch_semitones": kerf_core::MAX_SFX_PITCH,
            "lanes": "Effects go on the audio tracks named FX, FX 2, FX 3, … (made when needed) unless track_id says another.",
        }))
    }

    #[tool(
        description = "Make sound effects and put them on the timeline in one edit: whooshes on cuts, pops on titles, a riser into a drop, an impact on a reveal. Each of `effects` is {kind, at, …}: `kind` from list_sound_effects, `at` the timeline second the effect's HIT lands on (a whoosh's peak, an impact's first transient, the END of a riser) — Kerf starts the clip earlier by itself, so `at` is the cut or the title's first frame, never the clip's start. The effects are synthesized in milliseconds (nothing to download; the same spec is the same audio), and with no track_id each lands on the first unlocked audio track named FX, FX 2, … where it fits — a new lane is made when none does, so overlapping effects are fine. suggest_sound_effects finds the moments and its `placement` objects are exactly what this takes (cut the picture first, since a later edit moves the cuts); to choose by hand, list_sound_effects says what each sound is for. Everything is checked before anything renders, under the ripple in force, and a refusal names the effect (`effects[2] (riser): …`) and changes nothing; at most 256 per call. The audio lands in the library at once; placing the clips is an ordinary edit, ONE revision, STAGED in your proposal while the user's review is on. Afterwards get_levels (effects sit at their own level, about -3 dB in the clear and -8 dB under speech when suggested) and, to change one, update_sound_effect. Returns {clips: [{clip_id, asset_id, track_id, track_name, kind, at, start, end, volume_db, spec}], assets: [{id, name, duration}]} in the order asked."
    )]
    async fn add_sound_effects(
        &self,
        Parameters(p): Parameters<AddSoundEffectsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let cancel = context.ct.clone();
        self.add_sound_effects_now(p, move || cancel.is_cancelled()).await
    }

    #[tool(
        description = "Change a sound effect already on the timeline: render it again from a new spec and swap the clip's audio. The clip keeps its id, its gain (unless volume_db is given), fades and audio effects, and the timeline position of its HIT (a whoosh's peak, an impact's first transient, a riser's end) — not its start — so a longer, shorter or different version still lands on the same picture moment. Give the WHOLE spec: `kind` plus any of duration_s, pitch, intensity, tail, reverse, seed; what you leave out goes back to its default, not to what the clip has now (add_sound_effects' answer and list_assets' `sound_effect` show the current spec), and a new `kind` swaps a whoosh for a swoosh. The clip's trim and speed are reset to the whole render at normal speed. Refused before anything renders: a clip that is not a synthesized sound effect, a locked track, a render that would run into the next clip of its lane (with ripple on it pushes the later clips; pass ripple to override), and a hit that cannot land. One revision, STAGED in your proposal while the user's review is on. Returns {clip_id, asset_id, track_id, track_name, kind, at (where the hit is), start, end, volume_db, spec}."
    )]
    async fn update_sound_effect(
        &self,
        Parameters(p): Parameters<UpdateSoundEffectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let cancel = context.ct.clone();
        self.update_sound_effect_now(p, move || cancel.is_cancelled()).await
    }

    #[tool(
        description = "Where sound effects would work in the cut you are building. Reads the working timeline (your staged edits included) and the analysis already cached, and answers placements sorted by time: a whoosh or swoosh on each hard cut between two pictures (the kinds and seeds alternate down a run of cuts), a swoosh across a dissolve or other transition, a pop or swipe as a title appears, a punch on a jump-cut punch-in, a riser ending on a drop in the music with an impact on it, the kind a marker named `sfx:<kind>` (or `sfx`) asks for, and with density punchy an impact on the first picture. density: subtle (transitions, titles, drops and markers; at least 4 s apart), balanced (the default; adds the cuts between shots of 1.5 s and longer; 2 s apart) or punchy (every cut and the opening; 1 s apart). A moment within 0.25 s of an effect already on the timeline is skipped, so running it again after add_sound_effects suggests only what is left. Each suggestion is {placement, source, reason, score}: `placement` is exactly an element of add_sound_effects' `effects` (kind, seed, `at` — where the hit lands — and volume_db, about -3 dB, or -8 dB where someone is speaking; no track, the FX lanes allocate), `source` is cut, transition, title, punch_in, music_drop, marker or opening, `reason` says in a sentence what the sound is for and `score` (0 to 1) how much the moment wants one. `categories` and `kinds` limit the sounds (an effect of either list stays), `range` {start, end} keeps the hits inside that span of the whole cut's suggestions. It adds nothing: look the list over, drop what you do not want, change a kind or a gain, then pass the placements to add_sound_effects. Suggest after the cut is settled, since a later edit moves the cuts; footage that has not been analysed suggests less (analyze_asset scenes and transcript for the picture and speech, rhythm for music)."
    )]
    pub(super) async fn suggest_sound_effects(
        &self,
        Parameters(request): Parameters<kerf_core::SfxSuggestRequest>,
    ) -> Result<String, McpError> {
        let (project, owner) = (self.project.clone(), self.owner());
        let suggestions = blocking(move || {
            request.check().map_err(core_err)?;
            // Resolved under the lock, thought about with it released.
            let inputs = lock_agent_as(&project, owner.as_deref())
                .suggest_sound_effects_inputs()
                .map_err(core_err)?;
            let suggestions = kerf_core::suggest_sound_effects(&inputs, &request).map_err(core_err)?;
            Ok((request.density, suggestions))
        })
        .await?;
        let (density, suggestions) = suggestions;
        json(&serde_json::json!({
            "density": density.name(),
            "count": suggestions.len(),
            "suggestions": suggestions,
            "next": "Pass the `placement` of the ones you want as `effects` of add_sound_effects.",
        }))
    }
}

impl KerfMcp {
    /// `add_sound_effects` with its cancellation as a closure (the request's token, or a test's).
    pub(super) async fn add_sound_effects_now(
        &self,
        p: AddSoundEffectsParams,
        cancel: impl Fn() -> bool + Send + 'static,
    ) -> Result<String, McpError> {
        let owner = self.owner();
        // Checked now, before anything renders and under the ripple this call will have: every spec,
        // time, gain and track, and a rehearsal of the placement (landing on another clip of a named
        // track is refused as `edit_timeline` refuses it). A proposal another session owns refuses
        // here too. What changes while it renders is checked again when the clips are placed, and a
        // refusal there stores nothing.
        let (plan, generation) = {
            let project = lock_agent_edit(&self.project, owner.as_deref())?;
            (
                project
                    .with_ripple(p.ripple, |project| project.sound_effects_inputs(&p.effects))
                    .map_err(core_err)?,
                project.generation(),
            )
        };
        let (me, ripple) = (self.clone(), p.ripple);
        let placed = blocking(move || {
            let stop = crate::stop_or_replaced(cancel, &me.project, generation);
            // Rendered with the project lock released.
            let assets = Project::synthesize_sound_effects(&plan, &stop).map_err(core_err)?;
            // Landing it is an edit of the cut, so it goes into the proposal; the webview is told
            // once the lock is released.
            me.edit(|project| {
                require_generation(project, generation, "the sound effects were being made")?;
                let placed = project
                    .with_ripple(ripple, |project| project.place_sound_effects(&assets, &plan))
                    .map_err(core_err)?;
                let timeline = project.working_timeline().map_err(core_err)?;
                Ok(reads::sfx_answer(&plan, &placed, &timeline))
            })
        })
        .await?;
        json(&placed)
    }

    /// `update_sound_effect` with its cancellation as a closure.
    pub(super) async fn update_sound_effect_now(
        &self,
        p: UpdateSoundEffectParams,
        cancel: impl Fn() -> bool + Send + 'static,
    ) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let owner = self.owner();
        let (update, generation) = {
            let project = lock_agent_edit(&self.project, owner.as_deref())?;
            (
                project
                    .with_ripple(p.ripple, |project| {
                        project.update_sound_effect_inputs(clip_id, &p.spec, p.volume_db)
                    })
                    .map_err(core_err)?,
                project.generation(),
            )
        };
        let (me, ripple) = (self.clone(), p.ripple);
        let done = blocking(move || {
            let stop = crate::stop_or_replaced(cancel, &me.project, generation);
            let asset = Project::synthesize_sound_effect(&update.spec, &stop).map_err(core_err)?;
            me.edit(|project| {
                require_generation(project, generation, "the sound effect was being made")?;
                let placed = project
                    .with_ripple(ripple, |project| project.apply_sound_effect_update(&asset, &update))
                    .map_err(core_err)?;
                let timeline = project.working_timeline().map_err(core_err)?;
                Ok(reads::sfx_clip_row(
                    &placed,
                    &reads::sfx_lane(&timeline, placed.clip.id),
                    &update.spec,
                ))
            })
        })
        .await?;
        json(&done)
    }
}
