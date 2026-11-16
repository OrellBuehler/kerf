//! A clip's colour, grade, mask, blend, transition, effects, keyframes and per-property channels.

use super::*;

impl Project {
    /// Update a clip's color correction. Each `None` leaves that field unchanged.
    pub fn set_color(
        &self,
        clip_id: Uuid,
        brightness: Option<f64>,
        contrast: Option<f64>,
        saturation: Option<f64>,
        gamma: Option<f64>,
        temperature: Option<f64>,
    ) -> Result<Clip> {
        Color::check_values(brightness, contrast, saturation, gamma, temperature)?;
        self.edit_timeline("Set color", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let c = &mut timeline.tracks[ti].clips[ci].color;
            if let Some(v) = brightness {
                c.brightness = v;
            }
            if let Some(v) = contrast {
                c.contrast = v;
            }
            if let Some(v) = saturation {
                c.saturation = v;
            }
            if let Some(v) = gamma {
                c.gamma = v;
            }
            if let Some(v) = temperature {
                c.temperature = v;
            }
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Replace a clip's curves, colour wheels and LUT ([`Grade`]), normalized (points clamped and
    /// sorted, identity curves and neutral wheels dropped). The LUT's file is not read here: check it
    /// first, off the project lock, with [`crate::color_lut::check_grade`].
    pub fn set_grade(&self, clip_id: Uuid, grade: Grade) -> Result<Clip> {
        let grade = grade.normalized();
        if let Some(lut) = &grade.lut {
            if !std::path::Path::new(&lut.path).is_absolute() {
                return Err(Error::InvalidArgument(format!("the LUT path must be absolute: {}", lut.path)));
            }
        }
        self.edit_timeline("Set grade", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].grade = grade;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Cut a clip to a shape, or clear the mask. Outside the shape the clip goes
    /// transparent, so whatever is on a lower track shows through — which is how
    /// a face is blurred (a masked, blurred copy of the shot on the track above),
    /// how a picture-in-picture is rounded off, and how one region gets its own
    /// grade. Fields are clamped, since a zero-width shape would blank the clip.
    pub fn set_mask(&self, clip_id: Uuid, mask: Option<Mask>) -> Result<Clip> {
        let mask = mask.map(Mask::normalized);
        self.edit_timeline(if mask.is_some() { "Mask clip" } else { "Clear mask" }, |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            clip.mask = mask;
            // Clearing the mask clears its keys: there is nothing left for them to move.
            if clip.mask.is_none() {
                clip.channels.retain(|c| !c.prop.is_mask());
            }
            Ok(clip.clone())
        })
    }

    /// How a clip composites onto the tracks below ([`BlendMode`]); `Normal` is plain alpha-over.
    /// Only a clip on a video track draws a picture to blend.
    pub fn set_blend_mode(&self, clip_id: Uuid, mode: BlendMode) -> Result<Clip> {
        self.edit_timeline("Set blend mode", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            if timeline.tracks[ti].kind != StreamKind::Video {
                return Err(Error::InvalidArgument(
                    "a blend mode applies to a clip on a video track".to_string(),
                ));
            }
            let clip = &mut timeline.tracks[ti].clips[ci];
            clip.blend_mode = mode;
            Ok(clip.clone())
        })
    }

    /// Set or clear (`None`) the transition that blends a clip's start with the
    /// clip preceding it on the same track. Realized at export.
    pub fn set_transition(&self, clip_id: Uuid, transition: Option<Transition>) -> Result<Clip> {
        if transition.is_some_and(|t| !t.duration.is_finite() || t.duration <= 0.0) {
            return Err(Error::InvalidArgument("transition duration must be > 0".to_string()));
        }
        self.edit_timeline("Set transition", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].transition_in = transition;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    // ---- per-clip video / audio effects -----------------------------------

    /// Replace a clip's video effect chain (applied in order at export).
    pub fn set_video_effects(&self, clip_id: Uuid, effects: Vec<VideoEffect>) -> Result<Clip> {
        for e in &effects {
            validate_video_effect(e)?;
        }
        self.edit_timeline("Set video effects", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].effects = effects;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Replace a clip's audio effect chain (applied in order at export).
    pub fn set_audio_effects(&self, clip_id: Uuid, effects: Vec<AudioEffect>) -> Result<Clip> {
        for e in &effects {
            validate_audio_effect(e)?;
        }
        self.edit_timeline("Set audio effects", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].audio = effects;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    // ---- transform keyframes (animation) ----------------------------------

    /// Replace a clip's whole-transform keyframes — the bundle that animates scale, position,
    /// rotation and opacity together — re-sorted by time. An empty list clears **the bundle**:
    /// a transform number that has keys of its own ([`Project::set_property_keyframes`]) keeps
    /// them, so a clip animated that way is still animated afterwards. [`Project::clear_keyframes`]
    /// makes the whole transform static. A track left empty only to hold a number static
    /// against the bundle is dropped with the bundle (it has nothing left to hold it off).
    pub fn set_keyframes(&self, clip_id: Uuid, mut keyframes: Vec<Keyframe>) -> Result<Clip> {
        for k in &keyframes {
            validate_keyframe(k)?;
        }
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.edit_timeline("Set keyframes", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            clip.keyframes = keyframes;
            clip.prune_channels();
            Ok(clip.clone())
        })
    }

    /// Add a keyframe at `time` seconds from the clip's start (replacing any
    /// keyframe already at that time, and keeping how that one leaves). Each `None`
    /// channel captures the clip's current sampled transform there, so a lone keyframe
    /// "pins" the present pose. A key that lands inside an eased segment splits it
    /// ([`Clip::insert_keyframe`]): a hold stays held, a curve stays the same curve.
    /// Realized as animation at export when ≥1 keyframe exists.
    #[allow(clippy::too_many_arguments)]
    pub fn add_keyframe(
        &self,
        clip_id: Uuid,
        time: f64,
        scale: Option<f64>,
        pos_x: Option<f64>,
        pos_y: Option<f64>,
        rotation: Option<f64>,
        opacity: Option<f64>,
    ) -> Result<Clip> {
        if !time.is_finite() || time < 0.0 {
            return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
        }
        if scale.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err(Error::InvalidArgument("scale must be a finite value > 0".to_string()));
        }
        if [pos_x, pos_y, rotation].into_iter().flatten().any(|v| !v.is_finite()) {
            return Err(Error::InvalidArgument(
                "position and rotation must be finite numbers".to_string(),
            ));
        }
        if opacity.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
            return Err(Error::InvalidArgument("opacity must be within 0.0..=1.0".to_string()));
        }
        self.edit_timeline("Add keyframe", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let mut kf = Keyframe::from_transform(time, &clip.base_transform_at(time));
            if let Some(v) = scale {
                kf.scale = v;
            }
            if let Some(v) = pos_x {
                kf.pos_x = v;
            }
            if let Some(v) = pos_y {
                kf.pos_y = v;
            }
            if let Some(v) = rotation {
                kf.rotation = v;
            }
            if let Some(v) = opacity {
                kf.opacity = v;
            }
            clip.insert_keyframe(kf);
            // A number with a track of its own ignores the bundle's key: what was asked for it
            // goes into that track.
            for (prop, value) in [
                (Property::Scale, scale),
                (Property::PosX, pos_x),
                (Property::PosY, pos_y),
                (Property::Rotation, rotation),
                (Property::Opacity, opacity),
            ] {
                if let (Some(v), true) = (value, clip.channel(prop).is_some()) {
                    clip.insert_property_key(prop, PropertyKey::new(time, v));
                }
            }
            Ok(clip.clone())
        })
    }

    /// Set the easing of the segment that leaves the keyframe at `time` seconds from the
    /// clip's start (the key nearest it within a millisecond): how the clip travels from that
    /// pose to the next. The last key's easing has nothing to shape until a key follows it.
    pub fn set_keyframe_easing(&self, clip_id: Uuid, time: f64, easing: Easing) -> Result<Clip> {
        finite("keyframe time", time)?;
        validate_easing(&easing)?;
        self.edit_timeline("Set keyframe easing", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let near = |k: &Keyframe| (k.time - time).abs() <= 1e-3;
            let bundled = clip.keyframes.iter().any(near);
            // The transform key at that moment: the bundle's, and the key of every transform
            // number that has taken a track of its own off the bundle.
            let tracked: Vec<Property> = Property::TRANSFORM
                .into_iter()
                .filter(|p| {
                    clip.channel(*p)
                        .is_some_and(|t| t.keys.iter().any(|k| (k.time - time).abs() <= 1e-3))
                })
                .collect();
            if !bundled && tracked.is_empty() {
                return Err(Error::InvalidArgument(format!("the clip has no keyframe at {time:.3} s")));
            }
            if let Some(key) = clip
                .keyframes
                .iter_mut()
                .filter(|k| near(k))
                .min_by(|a, b| (a.time - time).abs().total_cmp(&(b.time - time).abs()))
            {
                key.easing = easing;
            }
            for prop in tracked {
                clip.set_property_easing(prop, time, easing);
            }
            Ok(clip.clone())
        })
    }

    /// Remove all transform keyframes from a clip (back to the static transform): the bundle
    /// and every transform number's own track. Colour and volume keys stay — clear those with
    /// [`Project::set_property_keyframes`].
    pub fn clear_keyframes(&self, clip_id: Uuid) -> Result<Clip> {
        self.edit_timeline("Clear keyframes", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            clip.clear_transform_animation();
            Ok(clip.clone())
        })
    }

    // ---- per-property keyframe channels -----------------------------------

    /// Replace the keys of one animatable number of a clip — scale, position, rotation,
    /// opacity, a colour number or the clip's volume — with `keys` (sorted by time), each a
    /// time in seconds from the clip's start, a value and the easing of the segment leaving it.
    /// No keys leaves the number static again.
    ///
    /// A transform number that the legacy whole-transform keyframes were driving is taken
    /// over by its own track (the others keep the bundle). Values are held to the range the
    /// static setters take (`opacity` 0..=1, `brightness` -1..=1, `volume` 0..=4, ...); a track
    /// is at most [`MAX_CHANNEL_KEYS`] keys long.
    pub fn set_property_keyframes(&self, clip_id: Uuid, prop: Property, keys: Vec<PropertyKey>) -> Result<Clip> {
        validate_property_keys(prop, &keys)?;
        self.edit_timeline(&format!("Set {} keyframes", prop.label()), move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            check_keyable(clip, prop, &keys)?;
            clip.set_property_keys(prop, keys);
            Ok(clip.clone())
        })
    }

    /// Replace the keys of several numbers of a clip in one edit — what one gesture on the dope
    /// sheet writes (a retime across rows, a paste, a delete). Each track is what
    /// [`Project::set_property_keyframes`] would take for its number; a number given twice is
    /// refused, and so is no number at all.
    pub fn set_property_tracks(&self, clip_id: Uuid, tracks: Vec<PropertyTrack>) -> Result<Clip> {
        if tracks.is_empty() {
            return Err(Error::InvalidArgument("give at least one property's keys".to_string()));
        }
        for (i, t) in tracks.iter().enumerate() {
            if tracks[..i].iter().any(|o| o.prop == t.prop) {
                return Err(Error::InvalidArgument(format!("{} is given more than once", t.prop.label())));
            }
            validate_property_keys(t.prop, &t.keys)?;
        }
        self.edit_timeline("Edit keyframes", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            for t in &tracks {
                check_keyable(clip, t.prop, &t.keys)?;
            }
            for t in tracks {
                clip.set_property_keys(t.prop, t.keys);
            }
            Ok(clip.clone())
        })
    }

    /// Set the easing of the segment that leaves one number's key at `time` seconds from the
    /// clip's start (the key nearest it within a millisecond). The number may be one the
    /// whole-transform keyframes drive: it is taken over by its own track and the others keep
    /// the bundle's easing.
    pub fn set_property_easing(&self, clip_id: Uuid, prop: Property, time: f64, easing: Easing) -> Result<Clip> {
        finite("keyframe time", time)?;
        validate_easing(&easing)?;
        self.edit_timeline(&format!("Set {} keyframe easing", prop.label()), move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            if !clip.set_property_easing(prop, time, easing) {
                return Err(Error::InvalidArgument(format!(
                    "the clip has no {} keyframe at {time:.3} s",
                    prop.label()
                )));
            }
            Ok(clip.clone())
        })
    }

    /// Copy the animation of `props` (every keyed number when empty) from one clip to another,
    /// shifted `offset` seconds later (earlier when negative: the head that falls before the
    /// destination's start is cut off and the pose it opens on is pinned there). The
    /// destination's own keys for those numbers are replaced; everything else of it is kept.
    /// Returns the destination clip. Refuses a number the source has no keys for.
    pub fn copy_keyframes(&self, from_clip: Uuid, to_clip: Uuid, props: &[Property], offset: f64) -> Result<Clip> {
        if !offset.is_finite() || offset.abs() > MAX_KEY_OFFSET {
            return Err(Error::InvalidArgument(format!(
                "offset must be a number within ±{MAX_KEY_OFFSET} seconds"
            )));
        }
        if from_clip == to_clip {
            return Err(Error::InvalidArgument(
                "copy keyframes from one clip to another: the two ids are the same clip".to_string(),
            ));
        }
        let props = props.to_vec();
        self.edit_timeline("Copy keyframes", move |timeline| {
            let source = timeline.clip(from_clip).ok_or(Error::ClipNotFound(from_clip))?.clone();
            if timeline.locate(to_clip).is_none() {
                return Err(Error::ClipNotFound(to_clip));
            }
            for p in &props {
                if !source.is_keyed(*p) {
                    return Err(Error::InvalidArgument(format!(
                        "the source clip has no {} keyframes",
                        p.label()
                    )));
                }
            }
            // Every keyed number, when none is named: a mask's numbers only onto a clip that has a
            // mask (they would be refused, and the rest would not be copied).
            let maskless = props.is_empty() && timeline.clip(to_clip).is_some_and(|c| c.mask.is_none());
            let tracks: Vec<PropertyTrack> = source
                .property_keys_shifted(&props, offset)
                .into_iter()
                .filter(|t| !(maskless && t.prop.is_mask()))
                .collect();
            if tracks.is_empty() {
                return Err(Error::InvalidArgument("the source clip has no keyframes to copy".to_string()));
            }
            let (ti, ci) = timeline.locate(to_clip).ok_or(Error::ClipNotFound(to_clip))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            for track in tracks {
                if track.keys.len() > MAX_CHANNEL_KEYS {
                    return Err(Error::InvalidArgument(format!(
                        "{} would have more than {MAX_CHANNEL_KEYS} keyframes",
                        track.prop.label()
                    )));
                }
                check_keyable(clip, track.prop, &track.keys)?;
                clip.set_property_keys(track.prop, track.keys);
            }
            Ok(clip.clone())
        })
    }
}
