//! A `.kerf` project: a SQLite database holding imported assets, cached
//! analysis metadata, and the non-destructive timeline (EDL). All timeline
//! operations mutate the stored EDL; nothing is re-encoded until [`Project::export`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::engine;
use crate::error::{Error, Result};
use crate::model::{
    Asset, AssetAnalysis, AudioEffect, Clip, EditSource, Keyframe, Revision, StreamInfo, StreamKind, Task, TaskStatus,
    TextKeyframe, TextOverlay, TimeRange, Timeline, Track, Transition, VideoEffect,
};

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

CREATE TABLE IF NOT EXISTS history (
    seq        INTEGER PRIMARY KEY,
    label      TEXT NOT NULL,
    source     TEXT NOT NULL,
    snapshot   TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id         TEXT PRIMARY KEY,
    prompt     TEXT NOT NULL,
    status     TEXT NOT NULL,
    result     TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Covers `claim_next_task` (WHERE status = 'queued' ORDER BY created_at LIMIT 1)
-- and the queue/asset list sorts. Idempotent, so safe to apply to older files.
CREATE INDEX IF NOT EXISTS idx_tasks_status_created ON tasks (status, created_at);
CREATE INDEX IF NOT EXISTS idx_tasks_created       ON tasks (created_at);
CREATE INDEX IF NOT EXISTS idx_assets_imported     ON assets (imported_at);
"#;

/// `meta` key holding the seq of the currently-applied revision.
const HISTORY_HEAD: &str = "history_head";

pub struct Project {
    conn: Connection,
    /// The `.kerf` file backing this project, or `None` for an in-memory one.
    /// Edits write through to the connection, so a file-backed project persists
    /// automatically; an in-memory one must be [`Project::save_as`]'d first.
    path: Option<PathBuf>,
    /// Attributed to edits recorded in the history (see [`Project::set_actor`]).
    actor: EditSource,
}

impl Project {
    /// Create (or overwrite the schema of) a `.kerf` file on disk.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let project = Self {
            conn: Connection::open(&path)?,
            path: Some(path),
            actor: EditSource::User,
        };
        project.init()?;
        Ok(project)
    }

    /// Open an existing `.kerf` file, ensuring the schema is present.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let project = Self {
            conn: Connection::open(&path)?,
            path: Some(path),
            actor: EditSource::User,
        };
        project.init()?;
        Ok(project)
    }

    /// An in-memory project, handy for tests and a throwaway sample.
    pub fn open_in_memory() -> Result<Self> {
        let project = Self {
            conn: Connection::open_in_memory()?,
            path: None,
            actor: EditSource::User,
        };
        project.init()?;
        Ok(project)
    }

    /// Set who subsequent edits are attributed to in the history. The MCP server
    /// calls this with [`EditSource::Agent`]; the desktop app keeps the default
    /// [`EditSource::User`].
    pub fn set_actor(&mut self, actor: EditSource) {
        self.actor = actor;
    }

    /// The `.kerf` file backing this project, if any. `None` means it lives only
    /// in memory (the seeded sample) and edits are not yet persisted to disk.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Snapshot the entire project database to a new `.kerf` file on disk. The
    /// in-memory project itself is unchanged; the caller reopens the file (via
    /// [`Project::open`]) to make subsequent edits write through to it. This is
    /// how "Save As" turns the throwaway sample into a persistent project.
    pub fn save_as(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        // `VACUUM INTO` refuses to write to an existing file; the save dialog
        // has already confirmed any overwrite, so clear it first.
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let dst = path
            .to_str()
            .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 project path: {}", path.display())))?;
        self.conn.execute("VACUUM INTO ?1", params![dst])?;
        Ok(())
    }

    /// An in-memory project seeded with demo assets, analysis, and a timeline.
    pub fn sample() -> Result<Self> {
        let project = Self::open_in_memory()?;
        project.seed_sample()?;
        Ok(project)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(SCHEMA)?;

        let has_timeline: bool = self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM timeline WHERE id = 1)", [], |r| r.get(0))?;
        if !has_timeline {
            self.save_timeline(&Timeline::new())?;
        }

        let has_history: bool = self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM history)", [], |r| r.get(0))?;
        if !has_history {
            let snapshot = serde_json::to_string(&self.timeline()?)?;
            self.conn.execute(
                "INSERT INTO history (seq, label, source, snapshot, created_at)
                 VALUES (0, 'Initial state', ?1, ?2, ?3)",
                params![EditSource::System.as_str(), snapshot, Utc::now().to_rfc3339()],
            )?;
            self.set_meta(HISTORY_HEAD, "0")?;
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
        let asset = Self::probe_asset(media_path.as_ref())?;
        self.insert_asset(&asset)?;
        Ok(asset)
    }

    /// Probe a media file into a fresh [`Asset`] record *without* `&self` — the
    /// ffprobe run doesn't need the project lock, so callers importing several
    /// files can probe them concurrently and take the lock only for the quick
    /// [`Project::insert_asset`].
    pub fn probe_asset(path: &Path) -> Result<Asset> {
        let probe = engine::probe(path)?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string());
        // A still image probes with no duration; give it a default timeline length
        // so it's placeable (the clip can be trimmed afterwards like any other).
        let is_image = probe.streams.iter().any(|s| s.image);
        let duration = if is_image && probe.duration <= 0.0 {
            crate::model::DEFAULT_IMAGE_DURATION
        } else {
            probe.duration
        };
        Ok(Asset {
            id: Uuid::new_v4(),
            path: path.to_string_lossy().into_owned(),
            name,
            duration,
            streams: probe.streams,
            imported_at: Utc::now(),
        })
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
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, name, duration, streams, imported_at FROM assets ORDER BY imported_at")?;
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
        let asset = self.require_asset(asset_id)?;
        // The heavy ffmpeg work lives in `analysis::analyze_asset_media`, a free
        // function — so the GUI/MCP adapters can run it without holding the
        // shared Project lock and then re-lock only for the quick `set_analysis`.
        let analysis = crate::analysis::analyze_asset_media(&asset)?;
        self.set_analysis(&analysis)?;
        Ok(analysis)
    }

    // ---- media extraction (preview frames, waveforms) ---------------------

    /// Decode a single frame of an asset at `time_secs` as PNG bytes, scaled to
    /// at most `max_width` px wide.
    pub fn frame_at(&self, asset_id: Uuid, time_secs: f64, max_width: u32) -> Result<Vec<u8>> {
        let asset = self.require_asset(asset_id)?;
        // A still image has one frame at t=0; seeking past it decodes nothing.
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        engine::frame_at(Path::new(&asset.path), time_secs, max_width)
    }

    /// Decode a single frame of an asset at `time_secs` as JPEG bytes (`quality`
    /// = ffmpeg `-q:v`, 2 = best … 31 = worst), scaled to at most `max_width` px
    /// wide. Smaller than [`frame_at`]'s PNG — for handing the frame to an LLM.
    pub fn frame_jpeg(&self, asset_id: Uuid, time_secs: f64, max_width: u32, quality: u8) -> Result<Vec<u8>> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_preview_frame(&asset, time_secs, max_width, quality, true)
    }

    /// Decode a preview frame for an already-resolved [`Asset`] as JPEG bytes,
    /// *without* needing `&self` — so the caller can release the project lock
    /// before the (potentially slow) ffmpeg decode runs, instead of freezing
    /// every other project op for its duration. `accurate = false` snaps to the
    /// nearest keyframe for fast scrubbing; a still decodes its one frame at t=0.
    pub fn decode_preview_frame(asset: &Asset, time_secs: f64, max_width: u32, quality: u8, accurate: bool) -> Result<Vec<u8>> {
        // A still image has one frame at t=0; seeking past it decodes nothing.
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        // Decode from the all-intra proxy when one is ready (every frame a
        // keyframe → the seek decodes exactly one frame); export always reads the
        // original — only previews consult the proxy.
        let path = Self::preview_source(asset);
        engine::frame_jpeg(&path, time_secs, max_width, quality, accurate)
    }

    /// Decode a window of an asset's audio as mono s16le PCM at `sample_rate`,
    /// for the GUI's preview playback. Static like
    /// [`Project::decode_preview_frame`] so the caller can release the project
    /// lock before the ffmpeg decode runs. Always reads the original source —
    /// proxies are video-only.
    pub fn decode_audio_pcm(asset: &Asset, start: f64, duration: f64, sample_rate: u32) -> Result<Vec<u8>> {
        engine::audio_pcm(Path::new(&asset.path), start, duration, sample_rate)
    }

    /// The media path a preview should decode for `asset`: its generated proxy
    /// when one is ready on disk, else the original source. Only the preview
    /// paths ([`Project::decode_preview_frame`] and the [`Project::timeline_frame`]
    /// compositor) consult this — export always uses the original `asset.path`.
    /// Stills and audio-only assets never get a proxy, so they resolve to the
    /// original. Falls back to the original whenever no proxy exists yet, so a
    /// preview never blocks waiting on generation.
    fn preview_source(asset: &Asset) -> PathBuf {
        let has_video = asset.streams.iter().any(|s| s.kind == StreamKind::Video);
        if has_video && !asset.is_image() {
            if let Some(proxy) = engine::ready_proxy(Path::new(&asset.path)) {
                return proxy;
            }
        }
        PathBuf::from(&asset.path)
    }

    /// Build a `columns`×`rows` contact sheet of an asset — frames sampled evenly
    /// across `[start, end)` (defaulting to the whole asset) tiled into one JPEG,
    /// each cell `cell_width` px wide. Returns the montage bytes and the row-major
    /// per-cell timestamps, so an LLM can skim the footage and name good moments.
    #[allow(clippy::too_many_arguments)]
    pub fn skim_asset(
        &self,
        asset_id: Uuid,
        start: Option<f64>,
        end: Option<f64>,
        columns: u32,
        rows: u32,
        cell_width: u32,
        quality: u8,
    ) -> Result<(Vec<u8>, Vec<f64>)> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_contact_sheet(&asset, start, end, columns, rows, cell_width, quality)
    }

    /// Build the contact sheet for an already-resolved [`Asset`], *without*
    /// `&self` — so the caller can release the project lock before the
    /// (many-seek) ffmpeg sampling runs. See [`Project::skim_asset`].
    #[allow(clippy::too_many_arguments)]
    pub fn decode_contact_sheet(
        asset: &Asset,
        start: Option<f64>,
        end: Option<f64>,
        columns: u32,
        rows: u32,
        cell_width: u32,
        quality: u8,
    ) -> Result<(Vec<u8>, Vec<f64>)> {
        let start = start.unwrap_or(0.0).max(0.0);
        let end = end.unwrap_or(asset.duration).min(asset.duration).max(start);
        engine::contact_sheet(Path::new(&asset.path), start, end, columns, rows, cell_width, quality)
    }

    /// Composite a single still of the current timeline at timeline time `t` as
    /// JPEG bytes (`quality` = ffmpeg `-q:v`), the canvas at most `max_width` px
    /// wide — what the edit looks like on screen at `t`, for an LLM to review.
    pub fn timeline_frame(&self, time_secs: f64, max_width: u32, quality: u8) -> Result<Vec<u8>> {
        let (timeline, assets) = self.timeline_frame_inputs()?;
        Self::composite_timeline_frame(&timeline, &assets, time_secs, max_width, quality)
    }

    /// The owned inputs the timeline-frame compositor needs (timeline + the
    /// proxy-swapped preview asset list), resolved together so a caller can pull
    /// them out under the project lock and then **drop the guard** before running
    /// the slow ffmpeg composite — see [`Project::composite_timeline_frame`].
    pub fn timeline_frame_inputs(&self) -> Result<(Timeline, Vec<Asset>)> {
        Ok((self.timeline()?, self.preview_assets()?))
    }

    /// Composite a timeline still from already-resolved inputs, **without**
    /// `&self` — so the GUI preview (which fetches frames continuously during
    /// playback) can release the shared project lock before this ffmpeg decode,
    /// instead of freezing every other op for its duration. Mirrors
    /// [`Project::decode_preview_frame`]'s lock-free shape for single frames.
    pub fn composite_timeline_frame(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        max_width: u32,
        quality: u8,
    ) -> Result<Vec<u8>> {
        engine::timeline_frame(
            timeline,
            assets,
            &engine::ExportOptions::default(),
            time_secs,
            max_width,
            quality,
        )
    }

    /// [`Project::list_assets`], but with each eligible asset's `path` swapped to
    /// its ready proxy — the asset list the timeline-preview compositor decodes
    /// from. Stream metadata (resolution / fps) is kept from the original, so the
    /// composite geometry and source-time mapping match the export exactly; only
    /// the decoded pixels come from the lighter all-intra proxy. Export reads
    /// [`Project::list_assets`] (originals) and is unaffected.
    fn preview_assets(&self) -> Result<Vec<Asset>> {
        let mut assets = self.list_assets()?;
        for asset in &mut assets {
            asset.path = Self::preview_source(asset).to_string_lossy().into_owned();
        }
        Ok(assets)
    }

    /// Reduce an asset's first audio stream to `buckets` peak magnitudes in
    /// `0.0..=1.0` for waveform rendering.
    pub fn waveform(&self, asset_id: Uuid, buckets: usize) -> Result<Vec<f32>> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_waveform(&asset, buckets)
    }

    /// Waveform peaks for an already-resolved [`Asset`], *without* `&self` — so
    /// the caller can release the project lock before the whole-file ffmpeg
    /// decode. See [`Project::waveform`].
    pub fn decode_waveform(asset: &Asset, buckets: usize) -> Result<Vec<f32>> {
        engine::waveform(Path::new(&asset.path), buckets, 8_000)
    }

    /// Reduce an asset's first audio stream to `buckets` RMS magnitudes in
    /// `0.0..=1.0` — a perceptual energy-over-time curve. Companion to
    /// [`Self::waveform`] (which returns peaks); RMS better reflects loudness.
    pub fn energy(&self, asset_id: Uuid, buckets: usize) -> Result<Vec<f32>> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_energy(&asset, buckets)
    }

    /// Energy envelope for an already-resolved [`Asset`], *without* `&self` —
    /// lock-free like [`Project::decode_waveform`].
    pub fn decode_energy(asset: &Asset, buckets: usize) -> Result<Vec<f32>> {
        engine::energy_envelope(Path::new(&asset.path), buckets, 8_000)
    }

    // ---- timeline ---------------------------------------------------------

    pub fn timeline(&self) -> Result<Timeline> {
        let data: String = self
            .conn
            .query_row("SELECT data FROM timeline WHERE id = 1", [], |r| r.get(0))?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save_timeline(&self, timeline: &Timeline) -> Result<()> {
        self.save_timeline_str(&serde_json::to_string(timeline)?)
    }

    /// Persist a pre-serialized timeline blob, so callers that already hold the
    /// JSON (an edit + its history snapshot) don't serialize the same timeline
    /// twice.
    fn save_timeline_str(&self, json: &str) -> Result<()> {
        self.conn
            .execute("INSERT OR REPLACE INTO timeline (id, data) VALUES (1, ?1)", params![json])?;
        Ok(())
    }

    /// Apply a mutation to the timeline, persist it, and record a new revision
    /// in the history (attributed to the current [`Project::actor`]). The blob
    /// write and the history append are wrapped in a single transaction — so an
    /// edit and its history head move atomically — and the timeline is
    /// serialized once and reused for both writes.
    fn edit_timeline<R>(&self, label: &str, f: impl FnOnce(&mut Timeline) -> Result<R>) -> Result<R> {
        let tx = self.conn.unchecked_transaction()?;
        let mut timeline = self.timeline()?;
        let result = f(&mut timeline)?;
        let json = serde_json::to_string(&timeline)?;
        self.save_timeline_str(&json)?;
        self.record_revision(label, self.actor, &json)?;
        tx.commit()?;
        Ok(result)
    }

    // ---- history ----------------------------------------------------------

    fn head(&self) -> Result<i64> {
        match self.meta(HISTORY_HEAD)?.and_then(|s| s.parse::<i64>().ok()) {
            Some(seq) => Ok(seq),
            // A missing/corrupt head must not be read as 0 — `record_revision`
            // would then `DELETE FROM history WHERE seq > 0` and wipe the whole
            // edit log. Recover the real tip from the history table and persist it.
            None => {
                let seq: i64 = self
                    .conn
                    .query_row("SELECT COALESCE(MAX(seq), 0) FROM history", [], |r| r.get(0))?;
                self.set_head(seq)?;
                Ok(seq)
            }
        }
    }

    fn set_head(&self, seq: i64) -> Result<()> {
        self.set_meta(HISTORY_HEAD, &seq.to_string())
    }

    /// Append a revision after the current head, dropping any redo branch.
    fn record_revision(&self, label: &str, source: EditSource, snapshot: &str) -> Result<i64> {
        let head = self.head()?;
        self.conn.execute("DELETE FROM history WHERE seq > ?1", params![head])?;
        let seq = head + 1;
        self.conn.execute(
            "INSERT INTO history (seq, label, source, snapshot, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![seq, label, source.as_str(), snapshot, Utc::now().to_rfc3339()],
        )?;
        self.set_head(seq)?;
        Ok(seq)
    }

    /// Restore the stored snapshot at `seq` as the live timeline and move the
    /// head there. Does not itself record a new revision.
    fn restore(&self, seq: i64) -> Result<Timeline> {
        let snapshot: Option<String> = self
            .conn
            .query_row("SELECT snapshot FROM history WHERE seq = ?1", params![seq], |r| r.get(0))
            .optional()?;
        let snapshot = snapshot.ok_or(Error::RevisionNotFound(seq))?;
        let timeline: Timeline = serde_json::from_str(&snapshot)?;
        let tx = self.conn.unchecked_transaction()?;
        self.save_timeline_str(&snapshot)?;
        self.set_head(seq)?;
        tx.commit()?;
        Ok(timeline)
    }

    /// The full edit history, oldest first; the entry matching the head is `current`.
    pub fn history(&self) -> Result<Vec<Revision>> {
        let head = self.head()?;
        let mut stmt = self
            .conn
            .prepare("SELECT seq, label, source, created_at FROM history ORDER BY seq")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut revisions = Vec::new();
        for row in rows {
            let (seq, label, source, created_at) = row?;
            revisions.push(Revision {
                seq,
                label,
                source: parse_source(&source),
                created_at: parse_dt(&created_at)?,
                current: seq == head,
            });
        }
        Ok(revisions)
    }

    pub fn can_undo(&self) -> Result<bool> {
        let head = self.head()?;
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM history WHERE seq < ?1", params![head], |r| r.get(0))?;
        Ok(count > 0)
    }

    pub fn can_redo(&self) -> Result<bool> {
        let head = self.head()?;
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM history WHERE seq > ?1", params![head], |r| r.get(0))?;
        Ok(count > 0)
    }

    /// Step the head back one revision, returning the restored timeline.
    pub fn undo(&self) -> Result<Timeline> {
        let head = self.head()?;
        let prev: Option<i64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM history WHERE seq < ?1", params![head], |r| r.get(0))?;
        match prev {
            Some(seq) => self.restore(seq),
            None => Err(Error::InvalidArgument("nothing to undo".to_string())),
        }
    }

    /// Step the head forward one revision, returning the restored timeline.
    pub fn redo(&self) -> Result<Timeline> {
        let head = self.head()?;
        let next: Option<i64> = self
            .conn
            .query_row("SELECT MIN(seq) FROM history WHERE seq > ?1", params![head], |r| r.get(0))?;
        match next {
            Some(seq) => self.restore(seq),
            None => Err(Error::InvalidArgument("nothing to redo".to_string())),
        }
    }

    /// Jump the head to any revision `seq`, returning the restored timeline.
    pub fn revert_to(&self, seq: i64) -> Result<Timeline> {
        self.restore(seq)
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
        self.edit_timeline("Add clip", |timeline| {
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
            let start = timeline_start.unwrap_or_else(|| timeline.track(tid).map(Track::end).unwrap_or(0.0));
            let clip = Clip::new(asset_id, source_in, source_out, start);
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
        self.edit_timeline("Split clip", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = timeline.tracks[ti].clips[ci].clone();
            if at <= clip.timeline_start || at >= clip.timeline_end() {
                return Err(Error::InvalidArgument(
                    "split point must lie strictly inside the clip".to_string(),
                ));
            }
            // Map the timeline split point to a source point honoring speed (the
            // source advances by |speed| per timeline second), and backwards for a
            // reversed clip, so the two halves stay gapless and keep total duration.
            let offset = (at - clip.timeline_start) * clip.speed_mag();
            let (mut left, mut right) = (clip.clone(), clip);
            right.id = Uuid::new_v4();
            right.timeline_start = at;
            right.transition_in = None; // the transition stays with the left (start) half
            if left.is_reversed() {
                let split_src = (left.source_out - offset).clamp(left.source_in, left.source_out);
                left.source_in = split_src;
                right.source_out = split_src;
            } else {
                let split_src = (left.source_in + offset).clamp(left.source_in, left.source_out);
                left.source_out = split_src;
                right.source_in = split_src;
            }

            timeline.tracks[ti].clips[ci] = left.clone();
            timeline.tracks[ti].clips.insert(ci + 1, right.clone());
            Ok((left, right))
        })
    }

    /// Adjust a clip's source in/out points. `timeline_start` moves the clip in
    /// the same edit — a left-edge trim from the GUI shifts the start so the
    /// right edge stays put, and doing both here keeps undo a single step.
    /// Omitted, the timeline position is preserved.
    pub fn trim(
        &self,
        clip_id: Uuid,
        source_in: Option<f64>,
        source_out: Option<f64>,
        timeline_start: Option<f64>,
    ) -> Result<Clip> {
        self.edit_timeline("Trim clip", |timeline| {
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
            if let Some(start) = timeline_start {
                clip.timeline_start = start.max(0.0);
            }
            let out = clip.clone();
            if timeline_start.is_some() {
                timeline.tracks[ti].sort_by_start();
            }
            Ok(out)
        })
    }

    /// Cut a **source-time** range out of a clip: the clip is split around the
    /// intersection of `[from, to]` with its source window, the middle piece
    /// removed, and later clips on the track ripple left to close the gap.
    /// This is the transcript-editing primitive — delete a sentence and the
    /// cut tightens. Returns the kept pieces in play order.
    pub fn cut_clip_range(&self, clip_id: Uuid, from: f64, to: f64) -> Result<Vec<Clip>> {
        self.edit_timeline("Cut range", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = timeline.tracks[ti].clips[ci].clone();
            let a = from.max(clip.source_in);
            let b = to.min(clip.source_out);
            if b - a <= 1e-9 {
                return Err(Error::InvalidArgument(
                    "range does not overlap the clip's source window".to_string(),
                ));
            }
            let removed = (b - a) / clip.speed_mag();

            // The kept source spans in play order — a reversed clip plays the
            // upper span first. A piece that is the sole survivor keeps the
            // original id and both fades (the cut is just a trim); otherwise
            // the fades facing the removed middle are dropped.
            let (head, tail) = if clip.is_reversed() {
                ((b, clip.source_out), (clip.source_in, a))
            } else {
                ((clip.source_in, a), (b, clip.source_out))
            };
            let head_ok = head.1 - head.0 > 1e-9;
            let tail_ok = tail.1 - tail.0 > 1e-9;
            let mut pieces: Vec<Clip> = Vec::new();
            let mut cursor = clip.timeline_start;
            if head_ok {
                let mut p = clip.clone();
                (p.source_in, p.source_out) = head;
                p.timeline_start = cursor;
                if tail_ok {
                    p.fade_out = 0.0;
                }
                cursor = p.timeline_end();
                pieces.push(p);
            }
            if tail_ok {
                let mut p = clip.clone();
                (p.source_in, p.source_out) = tail;
                p.timeline_start = cursor;
                if head_ok {
                    p.id = Uuid::new_v4();
                    p.fade_in = 0.0;
                    p.transition_in = None;
                }
                pieces.push(p);
            }

            let track = &mut timeline.tracks[ti];
            track.clips.remove(ci);
            for c in &mut track.clips {
                if c.timeline_start > clip.timeline_start + 1e-9 {
                    c.timeline_start = (c.timeline_start - removed).max(0.0);
                }
            }
            track.clips.extend(pieces.iter().cloned());
            track.sort_by_start();
            Ok(pieces)
        })
    }

    /// Move a clip to a new index within its track and re-flow the track gaplessly.
    pub fn reorder(&self, track_id: Uuid, clip_id: Uuid, new_index: usize) -> Result<()> {
        self.edit_timeline("Reorder clip", |timeline| {
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

    /// Move a clip to a new timeline position, optionally onto another track of
    /// the **same kind**. Free positioning: the clip keeps its duration and
    /// gaps are allowed. A move that would overlap another clip on the
    /// destination track is rejected, so each track stays a well-ordered,
    /// non-overlapping lane (which keeps the positional render well-defined).
    pub fn move_clip(&self, clip_id: Uuid, timeline_start: f64, track_id: Option<Uuid>) -> Result<Clip> {
        let start = timeline_start.max(0.0);
        self.edit_timeline("Move clip", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let src_kind = timeline.tracks[ti].kind;
            let dest_ti = match track_id {
                Some(tid) => {
                    let d = timeline
                        .tracks
                        .iter()
                        .position(|t| t.id == tid)
                        .ok_or(Error::TrackNotFound(tid))?;
                    if timeline.tracks[d].kind != src_kind {
                        return Err(Error::InvalidArgument(
                            "cannot move a clip to a track of a different kind".to_string(),
                        ));
                    }
                    d
                }
                None => ti,
            };
            let mut clip = timeline.tracks[ti].clips[ci].clone();
            let end = start + clip.duration();
            let overlaps = timeline.tracks[dest_ti]
                .clips
                .iter()
                .any(|c| c.id != clip_id && start < c.timeline_end() && c.timeline_start < end);
            if overlaps {
                return Err(Error::InvalidArgument(
                    "clip would overlap another clip on the destination track".to_string(),
                ));
            }
            clip.timeline_start = start;
            timeline.tracks[ti].clips.remove(ci);
            timeline.tracks[dest_ti].clips.push(clip.clone());
            timeline.tracks[dest_ti].sort_by_start();
            Ok(clip)
        })
    }

    /// Remove a clip and close the gap it leaves: every later clip on the **same
    /// track** shifts left by the removed clip's duration. (Plain [`remove`]
    /// leaves a gap.)
    pub fn ripple_delete(&self, clip_id: Uuid) -> Result<()> {
        self.edit_timeline("Ripple delete", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let removed = timeline.tracks[ti].clips[ci].clone();
            let dur = removed.duration();
            let from = removed.timeline_start;
            timeline.tracks[ti].clips.remove(ci);
            for c in &mut timeline.tracks[ti].clips {
                if c.timeline_start >= from {
                    c.timeline_start = (c.timeline_start - dur).max(0.0);
                }
            }
            Ok(())
        })
    }

    /// Append a new empty track of `kind`, keeping kinds grouped (video tracks
    /// above audio tracks) and auto-naming it (`V2`, `A2`, …) when `name` is
    /// omitted. Later video tracks composite **on top** at export.
    pub fn add_track(&self, kind: StreamKind, name: Option<String>) -> Result<Track> {
        self.edit_timeline("Add track", |timeline| {
            let count = timeline.tracks.iter().filter(|t| t.kind == kind).count();
            let name = name.unwrap_or_else(|| {
                let prefix = if kind == StreamKind::Audio { "A" } else { "V" };
                format!("{prefix}{}", count + 1)
            });
            let track = Track {
                id: Uuid::new_v4(),
                kind,
                name,
                clips: Vec::new(),
                duck: false,
            };
            // Insert video tracks just after the last video track and audio
            // tracks at the very end, so the lanes stay grouped (V1, V2, …, A1, A2).
            let at = match kind {
                StreamKind::Audio => timeline.tracks.len(),
                _ => timeline
                    .tracks
                    .iter()
                    .rposition(|t| t.kind == StreamKind::Video)
                    .map(|i| i + 1)
                    .unwrap_or(0),
            };
            timeline.tracks.insert(at, track.clone());
            Ok(track)
        })
    }

    /// Flag or unflag a track for export-time ducking: a flagged track's audio
    /// is sidechain-compressed under the non-ducked tracks (music dips under
    /// dialogue automatically).
    pub fn set_track_duck(&self, track_id: Uuid, duck: bool) -> Result<Track> {
        self.edit_timeline(if duck { "Duck track" } else { "Unduck track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.duck = duck;
            Ok(track.clone())
        })
    }

    /// Remove a track and all of its clips. Refuses to remove the last track.
    pub fn remove_track(&self, track_id: Uuid) -> Result<()> {
        self.edit_timeline("Remove track", |timeline| {
            let idx = timeline
                .tracks
                .iter()
                .position(|t| t.id == track_id)
                .ok_or(Error::TrackNotFound(track_id))?;
            if timeline.tracks.len() <= 1 {
                return Err(Error::InvalidArgument("cannot remove the last track".to_string()));
            }
            timeline.tracks.remove(idx);
            Ok(())
        })
    }

    /// Remove a clip from the timeline.
    pub fn remove(&self, clip_id: Uuid) -> Result<()> {
        self.edit_timeline("Remove clip", |timeline| {
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
        self.edit_timeline("Set volume", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].volume = volume;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Set a clip's fade-in and/or fade-out duration (seconds). `None` leaves a
    /// value unchanged; pass `Some(0.0)` to clear a fade. Negative values are
    /// rejected. The fade is realized at export (see the engine render path).
    pub fn set_fade(&self, clip_id: Uuid, fade_in: Option<f64>, fade_out: Option<f64>) -> Result<Clip> {
        if fade_in.is_some_and(|v| v < 0.0) || fade_out.is_some_and(|v| v < 0.0) {
            return Err(Error::InvalidArgument("fade duration must be >= 0".to_string()));
        }
        self.edit_timeline("Set fade", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            if let Some(value) = fade_in {
                clip.fade_in = value;
            }
            if let Some(value) = fade_out {
                clip.fade_out = value;
            }
            Ok(clip.clone())
        })
    }

    /// Set a clip's playback speed (1.0 = unchanged, negative = reverse). The
    /// magnitude is clamped away from zero so the duration stays finite. Changing
    /// speed retimes the clip and so changes its timeline duration (like a trim).
    pub fn set_speed(&self, clip_id: Uuid, speed: f64) -> Result<Clip> {
        if !speed.is_finite() || speed == 0.0 {
            return Err(Error::InvalidArgument("speed must be a non-zero, finite number".to_string()));
        }
        self.edit_timeline("Set speed", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].speed = speed;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Update a clip's geometric transform. Each `None` leaves that field
    /// unchanged. Realized when compositing at export.
    #[allow(clippy::too_many_arguments)]
    pub fn set_transform(
        &self,
        clip_id: Uuid,
        scale: Option<f64>,
        pos_x: Option<f64>,
        pos_y: Option<f64>,
        rotation: Option<f64>,
        opacity: Option<f64>,
        crop_left: Option<f64>,
        crop_right: Option<f64>,
        crop_top: Option<f64>,
        crop_bottom: Option<f64>,
    ) -> Result<Clip> {
        if scale.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err(Error::InvalidArgument("scale must be a finite value > 0".to_string()));
        }
        if opacity.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
            return Err(Error::InvalidArgument("opacity must be within 0.0..=1.0".to_string()));
        }
        if [crop_left, crop_right, crop_top, crop_bottom]
            .into_iter()
            .flatten()
            .any(|c| !(0.0..1.0).contains(&c))
        {
            return Err(Error::InvalidArgument("crop fractions must be within 0.0..1.0".to_string()));
        }
        self.edit_timeline("Set transform", |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let t = &mut timeline.tracks[ti].clips[ci].transform;
            if let Some(v) = scale {
                t.scale = v;
            }
            if let Some(v) = pos_x {
                t.pos_x = v;
            }
            if let Some(v) = pos_y {
                t.pos_y = v;
            }
            if let Some(v) = rotation {
                t.rotation = v;
            }
            if let Some(v) = opacity {
                t.opacity = v;
            }
            if let Some(v) = crop_left {
                t.crop_left = v;
            }
            if let Some(v) = crop_right {
                t.crop_right = v;
            }
            if let Some(v) = crop_top {
                t.crop_top = v;
            }
            if let Some(v) = crop_bottom {
                t.crop_bottom = v;
            }
            if t.crop_left + t.crop_right >= 1.0 || t.crop_top + t.crop_bottom >= 1.0 {
                return Err(Error::InvalidArgument("crop removes the entire frame".to_string()));
            }
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Update a clip's color correction. Each `None` leaves that field unchanged.
    pub fn set_color(
        &self,
        clip_id: Uuid,
        brightness: Option<f64>,
        contrast: Option<f64>,
        saturation: Option<f64>,
        gamma: Option<f64>,
    ) -> Result<Clip> {
        if brightness.is_some_and(|v| !(-1.0..=1.0).contains(&v)) {
            return Err(Error::InvalidArgument("brightness must be within -1.0..=1.0".to_string()));
        }
        if contrast.is_some_and(|v| !(0.0..=4.0).contains(&v)) {
            return Err(Error::InvalidArgument("contrast must be within 0.0..=4.0".to_string()));
        }
        if saturation.is_some_and(|v| !(0.0..=3.0).contains(&v)) {
            return Err(Error::InvalidArgument("saturation must be within 0.0..=3.0".to_string()));
        }
        if gamma.is_some_and(|v| !(0.1..=10.0).contains(&v)) {
            return Err(Error::InvalidArgument("gamma must be within 0.1..=10.0".to_string()));
        }
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
            Ok(timeline.tracks[ti].clips[ci].clone())
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

    /// Append the non-silent spans of an asset as clips, using cached analysis.
    pub fn remove_silence(&self, asset_id: Uuid) -> Result<Vec<Clip>> {
        let asset = self.require_asset(asset_id)?;
        let analysis = self
            .get_analysis(asset_id)?
            .ok_or_else(|| Error::InvalidArgument("no analysis available for asset; run analysis first".to_string()))?;

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
        self.edit_timeline("Remove silence", |timeline| {
            let tid = timeline
                .first_track_of(primary)
                .ok_or_else(|| Error::Other("no suitable track for asset".to_string()))?;
            let mut start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
            let mut clips = Vec::new();
            for (src_in, src_out) in keep {
                let clip = Clip::new(asset_id, src_in, src_out, start);
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
            return Err(Error::InvalidArgument("asset has no audio stream".to_string()));
        }
        self.edit_timeline("Extract audio", |timeline| {
            let tid = timeline
                .first_track_of(StreamKind::Audio)
                .ok_or_else(|| Error::Other("no audio track".to_string()))?;
            let start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
            let clip = Clip::new(asset_id, 0.0, asset.duration, start);
            timeline.track_mut(tid).unwrap().clips.push(clip.clone());
            Ok(clip)
        })
    }

    /// Append the full length of each asset sequentially (stitch). One atomic
    /// edit — a single timeline write and one "Concatenate" revision — rather
    /// than one `cut_clip` (and one undo step) per asset.
    pub fn concatenate(&self, asset_ids: &[Uuid]) -> Result<Vec<Clip>> {
        // Validate every asset up front so the edit either fully applies or not
        // at all (no partial stitch left behind on a bad id).
        let mut plan = Vec::with_capacity(asset_ids.len());
        for &asset_id in asset_ids {
            let asset = self.require_asset(asset_id)?;
            plan.push((asset_id, asset.primary_kind(), asset.duration));
        }
        self.edit_timeline("Concatenate", |timeline| {
            let mut clips = Vec::with_capacity(plan.len());
            for (asset_id, primary, duration) in &plan {
                let tid = timeline
                    .first_track_of(*primary)
                    .ok_or_else(|| Error::Other("no suitable track for asset".to_string()))?;
                let start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
                let clip = Clip::new(*asset_id, 0.0, *duration, start);
                timeline.track_mut(tid).unwrap().clips.push(clip.clone());
                clips.push(clip);
            }
            Ok(clips)
        })
    }

    /// Render the timeline to `output_path`. Requires the `ffmpeg` feature.
    pub fn export(&self, output_path: impl AsRef<Path>, format: &str) -> Result<PathBuf> {
        let timeline = self.timeline()?;
        let assets = self.list_assets()?;
        let output = output_path.as_ref();
        engine::render(&timeline, &assets, output, format)?;
        Ok(output.to_path_buf())
    }

    /// Like [`export`] but with explicit [`engine::ExportOptions`].
    pub fn export_with(&self, output_path: impl AsRef<Path>, opts: &engine::ExportOptions) -> Result<PathBuf> {
        let timeline = self.timeline()?;
        let assets = self.list_assets()?;
        let output = output_path.as_ref();
        engine::render_with(&timeline, &assets, output, opts)?;
        Ok(output.to_path_buf())
    }

    // ---- agent task queue -------------------------------------------------

    /// Enqueue a task for a connected agent to claim. Returns the new `queued`
    /// task.
    pub fn add_task(&self, prompt: &str) -> Result<Task> {
        let now = Utc::now();
        let task = Task {
            id: Uuid::new_v4(),
            prompt: prompt.to_string(),
            status: TaskStatus::Queued,
            result: None,
            created_at: now,
            updated_at: now,
        };
        self.upsert_task(&task)?;
        Ok(task)
    }

    fn upsert_task(&self, task: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tasks (id, prompt, status, result, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                task.id.to_string(),
                task.prompt,
                task.status.as_str(),
                task.result,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, prompt, status, result, created_at, updated_at FROM tasks ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (id, prompt, status, result, created_at, updated_at) = row?;
            tasks.push(row_to_task(id, prompt, status, result, created_at, updated_at)?);
        }
        Ok(tasks)
    }

    pub fn get_task(&self, id: Uuid) -> Result<Option<Task>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, prompt, status, result, created_at, updated_at FROM tasks WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((id, prompt, status, result, created_at, updated_at)) => {
                Ok(Some(row_to_task(id, prompt, status, result, created_at, updated_at)?))
            }
            None => Ok(None),
        }
    }

    pub fn require_task(&self, id: Uuid) -> Result<Task> {
        self.get_task(id)?.ok_or(Error::TaskNotFound(id))
    }

    /// Mark a specific task `working` (an agent has claimed it).
    pub fn claim_task(&self, id: Uuid) -> Result<Task> {
        self.set_task_state(id, TaskStatus::Working, None)
    }

    /// Claim the oldest `queued` task, marking it `working`. Returns `None` when
    /// nothing is waiting — the agent's "give me work" primitive.
    pub fn claim_next_task(&self) -> Result<Option<Task>> {
        let next: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM tasks WHERE status = 'queued' ORDER BY created_at LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match next {
            Some(id) => Ok(Some(self.set_task_state(parse_uuid(&id)?, TaskStatus::Working, None)?)),
            None => Ok(None),
        }
    }

    /// Mark a task `ready` for review, recording the agent's summary.
    pub fn complete_task(&self, id: Uuid, result: Option<String>) -> Result<Task> {
        self.set_task_state(id, TaskStatus::Ready, Some(result))
    }

    /// Mark a task `failed`, recording the error.
    pub fn fail_task(&self, id: Uuid, error: &str) -> Result<Task> {
        self.set_task_state(id, TaskStatus::Failed, Some(Some(error.to_string())))
    }

    /// Mark a task `done` (the user accepted the staged edit).
    pub fn resolve_task(&self, id: Uuid) -> Result<Task> {
        self.set_task_state(id, TaskStatus::Done, None)
    }

    pub fn remove_task(&self, id: Uuid) -> Result<()> {
        let affected = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id.to_string()])?;
        if affected == 0 {
            return Err(Error::TaskNotFound(id));
        }
        Ok(())
    }

    /// Transition a task. `result == None` leaves the stored result untouched;
    /// `Some(value)` overwrites it (with `value` itself possibly `None`).
    fn set_task_state(&self, id: Uuid, status: TaskStatus, result: Option<Option<String>>) -> Result<Task> {
        let mut task = self.require_task(id)?;
        task.status = status;
        if let Some(value) = result {
            task.result = value;
        }
        task.updated_at = Utc::now();
        self.upsert_task(&task)?;
        Ok(task)
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
                    image: false,
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
                image: false,
            }],
            imported_at: Utc::now(),
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
                },
                crate::model::TranscriptSegment {
                    start: 5.5,
                    end: 12.5,
                    text: "Today we are talking about non-destructive editing.".to_string(),
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
                bpm: 120.0,
                beats: vec![0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0],
                confidence: 0.62,
            }),
            audio_class: Some(crate::model::AudioClassification {
                class: crate::model::AudioClass::Speech,
                confidence: 0.71,
            }),
        })?;

        // A small starter timeline: an interview cut followed by some b-roll.
        self.cut_clip(interview.id, 0.0, 12.5)?;
        self.cut_clip(broll.id, 0.0, 8.0)?;
        self.extract_audio(interview.id)?;

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

    /// Replace a clip's transform keyframes (re-sorted by time). An empty list
    /// clears the animation, so the static transform is used again.
    pub fn set_keyframes(&self, clip_id: Uuid, mut keyframes: Vec<Keyframe>) -> Result<Clip> {
        for k in &keyframes {
            validate_keyframe(k)?;
        }
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.edit_timeline("Set keyframes", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].keyframes = keyframes;
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    /// Add a keyframe at `time` seconds from the clip's start (replacing any
    /// keyframe already at that time). Each `None` channel captures the clip's
    /// current sampled transform there, so a lone keyframe "pins" the present
    /// pose. Realized as animation at export when ≥1 keyframe exists.
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
        if opacity.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
            return Err(Error::InvalidArgument("opacity must be within 0.0..=1.0".to_string()));
        }
        self.edit_timeline("Add keyframe", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let clip = &mut timeline.tracks[ti].clips[ci];
            let mut kf = Keyframe::from_transform(time, &clip.transform_at(time));
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
            clip.keyframes.retain(|k| (k.time - time).abs() > 1e-6);
            clip.keyframes.push(kf);
            clip.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
            Ok(clip.clone())
        })
    }

    /// Remove all transform keyframes from a clip (back to the static transform).
    pub fn clear_keyframes(&self, clip_id: Uuid) -> Result<Clip> {
        self.edit_timeline("Clear keyframes", move |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].keyframes.clear();
            Ok(timeline.tracks[ti].clips[ci].clone())
        })
    }

    // ---- text overlays (titles / lower-thirds / captions) -----------------

    /// Add a text overlay drawn over the composited picture, returning it.
    pub fn add_overlay(&self, text: String, start: f64, end: f64) -> Result<TextOverlay> {
        if !start.is_finite() || !end.is_finite() || end <= start {
            return Err(Error::InvalidArgument("overlay end must be after start".to_string()));
        }
        let overlay = TextOverlay::new(text, start.max(0.0), end);
        self.edit_timeline("Add text overlay", move |timeline| {
            timeline.overlays.push(overlay.clone());
            Ok(overlay)
        })
    }

    /// Update mutable fields of a text overlay; each `None` leaves a field
    /// unchanged. Pass an empty `bg` to clear the box background, or an empty
    /// `font` to revert to the default font.
    #[allow(clippy::too_many_arguments)]
    pub fn update_overlay(
        &self,
        overlay_id: Uuid,
        text: Option<String>,
        start: Option<f64>,
        end: Option<f64>,
        pos_x: Option<f64>,
        pos_y: Option<f64>,
        size: Option<f64>,
        color: Option<String>,
        bg: Option<String>,
        font: Option<String>,
        bold: Option<bool>,
    ) -> Result<TextOverlay> {
        if size.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err(Error::InvalidArgument("size must be a finite value > 0".to_string()));
        }
        self.edit_timeline("Update text overlay", move |timeline| {
            let o = timeline
                .overlays
                .iter_mut()
                .find(|o| o.id == overlay_id)
                .ok_or(Error::OverlayNotFound(overlay_id))?;
            if let Some(v) = text {
                o.text = v;
            }
            if let Some(v) = start {
                o.start = v.max(0.0);
            }
            if let Some(v) = end {
                o.end = v;
            }
            if let Some(v) = pos_x {
                o.pos_x = v;
            }
            if let Some(v) = pos_y {
                o.pos_y = v;
            }
            if let Some(v) = size {
                o.size = v;
            }
            if let Some(v) = color {
                o.color = v;
            }
            if let Some(v) = bg {
                o.bg = if v.is_empty() { None } else { Some(v) };
            }
            if let Some(v) = font {
                o.font = if v.is_empty() { None } else { Some(v) };
            }
            if let Some(v) = bold {
                o.bold = v;
            }
            if o.end <= o.start {
                return Err(Error::InvalidArgument("overlay end must be after start".to_string()));
            }
            Ok(o.clone())
        })
    }

    /// Remove a text overlay.
    pub fn remove_overlay(&self, overlay_id: Uuid) -> Result<()> {
        self.edit_timeline("Remove text overlay", move |timeline| {
            let before = timeline.overlays.len();
            timeline.overlays.retain(|o| o.id != overlay_id);
            if timeline.overlays.len() == before {
                return Err(Error::OverlayNotFound(overlay_id));
            }
            Ok(())
        })
    }

    /// Set (or clear, with an empty list) an overlay's position/opacity keyframes.
    pub fn set_overlay_keyframes(&self, overlay_id: Uuid, mut keyframes: Vec<TextKeyframe>) -> Result<TextOverlay> {
        for k in &keyframes {
            if !k.time.is_finite() || k.time < 0.0 {
                return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
            }
            if !(0.0..=1.0).contains(&k.opacity) {
                return Err(Error::InvalidArgument("opacity must be within 0.0..=1.0".to_string()));
            }
        }
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.edit_timeline("Set overlay keyframes", move |timeline| {
            let o = timeline
                .overlays
                .iter_mut()
                .find(|o| o.id == overlay_id)
                .ok_or(Error::OverlayNotFound(overlay_id))?;
            o.keyframes = keyframes;
            Ok(o.clone())
        })
    }

    /// Generate caption overlays from an asset's cached transcript — one per
    /// segment, low-center with a translucent box. The segments keep the
    /// transcript's own timestamps, so they line up when the asset sits at the
    /// start of the timeline at normal speed. Returns the overlays created.
    pub fn captions_from_transcript(&self, asset_id: Uuid) -> Result<Vec<TextOverlay>> {
        let analysis = self
            .get_analysis(asset_id)?
            .ok_or_else(|| Error::InvalidArgument("no analysis available for asset; run analysis first".to_string()))?;
        let overlays: Vec<TextOverlay> = analysis
            .transcript
            .iter()
            .filter(|s| !s.text.trim().is_empty() && s.end > s.start)
            .map(|s| {
                let mut o = TextOverlay::new(s.text.trim().to_string(), s.start.max(0.0), s.end);
                o.pos_y = 0.88;
                o.size = 0.05;
                o.bg = Some("black@0.5".to_string());
                o
            })
            .collect();
        if overlays.is_empty() {
            return Err(Error::InvalidArgument("asset has no usable transcript".to_string()));
        }
        let created = overlays.clone();
        self.edit_timeline("Add captions from transcript", move |timeline| {
            timeline.overlays.extend(overlays);
            Ok(())
        })?;
        Ok(created)
    }

    /// Render an asset's cached transcript as a SubRip (`.srt`) document.
    pub fn transcript_srt(&self, asset_id: Uuid) -> Result<String> {
        let analysis = self
            .get_analysis(asset_id)?
            .ok_or_else(|| Error::InvalidArgument("no analysis available for asset; run analysis first".to_string()))?;
        if analysis.transcript.is_empty() {
            return Err(Error::InvalidArgument("asset has no transcript".to_string()));
        }
        Ok(crate::model::transcript_to_srt(&analysis.transcript))
    }
}

fn validate_video_effect(e: &VideoEffect) -> Result<()> {
    match e {
        VideoEffect::Blur { sigma } => {
            if !sigma.is_finite() || *sigma < 0.0 {
                return Err(Error::InvalidArgument("blur sigma must be a finite value >= 0".to_string()));
            }
        }
        VideoEffect::Sharpen { amount } => {
            if !amount.is_finite() {
                return Err(Error::InvalidArgument("sharpen amount must be finite".to_string()));
            }
        }
        VideoEffect::ChromaKey { similarity, blend, .. } => {
            if !(0.0..=1.0).contains(similarity) || !(0.0..=1.0).contains(blend) {
                return Err(Error::InvalidArgument(
                    "chroma key similarity / blend must be within 0.0..=1.0".to_string(),
                ));
            }
        }
        VideoEffect::Grayscale | VideoEffect::Invert | VideoEffect::Vignette => {}
    }
    Ok(())
}

fn validate_audio_effect(e: &AudioEffect) -> Result<()> {
    let positive = |v: f64, name: &str| {
        if v.is_finite() && v > 0.0 {
            Ok(())
        } else {
            Err(Error::InvalidArgument(format!("{name} must be a finite value > 0")))
        }
    };
    match e {
        AudioEffect::Highpass { hz } | AudioEffect::Lowpass { hz } => positive(*hz, "frequency")?,
        AudioEffect::Equalizer { hz, width, gain_db } => {
            positive(*hz, "frequency")?;
            positive(*width, "width")?;
            if !gain_db.is_finite() {
                return Err(Error::InvalidArgument("gain_db must be finite".to_string()));
            }
        }
        AudioEffect::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        } => {
            if !threshold_db.is_finite() || !makeup_db.is_finite() {
                return Err(Error::InvalidArgument("compressor dB values must be finite".to_string()));
            }
            if !ratio.is_finite() || *ratio < 1.0 {
                return Err(Error::InvalidArgument("compressor ratio must be >= 1".to_string()));
            }
            positive(*attack_ms, "attack_ms")?;
            positive(*release_ms, "release_ms")?;
        }
        AudioEffect::Gate { threshold_db } => {
            if !threshold_db.is_finite() {
                return Err(Error::InvalidArgument("gate threshold_db must be finite".to_string()));
            }
        }
    }
    Ok(())
}

fn validate_keyframe(k: &Keyframe) -> Result<()> {
    if !k.time.is_finite() || k.time < 0.0 {
        return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
    }
    if !k.scale.is_finite() || k.scale <= 0.0 {
        return Err(Error::InvalidArgument(
            "keyframe scale must be a finite value > 0".to_string(),
        ));
    }
    if !(0.0..=1.0).contains(&k.opacity) {
        return Err(Error::InvalidArgument(
            "keyframe opacity must be within 0.0..=1.0".to_string(),
        ));
    }
    if ![k.pos_x, k.pos_y, k.rotation].iter().all(|v| v.is_finite()) {
        return Err(Error::InvalidArgument("keyframe values must be finite".to_string()));
    }
    Ok(())
}

fn row_to_task(
    id: String,
    prompt: String,
    status: String,
    result: Option<String>,
    created_at: String,
    updated_at: String,
) -> Result<Task> {
    Ok(Task {
        id: parse_uuid(&id)?,
        prompt,
        status: TaskStatus::parse(&status).ok_or_else(|| Error::Other(format!("invalid task status {status}")))?,
        result,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

fn row_to_asset(id: String, path: String, name: String, duration: f64, streams: String, imported_at: String) -> Result<Asset> {
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

fn parse_source(s: &str) -> EditSource {
    match s {
        "agent" => EditSource::Agent,
        "system" => EditSource::System,
        _ => EditSource::User,
    }
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

    fn asset_with(path: &str, streams: Vec<StreamInfo>) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            path: path.into(),
            name: "x".into(),
            duration: 10.0,
            streams,
            imported_at: Utc::now(),
        }
    }

    fn vid_stream(image: bool) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: if image { "png".into() } else { "h264".into() },
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            sample_rate: None,
            channels: None,
            image,
        }
    }

    fn aud_stream() -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Audio,
            codec: "aac".into(),
            width: None,
            height: None,
            fps: None,
            sample_rate: Some(48_000),
            channels: Some(2),
            image: false,
        }
    }

    #[test]
    fn preview_source_falls_back_to_original_without_a_proxy() {
        // A video asset with no generated proxy decodes from the original, so a
        // preview never breaks or blocks on a proxy that hasn't landed yet.
        let asset = asset_with("/no-such-kerf-source.mp4", vec![vid_stream(false)]);
        assert_eq!(Project::preview_source(&asset), PathBuf::from(&asset.path));
    }

    #[test]
    fn preview_source_skips_proxy_for_stills_and_audio_only() {
        let image = asset_with("/still.png", vec![vid_stream(true)]);
        let audio = asset_with("/voice.wav", vec![aud_stream()]);
        assert_eq!(Project::preview_source(&image), PathBuf::from(&image.path));
        assert_eq!(Project::preview_source(&audio), PathBuf::from(&audio.path));
    }

    #[test]
    fn preview_source_uses_proxy_once_one_exists() {
        // A unique per-process source path keeps the deterministic proxy path
        // distinct across concurrent test runs (no shared-file race).
        let path = format!("/kerf-test-proxy-source-{}.mp4", std::process::id());
        let asset = asset_with(&path, vec![vid_stream(false)]);
        let Some(proxy) = crate::engine::proxy_path(Path::new(&asset.path)) else {
            return; // no cache dir on this platform — nothing to resolve to
        };
        if let Some(dir) = proxy.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&proxy, b"stub").unwrap();
        let resolved = Project::preview_source(&asset);
        let _ = std::fs::remove_file(&proxy);
        assert_eq!(resolved, proxy);
    }

    #[test]
    fn trim_with_timeline_start_keeps_the_right_edge_put() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();

        // 4s clip at t=2 (source 3..7); a left-edge trim tightens the source
        // in-point and moves the start in one edit, so the end stays at t=6.
        let clip = project.add_clip_to_timeline(asset.id, None, 3.0, 7.0, Some(2.0)).unwrap();
        let trimmed = project.trim(clip.id, Some(4.0), None, Some(3.0)).unwrap();
        assert!((trimmed.timeline_start - 3.0).abs() < 1e-9);
        assert!((trimmed.timeline_end() - 6.0).abs() < 1e-9);

        let history = project.history().unwrap();
        assert_eq!(history.last().unwrap().label, "Trim clip");
    }

    #[test]
    fn cut_clip_range_splits_and_ripples() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();
        // Two 10s clips back to back; cut source 4..6 out of the first.
        let a = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        let b = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        let pieces = project.cut_clip_range(a.id, 4.0, 6.0).unwrap();
        assert_eq!(pieces.len(), 2);
        assert!((pieces[0].source_out - 4.0).abs() < 1e-9);
        assert!((pieces[1].source_in - 6.0).abs() < 1e-9);
        assert!((pieces[1].timeline_start - 4.0).abs() < 1e-9);
        // The following clip rippled left by the removed 2 seconds.
        let timeline = project.timeline().unwrap();
        let moved = timeline.clip(b.id).unwrap();
        assert!((moved.timeline_start - 8.0).abs() < 1e-9, "{}", moved.timeline_start);
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
                image: false,
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

    #[test]
    fn text_overlay_add_update_remove_roundtrip() {
        let project = Project::open_in_memory().unwrap();
        let o = project.add_overlay("Hello".into(), 1.0, 4.0).unwrap();
        assert_eq!(project.timeline().unwrap().overlays.len(), 1);
        let updated = project
            .update_overlay(
                o.id,
                Some("Hi".into()),
                None,
                Some(5.0),
                None,
                None,
                None,
                None,
                Some("black@0.5".into()),
                Some("Arial".into()),
                Some(true),
            )
            .unwrap();
        assert_eq!(updated.text, "Hi");
        assert!((updated.end - 5.0).abs() < 1e-9);
        assert_eq!(updated.bg.as_deref(), Some("black@0.5"));
        assert_eq!(updated.font.as_deref(), Some("Arial"));
        assert!(updated.bold);
        // An empty bg / font string clears it back to the default.
        let cleared = project
            .update_overlay(
                o.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(String::new()),
                Some(String::new()),
                None,
            )
            .unwrap();
        assert!(cleared.bg.is_none());
        assert!(cleared.font.is_none());
        project.remove_overlay(o.id).unwrap();
        assert!(project.timeline().unwrap().overlays.is_empty());
        assert!(project.remove_overlay(o.id).is_err());
    }

    #[test]
    fn keyframes_add_pins_pose_and_clear_resets() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();
        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        // Pin the current (static) scale at t=0, then animate to 1.5 at t=4.
        project
            .set_transform(clip.id, Some(1.2), None, None, None, None, None, None, None, None)
            .unwrap();
        let pinned = project.add_keyframe(clip.id, 0.0, None, None, None, None, None).unwrap();
        assert_eq!(pinned.keyframes.len(), 1);
        assert!((pinned.keyframes[0].scale - 1.2).abs() < 1e-9); // captured the static pose
        let animated = project.add_keyframe(clip.id, 4.0, Some(1.5), None, None, None, None).unwrap();
        assert_eq!(animated.keyframes.len(), 2);
        assert!(animated.is_animated());
        // Re-adding at the same time replaces (no duplicate).
        let replaced = project.add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, None).unwrap();
        assert_eq!(replaced.keyframes.len(), 2);
        assert!(!project.clear_keyframes(clip.id).unwrap().is_animated());
    }

    #[test]
    fn video_and_audio_effects_persist_and_validate() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/x.mp4", vec![vid_stream(false), aud_stream()]);
        project.insert_asset(&asset).unwrap();
        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        let updated = project
            .set_video_effects(clip.id, vec![VideoEffect::Blur { sigma: 5.0 }, VideoEffect::Grayscale])
            .unwrap();
        assert_eq!(updated.effects.len(), 2);
        // An out-of-range chroma key is rejected.
        assert!(project
            .set_video_effects(
                clip.id,
                vec![VideoEffect::ChromaKey {
                    color: "green".into(),
                    similarity: 2.0,
                    blend: 0.0
                }]
            )
            .is_err());
        let a = project
            .set_audio_effects(clip.id, vec![AudioEffect::Highpass { hz: 80.0 }])
            .unwrap();
        assert_eq!(a.audio.len(), 1);
        assert!(project
            .set_audio_effects(clip.id, vec![AudioEffect::Highpass { hz: -1.0 }])
            .is_err());
    }

    #[test]
    fn split_maps_the_timeline_point_through_speed() {
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
                image: false,
            }],
            imported_at: Utc::now(),
        };
        project.insert_asset(&asset).unwrap();

        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        project.set_speed(clip.id, 2.0).unwrap(); // 10s of source over 5s of timeline
                                                  // Split at timeline t=2.0 → 4.0s into the source (2.0 * 2x).
        let (left, right) = project.split_at(clip.id, 2.0).unwrap();
        assert!((left.source_out - 4.0).abs() < 1e-9, "left out: {}", left.source_out);
        assert!((right.source_in - 4.0).abs() < 1e-9, "right in: {}", right.source_in);
        assert!((left.duration() - 2.0).abs() < 1e-9, "left dur: {}", left.duration());
        assert!((right.duration() - 3.0).abs() < 1e-9, "right dur: {}", right.duration());
        // Gapless: the two halves still sum to the original timeline duration.
        assert!((left.duration() + right.duration() - 5.0).abs() < 1e-9);
        assert!((right.timeline_start - 2.0).abs() < 1e-9);
        assert_eq!(left.speed, 2.0);
        assert_eq!(right.speed, 2.0);
    }

    #[test]
    fn set_fade_persists_and_validates() {
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
                image: false,
            }],
            imported_at: Utc::now(),
        };
        project.insert_asset(&asset).unwrap();
        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        assert_eq!(clip.fade_in, 0.0);

        // Setting only fade_in leaves fade_out untouched.
        let faded = project.set_fade(clip.id, Some(0.5), None).unwrap();
        assert_eq!(faded.fade_in, 0.5);
        assert_eq!(faded.fade_out, 0.0);

        let faded = project.set_fade(clip.id, None, Some(1.0)).unwrap();
        assert_eq!(faded.fade_in, 0.5);
        assert_eq!(faded.fade_out, 1.0);

        // It persists to the stored timeline.
        let stored = project.timeline().unwrap().clip(clip.id).unwrap().clone();
        assert_eq!(stored.fade_in, 0.5);
        assert_eq!(stored.fade_out, 1.0);

        // Negative fades are rejected.
        assert!(matches!(
            project.set_fade(clip.id, Some(-1.0), None),
            Err(Error::InvalidArgument(_))
        ));
    }

    fn project_with_video_asset() -> (Project, Uuid) {
        let project = Project::open_in_memory().unwrap();
        let asset = Asset {
            id: Uuid::new_v4(),
            path: "/x.mp4".into(),
            name: "x.mp4".into(),
            duration: 60.0,
            streams: vec![StreamInfo {
                index: 0,
                kind: StreamKind::Video,
                codec: "h264".into(),
                width: Some(1920),
                height: Some(1080),
                fps: Some(30.0),
                sample_rate: None,
                channels: None,
                image: false,
            }],
            imported_at: Utc::now(),
        };
        let id = asset.id;
        project.insert_asset(&asset).unwrap();
        (project, id)
    }

    #[test]
    fn move_clip_repositions_and_rejects_overlap() {
        let (project, asset) = project_with_video_asset();
        let a = project.cut_clip(asset, 0.0, 5.0).unwrap(); // [0,5)
        let b = project.cut_clip(asset, 0.0, 5.0).unwrap(); // appended [5,10)

        // Free move into the open space well after b.
        let moved = project.move_clip(a.id, 20.0, None).unwrap();
        assert!((moved.timeline_start - 20.0).abs() < 1e-9);
        // The track is re-sorted by start (b first now).
        let tl = project.timeline().unwrap();
        let starts: Vec<f64> = tl.tracks[0].clips.iter().map(|c| c.timeline_start).collect();
        assert_eq!(starts, vec![5.0, 20.0]);

        // Dropping a back on top of b overlaps -> rejected.
        assert!(matches!(project.move_clip(a.id, 6.0, None), Err(Error::InvalidArgument(_))));
        assert_eq!(b.timeline_start, 5.0);

        // A negative start clamps to 0.
        let moved = project.move_clip(a.id, -3.0, None).unwrap();
        assert_eq!(moved.timeline_start, 0.0);
    }

    #[test]
    fn move_clip_across_tracks_same_kind_only() {
        let (project, asset) = project_with_video_asset();
        let clip = project.cut_clip(asset, 0.0, 5.0).unwrap();
        let v2 = project.add_track(StreamKind::Video, None).unwrap();
        let a1 = project.timeline().unwrap().first_track_of(StreamKind::Audio).unwrap();

        // Lift the clip onto the second video track (B-roll lane).
        project.move_clip(clip.id, 0.0, Some(v2.id)).unwrap();
        let tl = project.timeline().unwrap();
        assert!(tl.track(v2.id).unwrap().clips.iter().any(|c| c.id == clip.id));

        // Moving a video clip onto an audio track is rejected.
        assert!(matches!(
            project.move_clip(clip.id, 0.0, Some(a1)),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn ripple_delete_closes_the_gap() {
        let (project, asset) = project_with_video_asset();
        let a = project.cut_clip(asset, 0.0, 5.0).unwrap(); // [0,5)
        let b = project.cut_clip(asset, 0.0, 5.0).unwrap(); // [5,10)
        project.cut_clip(asset, 0.0, 5.0).unwrap(); // [10,15)

        project.ripple_delete(a.id).unwrap();
        let tl = project.timeline().unwrap();
        let starts: Vec<f64> = tl.tracks[0].clips.iter().map(|c| c.timeline_start).collect();
        // b and the third clip each shift left by 5s, closing the gap.
        assert_eq!(starts, vec![0.0, 5.0]);
        assert!(tl.clip(b.id).is_some());
    }

    #[test]
    fn add_and_remove_track() {
        let (project, _asset) = project_with_video_asset();
        let before = project.timeline().unwrap().tracks.len(); // V1 + A1

        let v2 = project.add_track(StreamKind::Video, None).unwrap();
        assert_eq!(v2.name, "V2");
        let tl = project.timeline().unwrap();
        assert_eq!(tl.tracks.len(), before + 1);
        // Video tracks stay grouped above audio tracks.
        let kinds: Vec<StreamKind> = tl.tracks.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![StreamKind::Video, StreamKind::Video, StreamKind::Audio]);

        project.remove_track(v2.id).unwrap();
        assert_eq!(project.timeline().unwrap().tracks.len(), before);
    }

    #[test]
    fn history_undo_redo_revert() {
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
                image: false,
            }],
            imported_at: Utc::now(),
        };
        project.insert_asset(&asset).unwrap();

        let clipped = |p: &Project| -> usize { p.timeline().unwrap().tracks.iter().map(|t| t.clips.len()).sum() };

        // Baseline (seq 0) is the only revision; nothing to undo yet.
        assert!(!project.can_undo().unwrap());
        assert_eq!(project.history().unwrap().len(), 1);

        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
        project.split_at(clip.id, 4.0).unwrap();
        assert_eq!(clipped(&project), 2);
        assert_eq!(project.history().unwrap().len(), 3); // baseline + add + split

        // Undo the split, then the add.
        project.undo().unwrap();
        assert_eq!(clipped(&project), 1);
        assert!(project.can_redo().unwrap());

        // Redo the split back.
        project.redo().unwrap();
        assert_eq!(clipped(&project), 2);

        // Revert all the way to the empty baseline.
        project.revert_to(0).unwrap();
        assert_eq!(clipped(&project), 0);
        assert!(project.history().unwrap().iter().find(|r| r.seq == 0).unwrap().current);

        // A new edit from a non-tip head truncates the redo branch.
        project.cut_clip(asset.id, 0.0, 5.0).unwrap();
        assert_eq!(clipped(&project), 1);
        assert_eq!(project.history().unwrap().len(), 2); // baseline + the new edit
        assert!(!project.can_redo().unwrap());
    }

    #[test]
    fn edits_are_attributed_to_actor() {
        let mut project = Project::open_in_memory().unwrap();
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
                image: false,
            }],
            imported_at: Utc::now(),
        };
        project.insert_asset(&asset).unwrap();

        project.set_actor(crate::model::EditSource::Agent);
        project.cut_clip(asset.id, 0.0, 5.0).unwrap();
        let latest = project.history().unwrap().pop().unwrap();
        assert_eq!(latest.source, crate::model::EditSource::Agent);
    }

    #[test]
    fn task_queue_lifecycle() {
        let project = Project::open_in_memory().unwrap();
        assert!(project.list_tasks().unwrap().is_empty());

        let queued = project.add_task("trim the intro").unwrap();
        assert_eq!(queued.status, TaskStatus::Queued);

        let claimed = project.claim_next_task().unwrap().unwrap();
        assert_eq!(claimed.id, queued.id);
        assert_eq!(claimed.status, TaskStatus::Working);
        // The queue is now empty, so there is nothing left to claim.
        assert!(project.claim_next_task().unwrap().is_none());

        let ready = project.complete_task(queued.id, Some("done".to_string())).unwrap();
        assert_eq!(ready.status, TaskStatus::Ready);
        assert_eq!(ready.result.as_deref(), Some("done"));

        let resolved = project.resolve_task(queued.id).unwrap();
        assert_eq!(resolved.status, TaskStatus::Done);
        // resolve leaves the agent's summary intact.
        assert_eq!(resolved.result.as_deref(), Some("done"));

        project.remove_task(queued.id).unwrap();
        assert!(project.list_tasks().unwrap().is_empty());
        assert!(matches!(project.require_task(queued.id), Err(Error::TaskNotFound(_))));
    }

    #[test]
    fn sample_project_seeds_tasks() {
        let project = Project::sample().unwrap();
        let tasks = project.list_tasks().unwrap();
        assert_eq!(tasks.len(), 3);
        assert!(tasks.iter().any(|t| t.status == TaskStatus::Done));
        assert!(tasks.iter().any(|t| t.status == TaskStatus::Queued));
    }

    #[test]
    fn save_as_snapshots_and_reopens_with_state() {
        let project = Project::sample().unwrap();
        assert!(project.path().is_none(), "in-memory project has no path");
        let assets = project.list_assets().unwrap().len();
        let tracks = project.timeline().unwrap().tracks.len();
        let tasks = project.list_tasks().unwrap().len();

        let path = std::env::temp_dir().join(format!("kerf-save-as-{}.kerf", Uuid::new_v4()));
        project.save_as(&path).unwrap();

        // Reopening the snapshot is file-backed and preserves the full state.
        let reopened = Project::open(&path).unwrap();
        assert_eq!(reopened.path(), Some(path.as_path()));
        assert_eq!(reopened.list_assets().unwrap().len(), assets);
        assert_eq!(reopened.timeline().unwrap().tracks.len(), tracks);
        assert_eq!(reopened.list_tasks().unwrap().len(), tasks);

        // save_as overwrites an existing file (the dialog confirms the overwrite).
        // Drop the open connection first: on Windows an open handle locks the file,
        // so the overwrite's remove_file would fail with "used by another process".
        drop(reopened);
        Project::sample().unwrap().save_as(&path).unwrap();

        std::fs::remove_file(&path).ok();
    }
}
