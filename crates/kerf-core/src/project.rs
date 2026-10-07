//! A `.kerf` project: a SQLite database holding imported assets, cached
//! analysis metadata, and the non-destructive timeline (EDL). All timeline
//! operations mutate the stored EDL; nothing is re-encoded until [`Project::export`].

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::captions_import::{CaptionFile, CaptionImportRequest, ImportSummary, MAX_CAPTION_OFFSET, MAX_IMPORTED_CAPTIONS};
use crate::engine::{self, ExportProgress};
use crate::error::{Error, Result};
use crate::model::{default_beat_tolerance, fmt_time};
use crate::model::{
    Asset, AssetAnalysis, AudioEffect, CaptionOptions, CaptionStyle, CaptionTimeBase, Clip, ClipCut, ClipMove, CropFrame,
    Delivery, EditOutcome, EditSource, Framing, Keyframe, Marker, Mask, Projection, Reframe, ReframeKeyframe, Revision,
    SourceLimits, SplitSide, StagedEdit, StreamInfo, StreamKind, Task, TaskStatus, Tempo, TextKeyframe, TextOverlay, TimeRange,
    Timeline, TimelineDiff, Track, TranscriptSegment, Transition, VideoEffect, Voiceover, MAX_FOV, MIN_FOV,
};
use crate::model::{Detached, DetachedMany};

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
    source_paths TEXT,
    -- JSON `Voiceover` for audio Kerf synthesized from a script; NULL for
    -- media the user brought. Migrated onto older files like `source_paths`.
    voiceover    TEXT
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

/// The audio track [`Project::place_voiceover`] puts narration on by default.
pub const VOICEOVER_TRACK: &str = "VO";

/// An asset name for a voiceover: its script's opening words.
fn voiceover_name(text: &str) -> String {
    const WORDS: usize = 6;
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut name = words.iter().take(WORDS).copied().collect::<Vec<_>>().join(" ");
    if words.len() > WORDS {
        name.push('…');
    }
    format!("Voiceover: {name}")
}

/// `meta` key holding the seq of the currently-applied revision.
const HISTORY_HEAD: &str = "history_head";

/// `meta` key holding whether editing ripples (`"true"` / `"false"`; absent
/// reads as off). A property of the project, like the speech model — not part of
/// the timeline, so flipping it is not an edit and records no revision.
const RIPPLE_MODE: &str = "ripple_mode";

pub struct Project {
    conn: Connection,
    /// The `.kerf` file backing this project, or `None` for an in-memory one.
    /// Edits write through to the connection, so a file-backed project persists
    /// automatically; an in-memory one must be [`Project::save_as`]'d first.
    path: Option<PathBuf>,
    /// Attributed to edits recorded in the history (see [`Project::set_actor`]).
    actor: EditSource,
    /// A per-call answer to "does this edit ripple?" that outranks the project's
    /// flag while it is set — see [`Project::with_ripple`]. A `Cell`, because the
    /// edit methods take `&self` and the project is only ever used from one
    /// thread at a time (it sits behind the app's mutex).
    ripple_override: Cell<Option<bool>>,
    /// A per-call answer to "do linked clips travel together?" — see
    /// [`Project::with_links`]. Unset means yes: links are always in force unless a
    /// call says otherwise.
    links_override: Cell<Option<bool>>,
}

impl Project {
    /// Create (or overwrite the schema of) a `.kerf` file on disk.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let project = Self {
            conn: Connection::open(&path)?,
            path: Some(path),
            actor: EditSource::User,
            ripple_override: Cell::new(None),
            links_override: Cell::new(None),
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
            ripple_override: Cell::new(None),
            links_override: Cell::new(None),
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
            ripple_override: Cell::new(None),
            links_override: Cell::new(None),
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
        let has_voiceover = self.conn.prepare("SELECT voiceover FROM assets LIMIT 1").is_ok();
        if !has_voiceover {
            self.conn.execute("ALTER TABLE assets ADD COLUMN voiceover TEXT", [])?;
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

    // ---- ripple mode ------------------------------------------------------

    /// Whether this project edits in **ripple mode**: when on, an edit that
    /// changes how much footage sits ahead of a clip — a trim, a speed change, a
    /// delete, an insert onto footage — carries the later clips on that track
    /// along, gaps kept (see [`Timeline::ripple_from`] for the exact rules).
    /// Off by default, and persisted with the project. This is the *project's*
    /// setting; whether a given edit ripples is [`Project::ripple_active`].
    pub fn ripple_mode(&self) -> Result<bool> {
        Ok(matches!(self.meta(RIPPLE_MODE)?.as_deref(), Some("true" | "1")))
    }

    /// Turn ripple mode on or off for the project. Not an edit: it records no
    /// revision and does not touch the timeline.
    pub fn set_ripple_mode(&self, on: bool) -> Result<()> {
        self.set_meta(RIPPLE_MODE, if on { "true" } else { "false" })
    }

    /// Whether an edit made right now ripples: a per-call override from
    /// [`Project::with_ripple`] when one is in force, else the project's flag.
    pub fn ripple_active(&self) -> Result<bool> {
        match self.ripple_override.get() {
            Some(on) => Ok(on),
            None => self.ripple_mode(),
        }
    }

    /// Run `f` with ripple forced `on` or `off` for every edit it makes, or — for
    /// `None` — leave whatever is in force alone (the project's flag, or an
    /// enclosing override). This is how a tool takes an optional per-call
    /// `ripple` argument without every edit method growing a parameter:
    ///
    /// ```text
    /// project.with_ripple(args.ripple, |p| p.trim(clip, None, Some(4.0), None))?;
    /// ```
    ///
    /// The override is undone when `f` returns, however it returns, and it
    /// covers staged agent edits the same way as live ones.
    pub fn with_ripple<R>(&self, ripple: Option<bool>, f: impl FnOnce(&Project) -> R) -> R {
        struct Restore<'a>(&'a Cell<Option<bool>>, Option<bool>);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let _restore = Restore(&self.ripple_override, self.ripple_override.get());
        if ripple.is_some() {
            self.ripple_override.set(ripple);
        }
        f(self)
    }

    // ---- linked clips ----------------------------------------------------------

    /// Whether an edit made right now carries linked clips together: yes, unless an
    /// enclosing [`Project::with_links`] said no. Links are a property of the clips
    /// (`Clip::link_id`), so unlike ripple there is no project-wide switch — only
    /// the per-call escape hatch (the GUI's Alt-drag, an MCP tool's `link: false`).
    pub fn links_active(&self) -> bool {
        self.links_override.get().unwrap_or(true)
    }

    /// Run `f` with linked-clip propagation forced `on` or `off` for every edit it
    /// makes, or — for `None` — leave whatever is in force alone. Off, an edit
    /// touches only the clips it names and the ripple pass drops its sync lock, so
    /// a picture and its sound can be moved, trimmed or removed apart (they stay
    /// linked; the partners may then no longer line up). The override is undone
    /// when `f` returns, however it returns, and covers staged agent edits the same
    /// way as live ones.
    ///
    /// ```text
    /// project.with_links(args.link, |p| p.move_clip(clip, 4.0, None))?;
    /// ```
    pub fn with_links<R>(&self, links: Option<bool>, f: impl FnOnce(&Project) -> R) -> R {
        struct Restore<'a>(&'a Cell<Option<bool>>, Option<bool>);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let _restore = Restore(&self.links_override, self.links_override.get());
        if links.is_some() {
            self.links_override.set(links);
        }
        f(self)
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
            voiceover: None,
        })
    }

    /// Insert (or replace) an asset record directly.
    pub fn insert_asset(&self, asset: &Asset) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO assets (id, path, name, duration, streams, imported_at, source_paths, voiceover)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
                asset.voiceover.as_ref().map(serde_json::to_string).transpose()?,
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
        let row = self
            .conn
            .query_row(
                &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE path = ?1 LIMIT 1"),
                params![path],
                read_asset_row,
            )
            .optional()?;
        row.map(row_to_asset).transpose()
    }

    pub fn list_assets(&self) -> Result<Vec<Asset>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {ASSET_COLUMNS} FROM assets ORDER BY imported_at"))?;
        let rows = stmt.query_map([], read_asset_row)?;
        let mut assets = Vec::new();
        for row in rows {
            assets.push(row_to_asset(row?)?);
        }
        Ok(assets)
    }

    pub fn get_asset(&self, id: Uuid) -> Result<Option<Asset>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = ?1"),
                params![id.to_string()],
                read_asset_row,
            )
            .optional()?;
        row.map(row_to_asset).transpose()
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
    /// [`Project::decode_preview_frame`] zoomed into `region` (fractions of
    /// the frame). Reads the **original** source rather than the 1280-wide
    /// proxy: a zoom is a request for the pixels the proxy threw away.
    pub fn decode_preview_region(
        asset: &Asset,
        time_secs: f64,
        region: engine::Region,
        max_width: u32,
        quality: u8,
    ) -> Result<Vec<u8>> {
        let time_secs = if asset.is_image() { 0.0 } else { time_secs };
        engine::frame_jpeg_region(Path::new(&asset.path), time_secs, region, max_width, quality, true)
    }

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

    /// [`Project::composite_timeline_frame`] zoomed into `region` of the
    /// composited canvas — the same lock-free shape, for an agent checking a
    /// detail of the cut (a caption against the safe area, a mask edge).
    pub fn composite_timeline_region(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        region: engine::Region,
        max_width: u32,
        quality: u8,
    ) -> Result<Vec<u8>> {
        let opts = engine::ExportOptions {
            hwaccel: engine::decode_hwaccel(),
            ..engine::ExportOptions::default()
        };
        engine::timeline_frame_region(timeline, assets, &opts, time_secs, region, max_width, quality)
    }

    /// How ready the current cut is for each publishing target — what a platform
    /// would reject, what it would accept and then quietly under-distribute, and
    /// what would simply be better.
    ///
    /// Reads the **working** timeline, so an agent assembling a cut sees the
    /// verdict on its own proposal rather than on the user's live one.
    /// `frame` overrides the shape the cut is judged at — what the export dialog
    /// passes when a render is about to resize away from the project frame.
    pub fn platform_check(&self, frame: Option<(u32, u32)>) -> Result<Vec<crate::platform::DeliveryCheck>> {
        Ok(crate::platform::check_all(&self.cut_summary(frame)?))
    }

    /// What the readiness check needs to know about the current cut.
    pub fn cut_summary(&self, frame: Option<(u32, u32)>) -> Result<crate::platform::CutSummary> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        // The same gate the export applies, so a muted track is as absent here
        // as it will be in the file.
        let rendered = timeline.for_render();
        let (width, height) = frame.unwrap_or_else(|| engine::delivery_frame(&rendered, &assets));
        // Audio-bearing means what the export means by it: any clip whose asset
        // carries an audio stream, on a video track as much as an audio one.
        // A picture clip whose sound was detached adds none (its audio clip does).
        let has_audio = rendered
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .any(|c| c.source_audio && assets.iter().find(|a| a.id == c.asset_id).is_some_and(|a| a.has_audio()));
        Ok(crate::platform::CutSummary {
            duration: rendered.duration(),
            width,
            height,
            has_audio,
            has_text: !rendered.overlays.is_empty(),
        })
    }

    /// The owned inputs a **cover frame** render needs: the working timeline and
    /// the **original** assets. Deliberately not the proxy-swapped preview list
    /// — a cover is a delivered image, so it comes off the same media the export
    /// reads. Resolved together so the caller can drop the project lock before
    /// [`Project::render_still`] runs ffmpeg.
    pub fn export_still_inputs(&self) -> Result<(Timeline, Vec<Asset>)> {
        Ok((self.working_timeline()?, self.list_assets()?))
    }

    /// Write the composited frame at `time_secs` to `path` as a cover image,
    /// **without** `&self` — the lock-free half of [`Project::export_still`].
    ///
    /// `format` defaults to whatever the path's extension asks for. The frame is
    /// rendered at the project's full delivery resolution through the export
    /// graph, so the cover is a real frame of the finished video.
    pub fn render_still(
        timeline: &Timeline,
        assets: &[Asset],
        time_secs: f64,
        path: impl AsRef<Path>,
        format: Option<engine::ImageFormat>,
    ) -> Result<PathBuf> {
        let path = path.as_ref();
        let format = format.unwrap_or_else(|| engine::ImageFormat::from_path(path));
        let opts = engine::ExportOptions {
            hwaccel: engine::decode_hwaccel(),
            ..engine::ExportOptions::default()
        };
        // `-q:v 2` is the highest useful JPEG quality; a cover is re-encoded by
        // whatever platform receives it, so there is nothing to gain by saving
        // bytes here and plenty to lose.
        engine::export_still(timeline, assets, &opts, time_secs, path, format, 2)
    }

    /// Render a cover frame for the current timeline at `time_secs`.
    pub fn export_still(&self, time_secs: f64, path: impl AsRef<Path>, format: Option<engine::ImageFormat>) -> Result<PathBuf> {
        let (timeline, assets) = self.export_still_inputs()?;
        Self::render_still(&timeline, &assets, time_secs, path, format)
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
            let source = Self::preview_source(asset);
            if source != Path::new(&asset.path) {
                // The proxy was tone-mapped when it was encoded, so the graph
                // must not convert it again.
                *asset = asset.as_sdr_proxy();
                asset.path = source.to_string_lossy().into_owned();
            }
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

    /// `[start, end)` **source seconds** of an asset's audio as `buckets` min/max
    /// peak pairs per channel — what the timeline draws a clip's waveform from.
    /// Served from a cached peak pyramid (one decode per file, ever), so any
    /// window at any zoom is a slice read. `buckets` is capped at
    /// [`crate::MAX_WAVEFORM_BUCKETS`]; see [`crate::WaveformRange`] for the shape.
    /// An asset with no audio stream is an `InvalidArgument`.
    pub fn waveform_range(&self, asset_id: Uuid, start: f64, end: f64, buckets: usize) -> Result<engine::WaveformRange> {
        let asset = self.require_asset(asset_id)?;
        Self::decode_waveform_range(&asset, start, end, buckets)
    }

    /// [`Project::waveform_range`] for an already-resolved [`Asset`], *without*
    /// `&self` — so the caller can release the project lock before the first
    /// call's whole-file decode (later calls are memoized and cheap). Ungated
    /// like [`Project::decode_waveform`]: a waveform appearing is not worth
    /// queueing behind an export.
    pub fn decode_waveform_range(asset: &Asset, start: f64, end: f64, buckets: usize) -> Result<engine::WaveformRange> {
        // A video-only asset has nothing to draw; say so rather than spawn an
        // ffmpeg to fail on `-map 0:a:0`. (No stream info at all is tried anyway.)
        if !asset.streams.is_empty() && !asset.streams.iter().any(|s| s.kind == StreamKind::Audio) {
            return Err(Error::InvalidArgument(format!("asset {} has no audio stream", asset.id)));
        }
        engine::waveform_range_of(Path::new(&asset.path), start, end, buckets)
    }

    /// The ready proxy a filmstrip should decode instead of `asset`'s original, if
    /// one has already been generated — never waited for: without one the original
    /// is decoded, and the result is the same cache entry either way. (A proxy's
    /// path depends on whether the source is HDR, which can take an `ffprobe`: that
    /// is why this is resolved inside [`Project::decode_filmstrip`], off the project
    /// lock, and only when the strip is not already cached.)
    fn filmstrip_proxy(asset: &Asset) -> Option<PathBuf> {
        let source = Self::preview_source(asset);
        (source != Path::new(&asset.path)).then_some(source)
    }

    /// The thumbnail strip of an asset's video (see [`crate::Filmstrip`] for what a
    /// thumbnail is and how to place one), from the cache when it exists, else
    /// built with one decode. Holds `&self` for the decode — a surface resolves the
    /// asset with [`Project::require_asset`] under the lock and calls
    /// [`Project::decode_filmstrip`] with it released instead. An asset with no
    /// video stream is an `InvalidArgument`.
    pub fn filmstrip(&self, asset_id: Uuid) -> Result<std::sync::Arc<engine::Filmstrip>> {
        Self::decode_filmstrip(&self.require_asset(asset_id)?)
    }

    /// [`Project::filmstrip`] for an already-resolved [`Asset`], *without*
    /// `&self` — so the caller can release the project lock before anything slow
    /// runs: the asset's ready proxy is looked up here (decoded in preference to
    /// the original), and the first call's whole-file decode follows (later calls
    /// are cached and cheap). Ungated like [`Project::decode_waveform_range`]: the
    /// timeline draws from it, and a thumbnail appearing is not worth queueing
    /// behind an export — but, being a video decode, it keeps to a couple of niced
    /// threads at every CPU budget (see `engine::filmstrip`).
    pub fn decode_filmstrip(asset: &Asset) -> Result<std::sync::Arc<engine::Filmstrip>> {
        engine::filmstrip_for(asset, || Self::filmstrip_proxy(asset))
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
        Ok(serde_json::from_str(&self.timeline_json()?)?)
    }

    /// The live timeline's raw stored JSON, byte-for-byte as last written by
    /// [`Self::save_timeline_str`] — used to compare content against a staged
    /// row's `base` (itself the same raw string, see [`Self::begin_staging`])
    /// without round-tripping either through `Timeline` first.
    fn timeline_json(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT data FROM timeline WHERE id = 1", [], |r| r.get(0))?)
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
    ///
    /// **Ripple mode** lives here, so every op honors it uniformly — the agent's
    /// staged ones included: with it on (see [`Project::ripple_active`]) the
    /// timeline is snapshotted before `f` runs and the result goes through
    /// [`Timeline::ripple_from`] before it is stored. With it off nothing is
    /// cloned and the edit is exactly what `f` made.
    ///
    /// **Linked clips** are kept in step here too, for every op: with links in
    /// force and something linked, the clips an edit (and the ripple) moved take their
    /// linked partners along ([`Timeline::conform_links`]), and an edit that would
    /// still leave an in-step picture and sound apart is refused
    /// ([`Timeline::first_sync_break`]) — whether the op knew about links or not.
    /// Afterwards a link left with one clip (its partner was cut, deleted, or on a
    /// track that was removed) is dissolved.
    fn edit_timeline<R>(&self, label: &str, f: impl FnOnce(&mut Timeline) -> Result<R>) -> Result<R> {
        let ripple = self.ripple_active()?;
        self.run_edit(|_| label.to_string(), ripple, &[], f)
    }

    /// [`Self::edit_timeline`] for an op that decides its own layout and so is
    /// never rippled: `ripple_delete` and `cut_clip_range` and the beat snap
    /// already close or reflow the track themselves (so rippling them again would
    /// be at best a no-op and at worst — a snap whose compensating moves happen to
    /// land a clip back where it started — a second, wrong shift), `reorder`
    /// reflows, and a move changes where clips *are*, not how much footage sits
    /// ahead of them.
    fn edit_timeline_exact<R>(&self, label: &str, f: impl FnOnce(&mut Timeline) -> Result<R>) -> Result<R> {
        self.run_edit(|_| label.to_string(), false, &[], f)
    }

    /// [`Self::edit_timeline`] for an op that **names clips** (`anchors`), whose track
    /// speaks for a link group when the sync lock has to choose, and whose label may
    /// depend on what the edit did (`label` is given its result — a group move counts
    /// the partners it carried, which only the edit knows).
    fn edit_named<R>(
        &self,
        anchors: &[Uuid],
        label: impl FnOnce(&R) -> String,
        f: impl FnOnce(&mut Timeline) -> Result<R>,
    ) -> Result<R> {
        let ripple = self.ripple_active()?;
        self.run_edit(label, ripple, anchors, f)
    }

    /// [`Self::edit_named`] for an op that is never rippled.
    fn edit_named_exact<R>(
        &self,
        anchors: &[Uuid],
        label: impl FnOnce(&R) -> String,
        f: impl FnOnce(&mut Timeline) -> Result<R>,
    ) -> Result<R> {
        self.run_edit(label, false, anchors, f)
    }

    fn run_edit<R>(
        &self,
        label: impl FnOnce(&R) -> String,
        ripple: bool,
        anchors: &[Uuid],
        f: impl FnOnce(&mut Timeline) -> Result<R>,
    ) -> Result<R> {
        let links = self.links_active();
        let anchors: HashSet<Uuid> = anchors.iter().copied().collect();
        let f = move |timeline: &mut Timeline| -> Result<R> {
            // The sync lock and guard: with links in force and something linked, the
            // partners of what the edit moved follow it, and an edit that would
            // leave an in-step picture and sound apart is refused. A project that
            // links nothing skips all of it, and the snapshot with it.
            let sync = links && timeline.has_links();
            let before = (ripple || sync).then(|| timeline.clone());
            let result = f(timeline)?;
            if let Some(before) = &before {
                if ripple {
                    *timeline = timeline.ripple_lanes(before);
                }
                if sync {
                    timeline.conform_links(before, &anchors, &HashMap::new())?;
                    if let Some((a, b)) = timeline.first_sync_break(before) {
                        return Err(Error::InvalidArgument(format!(
                            "that edit would leave the linked clips on {a} and {b} out of step with each other — unlink them first if they are meant to part"
                        )));
                    }
                }
            }
            if timeline.has_links() {
                timeline.dissolve_all_orphans();
            }
            Ok(result)
        };
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
        self.record_revision(&label(&result), self.actor, &json)?;
        tx.commit()?;
        Ok(result)
    }

    /// Whether the timeline an edit made now would land on has any linked clip —
    /// answered from the stored JSON (`link_id` is only ever written for a linked
    /// clip) without parsing it, so an op that wants to know *before* it runs, on
    /// a project that links nothing, pays one `instr` rather than a timeline load.
    fn working_has_links(&self) -> Result<bool> {
        const KEY: &str = "\"link_id\"";
        if self.actor == EditSource::Agent {
            if let Some(row) = self.staged_row()? {
                return Ok(row.timeline.contains(KEY));
            }
        }
        let found: i64 = self
            .conn
            .query_row("SELECT instr(data, ?1) FROM timeline WHERE id = 1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        Ok(found > 0)
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
    fn edit_staged<R>(
        &self,
        row: StagedRow,
        label: impl FnOnce(&R) -> String,
        f: impl FnOnce(&mut Timeline) -> Result<R>,
    ) -> Result<R> {
        let mut timeline: Timeline = serde_json::from_str(&row.timeline)?;
        let result = f(&mut timeline)?;
        let mut edits = row.edits;
        edits.push(label(&result));
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
        let snapshot = self.timeline_json()?;
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
            // Compared by content, not `base_seq` against the current head: an
            // undo followed by a new edit reinserts a *different* revision at
            // the same seq, which a seq comparison alone would miss.
            stale: self.timeline_json()? != row.base,
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
        // Content, not `base_seq`: `seq` is recycled by `record_revision` once
        // an undo branches off an earlier point, so a plain seq comparison
        // would miss "undo, then make an unrelated edit" landing right back on
        // the seq the proposal was staged from with completely different
        // content underneath it.
        if self.timeline_json()? != row.base && !force {
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
    ///
    /// **Linked clips are split with it**: each partner that has `at` inside it is
    /// cut at the same time, and the new right halves are linked to each other (the
    /// left halves keep the group) — the razor on a picture cuts its sound too. A
    /// partner on a locked track refuses the whole split; `with_links(Some(false))`
    /// splits the named clip alone. The `(left, right)` returned are the named
    /// clip's halves.
    pub fn split_at(&self, clip_id: Uuid, at: f64) -> Result<(Clip, Clip)> {
        let links = self.links_active();
        self.edit_timeline("Split clip", |timeline| {
            if links {
                timeline.split_clip_linked(clip_id, at)
            } else {
                timeline.split_clip(clip_id, at)
            }
        })
    }

    /// Adjust a clip's source in/out points. `timeline_start` moves the clip in
    /// the same edit — a left-edge trim from the GUI shifts the start so the
    /// right edge stays put, and doing both here keeps undo a single step.
    /// Omitted, the timeline position is preserved.
    ///
    /// In **ripple mode** the later clips on the track follow the change in the
    /// clip's length, and a left-edge trim keeps the clip's start whether or not
    /// `timeline_start` was passed (see [`Timeline::ripple_from`]); the returned
    /// clip is the one as it ended up.
    ///
    /// **Linked clips follow the edge**: a partner that shares the edge being
    /// trimmed (within 1 ms) has that edge moved by the same amount, clamped to its
    /// own footage; a pure move (`timeline_start` alone) moves them by the same Δt.
    /// A locked partner refuses the trim. `with_links(Some(false))` trims the named
    /// clip alone.
    pub fn trim(
        &self,
        clip_id: Uuid,
        source_in: Option<f64>,
        source_out: Option<f64>,
        timeline_start: Option<f64>,
    ) -> Result<Clip> {
        // The partners follow the trimmed edge only if there are any: a project that
        // links nothing never loads its assets for this.
        let links = self.links_active() && self.working_has_links()?;
        let footage = if links { self.source_limits()? } else { SourceLimits::new() };
        let clip = self.edit_named(
            &[clip_id],
            |_| "Trim clip".to_string(),
            |timeline| {
                let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
                let was = timeline.tracks[ti].clips[ci].clone();
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
                if links {
                    // The partners follow the edge that moved (see `carry_extent_edit`).
                    timeline.carry_extent_edit(clip_id, &was, &footage)?;
                }
                Ok(out)
            },
        )?;
        // The ripple can move the trimmed clip itself (a left-edge trim keeps its
        // start), so hand back what is on the timeline rather than what the trim
        // alone produced.
        if self.ripple_active()? {
            if let Some(current) = self.working_timeline()?.clip(clip.id) {
                return Ok(current.clone());
            }
        }
        Ok(clip)
    }

    /// Cut a **source-time** range out of a clip: the clip is split around the
    /// intersection of `[from, to]` with its source window, the middle piece
    /// removed, and later clips on the track ripple left to close the gap.
    /// This is the transcript-editing primitive — delete a sentence and the
    /// cut tightens. Returns the kept pieces in play order. It closes the gap
    /// itself, so ripple mode leaves it alone.
    ///
    /// **Linked clips lose the same stretch**: the *timeline* span the cut removes
    /// is taken out of every partner it overlaps too, each closing up on its own
    /// track — so cutting a sentence from a picture cuts its detached sound. A
    /// locked partner refuses the cut; `with_links(Some(false))` cuts the named clip
    /// alone.
    pub fn cut_clip_range(&self, clip_id: Uuid, from: f64, to: f64) -> Result<Vec<Clip>> {
        let links = self.links_active();
        self.edit_named_exact(
            &[clip_id],
            |_| "Cut range".to_string(),
            |timeline| {
                if links {
                    timeline.cut_clip_range_linked(clip_id, from, to)
                } else {
                    timeline.cut_clip_range(clip_id, from, to)
                }
            },
        )
    }

    /// Move a clip to a new index within its track and re-flow the track gaplessly.
    pub fn reorder(&self, track_id: Uuid, clip_id: Uuid, new_index: usize) -> Result<()> {
        self.edit_named_exact(
            &[clip_id],
            |_| "Reorder clip".to_string(),
            |timeline| {
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
            },
        )
    }

    /// Move a clip to a new timeline position, optionally onto another track of
    /// the **same kind**. Free positioning: the clip keeps its duration and
    /// gaps are allowed. A move that would overlap another clip on the
    /// destination track is rejected, so each track stays a well-ordered,
    /// non-overlapping lane (which keeps the positional render well-defined).
    /// A move never ripples, whatever the ripple mode.
    ///
    /// **Linked clips move with it** — each partner by the same Δt, on its own track
    /// (a track change is the named clip's alone). A clip with partners is moved as
    /// the group it is, through [`Project::move_clips`]: one `Move N clips`
    /// revision, all or nothing, refusing a locked track (the named clip's too) and
    /// a partner that would start before 0 or land on another clip.
    /// `with_links(Some(false))` moves the named clip alone, by the rules below.
    pub fn move_clip(&self, clip_id: Uuid, timeline_start: f64, track_id: Option<Uuid>) -> Result<Clip> {
        let start = timeline_start.max(0.0);
        if self.links_active() && self.working_has_links()? && !self.working_timeline()?.link_partners(clip_id).is_empty() {
            let moved = self.move_clips(&[ClipMove {
                clip_id,
                timeline_start: start,
                track_id,
            }])?;
            return Ok(moved.into_iter().next().expect("the named clip was moved"));
        }
        self.edit_timeline_exact("Move clip", |timeline| {
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

    /// Move several clips in **one** edit — one revision, `Move N clips` — for a
    /// marquee selection dragged together. Each [`ClipMove`] names a clip, where
    /// it starts afterwards (absolute seconds) and, optionally, another track of
    /// the same kind; the UI works out the landing spots, this decides whether
    /// they are legal.
    ///
    /// All or nothing: the whole group is validated before anything moves, and
    /// any error — an unknown clip or track, a different-kind track, a locked
    /// track (source or destination), a clip named twice, a start before 0 or
    /// not finite, or a landing spot that overlaps a clip that is staying or
    /// another clip in the group — leaves the timeline and its history exactly as
    /// they were. Clips moving together may pass through the places they are
    /// leaving, so nudging a run of abutting clips by a second is fine.
    /// Never ripples. Returns the moved clips in request order.
    ///
    /// **Linked clips travel**: every clip linked to one named, and not named
    /// itself, moves by the same Δt on its own track (`Timeline::with_linked_moves`),
    /// and is checked in the same group — so the answer is the named clips in
    /// request order *then* the partners carried along, and the label counts all of
    /// them. A partner that would start before 0, or sits on a locked track, refuses
    /// the move. `with_links(Some(false))` moves exactly what is named.
    pub fn move_clips(&self, moves: &[ClipMove]) -> Result<Vec<Clip>> {
        let links = self.links_active();
        // The label counts the partners carried along, which only the edit knows.
        let label = |moved: &Vec<Clip>| match moved.len() {
            1 => "Move clip".to_string(),
            n => format!("Move {n} clips"),
        };
        // Named clips are authoritative: two partners both named and moved apart were
        // parted by hand, which the guard refuses rather than the sync lock undoing.
        let anchors: Vec<Uuid> = moves.iter().map(|m| m.clip_id).collect();
        self.edit_named_exact(&anchors, label, |timeline| {
            if links {
                let all = timeline.with_linked_moves(moves)?;
                timeline.move_clips(&all)
            } else {
                timeline.move_clips(moves)
            }
        })
    }

    // ---- edit modes: roll, slip, slide, split-and-remove ---------------------

    /// How far each asset's footage reaches — what roll, slip and slide clamp a
    /// clip's source window to ([`crate::model::Asset::source_limit`]: the
    /// duration, infinite for a still).
    pub fn source_limits(&self) -> Result<SourceLimits> {
        Ok(self
            .list_assets()?
            .into_iter()
            .map(|asset| (asset.id, asset.source_limit()))
            .collect())
    }

    /// **Roll** the cut between two adjacent clips of one track by `delta`
    /// seconds (positive is later): `clip_a`'s end and `clip_b`'s start move
    /// together, so the pair covers the same stretch and nothing after it moves.
    /// Clamped to each clip's footage and a 0.05 s floor; the outcome says how far
    /// it really went. One `Roll edit` revision. See [`Timeline::roll_edit`] for
    /// every rule. Never ripples — it moves no length, only where footage changes
    /// hands — whatever the ripple mode.
    ///
    /// **Linked clips roll too**: each pair of partners sharing the cut (a partner of
    /// `clip_a` and one of `clip_b` that touch on one track) rolls by the same
    /// amount, and the whole group clamps to its tightest pair. A partner with no
    /// pair — a clip running through the cut — is left alone.
    pub fn roll_edit(&self, clip_a: Uuid, clip_b: Uuid, delta: f64) -> Result<EditOutcome> {
        let footage = self.source_limits()?;
        let links = self.links_active();
        self.edit_timeline_exact("Roll edit", |timeline| {
            if links {
                timeline.roll_edit_linked(clip_a, clip_b, delta, &footage)
            } else {
                timeline.roll_edit(clip_a, clip_b, delta, &footage)
            }
        })
    }

    /// **Slip** a clip: show a different part of its footage in the same place
    /// and for the same length. `delta` is in *source* seconds; positive starts
    /// the clip later in its own footage (the mirrored window for a reversed
    /// clip). Clamped to the footage; a still has none to slip. One `Slip clip`
    /// revision. See [`Timeline::slip_clip`]. Never ripples.
    ///
    /// **Linked clips slip too**, by the same *timeline* shift of the footage (a
    /// partner at another speed slips by the matching source seconds; a still, with
    /// no footage, is skipped), and the group clamps to its tightest member.
    pub fn slip_clip(&self, clip_id: Uuid, delta: f64) -> Result<EditOutcome> {
        let footage = self.source_limits()?;
        let links = self.links_active();
        self.edit_timeline_exact("Slip clip", |timeline| {
            if links {
                timeline.slip_clip_linked(clip_id, delta, &footage)
            } else {
                timeline.slip_clip(clip_id, delta, &footage)
            }
        })
    }

    /// **Slide** a clip along its track by `delta` timeline seconds, the
    /// neighbours that touch it giving way (the previous clip's end and the next
    /// clip's start move with it), so its content and the stretch the three span
    /// are unchanged. Clamped to the neighbours' footage and a 0.05 s floor. One
    /// `Slide clip` revision. See [`Timeline::slide_clip`] for the rules with
    /// gaps. Never ripples.
    ///
    /// **Linked clips slide too**, each with its own touching neighbours giving way,
    /// and the group clamps to its tightest member.
    pub fn slide_clip(&self, clip_id: Uuid, delta: f64) -> Result<EditOutcome> {
        let footage = self.source_limits()?;
        let links = self.links_active();
        self.edit_timeline_exact("Slide clip", |timeline| {
            if links {
                timeline.slide_clip_linked(clip_id, delta, &footage)
            } else {
                timeline.slide_clip(clip_id, delta, &footage)
            }
        })
    }

    /// **Split and remove**: cut a clip at timeline time `at` and drop the
    /// `side` half, in one `Split and remove left` / `… right` revision. Unlike
    /// the three above this *does* follow ripple mode, because it changes a clip's
    /// length like any trim: with it on, the later clips on the track close the
    /// gap (a left removal keeps the clip's own start and pulls the rest in); with
    /// it off the gap stays, exactly as after a plain split and delete. The
    /// surviving half keeps the clip's id; see [`Timeline::split_remove`].
    /// Returns it as it ended up on the timeline.
    pub fn split_remove(&self, clip_id: Uuid, at: f64, side: SplitSide) -> Result<Clip> {
        let mut kept = self.split_remove_clips(&[ClipCut { clip_id, at }], side)?;
        Ok(kept.remove(0))
    }

    /// [`Self::split_remove`] on several clips as **one** revision — the playhead
    /// trim of a selection, V1 and its A1 partner together, which undoes in one
    /// step. Each cut names a clip and the time it is cut at; `side` is the same for
    /// all. All or nothing, one clip per track (see
    /// [`Timeline::split_remove_clips`]); each track then ripples on its own, from its
    /// own one cut, when ripple mode is on. The label is `Split and remove left` /
    /// `… right`, with `(N clips)` appended for a group. Returns the surviving
    /// clips, in request order, as they ended up on the timeline.
    ///
    /// **Linked clips are cut with them**: a partner not named, on a track the
    /// request does not already cut, with the cut time inside it, is cut at the same
    /// time (`Timeline::with_linked_cuts`) — so a one-clip call on a picture trims
    /// its sound too, and the answer is the named survivors then the partners'. A
    /// partner the cut time falls outside is untouched. `with_links(Some(false))`
    /// cuts exactly what is named.
    pub fn split_remove_clips(&self, cuts: &[ClipCut], side: SplitSide) -> Result<Vec<Clip>> {
        let what = match side {
            SplitSide::Left => "Split and remove left",
            SplitSide::Right => "Split and remove right",
        };
        let links = self.links_active();
        let anchors: Vec<Uuid> = cuts.iter().map(|c| c.clip_id).collect();
        // The label counts the partners cut with the named clips: what the edit cut.
        let label = |kept: &Vec<Clip>| match kept.len() {
            0 | 1 => what.to_string(),
            n => format!("{what} ({n} clips)"),
        };
        let kept = self.edit_named(&anchors, label, |timeline| {
            if links {
                let all = timeline.with_linked_cuts(cuts)?;
                timeline.split_remove_clips(&all, side)
            } else {
                timeline.split_remove_clips(cuts, side)
            }
        })?;
        // As `trim`: ripple can move a surviving half itself (a left removal keeps
        // its start), so hand back what is on the timeline.
        if self.ripple_active()? {
            let now = self.working_timeline()?;
            return Ok(kept
                .into_iter()
                .map(|clip| now.clip(clip.id).cloned().unwrap_or(clip))
                .collect());
        }
        Ok(kept)
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

        self.edit_timeline_exact("Insert clips", |timeline| {
            // A copy is a new clip, so it cannot share a link group with the original
            // — but copies of a linked *group* (a picture and its sound, pasted
            // together) are linked to each other under a fresh id, and a copy whose
            // partner was not pasted along with it is simply unlinked.
            let mut members: HashMap<Uuid, usize> = HashMap::new();
            for (_, clip) in placements {
                if let Some(link) = clip.link_id {
                    *members.entry(link).or_default() += 1;
                }
            }
            let fresh: HashMap<Uuid, Uuid> = members
                .into_iter()
                .filter(|(_, n)| *n >= 2)
                .map(|(link, _)| (link, Uuid::new_v4()))
                .collect();
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
                copy.link_id = clip.link_id.and_then(|link| fresh.get(&link).copied());
                staged.push((ti, copy));
            }

            // A picture whose sound was detached is silent *because* that sound plays
            // from its partner. Pasted without the partner it would be a silent clip
            // for good, so it gets its own sound back; kept with a partner that does
            // carry the same footage's audio it stays muted (heard once, from there).
            let carriers: HashSet<(Uuid, Uuid)> = staged
                .iter()
                .filter(|(ti, c)| timeline.tracks[*ti].kind == StreamKind::Audio && c.link_id.is_some())
                .map(|(_, c)| (c.link_id.expect("filtered"), c.asset_id))
                .collect();
            for (ti, clip) in &mut staged {
                if timeline.tracks[*ti].kind == StreamKind::Video
                    && !clip.source_audio
                    && !clip.link_id.is_some_and(|link| carriers.contains(&(link, clip.asset_id)))
                {
                    clip.source_audio = true;
                }
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
    /// leaves a gap — unless ripple mode is on, which makes it this.)
    ///
    /// **Linked clips are deleted with it**, each closing the gap on its own track
    /// by its own length; a partner on a locked track refuses the delete.
    /// `with_links(Some(false))` deletes the named clip alone.
    pub fn ripple_delete(&self, clip_id: Uuid) -> Result<()> {
        let links = self.links_active();
        self.edit_named_exact(
            &[clip_id],
            |_| "Ripple delete".to_string(),
            |timeline| {
                if links {
                    timeline.ripple_delete_linked(clip_id).map(|_| ())
                } else {
                    timeline.ripple_delete_clip(clip_id)
                }
            },
        )
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

    /// Set a track's fader, the gain riding every clip on it. Clamped to
    /// `0..=4` (+12 dB), which is as far up as a fader has any business going.
    pub fn set_track_volume(&self, track_id: Uuid, volume: f32) -> Result<Track> {
        let volume = volume.clamp(0.0, 4.0);
        self.edit_timeline("Set track level", |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.volume = volume;
            Ok(track.clone())
        })
    }

    /// Set a track's stereo placement, -1 (hard left) to 1 (hard right).
    pub fn set_track_pan(&self, track_id: Uuid, pan: f32) -> Result<Track> {
        let pan = pan.clamp(-1.0, 1.0);
        self.edit_timeline("Set track pan", |timeline| {
            let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
            track.pan = pan;
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

    /// Remove a clip from the timeline. Leaves a gap, unless ripple mode is on,
    /// in which case the later clips on its track close it.
    ///
    /// **Linked clips are removed with it** (as one `Remove N clips` revision, all or
    /// nothing, refusing a locked track — the clip's own included, as every group
    /// edit does); `with_links(Some(false))` removes the named clip alone.
    pub fn remove(&self, clip_id: Uuid) -> Result<()> {
        if self.links_active() && self.working_has_links()? && !self.working_timeline()?.link_partners(clip_id).is_empty() {
            return self.remove_clips(&[clip_id]).map(|_| ());
        }
        self.edit_named(
            &[clip_id],
            |_| "Remove clip".to_string(),
            |timeline| {
                let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
                timeline.tracks[ti].clips.remove(ci);
                Ok(())
            },
        )
    }

    /// Remove several clips in **one** edit — one revision — for a multi-select
    /// delete. All or nothing: an unknown id, or a clip on a locked track, refuses
    /// the whole thing; a clip named twice is removed once. Leaves gaps — unless
    /// ripple mode is on (or forced with [`Project::with_ripple`]), in which case
    /// every track closes up behind what it lost, which is how a multi-select
    /// *ripple* delete is made. Returns how many clips were removed.
    ///
    /// **Linked clips go too**: every partner of a named clip is removed with it
    /// (and counted), refusing the whole removal if one is on a locked track —
    /// `with_links(Some(false))` removes exactly what is named.
    pub fn remove_clips(&self, clip_ids: &[Uuid]) -> Result<usize> {
        let ripple = self.ripple_active()?;
        let links = self.links_active();
        // The label counts the partners removed with the named clips, which only the
        // edit knows (it returns how many it removed).
        let label = |n: &usize| match (*n, ripple) {
            (1, false) => "Remove clip".to_string(),
            (1, true) => "Ripple delete".to_string(),
            (n, false) => format!("Remove {n} clips"),
            (n, true) => format!("Ripple delete {n} clips"),
        };
        self.edit_named(clip_ids, label, |timeline| {
            if links {
                timeline.remove_clips_linked(clip_ids)
            } else {
                timeline.remove_clips(clip_ids)
            }
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
    ///
    /// **Linked clips are retimed by the same ratio**, so a picture and its sound
    /// keep step (a partner at 1× next to a clip going 1× → 2× goes to 2×); each
    /// track's length changes by its own, and ripple mode follows per track.
    /// `with_links(Some(false))` retimes the named clip alone.
    pub fn set_speed(&self, clip_id: Uuid, speed: f64) -> Result<Clip> {
        if !speed.is_finite() || speed == 0.0 {
            return Err(Error::InvalidArgument("speed must be a non-zero, finite number".to_string()));
        }
        let links = self.links_active();
        self.edit_named(
            &[clip_id],
            |_| "Set speed".to_string(),
            |timeline| {
                if links {
                    return timeline.set_speed_linked(clip_id, speed);
                }
                let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
                timeline.tracks[ti].clips[ci].speed = speed;
                Ok(timeline.tracks[ti].clips[ci].clone())
            },
        )
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
        let mut crops = Vec::new();
        let mut first_error = None;
        for job in &plan.jobs {
            match engine::salience_map(&job.path, job.start, job.end) {
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
        let mut out = Vec::new();
        let mut first_error = None;
        for job in &plan.jobs {
            let needs: Vec<(u32, u32)> = plan
                .ratios
                .iter()
                .copied()
                .filter(|&(w, h)| crate::model::needs_crop(job.width, job.height, w as f64 / h as f64))
                .collect();
            let map = if needs.is_empty() {
                None
            } else {
                match engine::salience_map(&job.path, job.start, job.end) {
                    Ok(map) => Some(map),
                    Err(e) => {
                        tracing::warn!(clip = %job.clip_id, path = %job.path.display(), error = %e, "could not sample a shot for framing");
                        first_error.get_or_insert(e);
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

    /// Cut a clip to a shape, or clear the mask. Outside the shape the clip goes
    /// transparent, so whatever is on a lower track shows through — which is how
    /// a face is blurred (a masked, blurred copy of the shot on the track above),
    /// how a picture-in-picture is rounded off, and how one region gets its own
    /// grade. Fields are clamped, since a zero-width shape would blank the clip.
    pub fn set_mask(&self, clip_id: Uuid, mask: Option<Mask>) -> Result<Clip> {
        let mask = mask.map(Mask::normalized);
        self.edit_timeline(if mask.is_some() { "Mask clip" } else { "Clear mask" }, |timeline| {
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            timeline.tracks[ti].clips[ci].mask = mask;
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

        let mut silence: Vec<TimeRange> = analysis.silence_segments;
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
    ///
    /// **Linked clips are re-synced afterwards**: the snap reflows a lane without
    /// knowing about links, so each clip it retimed carries the change to its
    /// partners (`Timeline::carry_links_since` — a partner follows an edge it
    /// shared, a group the snap changed in several members is left as the snap made
    /// it). `with_links(Some(false))` snaps the lanes alone.
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
        let links = self.links_active();
        self.edit_timeline_exact("Cut to the beat", |timeline| {
            let before = links.then(|| timeline.clone());
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
            if let Some(before) = before {
                timeline.carry_links_since(&before, &limits)?;
            }
            Ok(aligned)
        })
    }

    /// **Extract audio**: give an asset's sound its own clip on an audio track, for
    /// every use of the asset on the timeline that still plays it.
    ///
    /// **Verified bug, fixed here:** this used to append the asset's whole audio to
    /// the first audio track *and leave the picture's own sound on*, so an asset
    /// already cut onto V1 was heard twice — the export mixes the audio of every
    /// clip whose asset carries an audio stream, video tracks included (see
    /// `doubled_audio_is_what_extract_audio_used_to_make` in the engine tests).
    /// Now each clip of the asset on a video track still playing its own sound is
    /// **detached** ([`Project::detach_audio`]: an audio clip with the same span and
    /// position, linked, the picture muted) in one `Extract audio` revision. A clip
    /// that cannot be (its track is locked) is **skipped and reported**, not a reason
    /// to fail the rest; nothing to extract — the asset is only in the bin, or its
    /// sound is already detached — is an error saying so, not a quiet fall-through.
    /// To put an asset's whole audio on an audio track, as itself, that is
    /// [`Project::add_asset_audio`].
    pub fn extract_audio(&self, asset_id: Uuid) -> Result<DetachedMany> {
        let asset = self.require_asset(asset_id)?;
        if !asset.has_audio() {
            return Err(Error::InvalidArgument("asset has no audio stream".to_string()));
        }
        let label = |_: &DetachedMany| "Extract audio".to_string();
        self.edit_named_exact(&[], label, |timeline| {
            let on_video: Vec<&Clip> = timeline
                .tracks
                .iter()
                .filter(|t| t.kind == StreamKind::Video)
                .flat_map(|t| t.clips.iter())
                .filter(|c| c.asset_id == asset_id)
                .collect();
            let sounding: Vec<Uuid> = on_video.iter().filter(|c| c.source_audio).map(|c| c.id).collect();
            if sounding.is_empty() {
                return Err(Error::InvalidArgument(if on_video.is_empty() {
                    "no clip of this asset is on a video track, so there is no sound of its own to extract — add_asset_audio puts its audio on an audio track".to_string()
                } else {
                    "this asset's sound is already on an audio track".to_string()
                }));
            }
            timeline.detach_audio_many(&sounding, &|_| true)
        })
    }

    /// **Add the asset's audio** as a clip of its own: the asset's whole audio, from
    /// the start, appended to the end of the first audio track (created when there is
    /// none). It never touches a picture clip, so an asset that is *also* on a video
    /// track playing its own sound is heard twice where the two overlap — for the
    /// sound of a clip already cut, that is [`Project::extract_audio`] /
    /// [`Project::detach_audio`], which mute the picture. One `Add audio` revision.
    pub fn add_asset_audio(&self, asset_id: Uuid) -> Result<Clip> {
        let asset = self.require_asset(asset_id)?;
        if !asset.has_audio() {
            return Err(Error::InvalidArgument("asset has no audio stream".to_string()));
        }
        self.edit_timeline_exact("Add audio", |timeline| {
            let tid = match timeline.first_track_of(StreamKind::Audio) {
                Some(tid) => tid,
                None => {
                    let track = Track::new(StreamKind::Audio, "A1");
                    let tid = track.id;
                    timeline.tracks.push(track);
                    tid
                }
            };
            let start = timeline.track(tid).map(Track::end).unwrap_or(0.0);
            let clip = Clip::for_asset(&asset, 0.0, asset.duration, start);
            timeline.track_mut(tid).unwrap().clips.push(clip.clone());
            Ok(clip)
        })
    }

    /// **Detach audio**: split a picture clip's own sound off onto an audio track.
    /// A new audio clip with the same source span, speed and timeline position goes
    /// on the audio track at the picture's own position (V1 → A1) when it has room,
    /// else the first audio track that does, else a new one; it is **linked** to the
    /// picture clip (so they move, trim, split and delete together), and the picture
    /// clip's own sound is muted — the sound is heard once, from the audio track.
    /// The picture track's fader is folded into the new clip's gain, so the **level**
    /// is what it was; the destination track's pan, duck and mute/solo now decide the
    /// rest of the mix (see [`Timeline::detach_audio`]). The audio clip carries volume,
    /// audio effects, fades and the transition; the picture keeps its own, inert
    /// while muted. One `Detach audio` revision. Refuses a clip that is not on a video
    /// track, whose asset has no audio, whose sound is already detached, or whose
    /// track or destination is locked.
    pub fn detach_audio(&self, clip_id: Uuid) -> Result<Detached> {
        let timeline = self.working_timeline()?;
        let clip = timeline.clip(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let has_audio = self.require_asset(clip.asset_id)?.has_audio();
        self.edit_timeline_exact("Detach audio", |timeline| timeline.detach_audio(clip_id, has_audio))
    }

    /// [`Project::detach_audio`] on several clips as **one** `Detach audio (N clips)`
    /// revision — a multi-select detach that undoes in a step. A clip that cannot be
    /// detached (not a video clip, an asset without audio, already detached, a
    /// locked track) is **skipped and reported**; the call fails only when nothing
    /// could be detached, so it never records an empty edit.
    pub fn detach_audio_clips(&self, clip_ids: &[Uuid]) -> Result<DetachedMany> {
        let timeline = self.working_timeline()?;
        let mut has_audio: HashMap<Uuid, bool> = HashMap::new();
        for id in clip_ids {
            if let Some(clip) = timeline.clip(*id) {
                if let std::collections::hash_map::Entry::Vacant(slot) = has_audio.entry(clip.asset_id) {
                    slot.insert(self.require_asset(clip.asset_id)?.has_audio());
                }
            }
        }
        let label = |done: &DetachedMany| match done.detached.len() {
            1 => "Detach audio".to_string(),
            n => format!("Detach audio ({n} clips)"),
        };
        self.edit_named_exact(&[], label, |timeline| {
            timeline.detach_audio_many(clip_ids, &|asset| has_audio.get(&asset).copied().unwrap_or(false))
        })
    }

    /// **Reattach audio**: the inverse of [`Project::detach_audio`] — delete the
    /// linked audio clip(s) carrying the picture's asset and let the picture play its
    /// own sound again. Name either clip of the pair. Refused when the picture has
    /// no audio partner to delete *and* another clip is already playing its sound
    /// (unmuting would double it). One `Reattach audio` revision; returns the picture
    /// clip. It is never rippled: deleting the sound is not a change in how much
    /// footage sits ahead of anything.
    pub fn reattach_audio(&self, clip_id: Uuid) -> Result<Clip> {
        self.edit_timeline_exact("Reattach audio", |timeline| timeline.reattach_audio(clip_id))
    }

    /// **Link** clips into one group (one `Link N clips` revision): from then on an
    /// edit to one is carried to the others (see [`Project::with_links`] for the
    /// escape hatch). At least two clips, on different tracks, none on a locked
    /// track. Returns the group's link id.
    pub fn link_clips(&self, clip_ids: &[Uuid]) -> Result<Uuid> {
        self.edit_timeline_exact(&format!("Link {} clips", clip_ids.len()), |timeline| {
            timeline.link_clips(clip_ids)
        })
    }

    /// **Unlink** clips (one `Unlink clips` revision): each leaves its group and a
    /// group left with a single clip dissolves. Errors when none was linked.
    /// Returns how many of the named clips were.
    pub fn unlink_clips(&self, clip_ids: &[Uuid]) -> Result<usize> {
        self.edit_timeline_exact("Unlink clips", |timeline| timeline.unlink_clips(clip_ids))
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
        let Some(id) = next else { return Ok(None) };
        let id = parse_uuid(&id)?;
        // Claiming a task opens a staging session for it, so the work an agent
        // does on the user's behalf is a proposal by default and never
        // rewrites the open cut unasked. A session already open for a
        // *different* task (or for none) must not silently absorb this one's
        // edits too — that folds two tasks' work into a single proposal and
        // leaves the second task with no review of its own — so refuse before
        // touching this task's state at all. The one exception is a session
        // whose owning task has already gone terminal: `fail_task` used to
        // leave one behind with nothing to ever clean it up, which wedged the
        // queue for good, so a claim is also the chance to clear it out.
        match self.staged_row()? {
            Some(row) if row.task_id == Some(id) => {}
            Some(row) => {
                let terminal = match row.task_id {
                    Some(owner) => self
                        .get_task(owner)?
                        .map(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Failed))
                        .unwrap_or(true),
                    None => false,
                };
                if !terminal {
                    return Err(Error::StagedEditPending);
                }
                self.discard_staged()?;
                self.begin_staging(Some(id), None)?;
            }
            None => {
                self.begin_staging(Some(id), None)?;
            }
        }
        Ok(Some(self.set_task_state(id, TaskStatus::Working, None)?))
    }

    /// Mark a task `ready` for review, recording the agent's summary.
    pub fn complete_task(&self, id: Uuid, result: Option<String>) -> Result<Task> {
        self.set_task_state(id, TaskStatus::Ready, Some(result))
    }

    /// Mark a task `failed`, recording the error, and drop any proposal staged
    /// under it — like `remove_task`, a task that will never be resolved must
    /// not leave its session behind to wedge the queue.
    pub fn fail_task(&self, id: Uuid, error: &str) -> Result<Task> {
        if self.staged_row()?.is_some_and(|r| r.task_id == Some(id)) {
            self.discard_staged()?;
        }
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
                    rotation: 0,
                    color_transfer: None,
                    color_primaries: None,
                    pix_fmt: None,
                    color_space: None,
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
                },
            ],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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

    /// Caption the cut: project every clip's cached transcript through the edit
    /// and write the result as text overlays, replacing any previous generated
    /// set.
    ///
    /// This is deliberately timeline-scoped rather than asset-scoped. A
    /// transcript is in source time and an overlay is in timeline time, and the
    /// two only agree on an untouched asset sitting at zero — which is not a cut.
    /// The moment anything is trimmed, reordered, retimed or (most of all)
    /// silence-removed, source time and timeline time diverge and every caption
    /// is on the wrong word. [`Timeline::captions`] does the projection, so
    /// captions follow the cut and words that were cut out get none.
    ///
    /// `opts.style` picks the look — a held subtitle line, or one word at a
    /// time — and everything else in [`CaptionOptions`] is an override on top
    /// of it.
    ///
    /// Errors when nothing on the timeline has a transcript to caption, rather
    /// than quietly writing no overlays.
    pub fn generate_captions(&self, opts: CaptionOptions) -> Result<Vec<TextOverlay>> {
        let timeline = self.working_timeline()?;
        let mut transcripts: HashMap<Uuid, Vec<TranscriptSegment>> = HashMap::new();
        let mut analyzed = false;
        for track in &timeline.tracks {
            for clip in &track.clips {
                if transcripts.contains_key(&clip.asset_id) {
                    continue;
                }
                let Some(analysis) = self.get_analysis(clip.asset_id)? else {
                    continue;
                };
                analyzed = true;
                transcripts.insert(clip.asset_id, analysis.transcript);
            }
        }
        if !analyzed {
            return Err(Error::InvalidArgument(
                "no analysis available for the clips on the timeline; run analysis first".to_string(),
            ));
        }
        let overlays = timeline.captions(&transcripts, opts);
        if overlays.is_empty() {
            return Err(Error::InvalidArgument(
                "no speech was transcribed for the footage in this cut".to_string(),
            ));
        }
        let created = overlays.clone();
        // Name the style in the edit log: recaptioning in the other one is a
        // different edit, and the history is where that has to be visible.
        let label = match opts.style {
            CaptionStyle::Lines => "Generate captions",
            CaptionStyle::WordPunch => "Generate word captions",
        };
        self.edit_timeline(label, move |timeline| {
            timeline.overlays.retain(|o| !o.generated);
            timeline.overlays.extend(overlays);
            Ok(())
        })?;
        Ok(created)
    }

    /// Caption the cut from a parsed subtitle file: lay its cues onto the cut and
    /// write them as caption overlays — as **one** `Import captions` revision.
    ///
    /// Taking the [`CaptionFile`] rather than text is the lock-free split again:
    /// [`parse_captions`](crate::captions_import::parse_captions) (and
    /// [`read_caption_file`](crate::captions_import::read_caption_file) before it)
    /// are pure and run with the project lock released, because reading a file —
    /// a few MB at the worst — must not stall an edit; this method only places
    /// what was read.
    ///
    /// `req.base` says which clock the file's times are on. The default,
    /// [`CaptionTimeBase::Timeline`], is a subtitle file made for the finished
    /// cut; [`CaptionTimeBase::Source`] is a file that times one asset's own
    /// footage (a transcript, subtitles for the uncut source) and is projected
    /// through that asset's clips exactly as a transcript is, so it follows trims,
    /// reorders and speed changes. `req.offset` is added to every cue first
    /// (seconds, either sign: a broadcast file that starts at `01:00:00` wants
    /// `-3600`). [`Timeline::place_cues`] does the placement with the same
    /// chunking, flicker floors, one-lane rule and frame fit as
    /// [`Project::generate_captions`], in the style `req.options` names.
    ///
    /// **The imported set is the caption set.** Its overlays are `generated`, so
    /// `Recaption` / `Clear captions` / the delivery re-fit treat them like any
    /// other captions — and that means importing *replaces* the previous
    /// generated or imported captions (the summary's `replaced` says how many),
    /// and a later [`Project::generate_captions`] replaces the imported ones.
    /// Captions are one lane of text at one position; keeping two sets would put
    /// two on screen at once. Typed titles are never touched.
    ///
    /// Honors the staging session like any edit: an agent's import lands in its
    /// proposal. Errors — and writes nothing, so the existing captions survive —
    /// when the file holds no cues, none of them can be shown, the offset is not
    /// a sensible number, or the result would be more than
    /// [`MAX_IMPORTED_CAPTIONS`] captions.
    pub fn import_captions(&self, file: &CaptionFile, req: CaptionImportRequest) -> Result<ImportSummary> {
        if file.cues.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "no captions found in the {} text ({} entries could not be read)",
                file.format.as_str().to_uppercase(),
                file.skipped
            )));
        }
        if !req.offset.is_finite() || req.offset.abs() > MAX_CAPTION_OFFSET {
            return Err(Error::InvalidArgument(format!(
                "offset must be a number of seconds within ±{MAX_CAPTION_OFFSET}"
            )));
        }
        if let CaptionTimeBase::Source(asset) = req.base {
            self.require_asset(asset)?;
        }
        let cues: Vec<TranscriptSegment> = file
            .cues
            .iter()
            .map(|c| TranscriptSegment {
                start: c.start + req.offset,
                end: c.end + req.offset,
                text: c.text.clone(),
            })
            .collect();
        let (first, last) = (cues[0].start, cues.iter().map(|c| c.end).fold(f64::MIN, f64::max));
        // The placement is computed from the very timeline the edit then writes
        // to — the proposal for an agent that is staging — so there is no gap
        // between looking at the cut and changing it.
        let (placement, captions, replaced) = self.edit_timeline("Import captions", |timeline| {
            if let CaptionTimeBase::Source(asset) = req.base {
                if !timeline.tracks.iter().flat_map(|t| &t.clips).any(|c| c.asset_id == asset) {
                    return Err(Error::InvalidArgument(format!(
                        "asset {asset} is not used by any clip in the cut; put it on the timeline first, \
                         or import with base \"timeline\" if the file already times the finished cut"
                    )));
                }
            }
            let mut placement = timeline.place_cues(&cues, req.base, req.options);
            if placement.overlays.is_empty() {
                let run = format!("they run {} to {}", fmt_time(first), fmt_time(last));
                return Err(Error::InvalidArgument(if placement.dropped_outside == cues.len() {
                    match req.base {
                        CaptionTimeBase::Timeline => format!(
                            "none of the {} cues fall inside the cut: {run} and the cut is {} long",
                            cues.len(),
                            fmt_time(timeline.for_render().duration()),
                        ),
                        CaptionTimeBase::Source(_) => format!(
                            "none of the {} cues land on footage this asset shows in the cut: {run} of the \
                             source (a muted track or a disabled clip does not count)",
                            cues.len(),
                        ),
                    }
                } else {
                    format!(
                        "none of the {} cues could be shown: {} outside the cut, {} too short to read, {} hidden by another",
                        cues.len(),
                        placement.dropped_outside,
                        placement.dropped_short,
                        placement.dropped_overlap,
                    )
                }));
            }
            if placement.overlays.len() > MAX_IMPORTED_CAPTIONS {
                return Err(Error::InvalidArgument(format!(
                    "this import would write {} captions; Kerf writes at most {MAX_IMPORTED_CAPTIONS} at a time \
                     (the `word_punch` look makes one caption per word — `lines` makes far fewer)",
                    placement.overlays.len()
                )));
            }
            let before = timeline.overlays.len();
            timeline.overlays.retain(|o| !o.generated);
            let replaced = before - timeline.overlays.len();
            let captions = placement.overlays.len();
            timeline.overlays.append(&mut placement.overlays);
            Ok((placement, captions, replaced))
        })?;
        Ok(ImportSummary {
            format: file.format,
            cues: cues.len(),
            placed: placement.placed,
            captions,
            skipped_lines: file.skipped,
            dropped_outside: placement.dropped_outside,
            dropped_short: placement.dropped_short,
            dropped_overlap: placement.dropped_overlap,
            replaced,
        })
    }

    /// Read `text` aloud and describe the result as an importable [`Asset`],
    /// *without* `&self` — synthesis runs for seconds to minutes (and may first
    /// download the voice model), so like [`Project::probe_import`] it happens
    /// with the project lock released; [`Project::place_voiceover`] then takes
    /// it only to land the result.
    ///
    /// The asset carries its [`Voiceover`], script and exact sentence timings
    /// included, which is what lets it be captioned without transcribing it.
    pub fn synthesize_voiceover(
        text: &str,
        voice: &str,
        speed: f64,
        progress: engine::tts::ProgressFn,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Asset> {
        let synthesis = engine::tts::synthesize(text, voice, speed, progress, cancel)?;
        let mut asset = Self::probe_asset(&synthesis.path)?;
        asset.name = voiceover_name(text);
        asset.voiceover = Some(Voiceover {
            text: text.trim().to_string(),
            voice: voice.to_string(),
            speed: speed.clamp(engine::tts::MIN_SPEED, engine::tts::MAX_SPEED),
            segments: synthesis.segments,
        });
        Ok(asset)
    }

    /// Land a synthesized voiceover: store the asset (reusing it when the same
    /// script was generated before), record its script as the asset's
    /// transcript, and put it on the timeline as one edit.
    ///
    /// With no `track_id` it goes on the audio track named [`VOICEOVER_TRACK`],
    /// which is created on first use — narration kept on its own lane, never
    /// dropped on top of the dialogue or the music bed on `A1`. With no
    /// `timeline_start` it is appended after whatever that track already holds.
    pub fn place_voiceover(&self, asset: &Asset, track_id: Option<Uuid>, timeline_start: Option<f64>) -> Result<(Asset, Clip)> {
        let asset = self.insert_or_get_asset(asset)?;
        if let Some(voiceover) = &asset.voiceover {
            // Keep whatever an earlier analysis measured; only the transcript is
            // ours to write.
            let mut analysis = self.get_analysis(asset.id)?.unwrap_or_else(|| AssetAnalysis {
                asset_id: asset.id,
                ..AssetAnalysis::default()
            });
            analysis.transcript = voiceover.segments.clone();
            self.set_analysis(&analysis)?;
        }
        let clip = self.edit_timeline("Add voiceover", |timeline| {
            let tid = match track_id {
                Some(t) => {
                    let track = timeline.track(t).ok_or(Error::TrackNotFound(t))?;
                    if track.kind != StreamKind::Audio {
                        return Err(Error::InvalidArgument("a voiceover goes on an audio track".to_string()));
                    }
                    t
                }
                None => match timeline
                    .tracks
                    .iter()
                    .find(|t| t.kind == StreamKind::Audio && t.name == VOICEOVER_TRACK)
                {
                    Some(track) => track.id,
                    None => {
                        let track = Track::new(StreamKind::Audio, VOICEOVER_TRACK.to_string());
                        let id = track.id;
                        timeline.tracks.push(track);
                        id
                    }
                },
            };
            let start = timeline_start
                .map(|t| t.max(0.0))
                .unwrap_or_else(|| timeline.track(tid).map(Track::end).unwrap_or(0.0));
            let clip = Clip::for_asset(&asset, 0.0, asset.duration, start);
            timeline
                .track_mut(tid)
                .expect("track resolved above")
                .clips
                .push(clip.clone());
            Ok(clip)
        })?;
        Ok((asset, clip))
    }

    /// Remove the captions [`Project::generate_captions`] wrote, leaving titles
    /// and lower-thirds alone.
    pub fn clear_captions(&self) -> Result<usize> {
        self.edit_timeline("Clear captions", move |timeline| {
            let before = timeline.overlays.len();
            timeline.overlays.retain(|o| !o.generated);
            Ok(before - timeline.overlays.len())
        })
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

/// The `assets` columns every asset read selects, in [`read_asset_row`]'s order.
const ASSET_COLUMNS: &str = "id, path, name, duration, streams, imported_at, source_paths, voiceover";

/// An `assets` row as stored, JSON columns still serialized.
struct AssetRow {
    id: String,
    path: String,
    name: String,
    duration: f64,
    streams: String,
    imported_at: String,
    source_paths: Option<String>,
    voiceover: Option<String>,
}

fn read_asset_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRow> {
    Ok(AssetRow {
        id: row.get(0)?,
        path: row.get(1)?,
        name: row.get(2)?,
        duration: row.get(3)?,
        streams: row.get(4)?,
        imported_at: row.get(5)?,
        source_paths: row.get(6)?,
        voiceover: row.get(7)?,
    })
}

fn row_to_asset(row: AssetRow) -> Result<Asset> {
    Ok(Asset {
        id: parse_uuid(&row.id)?,
        path: row.path,
        name: row.name,
        duration: row.duration,
        streams: serde_json::from_str(&row.streams)?,
        imported_at: parse_dt(&row.imported_at)?,
        source_paths: match row.source_paths {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        },
        voiceover: row.voiceover.map(|json| serde_json::from_str(&json)).transpose()?,
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
mod linked_tests;

/// The Rust half of the differential corpus the browser harness is replayed against.
#[cfg(test)]
mod linked_corpus;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions_import::{parse_captions, CaptionFormat, CaptionImportRequest, ImportSummary, MAX_IMPORTED_CAPTIONS};
    use crate::model::{ClipMove, DiffKind, Fit};

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
    fn platform_check_reads_the_cut_the_export_would_render() {
        let project = Project::sample().unwrap();
        // Cut for a vertical feed: the frame the checks compare against is the
        // project's delivery format, not the footage's shape.
        project
            .set_delivery_format(Some(crate::model::Delivery {
                width: 1080,
                height: 1920,
                fit: Fit::Cover,
            }))
            .unwrap();
        let summary = project.cut_summary(None).unwrap();
        assert_eq!((summary.width, summary.height), (1080, 1920));
        assert!(summary.duration > 0.0);
        assert!(summary.has_audio, "the sample has audio-bearing clips");

        let checks = project.platform_check(None).unwrap();
        assert_eq!(checks.len(), crate::platform::TARGETS.len());
        let reels = checks.iter().find(|c| c.target == "reels").unwrap();
        assert!(reels.ok, "a short vertical cut is publishable: {:?}", reels.issues);
        // The same cut is the wrong shape for a landscape player, and says so.
        let youtube = checks.iter().find(|c| c.target == "youtube").unwrap();
        assert!(youtube
            .issues
            .iter()
            .any(|i| i.severity == crate::platform::Severity::Warning && i.message.contains("letterboxed")));
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
    fn a_waveform_range_needs_an_asset_with_audio() {
        let project = Project::open_in_memory().unwrap();
        // Nothing here exists on disk: both refusals must come before any
        // ffmpeg is spawned, and say what is wrong.
        let silent = project
            .insert_or_get_asset(&asset_with("/nowhere/silent.mp4", vec![vid_stream(false)]))
            .unwrap();
        let err = project.waveform_range(silent.id, 0.0, 1.0, 100).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
        assert!(err.to_string().contains("no audio stream"), "{err}");
        let still = project
            .insert_or_get_asset(&asset_with("/nowhere/still.png", vec![vid_stream(true)]))
            .unwrap();
        assert!(matches!(
            project.waveform_range(still.id, 0.0, 1.0, 100),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            project.waveform_range(Uuid::new_v4(), 0.0, 1.0, 100),
            Err(Error::AssetNotFound(_))
        ));
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored waveform_range_reads`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn waveform_range_reads_an_imported_assets_peaks_without_the_project_lock() {
        use crate::engine::test_support::StatusBounded;
        let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let dir = std::env::temp_dir().join(format!("kerf-wave-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        // Left: a 0.5 sine; right: silence. Two seconds.
        let made = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("aevalsrc='0.5*sin(2*PI*440*t)|0':s=48000:d=2:c=stereo")
            .args(["-c:a", "pcm_f32le"])
            .arg(&wav)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());

        let project = Project::open_in_memory().unwrap();
        let asset = project.import_asset(&wav).unwrap();
        let range = project.waveform_range(asset.id, 0.0, 2.0, 40).unwrap();
        assert_eq!(
            (range.channels, range.buckets, range.min.len(), range.max.len()),
            (2, 40, 2, 2)
        );
        assert!(range.max[0].iter().all(|v| (0.45..=0.51).contains(v)), "{:?}", range.max[0]);
        assert!(range.min[1].iter().chain(&range.max[1]).all(|v| v.abs() < 1e-4));
        assert!((range.duration - 2.0).abs() < 0.01);

        // The lock-free static a surface calls after dropping the project lock
        // reads the same answer (from the memo this time).
        let again = Project::decode_waveform_range(&asset, 0.0, 2.0, 40).unwrap();
        assert_eq!(again, range);
        let _ = std::fs::remove_dir_all(&dir);
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

    fn voiceover_asset(path: &str) -> Asset {
        let mut asset = asset_with(path, vec![aud_stream()]);
        asset.duration = 4.0;
        asset.voiceover = Some(Voiceover {
            text: "Welcome to the shop. We open at nine.".into(),
            voice: "af_heart".into(),
            speed: 1.0,
            segments: vec![
                TranscriptSegment {
                    start: 0.0,
                    end: 1.6,
                    text: "Welcome to the shop.".into(),
                },
                TranscriptSegment {
                    start: 1.85,
                    end: 4.0,
                    text: "We open at nine.".into(),
                },
            ],
        });
        asset
    }

    #[test]
    fn a_voiceover_lands_on_its_own_track_and_captions_from_its_script() {
        let project = Project::open_in_memory().unwrap();
        let (asset, clip) = project
            .place_voiceover(&voiceover_asset("/vo/a.wav"), None, Some(2.0))
            .unwrap();
        let timeline = project.timeline().unwrap();
        let vo = timeline.tracks.iter().find(|t| t.name == VOICEOVER_TRACK).expect("VO track");
        assert_eq!(vo.kind, StreamKind::Audio);
        assert_eq!(vo.clips.len(), 1);
        assert_eq!((clip.timeline_start, clip.duration()), (2.0, 4.0));
        // The script is the transcript: no analysis pass, no speech model.
        assert_eq!(project.get_analysis(asset.id).unwrap().unwrap().transcript.len(), 2);

        let captions = project.generate_captions(CaptionOptions::default()).unwrap();
        assert!(!captions.is_empty());
        assert!(
            (captions[0].start - 2.0).abs() < 1e-9,
            "captions follow the clip onto the timeline"
        );
        assert!(captions.iter().all(|c| c.end <= 6.0 + 1e-9));

        // A second voiceover joins the same lane, after the first.
        let (_, second) = project.place_voiceover(&voiceover_asset("/vo/b.wav"), None, None).unwrap();
        let timeline = project.timeline().unwrap();
        assert_eq!(timeline.tracks.iter().filter(|t| t.name == VOICEOVER_TRACK).count(), 1);
        assert_eq!(second.timeline_start, 6.0);
    }

    #[test]
    fn a_voiceover_refuses_a_video_track() {
        let project = Project::open_in_memory().unwrap();
        let v1 = project.timeline().unwrap().first_track_of(StreamKind::Video).unwrap();
        assert!(project
            .place_voiceover(&voiceover_asset("/vo/a.wav"), Some(v1), None)
            .is_err());
    }

    #[test]
    fn voiceover_provenance_survives_a_save_and_reopen() {
        let project = Project::open_in_memory().unwrap();
        let asset = voiceover_asset("/vo/a.wav");
        project.insert_asset(&asset).unwrap();
        let path = std::env::temp_dir().join(format!("kerf-voiceover-{}.kerf", Uuid::new_v4()));
        project.save_as(&path).unwrap();
        let loaded = Project::open(&path).unwrap().require_asset(asset.id).unwrap();
        let voiceover = loaded.voiceover.expect("voiceover survives");
        assert_eq!(voiceover.voice, "af_heart");
        assert_eq!(voiceover.segments.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_older_file_gains_the_voiceover_column() {
        let path = std::env::temp_dir().join(format!("kerf-pre-voiceover-{}.kerf", Uuid::new_v4()));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE assets (id TEXT PRIMARY KEY, path TEXT NOT NULL, name TEXT NOT NULL,
                 duration REAL NOT NULL, streams TEXT NOT NULL, imported_at TEXT NOT NULL, source_paths TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO assets VALUES (?1, '/m.mp4', 'm', 1.0, '[]', ?2, NULL)",
                params![Uuid::new_v4().to_string(), Utc::now().to_rfc3339()],
            )
            .unwrap();
        }
        let project = Project::open(&path).unwrap();
        let assets = project.list_assets().unwrap();
        assert_eq!(assets.len(), 1);
        assert!(assets[0].voiceover.is_none());
        let _ = std::fs::remove_file(&path);
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
            voiceover: None,
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
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
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
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
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
        project.add_asset_audio(music.id).unwrap();
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
    fn a_filmstrip_needs_an_asset_with_video() {
        let project = Project::open_in_memory().unwrap();
        // Nothing here exists on disk: the refusal must come before any ffmpeg is
        // spawned, and say what is wrong.
        let voice = project
            .insert_or_get_asset(&asset_with("/nowhere/voice.wav", vec![aud_stream()]))
            .unwrap();
        let err = project.filmstrip(voice.id).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
        assert!(err.to_string().contains("no video stream"), "{err}");
        assert!(matches!(project.filmstrip(Uuid::new_v4()), Err(Error::AssetNotFound(_))));
    }

    #[test]
    fn a_filmstrip_decodes_the_proxy_only_once_it_exists() {
        let project = Project::open_in_memory().unwrap();
        // A unique per-process source path keeps the deterministic proxy path
        // distinct across concurrent test runs (no shared-file race).
        let path = format!("/kerf-test-filmstrip-source-{}.mp4", std::process::id());
        let asset = project
            .insert_or_get_asset(&asset_with(&path, vec![vid_stream(false)]))
            .unwrap();
        assert_eq!(
            Project::filmstrip_proxy(&asset),
            None,
            "no proxy yet: the original is decoded"
        );

        let width = crate::engine::proxy_width(asset.projection());
        let Some(proxy_file) = crate::engine::proxy_path(Path::new(&asset.path), width) else {
            return; // no cache dir on this platform — nothing to resolve to
        };
        if let Some(dir) = proxy_file.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&proxy_file, b"stub").unwrap();
        let resolved = Project::filmstrip_proxy(&asset);
        let _ = std::fs::remove_file(&proxy_file);
        assert_eq!(resolved, Some(proxy_file), "the ready proxy is preferred");

        // A still never has a proxy, so it resolves to the original.
        let still_path = format!("/kerf-test-filmstrip-still-{}.png", std::process::id());
        let still = project
            .insert_or_get_asset(&asset_with(&still_path, vec![vid_stream(true)]))
            .unwrap();
        assert_eq!(Project::filmstrip_proxy(&still), None);
    }

    /// `cargo test -p kerf-core --no-default-features -- --ignored filmstrip_of_an_imported`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn filmstrip_of_an_imported_asset_is_built_without_the_project_lock_and_then_cached() {
        use crate::engine::test_support::StatusBounded;
        let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let dir = std::env::temp_dir().join(format!("kerf-filmstrip-project-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        let made = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=640x360:rate=25:duration=6")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());

        let project = Project::open_in_memory().unwrap();
        let asset = project.import_asset(&clip).unwrap();
        let strip = project.filmstrip(asset.id).unwrap();
        // 6 s at 0.5 s; 640x360 is 16:9 -> 170x96, 12 of them on one sheet.
        assert_eq!(
            (
                strip.interval,
                strip.frames,
                strip.frame_width,
                strip.frame_height,
                strip.sheets.len()
            ),
            (0.5, 12, 170, 96, 1)
        );
        assert!(strip.sheets[0].jpeg.starts_with(&[0xFF, 0xD8]), "a JPEG");

        // The surface's shape: resolve the asset under the lock, decode with it
        // released. The second ask is the memoized strip, not a second decode.
        let resolved = project.require_asset(asset.id).unwrap();
        let again = Project::decode_filmstrip(&resolved).unwrap();
        assert!(std::sync::Arc::ptr_eq(&strip, &again));
        // The strip went to the real cache under a path unique to this run.
        if let Some(entry) = crate::engine::cached_filmstrip_dir(&resolved) {
            assert!(entry.join("manifest.json").is_file(), "{entry:?}");
            let _ = std::fs::remove_dir_all(entry);
        }
        let _ = std::fs::remove_dir_all(&dir);
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
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: None,
                color_space: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: None,
                color_space: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: None,
                color_space: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: None,
                color_space: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: None,
                color_space: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: None,
                color_space: None,
            }],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
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
    fn claiming_a_second_task_refuses_while_the_first_is_still_staged() {
        let mut project = Project::open_in_memory().unwrap();
        let first = project.add_task("tighten the intro").unwrap();
        let second = project.add_task("caption the cut").unwrap();
        project.set_actor(EditSource::Agent);

        let claimed = project.claim_next_task().unwrap().unwrap();
        assert_eq!(claimed.id, first.id);

        // The second task must not fold its edits into the first task's
        // session — it stays queued rather than sharing another task's proposal.
        assert!(matches!(project.claim_next_task(), Err(Error::StagedEditPending)));
        assert_eq!(project.require_task(second.id).unwrap().status, TaskStatus::Queued);
        let staged = project.staged().unwrap().unwrap();
        assert_eq!(staged.task_id, Some(first.id));
    }

    #[test]
    fn failing_a_task_discards_its_staged_edits_and_frees_the_queue() {
        let mut project = Project::open_in_memory().unwrap();
        let first = project.add_task("tighten the intro").unwrap();
        let second = project.add_task("caption the cut").unwrap();
        project.set_actor(EditSource::Agent);

        let claimed = project.claim_next_task().unwrap().unwrap();
        assert_eq!(claimed.id, first.id);
        project.add_track(StreamKind::Video, Some("B-roll".to_string())).unwrap();

        project.fail_task(first.id, "could not find a good cut").unwrap();
        assert!(project.staged().unwrap().is_none(), "the orphaned session is cleaned up");
        assert_eq!(project.timeline().unwrap().tracks.len(), 2, "its staged edit never landed");

        // The queue is not wedged: the next claim opens its own session.
        let claimed = project.claim_next_task().unwrap().unwrap();
        assert_eq!(claimed.id, second.id);
        let staged = project.staged().unwrap().unwrap();
        assert_eq!(staged.task_id, Some(second.id));
    }

    #[test]
    fn claim_next_task_clears_a_session_orphaned_by_an_already_terminal_task() {
        // A session left behind by a task that somehow went terminal without
        // going through `fail_task`/`remove_task` (or one from a build predating
        // that cleanup) must not wedge the queue forever.
        let mut project = Project::open_in_memory().unwrap();
        let first = project.add_task("tighten the intro").unwrap();
        let second = project.add_task("caption the cut").unwrap();
        project.set_actor(EditSource::Agent);
        project.begin_staging(Some(first.id), None).unwrap();
        project.complete_task(first.id, Some("done".to_string())).unwrap();
        project.resolve_task(first.id).unwrap();
        // `resolve_task` already applies/clears its own session; stage a fresh
        // one directly against the now-`Done` task to simulate the orphan.
        project.begin_staging(Some(first.id), None).unwrap();

        let claimed = project.claim_next_task().unwrap().unwrap();
        assert_eq!(claimed.id, second.id);
        let staged = project.staged().unwrap().unwrap();
        assert_eq!(staged.task_id, Some(second.id));
    }

    #[test]
    fn a_recycled_seq_from_undo_then_edit_is_still_caught_as_stale() {
        let mut project = Project::sample().unwrap();
        let clips: Vec<Clip> = project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .flat_map(|t| t.clips.clone())
            .collect();

        // Advance the head once so there is somewhere for undo to land.
        project.set_volume(clips[0].id, 0.5).unwrap();
        let base_seq = project.history().unwrap().last().unwrap().seq;

        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None).unwrap();
        project.set_volume(clips[0].id, 0.3).unwrap();
        assert_eq!(project.staged().unwrap().unwrap().base_seq, base_seq);

        // The user undoes underneath the proposal, then makes an unrelated
        // edit — `record_revision` prunes the undone row and reinserts a
        // *different* one at the same seq the proposal was staged from.
        project.set_actor(EditSource::User);
        project.undo().unwrap();
        project.set_volume(clips[1].id, 0.9).unwrap();
        project.set_actor(EditSource::Agent);

        assert_eq!(
            project.history().unwrap().last().unwrap().seq,
            base_seq,
            "the seq was recycled"
        );
        assert!(project.staged().unwrap().unwrap().stale);
        assert!(matches!(project.apply_staged(false), Err(Error::StagedEditStale)));

        // Forcing it still works, same as any other staleness.
        let applied = project.apply_staged(true).unwrap();
        assert_eq!(applied.clip(clips[0].id).unwrap().volume, 0.3);
        assert_eq!(applied.clip(clips[1].id).unwrap().volume, 1.0);
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

    // ---- smart crop ---------------------------------------------------------

    #[test]
    fn smart_crop_has_nothing_to_do_when_the_footage_is_already_the_frame() {
        let project = Project::sample().unwrap();
        // The sample is 1920x1080 with no explicit delivery frame, so the frame
        // is derived from the footage and every shot already fills it.
        let err = project.smart_crop_inputs(None).unwrap_err().to_string();
        assert!(err.contains("already"), "{err}");
    }

    #[test]
    fn smart_crop_plans_every_video_clip_for_a_vertical_delivery() {
        let project = Project::sample().unwrap();
        project
            .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
            .unwrap();
        let plan = project.smart_crop_inputs(None).unwrap();
        assert!((plan.target_aspect - 1080.0 / 1920.0).abs() < 1e-9);
        let video_clips: usize = project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .map(|t| t.clips.len())
            .sum();
        assert_eq!(plan.jobs.len(), video_clips);
        // Each job points at real media over the clip's own source window.
        for job in &plan.jobs {
            assert!(job.path.to_string_lossy().ends_with(".mp4"));
            assert!(job.end > job.start);
            // Both sample sources are 16:9 — 1080p and 4K.
            assert!((job.width as f64 / job.height as f64 - 16.0 / 9.0).abs() < 1e-6);
        }
    }

    #[test]
    fn smart_crop_can_be_narrowed_to_one_clip() {
        let project = Project::sample().unwrap();
        project
            .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
            .unwrap();
        let clip = first_video_clip(&project);
        let plan = project.smart_crop_inputs(Some(clip)).unwrap();
        assert_eq!(plan.jobs.len(), 1);
        assert_eq!(plan.jobs[0].clip_id, clip);
    }

    #[test]
    fn smart_crop_skips_a_locked_track_but_not_a_clip_named_on_one() {
        let project = Project::sample().unwrap();
        project
            .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
            .unwrap();
        let clip = first_video_clip(&project);
        let track = project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.clips.iter().any(|c| c.id == clip))
            .unwrap()
            .id;
        project.set_track_locked(track, true).unwrap();
        // The bulk pass leaves the locked track alone...
        let bulk = project.smart_crop_inputs(None);
        assert!(!bulk.map(|p| p.jobs.iter().any(|j| j.clip_id == clip)).unwrap_or(false));
        // ...but naming one of its clips is an explicit instruction.
        assert_eq!(project.smart_crop_inputs(Some(clip)).unwrap().jobs.len(), 1);
    }

    #[test]
    fn applying_crops_is_one_undoable_edit_that_only_counts_real_changes() {
        let project = Project::sample().unwrap();
        project
            .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
            .unwrap();
        let clip = first_video_clip(&project);
        let crop = CropFrame {
            left: 0.1,
            right: 0.5836,
            top: 0.0,
            bottom: 0.0,
            offset: -0.6,
        };
        let before = project.history().unwrap().len();
        assert_eq!(project.apply_smart_crops(&[(clip, crop)]).unwrap(), 1);
        let t = clip_transform(&project, clip);
        assert!((t.crop_left - 0.1).abs() < 1e-9 && (t.crop_right - 0.5836).abs() < 1e-9);
        assert_eq!(project.history().unwrap().len(), before + 1);
        assert_eq!(project.history().unwrap().last().unwrap().label, "Smart crop");

        // Re-running the same pass changes nothing, and leaves no revision behind.
        assert_eq!(project.apply_smart_crops(&[(clip, crop)]).unwrap(), 0);
        assert_eq!(project.history().unwrap().len(), before + 1);

        // The whole pass undoes in one step.
        project.undo().unwrap();
        assert_eq!(clip_transform(&project, clip).crop_left, 0.0);
    }

    #[test]
    fn applying_no_crops_writes_no_revision() {
        let project = Project::sample().unwrap();
        let before = project.history().unwrap().len();
        assert_eq!(project.apply_smart_crops(&[]).unwrap(), 0);
        assert_eq!(project.history().unwrap().len(), before);
    }

    #[test]
    fn framing_plans_every_shape_but_the_projects_own() {
        let project = Project::sample().unwrap();
        project
            .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
            .unwrap();
        let plan = project
            .framing_inputs(&[
                Delivery::new(1080, 1920, Fit::Cover),
                Delivery::new(1080, 1080, Fit::Cover),
                Delivery::new(720, 1280, Fit::Cover),
                Delivery::new(1920, 1080, Fit::Contain),
            ])
            .unwrap();
        // 9:16 is the project frame (and 720x1280 the same shape); the rest dedupe.
        assert_eq!(plan.ratios, vec![(1, 1), (16, 9)]);
        let video_clips: usize = project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .map(|t| t.clips.len())
            .sum();
        assert_eq!(plan.jobs.len(), video_clips);

        // Only the project's own shape asked for: nothing to frame, not an error.
        let none = project.framing_inputs(&[Delivery::new(1080, 1920, Fit::Cover)]).unwrap();
        assert!(none.ratios.is_empty() && none.jobs.is_empty());
    }

    #[test]
    fn framings_land_as_one_labelled_revision_and_only_when_they_change() {
        let project = Project::sample().unwrap();
        let clip = first_video_clip(&project);
        let crop = CropFrame {
            left: 0.3,
            right: 0.2625,
            top: 0.0,
            bottom: 0.0,
            offset: 0.2,
        };
        let framings = vec![
            (clip, Framing::new((1, 1), &crop)),
            (clip, Framing::new((4, 5), &CropFrame::default())),
        ];
        let before = project.history().unwrap().len();
        assert_eq!(project.apply_framings(&framings).unwrap(), 1, "one clip changed");
        let history = project.history().unwrap();
        assert_eq!(history.len(), before + 1);
        assert_eq!(history.last().unwrap().label, "Frame for 1:1, 4:5");
        let timeline = project.timeline().unwrap();
        let (ti, ci) = timeline.locate(clip).unwrap();
        let framed = &timeline.tracks[ti].clips[ci];
        assert_eq!(framed.framing_for((1, 1)).unwrap().crop_left, 0.3);
        assert_eq!(framed.framing_for((4, 5)).unwrap().crop_left, 0.0);
        // The project frame's own crop is untouched.
        assert!(!framed.transform.has_crop());

        // Same framings again: no change, no revision.
        assert_eq!(project.apply_framings(&framings).unwrap(), 0);
        assert_eq!(project.history().unwrap().len(), before + 1);

        // The whole pass undoes in one step.
        project.undo().unwrap();
        let timeline = project.timeline().unwrap();
        let (ti, ci) = timeline.locate(clip).unwrap();
        assert!(timeline.tracks[ti].clips[ci].framings.is_empty());
    }

    fn first_video_clip(project: &Project) -> Uuid {
        project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| t.clips.iter())
            .next()
            .unwrap()
            .id
    }

    fn clip_transform(project: &Project, clip_id: Uuid) -> crate::model::Transform {
        let timeline = project.timeline().unwrap();
        let (ti, ci) = timeline.locate(clip_id).unwrap();
        timeline.tracks[ti].clips[ci].transform
    }

    #[test]
    fn captions_survive_the_cut_that_silence_removal_makes() {
        let project = Project::sample().unwrap();
        let asset = project.list_assets().unwrap()[0].id;
        // The sample's transcript is two lines over 0..12.5s with a silence at
        // 12.5..14.0; put the asset on the timeline at a non-zero position and
        // trimmed, which is what makes source time and timeline time disagree.
        let timeline = project.timeline().unwrap();
        let track = timeline.tracks[0].id;
        for t in &timeline.tracks {
            for clip in &t.clips {
                project.remove(clip.id).unwrap();
            }
        }
        project
            .add_clip_to_timeline(asset, Some(track), 5.5, 12.5, Some(2.0))
            .unwrap();

        let created = project.generate_captions(CaptionOptions::default()).unwrap();
        assert!(!created.is_empty());
        // Every caption sits inside the clip's timeline span (2.0 .. 9.0), not
        // at the transcript's own 5.5 .. 12.5.
        for o in &created {
            assert!(o.start >= 2.0 - 1e-6 && o.end <= 9.0 + 1e-6, "{o:?} is in source time");
            assert!(o.generated);
        }
        // The line spoken before the in-point was trimmed away, so it gets none.
        assert!(
            !created.iter().any(|o| o.text.contains("Welcome")),
            "captioned words that are not in the cut: {created:?}"
        );

        // Regenerating replaces rather than stacks, and leaves a hand-made
        // title alone.
        let title = project.add_overlay("Chapter one".to_string(), 0.0, 1.5).unwrap();
        let again = project.generate_captions(CaptionOptions::default()).unwrap();
        let overlays = project.timeline().unwrap().overlays;
        assert_eq!(overlays.iter().filter(|o| o.generated).count(), again.len());
        assert!(overlays.iter().any(|o| o.id == title.id), "the typed title was thrown away");

        // Clearing takes only the generated ones.
        let cleared = project.clear_captions().unwrap();
        assert_eq!(cleared, again.len());
        let overlays = project.timeline().unwrap().overlays;
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].id, title.id);
    }

    #[test]
    fn captions_need_something_transcribed_on_the_timeline() {
        let project = Project::open_in_memory().unwrap();
        let err = project.generate_captions(CaptionOptions::default()).unwrap_err();
        assert!(err.to_string().contains("run analysis first"), "{err}");
    }

    const SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nHello there\n\n2\n00:00:04,000 --> 00:00:06,000\nGeneral Kenobi\n";

    /// A project whose timeline is one `len`-second clip of its first asset,
    /// cut from `source_in`, sitting at `at`.
    fn project_with_one_clip(source_in: f64, len: f64, at: f64) -> (Project, Uuid) {
        let project = Project::sample().unwrap();
        let asset = project.list_assets().unwrap()[0].id;
        let timeline = project.timeline().unwrap();
        let track = timeline.tracks[0].id;
        for t in &timeline.tracks {
            for clip in &t.clips {
                project.remove(clip.id).unwrap();
            }
        }
        project
            .add_clip_to_timeline(asset, Some(track), source_in, source_in + len, Some(at))
            .unwrap();
        (project, asset)
    }

    fn captions_on(project: &Project) -> Vec<TextOverlay> {
        project
            .working_timeline()
            .unwrap()
            .overlays
            .into_iter()
            .filter(|o| o.generated)
            .collect()
    }

    /// What the surfaces do, in the order they do it: parse with no project in
    /// hand, then place under the project.
    fn import_with(
        project: &Project,
        text: &str,
        format: Option<CaptionFormat>,
        req: CaptionImportRequest,
    ) -> Result<ImportSummary> {
        let file = parse_captions(text, format)?;
        project.import_captions(&file, req)
    }

    fn import_text(project: &Project, text: &str, base: CaptionTimeBase, options: CaptionOptions) -> Result<ImportSummary> {
        import_with(
            project,
            text,
            None,
            CaptionImportRequest {
                base,
                options,
                offset: 0.0,
            },
        )
    }

    fn import_srt(project: &Project) -> Result<ImportSummary> {
        import_text(project, SRT, CaptionTimeBase::Timeline, CaptionOptions::default())
    }

    #[test]
    fn importing_a_subtitle_file_is_one_revision_of_generated_captions() {
        let (project, _) = project_with_one_clip(0.0, 20.0, 0.0);
        let title = project.add_overlay("Chapter one".to_string(), 0.0, 1.5).unwrap();
        let revisions = project.history().unwrap().len();

        let summary = import_srt(&project).unwrap();
        assert_eq!(
            summary,
            ImportSummary {
                format: CaptionFormat::Srt,
                cues: 2,
                placed: 2,
                captions: 2,
                skipped_lines: 0,
                dropped_outside: 0,
                dropped_short: 0,
                dropped_overlap: 0,
                replaced: 0,
            }
        );
        let history = project.history().unwrap();
        assert_eq!(history.len(), revisions + 1, "one revision, however many captions");
        let last = history.last().unwrap();
        assert_eq!((last.label.as_str(), last.source), ("Import captions", EditSource::User));

        let captions = captions_on(&project);
        assert_eq!(
            captions.iter().map(|o| (o.text.as_str(), o.start, o.end)).collect::<Vec<_>>(),
            [("Hello there", 1.0, 3.0), ("General Kenobi", 4.0, 6.0)]
        );
        assert!(
            project.timeline().unwrap().overlays.iter().any(|o| o.id == title.id),
            "the typed title is untouched"
        );

        // Importing again replaces the set rather than stacking a second one…
        let again = import_text(
            &project,
            SRT,
            CaptionTimeBase::Timeline,
            CaptionOptions::styled(CaptionStyle::WordPunch),
        )
        .unwrap();
        assert_eq!(again.replaced, 2);
        assert_eq!(captions_on(&project).len(), again.captions);
        assert!(captions_on(&project).iter().all(|o| o.bold), "in the style asked for");
        // …and Clear takes imported captions exactly as it takes generated ones.
        assert_eq!(project.clear_captions().unwrap(), again.captions);
        let overlays = project.timeline().unwrap().overlays;
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].id, title.id);
    }

    #[test]
    fn a_later_generate_captions_replaces_an_imported_set() {
        // The documented interaction: captions are one lane, so whichever set was
        // written last is the set.
        let (project, _) = project_with_one_clip(5.5, 7.0, 2.0);
        import_srt(&project).unwrap();
        assert!(captions_on(&project).iter().any(|o| o.text == "Hello there"));
        let generated = project.generate_captions(CaptionOptions::default()).unwrap();
        let now = captions_on(&project);
        assert_eq!(now.len(), generated.len());
        assert!(!now.iter().any(|o| o.text == "Hello there"), "the imported set was replaced");
    }

    #[test]
    fn ass_cues_in_source_time_follow_the_cut() {
        // The asset's 5.5..12.5 sits at 2.0: source time 6.0 is timeline 2.5.
        let (project, asset) = project_with_one_clip(5.5, 7.0, 2.0);
        let ass = "[Script Info]\nTitle: x\n\n[Events]\n\
                   Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
                   Dialogue: 0,0:00:06.00,0:00:08.00,Default,,0,0,0,,Kept\n\
                   Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Trimmed away\n\
                   Dialogue: 0,0:00:30.00,0:00:32.00,Default,,0,0,0,,Past the footage\n\
                   Dialogue: 0,broken\n";
        let summary = import_text(&project, ass, CaptionTimeBase::Source(asset), CaptionOptions::default()).unwrap();
        assert_eq!(summary.format, CaptionFormat::Ass);
        assert_eq!(
            (summary.cues, summary.placed, summary.dropped_outside, summary.skipped_lines),
            (3, 1, 2, 1)
        );
        let captions = captions_on(&project);
        assert_eq!(captions.len(), 1);
        assert_eq!(captions[0].text, "Kept");
        assert!(
            (captions[0].start - 2.5).abs() < 1e-9 && (captions[0].end - 4.5).abs() < 1e-9,
            "{:?}",
            captions[0]
        );
    }

    #[test]
    fn an_offset_moves_every_cue_before_it_is_placed() {
        // A broadcast file whose clock starts at one hour.
        let (project, _) = project_with_one_clip(0.0, 20.0, 0.0);
        let broadcast = "1\n01:00:01,000 --> 01:00:03,000\nHello there\n\n2\n01:00:04,000 --> 01:00:06,000\nGeneral Kenobi\n";
        let req = |offset| CaptionImportRequest {
            offset,
            ..CaptionImportRequest::default()
        };
        let err = import_with(&project, broadcast, None, req(0.0)).unwrap_err();
        assert!(err.to_string().contains("fall inside the cut"), "{err}");
        let summary = import_with(&project, broadcast, None, req(-3600.0)).unwrap();
        assert_eq!((summary.placed, summary.dropped_outside), (2, 0));
        let captions = captions_on(&project);
        assert_eq!(
            captions.iter().map(|o| (o.start, o.end)).collect::<Vec<_>>(),
            [(1.0, 3.0), (4.0, 6.0)]
        );
        // A positive offset shifts the other way, and part of a cue pushed below
        // zero is clipped, the rest dropped.
        let early = "1\n00:00:00,000 --> 00:00:04,000\nStarts at zero\n\n2\n00:00:01,000 --> 00:00:02,000\nBefore it\n";
        let summary = import_with(&project, early, None, req(-2.0)).unwrap();
        assert_eq!((summary.placed, summary.dropped_outside), (1, 1), "{summary:?}");
        assert_eq!(captions_on(&project)[0].start, 0.0);
        // Not a number of seconds, or an absurd one: refused, not applied.
        for bad in [f64::NAN, f64::INFINITY, 1e9, -1e9] {
            let err = import_with(&project, SRT, None, req(bad)).unwrap_err();
            assert!(err.to_string().contains("offset"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_cue_too_short_to_read_is_counted_on_its_own() {
        let (project, _) = project_with_one_clip(0.0, 10.0, 0.0);
        let text = "1\n00:00:01,000 --> 00:00:03,000\nReadable\n\n2\n00:00:04,000 --> 00:00:04,050\nA blink\n\n3\n00:09:00,000 --> 00:09:03,000\nWay past the end\n";
        let summary = import_text(&project, text, CaptionTimeBase::Timeline, CaptionOptions::default()).unwrap();
        assert_eq!(
            (
                summary.cues,
                summary.placed,
                summary.dropped_outside,
                summary.dropped_short,
                summary.dropped_overlap
            ),
            (3, 1, 1, 1, 0)
        );
        assert_eq!(
            summary.cues,
            summary.placed + summary.dropped_outside + summary.dropped_short + summary.dropped_overlap
        );
        // When *nothing* could be shown the error says why, not "outside the cut".
        let err = import_srt_text(&project, "1\n00:00:04,000 --> 00:00:04,050\nA blink\n").unwrap_err();
        assert!(err.to_string().contains("too short to read"), "{err}");
    }

    fn import_srt_text(project: &Project, text: &str) -> Result<ImportSummary> {
        import_text(project, text, CaptionTimeBase::Timeline, CaptionOptions::default())
    }

    #[test]
    fn an_import_that_would_write_too_many_captions_is_refused_whole() {
        // 9,000 five-word cues in word punch: 45,000 one-word captions. The cut is
        // empty, so the cues are not clipped to anything — which is exactly when
        // an unbounded import used to write all of them.
        let project = Project::open_in_memory().unwrap();
        let text: String = (0..9_000)
            .map(|i| {
                let (a, b) = (i as f64 * 1.5, i as f64 * 1.5 + 1.0);
                let ts = |t: f64| {
                    format!(
                        "{:02}:{:02}:{:02},000",
                        (t / 3600.0) as u32,
                        (t as u32 / 60) % 60,
                        t as u32 % 60
                    )
                };
                format!("{i}\n{} --> {}\nalpha bravo charlie delta echo\n\n", ts(a), ts(b))
            })
            .collect();
        let revisions = project.history().unwrap().len();
        let err = import_text(
            &project,
            &text,
            CaptionTimeBase::Timeline,
            CaptionOptions::styled(CaptionStyle::WordPunch),
        )
        .unwrap_err();
        assert!(err.to_string().contains("at most 20000 at a time"), "{err}");
        assert!(project.timeline().unwrap().overlays.is_empty());
        assert_eq!(project.history().unwrap().len(), revisions);
        // The same file in the lines look is a few captions per cue and lands.
        let summary = import_text(&project, &text, CaptionTimeBase::Timeline, CaptionOptions::default()).unwrap();
        assert!(
            summary.captions <= MAX_IMPORTED_CAPTIONS && summary.placed > 8_000,
            "{summary:?}"
        );
    }

    #[test]
    fn cues_on_an_empty_timeline_are_bounded_by_the_day_not_by_the_file() {
        let project = Project::open_in_memory().unwrap();
        let text = "1\n00:00:01,000 --> 00:00:03,000\nToday\n\n2\n30:00:00,000 --> 30:00:03,000\nA day and a bit on\n";
        let summary = import_text(&project, text, CaptionTimeBase::Timeline, CaptionOptions::default()).unwrap();
        assert_eq!((summary.placed, summary.dropped_outside), (1, 1), "{summary:?}");
    }

    #[test]
    fn an_import_that_cannot_land_writes_nothing_and_keeps_the_captions_it_would_replace() {
        let (project, asset) = project_with_one_clip(0.0, 10.0, 0.0);
        import_srt(&project).unwrap();
        let before = project.timeline().unwrap();
        let revisions = project.history().unwrap().len();
        let refused = |result: Result<ImportSummary>, needle: &str| {
            let err = result.unwrap_err();
            assert!(err.to_string().contains(needle), "{err}");
        };
        let at_timeline = |text: &str| import_text(&project, text, CaptionTimeBase::Timeline, CaptionOptions::default());
        let at_source =
            |text: &str, asset| import_text(&project, text, CaptionTimeBase::Source(asset), CaptionOptions::default());

        // Nothing to read.
        refused(at_timeline("not a subtitle file"), "no captions found");
        // Every cue after the cut ends.
        refused(
            at_timeline("1\n00:05:00,000 --> 00:05:02,000\nToo late\n"),
            "fall inside the cut",
        );
        // An asset that is not in the project, and one that is but not in the cut.
        let missing = Uuid::new_v4();
        assert!(matches!(at_source(SRT, missing), Err(Error::AssetNotFound(id)) if id == missing));
        let unused = project
            .list_assets()
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .find(|id| *id != asset)
            .expect("a second sample asset");
        refused(at_source(SRT, unused), "not used by any clip");
        // Source time the clips do not show.
        refused(
            at_source("1\n00:09:00,000 --> 00:09:02,000\nElsewhere\n", asset),
            "land on footage",
        );
        // An ASS file with an unusable Format line.
        refused(
            at_timeline("[Events]\nFormat: Layer, Text\nDialogue: 0,hi"),
            "Start, End and Text",
        );

        assert_eq!(project.history().unwrap().len(), revisions, "a refused import is not an edit");
        assert_eq!(
            serde_json::to_string(&project.timeline().unwrap()).unwrap(),
            serde_json::to_string(&before).unwrap()
        );
    }

    #[test]
    fn a_named_format_overrides_the_guess() {
        let (project, _) = project_with_one_clip(0.0, 10.0, 0.0);
        let err = import_with(&project, SRT, Some(CaptionFormat::Ass), CaptionImportRequest::default()).unwrap_err();
        assert!(err.to_string().contains("no captions found in the ASS"), "{err}");
    }

    #[test]
    fn an_agents_import_is_staged_and_lands_as_one_agent_revision() {
        let (mut project, _) = project_with_one_clip(0.0, 10.0, 0.0);
        project.set_actor(EditSource::Agent);
        let revisions = project.history().unwrap().len();
        project.begin_staging(None, None).unwrap();

        let summary = import_srt(&project).unwrap();
        assert_eq!(summary.captions, 2);

        // The cut the user is looking at has not moved; the agent's own view has.
        assert!(project.timeline().unwrap().overlays.is_empty());
        assert_eq!(captions_on(&project).len(), 2, "the agent sees its proposal");
        let staged = project.staged().unwrap().expect("a proposal");
        assert_eq!(staged.edits, ["Import captions"]);
        assert!(staged.diff.entries.iter().any(|e| e.kind == DiffKind::OverlayAdded));
        assert_eq!(project.history().unwrap().len(), revisions);

        project.apply_staged(false).unwrap();
        assert_eq!(project.timeline().unwrap().overlays.len(), 2);
        let history = project.history().unwrap();
        assert_eq!(history.len(), revisions + 1);
        assert_eq!(history.last().unwrap().source, EditSource::Agent);
    }

    /// A phone's HLG footage reaches the preview two ways — straight from the
    /// original (tone-mapped in the graph) until its proxy lands, then from the
    /// proxy (tone-mapped when it was encoded). Both must convert exactly once,
    /// so the same moment looks the same either side of the swap.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored hdr_asset_previews`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    #[allow(clippy::print_stderr)]
    fn hdr_asset_previews_through_a_tonemapped_proxy_exactly_once() {
        use crate::engine::test_support::StatusBounded;
        let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let ffprobe = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
        let dir = std::env::temp_dir().join(format!("kerf-hdr-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("hlg.mp4");
        let sdr = dir.join("sdr.mp4");
        let tagged = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=640x360:rate=30:duration=2,format=yuv420p")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args([
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-colorspace",
                "bt709",
                "-color_range",
                "tv",
            ])
            .arg(&sdr)
            .status_bounded();
        assert!(tagged.unwrap().success());
        // FFmpeg 9 wants the card's colourspace stated in the graph, FFmpeg 4
        // wants it on a file: one of the two starts `zscale` from a known place.
        let convert = |input: &[&str]| {
            std::process::Command::new(&ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(input)
                .args([
                    "-vf",
                    "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
                ])
                .args(["-c:v", "libx265", "-x265-params", "log-level=error"])
                .args([
                    "-color_primaries",
                    "bt2020",
                    "-color_trc",
                    "arib-std-b67",
                    "-colorspace",
                    "bt2020nc",
                ])
                .arg(&media)
                .status_bounded()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        let card = "testsrc2=size=640x360:rate=30:duration=2,format=yuv420p,\
                    setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv";
        let made = convert(&["-f", "lavfi", "-i", card]) || convert(&["-i", sdr.to_str().unwrap()]);
        if !made {
            eprintln!("skipped: this ffmpeg cannot make the HLG test clip");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let rgb = |jpeg: &[u8]| -> Vec<u8> {
            let file = dir.join("frame.jpg");
            std::fs::write(&file, jpeg).unwrap();
            let out = std::process::Command::new(&ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-i"])
                .arg(&file)
                .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"])
                .output()
                .unwrap();
            out.stdout
        };

        let project = Project::open_in_memory().unwrap();
        let asset = project.import_asset(&media).unwrap();
        assert_eq!(asset.hdr(), Some(crate::model::Hdr::Hlg));
        project.add_clip_to_timeline(asset.id, None, 0.0, 2.0, Some(0.0)).unwrap();
        let (timeline, originals) = (project.timeline().unwrap(), project.list_assets().unwrap());
        let before = project.preview_assets().unwrap();
        assert_eq!(before[0].path, asset.path, "no proxy yet, so the original is decoded");
        assert!(before[0].hdr().is_some(), "and the graph tone-maps it");

        let from_original = rgb(&Project::composite_timeline_frame(&timeline, &originals, 1.0, 640, 2).unwrap());

        let proxy = engine::generate_proxy(&media, engine::proxy_width(asset.projection())).unwrap();
        let tags = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=color_transfer,pix_fmt",
                "-of",
                "csv=p=0",
            ])
            .arg(&proxy)
            .output()
            .unwrap();
        let tags = String::from_utf8_lossy(&tags.stdout).trim().to_string();
        let after = project.preview_assets().unwrap();
        let (timeline, _) = project.timeline_frame_inputs().unwrap();
        let from_proxy = rgb(&Project::composite_timeline_frame(&timeline, &after, 1.0, 640, 2).unwrap());
        let _ = std::fs::remove_file(&proxy);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(tags, "yuv420p,bt709", "the proxy is SDR");
        assert_eq!(after[0].path, proxy.to_string_lossy());
        assert!(after[0].hdr().is_none(), "so the graph leaves it alone");
        assert_eq!(from_original.len(), from_proxy.len());
        let mad = from_original
            .iter()
            .zip(&from_proxy)
            .map(|(a, b)| (*a as f64 - *b as f64).abs())
            .sum::<f64>()
            / from_original.len() as f64;
        assert!(
            mad < 8.0,
            "proxy and original previews differ by {mad:.1} levels: converted twice or not at all"
        );
    }

    /// Deletes a file when dropped, pass or fail: a proxy a test generated into the
    /// user's own cache must not outlive a failing assert.
    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Deletes a scratch directory when dropped, pass or fail.
    struct RemoveDirOnDrop(PathBuf);

    impl Drop for RemoveDirOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Six seconds of 30 fps video whose frames number themselves (bit k of the
    /// frame number is the k-th eighth of the picture's width, white for 1), the
    /// video starting `lead` seconds after the audio. `None` when this ffmpeg
    /// cannot make it.
    fn late_barcode_source(dir: &Path, lead: f64) -> Option<PathBuf> {
        use crate::engine::test_support::StatusBounded;
        let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let run = |args: &[&str]| {
            std::process::Command::new(&ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(args)
                .status_bounded()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        let bars = "color=c=black:s=640x360:r=30:d=6,format=yuv420p,\
                    geq=lum='if(bitand(trunc(N/pow(2,trunc(X*8/W))),1),235,16)':cb=128:cr=128";
        let (video, audio, media) = (dir.join("v.mp4"), dir.join("a.mp4"), dir.join(format!("late-{lead}.mp4")));
        let delay = lead.to_string();
        let made = run(&[
            "-f",
            "lavfi",
            "-i",
            bars,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "30",
            "-pix_fmt",
            "yuv420p",
            video.to_str().unwrap(),
        ]) && run(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=8",
            "-c:a",
            "aac",
            audio.to_str().unwrap(),
        ]) && run(&[
            "-itsoffset",
            &delay,
            "-i",
            video.to_str().unwrap(),
            "-i",
            audio.to_str().unwrap(),
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c",
            "copy",
            media.to_str().unwrap(),
        ]);
        made.then_some(media)
    }

    /// Which numbered frame of [`late_barcode_source`] a JPEG shows.
    fn barcode_of(dir: &Path, jpeg: &[u8]) -> u32 {
        let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let file = dir.join("frame.jpg");
        std::fs::write(&file, jpeg).unwrap();
        let out = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&file)
            .args(["-vf", "scale=640:360,format=gray", "-f", "rawvideo", "pipe:1"])
            .output()
            .unwrap();
        (0..8)
            .filter(|k| out.stdout[180 * 640 + (2 * k + 1) * 640 / 16] > 125)
            .map(|k| 1u32 << k)
            .sum()
    }

    /// A source whose video starts after its audio (a camera that opens its mic
    /// first). Its preview decodes the proxy and its export decodes the original,
    /// and a clip cut from the middle must show the same frame in both: an input
    /// `-ss` is relative to the *container's* start, and an FFmpeg-9 proxy of such
    /// a file used to start its container at the video, so every cut landed a few
    /// frames deeper in the preview than in the render.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored late_starting_source`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    #[allow(clippy::print_stderr)]
    fn late_starting_source_previews_the_frame_the_export_cuts() {
        let dir = std::env::temp_dir().join(format!("kerf-late-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _scratch = RemoveDirOnDrop(dir.clone());
        let Some(media) = late_barcode_source(&dir, 0.064) else {
            eprintln!("skipped: this ffmpeg cannot make the late-starting test clip");
            return;
        };

        let project = Project::open_in_memory().unwrap();
        let asset = project.import_asset(&media).unwrap();
        // Two cuts from the middle of the source, one on the frame grid and one off it.
        project.add_clip_to_timeline(asset.id, None, 0.5, 2.0, Some(0.0)).unwrap();
        project.add_clip_to_timeline(asset.id, None, 1.234, 3.0, Some(2.0)).unwrap();
        let (timeline, originals) = (project.timeline().unwrap(), project.list_assets().unwrap());
        let proxy = engine::generate_proxy(&media, engine::proxy_width(asset.projection())).unwrap();
        let _cleanup = RemoveOnDrop(proxy.clone());
        let (_, previews) = project.timeline_frame_inputs().unwrap();
        assert_eq!(previews[0].path, proxy.to_string_lossy(), "the preview decodes the proxy");

        let mut mismatches = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for t in [0.0, 0.1, 0.25, 0.5, 1.0, 1.4, 2.0, 2.05, 2.3, 3.0, 3.7] {
            let render = barcode_of(
                &dir,
                &Project::composite_timeline_frame(&timeline, &originals, t, 640, 2).unwrap(),
            );
            let preview = barcode_of(
                &dir,
                &Project::composite_timeline_frame(&timeline, &previews, t, 640, 2).unwrap(),
            );
            seen.insert(render);
            if render != preview {
                mismatches.push((t, render, preview));
            }
        }
        assert!(seen.len() > 5, "the clips never moved: {seen:?}");
        assert!(
            mismatches.is_empty(),
            "(time, export's frame, preview's frame) differ: {mismatches:?}"
        );
    }

    /// The same source, played from the very head of a clip cut at `source_in` 0 —
    /// which reads the proxy from its start with no `-ss`, where every other cut
    /// seeks. A padded proxy opens with a clone of the first frame that the original
    /// has no frame for, and the clip came out `lead` seconds of held frame late
    /// (with a 0.5 s lead: frame 15 at a second in where the export shows 30) —
    /// while a cut 2 ms in, which seeks past the clone, was exact. The cliff is at
    /// the most common clip there is, so both sides of it are pinned: the streamed
    /// frames of the proxy, as the player shows them, against the original's.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored head_clip_of_a_late`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    #[allow(clippy::print_stderr)]
    fn head_clip_of_a_late_starting_source_streams_the_frames_the_export_renders() {
        let dir = std::env::temp_dir().join(format!("kerf-late-head-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _scratch = RemoveDirOnDrop(dir.clone());
        let Some(media) = late_barcode_source(&dir, 0.5) else {
            eprintln!("skipped: this ffmpeg cannot make the late-starting test clip");
            return;
        };
        let project = Project::open_in_memory().unwrap();
        let asset = project.import_asset(&media).unwrap();
        let proxy = engine::generate_proxy(&media, engine::proxy_width(asset.projection())).unwrap();
        let _cleanup = RemoveOnDrop(proxy.clone());

        // A frame a clip shows, 40 frames from its start — past the second mark, where the
        // held-frame slip is a whole `lead` of them.
        for source_in in [0.0, 0.0005, 0.002, 0.5] {
            let clip = project
                .add_clip_to_timeline(asset.id, None, source_in, 4.0, Some(0.0))
                .unwrap();
            let originals = project.list_assets().unwrap();
            let (timeline, previews) = project.timeline_frame_inputs().unwrap();
            assert_eq!(previews[0].path, proxy.to_string_lossy(), "the preview decodes the proxy");
            let stream = |assets: &[Asset]| -> Vec<u32> {
                let mut frames = Vec::new();
                engine::stream_preview(&timeline, assets, 0.0, 30.0, &mut |f| {
                    frames.push(barcode_of(&dir, &f.jpeg));
                    frames.len() < 40
                })
                .unwrap();
                frames
            };
            let (export, preview) = (stream(&originals), stream(&previews));
            assert!(
                export.len() == 40 && export[39] > export[0],
                "source_in {source_in}: {export:?}"
            );
            assert_eq!(
                preview, export,
                "source_in {source_in}: the preview's frames differ from the export's"
            );
            project.remove_clips(&[clip.id]).unwrap();
        }
    }

    // ---- ripple mode and multi-clip edits ---------------------------------------

    /// V1 holding `a [0,5)  b [6,10)  c [12,15)` — a gap of 1s and one of 2s.
    fn gapped_project() -> (Project, Uuid, [Clip; 3]) {
        let (project, asset) = project_with_video_asset();
        let a = project.add_clip_to_timeline(asset, None, 0.0, 5.0, Some(0.0)).unwrap();
        let b = project.add_clip_to_timeline(asset, None, 0.0, 4.0, Some(6.0)).unwrap();
        let c = project.add_clip_to_timeline(asset, None, 0.0, 3.0, Some(12.0)).unwrap();
        (project, asset, [a, b, c])
    }

    /// Where each clip on the first track starts, in lane order.
    fn v1_starts(project: &Project) -> Vec<f64> {
        project.timeline().unwrap().tracks[0]
            .clips
            .iter()
            .map(|c| c.timeline_start)
            .collect()
    }

    #[test]
    fn ripple_mode_is_off_by_default_persists_with_the_file_and_is_not_an_edit() {
        let path = std::env::temp_dir().join(format!("kerf-ripple-{}.kerf", Uuid::new_v4()));
        let project = Project::create(&path).unwrap();
        assert!(!project.ripple_mode().unwrap());
        let revisions = project.history().unwrap().len();

        project.set_ripple_mode(true).unwrap();
        assert!(project.ripple_mode().unwrap() && project.ripple_active().unwrap());
        assert_eq!(project.history().unwrap().len(), revisions, "a setting, not an edit");
        drop(project);

        let reopened = Project::open(&path).unwrap();
        assert!(reopened.ripple_mode().unwrap(), "it travels with the project");
        reopened.set_ripple_mode(false).unwrap();
        assert!(!reopened.ripple_mode().unwrap());
        drop(reopened);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn with_the_mode_off_a_trim_leaves_the_later_clips_where_they_were() {
        let (project, _, [a, ..]) = gapped_project();
        project.trim(a.id, None, Some(3.0), None).unwrap();
        assert_eq!(v1_starts(&project), vec![0.0, 6.0, 12.0]);
    }

    #[test]
    fn a_ripple_trim_carries_the_track_along_as_one_revision() {
        let (project, _, [a, ..]) = gapped_project();
        project.set_ripple_mode(true).unwrap();
        let revisions = project.history().unwrap().len();

        project.trim(a.id, None, Some(3.0), None).unwrap(); // 5s -> 3s
        assert_eq!(v1_starts(&project), vec![0.0, 4.0, 10.0], "gaps of 1s and 2s kept");
        assert_eq!(project.history().unwrap().len(), revisions + 1);

        project.undo().unwrap();
        assert_eq!(v1_starts(&project), vec![0.0, 6.0, 12.0], "undo is one step too");
    }

    #[test]
    fn a_ripple_left_trim_keeps_the_start_and_reports_the_clip_as_it_ended_up() {
        let (project, _, [_, b, _]) = gapped_project();
        project.set_ripple_mode(true).unwrap();

        // What the GUI sends for a left-edge drag of one second: the in-point
        // moves and so does the start, to hold the right edge at 10.0.
        let trimmed = project.trim(b.id, Some(1.0), None, Some(7.0)).unwrap();
        assert_eq!(trimmed.timeline_start, 6.0, "the clip stays where it started");
        assert_eq!(trimmed.duration(), 3.0);
        assert_eq!(v1_starts(&project), vec![0.0, 6.0, 11.0], "and the rest follows it in");
    }

    #[test]
    fn ripple_mode_follows_speed_changes_deletes_and_inserts() {
        let (project, asset, [_, b, c]) = gapped_project();
        project.set_ripple_mode(true).unwrap();

        project.set_speed(b.id, 2.0).unwrap(); // 4s -> 2s
        assert_eq!(v1_starts(&project), vec![0.0, 6.0, 10.0]);

        project.remove(b.id).unwrap(); // closes b's 2s
        assert_eq!(v1_starts(&project), vec![0.0, 8.0]);

        // Appending moves nothing; dropping onto `c` pushes it by the new length.
        project.cut_clip(asset, 0.0, 2.0).unwrap();
        assert_eq!(v1_starts(&project)[1], 8.0);
        project.add_clip_to_timeline(asset, None, 0.0, 2.0, Some(8.0)).unwrap();
        let tl = project.timeline().unwrap();
        assert_eq!(tl.clip(c.id).unwrap().timeline_start, 10.0);
    }

    #[test]
    fn ripple_mode_leaves_ops_that_close_the_gap_themselves_alone() {
        // The same cut, edited with the mode off and on, must come out the same.
        let run = |ripple: bool| {
            let (project, _, [a, b, _]) = gapped_project();
            project.set_ripple_mode(ripple).unwrap();
            project.ripple_delete(b.id).unwrap();
            let after_delete = v1_starts(&project);
            project.cut_clip_range(a.id, 1.0, 2.0).unwrap();
            (after_delete, v1_starts(&project))
        };
        let (plain_delete, plain_cut) = run(false);
        assert_eq!(plain_delete, vec![0.0, 8.0]);
        assert_eq!(run(true), (plain_delete, plain_cut));
    }

    #[test]
    fn a_move_never_ripples() {
        let (project, _, [a, ..]) = gapped_project();
        project.set_ripple_mode(true).unwrap();
        project.move_clip(a.id, 0.5, None).unwrap();
        assert_eq!(v1_starts(&project), vec![0.5, 6.0, 12.0]);
    }

    #[test]
    fn the_beat_snap_comes_out_the_same_in_ripple_mode() {
        let run = |ripple: bool| {
            let project = Project::open_in_memory().unwrap();
            project.set_ripple_mode(ripple).unwrap();
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
            project.add_asset_audio(music.id).unwrap();
            project.cut_clip(video.id, 0.0, 1.1).unwrap();
            project.cut_clip(video.id, 2.0, 2.9).unwrap();
            project.cut_clip(video.id, 4.0, 5.0).unwrap();
            project.snap_to_beats(None, None).unwrap();
            project.timeline().unwrap().tracks[0]
                .clips
                .iter()
                .map(|c| (c.timeline_start, c.timeline_end()))
                .collect::<Vec<_>>()
        };
        let plain = run(false);
        assert_eq!(plain, vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)]);
        assert_eq!(run(true), plain);
    }

    #[test]
    fn with_ripple_overrides_the_flag_for_the_call_and_nothing_else() {
        let (project, _, [a, b, _]) = gapped_project();
        assert!(!project.ripple_active().unwrap());

        // Forced on over a project that is off.
        project.with_ripple(Some(true), |p| {
            assert!(p.ripple_active().unwrap());
            p.trim(a.id, None, Some(4.0), None).unwrap();
        });
        assert_eq!(v1_starts(&project), vec![0.0, 5.0, 11.0]);
        assert!(!project.ripple_mode().unwrap(), "the project's flag was never touched");
        assert!(!project.ripple_active().unwrap(), "and the override is gone");

        // Forced off over a project that is on; `None` inherits; nesting restores.
        project.set_ripple_mode(true).unwrap();
        project.with_ripple(Some(false), |p| {
            p.trim(a.id, None, Some(3.0), None).unwrap();
            assert!(!p.ripple_active().unwrap());
            p.with_ripple(None, |inner| assert!(!inner.ripple_active().unwrap(), "inherits the outer"));
            p.with_ripple(Some(true), |inner| assert!(inner.ripple_active().unwrap()));
            assert!(!p.ripple_active().unwrap(), "inner override undone");
        });
        assert_eq!(v1_starts(&project), vec![0.0, 5.0, 11.0], "a 1s trim that did not ripple");
        project.with_ripple(None, |p| p.trim(b.id, None, Some(3.0), None).unwrap());
        assert_eq!(v1_starts(&project), vec![0.0, 5.0, 10.0], "None follows the project's flag");
        assert!(project.ripple_active().unwrap());
    }

    #[test]
    fn a_staged_agent_edit_ripples_in_the_proposal_and_not_the_live_cut() {
        let (mut project, _, [a, ..]) = gapped_project();
        project.set_ripple_mode(true).unwrap();
        project.set_actor(EditSource::Agent);
        let revisions = project.history().unwrap().len();

        project.begin_staging(None, None).unwrap();
        project.trim(a.id, None, Some(3.0), None).unwrap();

        let live: Vec<f64> = v1_starts(&project);
        assert_eq!(live, vec![0.0, 6.0, 12.0], "the cut the user is looking at has not moved");
        let staged = project.working_timeline().unwrap();
        let proposed: Vec<f64> = staged.tracks[0].clips.iter().map(|c| c.timeline_start).collect();
        assert_eq!(proposed, vec![0.0, 4.0, 10.0]);

        let diff = project.staged().unwrap().unwrap().diff;
        let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == DiffKind::ClipMoved).count(),
            2,
            "the review card shows the clips that followed: {kinds:?}"
        );
        assert!(kinds.contains(&DiffKind::ClipRetrimmed));

        project.apply_staged(false).unwrap();
        assert_eq!(v1_starts(&project), vec![0.0, 4.0, 10.0]);
        assert_eq!(project.history().unwrap().len(), revisions + 1);
    }

    #[test]
    fn ripple_mode_never_moves_a_locked_track() {
        let (project, _, [a, ..]) = gapped_project();
        let v1 = project.timeline().unwrap().tracks[0].id;
        project.set_track_locked(v1, true).unwrap();
        project.set_ripple_mode(true).unwrap();
        project.trim(a.id, None, Some(3.0), None).unwrap();
        assert_eq!(v1_starts(&project), vec![0.0, 6.0, 12.0]);
    }

    #[test]
    fn move_clips_is_one_revision_and_keeps_the_group_together() {
        let (project, _, [a, b, c]) = gapped_project();
        let revisions = project.history().unwrap().len();

        let moved = project
            .move_clips(&[
                ClipMove {
                    clip_id: b.id,
                    timeline_start: 7.0,
                    track_id: None,
                },
                ClipMove {
                    clip_id: c.id,
                    timeline_start: 13.0,
                    track_id: None,
                },
            ])
            .unwrap();
        assert_eq!(moved.iter().map(|c| c.timeline_start).collect::<Vec<_>>(), vec![7.0, 13.0]);
        assert_eq!(v1_starts(&project), vec![0.0, 7.0, 13.0]);
        assert!(project.timeline().unwrap().clip(a.id).is_some());

        let history = project.history().unwrap();
        assert_eq!(history.len(), revisions + 1);
        assert_eq!(history.last().unwrap().label, "Move 2 clips");
    }

    #[test]
    fn a_refused_group_move_leaves_the_timeline_and_history_alone() {
        let (project, _, [a, b, _]) = gapped_project();
        let revisions = project.history().unwrap().len();
        let before = serde_json::to_string(&project.timeline().unwrap()).unwrap();

        // `b` would be fine; `a` lands on `b`'s spot.
        let result = project.move_clips(&[
            ClipMove {
                clip_id: b.id,
                timeline_start: 20.0,
                track_id: None,
            },
            ClipMove {
                clip_id: a.id,
                timeline_start: 12.5,
                track_id: None,
            },
        ]);
        assert!(matches!(result, Err(Error::InvalidArgument(_))), "{result:?}");
        assert_eq!(project.history().unwrap().len(), revisions);
        assert_eq!(serde_json::to_string(&project.timeline().unwrap()).unwrap(), before);
    }

    #[test]
    fn an_agents_group_move_is_staged_like_any_other_edit() {
        let (mut project, _, [a, ..]) = gapped_project();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None).unwrap();
        project
            .move_clips(&[ClipMove {
                clip_id: a.id,
                timeline_start: 0.5,
                track_id: None,
            }])
            .unwrap();
        assert_eq!(v1_starts(&project)[0], 0.0, "the live cut is untouched");
        assert_eq!(project.working_timeline().unwrap().clip(a.id).unwrap().timeline_start, 0.5);
        assert_eq!(project.staged().unwrap().unwrap().edits, vec!["Move clip".to_string()]);
    }

    #[test]
    fn remove_clips_is_one_revision_and_all_or_nothing() {
        let (project, _, [a, b, c]) = gapped_project();
        let revisions = project.history().unwrap().len();

        // One bad id and nothing is removed.
        assert!(matches!(
            project.remove_clips(&[a.id, Uuid::new_v4()]),
            Err(Error::ClipNotFound(_))
        ));
        assert_eq!(project.history().unwrap().len(), revisions);
        assert_eq!(v1_starts(&project).len(), 3);

        assert_eq!(project.remove_clips(&[a.id, c.id, a.id]).unwrap(), 2);
        let tl = project.timeline().unwrap();
        assert_eq!(tl.tracks[0].clips.len(), 1);
        assert_eq!(tl.tracks[0].clips[0].id, b.id);
        assert_eq!(tl.tracks[0].clips[0].timeline_start, 6.0, "a plain delete leaves the gap");
        let history = project.history().unwrap();
        assert_eq!(history.len(), revisions + 1);
        assert_eq!(history.last().unwrap().label, "Remove 2 clips");
    }

    #[test]
    fn remove_clips_under_ripple_closes_every_track_behind_it() {
        let (project, asset, [a, b, c]) = gapped_project();
        let v2 = project.add_track(StreamKind::Video, None).unwrap();
        let on_v2 = project.add_clip_to_timeline(asset, Some(v2.id), 0.0, 2.0, Some(1.0)).unwrap();
        let later_on_v2 = project.add_clip_to_timeline(asset, Some(v2.id), 0.0, 2.0, Some(5.0)).unwrap();

        // Forced on for the call: a multi-select *ripple* delete.
        let n = project
            .with_ripple(Some(true), |p| p.remove_clips(&[a.id, on_v2.id]))
            .unwrap();
        assert_eq!(n, 2);
        let tl = project.timeline().unwrap();
        assert_eq!(tl.clip(b.id).unwrap().timeline_start, 1.0, "b: 6 - 5");
        assert_eq!(tl.clip(c.id).unwrap().timeline_start, 7.0);
        assert_eq!(
            tl.clip(later_on_v2.id).unwrap().timeline_start,
            3.0,
            "V2 closed by its own 2s"
        );
        assert_eq!(project.history().unwrap().last().unwrap().label, "Ripple delete 2 clips");
        assert!(!project.ripple_mode().unwrap());
    }

    #[test]
    fn a_ripple_remove_of_one_clip_is_a_ripple_delete() {
        let remove = |ripple_delete: bool| {
            let (project, _, [_, b, _]) = gapped_project();
            if ripple_delete {
                project.ripple_delete(b.id).unwrap();
            } else {
                project.with_ripple(Some(true), |p| p.remove(b.id)).unwrap();
            }
            serde_json::to_value(&project.timeline().unwrap().tracks[0].clips)
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["timeline_start"].as_f64().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(remove(true), remove(false));
    }

    #[test]
    fn group_edits_refuse_a_locked_track() {
        let (project, _, [a, b, _]) = gapped_project();
        let v1 = project.timeline().unwrap().tracks[0].id;
        project.set_track_locked(v1, true).unwrap();
        let revisions = project.history().unwrap().len();

        let moved = project.move_clips(&[ClipMove {
            clip_id: a.id,
            timeline_start: 0.5,
            track_id: None,
        }]);
        assert!(matches!(moved, Err(Error::InvalidArgument(why)) if why.contains("locked")));
        assert!(matches!(
            project.remove_clips(&[b.id]),
            Err(Error::InvalidArgument(why)) if why.contains("locked")
        ));
        assert_eq!(project.history().unwrap().len(), revisions);
    }

    // ---- edit modes: roll, slip, slide, split-and-remove -------------------------

    /// V1 holding `a [0,4)` src 10..14, `b [4,8)` src 20..24 and `c [8,12)` src
    /// 30..34, all abutting, of a 60 s asset.
    fn abutting_project() -> (Project, Uuid, [Clip; 3]) {
        let (project, asset) = project_with_video_asset();
        let a = project.add_clip_to_timeline(asset, None, 10.0, 14.0, Some(0.0)).unwrap();
        let b = project.add_clip_to_timeline(asset, None, 20.0, 24.0, Some(4.0)).unwrap();
        let c = project.add_clip_to_timeline(asset, None, 30.0, 34.0, Some(8.0)).unwrap();
        (project, asset, [a, b, c])
    }

    /// Each clip of V1 as `(start, source_in, source_out)`, in lane order.
    fn v1_layout(project: &Project) -> Vec<(f64, f64, f64)> {
        project.timeline().unwrap().tracks[0]
            .clips
            .iter()
            .map(|c| (c.timeline_start, c.source_in, c.source_out))
            .collect()
    }

    #[test]
    fn roll_slip_and_slide_are_one_labelled_revision_each() {
        let (project, _, [a, b, c]) = abutting_project();
        let revisions = project.history().unwrap().len();

        let rolled = project.roll_edit(a.id, b.id, 1.0).unwrap();
        assert_eq!((rolled.requested, rolled.applied, rolled.clamped), (1.0, 1.0, false));
        assert_eq!(rolled.clips.iter().map(|x| x.id).collect::<Vec<_>>(), vec![a.id, b.id]);
        assert_eq!(
            v1_layout(&project),
            vec![(0.0, 10.0, 15.0), (5.0, 21.0, 24.0), (8.0, 30.0, 34.0)]
        );

        let slipped = project.slip_clip(c.id, -2.0).unwrap();
        assert_eq!(slipped.applied, -2.0);
        assert_eq!(v1_layout(&project)[2], (8.0, 28.0, 32.0));

        // `b` is now [5,8) and touches both neighbours.
        let slid = project.slide_clip(b.id, 1.0).unwrap();
        assert_eq!(slid.clips.iter().map(|x| x.id).collect::<Vec<_>>(), vec![a.id, b.id, c.id]);
        assert_eq!(
            v1_layout(&project),
            vec![(0.0, 10.0, 16.0), (6.0, 21.0, 24.0), (9.0, 29.0, 32.0)]
        );

        let history = project.history().unwrap();
        assert_eq!(history.len(), revisions + 3, "one revision per op");
        let labels: Vec<_> = history[revisions..].iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Roll edit", "Slip clip", "Slide clip"]);
    }

    #[test]
    fn the_three_exact_edit_modes_never_ripple_whatever_the_mode() {
        let run = |ripple: bool| {
            let (project, _, [a, b, c]) = abutting_project();
            project.set_ripple_mode(ripple).unwrap();
            project.roll_edit(a.id, b.id, -1.5).unwrap();
            project.slip_clip(a.id, 3.0).unwrap();
            // The last clip slid later carries the end of the track with it — the
            // one place a ripple of the following clips could have been mistaken.
            project.slide_clip(c.id, 2.0).unwrap();
            project.slide_clip(b.id, -0.5).unwrap();
            v1_layout(&project)
        };
        assert_eq!(run(true), run(false));
    }

    #[test]
    fn a_refused_edit_mode_records_no_revision() {
        let (project, _, [a, b, c]) = abutting_project();
        let revisions = project.history().unwrap().len();
        let before = serde_json::to_string(&project.timeline().unwrap()).unwrap();

        assert!(
            matches!(project.roll_edit(a.id, c.id, 1.0), Err(Error::InvalidArgument(_))),
            "not adjacent"
        );
        assert!(matches!(
            project.roll_edit(a.id, Uuid::new_v4(), 1.0),
            Err(Error::ClipNotFound(_))
        ));
        assert!(matches!(project.slip_clip(a.id, 0.0), Err(Error::InvalidArgument(_))));
        // `b` starts 20 s into a 60 s asset; a slip later that clamps to nothing is no edit.
        project.slip_clip(b.id, 100.0).unwrap();
        let revisions = revisions + 1;
        assert!(matches!(project.slip_clip(b.id, 1.0), Err(Error::InvalidArgument(why)) if why.contains("no footage left")));
        assert!(matches!(
            project.split_remove(a.id, 50.0, SplitSide::Left),
            Err(Error::InvalidArgument(_))
        ));
        assert_eq!(project.history().unwrap().len(), revisions);
        assert_ne!(
            serde_json::to_string(&project.timeline().unwrap()).unwrap(),
            before,
            "only the one slip landed"
        );
    }

    #[test]
    fn a_locked_track_refuses_every_edit_mode() {
        let (project, _, [a, b, _]) = abutting_project();
        let v1 = project.timeline().unwrap().tracks[0].id;
        project.set_track_locked(v1, true).unwrap();
        let revisions = project.history().unwrap().len();
        let why = |r: Result<()>| match r {
            Err(Error::InvalidArgument(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(why(project.roll_edit(a.id, b.id, 1.0).map(|_| ())).contains("locked"));
        assert!(why(project.slip_clip(a.id, 1.0).map(|_| ())).contains("locked"));
        assert!(why(project.slide_clip(b.id, 1.0).map(|_| ())).contains("locked"));
        assert!(why(project.split_remove(a.id, 2.0, SplitSide::Left).map(|_| ())).contains("locked"));
        assert_eq!(project.history().unwrap().len(), revisions);
    }

    #[test]
    fn edit_modes_clamp_a_still_to_nothing_but_the_neighbours() {
        let (project, asset) = project_with_video_asset();
        let mut image = project.require_asset(asset).unwrap();
        image.id = Uuid::new_v4();
        image.path = "/x.png".into();
        image.name = "x.png".into();
        image.duration = 5.0;
        image.streams[0].image = true;
        project.insert_asset(&image).unwrap();
        let limits = project.source_limits().unwrap();
        assert_eq!(limits[&asset], 60.0);
        assert!(limits[&image.id].is_infinite());

        let video = project.add_clip_to_timeline(asset, None, 10.0, 15.0, Some(0.0)).unwrap();
        let still = project.add_clip_to_timeline(image.id, None, 0.0, 5.0, Some(5.0)).unwrap();
        assert!(matches!(project.slip_clip(still.id, 1.0), Err(Error::InvalidArgument(why)) if why.contains("still")));
        // Rolled earlier the still grows, its window staying at 0.
        let out = project.roll_edit(video.id, still.id, -2.0).unwrap();
        assert_eq!(out.applied, -2.0);
        assert_eq!(v1_layout(&project)[1], (3.0, 0.0, 7.0));
    }

    #[test]
    fn an_agents_edit_modes_are_staged_like_any_other_edit() {
        let (mut project, _, [a, b, c]) = abutting_project();
        let live = v1_layout(&project);
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None).unwrap();
        project.roll_edit(a.id, b.id, 1.0).unwrap();
        project.slip_clip(c.id, 1.0).unwrap();
        project.slide_clip(b.id, 0.5).unwrap();
        project.split_remove(c.id, 10.0, SplitSide::Right).unwrap();

        assert_eq!(v1_layout(&project), live, "the live cut is untouched");
        let staged = project.staged().unwrap().unwrap();
        assert_eq!(
            staged.edits,
            ["Roll edit", "Slip clip", "Slide clip", "Split and remove right"]
        );
        let proposal = project.working_timeline().unwrap();
        assert_eq!(
            proposal.clip(c.id).unwrap().source_in,
            31.5,
            "slipped by 1, then the slide trimmed half a second off its head"
        );
        assert!(!staged.diff.is_empty(), "the review card has something to show");
    }

    #[test]
    fn a_group_split_remove_is_one_revision_and_undoes_in_one_step() {
        let (project, asset) = project_with_video_asset();
        let tracks = project.timeline().unwrap().tracks;
        let (v1, a1) = (tracks[0].id, tracks[1].id);
        let v = project.add_clip_to_timeline(asset, Some(v1), 10.0, 16.0, Some(0.0)).unwrap();
        let a = project.add_clip_to_timeline(asset, Some(a1), 10.0, 16.0, Some(1.0)).unwrap();
        let revisions = project.history().unwrap().len();

        let cuts = [ClipCut { clip_id: v.id, at: 3.0 }, ClipCut { clip_id: a.id, at: 3.0 }];
        let kept = project.split_remove_clips(&cuts, SplitSide::Left).unwrap();
        assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), vec![v.id, a.id]);
        let tl = project.timeline().unwrap();
        assert_eq!(
            (tl.clip(v.id).unwrap().timeline_start, tl.clip(a.id).unwrap().timeline_start),
            (3.0, 3.0)
        );
        let history = project.history().unwrap();
        assert_eq!(history.len(), revisions + 1, "one revision for both tracks");
        assert_eq!(history.last().unwrap().label, "Split and remove left (2 clips)");

        let undone = project.undo().unwrap();
        assert_eq!(
            (
                undone.clip(v.id).unwrap().timeline_start,
                undone.clip(a.id).unwrap().timeline_start
            ),
            (0.0, 1.0)
        );

        // A single cut keeps the single label, and is the same code path.
        project.split_remove_clips(&cuts[..1], SplitSide::Right).unwrap();
        assert_eq!(project.history().unwrap().last().unwrap().label, "Split and remove right");
    }

    #[test]
    fn a_refused_group_split_remove_records_nothing_and_ripple_is_per_track() {
        let (project, asset) = project_with_video_asset();
        let tracks = project.timeline().unwrap().tracks;
        let (v1, a1) = (tracks[0].id, tracks[1].id);
        let v = project.add_clip_to_timeline(asset, Some(v1), 10.0, 16.0, Some(0.0)).unwrap();
        let w = project.add_clip_to_timeline(asset, Some(v1), 0.0, 2.0, Some(8.0)).unwrap();
        let a = project.add_clip_to_timeline(asset, Some(a1), 10.0, 16.0, Some(1.0)).unwrap();
        let x = project.add_clip_to_timeline(asset, Some(a1), 0.0, 2.0, Some(9.0)).unwrap();
        let revisions = project.history().unwrap().len();

        let bad = [ClipCut { clip_id: v.id, at: 3.0 }, ClipCut { clip_id: a.id, at: 50.0 }];
        assert!(matches!(
            project.split_remove_clips(&bad, SplitSide::Left),
            Err(Error::InvalidArgument(_))
        ));
        assert_eq!(project.history().unwrap().len(), revisions);
        assert_eq!(project.timeline().unwrap().clip(v.id).unwrap().timeline_start, 0.0);

        // Ripple on: each lane closes by what it lost — V1 by 2 s, A1 by 4 s.
        project.set_ripple_mode(true).unwrap();
        let cuts = [ClipCut { clip_id: v.id, at: 2.0 }, ClipCut { clip_id: a.id, at: 5.0 }];
        let kept = project.split_remove_clips(&cuts, SplitSide::Left).unwrap();
        assert_eq!(
            kept.iter().map(|c| c.timeline_start).collect::<Vec<_>>(),
            vec![0.0, 1.0],
            "handed back as they ended up"
        );
        let tl = project.timeline().unwrap();
        assert_eq!(
            (tl.clip(w.id).unwrap().timeline_start, tl.clip(x.id).unwrap().timeline_start),
            (6.0, 5.0)
        );
    }

    #[test]
    fn an_agents_group_split_remove_is_staged() {
        let (mut project, asset) = project_with_video_asset();
        let tracks = project.timeline().unwrap().tracks;
        let v = project
            .add_clip_to_timeline(asset, Some(tracks[0].id), 10.0, 16.0, Some(0.0))
            .unwrap();
        let a = project
            .add_clip_to_timeline(asset, Some(tracks[1].id), 10.0, 16.0, Some(0.0))
            .unwrap();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None).unwrap();
        let cuts = [ClipCut { clip_id: v.id, at: 2.0 }, ClipCut { clip_id: a.id, at: 2.0 }];
        project.split_remove_clips(&cuts, SplitSide::Right).unwrap();
        assert_eq!(
            project.timeline().unwrap().clip(v.id).unwrap().duration(),
            6.0,
            "the live cut is untouched"
        );
        assert_eq!(project.working_timeline().unwrap().clip(v.id).unwrap().duration(), 2.0);
        assert_eq!(project.staged().unwrap().unwrap().edits, ["Split and remove right (2 clips)"]);
    }

    #[test]
    fn a_staged_slip_alone_still_shows_up_in_the_review_diff() {
        // A slip changes no timing at all, only which footage shows: a diff that
        // missed it would read as an empty proposal and `apply_staged` would drop it.
        let (mut project, _, [a, ..]) = abutting_project();
        project.set_actor(EditSource::Agent);
        project.begin_staging(None, None).unwrap();
        project.slip_clip(a.id, 2.0).unwrap();
        let staged = project.staged().unwrap().unwrap();
        assert_eq!(staged.diff.entries.len(), 1);
        assert!(
            staged.diff.summary().contains("Slipped clip on V1"),
            "{}",
            staged.diff.summary()
        );
    }

    #[test]
    fn split_remove_follows_ripple_mode_like_a_trim_and_keeps_the_clips_identity() {
        let revisions = |project: &Project| project.history().unwrap().len();

        // Off: a plain split-and-delete — the gap stays where the half was.
        let (project, _, [a, b, c]) = abutting_project();
        let before = revisions(&project);
        let kept = project.split_remove(b.id, 6.0, SplitSide::Right).unwrap();
        assert_eq!(kept.id, b.id);
        assert_eq!(
            v1_layout(&project),
            vec![(0.0, 10.0, 14.0), (4.0, 20.0, 22.0), (8.0, 30.0, 34.0)]
        );
        assert_eq!(revisions(&project), before + 1);
        assert_eq!(project.history().unwrap().last().unwrap().label, "Split and remove right");
        let _ = (a, c);

        let (project, _, [_, b, _]) = abutting_project();
        let kept = project.split_remove(b.id, 6.0, SplitSide::Left).unwrap();
        assert_eq!((kept.timeline_start, kept.source_in), (6.0, 22.0));
        assert_eq!(
            v1_layout(&project),
            vec![(0.0, 10.0, 14.0), (6.0, 22.0, 24.0), (8.0, 30.0, 34.0)]
        );
        assert_eq!(project.history().unwrap().last().unwrap().label, "Split and remove left");

        // On: the track closes up, and a left removal keeps the clip's own start.
        let (project, _, [_, b, _]) = abutting_project();
        project.set_ripple_mode(true).unwrap();
        project.split_remove(b.id, 6.0, SplitSide::Right).unwrap();
        assert_eq!(
            v1_layout(&project),
            vec![(0.0, 10.0, 14.0), (4.0, 20.0, 22.0), (6.0, 30.0, 34.0)]
        );

        let (project, _, [_, b, _]) = abutting_project();
        project.set_ripple_mode(true).unwrap();
        let kept = project.split_remove(b.id, 6.0, SplitSide::Left).unwrap();
        assert_eq!(
            kept.timeline_start, 4.0,
            "handed back as it ended up, not as the edit alone made it"
        );
        assert_eq!(
            v1_layout(&project),
            vec![(0.0, 10.0, 14.0), (4.0, 22.0, 24.0), (6.0, 30.0, 34.0)]
        );

        // A per-call override, as every ripple-following edit takes.
        let (project, _, [_, b, _]) = abutting_project();
        project.set_ripple_mode(true).unwrap();
        project
            .with_ripple(Some(false), |p| p.split_remove(b.id, 6.0, SplitSide::Right))
            .unwrap();
        assert_eq!(v1_layout(&project)[2].0, 8.0);
    }
}
