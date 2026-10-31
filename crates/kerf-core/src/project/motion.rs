//! Motion presets at the project level: one edit that writes a preset's keys into several clips.
//!
//! The keys themselves are `model::motion`'s (pure, table-driven); this is the edit around them: one
//! revision for every clip, all or nothing, through `edit_timeline` — so ripple, the history and an
//! agent's staging apply as to any edit, and the diff a reviewer reads is the keyframe phrases the
//! channels already have ("scale keyframes 0 → 3").

use super::*;
use crate::model::{MotionMode, MotionPreset};

/// A clip a preset was written into, and the window it covers (seconds from the clip's start).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MotionApplied {
    pub clip: Clip,
    pub start: f64,
    pub end: f64,
}

impl Project {
    /// Write `preset` into the animation of each of `clip_ids` (a clip named twice once) as **one**
    /// revision, all or nothing. `at` is where the window starts in seconds from each clip's start
    /// (default: 0 for an entrance or an emphasis, `duration` before the clip's end for an exit),
    /// `duration` its length (default: the preset's, or the rest of the clip), **clamped to each clip**;
    /// `intensity` (0.25..=2, default 1) scales how far the keys depart from the pose. See
    /// `model::motion` for how `mode` merges the keys with the clip's own.
    ///
    /// A clip on an audio track has no picture to move and refuses the whole edit, as does an unknown
    /// id or a value that is out of range or would leave a track with more than [`MAX_CHANNEL_KEYS`]
    /// keys. **A locked track does not refuse**: a lock guards what is on a track, where, how long and
    /// whether it plays, and a clip's look — keyframes included — stays editable
    /// (`Timeline::ensure_editable`). Linked partners are not touched (a property edit is not carried).
    pub fn apply_motion_preset(
        &self,
        clip_ids: &[Uuid],
        preset: MotionPreset,
        mode: MotionMode,
        at: Option<f64>,
        duration: Option<f64>,
        intensity: Option<f64>,
    ) -> Result<Vec<MotionApplied>> {
        let mut ids: Vec<Uuid> = Vec::with_capacity(clip_ids.len());
        for id in clip_ids {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        if ids.is_empty() {
            return Err(Error::InvalidArgument("name at least one clip".to_string()));
        }
        let label = match ids.len() {
            1 => format!("Apply motion preset {}", preset.label()),
            n => format!("Apply motion preset {} ({n} clips)", preset.label()),
        };
        self.edit_timeline(&label, move |timeline| {
            let mut applied = Vec::with_capacity(ids.len());
            for id in ids {
                let (ti, ci) = timeline.locate(id).ok_or(Error::ClipNotFound(id))?;
                let track = &mut timeline.tracks[ti];
                if track.kind == StreamKind::Audio {
                    return Err(Error::InvalidArgument(format!(
                        "clip {id} is on audio track {}: a sound has no picture to move",
                        track.name
                    )));
                }
                let clip = &mut track.clips[ci];
                let (start, end) = clip.apply_motion(preset, mode, at, duration, intensity)?;
                for track in clip.channels.iter().filter(|t| preset.properties().contains(&t.prop)) {
                    validate_property_keys(track.prop, &track.keys)?;
                }
                applied.push(MotionApplied {
                    clip: clip.clone(),
                    start,
                    end,
                });
            }
            Ok(applied)
        })
    }
}
