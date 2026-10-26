//! Stabilizing a clip: the setting is an edit, the analysis a cache measured off the lock
//! (`crate::stabilize` has the design).
//!
//! The usual heavy-op split: [`Project::stabilize_inputs`] resolves the clip's footage and the window
//! to measure under the lock, the static [`Project::analyze_stabilization`] measures it with the lock
//! released and keeps the result beside the footage — nothing is applied afterwards, since the
//! timeline holds only the setting. [`Project::stabilization_status_inputs`] /
//! [`Project::stabilization_statuses`] say where each clip stands, the second off the lock (it reads
//! the cache).

use std::path::PathBuf;

use uuid::Uuid;

use super::Project;
use crate::clip_timing::{clip_source_window, transition_fx};
use crate::error::{Error, Result};
use crate::model::{Asset, Clip, Stabilize, StreamKind, Timeline};
use crate::stabilize::{self as steady, ClipStabilizeStatus, StabilizeStatus, MAX_ANALYSIS_SECONDS, RANGE_MARGIN};

/// What [`Project::analyze_stabilization`] measures: a window of a clip's original footage.
#[derive(Debug, Clone, PartialEq)]
pub struct StabilizeJob {
    pub clip_id: Uuid,
    pub asset_id: Uuid,
    /// The original file (export reads it; an analysis of a proxy would not match its frames).
    pub path: PathBuf,
    /// The source seconds measured: the clip's window and [`RANGE_MARGIN`] either side.
    pub range: (f64, f64),
    /// Where the clip's own source window lies (`source_in..source_out`).
    pub window: (f64, f64),
    /// How late the file's picture starts (its seek is the export's).
    pub lead: f64,
}

/// What [`Project::stabilization_statuses`] needs of one clip, read under the lock.
#[derive(Debug, Clone, PartialEq)]
pub struct StabilizeStatusInput {
    pub clip_id: Uuid,
    pub stabilize: Option<Stabilize>,
    /// Why the clip cannot be stabilized, when it cannot.
    pub unsupported: Option<String>,
    pub asset_id: Uuid,
    pub path: Option<PathBuf>,
    pub window: (f64, f64),
}

/// Why `clip` (on a track of `kind`, its footage `asset`) cannot be stabilized, or `None` when it can.
pub(crate) fn why_not_stabilized(clip: &Clip, kind: StreamKind, asset: Option<&Asset>) -> Option<String> {
    let why = if kind != StreamKind::Video {
        "a clip on an audio track has no picture to steady"
    } else if clip.is_generated() {
        "a title draws its own picture and has no camera to steady"
    } else if let Some(asset) = asset {
        if asset.is_image() {
            "a still image does not move"
        } else if !asset.streams.iter().any(|s| s.kind == StreamKind::Video) {
            "the clip has no picture"
        } else if asset.projection().is_some() || clip.reframe.is_some() {
            "360 footage is not stabilized: its reframed camera is the framing"
        } else {
            return None;
        }
    } else {
        "the clip's media is not in the library"
    };
    Some(why.to_string())
}

impl Project {
    /// Stabilize a clip (`Some`: the setting, checked) or stop (`None`), as one `Stabilize clip` /
    /// `Stop stabilizing clip` revision. A still, a title, a clip of sound and 360 footage are refused
    /// with why. Like the rest of a clip's look it may change on a locked track. The analysis is the
    /// caller's to start ([`Project::stabilize_inputs`]); until it lands the clip plays as shot.
    pub fn set_clip_stabilize(&self, clip_id: Uuid, stabilize: Option<Stabilize>) -> Result<Clip> {
        if let Some(s) = &stabilize {
            s.check()?;
            let timeline = self.working_timeline()?;
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &timeline.tracks[ti].clips[ci];
            let asset = if clip.is_generated() {
                None
            } else {
                self.get_asset(clip.asset_id)?
            };
            if let Some(why) = why_not_stabilized(clip, timeline.tracks[ti].kind, asset.as_ref()) {
                return Err(Error::InvalidArgument(format!("cannot stabilize this clip: {why}")));
            }
        }
        let label = if stabilize.is_some() {
            "Stabilize clip"
        } else {
            "Stop stabilizing clip"
        };
        self.edit_timeline(label, |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            clip.stabilize = stabilize;
            Ok(clip.clone())
        })
    }

    /// What analysing `clip_id`'s stabilization measures (the working timeline's clip, its original
    /// footage), resolved under the lock for [`Project::analyze_stabilization`] to run without it.
    /// Refused for a clip that is not stabilized or cannot be.
    pub fn stabilize_inputs(&self, clip_id: Uuid) -> Result<StabilizeJob> {
        let timeline = self.working_timeline()?;
        let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let clip = &timeline.tracks[ti].clips[ci];
        if clip.stabilize.is_none() {
            return Err(Error::InvalidArgument(
                "the clip is not stabilized: turn stabilization on first".into(),
            ));
        }
        let asset = (!clip.is_generated())
            .then(|| self.get_asset(clip.asset_id))
            .transpose()?
            .flatten();
        if let Some(why) = why_not_stabilized(clip, timeline.tracks[ti].kind, asset.as_ref()) {
            return Err(Error::InvalidArgument(format!("cannot stabilize this clip: {why}")));
        }
        let asset = asset.ok_or(Error::AssetNotFound(clip.asset_id))?;
        let (start, end) = measured_window(&timeline, &self.list_assets()?, ti, ci);
        let range = ((start - RANGE_MARGIN).max(0.0), (end + RANGE_MARGIN).min(asset.duration));
        if range.1 - range.0 > MAX_ANALYSIS_SECONDS {
            return Err(Error::InvalidArgument(format!(
                "the clip is too long to stabilize in one analysis (at most {} minutes of footage)",
                MAX_ANALYSIS_SECONDS / 60.0
            )));
        }
        let lead = asset
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Video)
            .filter(|s| !s.image)
            .map_or(0.0, |s| s.lead);
        Ok(StabilizeJob {
            clip_id,
            asset_id: asset.id,
            path: PathBuf::from(&asset.path),
            range,
            window: (clip.source_in, clip.source_out),
            lead,
        })
    }

    /// Measure the camera of `job`'s clip and keep it beside the footage, with no `&self` (the lock
    /// released). `Ok(false)` when there was nothing to measure: this FFmpeg has no vid.stab (the
    /// clip is steadied by `deshake`), or an analysis already covers the clip. `progress` gets the
    /// fraction measured; `cancel` stops it ([`Error::Cancelled`]). Runs in the background lane of the
    /// heavy-job queue; [`Project::stabilization_statuses`] shows it queued, running and failed.
    pub fn analyze_stabilization(job: &StabilizeJob, progress: &mut dyn FnMut(f64), cancel: &dyn Fn() -> bool) -> Result<bool> {
        if !crate::engine::vidstab_available()
            || steady::analysis_for(job.asset_id, Some(&job.path), job.window.0, job.window.1).is_some()
        {
            steady::job_ended(job.clip_id, None);
            return Ok(false);
        }
        steady::job_queued(job.clip_id);
        let measured = crate::engine::measure_motion(
            &job.path,
            job.range.0,
            job.range.1,
            job.lead,
            &mut |f| {
                steady::job_progress(job.clip_id, f);
                progress(f);
            },
            cancel,
        )
        .and_then(|analysis| steady::store(job.asset_id, job.range, &analysis));
        match measured {
            Ok(_) => {
                steady::job_ended(job.clip_id, None);
                Ok(true)
            }
            Err(Error::Cancelled) => {
                steady::job_ended(job.clip_id, None);
                Err(Error::Cancelled)
            }
            Err(e) => {
                steady::job_ended(job.clip_id, Some(e.to_string()));
                Err(e)
            }
        }
    }

    /// What [`Project::stabilization_statuses`] reads of the working timeline: the named clips, or
    /// every stabilized one. An unknown id is [`Error::ClipNotFound`].
    pub fn stabilization_status_inputs(&self, clip_ids: Option<&[Uuid]>) -> Result<Vec<StabilizeStatusInput>> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        let input = |ti: usize, ci: usize| {
            let clip = &timeline.tracks[ti].clips[ci];
            let asset = assets.iter().find(|a| a.id == clip.asset_id);
            StabilizeStatusInput {
                clip_id: clip.id,
                stabilize: clip.stabilize,
                unsupported: why_not_stabilized(clip, timeline.tracks[ti].kind, asset),
                asset_id: clip.asset_id,
                path: asset.map(|a| PathBuf::from(&a.path)),
                window: (clip.source_in, clip.source_out),
            }
        };
        match clip_ids {
            Some(ids) => ids
                .iter()
                .map(|&id| {
                    let (ti, ci) = timeline.locate(id).ok_or(Error::ClipNotFound(id))?;
                    Ok(input(ti, ci))
                })
                .collect(),
            None => Ok(timeline
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(ti, t)| t.clips.iter().enumerate().map(move |(ci, c)| (ti, ci, c)))
                .filter(|(_, _, c)| c.stabilize.is_some())
                .map(|(ti, ci, _)| input(ti, ci))
                .collect()),
        }
    }

    /// Where each clip's stabilization stands ([`StabilizeStatus`]), with no `&self`: it reads the
    /// analyses kept on disk and this session's jobs.
    pub fn stabilization_statuses(inputs: &[StabilizeStatusInput]) -> Vec<ClipStabilizeStatus> {
        let vidstab = crate::engine::vidstab_available();
        inputs
            .iter()
            .map(|input| ClipStabilizeStatus {
                clip_id: input.clip_id,
                status: status_of(input, vidstab),
            })
            .collect()
    }
}

/// One clip's status (see [`StabilizeStatus`] for the order the states are judged in).
fn status_of(input: &StabilizeStatusInput, vidstab: bool) -> StabilizeStatus {
    if input.stabilize.is_none() {
        return StabilizeStatus::Off;
    }
    if let Some(reason) = &input.unsupported {
        return StabilizeStatus::Unsupported { reason: reason.clone() };
    }
    if !vidstab {
        return StabilizeStatus::Basic;
    }
    let job = steady::job_status(input.clip_id);
    if let Some(job @ (StabilizeStatus::Queued | StabilizeStatus::Analyzing { .. })) = job {
        return job;
    }
    let analysed = input
        .path
        .as_deref()
        .and_then(|p| steady::analysis_for(input.asset_id, Some(p), input.window.0, input.window.1))
        .is_some();
    match job {
        _ if analysed => StabilizeStatus::Ready,
        Some(failed) => failed,
        None => StabilizeStatus::Missing,
    }
}

/// The source window the picture of `timeline`'s clip `(ti, ci)` reads, its transition's tail
/// included.
fn measured_window(timeline: &Timeline, assets: &[Asset], ti: usize, ci: usize) -> (f64, f64) {
    let flat = timeline.tracks[..ti].iter().map(|t| t.clips.len()).sum::<usize>() + ci;
    let fx = transition_fx(timeline, assets);
    clip_source_window(&timeline.tracks[ti].clips[ci], &fx[flat])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Project, Uuid, Uuid) {
        let project = Project::sample().unwrap();
        let tl = project.timeline().unwrap();
        let video = tl.tracks.iter().find(|t| t.kind == StreamKind::Video).unwrap();
        let clip = video.clips[0].id;
        let audio = tl
            .tracks
            .iter()
            .find(|t| t.kind == StreamKind::Audio && !t.clips.is_empty())
            .map(|t| t.clips[0].id)
            .unwrap_or(Uuid::nil());
        (project, clip, audio)
    }

    #[test]
    fn stabilizing_is_one_revision_and_stopping_another() {
        let (project, clip, _) = sample();
        let before = project.history().unwrap().len();
        let set = project.set_clip_stabilize(clip, Some(Stabilize::default())).unwrap();
        assert_eq!(set.stabilize, Some(Stabilize::default()));
        let history = project.history().unwrap();
        assert_eq!(history.len(), before + 1);
        assert_eq!(history.last().unwrap().label, "Stabilize clip");
        let off = project.set_clip_stabilize(clip, None).unwrap();
        assert_eq!(off.stabilize, None);
        assert_eq!(project.history().unwrap().last().unwrap().label, "Stop stabilizing clip");
    }

    #[test]
    fn a_bad_setting_or_a_clip_that_cannot_be_steadied_is_refused_and_writes_nothing() {
        let (project, clip, audio) = sample();
        let before = project.history().unwrap().len();
        let bad = Stabilize {
            strength: 2.0,
            zoom: None,
        };
        assert!(project.set_clip_stabilize(clip, Some(bad)).is_err());
        if !audio.is_nil() {
            let err = project
                .set_clip_stabilize(audio, Some(Stabilize::default()))
                .unwrap_err()
                .to_string();
            assert!(err.contains("audio track"), "{err}");
        }
        assert!(matches!(
            project.set_clip_stabilize(Uuid::new_v4(), Some(Stabilize::default())),
            Err(Error::ClipNotFound(_))
        ));
        assert_eq!(project.history().unwrap().len(), before);
        assert!(project
            .stabilize_inputs(clip)
            .unwrap_err()
            .to_string()
            .contains("not stabilized"));
    }

    #[test]
    fn the_analysis_measures_the_clips_window_of_the_original_with_a_margin() {
        let (project, clip, _) = sample();
        project.set_clip_stabilize(clip, Some(Stabilize::default())).unwrap();
        let job = project.stabilize_inputs(clip).unwrap();
        let tl = project.timeline().unwrap();
        let c = tl.clip(clip).unwrap();
        let asset = project.require_asset(c.asset_id).unwrap();
        assert_eq!(job.path, PathBuf::from(&asset.path));
        assert_eq!(job.window, (c.source_in, c.source_out));
        assert!(job.range.0 <= c.source_in && job.range.0 >= (c.source_in - RANGE_MARGIN).max(0.0));
        assert!(job.range.1 >= c.source_out.min(asset.duration) && job.range.1 <= asset.duration);
    }

    #[test]
    fn the_status_says_off_unsupported_basic_missing_failed_or_ready() {
        let input = StabilizeStatusInput {
            clip_id: Uuid::new_v4(),
            stabilize: None,
            unsupported: None,
            asset_id: Uuid::new_v4(),
            path: Some(PathBuf::from("/nowhere/clip.mp4")),
            window: (0.0, 1.0),
        };
        assert_eq!(status_of(&input, true), StabilizeStatus::Off);
        let on = StabilizeStatusInput {
            stabilize: Some(Stabilize::default()),
            ..input
        };
        assert_eq!(status_of(&on, false), StabilizeStatus::Basic);
        assert_eq!(status_of(&on, true), StabilizeStatus::Missing);
        let still = StabilizeStatusInput {
            unsupported: Some("a still image does not move".into()),
            ..on.clone()
        };
        assert!(matches!(status_of(&still, true), StabilizeStatus::Unsupported { .. }));
        steady::job_queued(on.clip_id);
        assert_eq!(status_of(&on, true), StabilizeStatus::Queued);
        steady::job_ended(on.clip_id, Some("boom".into()));
        assert_eq!(status_of(&on, true), StabilizeStatus::Failed { reason: "boom".into() });
        steady::job_ended(on.clip_id, None);
        assert_eq!(status_of(&on, true), StabilizeStatus::Missing);
    }
}
