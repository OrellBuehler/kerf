//! Live behaviours at the project level: set them on several clips in one edit, or bake them into keys.
//!
//! The behaviours themselves are `model::behavior`'s (pure, closed-form); this is the edit around
//! them: one revision for every clip, all or nothing, through `edit_timeline` — so ripple, the
//! history and an agent's staging apply as to any edit, and the diff a reviewer reads is
//! "behaviours: sway added". A behaviour is part of a clip's look: linked partners are not touched
//! and a locked track does not refuse (`Timeline::ensure_editable`).

use super::*;
use crate::model::{check_behaviors, Baked, Behavior};

/// A clip a bake wrote keys into, and what it wrote.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BakedClip {
    pub clip: Clip,
    pub baked: Baked,
}

/// The frame rate a bake samples at when none is named: the delivery's, else the first picture's
/// footage, else 30.
const DEFAULT_BAKE_FPS: f64 = 30.0;

impl Project {
    /// Replace the behaviours of each of `clip_ids` (a clip named twice once) with `behaviors` — an
    /// empty list clears them — as **one** revision, all or nothing. Each is checked
    /// ([`check_behaviors`]: at most `MAX_BEHAVIORS`, every number in its range); a clip on an audio
    /// track has no picture to move and refuses the whole edit.
    pub fn set_behaviors(&self, clip_ids: &[Uuid], behaviors: Vec<Behavior>) -> Result<Vec<Clip>> {
        let ids = distinct(clip_ids)?;
        check_behaviors(&behaviors)?;
        let label = match (behaviors.is_empty(), ids.len()) {
            (true, 1) => "Clear behaviours".to_string(),
            (true, n) => format!("Clear behaviours ({n} clips)"),
            (false, 1) => "Set behaviours".to_string(),
            (false, n) => format!("Set behaviours ({n} clips)"),
        };
        self.edit_timeline(&label, move |timeline| {
            let mut done = Vec::with_capacity(ids.len());
            for id in ids {
                let clip = picture_clip(timeline, id)?;
                clip.set_behaviors(behaviors.clone())?;
                done.push(clip.clone());
            }
            Ok(done)
        })
    }

    /// Sample the behaviours of each of `clip_ids` into keys of the numbers they move and clear them
    /// ([`Clip::bake_behaviors`]) as **one** revision, all or nothing: exact at the frames of an `fps`
    /// grid (default: the delivery's rate, else the footage's, else 30), straight lines between.
    /// A clip with no behaviours, or one a bake would put more than [`MAX_CHANNEL_KEYS`] keys on a
    /// number of, refuses the whole edit and says which.
    pub fn bake_behaviors(&self, clip_ids: &[Uuid], fps: Option<f64>) -> Result<Vec<BakedClip>> {
        let ids = distinct(clip_ids)?;
        let fps = match fps {
            Some(fps) => fps,
            None => self.default_bake_fps()?,
        };
        let label = match ids.len() {
            1 => "Bake behaviours".to_string(),
            n => format!("Bake behaviours ({n} clips)"),
        };
        self.edit_timeline(&label, move |timeline| {
            let mut done = Vec::with_capacity(ids.len());
            for id in ids {
                let clip = picture_clip(timeline, id)?;
                let baked = clip.bake_behaviors(fps, MAX_CHANNEL_KEYS)?;
                for track in &clip.channels {
                    validate_property_keys(track.prop, &track.keys)?;
                }
                done.push(BakedClip {
                    clip: clip.clone(),
                    baked,
                });
            }
            Ok(done)
        })
    }

    /// The rate [`Project::bake_behaviors`] samples at when it is not told one.
    fn default_bake_fps(&self) -> Result<f64> {
        let timeline = self.working_timeline()?;
        if let Some(fps) = timeline.format.and_then(|d| d.fps) {
            return Ok(fps);
        }
        let assets = self.list_assets()?;
        Ok(timeline
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| &t.clips)
            .find_map(|c| {
                assets
                    .iter()
                    .find(|a| a.id == c.asset_id)?
                    .streams
                    .iter()
                    .find(|s| s.kind == StreamKind::Video)?
                    .fps
                    .filter(|f| f.is_finite() && *f > 0.0)
            })
            .unwrap_or(DEFAULT_BAKE_FPS))
    }
}

/// `ids` without repeats; at least one.
fn distinct(ids: &[Uuid]) -> Result<Vec<Uuid>> {
    let mut out: Vec<Uuid> = Vec::with_capacity(ids.len());
    for id in ids {
        if !out.contains(id) {
            out.push(*id);
        }
    }
    if out.is_empty() {
        return Err(Error::InvalidArgument("name at least one clip".to_string()));
    }
    Ok(out)
}

/// The clip `id`, which must have a picture to move.
fn picture_clip(timeline: &mut Timeline, id: Uuid) -> Result<&mut Clip> {
    let (ti, ci) = timeline.locate(id).ok_or(Error::ClipNotFound(id))?;
    let track = &mut timeline.tracks[ti];
    if track.kind == StreamKind::Audio {
        return Err(Error::InvalidArgument(format!(
            "clip {id} is on audio track {}: a sound has no picture to move",
            track.name
        )));
    }
    Ok(&mut track.clips[ci])
}
