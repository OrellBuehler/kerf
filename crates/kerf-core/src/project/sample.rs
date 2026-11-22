//! The sample project a new in-memory project is seeded with.

use super::*;

impl Project {
    // ---- sample seed ------------------------------------------------------

    pub(super) fn seed_sample(&self) -> Result<()> {
        self.set_meta("name", "Sample Project")?;

        let interview = Asset {
            id: Uuid::new_v4(),
            path: "/samples/interview.mp4".to_string(),
            name: "interview.mp4".to_string(),
            duration: 120.0,
            streams: vec![
                StreamInfo {
                    index: 0,
                    kind: StreamKind::Video,
                    codec: "h264".to_string(),
                    width: Some(1920),
                    height: Some(1080),
                    fps: Some(30.0),
                    sample_rate: None,
                    channels: None,
                    image: false,
                    projection: None,
                    rotation: 0,
                    color_transfer: None,
                    color_primaries: None,
                    pix_fmt: None,
                    color_space: None,
                    color_range: None,
                    lead: 0.0,
                    time_base: None,
                },
                StreamInfo {
                    index: 1,
                    kind: StreamKind::Audio,
                    codec: "aac".to_string(),
                    width: None,
                    height: None,
                    fps: None,
                    sample_rate: Some(48_000),
                    channels: Some(2),
                    image: false,
                    projection: None,
                    rotation: 0,
                    color_transfer: None,
                    color_primaries: None,
                    pix_fmt: None,
                    color_space: None,
                    color_range: None,
                    lead: 0.0,
                    time_base: None,
                },
            ],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
            music_score: None,
            sound_effect: None,
            tags: Vec::new(),
        };

        let broll = Asset {
            id: Uuid::new_v4(),
            path: "/samples/broll.mp4".to_string(),
            name: "broll.mp4".to_string(),
            duration: 45.0,
            streams: vec![StreamInfo {
                index: 0,
                kind: StreamKind::Video,
                codec: "h264".to_string(),
                width: Some(3840),
                height: Some(2160),
                fps: Some(24.0),
                sample_rate: None,
                channels: None,
                image: false,
                projection: None,
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: None,
                color_space: None,
                color_range: None,
                lead: 0.0,
                time_base: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
            music_score: None,
            sound_effect: None,
            tags: Vec::new(),
        };

        self.insert_asset(&interview)?;
        self.insert_asset(&broll)?;

        self.set_analysis(&AssetAnalysis {
            asset_id: interview.id,
            silence_segments: vec![TimeRange { start: 12.5, end: 14.0 }, TimeRange { start: 60.0, end: 63.2 }],
            scene_changes: vec![0.0, 30.0, 75.0, 110.0],
            transcript: vec![
                crate::model::TranscriptSegment {
                    start: 0.0,
                    end: 5.5,
                    text: "Welcome back to the channel.".to_string(),
                    ..Default::default()
                },
                crate::model::TranscriptSegment {
                    start: 5.5,
                    end: 12.5,
                    text: "Today we are talking about non-destructive editing.".to_string(),
                    ..Default::default()
                },
            ],
            loudness: Some(crate::model::Loudness {
                integrated_lufs: -16.2,
                loudness_range: 6.4,
                true_peak_dbtp: -1.5,
                threshold_lufs: -26.5,
            }),
            onsets: vec![0.5, 1.2, 2.0, 2.8, 3.6, 5.6],
            tempo: Some(crate::model::Tempo {
                downbeats: Vec::new(),
                bpm: 120.0,
                beats: vec![0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0],
                confidence: 0.62,
            }),
            audio_class: Some(crate::model::AudioClassification {
                class: crate::model::AudioClass::Speech,
                confidence: 0.71,
            }),
            ran: crate::model::AnalysisKind::ALL.to_vec(),
            music: None,
        })?;

        // A small starter timeline: an interview cut followed by some b-roll.
        let cut = self.cut_clip(interview.id, 0.0, 12.5)?;
        self.cut_clip(broll.id, 0.0, 8.0)?;
        // The interview's sound on its own audio track, as one `Extract audio` edit.
        // It is detached (so the sample does not sound twice, which is what putting
        // the same asset on A1 with the picture's own sound left on used to make) and
        // then left unlinked: the sample stands for a project that never linked
        // anything, and the tests built on it edit its clips one by one.
        self.edit_timeline("Extract audio", |timeline| {
            timeline.detach_audio(cut.id, true)?;
            timeline.unlink_clips(&[cut.id]).map(|_| ())
        })?;

        // A representative agent queue spanning the task lifecycle.
        let applied = self.add_task("Assemble a rough cut from the interview")?;
        self.complete_task(
            applied.id,
            Some("Kept 6 segments; cut 2 fillers and 14 silences (−1:48)".to_string()),
        )?;
        self.resolve_task(applied.id)?;

        let staged = self.add_task("Tighten the intro and remove filler words")?;
        self.complete_task(staged.id, Some("Staged 3 cuts; review on the timeline".to_string()))?;

        self.add_task("Balance the voiceover levels against the music bed")?;

        Ok(())
    }
}
