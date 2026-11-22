//! Captions, the cut's transcript, imported captions, voiceover, stems and stored transcripts.

use super::*;

impl Project {
    /// Caption the cut: project every clip's cached transcript through the edit
    /// and write the result as text overlays, replacing any previous generated
    /// set.
    ///
    /// This is deliberately timeline-scoped rather than asset-scoped. A
    /// transcript is in source time and an overlay is in timeline time, and the
    /// two only agree on an untouched asset sitting at zero — which is not a cut.
    /// The moment anything is trimmed, reordered, retimed or (most of all)
    /// silence-removed, source time and timeline time diverge and every caption
    /// is on the wrong word. [`Timeline::captions`] does the projection, so
    /// captions follow the cut and words that were cut out get none.
    ///
    /// `opts.style` picks the look — a held subtitle line, or one word at a
    /// time — and everything else in [`CaptionOptions`] is an override on top
    /// of it.
    ///
    /// Errors when nothing on the timeline has a transcript to caption, rather
    /// than quietly writing no overlays.
    pub fn generate_captions(&self, opts: CaptionOptions) -> Result<Vec<TextOverlay>> {
        let timeline = self.working_timeline()?;
        let mut transcripts: HashMap<Uuid, Vec<TranscriptSegment>> = HashMap::new();
        let mut analyzed = false;
        for track in &timeline.tracks {
            for clip in &track.clips {
                if transcripts.contains_key(&clip.asset_id) {
                    continue;
                }
                let Some(analysis) = self.get_analysis(clip.asset_id)? else {
                    continue;
                };
                analyzed = true;
                transcripts.insert(clip.asset_id, analysis.transcript);
            }
        }
        if !analyzed {
            return Err(Error::InvalidArgument(
                "no analysis available for the clips on the timeline; run analysis first".to_string(),
            ));
        }
        let overlays = timeline.captions(&transcripts, opts);
        if overlays.is_empty() {
            return Err(Error::InvalidArgument(
                "no speech was transcribed for the footage in this cut".to_string(),
            ));
        }
        let created = overlays.clone();
        // Name the style in the edit log: recaptioning in the other one is a
        // different edit, and the history is where that has to be visible.
        let label = match opts.style {
            CaptionStyle::Lines => "Generate captions",
            CaptionStyle::WordPunch => "Generate word captions",
        };
        self.edit_timeline(label, move |timeline| {
            timeline.overlays.retain(|o| !o.generated);
            timeline.overlays.extend(overlays);
            Ok(())
        })?;
        Ok(created)
    }

    /// What the cut says: every audible clip's cached transcript mapped into timeline time
    /// ([`Timeline::cut_transcript`] over the working timeline), and the assets the cut plays
    /// that have no transcript yet. Reads the cache only — nothing is transcribed here.
    pub fn cut_transcript(&self) -> Result<CutTranscript> {
        Ok(self.cut_transcript_inputs()?.read())
    }

    /// The part of [`Project::cut_transcript`] that needs the project: the working timeline and
    /// the transcripts of what it plays. [`CutTranscriptInputs::read`] maps them with the lock
    /// released.
    pub fn cut_transcript_inputs(&self) -> Result<CutTranscriptInputs> {
        let timeline = self.working_timeline()?;
        let rendered = timeline.for_render();
        let mut heard: Vec<(Uuid, usize)> = Vec::new();
        for clip in rendered.tracks.iter().flat_map(|t| &t.clips) {
            if !clip.source_audio || clip.asset_id == GENERATED_ASSET {
                continue;
            }
            match heard.iter_mut().find(|(id, _)| *id == clip.asset_id) {
                Some((_, clips)) => *clips += 1,
                None => heard.push((clip.asset_id, 1)),
            }
        }
        let mut transcripts: HashMap<Uuid, Vec<TranscriptSegment>> = HashMap::new();
        let mut untranscribed = Vec::new();
        for (asset_id, clips) in heard {
            match self.get_analysis(asset_id)?.filter(|a| a.done(AnalysisKind::Transcript)) {
                Some(analysis) => {
                    transcripts.insert(asset_id, analysis.transcript);
                }
                None => {
                    if let Some(asset) = self
                        .stored_asset(asset_id)?
                        .filter(|a| a.has_audio() && a.sound_effect.is_none())
                    {
                        untranscribed.push(Untranscribed {
                            asset_id,
                            name: asset.name,
                            clips,
                        });
                    }
                }
            }
        }
        Ok(CutTranscriptInputs {
            timeline,
            transcripts,
            untranscribed,
        })
    }

    /// Caption the cut from a parsed subtitle file: lay its cues onto the cut and
    /// write them as caption overlays — as **one** `Import captions` revision.
    ///
    /// Taking the [`CaptionFile`] rather than text is the lock-free split again:
    /// [`parse_captions`](crate::captions_import::parse_captions) (and
    /// [`read_caption_file`](crate::captions_import::read_caption_file) before it)
    /// are pure and run with the project lock released, because reading a file —
    /// a few MB at the worst — must not stall an edit; this method only places
    /// what was read.
    ///
    /// `req.base` says which clock the file's times are on. The default,
    /// [`CaptionTimeBase::Timeline`], is a subtitle file made for the finished
    /// cut; [`CaptionTimeBase::Source`] is a file that times one asset's own
    /// footage (a transcript, subtitles for the uncut source) and is projected
    /// through that asset's clips exactly as a transcript is, so it follows trims,
    /// reorders and speed changes. `req.offset` is added to every cue first
    /// (seconds, either sign: a broadcast file that starts at `01:00:00` wants
    /// `-3600`). [`Timeline::place_cues`] does the placement with the same
    /// chunking, flicker floors, one-lane rule and frame fit as
    /// [`Project::generate_captions`], in the style `req.options` names.
    ///
    /// **The imported set is the caption set.** Its overlays are `generated`, so
    /// `Recaption` / `Clear captions` / the delivery re-fit treat them like any
    /// other captions — and that means importing *replaces* the previous
    /// generated or imported captions (the summary's `replaced` says how many),
    /// and a later [`Project::generate_captions`] replaces the imported ones.
    /// Captions are one lane of text at one position; keeping two sets would put
    /// two on screen at once. Typed titles are never touched.
    ///
    /// Honors the staging session like any edit: an agent's import lands in its
    /// proposal. Errors — and writes nothing, so the existing captions survive —
    /// when the file holds no cues, none of them can be shown, the offset is not
    /// a sensible number, or the result would be more than
    /// [`MAX_IMPORTED_CAPTIONS`] captions.
    pub fn import_captions(&self, file: &CaptionFile, req: CaptionImportRequest) -> Result<ImportSummary> {
        if file.cues.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "no captions found in the {} text ({} entries could not be read)",
                file.format.as_str().to_uppercase(),
                file.skipped
            )));
        }
        if !req.offset.is_finite() || req.offset.abs() > MAX_CAPTION_OFFSET {
            return Err(Error::InvalidArgument(format!(
                "offset must be a number of seconds within ±{MAX_CAPTION_OFFSET}"
            )));
        }
        if let CaptionTimeBase::Source(asset) = req.base {
            self.require_asset(asset)?;
        }
        let cues: Vec<TranscriptSegment> = file
            .cues
            .iter()
            .map(|c| TranscriptSegment {
                start: c.start + req.offset,
                end: c.end + req.offset,
                text: c.text.clone(),
                ..Default::default()
            })
            .collect();
        let (first, last) = (cues[0].start, cues.iter().map(|c| c.end).fold(f64::MIN, f64::max));
        // The placement is computed from the very timeline the edit then writes
        // to — the proposal for an agent that is staging — so there is no gap
        // between looking at the cut and changing it.
        let (placement, captions, replaced) = self.edit_timeline("Import captions", |timeline| {
            if let CaptionTimeBase::Source(asset) = req.base {
                if !timeline.tracks.iter().flat_map(|t| &t.clips).any(|c| c.asset_id == asset) {
                    return Err(Error::InvalidArgument(format!(
                        "asset {asset} is not used by any clip in the cut; put it on the timeline first, \
                         or import with base \"timeline\" if the file already times the finished cut"
                    )));
                }
            }
            let mut placement = timeline.place_cues(&cues, req.base, req.options);
            if placement.overlays.is_empty() {
                let run = format!("they run {} to {}", fmt_time(first), fmt_time(last));
                return Err(Error::InvalidArgument(if placement.dropped_outside == cues.len() {
                    match req.base {
                        CaptionTimeBase::Timeline => format!(
                            "none of the {} cues fall inside the cut: {run} and the cut is {} long",
                            cues.len(),
                            fmt_time(timeline.for_render().duration()),
                        ),
                        CaptionTimeBase::Source(_) => format!(
                            "none of the {} cues land on footage this asset shows in the cut: {run} of the \
                             source (a muted track or a disabled clip does not count)",
                            cues.len(),
                        ),
                    }
                } else {
                    format!(
                        "none of the {} cues could be shown: {} outside the cut, {} too short to read, {} hidden by another",
                        cues.len(),
                        placement.dropped_outside,
                        placement.dropped_short,
                        placement.dropped_overlap,
                    )
                }));
            }
            if placement.overlays.len() > MAX_IMPORTED_CAPTIONS {
                return Err(Error::InvalidArgument(format!(
                    "this import would write {} captions; Kerf writes at most {MAX_IMPORTED_CAPTIONS} at a time \
                     (the `word_punch` look makes one caption per word — `lines` makes far fewer)",
                    placement.overlays.len()
                )));
            }
            let before = timeline.overlays.len();
            timeline.overlays.retain(|o| !o.generated);
            let replaced = before - timeline.overlays.len();
            let captions = placement.overlays.len();
            timeline.overlays.append(&mut placement.overlays);
            Ok((placement, captions, replaced))
        })?;
        Ok(ImportSummary {
            format: file.format,
            cues: cues.len(),
            placed: placement.placed,
            captions,
            skipped_lines: file.skipped,
            dropped_outside: placement.dropped_outside,
            dropped_short: placement.dropped_short,
            dropped_overlap: placement.dropped_overlap,
            replaced,
        })
    }

    /// Read `text` aloud and describe the result as an importable [`Asset`],
    /// *without* `&self` — synthesis runs for seconds to minutes (and may first
    /// download the voice model), so like [`Project::probe_import`] it happens
    /// with the project lock released; [`Project::place_voiceover`] then takes
    /// it only to land the result.
    ///
    /// The asset carries its [`Voiceover`], script and exact sentence timings
    /// included, which is what lets it be captioned without transcribing it.
    pub fn synthesize_voiceover(
        text: &str,
        voice: &str,
        speed: f64,
        progress: engine::tts::ProgressFn,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Asset> {
        let synthesis = engine::tts::synthesize(text, voice, speed, progress, cancel)?;
        let mut asset = Self::probe_asset(&synthesis.path)?;
        asset.name = voiceover_name(text);
        asset.voiceover = Some(Voiceover {
            text: text.trim().to_string(),
            voice: voice.to_string(),
            speed: speed.clamp(engine::tts::MIN_SPEED, engine::tts::MAX_SPEED),
            segments: synthesis.segments,
        });
        Ok(asset)
    }

    /// Separate an asset's sound into drums, bass, other and vocals and describe each
    /// stem as an importable [`Asset`] (`"<name> · drums"` …), *without* `&self*: the
    /// separation runs for a minute or more (after a first-use model download), so it
    /// happens with the project lock released; [`Project::place_stems`] lands the result.
    pub fn separate_stems_media(
        asset: &Asset,
        progress: engine::tts::ProgressFn,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<Asset>> {
        if !asset.streams.iter().any(|s| s.kind == StreamKind::Audio) {
            return Err(Error::InvalidArgument("this asset has no sound to separate".to_string()));
        }
        let paths = engine::stems::separate(Path::new(&asset.path), progress, cancel)?;
        paths
            .iter()
            .zip(engine::STEM_NAMES)
            .map(|(path, name)| {
                let mut stem = Self::probe_asset(path)?;
                stem.name = format!("{} · {name}", asset.name);
                Ok(stem)
            })
            .collect()
    }

    /// Store separated stems (reusing ones already imported) and, with `clip_id`, lay
    /// them under that clip as one edit: each on a new audio track named after it, at
    /// the clip's position, span, speed and gain — and the clip's own sound switched
    /// off (an audio clip disabled, a picture's `source_audio` off), so the mix is not
    /// heard twice. The clip must be of the separated asset and not linked to a sound
    /// clip elsewhere (separate that one instead).
    pub fn place_stems(&self, source: Uuid, stems: &[Asset], clip_id: Option<Uuid>) -> Result<StemsPlaced> {
        // The separation ran with the lock released: the asset it came from (or the whole project)
        // may be gone by now, and stems with no source have no business in the library.
        if self.stored_asset(source)?.is_none() {
            return Err(Error::AssetNotFound(source));
        }
        let stems: Vec<Asset> = stems.iter().map(|a| self.insert_or_get_asset(a)).collect::<Result<_>>()?;
        let Some(clip_id) = clip_id else {
            return Ok(StemsPlaced {
                assets: stems,
                clips: Vec::new(),
            });
        };
        let clips = self.edit_timeline_exact("Separate stems", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let track = &timeline.tracks[ti];
            let clip = track.clips[ci].clone();
            if clip.asset_id != source {
                return Err(Error::InvalidArgument("that clip is not of the separated asset".to_string()));
            }
            if track.locked {
                return Err(Error::InvalidArgument(format!("{} is locked", track.name)));
            }
            let on_audio = track.kind == StreamKind::Audio;
            if !on_audio && clip.link_id.is_some() {
                return Err(Error::InvalidArgument(
                    "this picture's sound is on a linked audio clip — separate that clip instead".to_string(),
                ));
            }
            let mut placed = Vec::new();
            for (stem, name) in stems.iter().zip(engine::STEM_NAMES) {
                let mut c = Clip::for_asset(stem, clip.source_in, clip.source_out.min(stem.duration), clip.timeline_start);
                c.speed = clip.speed;
                c.speed_ramp = clip.speed_ramp.clone();
                c.volume = clip.volume;
                c.fade_in = clip.fade_in;
                c.fade_out = clip.fade_out;
                let mut lane = Track::new(StreamKind::Audio, capitalized(name));
                lane.clips.push(c.clone());
                timeline.tracks.push(lane);
                placed.push(c);
            }
            let original = &mut timeline.tracks[ti].clips[ci];
            if on_audio {
                original.enabled = false;
            } else {
                original.source_audio = false;
            }
            Ok(placed)
        })?;
        Ok(StemsPlaced { assets: stems, clips })
    }

    /// Land a synthesized voiceover: store the asset (reusing it when the same
    /// script was generated before), record its script as the asset's
    /// transcript, and put it on the timeline as one edit.
    ///
    /// With no `track_id` it goes on the audio track named [`VOICEOVER_TRACK`],
    /// which is created on first use — narration kept on its own lane, never
    /// dropped on top of the dialogue or the music bed on `A1`. With no
    /// `timeline_start` it is appended after whatever that track already holds.
    pub fn place_voiceover(&self, asset: &Asset, track_id: Option<Uuid>, timeline_start: Option<f64>) -> Result<(Asset, Clip)> {
        // Where it goes may not be locked: a named track, or the voiceover lane the user locked. Asked
        // before the asset is stored, so a refusal leaves nothing behind, and again in the edit.
        let lane_open = |timeline: &Timeline| -> Result<()> {
            match track_id {
                Some(t) => timeline.ensure_track_editable(t),
                None => match timeline
                    .tracks
                    .iter()
                    .find(|t| t.kind == StreamKind::Audio && t.name == VOICEOVER_TRACK)
                {
                    Some(lane) => timeline.ensure_track_editable(lane.id),
                    None => Ok(()),
                },
            }
        };
        lane_open(&self.working_timeline()?)?;
        let asset = self.insert_or_get_asset(asset)?;
        if let Some(voiceover) = &asset.voiceover {
            // Keep whatever an earlier analysis measured; only the transcript is
            // ours to write.
            let mut analysis = self.get_analysis(asset.id)?.unwrap_or_else(|| AssetAnalysis {
                asset_id: asset.id,
                ..AssetAnalysis::default()
            });
            analysis.merge(&AssetAnalysis {
                asset_id: asset.id,
                transcript: voiceover.segments.clone(),
                ran: vec![crate::model::AnalysisKind::Transcript],
                ..AssetAnalysis::default()
            });
            self.set_analysis(&analysis)?;
        }
        let clip_id = Uuid::new_v4();
        self.edit_lanes.borrow_mut().push(clip_id);
        let clip = self.edit_timeline("Add voiceover", |timeline| {
            lane_open(timeline)?;
            let tid = match track_id {
                Some(t) => {
                    let track = timeline.track(t).ok_or(Error::TrackNotFound(t))?;
                    if track.kind != StreamKind::Audio {
                        return Err(Error::InvalidArgument("a voiceover goes on an audio track".to_string()));
                    }
                    t
                }
                None => match timeline
                    .tracks
                    .iter()
                    .find(|t| t.kind == StreamKind::Audio && t.name == VOICEOVER_TRACK)
                {
                    Some(track) => track.id,
                    None => {
                        let track = Track::new(StreamKind::Audio, VOICEOVER_TRACK.to_string());
                        let id = track.id;
                        timeline.tracks.push(track);
                        id
                    }
                },
            };
            let start = timeline_start
                .map(|t| t.max(0.0))
                .unwrap_or_else(|| timeline.track(tid).map(Track::end).unwrap_or(0.0));
            let mut clip = Clip::for_asset(&asset, 0.0, asset.duration, start);
            clip.id = clip_id;
            timeline
                .track_mut(tid)
                .expect("track resolved above")
                .clips
                .push(clip.clone());
            Ok(clip)
        })?;
        Ok((asset, clip))
    }

    /// Remove the captions [`Project::generate_captions`] wrote, leaving titles
    /// and lower-thirds alone.
    pub fn clear_captions(&self) -> Result<usize> {
        self.edit_timeline("Clear captions", move |timeline| {
            let before = timeline.overlays.len();
            timeline.overlays.retain(|o| !o.generated);
            Ok(before - timeline.overlays.len())
        })
    }

    /// Render an asset's cached transcript as a SubRip (`.srt`) document.
    pub fn transcript_srt(&self, asset_id: Uuid) -> Result<String> {
        let analysis = self
            .get_analysis(asset_id)?
            .ok_or_else(|| Error::InvalidArgument("no analysis available for asset; run analysis first".to_string()))?;
        if analysis.transcript.is_empty() {
            return Err(Error::InvalidArgument("asset has no transcript".to_string()));
        }
        Ok(crate::model::transcript_to_srt(&analysis.transcript))
    }

    /// An asset's cached transcript; an error that says why when there is none.
    pub fn transcript(&self, asset_id: Uuid) -> Result<Vec<TranscriptSegment>> {
        self.require_asset(asset_id)?;
        let transcript = self.get_analysis(asset_id)?.map(|a| a.transcript).unwrap_or_default();
        if transcript.is_empty() {
            return Err(Error::InvalidArgument(
                "asset has no transcript; run analyze_asset with the transcript step first".to_string(),
            ));
        }
        Ok(transcript)
    }

    /// Store a re-timed / speaker-labelled transcript ([`crate::refine_transcript_media`])
    /// for an asset, replacing the cached one and leaving every other kind as it was. `read`
    /// is the transcript the refinement started from: when the cache no longer holds it (a
    /// transcription finished meanwhile) nothing is stored, so a newer transcript is not
    /// overwritten by a refinement of the older one.
    pub fn store_transcript(
        &self,
        asset_id: Uuid,
        read: &[TranscriptSegment],
        transcript: Vec<TranscriptSegment>,
    ) -> Result<AssetAnalysis> {
        if self.transcript(asset_id)? != read {
            return Err(Error::InvalidArgument(
                "the transcript changed while it was being refined; refine it again".to_string(),
            ));
        }
        self.merge_analysis(&AssetAnalysis {
            asset_id,
            transcript,
            ran: vec![crate::model::AnalysisKind::Transcript],
            ..AssetAnalysis::default()
        })
    }

    /// When each speaker of an asset's transcript talks, in source time (only `speaker`'s
    /// when given); words less than `join_gap` seconds apart are one range.
    pub fn speaker_ranges(
        &self,
        asset_id: Uuid,
        speaker: Option<&str>,
        join_gap: f64,
    ) -> Result<Vec<crate::transcript::SpeakerRanges>> {
        let transcript = self.transcript(asset_id)?;
        if !transcript.iter().any(|s| s.speaker.is_some()) {
            return Err(Error::InvalidArgument(
                "the transcript has no speaker labels; refine it (refine_transcript) to label speakers".to_string(),
            ));
        }
        let ranges = crate::transcript::speaker_ranges(&transcript, speaker, join_gap.max(0.0));
        if let (Some(want), true) = (speaker, ranges.is_empty()) {
            let mut known: Vec<String> = transcript.iter().filter_map(|s| s.speaker.clone()).collect();
            known.sort();
            known.dedup();
            return Err(Error::InvalidArgument(format!(
                "no speaker {want:?} in the transcript; it has {}",
                known.join(", ")
            )));
        }
        Ok(ranges)
    }

    /// The filler words (`fillers`, else the usual um / uh / you know…) in an asset's
    /// transcript, by word bounds, in source time.
    pub fn filler_words(&self, asset_id: Uuid, fillers: &[String]) -> Result<Vec<crate::transcript::FillerWord>> {
        Ok(crate::transcript::filler_words(&self.transcript(asset_id)?, fillers))
    }

    /// What [`crate::transcript::search_transcripts`] reads, under the lock: the assets in library order
    /// (only `asset_ids`, when given), their cached analysis unparsed, and the working timeline.
    pub fn transcript_search_inputs(&self, asset_ids: Option<&[Uuid]>) -> Result<crate::transcript::TranscriptSearchInputs> {
        let library = self.list_assets()?;
        let named: Option<HashSet<Uuid>> = asset_ids.map(|ids| ids.iter().copied().collect());
        if let Some(missing) = named.iter().flatten().find(|id| !library.iter().any(|a| a.id == **id)) {
            return Err(Error::AssetNotFound(*missing));
        }
        let assets: Vec<crate::transcript::search::SearchAsset> = library
            .into_iter()
            .filter(|a| named.as_ref().is_none_or(|n| n.contains(&a.id)))
            .map(|a| crate::transcript::search::SearchAsset {
                has_audio: a.has_audio(),
                named: named.is_some(),
                id: a.id,
                name: a.name,
            })
            .collect();
        let mut stmt = self.conn.prepare("SELECT data FROM analysis WHERE asset_id = ?1")?;
        let mut cached = HashMap::new();
        for asset in &assets {
            let data = stmt
                .query_row(params![asset.id.to_string()], |r| r.get::<_, String>(0))
                .optional()?;
            if let Some(data) = data {
                cached.insert(asset.id, data);
            }
        }
        Ok(crate::transcript::TranscriptSearchInputs {
            assets,
            cached,
            timeline: self.working_timeline()?,
        })
    }
}

/// What [`Project::cut_transcript_inputs`] read under the lock.
#[derive(Debug, Clone)]
pub struct CutTranscriptInputs {
    timeline: Timeline,
    transcripts: HashMap<Uuid, Vec<TranscriptSegment>>,
    untranscribed: Vec<Untranscribed>,
}

impl CutTranscriptInputs {
    /// Map the transcripts through the cut. Pure.
    pub fn read(self) -> CutTranscript {
        CutTranscript {
            lines: self.timeline.cut_transcript(&self.transcripts),
            untranscribed: self.untranscribed,
        }
    }
}

/// The transcript of the cut (see [`Project::cut_transcript`]).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CutTranscript {
    pub lines: Vec<CutLine>,
    /// Assets the cut plays the sound of that have not been transcribed, in track order.
    pub untranscribed: Vec<Untranscribed>,
}

/// An asset the cut plays the sound of with no transcript yet.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Untranscribed {
    pub asset_id: Uuid,
    pub name: String,
    /// Audible clips of the cut cut from it.
    pub clips: usize,
}

/// What [`Project::place_stems`] did: the stem assets (in `STEM_NAMES` order) and the
/// clips it laid under the separated clip, if one was named.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StemsPlaced {
    pub assets: Vec<Asset>,
    pub clips: Vec<Clip>,
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}
