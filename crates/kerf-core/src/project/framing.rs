//! Smart crop and the framing for other deliveries: the sampling and what is applied.

use super::*;

/// One clip queued for smart-crop sampling: which media to look at, over which
/// source window, and the shape it was shot in.
#[derive(Debug, Clone)]
pub struct SmartCropJob {
    pub clip_id: Uuid,
    pub path: PathBuf,
    pub start: f64,
    pub end: f64,
    pub width: u32,
    pub height: u32,
}

/// Everything the smart-crop sampler needs, resolved in one pass under the
/// project lock so the decodes it drives can run with the guard dropped.
#[derive(Debug, Clone)]
pub struct SmartCropPlan {
    /// The aspect of the delivery frame — what every job is framed for.
    pub target_aspect: f64,
    pub jobs: Vec<SmartCropJob>,
}

/// A framing pass for a multi-format export: the delivery shapes the cut is
/// about to be rendered at *besides* the one it is cut for, and the clips to
/// frame for each. Same lock-free shape as [`SmartCropPlan`].
#[derive(Debug, Clone)]
pub struct FramingPlan {
    /// The shapes to frame for, in lowest terms, the project frame's own
    /// excluded — its crop is the clip's transform already.
    pub ratios: Vec<(u32, u32)>,
    pub jobs: Vec<SmartCropJob>,
}

impl Project {
    // ---- smart crop ---------------------------------------------------------

    /// Frame each shot for the delivery frame instead of centring it blindly.
    ///
    /// Reshaping a cut — 16:9 footage into a 9:16 Reel — throws away most of the
    /// width, and both fits pick that width without looking: `Cover` takes the
    /// middle, `Contain` keeps everything and shrinks it into a letterboxed
    /// strip. Neither is right when the subject stands in the left third, which
    /// is where a subject usually stands. This samples where each shot's content
    /// actually is and writes the crop that keeps it, per clip — so a cut of six
    /// shots gets six framings rather than one compromise.
    ///
    /// The result is an ordinary `Transform` crop: visible in the inspector,
    /// adjustable by hand, undoable in one step, and rendered by the graph that
    /// was already there. Kerf proposes the framing; the crop sliders remain the
    /// truth. `clip_id` narrows it to one clip; `None` reframes every clip on an
    /// unlocked video track. Returns how many clips moved.
    pub fn smart_crop(&self, clip_id: Option<Uuid>) -> Result<usize> {
        let plan = self.smart_crop_inputs(clip_id)?;
        let crops = Self::sample_smart_crops(&plan)?;
        self.apply_smart_crops(&crops)
    }

    /// Resolve what [`Project::smart_crop`] has to look at, **without** decoding
    /// anything — so a caller can pull this out under the shared project lock and
    /// drop the guard before [`Project::sample_smart_crops`] runs ffmpeg over
    /// every clip. Mirrors [`Project::timeline_frame_inputs`]' shape.
    ///
    /// Clips already the right shape are left out rather than sampled: there is
    /// no window to choose, and a no-op crop written into every clip would only
    /// be noise. A 360 clip is left out too — its `reframe` already aims a camera
    /// at the sphere, and that is the framing decision.
    pub fn smart_crop_inputs(&self, clip_id: Option<Uuid>) -> Result<SmartCropPlan> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        let (fw, fh) = engine::delivery_frame(&timeline, &assets);
        let target_aspect = fw as f64 / fh.max(1) as f64;

        let mut jobs = Vec::new();
        let mut skipped_shape = 0usize;
        for track in timeline.tracks.iter().filter(|t| t.kind == StreamKind::Video) {
            // A locked track is locked against a bulk pass, but naming one of its
            // clips is still an explicit instruction.
            if track.locked && clip_id.is_none() {
                continue;
            }
            for clip in &track.clips {
                if clip_id.is_some_and(|id| id != clip.id) {
                    continue;
                }
                if clip.reframe.is_some() {
                    continue;
                }
                let Some(asset) = assets.iter().find(|a| a.id == clip.asset_id) else {
                    continue;
                };
                let Some((w, h)) = asset
                    .streams
                    .iter()
                    .find(|s| s.kind == StreamKind::Video)
                    .and_then(|s| s.width.zip(s.height))
                else {
                    continue;
                };
                // Nothing to choose when the shot is already the delivery shape.
                if !crate::model::needs_crop(w, h, target_aspect) {
                    skipped_shape += 1;
                    continue;
                }
                // A still has one frame at t=0 and no source timeline to seek
                // into — sampling its clip window would decode nothing.
                let (start, end) = if asset.is_image() {
                    (0.0, 0.04)
                } else {
                    (clip.source_in.min(clip.source_out), clip.source_in.max(clip.source_out))
                };
                jobs.push(SmartCropJob {
                    clip_id: clip.id,
                    path: PathBuf::from(&asset.path),
                    start,
                    end,
                    width: w,
                    height: h,
                });
            }
        }

        if jobs.is_empty() {
            let why = if skipped_shape > 0 {
                format!("every shot is already {fw}x{fh}-shaped — nothing to reframe")
            } else if clip_id.is_some() {
                "that clip cannot be reframed — it is 360 footage, or its asset has no video".to_string()
            } else {
                "no video clips to reframe".to_string()
            };
            return Err(Error::InvalidArgument(why));
        }
        Ok(SmartCropPlan { target_aspect, jobs })
    }

    /// Sample every job in `plan` and pick its crop. Static and lock-free — this
    /// is the slow half (one short ffmpeg decode per clip), so it must not run
    /// with the project locked. A clip whose media cannot be read is skipped
    /// rather than failing the batch; if *nothing* could be read, the first error
    /// is returned so the caller has something to show.
    pub fn sample_smart_crops(plan: &SmartCropPlan) -> Result<Vec<(Uuid, CropFrame)>> {
        Self::sample_smart_crops_with(plan, &mut |_| {}, &|| false)
    }

    /// [`Project::sample_smart_crops`] that reports and can be stopped: `progress` is told the
    /// fraction of clips sampled (`0.0` before the first, then after each) and `cancel` is
    /// polled while a clip decodes — a stop is [`Error::Cancelled`], not a skipped clip.
    pub fn sample_smart_crops_with(
        plan: &SmartCropPlan,
        progress: &mut dyn FnMut(f32),
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<(Uuid, CropFrame)>> {
        let mut crops = Vec::new();
        let mut first_error = None;
        let total = plan.jobs.len().max(1) as f32;
        progress(0.0);
        for (i, job) in plan.jobs.iter().enumerate() {
            let sampled = engine::salience_map_cancellable(&job.path, job.start, job.end, cancel);
            if matches!(sampled, Err(Error::Cancelled)) {
                return Err(Error::Cancelled);
            }
            progress((i + 1) as f32 / total);
            match sampled {
                Ok(map) => {
                    if let Some(crop) = map.crop_for(job.width, job.height, plan.target_aspect) {
                        crops.push((job.clip_id, crop));
                    }
                }
                Err(e) => {
                    tracing::warn!(clip = %job.clip_id, path = %job.path.display(), error = %e, "could not sample a shot for smart crop");
                    first_error.get_or_insert(e);
                }
            }
        }
        match (crops.is_empty(), first_error) {
            (true, Some(e)) => Err(e),
            _ => Ok(crops),
        }
    }

    /// Write sampled crops onto their clips as one undoable edit. Returns how
    /// many clips moved.
    ///
    /// Crops matching what a clip already had are dropped first, so re-running
    /// the pass over an unchanged cut reports 0 *and* leaves the history alone —
    /// a revision that changed nothing is only noise in the edit log.
    pub fn apply_smart_crops(&self, crops: &[(Uuid, CropFrame)]) -> Result<usize> {
        let timeline = self.working_timeline()?;
        let pending: Vec<_> = crops
            .iter()
            .filter(|(clip_id, crop)| {
                timeline.locate(*clip_id).is_some_and(|(ti, ci)| {
                    let t = &timeline.tracks[ti].clips[ci].transform;
                    (t.crop_left, t.crop_right, t.crop_top, t.crop_bottom) != (crop.left, crop.right, crop.top, crop.bottom)
                })
            })
            .collect();
        if pending.is_empty() {
            return Ok(0);
        }
        self.edit_timeline("Smart crop", |timeline| {
            let mut changed = 0;
            for (clip_id, crop) in &pending {
                let Some((ti, ci)) = timeline.locate(*clip_id) else {
                    continue;
                };
                let t = &mut timeline.tracks[ti].clips[ci].transform;
                (t.crop_left, t.crop_right, t.crop_top, t.crop_bottom) = (crop.left, crop.right, crop.top, crop.bottom);
                changed += 1;
            }
            Ok(changed)
        })
    }

    // ---- framing for other deliveries -----------------------------------------

    /// Plan a framing pass for the shapes a multi-format export is about to
    /// render at, **without** decoding anything — the lock-held half, like
    /// [`Project::smart_crop_inputs`].
    ///
    /// Smart crop bakes its answer into each clip's transform for the *one*
    /// frame the project is cut for. Delivering the same cut at a second shape
    /// needs a second answer per clip, kept beside the first ([`Framing`]) —
    /// this plans it for every requested shape that is not the project's own.
    /// A plan with no jobs is not an error: a cut with nothing to frame (every
    /// shape requested is the project's, or there is no flat video) is simply
    /// exported as it is.
    pub fn framing_inputs(&self, deliveries: &[Delivery]) -> Result<FramingPlan> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        let (fw, fh) = engine::delivery_frame(&timeline, &assets);
        let own = Delivery::new(fw, fh, crate::model::Fit::Contain).ratio();

        let mut ratios: Vec<(u32, u32)> = Vec::new();
        for d in deliveries {
            let r = d.ratio();
            if r != own && !ratios.contains(&r) {
                ratios.push(r);
            }
        }

        let mut jobs = Vec::new();
        if !ratios.is_empty() {
            for track in timeline.tracks.iter().filter(|t| t.kind == StreamKind::Video && !t.locked) {
                for clip in &track.clips {
                    // A 360 clip's virtual camera is its framing for every shape.
                    if clip.reframe.is_some() {
                        continue;
                    }
                    let Some(asset) = assets.iter().find(|a| a.id == clip.asset_id) else {
                        continue;
                    };
                    let Some((w, h)) = asset
                        .streams
                        .iter()
                        .find(|s| s.kind == StreamKind::Video)
                        .and_then(|s| s.width.zip(s.height))
                    else {
                        continue;
                    };
                    let (start, end) = if asset.is_image() {
                        (0.0, 0.04)
                    } else {
                        (clip.source_in.min(clip.source_out), clip.source_in.max(clip.source_out))
                    };
                    jobs.push(SmartCropJob {
                        clip_id: clip.id,
                        path: PathBuf::from(&asset.path),
                        start,
                        end,
                        width: w,
                        height: h,
                    });
                }
            }
        }
        Ok(FramingPlan { ratios, jobs })
    }

    /// Sample every job in `plan` once and pick a crop for each shape. Static
    /// and lock-free — one short ffmpeg decode per clip, shared by all the
    /// shapes, since the salience map is a property of the shot and the crop
    /// is what changes with the frame.
    ///
    /// A shot already a shape gets an *identity* framing for it (no crop)
    /// rather than none: the render looks the framing up by shape, and a miss
    /// would leave the shot wearing the project frame's crop — a 16:9 shot cut
    /// 9:16 would deliver at 16:9 as the narrow strip that crop keeps. A clip
    /// whose media cannot be read is skipped like in smart crop; if nothing
    /// could be read at all, the first error is returned.
    pub fn sample_framings(plan: &FramingPlan) -> Result<Vec<(Uuid, Framing)>> {
        Self::sample_framings_with(plan, &mut |_| {}, &|| false)
    }

    /// [`Project::sample_framings`] that reports and can be stopped (see
    /// [`Project::sample_smart_crops_with`]).
    pub fn sample_framings_with(
        plan: &FramingPlan,
        progress: &mut dyn FnMut(f32),
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<(Uuid, Framing)>> {
        let mut out = Vec::new();
        let mut first_error = None;
        let total = plan.jobs.len().max(1) as f32;
        progress(0.0);
        for (i, job) in plan.jobs.iter().enumerate() {
            let done = (i + 1) as f32 / total;
            let needs: Vec<(u32, u32)> = plan
                .ratios
                .iter()
                .copied()
                .filter(|&(w, h)| crate::model::needs_crop(job.width, job.height, w as f64 / h as f64))
                .collect();
            let map = if needs.is_empty() {
                None
            } else {
                match engine::salience_map_cancellable(&job.path, job.start, job.end, cancel) {
                    Ok(map) => Some(map),
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(e) => {
                        tracing::warn!(clip = %job.clip_id, path = %job.path.display(), error = %e, "could not sample a shot for framing");
                        first_error.get_or_insert(e);
                        progress(done);
                        continue;
                    }
                }
            };
            for &ratio in &plan.ratios {
                let crop = if needs.contains(&ratio) {
                    let aspect = ratio.0 as f64 / ratio.1 as f64;
                    match map.as_ref().and_then(|m| m.crop_for(job.width, job.height, aspect)) {
                        Some(crop) => crop,
                        None => continue,
                    }
                } else {
                    CropFrame::default()
                };
                out.push((job.clip_id, Framing::new(ratio, &crop)));
            }
            progress(done);
        }
        match (out.is_empty(), first_error) {
            (true, Some(e)) => Err(e),
            _ => Ok(out),
        }
    }

    /// Write sampled framings onto their clips as one undoable edit, labelled
    /// with the shapes it framed for. Returns how many clips changed; framings
    /// a clip already carries are dropped first, so a re-run over an unchanged
    /// cut reports 0 and leaves the history alone.
    pub fn apply_framings(&self, framings: &[(Uuid, Framing)]) -> Result<usize> {
        let timeline = self.working_timeline()?;
        let pending: Vec<_> = framings
            .iter()
            .filter(|(clip_id, f)| {
                timeline
                    .locate(*clip_id)
                    .is_some_and(|(ti, ci)| timeline.tracks[ti].clips[ci].framing_for(f.ratio()) != Some(f))
            })
            .collect();
        if pending.is_empty() {
            return Ok(0);
        }
        let mut shapes: Vec<String> = Vec::new();
        for (_, f) in &pending {
            let label = f.ratio_label();
            if !shapes.contains(&label) {
                shapes.push(label);
            }
        }
        let label = format!("Frame for {}", shapes.join(", "));
        self.edit_timeline(&label, |timeline| {
            let mut touched: Vec<Uuid> = Vec::new();
            for (clip_id, f) in &pending {
                let Some((ti, ci)) = timeline.locate(*clip_id) else {
                    continue;
                };
                if timeline.tracks[ti].clips[ci].set_framing(*f) && !touched.contains(clip_id) {
                    touched.push(*clip_id);
                }
            }
            Ok(touched.len())
        })
    }
}
