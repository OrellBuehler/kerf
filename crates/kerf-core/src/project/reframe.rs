//! 360 footage: an asset's projection and a clip's reframe.

use super::*;

impl Project {
    // ---- 360 reframe ------------------------------------------------------

    /// Point a clip's virtual 360 camera. Each `None` channel is left as it is,
    /// so a caller can nudge yaw alone. A clip that is not yet reframed picks up
    /// a default reframe for its asset's projection first — and an asset that is
    /// not 360 at all is rejected, since reprojecting flat footage is never what
    /// was meant.
    #[allow(clippy::too_many_arguments)]
    /// Record (or clear, with `None`) the spherical projection of an asset's
    /// video, overriding what probing decided.
    ///
    /// Detection is deliberately conservative — a 360 file carrying no spherical
    /// metadata and no recognizable geometry probes as flat — so this is the
    /// escape hatch for footage Kerf could not identify. It is a property of the
    /// *asset*, not of one clip: every clip cut from it afterwards is reframed by
    /// default ([`Clip::for_asset`]), and it survives save/reopen. Clips already
    /// on the timeline keep whatever reframe they have.
    pub fn set_asset_projection(&self, asset_id: Uuid, projection: Option<Projection>) -> Result<Asset> {
        if projection.is_some_and(|p| !p.is_spherical()) {
            return Err(Error::InvalidArgument(
                "asset projection must be a spherical projection (equirect, dual_fisheye, fisheye)".to_string(),
            ));
        }
        let mut asset = self.require_asset(asset_id)?;
        if !asset.streams.iter().any(|s| s.kind == StreamKind::Video) {
            return Err(Error::InvalidArgument(
                "cannot set a projection on an asset with no video stream".to_string(),
            ));
        }
        // An agent's change to the library is held back with its proposal like any other edit of
        // its (and with review on it opens one), landing when the user accepts.
        if self.actor == EditSource::Agent && (self.staged_exists()? || self.agent_review.get()) {
            return self.stage_asset_projection(asset, projection);
        }
        for stream in asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            stream.projection = projection;
        }
        self.insert_asset(&asset)?;
        Ok(asset)
    }

    /// Hold an asset's new projection back with the agent's proposal (opening one if none is).
    fn stage_asset_projection(&self, mut asset: Asset, projection: Option<Projection>) -> Result<Asset> {
        let stored = self.stored_asset(asset.id)?.ok_or(Error::AssetNotFound(asset.id))?;
        let id = StagedAssetChange::projection_id(asset.id);
        let label = match projection {
            Some(p) => format!("Mark {} as {}", asset.name, projection_label(p)),
            None => format!("Mark {} as flat (not 360)", asset.name),
        };
        let change = (stored.projection() != projection).then(|| StagedAssetChange {
            id: id.clone(),
            asset_id: asset.id,
            projection,
            tag_edit: None,
            summary: label.clone(),
        });
        self.stage_asset_changes(label, vec![(id, change)])?;
        for stream in asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            stream.projection = projection;
        }
        Ok(asset)
    }

    /// Hold asset changes back with the agent's proposal (opening one if none is) as one undoable step. Each
    /// entry is a change id and the change to hold under it, which replaces an earlier one with that id; `None`
    /// asks for what the library already has, so it stages nothing but takes an earlier ask back. A call that
    /// moves nothing leaves no proposal behind.
    pub(super) fn stage_asset_changes(&self, label: String, changes: Vec<(String, Option<StagedAssetChange>)>) -> Result<()> {
        let declared = self.agent_owner.borrow().clone();
        let tx = self.tx()?;
        if !self.staged_exists()? {
            self.open_session(None, None, declared.as_deref())?;
        }
        let row = self.staged_row()?.ok_or(Error::NoStagedEdit)?;
        if declared.is_some() && owned_by_other(row.owner.as_deref(), declared.as_deref()) {
            return Err(Error::StagedEditOwnedByOther);
        }
        self.adopt_staged(row.owner.as_deref())?;
        let mut assets = row.assets.clone();
        let mut moved = false;
        for (id, change) in changes {
            let had = assets.iter().any(|c| c.id == id);
            assets.retain(|c| c.id != id);
            moved |= had || change.is_some();
            assets.extend(change);
        }
        if !moved {
            return Ok(());
        }
        self.push_staged_step(&row, &row.timeline, &assets, label)?;
        tx.commit()?;
        Ok(())
    }

    /// One `Option` per `v360` parameter: the Tauri command and the MCP tool both
    /// patch a subset, so the arity follows the filter's, not a struct's.
    #[allow(clippy::too_many_arguments)]
    pub fn set_reframe(
        &self,
        clip_id: Uuid,
        yaw: Option<f64>,
        pitch: Option<f64>,
        roll: Option<f64>,
        fov: Option<f64>,
        lens_fov: Option<f64>,
        input: Option<Projection>,
        output: Option<Projection>,
    ) -> Result<Clip> {
        validate_angle("yaw", yaw)?;
        validate_angle("pitch", pitch)?;
        validate_angle("roll", roll)?;
        validate_fov(fov)?;
        validate_lens_fov(lens_fov)?;
        if input.is_some_and(|p| !p.is_spherical()) {
            return Err(Error::InvalidArgument(
                "reframe input must be a spherical projection (equirect, dual_fisheye, fisheye)".to_string(),
            ));
        }
        if output.is_some_and(|p| !matches!(p, Projection::Flat | Projection::Equirect)) {
            return Err(Error::InvalidArgument("reframe output must be flat or equirect".to_string()));
        }
        let fallback = self.clip_asset_projection(clip_id)?;
        self.edit_timeline("Set reframe", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let rf = match clip.reframe.as_mut() {
                Some(rf) => rf,
                None => {
                    let seed = input.or(fallback).ok_or_else(|| {
                        Error::InvalidArgument(
                            "this clip's asset is not 360 footage; pass an explicit input projection to reframe it anyway"
                                .to_string(),
                        )
                    })?;
                    clip.reframe.insert(Reframe::new(seed))
                }
            };
            if let Some(v) = yaw {
                rf.yaw = v;
            }
            if let Some(v) = pitch {
                rf.pitch = v;
            }
            if let Some(v) = roll {
                rf.roll = v;
            }
            if let Some(v) = fov {
                rf.fov = v;
            }
            if let Some(v) = lens_fov {
                rf.lens_fov = v;
            }
            if let Some(v) = input {
                rf.input = v;
            }
            if let Some(v) = output {
                rf.output = v;
            }
            Ok(clip.clone())
        })
    }

    /// Stop reprojecting a clip, leaving its source projection untouched (a raw
    /// equirect or dual-fisheye picture on the timeline).
    pub fn clear_reframe(&self, clip_id: Uuid) -> Result<Clip> {
        self.edit_timeline("Clear reframe", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].reframe = None;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Replace a clip's camera animation (re-sorted by time). An empty list
    /// clears it, so the static pose is used again.
    pub fn set_reframe_keyframes(&self, clip_id: Uuid, mut keyframes: Vec<ReframeKeyframe>) -> Result<Clip> {
        for k in &keyframes {
            validate_reframe_keyframe(k)?;
        }
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.edit_timeline("Set reframe keyframes", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let rf = clip
                .reframe
                .as_mut()
                .ok_or_else(|| Error::InvalidArgument("this clip is not reframed".to_string()))?;
            rf.keyframes = keyframes;
            Ok(clip.clone())
        })
    }

    /// Add a camera keyframe at `time` seconds from the clip's start (replacing
    /// any keyframe already there). Each `None` channel captures the camera's
    /// current sampled pose, so a lone keyframe pins where it is now.
    pub fn add_reframe_keyframe(
        &self,
        clip_id: Uuid,
        time: f64,
        yaw: Option<f64>,
        pitch: Option<f64>,
        roll: Option<f64>,
        fov: Option<f64>,
    ) -> Result<Clip> {
        if !time.is_finite() || time < 0.0 {
            return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
        }
        validate_angle("yaw", yaw)?;
        validate_angle("pitch", pitch)?;
        validate_angle("roll", roll)?;
        validate_fov(fov)?;
        self.edit_timeline("Add reframe keyframe", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let pose = clip
                .reframe_at(time)
                .ok_or_else(|| Error::InvalidArgument("this clip is not reframed".to_string()))?;
            let rf = clip.reframe.as_mut().expect("checked above");
            let mut kf = ReframeKeyframe::from_pose(time, &pose);
            if let Some(v) = yaw {
                kf.yaw = v;
            }
            if let Some(v) = pitch {
                kf.pitch = v;
            }
            if let Some(v) = roll {
                kf.roll = v;
            }
            if let Some(v) = fov {
                kf.fov = v;
            }
            rf.keyframes.retain(|k| (k.time - time).abs() > 1e-6);
            rf.keyframes.push(kf);
            rf.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
            Ok(clip.clone())
        })
    }

    /// The projection of the asset a clip references, if it is 360 footage.
    fn clip_asset_projection(&self, clip_id: Uuid) -> Result<Option<Projection>> {
        let timeline = self.working_timeline()?;
        let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let asset_id = timeline.tracks[ti].clips[ci].asset_id;
        Ok(self.get_asset(asset_id)?.and_then(|a| a.projection()))
    }
}
