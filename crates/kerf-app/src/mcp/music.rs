//! Music: its structure, fitting a cut to it, composing it, stems and snapping cuts to beats.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ComposeAlignParams {
    #[schemars(
        description = "What to fit the music to: `cuts` (the boundaries between the clips of a video track; the picture's first start and last end are not cuts) or `markers` (every timeline marker). Read from the cut you are proposing, staged edits included."
    )]
    pub(super) to: kerf_core::AlignTo,
    #[schemars(description = "With `cuts`: the video track whose cuts to use (default the first video track)")]
    track_id: Option<String>,
    #[schemars(
        description = "The most the tempo may be nudged, as a fraction of it: 0.06 (the default) is up to 6 % faster or slower, 0 keeps the tempo and only adds the hits, 0.25 at most"
    )]
    max_tempo_change: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ComposeMusicParams {
    #[schemars(
        description = "The piece, as a score: {bpm 40-240, key, style, sections, hits?, seed?, instruments?}, 4/4 only. key: a tonic and an optional m — G, F#m, Bb, Ebm. style: tropical_house (four-on-the-floor, marimba stabs, pan-flute hook, 100-120 BPM), pop (backbeat, fingered bass, piano, strings, 100-124), lofi (lazy swung boom-bap, Rhodes sevenths, vibraphone, 70-90), cinematic (strings, horns, timpani and big hits, 60-100), ambient (pad and harp, no real beat, 55-80), rock (power-chord guitar, picked bass, live drums, 100-150); music_status lists them with a tempo. sections = [{kind, bars, chords, energy?}] played in order, bars 1-128 each, 400 bars and 10 minutes at most. kind shapes the arrangement: intro (sparse: pad and a hint of the groove), verse (the steady groove), build (pulsing bass, a snare roll that quickens, a riser into whatever follows), drop (everything at once, with a crash and impact), breakdown (no kick or bass: pad, keys and hook), outro (thins out and its last bar resolves on the tonic chord). A typical piece: intro, build, drop, breakdown, build, drop, outro. energy 0-1 sets how loud and busy a section is when its kind's default is not what you want (intro 0.3, verse 0.5, build 0.7, drop 1, breakdown 0.3, outro 0.3). chords: ONE per bar; a list shorter than bars cycles, so [\"vi\",\"IV\",\"I\",\"V\"] over 8 bars plays the four chords twice. Write chord names (C, Em, F#m7, Gsus4, D/F#, Bbmaj7, E5, Cadd9, Am7b5 — types: maj min dim aug sus2 sus4 5 7 maj7 m7 m7b5 dim7 6 m6 add9 madd9 9 maj9 m9) or roman numerals in the score's key (I, V, vi, IV, ii7, bVII): upper case is major, lower case minor, a trailing 7 / maj7 / sus4 etc. adds the type, a leading b or # moves the root a semitone, and numerals count the key's own scale (in A minor, VI is F and VII is G). lofi turns plain triads into sevenths and ambient adds a ninth. hits = [{at_s, kind}] — moments in the picture, at_s in seconds from the START OF THE MUSIC (not the timeline): impact (a crash, timpani and low boom), riser (a rising sweep about a bar long that peaks at at_s), dropout (everything but the effects goes silent for one beat from at_s), whoosh (a ~1 s sweep peaking at at_s). A drop already opens with its own crash and impact, so hits are for moments in the picture the sections do not cover (align adds them at cuts for you). seed (an integer 0-9007199254740991, i.e. 2^53 - 1; a bigger one is refused) varies the humanization and the hook; instruments overrides a role's General MIDI program (drums, bass, pad, keys, lead: 0-127). A score that is wrong (an unknown chord, bars 0, a hit past the end) is refused with the offending section or chord named, and so is a field the score does not have: a misspelled `chord`, `enrgy` or `at` is an error, not a silent default."
    )]
    pub(super) score: kerf_core::MusicScore,
    #[schemars(
        description = "Audio track to put the music on; omitted, it goes on the `M` track (created on first use). Ignored with replace_clip_id."
    )]
    track_id: Option<String>,
    #[schemars(
        description = "Timeline time to start at, in seconds; omitted, it is appended after what the track already holds. With `align` this is the start the music is fitted from, so pass it when the music should start after 0. Ignored with replace_clip_id (the clip keeps its place)."
    )]
    at: Option<f64>,
    #[schemars(
        description = "Fit the score to the picture of the cut you are proposing before it is rendered: {to: \"cuts\" | \"markers\", track_id?, max_tempo_change?}; omit to render the score as written. The tempo is nudged (within max_tempo_change, default 0.06 = 6 %, at most 0.25; never to one that ends the piece before a hit you wrote) so as many cuts as possible land on beats (a downbeat counts double). Then hits are added: an impact on every cut that lands on a downbeat; and for each `drop` that starts more than a beat in and that a cut lands within a beat of, a dropout (a beat of silence) one beat before it, plus a riser peaking at the drop's start — except when the drop follows a build (the build ends in its own riser) or starts earlier than half a bar, where only the dropout is added. Hits within 50 ms of one you wrote are not added twice, and a score stops at 256 hits (`align.notes` says if some were left out). To make a cut hit hard, lay out the sections so a drop starts near it. Cuts are the boundaries between the clips of a video track (default: the first); give `at` if the music does not start at 0. Align after the picture is cut: a later edit moves the cuts, and the music is then composed again with replace_clip_id. The answer's `align` lists the cuts on a beat / downbeat before and after and the hits added, and `score` is the score actually rendered (new bpm, added hits)."
    )]
    pub(super) align: Option<ComposeAlignParams>,
    #[schemars(
        description = "UUID of a music clip made by compose_music: replace its music with the new render instead of adding a clip. This is how a score is edited and composed again. The WHOLE bed goes: every clip of that clip's asset on its track (what fit_music made of one piece, in segments) becomes ONE clip of the new render, at the earliest of their starts, with the gain, fade-in and effects of the first and the fade-out of the last; its length becomes the render's. Refused: a clip that is not composed music, a locked track, music linked to a picture (unlink_clips first). The answer's `replaced` lists the clip ids that were replaced."
    )]
    replace_clip_id: Option<String>,
    #[schemars(
        description = "Ripple override for placing the music, and for the check made before it renders. Omitted, the project's ripple mode applies (get_ripple_mode). It matters when the music lands on or runs into other clips of its track: with ripple on they are pushed later (a start inside a clip is still refused, ripple never splits a clip); with it off the request is refused before anything renders. Appending to the track moves nothing either way. Ripple is an attempt: it never moves a locked track (placing ON one is refused), and a lane it would leave overlapping is refused."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct SnapToBeatsParams {
    #[schemars(description = "UUID of the track to align; every unlocked video track when omitted")]
    track_id: Option<String>,
    #[schemars(
        description = "How far a cut may move to reach a beat (seconds). Defaults to half a beat, which lands every cut on the beat it is nearest"
    )]
    tolerance: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SeparateStemsParams {
    #[schemars(description = "UUID of the asset whose sound to separate (music, or footage with dialogue over a bed)")]
    asset_id: String,
    #[schemars(
        description = "UUID of a clip of that asset to lay the stems under: each stem goes on its own new audio track at the clip's position and span, and the clip's own sound is switched off. Omit to only add the stems to the library"
    )]
    clip_id: Option<String>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PlanMusicFitParams {
    #[schemars(description = "UUID of the music clip (on an audio track, at normal speed)")]
    clip_id: String,
    #[schemars(
        description = "How long the music should last, in seconds. Omit to fit it to the picture: from the clip's start to the end of the last video clip"
    )]
    target: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct FitMusicParams {
    #[schemars(description = "UUID of the music clip (on an audio track, at normal speed, not linked to a picture)")]
    clip_id: String,
    #[schemars(
        description = "How long the music should last, in seconds. Omit to fit it to the picture: from the clip's start to the end of the last video clip"
    )]
    target: Option<f64>,
    #[schemars(
        description = "When the nearest bar-aligned arrangement runs over the target, cut it there and fade out over the last 2 s (default true). False keeps the whole arrangement, ending included"
    )]
    fade_out: Option<bool>,
}

#[tool_router(router = tool_router_music, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Whether music can be composed on this machine: whether the SoundFont (the instrument samples, ~32 MB, downloaded by the first compose_music) is already on disk, how many compositions are rendering now, and the styles on offer with their usual tempo."
    )]
    fn music_status(&self) -> Result<String, McpError> {
        json(&kerf_core::music_status())
    }

    #[tool(
        description = "Compose original music from a score and put it on the timeline. You write the score \
                       (tempo, key, style, sections of chords, optional hits: the `score` field says what each \
                       takes) and Kerf plays it through a General MIDI SoundFont synthesizer, so the same score is \
                       the same audio. Styles: tropical_house, pop, lofi, cinematic, ambient, rock (music_status \
                       lists them with a tempo). The result is a 48 kHz stereo clip on the `M` audio track (created \
                       on first use) at `at`, else appended after what the track holds; peaks at -1 dBFS, loudness \
                       left to the master bus and export. \
                       Cut the picture first, then compose with `align` so the tempo and hits land on the cuts. To \
                       change a piece, edit its score (list_assets shows it as `music_score`) and call again with \
                       replace_clip_id. The asset's tempo and bars are written from the score, so \
                       get_music_structure, fit_music and snap_to_beats work at once (no analyze_asset needed). \
                       Then: fit_music if the piece is the wrong length, set_track_duck plus set_master_duck to dip \
                       it under dialogue, and get_levels and get_band_balance to check it. \
                       The first call downloads the SoundFont (~32 MB). A piece renders in seconds to about a \
                       minute: send a `progressToken` for progress (stages download_soundfont, waiting, render) and \
                       cancel the request to stop it, or pass `background: true` for a job_id. Before anything \
                       renders, the score, the track, the replaced clip and the align target are checked, and so \
                       is (under the ripple in force) whether the music would land on another clip of its track; \
                       if the cut changes while it renders, placing it is checked again and a refusal stores \
                       nothing. Returns {asset, clip, score, align, replaced: [clip ids]}."
    )]
    async fn compose_music(
        &self,
        Parameters(p): Parameters<ComposeMusicParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = |name: &str, value: &Option<String>| value.as_deref().map(|s| parse_arg(name, s)).transpose();
        let align = match p.align {
            None => None,
            Some(a) => Some(kerf_core::ScoreAlign {
                to: a.to,
                track_id: id("align.track_id", &a.track_id)?,
                max_tempo_change: a.max_tempo_change,
            }),
        };
        let request = kerf_core::ComposeRequest {
            score: p.score,
            track_id: id("track_id", &p.track_id)?,
            at: p.at,
            align,
            replace_clip_id: id("replace_clip_id", &p.replace_clip_id)?,
        };
        let owner = self.owner();
        // Checked now, before a minute of rendering, under the ripple this call will have: the
        // score, the track, clip and align target, a locked track and a rehearsal of the
        // placement (landing on another clip of the track is refused as `edit_timeline` refuses
        // it). A proposal another session owns refuses here too. What changes while it renders
        // is checked again when the music is placed, and a refusal there stores nothing.
        let (plan, generation) = {
            let project = lock_agent_edit(&self.project, owner.as_deref())?;
            (
                project
                    .with_ripple(p.ripple, |project| project.compose_music_inputs(&request))
                    .map_err(core_err)?,
                project.generation(),
            )
        };
        let (me, ripple) = (self.clone(), p.ripple);
        // The GUI's Stop on a composition ends an agent's too (`begin_music`).
        let gui_stop = self.app.state::<crate::AppState>().begin_music();
        self.long_tool("compose_music", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let composed = blocking(move || {
                let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
                    let message = match &detail {
                        Some(d) => format!("{stage}: {d}"),
                        None => stage.to_string(),
                    };
                    run.progress(fraction, message);
                    // The GUI shows the same progress it shows for its own compositions.
                    let _ = app.emit(
                        "music-progress",
                        crate::VoiceoverProgressEvent {
                            stage: stage.to_string(),
                            fraction,
                            detail,
                        },
                    );
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let asset = Project::synthesize_music(&plan.score, &mut on_progress, &stop).map_err(core_err)?;
                // Landing it is an edit of the cut, so it goes into the proposal.
                let project = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&project, generation, "the music was being composed")?;
                let (asset, clip) = project
                    .with_ripple(ripple, |project| {
                        project.place_music(&asset, plan.track_id, plan.at, plan.replace_clip_id)
                    })
                    .map_err(core_err)?;
                Ok(kerf_core::Composed {
                    asset,
                    clip,
                    score: plan.score,
                    align: plan.align,
                    replaced: plan.replaces,
                })
            })
            .await?;
            me.changed();
            // `score` carries the score; the asset need not repeat it.
            let mut asset = reads::compact_asset(&composed.asset);
            asset.music_score = None;
            json(&serde_json::json!({
                "asset": asset,
                "clip": composed.clip,
                "score": composed.score,
                "align": composed.align,
                "replaced": composed.replaced,
            }))
        })
        .await
    }

    #[tool(
        description = "Whether stem separation is ready on this machine: whether the ONNX runtime and the ~170 MB Demucs model still have to be downloaded, and the stems it makes."
    )]
    fn stems_status(&self) -> Result<String, McpError> {
        json(&kerf_core::stems_status())
    }

    #[tool(
        description = "Split an asset's sound into four stems — drums, bass, other, vocals — with the Demucs htdemucs model, \
                       added to the library as FLAC assets (\"<name> · drums\" …). With clip_id they are also laid under \
                       that clip on four new audio tracks with the clip's own sound switched off, so each part can be \
                       mixed, muted or re-cut on its own: drums from one section under another, the vocals pulled down \
                       under a voiceover, or dialogue separated from the background of a clip (its speech lands in \
                       vocals). The first call downloads the runtime and model; separation takes a fraction of the audio's length on a \
                       modern CPU (minutes on an old one) — send a `progressToken` for progress and cancel the request to \
                       stop it, or pass `background: true` for a job_id. Cached per file. Returns the stem assets and the placed clips."
    )]
    async fn separate_stems(
        &self,
        Parameters(p): Parameters<SeparateStemsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let asset_id = parse_arg("asset_id", &p.asset_id)?;
        let clip_id = p.clip_id.as_deref().map(|s| parse_arg("clip_id", s)).transpose()?;
        let (asset, generation) = {
            let project = self.lock();
            let asset = project.require_asset(asset_id).map_err(core_err)?;
            if let Some(clip) = clip_id {
                let timeline = project.working_timeline().map_err(core_err)?;
                if !timeline.tracks.iter().any(|t| t.clips.iter().any(|c| c.id == clip)) {
                    return Err(core_err(kerf_core::Error::ClipNotFound(clip)));
                }
            }
            (asset, project.generation())
        };
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on a separation ends an agent's too (`begin_stems`).
        let gui_stop = self.app.state::<crate::AppState>().begin_stems();
        self.long_tool("separate_stems", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let placed = blocking(move || {
                let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
                    let message = match &detail {
                        Some(d) => format!("{stage}: {d}"),
                        None => stage.to_string(),
                    };
                    run.progress(fraction, message);
                    let _ = app.emit(
                        "stems-progress",
                        crate::VoiceoverProgressEvent {
                            stage: stage.to_string(),
                            fraction,
                            detail,
                        },
                    );
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let stems = Project::separate_stems_media(&asset, &mut on_progress, &stop).map_err(core_err)?;
                // The stems land in the library either way; laying them under a clip is an edit.
                let project = match clip_id {
                    Some(_) => lock_agent_edit(&project, owner.as_deref())?,
                    None => lock_agent_as(&project, owner.as_deref()),
                };
                require_generation(&project, generation, "the stems were being made")?;
                project.place_stems(asset_id, &stems, clip_id).map_err(core_err)
            })
            .await?;
            me.changed();
            json(&placed)
        })
        .await
    }

    #[tool(
        description = "Cut to the beat: ripple a track's cuts onto the beat grid of the analyzed music on the audio tracks, retrimming each clip so its outgoing cut lands on a beat. Analyze the music asset first"
    )]
    fn snap_to_beats(&self, Parameters(p): Parameters<SnapToBeatsParams>) -> Result<String, McpError> {
        self.edit(|project| Self::snap_to_beats_op(project, p))
    }

    pub(super) fn snap_to_beats_op(project: &Project, p: SnapToBeatsParams) -> Result<String, McpError> {
        let track = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        let aligned = project.snap_to_beats(track, p.tolerance).map_err(core_err)?;
        // The *working* timeline, like every other read here: inside a task the
        // alignment lands in the staged proposal, and `timeline()` would hand
        // back the user's untouched cut — a timeline with none of the moves the
        // `cuts_aligned` count beside it just reported.
        let timeline = project.working_timeline().map_err(core_err)?;
        json(&serde_json::json!({ "cuts_aligned": aligned, "timeline": reads::timeline_brief(&timeline) }))
    }

    #[tool(
        description = "Read the bar structure of an analyzed music asset, compactly: tempo, the fitted beat grid \
                       (period, phase, which beat is the downbeat), every bar start, and the repeating 8- and 4-bar \
                       phrases (bar indices counted from the first downbeat) that fit_music may splice between. \
                       Analyze the asset first; `music` is null when it has no steady pulse."
    )]
    fn get_music_structure(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let analysis = self.lock().get_analysis(id).map_err(core_err)?;
        let Some(analysis) = analysis else {
            return Err(McpError::invalid_params(
                "the asset has not been analyzed yet — call analyze_asset first",
                None,
            ));
        };
        let music = analysis.music.map(|m| {
            serde_json::json!({
                "bpm": m.grid.bpm(),
                "grid": m.grid,
                "duration": m.duration,
                "bar_seconds": m.grid.bar_s(),
                "bars": m.bar_chroma.len(),
                "bar_starts": (0..m.bar_chroma.len()).map(|k| m.grid.bar_start(k)).collect::<Vec<_>>(),
                "intro_end": m.grid.first_downbeat().max(0.0),
                "phrases": m.phrases,
            })
        });
        json(&serde_json::json!({ "asset_id": id, "music": music, "loudness": analysis.loudness }))
    }

    #[tool(
        description = "Preview fit_music without changing anything: the bar-aligned arrangement (source segments in \
                       output order), how long it lasts, the remainder (target minus duration: positive is short, \
                       negative runs over and fit_music would fade it out) and how many splices it needs. The intro \
                       and ending always play; only whole repeating phrases are repeated or dropped."
    )]
    fn plan_music_fit(&self, Parameters(p): Parameters<PlanMusicFitParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let fit = self.lock().plan_music_fit(clip_id, p.target).map_err(core_err)?;
        json(&fit)
    }

    #[tool(
        description = "Fit music to length: replace a music clip with a bar-aligned edit list that lasts as long as \
                       the target (by default the picture from the clip's start). Keeps the intro and ending, repeats \
                       or drops whole repeating phrases, and crossfades each splice over 10 ms centred on it — \
                       non-destructive clips on the original file, nothing is rendered. Needs analyze_asset on the \
                       music first (a steady beat). If no arrangement hits the target on bar lines the nearest one is \
                       used and, with fade_out (default), cut at the target and faded. Check plan_music_fit first if \
                       the remainder matters. Returns the new clip ids, the plan and a summary of the timeline (get_timeline_state has all of it)."
    )]
    fn fit_music(&self, Parameters(p): Parameters<FitMusicParams>) -> Result<String, McpError> {
        self.edit(|project| Self::fit_music_op(project, p))
    }

    pub(super) fn fit_music_op(project: &Project, p: FitMusicParams) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let report = project
            .fit_music(clip_id, p.target, p.fade_out.unwrap_or(true))
            .map_err(core_err)?;
        let timeline = project.working_timeline().map_err(core_err)?;
        json(&serde_json::json!({ "fit": report, "timeline": reads::timeline_brief(&timeline) }))
    }
}
