//! Tracks, the master bus and the delivery format, and a clip's volume, fade, speed and transform.

use super::*;

impl Project {
    /// Append a new empty track of `kind`, keeping kinds grouped (video tracks
    /// before audio tracks, the order the project stores) and auto-naming it
    /// (`V2`, `A2`, …) when `name` is omitted. Later video tracks composite **on
    /// top** at export; the timeline draws them that way up, V2 above V1.
    pub fn add_track(&self, kind: StreamKind, name: Option<String>) -> Result<Track> {
        self.edit_timeline("Add track", |timeline| {
            let count = timeline.tracks.iter().filter(|t| t.kind == kind).count();
            let name = name.unwrap_or_else(|| {
                let prefix = if kind == StreamKind::Audio { "A" } else { "V" };
                format!("{prefix}{}", count + 1)
            });
            let track = Track::new(kind, name);
            // Insert video tracks just after the last video track and audio
            // tracks at the very end, so the lanes stay grouped (V1, V2, …, A1, A2).
            let at = match kind {
                StreamKind::Audio => timeline.tracks.len(),
                _ => timeline
                    .tracks
                    .iter()
                    .rposition(|t| t.kind == StreamKind::Video)
                    .map(|i| i + 1)
                    .unwrap_or(0),
            };
            timeline.tracks.insert(at, track.clone());
            Ok(track)
        })
    }

    /// Flag or unflag a track for export-time ducking: a flagged track's audio
    /// is sidechain-compressed under the non-ducked tracks (music dips under
    /// dialogue automatically).
    pub fn set_track_duck(&self, track_id: Uuid, duck: bool) -> Result<Track> {
        self.edit_timeline(if duck { "Duck track" } else { "Unduck track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.duck = duck;
            Ok(track.clone())
        })
    }

    /// Set a track's fader, the gain riding every clip on it. Clamped to
    /// `0..=4` (+12 dB), which is as far up as a fader has any business going.
    pub fn set_track_volume(&self, track_id: Uuid, volume: f32) -> Result<Track> {
        if !volume.is_finite() {
            return Err(Error::InvalidArgument("track level must be a number".to_string()));
        }
        let volume = volume.clamp(0.0, 4.0);
        self.edit_timeline("Set track level", |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.volume = volume;
            Ok(track.clone())
        })
    }

    /// Set a track's stereo placement, -1 (hard left) to 1 (hard right).
    pub fn set_track_pan(&self, track_id: Uuid, pan: f32) -> Result<Track> {
        if !pan.is_finite() {
            return Err(Error::InvalidArgument("track pan must be a number".to_string()));
        }
        let pan = pan.clamp(-1.0, 1.0);
        self.edit_timeline("Set track pan", |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.pan = pan;
            Ok(track.clone())
        })
    }

    /// Set the master fader: the linear gain on the finished mix, after every
    /// track and the duck bus and before `loudnorm`. Clamped to
    /// `0..=`[`MASTER_MAX_VOLUME`] (+12 dB); a value that is not a number is
    /// refused rather than clamped into nonsense. One revision, and an agent's
    /// goes to its staged proposal like any other edit.
    pub fn set_master_volume(&self, volume: f64) -> Result<MasterBus> {
        if !volume.is_finite() {
            return Err(Error::InvalidArgument("master volume must be a number".to_string()));
        }
        let volume = volume.clamp(0.0, MASTER_MAX_VOLUME);
        self.edit_timeline("Set master level", |timeline| {
            timeline.master.volume = volume;
            Ok(timeline.master)
        })
    }

    /// Switch the master limiter on or off, optionally moving its ceiling.
    ///
    /// The limiter holds the finished mix under `ceiling_db` (dBFS, clamped to
    /// `-24..=0`; omitted, it keeps the ceiling it had — `-1` for a master never
    /// touched) so a loud passage cannot clip the delivery. It is a **sample**
    /// peak ceiling: the true peak can land a fraction of a dB above it, which
    /// [`Project::levels`] reports. A ceiling given while switching off is kept
    /// for the next time. One revision.
    pub fn set_master_limiter(&self, enabled: bool, ceiling_db: Option<f64>) -> Result<MasterBus> {
        if ceiling_db.is_some_and(|c| !c.is_finite()) {
            return Err(Error::InvalidArgument("limiter ceiling must be a number".to_string()));
        }
        let ceiling_db = ceiling_db.map(|c| c.clamp(MASTER_MIN_CEILING_DB, 0.0));
        self.edit_timeline("Set master limiter", |timeline| {
            timeline.master.limiter = enabled;
            if let Some(c) = ceiling_db {
                timeline.master.ceiling_db = c;
            }
            Ok(timeline.master)
        })
    }

    /// Make the master limiter hold the **true** peak (it then runs on the mix upsampled
    /// four times) or only the sample peak (the default). Kept while the limiter is off,
    /// like its ceiling. One revision.
    pub fn set_master_true_peak(&self, enabled: bool) -> Result<MasterBus> {
        let label = if enabled { "Limit true peaks" } else { "Limit sample peaks" };
        self.edit_timeline(label, |timeline| {
            timeline.master.true_peak = enabled;
            Ok(timeline.master)
        })
    }

    /// Choose how ducked tracks dip: `None` for the sidechain compressor (the default),
    /// `Some(depth_db)` for the speech gate that lowers them by exactly that much while
    /// the other tracks speak (clamped to `-40..=-1` dB). One revision.
    pub fn set_master_duck(&self, depth_db: Option<f64>) -> Result<MasterBus> {
        if depth_db.is_some_and(|d| !d.is_finite()) {
            return Err(Error::InvalidArgument("duck depth must be a number".to_string()));
        }
        let depth_db = depth_db.map(|d| d.clamp(crate::model::DUCK_MIN_DEPTH_DB, -1.0));
        let label = match depth_db {
            Some(d) => format!("Duck by {d:.0} dB under speech"),
            None => "Duck with the compressor".to_string(),
        };
        self.edit_timeline(&label, |timeline| {
            timeline.master.duck_depth_db = depth_db;
            Ok(timeline.master)
        })
    }

    /// Set (or clear) the frame this project is cut for.
    ///
    /// The delivery frame decides the shape of every rendered picture — the
    /// scrubbed still, the streamed playback and the export — so a vertical cut
    /// is framed against the vertical frame instead of being cropped sight-unseen
    /// at render time. `None` restores the default: the shape follows the
    /// footage. An explicit `ExportOptions::resolution` still overrides it, so
    /// a one-off render at another size is unaffected.
    ///
    /// The frame carries the **frame rate** the cut is delivered at (`Delivery::fps`): an export,
    /// the playback and the stills use it when the export options name none, instead of the rate
    /// of whichever clip is first. It is checked (1 to 240 fps; 23.976 / 29.97 / 59.94 become the
    /// exact NTSC rates) and refused when it is not a rate; `None` follows the footage.
    pub fn set_delivery_format(&self, format: Option<Delivery>) -> Result<Timeline> {
        let format = format
            .map(|d| Delivery::new(d.width, d.height, d.fit).with_fps(d.fps))
            .transpose()?;
        let label = match format {
            Some(d) => match d.fps {
                Some(fps) => format!("Deliver {}x{} at {} fps", d.width, d.height, crate::model::fmt_fps(fps)),
                None => format!("Deliver {}x{}", d.width, d.height),
            },
            None => "Deliver at source shape".to_string(),
        };
        self.edit_timeline(&label, |timeline| {
            timeline.format = format;
            // Preset titles are drawn for the frame they were made at.
            let frame = timeline.preset_frame();
            timeline.regenerate_svg_presets(frame);
            Ok(())
        })?;
        self.working_timeline()
    }

    /// Mute or unmute a track: its clips stop rendering (silent for audio,
    /// hidden for video) while keeping their place on the timeline.
    pub fn set_track_muted(&self, track_id: Uuid, muted: bool) -> Result<Track> {
        self.edit_timeline(if muted { "Mute track" } else { "Unmute track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.muted = muted;
            Ok(track.clone())
        })
    }

    /// Solo or unsolo a track. While any track of a kind is soloed, the other
    /// tracks of that kind stop rendering; several may be soloed at once.
    pub fn set_track_solo(&self, track_id: Uuid, solo: bool) -> Result<Track> {
        self.edit_timeline(if solo { "Solo track" } else { "Unsolo track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.solo = solo;
            Ok(track.clone())
        })
    }

    /// Lock or unlock a track against editing. A locked track still renders;
    /// this only guards its clips from being moved, trimmed or split.
    ///
    /// A lock is the user's: an agent may lock a track but not unlock one (review on or off), so a
    /// lock is never only advisory — the refusal tells it to ask the user.
    pub fn set_track_locked(&self, track_id: Uuid, locked: bool) -> Result<Track> {
        let agent = self.actor == EditSource::Agent;
        self.edit_timeline(if locked { "Lock track" } else { "Unlock track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            if agent && !locked && track.locked {
                return Err(Error::InvalidArgument(format!(
                    "track {} is locked, and an agent cannot unlock a track — ask the user to",
                    track.name
                )));
            }
            track.locked = locked;
            Ok(track.clone())
        })
        .and_then(|track| {
            // In a proposal the agent sees a track as `working_timeline` lays it: a lock it sets on a track
            // the user unlocked since the proposal branched is the inherited one and follows the user, so
            // the answer says so rather than claim a lock that will not hold.
            if agent {
                if let Some(seen) = self.working_timeline()?.track(track_id) {
                    return Ok(seen.clone());
                }
            }
            Ok(track)
        })
    }

    /// Enable or disable a single clip. A disabled clip keeps its position,
    /// trims, effects and keyframes but drops out of the render.
    pub fn set_clip_enabled(&self, clip_id: Uuid, enabled: bool) -> Result<Clip> {
        self.edit_timeline(if enabled { "Enable clip" } else { "Disable clip" }, |timeline| {
            timeline.ensure_editable(clip_id)?;
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].enabled = enabled;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Remove a track and all of its clips. Refuses to remove the last track.
    ///
    /// An agent may not remove a locked track (it is the user's); the user may — deleting their own
    /// locked track is an explicit act.
    pub fn remove_track(&self, track_id: Uuid) -> Result<()> {
        let agent = self.actor == EditSource::Agent;
        self.edit_timeline("Remove track", |timeline| {
            let idx = timeline
                .tracks
                .iter()
                .position(|t| t.id == track_id)
                .ok_or(Error::TrackNotFound(track_id))?;
            if agent && timeline.tracks[idx].locked {
                return Err(Error::InvalidArgument(format!(
                    "track {} is locked — only the user can remove it",
                    timeline.tracks[idx].name
                )));
            }
            if timeline.tracks.len() <= 1 {
                return Err(Error::InvalidArgument("cannot remove the last track".to_string()));
            }
            timeline.tracks.remove(idx);
            Ok(())
        })
    }

    /// Remove a clip from the timeline. Leaves a gap, unless ripple mode is on,
    /// in which case the later clips on its track close it.
    ///
    /// **Linked clips are removed with it** (as one `Remove N clips` revision, all or
    /// nothing, refusing a locked track — the clip's own included, as every group
    /// edit does); `with_links(Some(false))` removes the named clip alone.
    pub fn remove(&self, clip_id: Uuid) -> Result<()> {
        if self.links_active() && self.working_has_links()? && !self.working_timeline()?.link_partners(clip_id).is_empty() {
            return self.remove_clips(&[clip_id]).map(|_| ());
        }
        self.edit_named(
            &[clip_id],
            |_| "Remove clip".to_string(),
            |timeline| {
                timeline.ensure_editable(clip_id)?;
                let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
                timeline.tracks[ti].clips.remove(ci);
                Ok(())
            },
        )
    }

    /// Remove several clips in **one** edit — one revision — for a multi-select
    /// delete. All or nothing: an unknown id, or a clip on a locked track, refuses
    /// the whole thing; a clip named twice is removed once. Leaves gaps — unless
    /// ripple mode is on (or forced with [`Project::with_ripple`]), in which case
    /// every track closes up behind what it lost, which is how a multi-select
    /// *ripple* delete is made. Returns how many clips were removed.
    ///
    /// **Linked clips go too**: every partner of a named clip is removed with it
    /// (and counted), refusing the whole removal if one is on a locked track —
    /// `with_links(Some(false))` removes exactly what is named.
    pub fn remove_clips(&self, clip_ids: &[Uuid]) -> Result<usize> {
        let ripple = self.ripple_active()?;
        let links = self.links_active();
        // The label counts the partners removed with the named clips, which only the
        // edit knows (it returns how many it removed).
        let label = |n: &usize| match (*n, ripple) {
            (1, false) => "Remove clip".to_string(),
            (1, true) => "Ripple delete".to_string(),
            (n, false) => format!("Remove {n} clips"),
            (n, true) => format!("Ripple delete {n} clips"),
        };
        self.edit_named(clip_ids, label, |timeline| {
            if links {
                timeline.remove_clips_linked(clip_ids)
            } else {
                timeline.remove_clips(clip_ids)
            }
        })
    }

    /// Set a clip's linear gain, from silence up to [`MAX_CHANNEL_VOLUME`] (+12 dB, the same
    /// ceiling a volume keyframe and the master fader have): a higher value is brought down to it,
    /// a negative or non-numeric one refused.
    pub fn set_volume(&self, clip_id: Uuid, volume: f32) -> Result<Clip> {
        if !volume.is_finite() || volume < 0.0 {
            return Err(Error::InvalidArgument("volume must be a number >= 0".to_string()));
        }
        let volume = volume.min(MAX_CHANNEL_VOLUME as f32);
        self.edit_timeline("Set volume", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].volume = volume;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Set a clip's fade-in and/or fade-out duration (seconds). `None` leaves a
    /// value unchanged; pass `Some(0.0)` to clear a fade. Negative values are
    /// rejected. The fade is realized at export (see the engine render path).
    pub fn set_fade(&self, clip_id: Uuid, fade_in: Option<f64>, fade_out: Option<f64>) -> Result<Clip> {
        Clip::check_fades(fade_in, fade_out)?;
        self.edit_timeline("Set fade", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            if let Some(value) = fade_in {
                clip.fade_in = value;
            }
            if let Some(value) = fade_out {
                clip.fade_out = value;
            }
            Ok(clip.clone())
        })
    }

    /// Set a clip's playback speed (1.0 = unchanged, negative = reverse). The
    /// magnitude is clamped away from zero so the duration stays finite. Changing
    /// speed retimes the clip and so changes its timeline duration (like a trim).
    ///
    /// **Linked clips are retimed by the same ratio**, so a picture and its sound
    /// keep step (a partner at 1× next to a clip going 1× → 2× goes to 2×); each
    /// track's length changes by its own, and ripple mode follows per track.
    /// `with_links(Some(false))` retimes the named clip alone. A constant speed replaces a speed
    /// ramp ([`Project::set_speed_ramp`]), on every clip it reaches.
    pub fn set_speed(&self, clip_id: Uuid, speed: f64) -> Result<Clip> {
        if !speed.is_finite() || speed == 0.0 {
            return Err(Error::InvalidArgument("speed must be a non-zero, finite number".to_string()));
        }
        if !(MIN_SPEED..=MAX_SPEED).contains(&speed.abs()) {
            return Err(Error::InvalidArgument(format!(
                "speed must be between {MIN_SPEED}× and {MAX_SPEED}× (negative to reverse), got {speed}"
            )));
        }
        let links = self.links_active();
        self.edit_named(
            &[clip_id],
            |_| "Set speed".to_string(),
            |timeline| {
                timeline.ensure_editable(clip_id)?;
                if links {
                    return timeline.set_speed_linked(clip_id, speed);
                }
                let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
                timeline.tracks[ti].clips[ci].speed = speed;
                timeline.tracks[ti].clips[ci].speed_ramp.clear();
                Ok(timeline.tracks[ti].clips[ci].clone())
            },
        )
    }

    /// Give a clip a **speed ramp**: its rate changes over its length instead of holding one value
    /// (`Clip::speed_ramp`, `model/speed_ramp.rs`). Each key is a clip-local timeline time (seconds from
    /// the clip's start, so a head trim carries the ramp with the footage) and a speed magnitude
    /// (`MIN_SPEED..=MAX_SPEED`); the speed moves in a straight line to the next key, or stays until it
    /// (`RampEasing::Hold`), runs at the first key's speed before it and the last key's after. The clip
    /// keeps its direction (its `speed`'s sign); its magnitude is not read while a ramp is set. Keys are
    /// put in time order; none clears the ramp, back to the clip's own `speed`.
    ///
    /// The source window stays and the clip's length changes with the ramp, so — like [`Project::set_speed`]
    /// — it ripples in ripple mode. **Linked clips get the same ramp on the timeline** (a key 3 s into the
    /// named clip lands at that moment of a partner that starts elsewhere), re-placed as for a speed
    /// change; `with_links(Some(false))` ramps the named clip alone. Errors on bad keys, a locked
    /// track, and a clear of a ramp that is not there.
    pub fn set_speed_ramp(&self, clip_id: Uuid, keys: Vec<SpeedKey>) -> Result<Clip> {
        let keys = check_speed_ramp(keys)?;
        let links = self.links_active();
        self.edit_named(
            &[clip_id],
            |_| "Set speed ramp".to_string(),
            |timeline| {
                timeline.ensure_editable(clip_id)?;
                if links {
                    return timeline.set_speed_ramp_linked(clip_id, keys);
                }
                timeline.set_speed_ramp(clip_id, keys)
            },
        )
    }

    /// Update a clip's geometric transform. Each `None` leaves that field
    /// unchanged. Realized when compositing at export.
    #[allow(clippy::too_many_arguments)]
    pub fn set_transform(
        &self,
        clip_id: Uuid,
        scale: Option<f64>,
        pos_x: Option<f64>,
        pos_y: Option<f64>,
        rotation: Option<f64>,
        opacity: Option<f64>,
        crop_left: Option<f64>,
        crop_right: Option<f64>,
        crop_top: Option<f64>,
        crop_bottom: Option<f64>,
    ) -> Result<Clip> {
        let patch = TransformPatch {
            scale,
            pos_x,
            pos_y,
            rotation,
            opacity,
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
        };
        patch.check_values()?;
        self.edit_timeline("Set transform", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let t = &mut timeline.tracks[ti].clips[ci].transform;
            patch.apply(t);
            t.check_crop()?;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Set a clip's transform as it should look `time` seconds into the clip, as one revision:
    /// a number that is animated gets a key there (in the whole-transform keyframes or in its own
    /// track, whichever drives it) and every other number — the crop edges always — its static
    /// value. What dragging a clip in the Preview writes.
    pub fn set_transform_at(&self, clip_id: Uuid, time: f64, patch: TransformPatch) -> Result<Clip> {
        if !time.is_finite() || time < 0.0 {
            return Err(Error::InvalidArgument("time must be >= 0".to_string()));
        }
        patch.check_values()?;
        self.edit_timeline("Set transform", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            clip.pose_transform(time, &patch);
            clip.transform.check_crop()?;
            Ok(clip.clone())
        })
    }
}
