//! Syncing a clip to another by their sound: the project half of
//! [`crate::engine::audio_sync`].
//!
//! The usual heavy-op split: [`Project::audio_sync_inputs`] resolves both clips under the
//! lock, [`Project::measure_audio_sync`] decodes and correlates with no `&self`, and
//! [`Project::apply_audio_sync`] moves the clip through [`Project::move_clip`] (linked
//! partners move with it; a move never ripples).

use std::path::PathBuf;

use serde::Serialize;
use uuid::Uuid;

use super::Project;
use crate::engine::audio_sync::{find_offset, SyncOptions};
use crate::engine::decode_audio_window;
use crate::error::{Error, Result};
use crate::model::{Clip, Timeline};

/// The rate both sounds are compared at: speech and transients are well inside 8 kHz, and
/// the refinement reads the offset to a fraction of a 16 kHz sample (tens of µs).
pub const SYNC_SAMPLE_RATE: u32 = 16_000;
/// The most of each clip compared, seconds: a longer clip is compared on its first 20 min.
pub const MAX_SYNC_SECONDS: f64 = 1200.0;

/// One side of a sync: the source window of the clip that is decoded.
#[derive(Debug, Clone)]
pub struct SyncSide {
    pub clip_id: Uuid,
    pub path: PathBuf,
    /// Source seconds where the decoded window starts (the clip's `source_in`).
    pub start: f64,
    pub secs: f64,
}

/// What [`Project::measure_audio_sync`] decodes: the reference clip and the clip moved to it.
#[derive(Debug, Clone)]
pub struct AudioSyncPlan {
    pub reference: SyncSide,
    pub clip: SyncSide,
}

/// The offset two clips' sounds were found at, before it is applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSyncMeasure {
    pub reference_id: Uuid,
    pub clip_id: Uuid,
    /// Source seconds the two windows started at.
    pub reference_start: f64,
    pub clip_start: f64,
    /// The clip's window lines up with the reference's this many seconds into it (negative:
    /// the clip's sound starts before the reference's window).
    pub lag: f64,
    pub confidence: f64,
    pub distinct: f64,
    pub reliable: bool,
    pub inverted: bool,
}

/// What syncing a clip by its sound did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AudioSync {
    pub reference_id: Uuid,
    pub clip_id: Uuid,
    /// Seconds the clip moves (moved, when `applied`) on the timeline; positive is later.
    pub offset: f64,
    /// Where the clip starts in sync with the reference.
    pub timeline_start: f64,
    /// The correlation peak over its RMS: 8 or more is a clear match.
    pub confidence: f64,
    /// The peak over the next-best match more than 50 ms away: 1.5 or more is unambiguous
    /// (music and hum can match at several offsets).
    pub distinct: f64,
    /// Clear and unambiguous enough to trust.
    pub reliable: bool,
    /// One recording is the other with its polarity inverted.
    pub inverted: bool,
    /// The clip was moved. An unreliable match is only reported unless forced.
    pub applied: bool,
}

impl Project {
    /// Resolve what syncing `clip_id` to `reference_id` by their sound decodes, without
    /// decoding: the lock-held half. Both clips play their source forward at normal speed
    /// and have sound; they are different clips, and not linked to each other (a linked
    /// pair moves together, so it could never be pulled into line).
    pub fn audio_sync_inputs(&self, reference_id: Uuid, clip_id: Uuid) -> Result<AudioSyncPlan> {
        if reference_id == clip_id {
            return Err(Error::InvalidArgument("a clip cannot be synced to itself".to_string()));
        }
        let timeline = self.working_timeline()?;
        if timeline.link_partners(clip_id).contains(&reference_id) {
            return Err(Error::InvalidArgument(
                "the two clips are linked and move together; detach them first".to_string(),
            ));
        }
        Ok(AudioSyncPlan {
            reference: self.sync_side(&timeline, reference_id)?,
            clip: self.sync_side(&timeline, clip_id)?,
        })
    }

    fn sync_side(&self, timeline: &Timeline, clip_id: Uuid) -> Result<SyncSide> {
        let clip = timeline.clip(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        check_plain_speed(clip)?;
        let asset = self.get_asset(clip.asset_id)?.ok_or(Error::AssetNotFound(clip.asset_id))?;
        if !asset.has_audio() {
            return Err(Error::InvalidArgument(format!("clip {clip_id} has no sound to sync by")));
        }
        Ok(SyncSide {
            clip_id,
            path: PathBuf::from(&asset.path),
            start: clip.source_in,
            secs: (clip.source_out - clip.source_in).min(MAX_SYNC_SECONDS),
        })
    }

    /// Decode both clips' sound (mono, [`SYNC_SAMPLE_RATE`]) and find the offset between them.
    /// Static and lock-free: the slow half. An answer that is not [`AudioSync::reliable`] is
    /// still an answer; whether to act on it is [`Project::apply_audio_sync`]'s question.
    pub fn measure_audio_sync(plan: &AudioSyncPlan) -> Result<AudioSyncMeasure> {
        let decode = |side: &SyncSide| -> Result<Vec<f32>> {
            Ok(decode_audio_window(&side.path, side.start, side.secs, SYNC_SAMPLE_RATE, 1)?.samples)
        };
        let (a, b) = (decode(&plan.reference)?, decode(&plan.clip)?);
        let found = find_offset(&a, &b, SYNC_SAMPLE_RATE, &SyncOptions::default())
            .ok_or_else(|| Error::InvalidArgument("not enough sound in one of the clips to sync by".to_string()))?;
        Ok(AudioSyncMeasure {
            reference_id: plan.reference.clip_id,
            clip_id: plan.clip.clip_id,
            reference_start: plan.reference.start,
            clip_start: plan.clip.start,
            lag: found.lag_fine / f64::from(SYNC_SAMPLE_RATE),
            confidence: f64::from(found.confidence),
            distinct: f64::from(found.distinct),
            reliable: found.reliable(),
            inverted: found.inverted,
        })
    }

    /// Move the measured clip so its sound lines up with the reference's, read against where
    /// both clips are *now* (either may have moved while the sound was decoded). One `Move
    /// clip` revision; linked partners move with it. An unreliable match is reported and not
    /// applied unless `force`; a sync that would start the clip before 0 is refused.
    pub fn apply_audio_sync(&self, m: &AudioSyncMeasure, force: bool) -> Result<AudioSync> {
        let timeline = self.working_timeline()?;
        let reference = timeline.clip(m.reference_id).ok_or(Error::ClipNotFound(m.reference_id))?;
        let clip = timeline.clip(m.clip_id).ok_or(Error::ClipNotFound(m.clip_id))?;
        check_plain_speed(reference)?;
        check_plain_speed(clip)?;
        let start = synced_start(reference, clip, m);
        let mut out = AudioSync {
            reference_id: m.reference_id,
            clip_id: m.clip_id,
            offset: start - clip.timeline_start,
            timeline_start: start,
            confidence: m.confidence,
            distinct: m.distinct,
            reliable: m.reliable,
            inverted: m.inverted,
            applied: false,
        };
        if !(m.reliable || force) {
            return Ok(out);
        }
        if start < -1e-9 {
            return Err(Error::InvalidArgument(format!(
                "in sync the clip would start {:.3}s before the timeline does; move the reference later first",
                -start
            )));
        }
        self.move_clip(m.clip_id, start.max(0.0), None)?;
        out.applied = true;
        Ok(out)
    }

    /// [`Project::audio_sync_inputs`] → [`Project::measure_audio_sync`] →
    /// [`Project::apply_audio_sync`], holding `&self` throughout.
    pub fn sync_clips_by_audio(&self, reference_id: Uuid, clip_id: Uuid, force: bool) -> Result<AudioSync> {
        let plan = self.audio_sync_inputs(reference_id, clip_id)?;
        let measured = Self::measure_audio_sync(&plan)?;
        self.apply_audio_sync(&measured, force)
    }
}

fn check_plain_speed(clip: &Clip) -> Result<()> {
    if clip.is_reversed() || (clip.speed_mag() - 1.0).abs() > 1e-9 {
        return Err(Error::InvalidArgument(format!(
            "clip {} plays at another speed or reversed; sync needs both at normal speed",
            clip.id
        )));
    }
    Ok(())
}

/// Where `clip` starts so that the moment `m` matched lands at one timeline time in both
/// clips (pure). The clip's window sample `n` is the reference's sample `n + lag`: source
/// time `y = clip_start + t` against `x = reference_start + t + lag`, each shown at
/// `timeline_start + (source − source_in)`.
fn synced_start(reference: &Clip, clip: &Clip, m: &AudioSyncMeasure) -> f64 {
    reference.timeline_start - reference.source_in + m.reference_start + m.lag - m.clip_start + clip.source_in
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(source_in: f64, source_out: f64, timeline_start: f64) -> Clip {
        Clip::new(Uuid::new_v4(), source_in, source_out, timeline_start)
    }

    fn measure(reference: &Clip, clip: &Clip, lag: f64) -> AudioSyncMeasure {
        AudioSyncMeasure {
            reference_id: reference.id,
            clip_id: clip.id,
            reference_start: reference.source_in,
            clip_start: clip.source_in,
            lag,
            confidence: 20.0,
            distinct: 3.0,
            reliable: true,
            inverted: false,
        }
    }

    #[test]
    fn the_synced_start_puts_the_matched_moment_at_one_time() {
        // The clip's sound starts 2.5 s into the reference's window.
        let r = clip(10.0, 40.0, 5.0);
        let c = clip(3.0, 20.0, 50.0);
        let m = measure(&r, &c, 2.5);
        assert!((synced_start(&r, &c, &m) - 7.5).abs() < 1e-12);
        // Measured, then the reference moved and was trimmed: the clip follows it.
        let mut moved = r.clone();
        moved.timeline_start = 8.0;
        moved.source_in = 11.0;
        assert!((synced_start(&moved, &c, &m) - (8.0 - 1.0 + 2.5)).abs() < 1e-12);
        // A clip trimmed at its head after the measure starts later by the trim.
        let mut trimmed = c.clone();
        trimmed.source_in = 4.0;
        assert!((synced_start(&r, &trimmed, &m) - 8.5).abs() < 1e-12);
    }

    #[test]
    fn a_reversed_or_sped_up_clip_is_refused() {
        let mut c = clip(0.0, 4.0, 0.0);
        assert!(check_plain_speed(&c).is_ok());
        c.speed = 2.0;
        assert!(check_plain_speed(&c).is_err());
        c.speed = -1.0;
        assert!(check_plain_speed(&c).is_err());
    }
}
