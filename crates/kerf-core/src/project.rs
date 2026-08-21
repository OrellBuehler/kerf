//! A `.kerf` project: a SQLite database holding imported assets, cached
//! analysis metadata, and the non-destructive timeline (EDL). All timeline
//! operations mutate the stored EDL; nothing is re-encoded until [`Project::export`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::engine::{self, ExportProgress};
use crate::error::{Error, Result};
use crate::model::default_beat_tolerance;
use crate::model::{
    Asset, AssetAnalysis, AudioEffect, Clip, Delivery, EditSource, Keyframe, Marker, Projection, Reframe, ReframeKeyframe,
    Revision, StagedEdit, StreamInfo, StreamKind, Task, TaskStatus, Tempo, TextKeyframe, TextOverlay, TimeRange, Timeline,
    TimelineDiff, Track, Transition, VideoEffect, MAX_FOV, MIN_FOV,
};

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS assets (
    id           TEXT PRIMARY KEY,
    path         TEXT NOT NULL,
    name         TEXT NOT NULL,
    duration     REAL NOT NULL,
    streams      TEXT NOT NULL,
    imported_at  TEXT NOT NULL,
    -- JSON array of the capture files a derived asset was built from (an
    -- Insta360 lens pair); NULL/absent for an ordinary asset. Older files get
    -- this column added by the migration in `init`.
    source_paths TEXT
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

-- At most one pending proposal (`id = 1`), holding the timeline the agent is
-- building, the one it branched from, and the labels of the edits it made.
CREATE TABLE IF NOT EXISTS staged (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    base_seq   INTEGER NOT NULL,
    base       TEXT NOT NULL,
    timeline   TEXT NOT NULL,
    edits      TEXT NOT NULL,
    task_id    TEXT,
    note       TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
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

        // `CREATE TABLE IF NOT EXISTS` leaves an existing table's columns alone,
        // so a column added after a `.kerf` file was written has to be migrated
        // onto it explicitly. Adding one is cheap and lossless; the guard is a
        // probe for the column rather than a schema version because that is the
        // whole of the migration story so far.
        let has_source_paths = self.conn.prepare("SELECT source_paths FROM assets LIMIT 1").is_ok();
        if !has_source_paths {
            self.conn.execute("ALTER TABLE assets ADD COLUMN source_paths TEXT", [])?;
        }

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

    /// Probe a media file and store its asset record, stitching an Insta360 lens
    /// pair into one 360 asset on the way (see [`Project::probe_import`]).
    pub fn import_asset(&self, media_path: impl AsRef<Path>) -> Result<Asset> {
        let asset = Self::probe_import(media_path.as_ref(), &mut |_| {})?;
        self.insert_or_get_asset(&asset)
    }

    /// Probe `path` into an importable [`Asset`], *without* `&self` like
    /// [`Project::probe_asset`] — and, when `path` turns out to be one lens of an
    /// Insta360 capture, stitch its pair into a single equirectangular video and
    /// describe that instead.
    ///
    /// An Insta360 capture is written as two files, one circular fisheye per
    /// lens, neither of which is 360 footage on its own: reframing one would show
    /// half the sphere. Stitching at import means the rest of Kerf — reframing,
    /// proxies, thumbnails, export — only ever sees ordinary equirect media, and
    /// the (slow, cached) re-encode happens once per capture instead of on every
    /// preview. `progress` reports that encode; it is never called for an
    /// ordinary file, which is probe-only and instant.
    pub fn probe_import(path: &Path, progress: &mut dyn FnMut(ExportProgress)) -> Result<Asset> {
        let asset = Self::probe_asset(path)?;
        let video = asset.streams.iter().find(|s| s.kind == StreamKind::Video);
        let Some((front, rear)) = video.and_then(|v| engine::insta360_pair(path, v.width, v.height)) else {
            return Ok(asset);
        };

        let stitched = engine::stitch_insta360(&front, &rear, asset.duration, progress)?;
        let mut stitched_asset = Self::probe_asset(&stitched)?;
        // The stitch is plain h264 with no spherical metadata of its own (the
        // ffmpeg CLI cannot write the `sv3d` box), so the projection we just
        // rendered it into is recorded here rather than re-detected.
        for stream in stitched_asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            stream.projection = Some(Projection::Equirect);
        }
        stitched_asset.name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(engine::insta360_pair_name)
            .unwrap_or(asset.name);
        stitched_asset.source_paths = vec![front.to_string_lossy().into_owned(), rear.to_string_lossy().into_owned()];
        Ok(stitched_asset)
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
            source_paths: Vec::new(),
        })
    }

    /// Insert (or replace) an asset record directly.
    pub fn insert_asset(&self, asset: &Asset) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO assets (id, path, name, duration, streams, imported_at, source_paths)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                asset.id.to_string(),
                asset.path,
                asset.name,
                asset.duration,
                serde_json::to_string(&asset.streams)?,
                asset.imported_at.to_rfc3339(),
                if asset.source_paths.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&asset.source_paths)?)
                },
            ],
        )?;
        Ok(())
    }

    /// Insert `asset`, unless one for the same file is already in the project —
    /// in which case that existing asset is returned untouched.
    ///
    /// Importing both halves of an Insta360 pair (or the same file twice) would
    /// otherwise land two asset rows for one piece of media: both imports stitch
    /// to the same cached file, so they arrive here with the same `path` and
    /// different fresh ids.
    pub fn insert_or_get_asset(&self, asset: &Asset) -> Result<Asset> {
        if let Some(existing) = self.asset_by_path(&asset.path)? {
            return Ok(existing);
        }
        self.insert_asset(asset)?;
        Ok(asset.clone())
    }

    /// The asset backed by `path`, if the project already has one.
    pub fn asset_by_path(&self, path: &str) -> Result<Option<Asset>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, name, duration, streams, imported_at, source_paths FROM assets WHERE path = ?1 LIMIT 1")?;
        let mut rows = stmt.query(params![path])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_asset(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            )?)),
            None => Ok(None),
        }
    }

    pub fn list_assets(&self) -> Result<Vec<Asset>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, name, duration, streams, imported_at, source_paths FROM assets ORDER BY imported_at")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut assets = Vec::new();
        for row in rows {
            let (id, path, name, duration, streams, imported_at, source_paths) = row?;
            assets.push(row_to_asset(id, path, name, duration, streams, imported_at, source_paths)?);
        }
        Ok(assets)
    }

    pub fn get_asset(&self, id: Uuid) -> Result<Option<Asset>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, path, name, duration, streams, imported_at, source_paths FROM assets WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((id, path, name, duration, streams, imported_at, source_paths)) => Ok(Some(row_to_asset(
                id,
                path,
                name,
                duration,
                streams,
                imported_at,
                source_paths,
            )?)),
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
    /// `effects` is the owning clip's audio chain, applied during the decode so
    /// the monitor hears its EQ / compressor / gate instead of the dry source.
    /// The chain runs *before* the Web Audio engine's volume and fade envelope,
    /// where the export runs it after the clip gain — a difference only a
    /// level-dependent effect (compressor, gate) can hear, and this is a preview
    /// monitor, not the export mix.
    pub fn decode_audio_pcm(
        asset: &Asset,
        start: f64,
        duration: f64,
        sample_rate: u32,
        effects: &[crate::model::AudioEffect],
    ) -> Result<Vec<u8>> {
        let filters = engine::audio_effects_filter(effects);
        engine::audio_pcm(Path::new(&asset.path), start, duration, sample_rate, filters.as_deref())
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
            if let Some(proxy) = engine::ready_proxy(Path::new(&asset.path), engine::proxy_width(asset.projection())) {
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
        Ok((self.working_timeline()?, self.preview_assets()?))
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
        // Hardware-accelerated decode like the single-frame path (with the same
        // learned software fallback inside the engine) — this runs continuously
        // while the user scrubs.
        let opts = engine::ExportOptions {
            hwaccel: engine::decode_hwaccel(),
            ..engine::ExportOptions::default()
        };
        engine::timeline_frame(timeline, assets, &opts, time_secs, max_width, quality)
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
        // While the agent has a staging session open its edits go to the
        // proposal instead, leaving the cut the user is looking at alone. The
        // GUI never stages, so a user edit always lands live — and makes any
        // open proposal `stale`.
        if self.actor == EditSource::Agent {
            if let Some(row) = self.staged_row()? {
                return self.edit_staged(row, label, f);
            }
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut timeline = self.timeline()?;
        let result = f(&mut timeline)?;
        let json = serde_json::to_string(&timeline)?;
        self.save_timeline_str(&json)?;
        self.record_revision(label, self.actor, &json)?;
        tx.commit()?;
        Ok(result)
    }

    // ---- staged edits -----------------------------------------------------

    /// The timeline the current actor is working on: the staged proposal while
    /// the agent has one open, otherwise the live timeline.
    ///
    /// Every read that an agent's next edit depends on goes through this, so the
    /// agent sees its own staged work — including the preview and export paths,
    /// which is the point: it can look at the cut it is proposing before handing
    /// it over. The GUI never stages, so it always sees the live timeline.
    pub fn working_timeline(&self) -> Result<Timeline> {
        if self.actor == EditSource::Agent {
            if let Some(timeline) = self.staged_timeline()? {
                return Ok(timeline);
            }
        }
        self.timeline()
    }

    fn staged_row(&self) -> Result<Option<StagedRow>> {
        self.conn
            .query_row(
                "SELECT base_seq, base, timeline, edits, task_id, note, created_at, updated_at FROM staged WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?
            .map(|(base_seq, base, timeline, edits, task_id, note, created_at, updated_at)| {
                Ok(StagedRow {
                    base_seq,
                    base,
                    timeline,
                    edits: serde_json::from_str(&edits)?,
                    task_id: task_id.as_deref().map(parse_uuid).transpose()?,
                    note,
                    created_at: parse_dt(&created_at)?,
                    updated_at: parse_dt(&updated_at)?,
                })
            })
            .transpose()
    }

    /// Apply an edit to the pending proposal rather than the live timeline,
    /// appending its label to the running list of what has been staged.
    fn edit_staged<R>(&self, row: StagedRow, label: &str, f: impl FnOnce(&mut Timeline) -> Result<R>) -> Result<R> {
        let mut timeline: Timeline = serde_json::from_str(&row.timeline)?;
        let result = f(&mut timeline)?;
        let mut edits = row.edits;
        edits.push(label.to_string());
        self.conn.execute(
            "UPDATE staged SET timeline = ?1, edits = ?2, updated_at = ?3 WHERE id = 1",
            params![
                serde_json::to_string(&timeline)?,
                serde_json::to_string(&edits)?,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(result)
    }

    /// Open a staging session branched from the live timeline: from here until
    /// [`Self::apply_staged`] or [`Self::discard_staged`], agent edits are held
    /// back for review instead of changing the cut the user sees.
    pub fn begin_staging(&self, task_id: Option<Uuid>, note: Option<&str>) -> Result<StagedEdit> {
        if self.staged_row()?.is_some() {
            return Err(Error::StagedEditPending);
        }
        let snapshot = serde_json::to_string(&self.timeline()?)?;
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO staged (id, base_seq, base, timeline, edits, task_id, note, created_at, updated_at)
             VALUES (1, ?1, ?2, ?2, '[]', ?3, ?4, ?5, ?5)",
            params![self.head()?, snapshot, task_id.map(|id| id.to_string()), note, now],
        )?;
        self.staged()?.ok_or(Error::NoStagedEdit)
    }

    /// The pending proposal — what it would change, and whether the live
    /// timeline has moved on underneath it — or `None` when nothing is staged.
    pub fn staged(&self) -> Result<Option<StagedEdit>> {
        let Some(row) = self.staged_row()? else {
            return Ok(None);
        };
        let base: Timeline = serde_json::from_str(&row.base)?;
        let proposed: Timeline = serde_json::from_str(&row.timeline)?;
        Ok(Some(StagedEdit {
            base_seq: row.base_seq,
            task_id: row.task_id,
            note: row.note,
            edits: row.edits,
            created_at: row.created_at,
            updated_at: row.updated_at,
            stale: self.head()? != row.base_seq,
            diff: base.diff(&proposed),
        }))
    }

    /// The staged timeline itself, for previewing the proposal.
    pub fn staged_timeline(&self) -> Result<Option<Timeline>> {
        match self.staged_row()? {
            Some(row) => Ok(Some(serde_json::from_str(&row.timeline)?)),
            None => Ok(None),
        }
    }

    /// Accept the proposal: it becomes the live timeline as a **single**
    /// revision, and the staging session closes.
    ///
    /// Refuses a `stale` proposal unless `force` — one branched from a cut the
    /// user has since edited would silently replace that newer work.
    pub fn apply_staged(&self, force: bool) -> Result<Timeline> {
        let row = self.staged_row()?.ok_or(Error::NoStagedEdit)?;
        if self.head()? != row.base_seq && !force {
            return Err(Error::StagedEditStale);
        }
        let base: Timeline = serde_json::from_str(&row.base)?;
        let proposed: Timeline = serde_json::from_str(&row.timeline)?;
        let diff = base.diff(&proposed);
        // A session that staged nothing (or staged and undid it) just closes —
        // recording a revision here would put an edit that changed nothing into
        // the user's history.
        if diff.is_empty() {
            self.conn.execute("DELETE FROM staged", [])?;
            return self.timeline();
        }
        let label = match (&row.note, row.edits.as_slice()) {
            (Some(note), _) => note.clone(),
            (None, [one]) => one.clone(),
            (None, edits) => format!("Agent edit ({} of {} changes)", diff.entries.len(), edits.len()),
        };
        let tx = self.conn.unchecked_transaction()?;
        self.save_timeline_str(&row.timeline)?;
        self.record_revision(&label, EditSource::Agent, &row.timeline)?;
        self.conn.execute("DELETE FROM staged", [])?;
        tx.commit()?;
        Ok(proposed)
    }

    /// Throw the proposal away, leaving the live timeline untouched.
    pub fn discard_staged(&self) -> Result<Timeline> {
        if self.conn.execute("DELETE FROM staged", [])? == 0 {
            return Err(Error::NoStagedEdit);
        }
        self.timeline()
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
        // Undo/redo/revert walk the *live* history. An agent holding staged
        // edits would be moving the ground under its own proposal, so make it
        // say which it means rather than guessing.
        if self.actor == EditSource::Agent && self.staged_row()?.is_some() {
            return Err(Error::InvalidArgument(
                "the timeline history is not available while edits are staged — apply or discard them first".to_string(),
            ));
        }
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

    fn revision_timeline(&self, seq: i64) -> Result<Timeline> {
        let snapshot: Option<String> = self
            .conn
            .query_row("SELECT snapshot FROM history WHERE seq = ?1", params![seq], |r| r.get(0))
            .optional()?;
        Ok(serde_json::from_str(&snapshot.ok_or(Error::RevisionNotFound(seq))?)?)
    }

    /// What changed between two stored revisions. Both snapshots are already
    /// kept for undo, so the edit log can explain itself rather than just
    /// listing operation names.
    pub fn diff_revisions(&self, from: i64, to: i64) -> Result<TimelineDiff> {
        Ok(self.revision_timeline(from)?.diff(&self.revision_timeline(to)?))
    }

    /// What one revision changed (`seq - 1` → `seq`). Revision 0 is the baseline
    /// and changed nothing.
    pub fn revision_diff(&self, seq: i64) -> Result<TimelineDiff> {
        let timeline = self.revision_timeline(seq)?;
        if seq <= 0 {
            return Ok(timeline.diff(&timeline));
        }
        self.diff_revisions(seq - 1, seq)
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
            let clip = Clip::for_asset(&asset, source_in, source_out, start);
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

    /// Insert `placements` — each a `(track_id, clip)` pair — so the earliest
    /// lands at `at`, preserving the relative offsets between them. Backs both
    /// paste (clips carried on a clipboard, whose sources may already be gone)
    /// and [`Self::duplicate_clips`].
    ///
    /// Everything about a clip comes along — trims, speed, transform, color,
    /// transition, effects, keyframes and reframe — which is exactly what
    /// `add_clip_to_timeline` cannot do, since that builds a fresh
    /// [`Clip::for_asset`]. Each clip is given a new id, so a clipboard can be
    /// pasted repeatedly.
    ///
    /// All-or-nothing: if any clip would overlap, the whole insert is rejected,
    /// so a partial paste can never land. Clips pasted alongside each other are
    /// checked against one another too, not just against what is already there.
    pub fn insert_clips(&self, placements: &[(Uuid, Clip)], at: f64) -> Result<Vec<Clip>> {
        if placements.is_empty() {
            return Err(Error::InvalidArgument("no clips to insert".to_string()));
        }
        let at = at.max(0.0);
        let base = placements.iter().map(|(_, c)| c.timeline_start).fold(f64::INFINITY, f64::min);

        self.edit_timeline("Insert clips", |timeline| {
            // Resolve every destination first, so an unknown track fails before
            // any edit lands.
            let mut staged: Vec<(usize, Clip)> = Vec::with_capacity(placements.len());
            for (track_id, clip) in placements {
                let ti = timeline
                    .tracks
                    .iter()
                    .position(|t| t.id == *track_id)
                    .ok_or(Error::TrackNotFound(*track_id))?;
                let mut copy = clip.clone();
                copy.id = Uuid::new_v4();
                copy.timeline_start = at + (clip.timeline_start - base);
                staged.push((ti, copy));
            }

            for (i, (ti, clip)) in staged.iter().enumerate() {
                let (start, end) = (clip.timeline_start, clip.timeline_end());
                let hits_existing = timeline.tracks[*ti]
                    .clips
                    .iter()
                    .any(|c| start < c.timeline_end() && c.timeline_start < end);
                let hits_sibling = staged
                    .iter()
                    .enumerate()
                    .any(|(j, (oti, o))| j != i && oti == ti && start < o.timeline_end() && o.timeline_start < end);
                if hits_existing || hits_sibling {
                    return Err(Error::InvalidArgument(
                        "pasted clips would overlap existing clips — move the playhead to free space".to_string(),
                    ));
                }
            }

            for (ti, clip) in &staged {
                timeline.tracks[*ti].clips.push(clip.clone());
                timeline.tracks[*ti].sort_by_start();
            }
            Ok(staged.into_iter().map(|(_, c)| c).collect())
        })
    }

    /// Copy the clips named by `clip_ids` and insert the copies so the earliest
    /// lands at `at`, each staying on its source track. The by-id convenience
    /// over [`Self::insert_clips`], for duplicate and for agent use.
    pub fn duplicate_clips(&self, clip_ids: &[Uuid], at: f64) -> Result<Vec<Clip>> {
        let timeline = self.working_timeline()?;
        let placements = clip_ids
            .iter()
            .map(|id| {
                timeline
                    .locate(*id)
                    .map(|(ti, ci)| (timeline.tracks[ti].id, timeline.tracks[ti].clips[ci].clone()))
                    .ok_or(Error::ClipNotFound(*id))
            })
            .collect::<Result<Vec<_>>>()?;
        self.insert_clips(&placements, at)
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
            let track = Track::new(kind, name);
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

    /// Set (or clear) the frame this project is cut for.
    ///
    /// The delivery frame decides the shape of every rendered picture — the
    /// scrubbed still, the streamed playback and the export — so a vertical cut
    /// is framed against the vertical frame instead of being cropped sight-unseen
    /// at render time. `None` restores the default: the shape follows the
    /// footage. An explicit `ExportOptions::resolution` still overrides it, so
    /// a one-off render at another size is unaffected.
    pub fn set_delivery_format(&self, format: Option<Delivery>) -> Result<Timeline> {
        let label = match format {
            Some(d) => format!("Deliver {}x{}", d.width, d.height),
            None => "Deliver at source shape".to_string(),
        };
        self.edit_timeline(&label, |timeline| {
            timeline.format = format.map(|d| Delivery::new(d.width, d.height, d.fit));
            Ok(())
        })?;
        self.working_timeline()
    }

    /// Mute or unmute a track: its clips stop rendering (silent for audio,
    /// hidden for video) while keeping their place on the timeline.
    pub fn set_track_muted(&self, track_id: Uuid, muted: bool) -> Result<Track> {
        self.edit_timeline(if muted { "Mute track" } else { "Unmute track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.muted = muted;
            Ok(track.clone())
        })
    }

    /// Solo or unsolo a track. While any track of a kind is soloed, the other
    /// tracks of that kind stop rendering; several may be soloed at once.
    pub fn set_track_solo(&self, track_id: Uuid, solo: bool) -> Result<Track> {
        self.edit_timeline(if solo { "Solo track" } else { "Unsolo track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.solo = solo;
            Ok(track.clone())
        })
    }

    /// Lock or unlock a track against editing. A locked track still renders;
    /// this only guards its clips from being moved, trimmed or split.
    pub fn set_track_locked(&self, track_id: Uuid, locked: bool) -> Result<Track> {
        self.edit_timeline(if locked { "Lock track" } else { "Unlock track" }, |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.locked = locked;
            Ok(track.clone())
        })
    }

    /// Enable or disable a single clip. A disabled clip keeps its position,
    /// trims, effects and keyframes but drops out of the render.
    pub fn set_clip_enabled(&self, clip_id: Uuid, enabled: bool) -> Result<Clip> {
        self.edit_timeline(if enabled { "Enable clip" } else { "Disable clip" }, |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].enabled = enabled;
            Ok(timeline.tracks[ti].clips[ci].clone())
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
        temperature: Option<f64>,
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
        if temperature.is_some_and(|v| !(-1.0..=1.0).contains(&v)) {
            return Err(Error::InvalidArgument("temperature must be within -1.0..=1.0".to_string()));
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
            if let Some(v) = temperature {
                c.temperature = v;
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
                let clip = Clip::for_asset(&asset, src_in, src_out, start);
                start += clip.duration();
                timeline.track_mut(tid).unwrap().clips.push(clip.clone());
                clips.push(clip);
            }
            Ok(clips)
        })
    }

    /// Ripple the cuts of a track onto the beat grid of the music on the audio
    /// tracks — "cut to the beat". Each clip is retrimmed so its outgoing cut
    /// lands on the nearest beat within `tolerance` seconds (default: half a
    /// beat, so every cut moves to the beat it is already closest to) and the
    /// rest of the track follows, preserving gaps. `track_id` picks one track;
    /// `None` aligns every unlocked video track. Returns how many cuts moved.
    ///
    /// Needs the music asset analyzed — the grid comes from the cached
    /// [`Tempo`], the same one the timeline ruler draws its beat ticks from.
    pub fn snap_to_beats(&self, track_id: Option<Uuid>, tolerance: Option<f64>) -> Result<usize> {
        let mut limits = HashMap::new();
        let mut tempos: HashMap<Uuid, Tempo> = HashMap::new();
        for asset in self.list_assets()? {
            // A still loops, so it stretches to whatever the beat asks for.
            let limit = if asset.is_image() { f64::INFINITY } else { asset.duration };
            limits.insert(asset.id, limit);
            if let Some(tempo) = self.get_analysis(asset.id)?.and_then(|a| a.tempo) {
                tempos.insert(asset.id, tempo);
            }
        }
        self.edit_timeline("Cut to the beat", |timeline| {
            let beats = timeline.beat_grid(&tempos);
            if beats.len() < 2 {
                return Err(Error::InvalidArgument(
                    "no beat grid — put rhythmic audio on an audio track and analyze it first".to_string(),
                ));
            }
            let tolerance = match tolerance {
                Some(value) if value > 0.0 => value,
                Some(_) => return Err(Error::InvalidArgument("tolerance must be positive".to_string())),
                None => default_beat_tolerance(&beats),
            };
            let targets: Vec<Uuid> = match track_id {
                Some(id) => {
                    timeline.track(id).ok_or(Error::TrackNotFound(id))?;
                    vec![id]
                }
                None => timeline
                    .tracks
                    .iter()
                    .filter(|t| t.kind == StreamKind::Video && !t.locked)
                    .map(|t| t.id)
                    .collect(),
            };
            let mut aligned = 0;
            for id in targets {
                let track = timeline.track_mut(id).ok_or(Error::TrackNotFound(id))?;
                aligned += track.align_cuts_to_beats(&beats, tolerance, &limits);
            }
            Ok(aligned)
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
            let clip = Clip::for_asset(&asset, 0.0, asset.duration, start);
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
            plan.push((asset.primary_kind(), asset));
        }
        self.edit_timeline("Concatenate", |timeline| {
            let mut clips = Vec::with_capacity(plan.len());
            for (primary, asset) in &plan {
                let tid = timeline
                    .first_track_of(*primary)
                    .ok_or_else(|| Error::Other("no suitable track for asset".to_string()))?;
                let start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
                let clip = Clip::for_asset(asset, 0.0, asset.duration, start);
                timeline.track_mut(tid).unwrap().clips.push(clip.clone());
                clips.push(clip);
            }
            Ok(clips)
        })
    }

    /// Render the timeline to `output_path`. Requires the `ffmpeg` feature.
    pub fn export(&self, output_path: impl AsRef<Path>, format: &str) -> Result<PathBuf> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        let output = output_path.as_ref();
        engine::render(&timeline, &assets, output, format)?;
        Ok(output.to_path_buf())
    }

    /// Like [`export`] but with explicit [`engine::ExportOptions`].
    pub fn export_with(&self, output_path: impl AsRef<Path>, opts: &engine::ExportOptions) -> Result<PathBuf> {
        let timeline = self.working_timeline()?;
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
            Some(id) => {
                let id = parse_uuid(&id)?;
                // Claiming a task opens a staging session for it, so the work an
                // agent does on the user's behalf is a proposal by default and
                // never rewrites the open cut unasked.
                if self.staged_row()?.is_none() {
                    self.begin_staging(Some(id), None)?;
                }
                Ok(Some(self.set_task_state(id, TaskStatus::Working, None)?))
            }
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

    /// Mark a task `done` — the user accepted its work, so a proposal staged
    /// under it is applied in the same breath.
    pub fn resolve_task(&self, id: Uuid) -> Result<Task> {
        if self.staged_row()?.is_some_and(|r| r.task_id == Some(id)) {
            self.apply_staged(false)?;
        }
        self.set_task_state(id, TaskStatus::Done, None)
    }

    /// Drop a task, and with it any proposal staged under it — dismissing the
    /// task is how the user says no to the edit.
    pub fn remove_task(&self, id: Uuid) -> Result<()> {
        let affected = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id.to_string()])?;
        if affected == 0 {
            return Err(Error::TaskNotFound(id));
        }
        if self.staged_row()?.is_some_and(|r| r.task_id == Some(id)) {
            self.discard_staged()?;
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
                    projection: None,
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
                },
            ],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
        for stream in asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            stream.projection = projection;
        }
        self.insert_asset(&asset)?;
        Ok(asset)
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

    // ---- text overlays (titles / lower-thirds / captions) -----------------

    /// Add a text overlay drawn over the composited picture, returning it.
    /// Drop a named marker at `time`. Markers are kept sorted by time, so the
    /// UI and `next`/`previous` navigation never have to re-sort.
    pub fn add_marker(&self, time: f64, name: String, color: Option<String>) -> Result<Marker> {
        if !time.is_finite() || time < 0.0 {
            return Err(Error::InvalidArgument("marker time must be >= 0".to_string()));
        }
        let marker = Marker {
            id: Uuid::new_v4(),
            time,
            name,
            color,
        };
        self.edit_timeline("Add marker", move |timeline| {
            timeline.markers.push(marker.clone());
            timeline.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
            Ok(marker)
        })
    }

    /// Rename, recolor or move a marker; each `None` leaves that field alone.
    /// Pass an empty `color` to clear it back to the UI default.
    pub fn update_marker(
        &self,
        marker_id: Uuid,
        time: Option<f64>,
        name: Option<String>,
        color: Option<String>,
    ) -> Result<Marker> {
        if time.is_some_and(|t| !t.is_finite() || t < 0.0) {
            return Err(Error::InvalidArgument("marker time must be >= 0".to_string()));
        }
        self.edit_timeline("Update marker", |timeline| {
            let marker = timeline
                .markers
                .iter_mut()
                .find(|m| m.id == marker_id)
                .ok_or_else(|| Error::InvalidArgument(format!("no marker {marker_id}")))?;
            if let Some(t) = time {
                marker.time = t;
            }
            if let Some(n) = name {
                marker.name = n;
            }
            if let Some(c) = color {
                marker.color = if c.is_empty() { None } else { Some(c) };
            }
            let out = marker.clone();
            timeline.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
            Ok(out)
        })
    }

    /// Remove a marker.
    pub fn remove_marker(&self, marker_id: Uuid) -> Result<()> {
        self.edit_timeline("Remove marker", |timeline| {
            let before = timeline.markers.len();
            timeline.markers.retain(|m| m.id != marker_id);
            if timeline.markers.len() == before {
                return Err(Error::InvalidArgument(format!("no marker {marker_id}")));
            }
            Ok(())
        })
    }

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

/// Angles are wrapped or clamped downstream (see [`Reframe::sample`]), so only
/// non-finite values are rejected here — a caller may legitimately pass 540°.
fn validate_angle(name: &str, v: Option<f64>) -> Result<()> {
    match v {
        Some(v) if !v.is_finite() => Err(Error::InvalidArgument(format!("{name} must be a finite number of degrees"))),
        _ => Ok(()),
    }
}

fn validate_fov(v: Option<f64>) -> Result<()> {
    match v {
        Some(v) if !v.is_finite() || !(MIN_FOV..=MAX_FOV).contains(&v) => Err(Error::InvalidArgument(format!(
            "field of view must be within {MIN_FOV}..={MAX_FOV} degrees"
        ))),
        _ => Ok(()),
    }
}

fn validate_lens_fov(v: Option<f64>) -> Result<()> {
    match v {
        Some(v) if !v.is_finite() || !(1.0..=360.0).contains(&v) => Err(Error::InvalidArgument(
            "lens field of view must be within 1..=360 degrees".to_string(),
        )),
        _ => Ok(()),
    }
}

fn validate_reframe_keyframe(k: &ReframeKeyframe) -> Result<()> {
    if !k.time.is_finite() || k.time < 0.0 {
        return Err(Error::InvalidArgument("keyframe time must be >= 0".to_string()));
    }
    validate_angle("yaw", Some(k.yaw))?;
    validate_angle("pitch", Some(k.pitch))?;
    validate_angle("roll", Some(k.roll))?;
    validate_fov(Some(k.fov))
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

fn row_to_asset(
    id: String,
    path: String,
    name: String,
    duration: f64,
    streams: String,
    imported_at: String,
    source_paths: Option<String>,
) -> Result<Asset> {
    Ok(Asset {
        id: parse_uuid(&id)?,
        path,
        name,
        duration,
        streams: serde_json::from_str(&streams)?,
        imported_at: parse_dt(&imported_at)?,
        source_paths: match source_paths {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        },
    })
}

/// The `staged` row as stored: timelines still serialized, so an edit that only
/// touches the proposal never pays to deserialize the base.
struct StagedRow {
    base_seq: i64,
    base: String,
    timeline: String,
    edits: Vec<String>,
    task_id: Option<Uuid>,
    note: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
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
    use crate::model::{DiffKind, Fit};

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
    fn importing_the_same_media_twice_reuses_the_asset() {
        // Both halves of an Insta360 pair stitch to one cached file and arrive
        // with the same path — that must be one asset, not two.
        let project = Project::open_in_memory().unwrap();
        let first = project
            .insert_or_get_asset(&asset_with("/cache/stitched.mp4", vec![vid_stream(false)]))
            .unwrap();
        let second = project
            .insert_or_get_asset(&asset_with("/cache/stitched.mp4", vec![vid_stream(false)]))
            .unwrap();
        assert_eq!(first.id, second.id, "the second import resolves to the first asset");
        assert_eq!(project.list_assets().unwrap().len(), 1);
    }

    #[test]
    fn stitched_asset_provenance_survives_a_save_and_reopen() {
        let project = Project::open_in_memory().unwrap();
        let mut asset = asset_with("/cache/stitched.mp4", vec![vid_stream(false)]);
        asset.source_paths = vec!["/dcim/VID_1_2_00_3.mp4".into(), "/dcim/VID_1_2_10_3.mp4".into()];
        project.insert_asset(&asset).unwrap();

        let dir = std::env::temp_dir().join(format!("kerf-stitch-provenance-{}.kerf", Uuid::new_v4()));
        project.save_as(&dir).unwrap();
        let reopened = Project::open(&dir).unwrap();
        let loaded = reopened.require_asset(asset.id).unwrap();
        assert_eq!(loaded.source_paths, asset.source_paths);
        let _ = std::fs::remove_file(&dir);
    }

    fn asset_with(path: &str, streams: Vec<StreamInfo>) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            path: path.into(),
            name: "x".into(),
            duration: 10.0,
            streams,
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
            projection: None,
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
            projection: None,
        }
    }

    #[test]
    fn snap_to_beats_lands_the_video_cuts_on_the_music_grid() {
        let project = Project::open_in_memory().unwrap();
        let video = project
            .insert_or_get_asset(&asset_with("/beat-video.mp4", vec![vid_stream(false)]))
            .unwrap();
        let music = project
            .insert_or_get_asset(&asset_with("/beat-music.wav", vec![aud_stream()]))
            .unwrap();
        project
            .set_analysis(&AssetAnalysis {
                asset_id: music.id,
                tempo: Some(crate::model::Tempo {
                    bpm: 120.0,
                    beats: (0..=20).map(|i| i as f64 * 0.5).collect(),
                    confidence: 0.8,
                }),
                ..Default::default()
            })
            .unwrap();
        project.extract_audio(music.id).unwrap();
        project.cut_clip(video.id, 0.0, 1.1).unwrap();
        project.cut_clip(video.id, 2.0, 3.4).unwrap();

        let moved = project.snap_to_beats(None, None).unwrap();
        assert!(moved > 0, "cuts off the grid should have moved");

        let timeline = project.timeline().unwrap();
        let cuts: Vec<f64> = timeline
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| t.clips.iter().map(Clip::timeline_end))
            .collect();
        assert_eq!(cuts, vec![1.0, 2.5]);

        // The music track is the grid, not a target — it keeps its full length.
        let audio = timeline.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap();
        assert_eq!(audio.clips[0].duration(), 10.0);
    }

    #[test]
    fn snap_to_beats_without_a_grid_says_what_is_missing() {
        let project = Project::open_in_memory().unwrap();
        let video = project
            .insert_or_get_asset(&asset_with("/no-music.mp4", vec![vid_stream(false)]))
            .unwrap();
        project.cut_clip(video.id, 0.0, 1.1).unwrap();
        let err = project.snap_to_beats(None, None).unwrap_err().to_string();
        assert!(err.contains("no beat grid"), "got: {err}");
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
        let width = crate::engine::proxy_width(asset.projection());
        let Some(proxy) = crate::engine::proxy_path(Path::new(&asset.path), width) else {
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
    fn mute_solo_lock_and_clip_enable_persist_and_gate_the_render() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();
        let clip = project.cut_clip(asset.id, 0.0, 5.0).unwrap();
        let tl = project.timeline().unwrap();
        let vid = tl.tracks.iter().find(|t| t.kind == StreamKind::Video).unwrap().id;

        // Each flag round-trips through the JSON blob independently.
        assert!(project.set_track_muted(vid, true).unwrap().muted);
        assert!(project.set_track_solo(vid, true).unwrap().solo);
        assert!(project.set_track_locked(vid, true).unwrap().locked);
        let saved = project.timeline().unwrap();
        let t = saved.track(vid).unwrap();
        assert!(t.muted && t.solo && t.locked);

        // Muted wins over soloed, so nothing reaches the graph.
        assert!(saved.for_render().track(vid).unwrap().clips.is_empty());

        // Unmuting brings it back; locking is an editing guard, not a render gate.
        project.set_track_muted(vid, false).unwrap();
        let saved = project.timeline().unwrap();
        assert_eq!(saved.for_render().track(vid).unwrap().clips.len(), 1);

        // Disabling the clip drops it while leaving it on the timeline.
        assert!(!project.set_clip_enabled(clip.id, false).unwrap().enabled);
        let saved = project.timeline().unwrap();
        assert_eq!(saved.track(vid).unwrap().clips.len(), 1, "still on the timeline");
        assert!(saved.for_render().track(vid).unwrap().clips.is_empty(), "but not rendered");

        // Every one of those was a labelled, revertible edit.
        let labels: Vec<_> = project.history().unwrap().iter().map(|r| r.label.clone()).collect();
        for want in ["Mute track", "Solo track", "Lock track", "Unmute track", "Disable clip"] {
            assert!(labels.contains(&want.to_string()), "missing {want} in {labels:?}");
        }
    }

    #[test]
    fn markers_stay_sorted_and_round_trip() {
        let project = Project::open_in_memory().unwrap();
        // Added out of order; the store keeps them sorted so the UI never re-sorts.
        project.add_marker(9.0, "late".into(), None).unwrap();
        let mid = project.add_marker(4.0, "middle".into(), Some("#f00".into())).unwrap();
        project.add_marker(1.0, "early".into(), None).unwrap();
        let names: Vec<_> = project.timeline().unwrap().markers.iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, ["early", "middle", "late"]);

        // Moving one re-sorts, renaming sticks, and an empty color clears it.
        let moved = project
            .update_marker(mid.id, Some(12.0), Some("moved".into()), Some(String::new()))
            .unwrap();
        assert!(moved.color.is_none());
        let names: Vec<_> = project.timeline().unwrap().markers.iter().map(|m| m.name.clone()).collect();
        assert_eq!(names, ["early", "late", "moved"]);

        project.remove_marker(mid.id).unwrap();
        assert_eq!(project.timeline().unwrap().markers.len(), 2);
        assert!(project.remove_marker(mid.id).is_err(), "removing twice must fail");
        assert!(project.add_marker(-1.0, "bad".into(), None).is_err());
    }

    #[test]
    fn duplicate_clips_preserves_everything_and_relative_offsets() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();
        // Two clips, 2s apart, the first carrying non-default properties.
        let a = project.add_clip_to_timeline(asset.id, None, 0.0, 2.0, Some(0.0)).unwrap();
        let b = project.add_clip_to_timeline(asset.id, None, 5.0, 6.0, Some(4.0)).unwrap();
        project.set_volume(a.id, 0.25).unwrap();
        project.set_speed(a.id, 2.0).unwrap();
        project
            .set_video_effects(a.id, vec![VideoEffect::Blur { sigma: 3.0 }])
            .unwrap();

        let copies = project.duplicate_clips(&[a.id, b.id], 20.0).unwrap();
        assert_eq!(copies.len(), 2);
        // The earliest lands on `at`, and the gap between them survives.
        assert!((copies[0].timeline_start - 20.0).abs() < 1e-9);
        assert!((copies[1].timeline_start - 24.0).abs() < 1e-9);
        // Fresh identities, but everything else carried over — which is exactly
        // what add_clip_to_timeline cannot do.
        assert_ne!(copies[0].id, a.id);
        assert!((copies[0].volume - 0.25).abs() < 1e-6);
        assert!((copies[0].speed - 2.0).abs() < 1e-9);
        assert_eq!(copies[0].effects.len(), 1);
        assert_eq!(project.timeline().unwrap().tracks[0].clips.len(), 4);

        // Overlapping an existing clip is rejected outright, leaving nothing behind.
        let before = project.timeline().unwrap().tracks[0].clips.len();
        assert!(project.duplicate_clips(&[a.id], 20.5).is_err());
        assert_eq!(project.timeline().unwrap().tracks[0].clips.len(), before, "no partial paste");

        assert!(project.duplicate_clips(&[], 0.0).is_err());
        assert!(project.duplicate_clips(&[Uuid::new_v4()], 30.0).is_err());
    }

    /// The point of `insert_clips` taking values rather than ids: cut-then-paste,
    /// where the source clip no longer exists by the time the paste happens.
    #[test]
    fn insert_clips_pastes_clips_whose_sources_are_already_gone() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();
        let clip = project.add_clip_to_timeline(asset.id, None, 0.0, 3.0, Some(0.0)).unwrap();
        project.set_volume(clip.id, 0.5).unwrap();

        // Snapshot it the way a clipboard would, then cut it.
        let tl = project.timeline().unwrap();
        let track_id = tl.tracks[0].id;
        let snapshot = tl.clip(clip.id).unwrap().clone();
        project.remove(clip.id).unwrap();
        assert!(project.timeline().unwrap().tracks[0].clips.is_empty());

        // Pasting still works, and the copy is a new identity carrying the edits.
        let pasted = project.insert_clips(&[(track_id, snapshot)], 10.0).unwrap();
        assert_eq!(pasted.len(), 1);
        assert_ne!(pasted[0].id, clip.id);
        assert!((pasted[0].timeline_start - 10.0).abs() < 1e-9);
        assert!((pasted[0].volume - 0.5).abs() < 1e-6);

        // Pasting the same clipboard again is fine — each insert re-ids.
        let again = project.insert_clips(&[(track_id, pasted[0].clone())], 20.0).unwrap();
        assert_ne!(again[0].id, pasted[0].id);
        assert_eq!(project.timeline().unwrap().tracks[0].clips.len(), 2);

        assert!(project.insert_clips(&[(Uuid::new_v4(), pasted[0].clone())], 30.0).is_err());
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
                projection: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
    fn a_360_clip_reframes_by_default_and_a_flat_one_never_does() {
        let project = Project::open_in_memory().unwrap();
        let mut v = vid_stream(false);
        v.width = Some(5760);
        v.height = Some(2880);
        v.projection = Some(Projection::DualFisheye);
        let sphere = asset_with("/VID_001.insv", vec![v]);
        let flat = asset_with("/plain.mp4", vec![vid_stream(false)]);
        project.insert_asset(&sphere).unwrap();
        project.insert_asset(&flat).unwrap();

        // Landing a 360 clip on the timeline points a camera at it, so it
        // previews as ordinary footage rather than two fisheye circles.
        let clip = project.cut_clip(sphere.id, 0.0, 10.0).unwrap();
        let rf = clip.reframe.expect("a 360 clip reframes on arrival");
        assert_eq!(rf.input, Projection::DualFisheye);
        assert_eq!(rf.output, Projection::Flat);

        assert!(
            project.cut_clip(flat.id, 0.0, 10.0).unwrap().reframe.is_none(),
            "ordinary footage is never reprojected"
        );
    }

    #[test]
    fn marking_an_asset_360_makes_later_clips_reframe() {
        // Footage kerf can't identify (no spherical metadata, no telltale
        // geometry) probes flat; marking the asset is the escape hatch, and it
        // has to stick to the asset so every later cut picks it up.
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/mystery.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();
        assert!(project.cut_clip(asset.id, 0.0, 5.0).unwrap().reframe.is_none());

        let updated = project.set_asset_projection(asset.id, Some(Projection::Equirect)).unwrap();
        assert_eq!(updated.projection(), Some(Projection::Equirect));
        let clip = project.cut_clip(asset.id, 0.0, 5.0).unwrap();
        assert_eq!(clip.reframe.expect("marked asset reframes").input, Projection::Equirect);
        // Persisted on the asset, not just on the returned copy.
        assert_eq!(
            project.require_asset(asset.id).unwrap().projection(),
            Some(Projection::Equirect)
        );

        // And it can be taken back off again.
        project.set_asset_projection(asset.id, None).unwrap();
        assert_eq!(project.require_asset(asset.id).unwrap().projection(), None);
    }

    #[test]
    fn asset_projection_rejects_flat_and_audio_only() {
        let project = Project::open_in_memory().unwrap();
        let video = asset_with("/v.mp4", vec![vid_stream(false)]);
        let audio = asset_with("/a.wav", vec![aud_stream()]);
        project.insert_asset(&video).unwrap();
        project.insert_asset(&audio).unwrap();
        assert!(project.set_asset_projection(video.id, Some(Projection::Flat)).is_err());
        assert!(project.set_asset_projection(audio.id, Some(Projection::Equirect)).is_err());
    }

    #[test]
    fn reframe_ops_aim_the_camera_and_pin_its_pose() {
        let project = Project::open_in_memory().unwrap();
        let mut v = vid_stream(false);
        v.projection = Some(Projection::Equirect);
        let asset = asset_with("/360.mp4", vec![v]);
        project.insert_asset(&asset).unwrap();
        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();

        // `None` leaves a channel alone, so yaw can be nudged on its own.
        let aimed = project
            .set_reframe(clip.id, Some(85.0), Some(-10.0), None, None, None, None, None)
            .unwrap();
        let rf = aimed.reframe.as_ref().unwrap();
        assert_eq!((rf.yaw, rf.pitch, rf.fov), (85.0, -10.0, 100.0));

        // A fresh keyframe captures the pose that is already there.
        let pinned = project.add_reframe_keyframe(clip.id, 0.0, None, None, None, None).unwrap();
        let kfs = &pinned.reframe.as_ref().unwrap().keyframes;
        assert_eq!(kfs.len(), 1);
        assert_eq!((kfs[0].yaw, kfs[0].pitch), (85.0, -10.0));

        let panned = project
            .add_reframe_keyframe(clip.id, 4.0, Some(-85.0), None, None, None)
            .unwrap();
        assert!(panned.reframe.as_ref().unwrap().is_animated());
        // Re-adding at the same time replaces rather than duplicates.
        let replaced = project
            .add_reframe_keyframe(clip.id, 0.0, Some(0.0), None, None, None)
            .unwrap();
        assert_eq!(replaced.reframe.as_ref().unwrap().keyframes.len(), 2);

        // Out-of-range field of view is refused up front, since v360 would
        // reject it at render time.
        assert!(project
            .set_reframe(clip.id, None, None, None, Some(0.0), None, None, None)
            .is_err());
        assert!(project.clear_reframe(clip.id).unwrap().reframe.is_none());
    }

    #[test]
    fn reframing_flat_footage_needs_an_explicit_projection() {
        let project = Project::open_in_memory().unwrap();
        let asset = asset_with("/plain.mp4", vec![vid_stream(false)]);
        project.insert_asset(&asset).unwrap();
        let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();

        assert!(
            project
                .set_reframe(clip.id, Some(30.0), None, None, None, None, None, None)
                .is_err(),
            "detection can miss, but silently reprojecting flat video is worse"
        );
        // …and the escape hatch when detection did miss.
        let forced = project
            .set_reframe(clip.id, Some(30.0), None, None, None, None, Some(Projection::Equirect), None)
            .unwrap();
        assert_eq!(forced.reframe.unwrap().input, Projection::Equirect);
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
                projection: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
                projection: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
                projection: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
                projection: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
                projection: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
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
    fn the_delivery_format_persists_and_clears() {
        let project = Project::sample().unwrap();
        assert!(project.timeline().unwrap().format.is_none(), "projects start at source shape");

        let tl = project
            .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
            .unwrap();
        assert_eq!(tl.format, Some(Delivery::new(1080, 1920, Fit::Cover)));

        // The timeline is stored as one JSON blob, so a reload is the real test.
        let path = std::env::temp_dir().join(format!("kerf-delivery-{}.kerf", Uuid::new_v4()));
        project.save_as(&path).unwrap();
        let reopened = Project::open(&path).unwrap();
        assert_eq!(
            reopened.timeline().unwrap().format,
            Some(Delivery::new(1080, 1920, Fit::Cover))
        );
        drop(reopened);
        let _ = std::fs::remove_file(&path);

        // And clearing it goes back to following the footage.
        assert!(project.set_delivery_format(None).unwrap().format.is_none());
        // Both edits are in the log, so the frame change is undoable like any other.
        let history = project.history().unwrap();
        assert!(history.iter().any(|r| r.label == "Deliver 1080x1920"), "{history:?}");
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

    /// A sample project with the agent driving, which is the only actor whose
    /// edits stage.
    fn agent_project() -> Project {
        let mut project = Project::sample().unwrap();
        project.set_actor(EditSource::Agent);
        project
    }

    fn first_clip(project: &Project) -> Clip {
        project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .flat_map(|t| t.clips.clone())
            .next()
            .unwrap()
    }

    #[test]
    fn staged_agent_edits_leave_the_live_timeline_alone_until_applied() {
        let project = agent_project();
        let clip = first_clip(&project);
        let before = project.timeline().unwrap();
        let revisions_before = project.history().unwrap().len();

        project.begin_staging(None, None).unwrap();
        project.set_volume(clip.id, 0.4).unwrap();
        project.remove(clip.id).unwrap();

        // The cut the user is looking at has not moved.
        let live = project.timeline().unwrap();
        assert_eq!(live.duration(), before.duration());
        assert!(live.clip(clip.id).is_some());
        assert_eq!(project.history().unwrap().len(), revisions_before);
        // …but the agent sees its own work.
        assert!(project.working_timeline().unwrap().clip(clip.id).is_none());

        let staged = project.staged().unwrap().unwrap();
        assert!(!staged.stale);
        assert_eq!(staged.edits, vec!["Set volume".to_string(), "Remove clip".to_string()]);
        assert!(staged.diff.entries.iter().any(|e| e.kind == DiffKind::ClipRemoved));

        let applied = project.apply_staged(false).unwrap();
        assert!(applied.clip(clip.id).is_none());
        assert!(project.timeline().unwrap().clip(clip.id).is_none());
        assert!(project.staged().unwrap().is_none());

        // Two edits, one revision: the user accepted a proposal, not a replay.
        let history = project.history().unwrap();
        assert_eq!(history.len(), revisions_before + 1);
        let latest = history.last().unwrap();
        assert_eq!(latest.source, EditSource::Agent);
        assert!(latest.label.starts_with("Agent edit ("), "{}", latest.label);
    }

    #[test]
    fn discarding_a_proposal_leaves_no_trace() {
        let project = agent_project();
        let clip = first_clip(&project);
        let revisions_before = project.history().unwrap().len();

        project.begin_staging(None, Some("tighten the intro")).unwrap();
        project.remove(clip.id).unwrap();
        project.discard_staged().unwrap();

        assert!(project.staged().unwrap().is_none());
        assert!(project.timeline().unwrap().clip(clip.id).is_some());
        assert_eq!(project.history().unwrap().len(), revisions_before);
        assert!(matches!(project.discard_staged(), Err(Error::NoStagedEdit)));
    }

    #[test]
    fn a_user_edit_underneath_makes_the_proposal_stale() {
        let mut project = agent_project();
        let clips: Vec<Clip> = project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .flat_map(|t| t.clips.clone())
            .collect();

        project.begin_staging(None, None).unwrap();
        project.set_volume(clips[0].id, 0.2).unwrap();

        // The user keeps cutting while the agent works.
        project.set_actor(EditSource::User);
        project.set_volume(clips[1].id, 0.9).unwrap();
        project.set_actor(EditSource::Agent);

        let staged = project.staged().unwrap().unwrap();
        assert!(staged.stale, "the live timeline moved on since the proposal branched");
        assert!(matches!(project.apply_staged(false), Err(Error::StagedEditStale)));

        // Forcing it is the explicit "replace that newer cut" choice.
        let applied = project.apply_staged(true).unwrap();
        assert_eq!(applied.clip(clips[0].id).unwrap().volume, 0.2);
        assert_eq!(applied.clip(clips[1].id).unwrap().volume, 1.0);
    }

    #[test]
    fn staging_refuses_to_nest_and_history_refuses_to_move_under_it() {
        let project = agent_project();
        project.begin_staging(None, None).unwrap();
        assert!(matches!(project.begin_staging(None, None), Err(Error::StagedEditPending)));
        // Undo would walk the live history out from under the proposal.
        assert!(project.undo().is_err());
        project.discard_staged().unwrap();
        assert!(project.begin_staging(None, None).is_ok());
    }

    #[test]
    fn applying_a_proposal_that_changed_nothing_adds_no_revision() {
        let project = agent_project();
        let revisions_before = project.history().unwrap().len();
        project.begin_staging(None, None).unwrap();
        project.apply_staged(false).unwrap();
        assert_eq!(project.history().unwrap().len(), revisions_before);
        assert!(project.staged().unwrap().is_none());
    }

    #[test]
    fn claiming_a_task_stages_its_work_and_resolving_applies_it() {
        let mut project = Project::open_in_memory().unwrap();
        let task = project.add_task("tighten the intro").unwrap();

        project.set_actor(EditSource::Agent);
        let claimed = project.claim_next_task().unwrap().unwrap();
        let staged = project.staged().unwrap().unwrap();
        assert_eq!(staged.task_id, Some(claimed.id));

        project.add_track(StreamKind::Video, Some("B-roll".to_string())).unwrap();
        assert_eq!(project.timeline().unwrap().tracks.len(), 2, "live cut untouched");
        assert_eq!(project.working_timeline().unwrap().tracks.len(), 3);

        project
            .complete_task(task.id, Some("added a B-roll track".to_string()))
            .unwrap();
        // Accepting the task is accepting its edits.
        project.resolve_task(task.id).unwrap();
        assert_eq!(project.timeline().unwrap().tracks.len(), 3);
        assert!(project.staged().unwrap().is_none());
    }

    #[test]
    fn dismissing_a_task_throws_its_staged_edits_away() {
        let mut project = Project::open_in_memory().unwrap();
        let task = project.add_task("tighten the intro").unwrap();
        project.set_actor(EditSource::Agent);
        project.claim_next_task().unwrap().unwrap();
        project.add_track(StreamKind::Video, Some("B-roll".to_string())).unwrap();

        project.remove_task(task.id).unwrap();
        assert!(project.staged().unwrap().is_none());
        assert_eq!(project.timeline().unwrap().tracks.len(), 2);
    }

    #[test]
    fn a_revision_explains_what_it_changed() {
        let project = Project::sample().unwrap();
        let clip = first_clip(&project);
        project.set_volume(clip.id, 0.25).unwrap();

        let seq = project.history().unwrap().last().unwrap().seq;
        let diff = project.revision_diff(seq).unwrap();
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::ClipChanged);
        assert!(
            diff.entries[0].detail.as_deref().unwrap().contains("volume 100% → 25%"),
            "{:?}",
            diff.entries[0].detail
        );
        // The baseline revision changed nothing by definition.
        assert!(project.revision_diff(0).unwrap().is_empty());
    }
}
