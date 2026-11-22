//! Silence, beats, music fit, detached audio, linked clips and concatenation.

use super::*;

impl Project {
    /// Append the non-silent spans of an asset as clips, using cached analysis.
    pub fn remove_silence(&self, asset_id: Uuid) -> Result<Vec<Clip>> {
        let asset = self.require_asset(asset_id)?;
        let analysis = self
            .get_analysis(asset_id)?
            .ok_or_else(|| Error::InvalidArgument("no analysis available for asset; run analysis first".to_string()))?;

        let mut silence: Vec<TimeRange> = analysis.silence_segments;
        silence.sort_by(|a, b| a.start.total_cmp(&b.start));

        let mut keep: Vec<(f64, f64)> = Vec::new();
        let mut cursor = 0.0;
        for span in &silence {
            if span.start > cursor {
                keep.push((cursor, span.start));
            }
            cursor = cursor.max(span.end);
        }
        if cursor < asset.duration {
            keep.push((cursor, asset.duration));
        }

        let primary = asset.primary_kind();
        self.edit_timeline("Remove silence", |timeline| {
            let tid = default_track(timeline, primary)?;
            let mut start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
            let mut clips = Vec::new();
            for (src_in, src_out) in keep {
                let clip = Clip::for_asset(&asset, src_in, src_out, start);
                start += clip.duration();
                timeline.track_mut(tid).unwrap().clips.push(clip.clone());
                clips.push(clip);
            }
            Ok(clips)
        })
    }

    /// Ripple the cuts of a track onto the beat grid of the music on the audio
    /// tracks — "cut to the beat". Each clip is retrimmed so its outgoing cut
    /// lands on the nearest beat within `tolerance` seconds (default: half a
    /// beat, so every cut moves to the beat it is already closest to) and the
    /// rest of the track follows, preserving gaps. `track_id` picks one track;
    /// `None` aligns every unlocked video track. Returns how many cuts moved.
    ///
    /// Needs the music asset analyzed — the grid comes from the cached
    /// [`Tempo`], the same one the timeline ruler draws its beat ticks from.
    ///
    /// **Linked clips are re-synced afterwards**: the snap reflows a lane without
    /// knowing about links, so each clip it retimed carries the change to its
    /// partners (`Timeline::carry_links_since` — a partner follows an edge it
    /// shared, a group the snap changed in several members is left as the snap made
    /// it; one that would land on a clip outside its group refuses the snap).
    /// `with_links(Some(false))` snaps the lanes alone.
    pub fn snap_to_beats(&self, track_id: Option<Uuid>, tolerance: Option<f64>) -> Result<usize> {
        let mut limits = HashMap::new();
        let mut tempos: HashMap<Uuid, Tempo> = HashMap::new();
        for asset in self.list_assets()? {
            // A still loops, so it stretches to whatever the beat asks for.
            let limit = if asset.is_image() { f64::INFINITY } else { asset.duration };
            limits.insert(asset.id, limit);
            if let Some(tempo) = self.get_analysis(asset.id)?.and_then(|a| a.tempo) {
                tempos.insert(asset.id, tempo);
            }
        }
        let links = self.links_active();
        self.edit_timeline_exact("Cut to the beat", |timeline| {
            let before = links.then(|| timeline.clone());
            let beats = timeline.beat_grid(&tempos);
            if beats.len() < 2 {
                return Err(Error::InvalidArgument(
                    "no beat grid — put rhythmic audio on an audio track and analyze it first".to_string(),
                ));
            }
            let tolerance = match tolerance {
                Some(value) if value.is_finite() && value > 0.0 => value,
                Some(_) => return Err(Error::InvalidArgument("tolerance must be positive".to_string())),
                None => default_beat_tolerance(&beats),
            };
            let targets: Vec<Uuid> = match track_id {
                Some(id) => {
                    timeline.ensure_track_editable(id)?;
                    vec![id]
                }
                None => timeline
                    .tracks
                    .iter()
                    .filter(|t| t.kind == StreamKind::Video && !t.locked)
                    .map(|t| t.id)
                    .collect(),
            };
            let mut aligned = 0;
            for id in targets {
                let track = timeline.track_mut(id).ok_or(Error::TrackNotFound(id))?;
                aligned += track.align_cuts_to_beats(&beats, tolerance, &limits);
            }
            if let Some(before) = before {
                let mut trimmed = Vec::new();
                let carried = timeline.carry_links_since_noted(&before, &limits, &mut trimmed)?;
                self.edit_notes.borrow_mut().extend(trimmed);
                self.edit_carried.borrow_mut().extend(carried);
            }
            Ok(aligned)
        })
    }

    /// What "fit music to length" would make of a music clip, without making it: the
    /// segments, how long they last and how far that is from `target` (by default the
    /// picture's length from the clip's start). See [`crate::model::plan_music_fit`].
    pub fn plan_music_fit(&self, clip_id: Uuid, target: Option<f64>) -> Result<MusicFit> {
        Ok(self.music_fit_inputs(clip_id, target)?.2)
    }

    fn music_fit_inputs(&self, clip_id: Uuid, target: Option<f64>) -> Result<(Clip, u32, MusicFit)> {
        let timeline = self.working_timeline()?;
        let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        if timeline.tracks[ti].kind != StreamKind::Audio {
            return Err(Error::InvalidArgument(
                "fit music to length works on a clip on an audio track".to_string(),
            ));
        }
        let clip = timeline.tracks[ti].clips[ci].clone();
        if (clip.speed - 1.0).abs() > 1e-9 || clip.is_ramped() {
            return Err(Error::InvalidArgument(
                "fit music to length needs the clip at normal speed".to_string(),
            ));
        }
        let asset = self.require_asset(clip.asset_id)?;
        let music = self.get_analysis(asset.id)?.and_then(|a| a.music).ok_or_else(|| {
            Error::InvalidArgument("this music has no bar grid — analyze the asset first (it needs a steady beat)".to_string())
        })?;
        let target = match target {
            Some(t) if t.is_finite() && t > 0.0 => t,
            Some(_) => {
                return Err(Error::InvalidArgument(
                    "target must be a positive number of seconds".to_string(),
                ))
            }
            None => {
                let picture_end = timeline
                    .tracks
                    .iter()
                    .filter(|t| t.kind == StreamKind::Video)
                    .map(Track::end)
                    .fold(0.0, f64::max);
                let t = picture_end - clip.timeline_start;
                if t <= 0.0 {
                    return Err(Error::InvalidArgument(
                        "there is no picture after this clip's start to fit it to".to_string(),
                    ));
                }
                t
            }
        };
        if target / music.grid.bar_s() > MAX_FIT_BARS as f64 {
            return Err(Error::InvalidArgument("that target is too long to fit music to".to_string()));
        }
        let rate = asset
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Audio)
            .and_then(|s| s.sample_rate)
            .unwrap_or(48_000);
        Ok((clip, rate, plan_music_fit(&music, target, rate)))
    }

    /// **Fit music to length**: replace a music clip with the bar-aligned edit list
    /// [`Self::plan_music_fit`] plans — intro and ending kept, whole phrases repeated or
    /// dropped, each splice crossfaded over a 10 ms window centred on it. Non-destructive:
    /// the result is clips on the source, not rendered audio. With `fade_out`, a fit that
    /// runs over its target is cut there and faded out. One revision; refused when the
    /// result would run into the next clip on the track.
    pub fn fit_music(&self, clip_id: Uuid, target: Option<f64>, fade_out: bool) -> Result<MusicFitReport> {
        let (clip, rate, fit) = self.music_fit_inputs(clip_id, target)?;
        if clip.link_id.is_some() {
            return Err(Error::InvalidArgument(
                "unlink the music from its picture before fitting it".to_string(),
            ));
        }
        let clips = music_fit_clips(&clip, &fit, rate, fade_out);
        self.edit_timeline_exact("Fit music to length", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let track = &mut timeline.tracks[ti];
            if track.locked {
                return Err(Error::InvalidArgument(format!("{} is locked", track.name)));
            }
            track.clips.remove(ci);
            let start = clips.first().map_or(clip.timeline_start, |c| c.timeline_start);
            let end = clips.last().map_or(start, Clip::timeline_end);
            if track
                .clips
                .iter()
                .any(|c| c.timeline_start < end - 1e-9 && c.timeline_end() > start + 1e-9)
            {
                return Err(Error::InvalidArgument(format!(
                    "the fitted music ({}) would run into the next clip on {} — make room first",
                    fmt_time(end - start),
                    track.name
                )));
            }
            for (k, c) in clips.iter().enumerate() {
                track.clips.insert(ci + k, c.clone());
            }
            let duration = end - start;
            Ok(MusicFitReport {
                clips: clips.iter().map(|c| c.id).collect(),
                faded: fade_out && fit.remainder < 0.0,
                duration,
                fit,
            })
        })
    }

    /// **Extract audio**: give an asset's sound its own clip on an audio track, for
    /// every use of the asset on the timeline that still plays it.
    ///
    /// **Verified bug, fixed here:** this used to append the asset's whole audio to
    /// the first audio track *and leave the picture's own sound on*, so an asset
    /// already cut onto V1 was heard twice — the export mixes the audio of every
    /// clip whose asset carries an audio stream, video tracks included (see
    /// `doubled_audio_is_what_extract_audio_used_to_make` in the engine tests).
    /// Now each clip of the asset on a video track still playing its own sound is
    /// **detached** ([`Project::detach_audio`]: an audio clip with the same span and
    /// position, linked, the picture muted) in one `Extract audio` revision. A clip
    /// that cannot be (its track is locked) is **skipped and reported**, not a reason
    /// to fail the rest; nothing to extract — the asset is only in the bin, or its
    /// sound is already detached — is an error saying so, not a quiet fall-through.
    /// To put an asset's whole audio on an audio track, as itself, that is
    /// [`Project::add_asset_audio`].
    pub fn extract_audio(&self, asset_id: Uuid) -> Result<DetachedMany> {
        let asset = self.require_asset(asset_id)?;
        if !asset.has_audio() {
            return Err(Error::InvalidArgument("asset has no audio stream".to_string()));
        }
        let label = |_: &DetachedMany| "Extract audio".to_string();
        self.edit_named_exact(&[], label, |timeline| {
            let on_video: Vec<&Clip> = timeline
                .tracks
                .iter()
                .filter(|t| t.kind == StreamKind::Video)
                .flat_map(|t| t.clips.iter())
                .filter(|c| c.asset_id == asset_id)
                .collect();
            let sounding: Vec<Uuid> = on_video.iter().filter(|c| c.source_audio).map(|c| c.id).collect();
            if sounding.is_empty() {
                return Err(Error::InvalidArgument(if on_video.is_empty() {
                    "no clip of this asset is on a video track, so there is no sound of its own to extract — add_asset_audio puts its audio on an audio track".to_string()
                } else {
                    "this asset's sound is already on an audio track".to_string()
                }));
            }
            timeline.detach_audio_many(&sounding, &|_| true)
        })
    }

    /// **Add the asset's audio** as a clip of its own: the asset's whole audio, from
    /// the start, appended to the end of the first audio track (created when there is
    /// none). It never touches a picture clip, so an asset that is *also* on a video
    /// track playing its own sound is heard twice where the two overlap — for the
    /// sound of a clip already cut, that is [`Project::extract_audio`] /
    /// [`Project::detach_audio`], which mute the picture. One `Add audio` revision.
    pub fn add_asset_audio(&self, asset_id: Uuid) -> Result<Clip> {
        let asset = self.require_asset(asset_id)?;
        if !asset.has_audio() {
            return Err(Error::InvalidArgument("asset has no audio stream".to_string()));
        }
        self.edit_timeline_exact("Add audio", |timeline| {
            // Every audio track locked is no reason to refuse: the sound gets a track of its own, as a
            // detach does.
            let tid = match timeline.first_unlocked_track_of(StreamKind::Audio) {
                Some(tid) => tid,
                None => {
                    let n = timeline.tracks.iter().filter(|t| t.kind == StreamKind::Audio).count();
                    let track = Track::new(StreamKind::Audio, format!("A{}", n + 1));
                    let tid = track.id;
                    timeline.tracks.push(track);
                    tid
                }
            };
            let start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
            let clip = Clip::for_asset(&asset, 0.0, asset.duration, start);
            timeline.track_mut(tid).unwrap().clips.push(clip.clone());
            Ok(clip)
        })
    }

    /// **Detach audio**: split a picture clip's own sound off onto an audio track.
    /// A new audio clip with the same source span, speed and timeline position goes
    /// on the audio track at the picture's own position (V1 → A1) when it has room,
    /// else the first audio track that does, else a new one; it is **linked** to the
    /// picture clip (so they move, trim, split and delete together), and the picture
    /// clip's own sound is muted — the sound is heard once, from the audio track.
    /// The picture track's fader is folded into the new clip's gain, so the **level**
    /// is what it was; the destination track's pan, duck and mute/solo now decide the
    /// rest of the mix (see [`Timeline::detach_audio`]). The audio clip carries volume,
    /// audio effects, fades and the transition; the picture keeps its own, inert
    /// while muted. One `Detach audio` revision. Refuses a clip that is not on a video
    /// track, whose asset has no audio, whose sound is already detached, or whose
    /// track or destination is locked.
    pub fn detach_audio(&self, clip_id: Uuid) -> Result<Detached> {
        let timeline = self.working_timeline()?;
        let clip = timeline.clip(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        // A title has no asset; the timeline says why it has nothing to detach.
        let has_audio = !clip.is_generated() && self.require_asset(clip.asset_id)?.has_audio();
        self.edit_timeline_exact("Detach audio", |timeline| timeline.detach_audio(clip_id, has_audio))
    }

    /// [`Project::detach_audio`] on several clips as **one** `Detach audio (N clips)`
    /// revision — a multi-select detach that undoes in a step. A clip that cannot be
    /// detached (not a video clip, an asset without audio, already detached, a
    /// locked track) is **skipped and reported**; the call fails only when nothing
    /// could be detached, so it never records an empty edit.
    pub fn detach_audio_clips(&self, clip_ids: &[Uuid]) -> Result<DetachedMany> {
        let timeline = self.working_timeline()?;
        let mut has_audio: HashMap<Uuid, bool> = HashMap::new();
        for id in clip_ids {
            if let Some(clip) = timeline.clip(*id).filter(|c| !c.is_generated()) {
                if let std::collections::hash_map::Entry::Vacant(slot) = has_audio.entry(clip.asset_id) {
                    slot.insert(self.require_asset(clip.asset_id)?.has_audio());
                }
            }
        }
        let label = |done: &DetachedMany| match done.detached.len() {
            1 => "Detach audio".to_string(),
            n => format!("Detach audio ({n} clips)"),
        };
        self.edit_named_exact(&[], label, |timeline| {
            timeline.detach_audio_many(clip_ids, &|asset| has_audio.get(&asset).copied().unwrap_or(false))
        })
    }

    /// **Reattach audio**: the inverse of [`Project::detach_audio`] — delete the
    /// linked audio clip(s) carrying the picture's asset and let the picture play its
    /// own sound again. Name either clip of the pair. Refused when the picture has
    /// no audio partner to delete *and* another clip is already playing its sound
    /// (unmuting would double it). One `Reattach audio` revision; returns the picture
    /// clip. It is never rippled: deleting the sound is not a change in how much
    /// footage sits ahead of anything.
    pub fn reattach_audio(&self, clip_id: Uuid) -> Result<Clip> {
        self.edit_timeline_exact("Reattach audio", |timeline| timeline.reattach_audio(clip_id))
    }

    /// [`Project::reattach_audio`] on several clips as **one** `Reattach audio (N clips)`
    /// revision — a multi-select reattach that undoes in a step. Name either clip of each
    /// pair. All or nothing, unlike [`Project::detach_audio_clips`]: a clip that cannot be
    /// reattached refuses the lot (the error names it) and records nothing. Returns the
    /// pictures.
    pub fn reattach_audio_clips(&self, clip_ids: &[Uuid]) -> Result<Vec<Clip>> {
        let label = |done: &Vec<Clip>| match done.len() {
            1 => "Reattach audio".to_string(),
            n => format!("Reattach audio ({n} clips)"),
        };
        self.edit_named_exact(&[], label, |timeline| timeline.reattach_audio_many(clip_ids))
    }

    /// **Link** clips into one group (one `Link N clips` revision): from then on an
    /// edit to one is carried to the others (see [`Project::with_links`] for the
    /// escape hatch). At least two clips, on different tracks, none on a locked
    /// track. Returns the group's link id.
    pub fn link_clips(&self, clip_ids: &[Uuid]) -> Result<Uuid> {
        self.edit_timeline_exact(&format!("Link {} clips", clip_ids.len()), |timeline| {
            timeline.link_clips(clip_ids)
        })
    }

    /// **Unlink** clips (one `Unlink clips` revision): each leaves its group and a
    /// group left with a single clip dissolves. Errors when none was linked.
    /// Returns how many of the named clips were.
    pub fn unlink_clips(&self, clip_ids: &[Uuid]) -> Result<usize> {
        self.edit_timeline_exact("Unlink clips", |timeline| timeline.unlink_clips(clip_ids))
    }

    /// Append the full length of each asset sequentially (stitch). One atomic
    /// edit — a single timeline write and one "Concatenate" revision — rather
    /// than one `cut_clip` (and one undo step) per asset.
    pub fn concatenate(&self, asset_ids: &[Uuid]) -> Result<Vec<Clip>> {
        // Validate every asset up front so the edit either fully applies or not
        // at all (no partial stitch left behind on a bad id).
        let mut plan = Vec::with_capacity(asset_ids.len());
        for &asset_id in asset_ids {
            let asset = self.require_asset(asset_id)?;
            plan.push((asset.primary_kind(), asset));
        }
        self.edit_timeline("Concatenate", |timeline| {
            let mut clips = Vec::with_capacity(plan.len());
            for (primary, asset) in &plan {
                let tid = default_track(timeline, *primary)?;
                let start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
                let clip = Clip::for_asset(asset, 0.0, asset.duration, start);
                timeline.track_mut(tid).unwrap().clips.push(clip.clone());
                clips.push(clip);
            }
            Ok(clips)
        })
    }
}
