//! A `.kerf` project: a SQLite database holding imported assets, cached
//! analysis metadata, and the non-destructive timeline (EDL). All timeline
//! operations mutate the stored EDL; nothing is re-encoded until [`Project::export`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::engine;
use crate::error::{Error, Result};
use crate::model::{Asset, AssetAnalysis, Clip, StreamInfo, StreamKind, TimeRange, Timeline, Track};

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS assets (
    id          TEXT PRIMARY KEY,
    path        TEXT NOT NULL,
    name        TEXT NOT NULL,
    duration    REAL NOT NULL,
    streams     TEXT NOT NULL,
    imported_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS analysis (
    asset_id TEXT PRIMARY KEY,
    data     TEXT NOT NULL,
    FOREIGN KEY (asset_id) REFERENCES assets (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS timeline (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    data TEXT NOT NULL
);
"#;

pub struct Project {
    conn: Connection,
}

impl Project {
    /// Create (or overwrite the schema of) a `.kerf` file on disk.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let project = Self {
            conn: Connection::open(path)?,
        };
        project.init()?;
        Ok(project)
    }

    /// Open an existing `.kerf` file, ensuring the schema is present.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let project = Self {
            conn: Connection::open(path)?,
        };
        project.init()?;
        Ok(project)
    }

    /// An in-memory project, handy for tests and a throwaway sample.
    pub fn open_in_memory() -> Result<Self> {
        let project = Self {
            conn: Connection::open_in_memory()?,
        };
        project.init()?;
        Ok(project)
    }

    /// An in-memory project seeded with demo assets, analysis, and a timeline.
    pub fn sample() -> Result<Self> {
        let project = Self::open_in_memory()?;
        project.seed_sample()?;
        Ok(project)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(SCHEMA)?;

        let has_timeline: bool =
            self.conn
                .query_row("SELECT EXISTS(SELECT 1 FROM timeline WHERE id = 1)", [], |r| {
                    r.get(0)
                })?;
        if !has_timeline {
            self.save_timeline(&Timeline::new())?;
        }

        self.conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('kerf_version', ?1)",
            params![env!("CARGO_PKG_VERSION")],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('created_at', ?1)",
            params![Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    // ---- meta -------------------------------------------------------------

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- assets -----------------------------------------------------------

    /// Probe a media file and store its asset record.
    pub fn import_asset(&self, media_path: impl AsRef<Path>) -> Result<Asset> {
        let path = media_path.as_ref();
        let probe = engine::probe(path)?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string());
        let asset = Asset {
            id: Uuid::new_v4(),
            path: path.to_string_lossy().into_owned(),
            name,
            duration: probe.duration,
            streams: probe.streams,
            imported_at: Utc::now(),
        };
        self.insert_asset(&asset)?;
        Ok(asset)
    }

    /// Insert (or replace) an asset record directly.
    pub fn insert_asset(&self, asset: &Asset) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO assets (id, path, name, duration, streams, imported_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                asset.id.to_string(),
                asset.path,
                asset.name,
                asset.duration,
                serde_json::to_string(&asset.streams)?,
                asset.imported_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_assets(&self) -> Result<Vec<Asset>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, duration, streams, imported_at FROM assets ORDER BY imported_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut assets = Vec::new();
        for row in rows {
            let (id, path, name, duration, streams, imported_at) = row?;
            assets.push(row_to_asset(id, path, name, duration, streams, imported_at)?);
        }
        Ok(assets)
    }

    pub fn get_asset(&self, id: Uuid) -> Result<Option<Asset>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, path, name, duration, streams, imported_at FROM assets WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((id, path, name, duration, streams, imported_at)) => {
                Ok(Some(row_to_asset(id, path, name, duration, streams, imported_at)?))
            }
            None => Ok(None),
        }
    }

    pub fn require_asset(&self, id: Uuid) -> Result<Asset> {
        self.get_asset(id)?.ok_or(Error::AssetNotFound(id))
    }

    // ---- analysis ---------------------------------------------------------

    pub fn set_analysis(&self, analysis: &AssetAnalysis) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO analysis (asset_id, data) VALUES (?1, ?2)",
            params![analysis.asset_id.to_string(), serde_json::to_string(analysis)?],
        )?;
        Ok(())
    }

    pub fn get_analysis(&self, asset_id: Uuid) -> Result<Option<AssetAnalysis>> {
        let data = self
            .conn
            .query_row(
                "SELECT data FROM analysis WHERE asset_id = ?1",
                params![asset_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        match data {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    /// Run silence + scene detection (and, with the `whisper` feature and a
    /// `KERF_WHISPER_MODEL` model, transcription) against an asset's media file,
    /// cache the result, and return it.
    pub fn analyze_asset(&self, asset_id: Uuid) -> Result<AssetAnalysis> {
        use crate::analysis::{
            analyze, AnalysisProviders, FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer,
            Transcriber,
        };

        let asset = self.require_asset(asset_id)?;
        let silence = FfmpegSilenceDetector::default();
        let scene = FfmpegSceneDetector::default();
        let null = NullAnalyzer;

        #[cfg(feature = "whisper")]
        let whisper = std::env::var("KERF_WHISPER_MODEL")
            .ok()
            .filter(|m| !m.is_empty())
            .map(|m| crate::analysis::WhisperTranscriber {
                model_path: m.into(),
                language: None,
            });
        #[cfg(feature = "whisper")]
        let transcriber: &dyn Transcriber = whisper
            .as_ref()
            .map(|w| w as &dyn Transcriber)
            .unwrap_or(&null);
        #[cfg(not(feature = "whisper"))]
        let transcriber: &dyn Transcriber = &null;

        let providers = AnalysisProviders {
            silence: &silence,
            scene: &scene,
            transcriber,
        };
        let analysis = analyze(&asset, &providers)?;
        self.set_analysis(&analysis)?;
        Ok(analysis)
    }

    // ---- media extraction (preview frames, waveforms) ---------------------

    /// Decode a single frame of an asset at `time_secs` as PNG bytes, scaled to
    /// at most `max_width` px wide.
    pub fn frame_at(&self, asset_id: Uuid, time_secs: f64, max_width: u32) -> Result<Vec<u8>> {
        let asset = self.require_asset(asset_id)?;
        engine::frame_at(Path::new(&asset.path), time_secs, max_width)
    }

    /// Reduce an asset's first audio stream to `buckets` peak magnitudes in
    /// `0.0..=1.0` for waveform rendering.
    pub fn waveform(&self, asset_id: Uuid, buckets: usize) -> Result<Vec<f32>> {
        let asset = self.require_asset(asset_id)?;
        engine::waveform(Path::new(&asset.path), buckets, 8_000)
    }

    // ---- timeline ---------------------------------------------------------

    pub fn timeline(&self) -> Result<Timeline> {
        let data: String =
            self.conn
                .query_row("SELECT data FROM timeline WHERE id = 1", [], |r| r.get(0))?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save_timeline(&self, timeline: &Timeline) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO timeline (id, data) VALUES (1, ?1)",
            params![serde_json::to_string(timeline)?],
        )?;
        Ok(())
    }

    fn edit_timeline<R>(&self, f: impl FnOnce(&mut Timeline) -> Result<R>) -> Result<R> {
        let mut timeline = self.timeline()?;
        let result = f(&mut timeline)?;
        self.save_timeline(&timeline)?;
        Ok(result)
    }

    // ---- timeline operations ---------------------------------------------

    /// Add a clip referencing `[source_in, source_out)` of an asset to a track.
    /// When `track_id` is omitted the asset's primary kind picks the track;
    /// when `timeline_start` is omitted the clip is appended after the last one.
    pub fn add_clip_to_timeline(
        &self,
        asset_id: Uuid,
        track_id: Option<Uuid>,
        source_in: f64,
        source_out: f64,
        timeline_start: Option<f64>,
    ) -> Result<Clip> {
        let asset = self.require_asset(asset_id)?;
        if source_out <= source_in {
            return Err(Error::InvalidArgument(
                "source_out must be greater than source_in".to_string(),
            ));
        }
        let primary = asset.primary_kind();
        self.edit_timeline(|timeline| {
            let tid = match track_id {
                Some(t) => {
                    if timeline.track(t).is_none() {
                        return Err(Error::TrackNotFound(t));
                    }
                    t
                }
                None => timeline
                    .first_track_of(primary)
                    .ok_or_else(|| Error::Other("no suitable track for asset".to_string()))?,
            };
            let start =
                timeline_start.unwrap_or_else(|| timeline.track(tid).map(Track::end).unwrap_or(0.0));
            let clip = Clip {
                id: Uuid::new_v4(),
                asset_id,
                source_in,
                source_out,
                timeline_start: start,
                volume: 1.0,
            };
            timeline.track_mut(tid).unwrap().clips.push(clip.clone());
            Ok(clip)
        })
    }

    /// Append a cut of `[start, end)` of an asset to the matching track.
    pub fn cut_clip(&self, asset_id: Uuid, start: f64, end: f64) -> Result<Clip> {
        self.add_clip_to_timeline(asset_id, None, start, end, None)
    }

    /// Split a timeline clip at timeline time `at` into two adjacent clips.
    pub fn split_at(&self, clip_id: Uuid, at: f64) -> Result<(Clip, Clip)> {
        self.edit_timeline(|timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = timeline.tracks[ti].clips[ci].clone();
            if at <= clip.timeline_start || at >= clip.timeline_end() {
                return Err(Error::InvalidArgument(
                    "split point must lie strictly inside the clip".to_string(),
                ));
            }
            let split_src = clip.source_in + (at - clip.timeline_start);

            let mut left = clip.clone();
            left.source_out = split_src;

            let mut right = clip;
            right.id = Uuid::new_v4();
            right.source_in = split_src;
            right.timeline_start = at;

            timeline.tracks[ti].clips[ci] = left.clone();
            timeline.tracks[ti].clips.insert(ci + 1, right.clone());
            Ok((left, right))
        })
    }

    /// Adjust a clip's source in/out points (timeline position is preserved).
    pub fn trim(&self, clip_id: Uuid, source_in: Option<f64>, source_out: Option<f64>) -> Result<Clip> {
        self.edit_timeline(|timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            if let Some(value) = source_in {
                clip.source_in = value;
            }
            if let Some(value) = source_out {
                clip.source_out = value;
            }
            if clip.source_out <= clip.source_in {
                return Err(Error::InvalidArgument(
                    "source_out must be greater than source_in".to_string(),
                ));
            }
            Ok(clip.clone())
        })
    }

    /// Move a clip to a new index within its track and re-flow the track gaplessly.
    pub fn reorder(&self, track_id: Uuid, clip_id: Uuid, new_index: usize) -> Result<()> {
        self.edit_timeline(|timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            let current = track
                .clips
                .iter()
                .position(|c| c.id == clip_id)
                .ok_or(Error::ClipNotFound(clip_id))?;
            let clip = track.clips.remove(current);
            let index = new_index.min(track.clips.len());
            track.clips.insert(index, clip);
            track.reflow();
            Ok(())
        })
    }

    /// Remove a clip from the timeline.
    pub fn remove(&self, clip_id: Uuid) -> Result<()> {
        self.edit_timeline(|timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips.remove(ci);
            Ok(())
        })
    }

    /// Set a clip's linear gain.
    pub fn set_volume(&self, clip_id: Uuid, volume: f32) -> Result<Clip> {
        if volume < 0.0 {
            return Err(Error::InvalidArgument("volume must be >= 0".to_string()));
        }
        self.edit_timeline(|timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].volume = volume;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Append the non-silent spans of an asset as clips, using cached analysis.
    pub fn remove_silence(&self, asset_id: Uuid) -> Result<Vec<Clip>> {
        let asset = self.require_asset(asset_id)?;
        let analysis = self.get_analysis(asset_id)?.ok_or_else(|| {
            Error::InvalidArgument("no analysis available for asset; run analysis first".to_string())
        })?;

        let mut silence: Vec<TimeRange> = analysis.silence_segments.clone();
        silence.sort_by(|a, b| a.start.total_cmp(&b.start));

        let mut keep: Vec<(f64, f64)> = Vec::new();
        let mut cursor = 0.0;
        for span in &silence {
            if span.start > cursor {
                keep.push((cursor, span.start));
            }
            cursor = cursor.max(span.end);
        }
        if cursor < asset.duration {
            keep.push((cursor, asset.duration));
        }

        let primary = asset.primary_kind();
        self.edit_timeline(|timeline| {
            let tid = timeline
                .first_track_of(primary)
                .ok_or_else(|| Error::Other("no suitable track for asset".to_string()))?;
            let mut start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
            let mut clips = Vec::new();
            for (src_in, src_out) in keep {
                let clip = Clip {
                    id: Uuid::new_v4(),
                    asset_id,
                    source_in: src_in,
                    source_out: src_out,
                    timeline_start: start,
                    volume: 1.0,
                };
                start += clip.duration();
                timeline.track_mut(tid).unwrap().clips.push(clip.clone());
                clips.push(clip);
            }
            Ok(clips)
        })
    }

    /// Append the full audio of an asset to the first audio track.
    pub fn extract_audio(&self, asset_id: Uuid) -> Result<Clip> {
        let asset = self.require_asset(asset_id)?;
        if !asset.has_audio() {
            return Err(Error::InvalidArgument(
                "asset has no audio stream".to_string(),
            ));
        }
        self.edit_timeline(|timeline| {
            let tid = timeline
                .first_track_of(StreamKind::Audio)
                .ok_or_else(|| Error::Other("no audio track".to_string()))?;
            let start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
            let clip = Clip {
                id: Uuid::new_v4(),
                asset_id,
                source_in: 0.0,
                source_out: asset.duration,
                timeline_start: start,
                volume: 1.0,
            };
            timeline.track_mut(tid).unwrap().clips.push(clip.clone());
            Ok(clip)
        })
    }

    /// Append the full length of each asset sequentially (stitch).
    pub fn concatenate(&self, asset_ids: &[Uuid]) -> Result<Vec<Clip>> {
        let mut clips = Vec::new();
        for &asset_id in asset_ids {
            let asset = self.require_asset(asset_id)?;
            clips.push(self.cut_clip(asset_id, 0.0, asset.duration)?);
        }
        Ok(clips)
    }

    /// Render the timeline to `output_path`. Requires the `ffmpeg` feature.
    pub fn export(&self, output_path: impl AsRef<Path>, format: &str) -> Result<PathBuf> {
        let timeline = self.timeline()?;
        let assets = self.list_assets()?;
        let output = output_path.as_ref();
        engine::render(&timeline, &assets, output, format)?;
        Ok(output.to_path_buf())
    }

    // ---- sample seed ------------------------------------------------------

    fn seed_sample(&self) -> Result<()> {
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
                },
            ],
            imported_at: Utc::now(),
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
            }],
            imported_at: Utc::now(),
        };

        self.insert_asset(&interview)?;
        self.insert_asset(&broll)?;

        self.set_analysis(&AssetAnalysis {
            asset_id: interview.id,
            silence_segments: vec![
                TimeRange { start: 12.5, end: 14.0 },
                TimeRange { start: 60.0, end: 63.2 },
            ],
            scene_changes: vec![0.0, 30.0, 75.0, 110.0],
            transcript: vec![
                crate::model::TranscriptSegment {
                    start: 0.0,
                    end: 5.5,
                    text: "Welcome back to the channel.".to_string(),
                },
                crate::model::TranscriptSegment {
                    start: 5.5,
                    end: 12.5,
                    text: "Today we are talking about non-destructive editing.".to_string(),
                },
            ],
        })?;

        // A small starter timeline: an interview cut followed by some b-roll.
        self.cut_clip(interview.id, 0.0, 12.5)?;
        self.cut_clip(broll.id, 0.0, 8.0)?;
        self.extract_audio(interview.id)?;

        Ok(())
    }
}

fn row_to_asset(
    id: String,
    path: String,
    name: String,
    duration: f64,
    streams: String,
    imported_at: String,
) -> Result<Asset> {
    Ok(Asset {
        id: parse_uuid(&id)?,
        path,
        name,
        duration,
        streams: serde_json::from_str(&streams)?,
        imported_at: parse_dt(&imported_at)?,
    })
}

fn parse_uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| Error::Other(format!("invalid uuid {s}: {e}")))
}

fn parse_dt(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| Error::Other(format!("invalid datetime {s}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_project_has_assets_and_timeline() {
        let project = Project::sample().unwrap();
        let assets = project.list_assets().unwrap();
        assert_eq!(assets.len(), 2);

        let timeline = project.timeline().unwrap();
        let total_clips: usize = timeline.tracks.iter().map(|t| t.clips.len()).sum();
        assert!(total_clips >= 3);
    }

    #[test]
    fn split_and_remove_roundtrip() {
        let project = Project::open_in_memory().unwrap();
        let asset = Asset {
            id: Uuid::new_v4(),
            path: "/x.mp4".into(),
            name: "x.mp4".into(),
            duration: 10.0,
            streams: vec![StreamInfo {
                index: 0,
                kind: StreamKind::Video,
                codec: "h264".into(),
                width: Some(1280),
                height: Some(720),
                fps: Some(25.0),
                sample_rate: None,
                channels: None,
            }],
            imported_at: Utc::now(),
        };
        project.insert_asset(&asset).unwrap();

        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        let (left, right) = project.split_at(clip.id, 4.0).unwrap();
        assert!((left.duration() - 4.0).abs() < 1e-9);
        assert!((right.duration() - 6.0).abs() < 1e-9);

        project.remove(right.id).unwrap();
        assert!(project.timeline().unwrap().clip(right.id).is_none());
    }
}
