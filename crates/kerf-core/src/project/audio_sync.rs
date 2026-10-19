//! Syncing a clip to another by their sound: the project half of
//! [`crate::engine::audio_sync`].
//!
//! The usual heavy-op split: [`Project::audio_sync_inputs`] resolves both clips under the
//! lock (and refuses what the move could not do, before anything is decoded),
//! [`Project::measure_audio_sync`] decodes and correlates with no `&self`, and
//! [`Project::apply_audio_sync`] moves the clip through [`Project::move_clip`] (linked
//! partners move with it; a move never ripples). Only each clip's used source window
//! (`source_in..source_out`, at most [`MAX_SYNC_SECONDS`]) is compared.

use std::path::PathBuf;

use serde::Serialize;
use uuid::Uuid;

use super::Project;
use crate::engine::audio_sync::{find_offset, SyncOptions};
use crate::engine::cpu::{self, Wait};
use crate::engine::decode_audio_window;
use crate::error::{Error, Result};
use crate::model::{Clip, Timeline};

/// The rate both sounds are compared at: speech and transients are well inside 8 kHz, and
/// the refinement reads the offset to a fraction of a 16 kHz sample (tens of µs).
pub const SYNC_SAMPLE_RATE: u32 = 16_000;
/// The most of each clip compared, seconds: a longer clip is compared on its first 20 min.
pub const MAX_SYNC_SECONDS: f64 = 1200.0;

/// One side of a sync: the source window of the clip that is decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncSide {
    pub clip_id: Uuid,
    pub path: PathBuf,
    /// Source seconds where the decoded window starts (the clip's `source_in`).
    pub start: f64,
    pub secs: f64,
}

/// What [`Project::measure_audio_sync`] decodes: the reference clip and the clip moved to it.
/// Equal plans decode the same sound, so a measurement of one stands for the other.
#[derive(Debug, Clone, PartialEq)]
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
    /// Infinite when nothing else correlated at all.
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
    /// (music and hum can match at several offsets). `null` when no other offset correlated
    /// at all — as unambiguous as it gets, and not a number JSON can carry.
    pub distinct: Option<f64>,
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
    /// pair moves together, so it could never be pulled into line). The move itself is
    /// checked here too, so a sync that could not be applied fails before the decode: the
    /// clip (and every partner moving with it) must be on unlocked tracks, and not on the
    /// reference's track, where in sync the two would overlap.
    pub fn audio_sync_inputs(&self, reference_id: Uuid, clip_id: Uuid) -> Result<AudioSyncPlan> {
        if reference_id == clip_id {
            return Err(Error::InvalidArgument("a clip cannot be synced to itself".to_string()));
        }
        let timeline = self.working_timeline()?;
        let partners = timeline.link_partners(clip_id);
        if partners.contains(&reference_id) {
            return Err(Error::InvalidArgument(
                "the two clips are linked and move together; detach them first".to_string(),
            ));
        }
        let (rt, _) = timeline.locate(reference_id).ok_or(Error::ClipNotFound(reference_id))?;
        let (ct, _) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        if rt == ct {
            return Err(Error::InvalidArgument(format!(
                "both clips are on track {}; in sync they would overlap, so move one to another track first",
                timeline.tracks[ct].name
            )));
        }
        for id in std::iter::once(clip_id).chain(partners) {
            if let Some((ti, _)) = timeline.locate(id) {
                if timeline.tracks[ti].locked {
                    return Err(Error::InvalidArgument(format!(
                        "track {} is locked",
                        timeline.tracks[ti].name
                    )));
                }
            }
        }
        Ok(AudioSyncPlan {
            reference: self.sync_side(&timeline, reference_id)?,
            clip: self.sync_side(&timeline, clip_id)?,
        })
    }

    fn sync_side(&self, timeline: &Timeline, clip_id: Uuid) -> Result<SyncSide> {
        let clip = timeline.clip(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        if clip.is_generated() {
            return Err(Error::InvalidArgument(format!(
                "clip {clip_id} is a title, which has no sound to sync by"
            )));
        }
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
    /// Static and lock-free: the slow half, on the heavy-job slot — `on_wait` hears why it
    /// queues for it, and `cancel` stops it while it waits and between the decodes
    /// ([`Error::Cancelled`]). An answer that is not [`AudioSync::reliable`] is still an
    /// answer; whether to act on it is [`Project::apply_audio_sync`]'s question.
    pub fn measure_audio_sync(
        plan: &AudioSyncPlan,
        on_wait: &mut dyn FnMut(Wait),
        cancel: &dyn Fn() -> bool,
    ) -> Result<AudioSyncMeasure> {
        if cancel() {
            return Err(Error::Cancelled);
        }
        let lease = cpu::lease_waiting(on_wait, cancel)?;
        let decode = |side: &SyncSide| -> Result<Vec<f32>> {
            if cancel() {
                return Err(Error::Cancelled);
            }
            Ok(decode_audio_window(&side.path, side.start, side.secs, SYNC_SAMPLE_RATE, 1, &lease, cancel)?.samples)
        };
        let (a, b) = (decode(&plan.reference)?, decode(&plan.clip)?);
        if cancel() {
            return Err(Error::Cancelled);
        }
        let found = find_offset(&a, &b, SYNC_SAMPLE_RATE, &SyncOptions::default())
            .ok_or_else(|| Error::InvalidArgument("not enough sound in one of the clips to sync by".to_string()))?;
        drop(lease);
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
            distinct: m.distinct.is_finite().then_some(m.distinct),
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
        let mut trimmed = c;
        trimmed.source_in = 4.0;
        assert!((synced_start(&r, &trimmed, &m) - 8.5).abs() < 1e-12);
    }

    /// Two files of one pink-noise take, the second starting 1.25 s into the first: decoded
    /// through ffmpeg and correlated, the offset comes back to the sample.
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn two_recordings_of_one_take_line_up_through_ffmpeg() {
        use crate::engine::test_support::StatusBounded;
        let dir = std::env::temp_dir().join(format!("kerf-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.flac"), dir.join("b.flac"));
        let run = |args: &[&str], out: &std::path::Path| {
            let ok = crate::engine::ffmpeg_command()
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(args)
                .arg(out)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(ok.success());
        };
        run(&["-f", "lavfi", "-i", "anoisesrc=d=12:c=pink:seed=7:r=48000"], &a);
        let a_path = a.to_string_lossy().into_owned();
        run(&["-ss", "1.25", "-i", &a_path, "-af", "volume=0.3"], &b);
        let side = |path: &std::path::Path, start: f64, secs: f64| SyncSide {
            clip_id: Uuid::new_v4(),
            path: path.to_path_buf(),
            start,
            secs,
        };
        let plan = AudioSyncPlan {
            reference: side(&a, 0.0, 12.0),
            clip: side(&b, 0.0, 6.0),
        };
        assert!(matches!(
            Project::measure_audio_sync(&plan, &mut |_| {}, &|| true),
            Err(Error::Cancelled)
        ));
        for (clip_start, want) in [(0.0, 1.25), (2.0, 3.25)] {
            let plan = AudioSyncPlan {
                reference: side(&a, 0.0, 12.0),
                clip: side(&b, clip_start, 6.0),
            };
            let m = Project::measure_audio_sync(&plan, &mut |_| {}, &|| false).expect("measure");
            assert!((m.lag - want).abs() < 1.0 / f64::from(SYNC_SAMPLE_RATE), "{m:?}");
            assert!(m.reliable && !m.inverted, "{m:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sync_the_move_would_refuse_fails_before_anything_is_decoded() {
        use crate::model::StreamKind;
        let project = Project::sample().unwrap();
        let timeline = project.timeline().unwrap();
        let on = |kind| {
            let t = timeline
                .tracks
                .iter()
                .find(|t| t.kind == kind && !t.clips.is_empty())
                .unwrap();
            (t.id, t.clips[0].clone())
        };
        let ((_, video), (sound, audio)) = (on(StreamKind::Video), on(StreamKind::Audio));
        let plan = project.audio_sync_inputs(video.id, audio.id).unwrap();
        assert_eq!(
            (plan.clip.start, plan.clip.secs),
            (audio.source_in, audio.source_out - audio.source_in)
        );
        assert!(project.audio_sync_inputs(audio.id, audio.id).is_err());

        // On one track the two would overlap once in sync.
        let again = project.cut_clip(audio.asset_id, 20.0, 25.0).unwrap();
        let now = project.timeline().unwrap();
        let (ti, _) = now.locate(again.id).unwrap();
        let neighbour = now.tracks[ti].clips.iter().find(|c| c.id != again.id).unwrap().id;
        let err = project.audio_sync_inputs(neighbour, again.id).unwrap_err().to_string();
        assert!(err.contains("would overlap"), "{err}");

        // A locked track cannot take the move; the reference's can, as it stays put.
        project.set_track_locked(sound, true).unwrap();
        let err = project.audio_sync_inputs(video.id, audio.id).unwrap_err().to_string();
        assert!(err.contains("is locked"), "{err}");
        assert!(project.audio_sync_inputs(audio.id, video.id).is_ok());
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
