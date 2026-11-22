//! A `.kerf` project: a SQLite database holding imported assets, cached
//! analysis metadata, and the non-destructive timeline (EDL). All timeline
//! operations mutate the stored EDL; nothing is re-encoded until [`Project::export`].

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::captions_import::{CaptionFile, CaptionImportRequest, ImportSummary, MAX_CAPTION_OFFSET, MAX_IMPORTED_CAPTIONS};
use crate::engine::{self, ExportProgress};
use crate::error::{Error, Result};
use crate::model::Color;
use crate::model::Grade;
use crate::model::MarkerPatch;
use crate::model::{check_speed_ramp, SpeedKey};
use crate::model::{default_beat_tolerance, fmt_time};
use crate::model::{music_fit_clips, plan_music_fit, MusicFit, MusicFitReport, MAX_FIT_BARS};
use crate::model::{AnalysisKind, CutLine};
use crate::model::{
    Asset, AssetAnalysis, AudioEffect, BlendMode, CaptionOptions, CaptionStyle, CaptionTimeBase, Clip, ClipCut, ClipMove,
    CropFrame, Delivery, Easing, EditOutcome, EditSource, Framing, Keyframe, Levels, Marker, Mask, MasterBus, Projection,
    Reframe, ReframeKeyframe, Revision, SourceLimits, SplitSide, StagedEdit, StreamInfo, StreamKind, Task, TaskStatus, Tempo,
    TextKeyframe, TextOverlay, TimeRange, Timeline, TimelineDiff, Track, TrackLevels, TranscriptSegment, Transition, VideoEffect,
    Voiceover, MASTER_MAX_VOLUME, MASTER_MIN_CEILING_DB, MAX_CHANNEL_VOLUME, MAX_FOV, MAX_SPEED, MAX_SVG_OVERLAYS,
    MAX_SVG_SECONDS, MIN_FOV, MIN_SPEED,
};
use crate::model::{AutoIncluded, SkippedChange, StagedAssetChange, Tag};
use crate::model::{Detached, DetachedMany};
use crate::model::{Property, PropertyKey, PropertyTrack, TransformPatch};
use crate::model::{SvgOverlay, SvgOverlayPatch, SvgPreset, GENERATED_ASSET};
use crate::svg::{check_params, SvgDoc};

mod assets;
mod audio_edits;
mod framing;
mod look;
mod overlays;
mod previews;
mod range_ops;
mod reframe;
mod sample;
mod settings;
mod tasks;
mod timeline_ops;
mod transcript;
mod validate;

pub use assets::*;
pub use framing::*;
pub use overlays::*;
pub use previews::AssetPlayback;
pub use transcript::*;
pub use validate::*;

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
    voiceover    TEXT,
    -- JSON `MusicScore` for audio Kerf composed from a score; NULL for media the
    -- user brought. Added to older files by the v2 to v3 migration (`store.rs`).
    music_score  TEXT,
    -- JSON array of the asset's `Tag`s; NULL for none. Added to older files by the v14 to v15 migration.
    tags         TEXT,
    -- JSON `SfxSpec` for a sound effect Kerf synthesized; NULL for media the user brought. Added to
    -- older files by the v18 to v19 migration (`store.rs`).
    sound_effect TEXT
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
    updated_at TEXT NOT NULL,
    -- The agent session (MCP session id) the proposal belongs to; NULL when none was named.
    -- Older files get this column from the v1 to v2 migration (`store.rs`).
    owner      TEXT,
    -- JSON array of the asset changes held back with the proposal (`StagedAssetChange`).
    -- Migrated onto older files with `owner`.
    assets     TEXT
);

-- The proposal's previous states, newest last: what `undo_staged` steps back through. Bounded
-- (`STAGED_UNDO_DEPTH`) and emptied with the proposal.
CREATE TABLE IF NOT EXISTS staged_undo (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    timeline TEXT NOT NULL,
    assets   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id         TEXT PRIMARY KEY,
    prompt     TEXT NOT NULL,
    status     TEXT NOT NULL,
    result     TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Fonts the project embeds (a template's typeface travels with it), by the SHA-256 of the file.
-- Added by schema v3 (`store::MIGRATIONS`); older files get it from the migration.
CREATE TABLE IF NOT EXISTS fonts (
    sha256 TEXT PRIMARY KEY,
    family TEXT NOT NULL,
    data   BLOB NOT NULL
);

-- Covers `claim_next_task` (WHERE status = 'queued' ORDER BY created_at LIMIT 1)
-- and the queue/asset list sorts. Idempotent, so safe to apply to older files.
CREATE INDEX IF NOT EXISTS idx_tasks_status_created ON tasks (status, created_at);
CREATE INDEX IF NOT EXISTS idx_tasks_created       ON tasks (created_at);
CREATE INDEX IF NOT EXISTS idx_assets_imported     ON assets (imported_at);
"#;

/// The audio track [`Project::place_voiceover`] puts narration on by default.
pub const VOICEOVER_TRACK: &str = "VO";

/// The audio track [`Project::place_music`] puts composed music on by default.
pub const MUSIC_TRACK: &str = "M";

/// The audio track [`Project::place_sound_effects`] puts sound effects on by default (`FX`, then `FX 2`,
/// `FX 3`… for the ones that overlap an earlier one).
pub const SFX_TRACK: &str = "FX";

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

/// `meta` key holding the proposal [`Project::discard_staged`] last threw away (a serialized
/// [`StagedRow`]), until [`Project::restore_discarded`] takes it back or another is discarded.
const DISCARDED_STAGED: &str = "discarded_staged";

/// Whether a proposal owned by `owner` belongs to a different session than `caller`: it has an
/// owner, and the caller is not it (a caller that names no session is not it either).
fn owned_by_other(owner: Option<&str>, caller: Option<&str>) -> bool {
    matches!(owner, Some(o) if caller != Some(o))
}

/// Refuse a clip on a track it cannot play on: a picture on a video track (a title is one), sound on an audio
/// track. `hint` ends the sentence with what to do instead.
fn check_track_kind(track: &Track, picture: bool, sound: bool, hint: &str) -> Result<()> {
    let (fits, kind, has) = match track.kind {
        StreamKind::Audio => (sound, "audio", "sound"),
        _ => (picture, "video", "picture"),
    };
    if fits {
        return Ok(());
    }
    Err(Error::InvalidArgument(format!(
        "a clip with no {has} cannot go on {kind} track {} — {hint}",
        track.name
    )))
}

/// The ids of the locked tracks of `timeline`.
fn locked_ids(timeline: &Timeline) -> HashSet<Uuid> {
    timeline.tracks.iter().filter(|t| t.locked).map(|t| t.id).collect()
}

/// Whether a track of an agent's proposal is locked for the agent: locked on the live cut now, or locked by
/// the proposal itself (`own`) where the cut it branched from (`base`) was not. A lock the proposal only
/// inherited follows the live cut — the user may have lifted it since.
fn effective_lock(own: bool, id: &Uuid, base: &HashSet<Uuid>, live: &HashSet<Uuid>) -> bool {
    live.contains(id) || (own && !base.contains(id))
}

/// Lay [`effective_lock`] over a proposal's tracks, returning each track's own flag to put back.
fn lay_effective_locks(proposal: &mut Timeline, base: &HashSet<Uuid>, live: &HashSet<Uuid>) -> HashMap<Uuid, bool> {
    let own: HashMap<Uuid, bool> = proposal.tracks.iter().map(|t| (t.id, t.locked)).collect();
    for track in &mut proposal.tracks {
        track.locked = effective_lock(track.locked, &track.id, base, live);
    }
    own
}

/// Where a clip of `kind` goes when the edit names no track: the first unlocked track of that kind.
/// Every one of them locked refuses (a locked track is never written to); none at all is the old
/// "no suitable track".
fn default_track(timeline: &Timeline, kind: StreamKind) -> Result<Uuid> {
    timeline.first_unlocked_track_of(kind).ok_or_else(|| {
        if timeline.first_track_of(kind).is_some() {
            let word = if kind == StreamKind::Audio { "audio" } else { "video" };
            Error::InvalidArgument(format!("every {word} track is locked — unlock one, or add a track"))
        } else {
            Error::Other("no suitable track for asset".to_string())
        }
    })
}

/// The timeline as the JSON that is stored, refusing one that holds a number JSON cannot say.
/// `serde_json` writes `NaN` and `inf` as `null`, which a plain number field then fails to read
/// back — the whole project would stop opening — so this is checked on every write, whatever
/// the setter that produced the number did or did not validate.
fn timeline_to_json(timeline: &Timeline) -> Result<String> {
    check_finite(timeline)?;
    Ok(serde_json::to_string(timeline)?)
}

/// The half of [`timeline_to_json`] that refuses a number JSON cannot say, for an edit whose write waits for the end of
/// its batch (`Project::batch` holds the timeline parsed): the edit that makes the number is still the one refused.
fn check_finite(timeline: &Timeline) -> Result<()> {
    match crate::finite::first_non_finite(timeline) {
        Some(at) => Err(Error::InvalidArgument(format!(
            "refusing to store the timeline: {at} is not a finite number"
        ))),
        None => Ok(()),
    }
}

/// `asset` with one held-back change applied: its tag edit run over its tags as they are, or else its projection.
fn apply_asset_change_to(asset: &mut Asset, change: &StagedAssetChange) {
    match &change.tag_edit {
        Some(edit) => asset.tags = edit.apply_capped(&asset.tags),
        None => {
            for stream in asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
                stream.projection = change.projection;
            }
        }
    }
}

/// `asset` with the held-back changes that are about it applied.
fn apply_asset_changes(asset: &mut Asset, changes: &[StagedAssetChange]) {
    let id = asset.id;
    for change in changes.iter().filter(|c| c.asset_id == id) {
        apply_asset_change_to(asset, change);
    }
}

/// `meta` key holding `[id, name]` pairs of the assets taken out of the library
/// ([`Project::remove_asset`]), newest last, so a refused undo can name the media it misses.
const REMOVED_ASSETS: &str = "removed_assets";

/// How many removed assets' names are kept.
const REMOVED_ASSETS_KEPT: usize = 200;

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
    /// The sounds the sync lock trimmed in the edit being made — the names of their
    /// tracks — so its revision can say so (`… (trimmed sound on A2)`). Filled while an
    /// edit runs and drained when it lands; a `RefCell` for the same reason as the
    /// overrides above.
    edit_notes: RefCell<Vec<String>>,
    /// The linked partners an edit carried a trim to (`Timeline::carry_extent_edit`), so
    /// `run_edit` can lane-check them once the ripple has run. Filled while an edit runs.
    edit_carried: RefCell<Vec<Uuid>>,
    /// Clips whose lane the edit being started must leave clear (checked after the ripple, against
    /// the cut before): `trim` and `add_clip_to_timeline` register theirs just before the edit runs.
    edit_lanes: RefCell<Vec<Uuid>>,
    /// Counts the projects this one has been replaced by ([`Project::replace_with`]), so a job that
    /// resolved its inputs under one project can tell its result is for another.
    generation: u64,
    /// What the connection's own write count is added to, so [`Project::change_count`] keeps
    /// counting up across [`Project::replace_with`] (the new connection starts again at zero).
    change_base: u64,
    /// Whether an agent's edits are held for review (see [`Project::set_agent_review`]).
    agent_review: Cell<bool>,
    /// The agent session the current call declared ([`Project::ensure_agent_staging`]); cleared
    /// whenever the actor is set, i.e. at every lock.
    agent_owner: RefCell<Option<String>>,
    /// The crash-recovery directory an untitled project lives in (see [`crate::recovery`]),
    /// locked while this is open. Declared after `conn` so the file closes before the lock goes.
    recovery: Option<crate::recovery::RecoveryDir>,
    /// How many revisions the history keeps (see [`Project::set_history_limit`]).
    history_limit: usize,
    /// The fonts this project embeds, kept beside the `fonts` table so that becoming the shared
    /// project ([`Project::replace_with`]) can make them the drawing fonts without reading the table
    /// under the lock. A `RefCell`, like the overrides above.
    embedded: RefCell<Vec<crate::fonts::EmbeddedFont>>,
    /// The edits of the [`Project::batch`] in progress, so they land as one step.
    batch: RefCell<Option<batch::BatchSteps>>,
}

/// How many steps back [`Project::undo_staged`] can take a proposal.
pub const STAGED_UNDO_DEPTH: usize = 20;

/// How many revisions a project keeps by default: every one is the whole timeline, so an
/// unbounded history grows the file without limit. `KERF_HISTORY_LIMIT` overrides it.
pub const DEFAULT_HISTORY_LIMIT: usize = 500;

fn default_history_limit() -> usize {
    std::env::var("KERF_HISTORY_LIMIT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .max(1)
}

/// What an edit's revision label says about the sounds it trimmed to make room: nothing
/// when none was, else ` (trimmed sound on A2, A3)` — each track once, in order.
fn trimmed_suffix(notes: &[String]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for n in notes {
        if !seen.contains(&n.as_str()) {
            seen.push(n);
        }
    }
    if seen.is_empty() {
        String::new()
    } else {
        format!(" (trimmed sound on {})", seen.join(", "))
    }
}

/// A write begun by [`Project::tx`]; dropped without [`Tx::commit`], it rolls back.
struct Tx<'a> {
    conn: &'a Connection,
    nested: bool,
    open: bool,
}

impl Tx<'_> {
    fn commit(mut self) -> Result<()> {
        self.conn
            .execute_batch(if self.nested { "RELEASE kerf_edit" } else { "COMMIT" })?;
        self.open = false;
        Ok(())
    }
}

impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if self.open && !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch(if self.nested {
                "ROLLBACK TO kerf_edit; RELEASE kerf_edit"
            } else {
                "ROLLBACK"
            });
        }
    }
}

/// The project-meta key holding the ids of assets whose proxy the user deleted.
const PROXY_DECLINED_KEY: &str = "proxy_declined";

/// The file a preview decodes for `asset` under `mode` (pure apart from `ready`, which
/// is only asked for when a proxy could be used): the ready proxy of an asset that has
/// a picture, unless the mode is the original.
fn resolve_preview_source(asset: &Asset, mode: crate::proxy::PreviewSource, ready: impl FnOnce() -> Option<PathBuf>) -> PathBuf {
    if mode != crate::proxy::PreviewSource::Original && crate::proxy::needs_proxy(asset) {
        if let Some(proxy) = ready() {
            return proxy;
        }
    }
    PathBuf::from(&asset.path)
}

/// The ready proxy a whole-file analysis read of an asset may decode in place of the original, or
/// `None` to read the original: [`resolve_preview_source`] when it picks a proxy, and only when
/// `faithful` says the proxy stands in for the file for what the read is after
/// ([`engine::ProxyFidelity`]). The proxy is a faster way to the same answer or it is not used.
fn resolve_analysis_proxy(
    asset: &Asset,
    mode: crate::proxy::PreviewSource,
    ready: impl FnOnce() -> Option<PathBuf>,
    faithful: impl FnOnce() -> bool,
) -> Option<PathBuf> {
    let source = resolve_preview_source(asset, mode, ready);
    (source != Path::new(&asset.path) && faithful()).then_some(source)
}

impl Project {
    fn with_conn(conn: Connection, path: Option<PathBuf>, recovery: Option<crate::recovery::RecoveryDir>) -> Self {
        Self {
            conn,
            path,
            actor: EditSource::User,
            ripple_override: Cell::new(None),
            links_override: Cell::new(None),
            edit_notes: RefCell::new(Vec::new()),
            edit_carried: RefCell::new(Vec::new()),
            edit_lanes: RefCell::new(Vec::new()),
            generation: 0,
            change_base: 0,
            agent_review: Cell::new(true),
            agent_owner: RefCell::new(None),
            recovery,
            history_limit: default_history_limit(),
            embedded: RefCell::new(Vec::new()),
            batch: RefCell::new(None),
        }
    }

    /// Create (or overwrite the schema of) a `.kerf` file on disk.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut project = Self::with_conn(Connection::open(&path)?, Some(path), None);
        project.init()?;
        Ok(project)
    }

    /// Open an existing `.kerf` file, ensuring the schema is present.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut project = Self::with_conn(Connection::open(&path)?, Some(path), None);
        project.init()?;
        Ok(project)
    }

    /// An in-memory project, handy for tests and a throwaway sample.
    pub fn open_in_memory() -> Result<Self> {
        let mut project = Self::with_conn(Connection::open_in_memory()?, None, None);
        project.init()?;
        Ok(project)
    }

    /// A new untitled project backed by a locked recovery directory under `root`, so a crash
    /// before the first Save loses nothing. It reads as unsaved (`path()` is `None`).
    pub fn create_recovery(root: &Path) -> Result<Self> {
        let dir = crate::recovery::RecoveryDir::create(root)?;
        let conn = match Connection::open(dir.project_path()) {
            Ok(conn) => conn,
            Err(e) => {
                let _ = dir.remove();
                return Err(e.into());
            }
        };
        let mut project = Self::with_conn(conn, None, Some(dir));
        if let Err(e) = project.init() {
            project.close();
            return Err(e);
        }
        Ok(project)
    }

    /// Reopen a session a crashed Kerf left behind; it stays untitled and keeps its directory
    /// (and lock) until it is saved or let go.
    pub fn open_recovered(found: crate::recovery::Recoverable) -> Result<Self> {
        let dir = found.dir;
        let mut project = Self::with_conn(Connection::open(dir.project_path())?, None, Some(dir));
        project.init()?;
        Ok(project)
    }

    /// The recovery session this untitled project lives in, if it has one.
    pub fn recovery_id(&self) -> Option<&str> {
        self.recovery.as_ref().map(|r| r.id())
    }

    pub fn recovery_dir(&self) -> Option<&Path> {
        self.recovery.as_ref().map(|r| r.dir())
    }

    /// Close the project; an untitled one's recovery directory goes with it. Call it with no lock
    /// held: deleting a directory is file-system work.
    pub fn close(self) {
        let Self { conn, recovery, .. } = self;
        drop(conn);
        if let Some(dir) = recovery {
            let path = dir.dir().to_path_buf();
            if let Err(e) = dir.remove() {
                tracing::warn!(dir = %path.display(), error = %e, "could not delete a recovery directory");
            }
        }
    }

    /// Become `other` (a project just opened or created), for the one `Project` the app and its MCP
    /// server share. The plan epoch moves **after** the swap: opening `other` moved it, but a frame
    /// that took its inputs from this project between that and the swap read the old project under the
    /// new epoch. Call it under the lock the shared project sits behind, so no reader sees the new
    /// project under the old epoch either.
    ///
    /// The **generation** moves too ([`Project::generation`]), and the app's agent-review setting
    /// carries over: it belongs to the app, not to the file. A discarded proposal kept in `other`'s
    /// file is dropped: it belongs to the session that discarded it, and a project that is opened
    /// again is not offered a restore of something thrown away before.
    ///
    /// The swap also makes `other`'s embedded fonts the ones titles are drawn with — **this is the only
    /// place opening a project reaches the process's fonts** (opening reads them, it registers none, so
    /// an open that is cancelled or refused leaves the open project drawing as it did), from what it
    /// already holds in memory: no table is read under the lock.
    ///
    /// Returns the project replaced, for the caller to [`Self::close`] once it has released the
    /// lock — closing deletes an untitled one's recovery directory, since replacing it means it
    /// was saved, recovered into another, or let go.
    #[must_use = "close the replaced project once the lock is released"]
    pub fn replace_with(&mut self, other: Project) -> Project {
        let generation = self.generation.wrapping_add(1);
        let _ = other.forget_discarded();
        self.swap_in(other, generation)
    }

    /// Become `copy`, a reopened copy of **this same project** saved to another file (Save As), and
    /// stay the same project: the **generation does not move**, so the jobs in flight (an analysis,
    /// an import, a voiceover, a proxy) that noted it still land in the project they began on —
    /// which, but for the file it lives in, is this one. The change count carries on as in
    /// [`Project::replace_with`], and so does the project replaced: close it after the lock is
    /// released (an untitled one's recovery directory goes — it was just saved). Only for that
    /// swap: anything that is a different project is [`Project::replace_with`]. Call it under the
    /// lock, after checking that [`Project::change_count`] did not move since the copy was written.
    #[must_use = "close the replaced project once the lock is released"]
    pub fn replace_with_saved_copy(&mut self, copy: Project) -> Project {
        let generation = self.generation;
        self.swap_in(copy, generation)
    }

    fn swap_in(&mut self, other: Project, generation: u64) -> Project {
        let review = self.agent_review.get();
        let change_base = self.change_count().saturating_add(1);
        let old = std::mem::replace(self, other);
        self.generation = generation;
        self.change_base = change_base;
        self.agent_review.set(review);
        // The fonts titles are drawn with are the active project's, whatever another project registered
        // between this one being opened and being swapped in.
        self.activate_fonts();
        crate::plan_epoch::bump_plan_epoch();
        crate::project_activity::mark(crate::project_activity::ALL);
        old
    }

    /// Close the project as the app exits: an untitled one's recovery directory is deleted only
    /// when it holds no work. The close guard asks about unsaved work, but it can be bypassed (a
    /// dialog that fails, a quit that skips it), and what it calls unsaved is narrower than what
    /// a recovery offers; anything left is offered at the next launch.
    pub fn close_at_exit(self) {
        let keep = match (&self.recovery, self.recovery.as_ref().map(|r| r.project_path())) {
            (Some(dir), Some(path)) => match crate::recovery::summarize_conn(&self.conn, dir.id(), &path) {
                Ok(info) => info.is_some(),
                Err(e) => {
                    tracing::warn!(error = %e, "could not tell whether the untitled project holds work; keeping it");
                    true
                }
            },
            _ => false,
        };
        if keep {
            if let Some(id) = self.recovery_id() {
                tracing::info!(session = id, "keeping the untitled project for recovery");
            }
            drop(self);
        } else {
            self.close();
        }
    }

    /// Which project this is, as a count of the times [`Project::replace_with`] swapped one in (an
    /// open or a new project). A job that resolves its inputs under the lock notes it, runs with
    /// the lock released, and drops its result if it has changed by the time it comes back: the
    /// result is for a project that is no longer open.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A count of every row this project's connection has inserted, updated or deleted since it
    /// was opened — analysis merged in, the ripple flag, meta, a declined proxy, not only edits.
    /// It never goes down, **not even across [`Project::replace_with`]** (a swapped-in project's
    /// count carries on from the one it replaced, so a number noted under the old project can
    /// never equal one read under the new), and so "did anything write while the lock was
    /// released?" is a comparison of two numbers instead of a fingerprint of the whole project. A
    /// write that rolled back still counts: the answer errs towards "something changed". It is
    /// still [`Project::generation`] that says the project itself is another one.
    pub fn change_count(&self) -> u64 {
        self.change_base.saturating_add(self.conn.total_changes())
    }

    /// Set who subsequent edits are attributed to in the history. The MCP server
    /// calls this with [`EditSource::Agent`]; the desktop app keeps the default
    /// [`EditSource::User`]. Setting the actor forgets which agent session the last call declared
    /// ([`Project::ensure_agent_staging`]), so a lock never inherits it.
    pub fn set_actor(&mut self, actor: EditSource) {
        self.actor = actor;
        self.agent_owner.get_mut().take();
    }

    /// Hold an agent's edits for review (the default): every edit made as [`EditSource::Agent`]
    /// lands in a staged proposal, opened by its first edit (or by claiming a task), instead of on
    /// the cut the user is looking at. Off, an agent edits the live cut directly — task work
    /// included: claiming a task opens no proposal, and each edit is an ordinary undoable revision
    /// attributed to the agent. Only a proposal the agent opens on purpose
    /// ([`Project::begin_staging`], the `stage_edits` tool) holds back its edits, and one that is
    /// already open when the switch flips is left exactly as it is: it keeps taking the agent's
    /// edits (its reads follow it, so they must not split between it and the live cut) until it is
    /// applied or discarded, and nothing applies it on the switch's account. The app sets this from
    /// its settings; it is not stored in the project file.
    pub fn set_agent_review(&self, on: bool) {
        self.agent_review.set(on);
    }

    /// Whether an agent's edits are held for review ([`Project::set_agent_review`]).
    pub fn agent_review(&self) -> bool {
        self.agent_review.get()
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
    ///
    /// Atomic: the snapshot is written to a temporary file **beside** the destination, synced, and
    /// renamed over it only once it is whole, so a failure (a full disk, a crash, a locked file)
    /// leaves the destination as it was — a project that already lives there is never deleted
    /// first. Saving a project onto its own file is a no-op: every edit is already written through.
    ///
    /// The destination is held to the same rules as an export's output
    /// ([`Project::check_output_path`]): a project saved over an asset's source file, a stitched
    /// capture or a proxy would destroy media the cut still needs, so that is refused before
    /// anything is written.
    pub fn save_as(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if self.path.as_deref().is_some_and(|own| crate::paths::same_file(own, path)) {
            return Ok(());
        }
        self.check_output_path(path)?;
        // Only what a title uses is saved: the fonts of a template turned down are not carried along.
        self.collect_unused_fonts_now();
        store::vacuum_into_atomic(&self.conn, path)
    }

    /// An in-memory project seeded with demo assets, analysis, and a timeline.
    pub fn sample() -> Result<Self> {
        let project = Self::open_in_memory()?;
        project.seed_sample()?;
        Ok(project)
    }

    fn init(&mut self) -> Result<()> {
        // Every row written through this connection moves the plan epoch (but for the background
        // tables), so a caller holding the inputs of a frame can tell they are stale without the lock,
        // and marks its area for the activity stream (`project_activity`).
        self.conn.update_hook(Some(|_action, _db: &str, table: &str, _row| {
            if crate::plan_epoch::table_moves_plan(table) {
                crate::plan_epoch::bump_plan_epoch();
            }
            crate::project_activity::mark(crate::project_activity::table_area(table));
        }))?;
        crate::plan_epoch::bump_plan_epoch();

        // `PRAGMA user_version` is the file's schema (`store::MIGRATIONS`). A newer one is refused
        // before anything is written; an older one is copied aside, then upgraded a step at a time.
        let found = store::user_version(&self.conn)?;
        store::check_supported(found)?;
        let blank = store::is_blank(&self.conn)?;
        if !blank && self.conn.is_readonly(rusqlite::MAIN_DB)? {
            // A write-protected file opens as it is: nothing is stamped, backed up or added. One
            // that needs a real upgrade cannot be read by this build without writing to it.
            if found < store::SCHEMA_VERSION && store::needs_upgrade(&self.conn, found) {
                return Err(Error::Other(
                    "this project was made by an older Kerf and needs upgrading, but its file is read-only — copy it somewhere you can write to and open the copy".into(),
                ));
            }
            self.load_fonts_logged();
            return Ok(());
        }
        if !blank && found < store::SCHEMA_VERSION {
            if let Some(path) = &self.path {
                store::backup_before_upgrade(&self.conn, path, found)?;
            }
        }
        self.conn.execute_batch(SCHEMA)?;
        if blank {
            store::stamp_current(&self.conn)?;
        } else {
            store::migrate(&self.conn, found)?;
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
        // `kerf_version` is the build that made the file; this is the last one that opened it to write.
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('last_kerf_version', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value WHERE value <> excluded.value",
            params![env!("CARGO_PKG_VERSION")],
        )?;
        // Fonts no title names any more are dropped now (the file, not the process's fonts: this project
        // is not the open one until it is swapped in).
        if let Err(e) = self.collect_unused_fonts() {
            tracing::warn!(error = %e, "could not tell which embedded fonts are unused");
        }
        self.load_fonts_logged();
        Ok(())
    }

    /// Opening a project reads the fonts it embeds into memory, **not into the process's registry**: they
    /// become the ones titles are drawn with when it becomes the open project ([`Project::replace_with`],
    /// [`Project::activate_fonts`]), so an open that is cancelled or refused changes nothing about how
    /// the open project draws. A project whose fonts cannot be read still opens: its titles fall back to
    /// the installed ones.
    fn load_fonts_logged(&self) {
        if let Err(e) = self.load_fonts() {
            tracing::warn!(error = %e, "could not read the fonts this project embeds");
        }
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
    /// revision and does not touch the timeline. It is the user's toolbar setting and applies at
    /// once, also to a proposal being built, so an agent may not flip it while the user's review
    /// of agent edits is on (it passes `ripple` on the edit instead, see [`Project::with_ripple`]).
    pub fn set_ripple_mode(&self, on: bool) -> Result<()> {
        if self.actor == EditSource::Agent && self.agent_review.get() {
            return Err(Error::InvalidArgument(
                "the ripple setting is the user's while they review agent edits: pass `ripple` on the edit for one call, or ask them to switch it"
                    .to_string(),
            ));
        }
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

    // ---- timeline ---------------------------------------------------------

    pub fn timeline(&self) -> Result<Timeline> {
        // Inside a batch the timeline its edits left is held parsed, and is the live one.
        if let Some(held) = self.held_timeline() {
            return Ok(held);
        }
        Ok(serde_json::from_str(&self.timeline_json()?)?)
    }

    /// The live timeline's raw stored JSON, byte-for-byte as last written by
    /// [`Self::save_timeline_str`] — used to compare content against a staged
    /// row's `base` (itself the same raw string, see [`Self::begin_staging`])
    /// without round-tripping either through `Timeline` first.
    fn timeline_json(&self) -> Result<String> {
        self.flush_held_timeline()?;
        Ok(self
            .conn
            .query_row("SELECT data FROM timeline WHERE id = 1", [], |r| r.get(0))?)
    }

    /// Begin a write that is all or nothing: a transaction of its own, or a savepoint inside the one
    /// already open (a [`Project::batch`]'s), so an edit made in a batch rolls back alone or with it.
    fn tx(&self) -> Result<Tx<'_>> {
        let nested = !self.conn.is_autocommit();
        self.conn
            .execute_batch(if nested { "SAVEPOINT kerf_edit" } else { "BEGIN DEFERRED" })?;
        Ok(Tx {
            conn: &self.conn,
            nested,
            open: true,
        })
    }

    pub fn save_timeline(&self, timeline: &Timeline) -> Result<()> {
        self.save_timeline_str(&timeline_to_json(timeline)?)
    }

    /// Persist a pre-serialized timeline blob, so callers that already hold the
    /// JSON (an edit + its history snapshot) don't serialize the same timeline
    /// twice.
    fn save_timeline_str(&self, json: &str) -> Result<()> {
        // A write that is not one of a batch's own edits replaces what the batch held.
        self.drop_held_timeline();
        self.write_timeline_str(json)
    }

    fn write_timeline_str(&self, json: &str) -> Result<()> {
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
        let notes = &self.edit_notes;
        notes.borrow_mut().clear();
        let carried = &self.edit_carried;
        carried.borrow_mut().clear();
        // The lanes the op asked to have checked, registered just before it called in.
        let lanes: Vec<Uuid> = self.edit_lanes.take();
        let f = move |timeline: &mut Timeline| -> Result<R> {
            // The sync lock and guard: with links in force and something linked, the
            // partners of what the edit moved follow it, and an edit that would
            // leave an in-step picture and sound apart is refused. A project that
            // links nothing skips all of it, and the snapshot with it.
            let sync = links && timeline.has_links();
            let before = (ripple || sync || !lanes.is_empty()).then(|| timeline.clone());
            // What an older build left broken is not this edit's to answer for.
            let known = timeline.known_violations();
            let result = f(timeline)?;
            if let Some(before) = &before {
                // What the edit itself left, before the per-lane ripple moves each track
                // by its own length: which of the clips it named it *moved apart* is
                // judged there.
                let left = (ripple && sync).then(|| timeline.clone());
                if ripple {
                    *timeline = timeline.ripple_lanes(before);
                }
                if sync {
                    let mut trimmed = Vec::new();
                    timeline.conform_links_noted(before, &anchors, &HashMap::new(), left.as_ref(), &mut trimmed)?;
                    notes.borrow_mut().extend(trimmed);
                    // The partners a trim carried, now that the ripple has made what room it will.
                    timeline.check_carried_lanes(before, &carried.take())?;
                    if let Some((a, b)) = timeline.first_sync_break(before) {
                        return Err(Error::InvalidArgument(format!(
                            "that edit would leave the linked clips on {a} and {b} out of step with each other — unlink them first if they are meant to part"
                        )));
                    }
                }
                // The clips the op itself put somewhere (a trimmed or a new clip) must not sit on
                // another clip — after the ripple, which may have made the room.
                timeline.check_own_lanes(before, &lanes)?;
            }
            if timeline.has_links() {
                timeline.dissolve_all_orphans();
            }
            timeline.check_new(&known)?;
            Ok(result)
        };
        // While the agent has a staging session open its edits go to the
        // proposal instead, leaving the cut the user is looking at alone — and
        // with review on, its first edit opens one. The GUI never stages, so a
        // user edit always lands live — and makes any open proposal `stale`.
        if self.actor == EditSource::Agent {
            let declared = self.agent_owner.borrow().clone();
            if let Some(row) = self.staged_row()? {
                if declared.is_some() && owned_by_other(row.owner.as_deref(), declared.as_deref()) {
                    return Err(Error::StagedEditOwnedByOther);
                }
                // A lock is the user's, and the user can set one while a proposal is open: the
                // proposal's edits honour the live cut's locks as they are now, laid over it for the
                // edit and taken off again, so the proposal does not record the user's lock as a
                // change of its own (one the agent made itself stays).
                let live_locks = self.live_locks()?;
                let base_locks = locked_ids(&serde_json::from_str(&row.base)?);
                let f = move |timeline: &mut Timeline| -> Result<R> {
                    let own = lay_effective_locks(timeline, &base_locks, &live_locks);
                    let out = f(timeline);
                    for track in &mut timeline.tracks {
                        if let Some(&was) = own.get(&track.id) {
                            if track.locked == effective_lock(was, &track.id, &base_locks, &live_locks) {
                                track.locked = was;
                            }
                        }
                    }
                    out
                };
                let tx = self.tx()?;
                self.adopt_staged(row.owner.as_deref())?;
                let out = self.edit_staged(row, label, f)?;
                tx.commit()?;
                return Ok(out);
            }
            if self.agent_review.get() {
                // The session opens in the same transaction as the edit it was opened for, so an
                // edit that fails leaves no empty proposal behind.
                let tx = self.tx()?;
                self.open_session(None, None, declared.as_deref())?;
                let row = self.staged_row()?.ok_or(Error::NoStagedEdit)?;
                let out = self.edit_staged(row, label, f)?;
                tx.commit()?;
                return Ok(out);
            }
        }
        let tx = self.tx()?;
        let mut timeline = self.timeline()?;
        let result = f(&mut timeline)?;
        if self.in_batch() {
            // A batch writes its timeline once, at its end: until then the next edit starts from this one, held
            // parsed, instead of reading, parsing, serializing and rewriting the whole blob again.
            check_finite(&timeline)?;
            let (label, notes) = (label(&result), self.edit_notes.take());
            self.hold_timeline(timeline);
            self.note_batch_step(&label, &notes, false);
        } else {
            let json = timeline_to_json(&timeline)?;
            self.save_timeline_str(&json)?;
            let (label, notes) = (label(&result), self.edit_notes.take());
            self.record_revision(&format!("{label}{}", trimmed_suffix(&notes)), self.actor, &json)?;
        }
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
        if let Some(links) = self.held_has_links() {
            return Ok(links);
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
    ///
    /// The user's track locks are read off the live cut as they are now and laid over the proposal (the
    /// stored proposal is left as it was): the user can lock a track while the agent works, and the agent
    /// has to see the lock it will be refused by.
    pub fn working_timeline(&self) -> Result<Timeline> {
        if self.actor == EditSource::Agent {
            if let Some(row) = self.staged_row()? {
                let mut timeline: Timeline = serde_json::from_str(&row.timeline)?;
                let base = locked_ids(&serde_json::from_str(&row.base)?);
                lay_effective_locks(&mut timeline, &base, &self.live_locks()?);
                return Ok(timeline);
            }
        }
        self.timeline()
    }

    /// The tracks locked on the live cut.
    fn live_locks(&self) -> Result<HashSet<Uuid>> {
        Ok(locked_ids(&self.timeline()?))
    }

    fn staged_row(&self) -> Result<Option<StagedRow>> {
        self.conn
            .query_row(
                "SELECT base_seq, base, timeline, edits, task_id, note, created_at, updated_at, owner, assets
                 FROM staged WHERE id = 1",
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
                        r.get::<_, Option<String>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(base_seq, base, timeline, edits, task_id, note, created_at, updated_at, owner, assets)| {
                    Ok(StagedRow {
                        base_seq,
                        base,
                        timeline,
                        edits: serde_json::from_str(&edits)?,
                        task_id: task_id.as_deref().map(parse_uuid).transpose()?,
                        note,
                        created_at: parse_dt(&created_at)?,
                        updated_at: parse_dt(&updated_at)?,
                        owner,
                        assets: assets.as_deref().map(serde_json::from_str).transpose()?.unwrap_or_default(),
                        discarded_by_user: false,
                    })
                },
            )
            .transpose()
    }

    /// Whether a proposal is open — a lookup that loads none of it.
    fn staged_exists(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM staged WHERE id = 1)", [], |r| r.get(0))?)
    }

    /// The asset changes an open proposal holds back, as the agent should see them — empty for
    /// anyone else, and when nothing is staged.
    fn pending_asset_changes(&self) -> Result<Vec<StagedAssetChange>> {
        if self.actor != EditSource::Agent {
            return Ok(Vec::new());
        }
        let json: Option<Option<String>> = self
            .conn
            .query_row("SELECT assets FROM staged WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        match json.flatten() {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Vec::new()),
        }
    }

    /// Apply an edit to the pending proposal rather than the live timeline,
    /// appending its label to the running list of what has been staged. Runs inside the
    /// caller's transaction.
    fn edit_staged<R>(
        &self,
        row: StagedRow,
        label: impl FnOnce(&R) -> String,
        f: impl FnOnce(&mut Timeline) -> Result<R>,
    ) -> Result<R> {
        let mut timeline: Timeline = serde_json::from_str(&row.timeline)?;
        let result = f(&mut timeline)?;
        let json = timeline_to_json(&timeline)?;
        let (label, notes) = (label(&result), self.edit_notes.take());
        match self.note_batch_step(&label, &notes, true) {
            Some(false) => self.continue_staged_step(&json)?,
            _ => self.push_staged_step(&row, &json, &row.assets, format!("{label}{}", trimmed_suffix(&notes)))?,
        }
        Ok(result)
    }

    /// Move the proposal one step on: remember where it was (so [`Project::undo_staged`] can come
    /// back), store its new timeline and held-back asset changes, and add the step's label.
    fn push_staged_step(&self, row: &StagedRow, timeline: &str, assets: &[StagedAssetChange], label: String) -> Result<()> {
        self.conn.execute(
            "INSERT INTO staged_undo (timeline, assets) VALUES (?1, ?2)",
            params![row.timeline, serde_json::to_string(&row.assets)?],
        )?;
        self.conn.execute(
            "DELETE FROM staged_undo WHERE seq NOT IN (SELECT seq FROM staged_undo ORDER BY seq DESC LIMIT ?1)",
            params![STAGED_UNDO_DEPTH as i64],
        )?;
        let mut edits = row.edits.clone();
        edits.push(label);
        self.conn.execute(
            "UPDATE staged SET timeline = ?1, assets = ?2, edits = ?3, updated_at = ?4 WHERE id = 1",
            params![
                timeline,
                serde_json::to_string(assets)?,
                serde_json::to_string(&edits)?,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Open a staging session branched from the live timeline: from here until
    /// [`Self::apply_staged`] or [`Self::discard_staged`], agent edits are held
    /// back for review instead of changing the cut the user sees. `owner` is the agent session
    /// (its MCP session id) the proposal belongs to; another session's edits are refused while it
    /// is open.
    pub fn begin_staging(&self, task_id: Option<Uuid>, note: Option<&str>, owner: Option<&str>) -> Result<StagedEdit> {
        self.open_session(task_id, note, owner)?;
        self.staged()?.ok_or(Error::NoStagedEdit)
    }

    fn open_session(&self, task_id: Option<Uuid>, note: Option<&str>, owner: Option<&str>) -> Result<()> {
        if self.staged_exists()? {
            return Err(Error::StagedEditPending);
        }
        let snapshot = self.timeline_json()?;
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO staged (id, base_seq, base, timeline, edits, task_id, note, created_at, updated_at, owner, assets)
             VALUES (1, ?1, ?2, ?2, '[]', ?3, ?4, ?5, ?5, ?6, '[]')",
            params![self.head()?, snapshot, task_id.map(|id| id.to_string()), note, now, owner],
        )?;
        self.conn.execute("DELETE FROM staged_undo WHERE 1", [])?;
        Ok(())
    }

    /// Say which agent session the current lock belongs to, and nothing more: no proposal is
    /// opened and no ownership is checked here. What a tool that only *ends* a proposal (apply,
    /// discard, undo a step) calls, so the owner rule can tell its caller from a stranger;
    /// [`Project::ensure_agent_staging`] does this and refuses a stranger early for an edit.
    /// Forgotten at the next [`Project::set_actor`], i.e. every lock. A no-op for the user.
    pub fn declare_agent(&self, owner: Option<&str>) {
        if self.actor == EditSource::Agent {
            *self.agent_owner.borrow_mut() = owner.map(str::to_string);
        }
    }

    /// What the adapter calls, inside the same lock as the edit it is about to make, to say which
    /// agent session is acting and to refuse it early when the proposal is another's.
    ///
    /// It **declares and checks, and writes nothing**: a proposal **owned by a different session**
    /// is refused with [`Error::StagedEditOwnedByOther`] — one agent must not fold its edits into,
    /// apply or discard another's pending work (a caller that names no session counts as different
    /// from an owner) — and that is all. The session itself is opened (with review on and none
    /// open), or adopted (a claimed task's proposal has no owner yet; the first named session to
    /// edit it takes it), *by the edit*, inside the edit's own transaction: a call that opened it
    /// here would leave an empty proposal owned by the agent behind whenever the edit then failed.
    /// A no-op for the user's own edits.
    pub fn ensure_agent_staging(&self, owner: Option<&str>) -> Result<()> {
        if self.actor != EditSource::Agent {
            return Ok(());
        }
        self.declare_agent(owner);
        let row_owner: Option<Option<String>> = self
            .conn
            .query_row("SELECT owner FROM staged WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        match row_owner {
            Some(Some(held)) if owner != Some(held.as_str()) => Err(Error::StagedEditOwnedByOther),
            _ => Ok(()),
        }
    }

    /// Let go of a proposal whose agent session is gone. `gone` is asked about the session that
    /// owns the open proposal (if any) and says whether that connection no longer exists — it
    /// closed, expired, or the server was restarted (a new token). Such a proposal is nobody's:
    /// its owner is cleared, so the agent that reconnected, or the next named session to edit it,
    /// takes it over ([`Project::ensure_agent_staging`]) instead of being told another agent owns
    /// it. Returns whether an owner was released. Writes nothing when there is no owner.
    pub fn release_orphaned_staging(&self, gone: impl FnOnce(&str) -> bool) -> Result<bool> {
        let owner: Option<Option<String>> = self
            .conn
            .query_row("SELECT owner FROM staged WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        let Some(Some(owner)) = owner else { return Ok(false) };
        if !gone(&owner) {
            return Ok(false);
        }
        self.conn
            .execute("UPDATE staged SET owner = NULL WHERE id = 1 AND owner = ?1", params![owner])?;
        Ok(true)
    }

    /// Give a proposal that has no owner to the session the current call declared, if it named
    /// one. Part of the transaction of the edit that is staging into it.
    fn adopt_staged(&self, row_owner: Option<&str>) -> Result<()> {
        if row_owner.is_none() {
            if let Some(owner) = self.agent_owner.borrow().as_deref() {
                self.conn
                    .execute("UPDATE staged SET owner = ?1 WHERE id = 1 AND owner IS NULL", params![owner])?;
            }
        }
        Ok(())
    }

    /// Refuse an agent that is not the proposal's owner.
    fn check_owner(&self, row_owner: Option<&str>) -> Result<()> {
        if self.actor == EditSource::Agent && owned_by_other(row_owner, self.agent_owner.borrow().as_deref()) {
            return Err(Error::StagedEditOwnedByOther);
        }
        Ok(())
    }

    /// Who may land a proposal: the user, always; an agent only with review off, and only its own.
    fn check_may_apply(&self, row: &StagedRow) -> Result<()> {
        if self.actor == EditSource::Agent {
            if self.agent_review.get() {
                return Err(Error::AgentReviewRequired);
            }
            self.check_owner(row.owner.as_deref())?;
        }
        Ok(())
    }

    /// The pending proposal — what it would change, and whether the live
    /// timeline has moved on underneath it — or `None` when nothing is staged.
    pub fn staged(&self) -> Result<Option<StagedEdit>> {
        let Some(row) = self.staged_row()? else {
            return Ok(None);
        };
        let undo_depth: i64 = self.conn.query_row("SELECT COUNT(*) FROM staged_undo", [], |r| r.get(0))?;
        Ok(Some(self.staged_view(row, undo_depth as usize)?))
    }

    fn staged_view(&self, row: StagedRow, undo_depth: usize) -> Result<StagedEdit> {
        let base: Timeline = serde_json::from_str(&row.base)?;
        let proposed: Timeline = serde_json::from_str(&row.timeline)?;
        Ok(StagedEdit {
            base_seq: row.base_seq,
            task_id: row.task_id,
            note: row.note,
            owned_by_agent: row.owner.is_some(),
            owner: row.owner,
            edits: row.edits,
            undo_depth,
            asset_changes: row.assets,
            created_at: row.created_at,
            updated_at: row.updated_at,
            // Compared by content, not `base_seq` against the current head: an
            // undo followed by a new edit reinserts a *different* revision at
            // the same seq, which a seq comparison alone would miss.
            stale: self.timeline_json()? != row.base,
            diff: base.diff(&proposed),
        })
    }

    /// The staged timeline itself, for previewing the proposal.
    pub fn staged_timeline(&self) -> Result<Option<Timeline>> {
        match self.staged_row()? {
            Some(row) => Ok(Some(serde_json::from_str(&row.timeline)?)),
            None => Ok(None),
        }
    }

    /// Take back the last step of the proposal — the label of the step and the proposal as it
    /// stands without it. Steps are remembered back [`STAGED_UNDO_DEPTH`] deep, across edits to the
    /// timeline and held-back asset changes alike; the live cut and its history are not touched.
    pub fn undo_staged(&self) -> Result<(String, StagedEdit)> {
        let row = self.staged_row()?.ok_or(Error::NoStagedEdit)?;
        self.check_owner(row.owner.as_deref())?;
        let step: Option<(i64, String, String)> = self
            .conn
            .query_row(
                "SELECT seq, timeline, assets FROM staged_undo ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((seq, timeline, assets)) = step else {
            return Err(Error::InvalidArgument(format!(
                "nothing left to undo in the proposal (it remembers the last {STAGED_UNDO_DEPTH} steps)"
            )));
        };
        let mut edits = row.edits;
        let label = edits.pop().unwrap_or_else(|| "edit".to_string());
        let tx = self.tx()?;
        self.conn.execute(
            "UPDATE staged SET timeline = ?1, assets = ?2, edits = ?3, updated_at = ?4 WHERE id = 1",
            params![timeline, assets, serde_json::to_string(&edits)?, Utc::now().to_rfc3339()],
        )?;
        self.conn.execute("DELETE FROM staged_undo WHERE seq = ?1", params![seq])?;
        tx.commit()?;
        let view = self.staged()?.ok_or(Error::NoStagedEdit)?;
        Ok((label, view))
    }

    /// Accept the proposal: it becomes the live timeline as a **single**
    /// revision, and the staging session closes. The asset changes it held back are written with
    /// it.
    ///
    /// Refuses a `stale` proposal unless `force` — one branched from a cut the
    /// user has since edited would silently replace that newer work. Called as the agent, it is
    /// refused while review is on ([`Error::AgentReviewRequired`]): accepting is the user's, and
    /// `force` is no way round it.
    pub fn apply_staged(&self, force: bool) -> Result<Timeline> {
        let row = self.staged_row()?.ok_or(Error::NoStagedEdit)?;
        self.check_may_apply(&row)?;
        // Content, not `base_seq`: `seq` is recycled by `record_revision` once
        // an undo branches off an earlier point, so a plain seq comparison
        // would miss "undo, then make an unrelated edit" landing right back on
        // the seq the proposal was staged from with completely different
        // content underneath it.
        let stale = self.timeline_json()? != row.base;
        if stale && !force {
            return Err(Error::StagedEditStale);
        }
        let base: Timeline = serde_json::from_str(&row.base)?;
        let mut proposed: Timeline = serde_json::from_str(&row.timeline)?;
        let diff = base.diff(&proposed);
        // Forced over newer work, the locks are the user's as they are now (and those the proposal added):
        // a lock it only inherited, which the user has since lifted, is not put back.
        let landing = if stale {
            lay_effective_locks(&mut proposed, &locked_ids(&base), &self.live_locks()?);
            timeline_to_json(&proposed)?
        } else {
            row.timeline.clone()
        };
        // A session that staged nothing (or staged and undid it) just closes —
        // recording a revision here would put an edit that changed nothing into
        // the user's history.
        if diff.is_empty() && row.assets.is_empty() {
            self.clear_staged()?;
            // What was staged and undone left its fonts behind, and nothing names them now.
            self.collect_unused_fonts_now();
            return self.timeline();
        }
        let label = match (&row.note, row.edits.as_slice()) {
            (Some(note), _) => note.clone(),
            (None, [one]) => one.clone(),
            (None, edits) => format!("Agent edit ({} of {} changes)", diff.entries.len(), edits.len()),
        };
        let tx = self.tx()?;
        if !diff.is_empty() {
            self.save_timeline_str(&landing)?;
            self.record_revision(&label, EditSource::Agent, &landing)?;
        }
        for change in &row.assets {
            self.apply_asset_change(change)?;
        }
        self.clear_staged()?;
        self.forget_discarded()?;
        tx.commit()?;
        if diff.is_empty() {
            return self.timeline();
        }
        Ok(proposed)
    }

    /// Accept only some of the proposal's changes: the entries of [`StagedEdit::diff`] (and the
    /// held-back asset changes of [`StagedEdit::asset_changes`]) named by `entry_ids`, each
    /// written onto the live cut by the clip, text, marker or track it is about — one revision.
    ///
    /// What cannot be taken apart is not guessed at. A linked partner's change, the track a
    /// picked clip lands on, and the moves that take clips off a track the proposal removes come
    /// with the pick and are **reported** ([`SubsetApplied::auto_included`]); a pick that would put
    /// clips on top of each other or pull linked clips out of step is refused with where
    /// ([`Timeline::merge_subset`]). A pick whose target the user has since removed is skipped and
    /// listed, not fatal.
    ///
    /// What was not picked stays: the session carries on branched from the cut as it now is, with
    /// only the rest of the changes left to review (it closes when nothing is left). Stale and
    /// agent rules are those of [`Project::apply_staged`] — as the agent it is refused while review
    /// is on.
    pub fn apply_staged_subset(&self, entry_ids: &[String], force: bool) -> Result<SubsetApplied> {
        let row = self.staged_row()?.ok_or(Error::NoStagedEdit)?;
        self.check_may_apply(&row)?;
        let live_json = self.timeline_json()?;
        if live_json != row.base && !force {
            return Err(Error::StagedEditStale);
        }
        if entry_ids.is_empty() {
            return Err(Error::InvalidArgument("pick at least one change to apply".to_string()));
        }
        let base: Timeline = serde_json::from_str(&row.base)?;
        let proposed: Timeline = serde_json::from_str(&row.timeline)?;
        let live: Timeline = serde_json::from_str(&live_json)?;
        let all_entries = base.diff(&proposed).entries;
        let total = all_entries.len() + row.assets.len();

        let (asset_picks, timeline_picks): (Vec<&String>, Vec<&String>) =
            entry_ids.iter().partition(|id| row.assets.iter().any(|c| &c.id == *id));
        let timeline_picks: Vec<String> = timeline_picks.into_iter().cloned().collect();
        let merge = if timeline_picks.is_empty() {
            None
        } else {
            Some(live.merge_subset(&base, &proposed, &timeline_picks)?)
        };
        let applied_assets: Vec<&StagedAssetChange> = row.assets.iter().filter(|c| asset_picks.contains(&&c.id)).collect();
        let applied = merge.as_ref().map_or(0, |m| m.applied.len()) + applied_assets.len();
        let skipped = merge.as_ref().map(|m| m.skipped.clone()).unwrap_or_default();
        if applied == 0 {
            let why = skipped
                .iter()
                .map(|s| format!("{} ({})", s.summary, s.why))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::InvalidArgument(format!("none of those changes can be applied: {why}")));
        }
        let merged_json = match &merge {
            Some(m) => timeline_to_json(&m.timeline)?,
            None => live_json.clone(),
        };

        let label = match &row.note {
            Some(note) => format!("{note} ({applied} of {total})"),
            None => format!("Agent edit ({applied} of {total} changes)"),
        };
        let tx = self.tx()?;
        if merged_json != live_json {
            self.save_timeline_str(&merged_json)?;
            self.record_revision(&label, EditSource::Agent, &merged_json)?;
        }
        for change in &applied_assets {
            self.apply_asset_change(change)?;
        }
        self.forget_discarded()?;
        let remaining_assets: Vec<StagedAssetChange> =
            row.assets.iter().filter(|c| !asset_picks.contains(&&c.id)).cloned().collect();
        let merged: Timeline = serde_json::from_str(&merged_json)?;
        // What is left of the proposal is the changes that were neither applied nor skipped (a skipped
        // pick no longer fits the cut), laid on the cut *as it now is*. The proposal's own timeline is
        // not kept as it was: after a forced accept it lacks whatever the user did since it branched,
        // and a remainder that was "the proposal against the new cut" would read as — and apply as —
        // undoing that work.
        let done: HashSet<&str> = merge
            .iter()
            .flat_map(|m| {
                m.applied
                    .iter()
                    .map(String::as_str)
                    .chain(m.skipped.iter().map(|s| s.id.as_str()))
            })
            .collect();
        let rest: Vec<String> = all_entries
            .iter()
            .filter(|e| !done.contains(e.id.as_str()))
            .map(|e| e.id.clone())
            .collect();
        let (rest_timeline, dropped) = if rest.is_empty() {
            (merged.clone(), 0)
        } else {
            let again = merged.rebase_rest(&base, &proposed, &rest)?;
            let dropped = again.skipped.len();
            (again.timeline, dropped)
        };
        let remaining = merged.diff(&rest_timeline).entries.len() + remaining_assets.len();
        let closed = remaining == 0;
        if closed {
            self.clear_staged()?;
        } else {
            // The rest of the proposal now branches from the cut as it stands.
            self.conn.execute(
                "UPDATE staged SET base = ?1, base_seq = ?2, timeline = ?3, assets = ?4, updated_at = ?5 WHERE id = 1",
                params![
                    merged_json,
                    self.head()?,
                    timeline_to_json(&rest_timeline)?,
                    serde_json::to_string(&remaining_assets)?,
                    Utc::now().to_rfc3339()
                ],
            )?;
            self.conn.execute("DELETE FROM staged_undo WHERE 1", [])?;
        }
        tx.commit()?;

        let auto_included = merge.as_ref().map(|m| m.auto_included.clone()).unwrap_or_default();
        let mut message = format!("Applied {applied} of {total} change{}", if total == 1 { "" } else { "s" });
        if !auto_included.is_empty() {
            message.push_str(&format!(
                " — {} came with the ones you picked ({})",
                auto_included.len(),
                auto_included
                    .iter()
                    .map(|a| a.summary.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if !skipped.is_empty() {
            message.push_str(&format!("; {} no longer apply", skipped.len()));
        }
        if dropped > 0 {
            message.push_str(&format!(
                "; {dropped} more of the proposal no longer fit{} the cut and {} dropped from it",
                if dropped == 1 { "s" } else { "" },
                if dropped == 1 { "was" } else { "were" }
            ));
        }
        message.push_str(&if closed {
            ".".to_string()
        } else {
            format!("; {remaining} left to review.")
        });
        Ok(SubsetApplied {
            applied,
            auto_included,
            skipped,
            remaining,
            closed,
            message,
            timeline: merged,
        })
    }

    /// Write one held-back asset change to the library.
    fn apply_asset_change(&self, change: &StagedAssetChange) -> Result<()> {
        let Some(mut asset) = self.stored_asset(change.asset_id)? else {
            // Removed since: nothing left to mark.
            return Ok(());
        };
        apply_asset_change_to(&mut asset, change);
        self.insert_asset(&asset)
    }

    /// Drop the kept discarded proposal: a newer one was applied, or the project it belonged to is
    /// no longer the one open, so bringing it back would read as an undo of something else.
    fn forget_discarded(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM meta WHERE key = ?1", params![DISCARDED_STAGED])?;
        Ok(())
    }

    /// Empty the staged table. `WHERE 1` keeps SQLite from truncating it, which does not call the
    /// update hook the plan epoch is moved by.
    fn clear_staged(&self) -> Result<usize> {
        self.conn.execute("DELETE FROM staged_undo WHERE 1", [])?;
        Ok(self.conn.execute("DELETE FROM staged WHERE 1", [])?)
    }

    /// Throw the proposal away, leaving the live timeline untouched. The last proposal thrown away
    /// is kept in the project file, so a discard made by mistake can be taken back with
    /// [`Project::restore_discarded`]; an empty proposal has nothing worth keeping.
    pub fn discard_staged(&self) -> Result<Timeline> {
        let row = self.staged_row()?.ok_or(Error::NoStagedEdit)?;
        self.check_owner(row.owner.as_deref())?;
        self.discard_row(&row)?;
        self.timeline()
    }

    /// Close the open proposal `row` without asking who may, keeping it if it held anything.
    fn discard_row(&self, row: &StagedRow) -> Result<()> {
        if !row.edits.is_empty() || !row.assets.is_empty() {
            let mut kept = serde_json::to_value(row)?;
            kept["discarded_by_user"] = (self.actor != EditSource::Agent).into();
            self.set_meta(DISCARDED_STAGED, &kept.to_string())?;
        }
        self.clear_staged()?;
        // A template the proposal applied came with its fonts, stored when it was proposed: with the
        // proposal gone nothing names them (unless the live cut does).
        self.collect_unused_fonts_now();
        Ok(())
    }

    /// The proposal [`Project::discard_staged`] last threw away, as it would come back (`stale`
    /// against the cut as it is now), or `None` when there is none.
    pub fn discarded_staged(&self) -> Result<Option<StagedEdit>> {
        match self.discarded_row()? {
            Some(row) => Ok(Some(self.staged_view(row, 0)?)),
            None => Ok(None),
        }
    }

    fn discarded_row(&self) -> Result<Option<StagedRow>> {
        match self.meta(DISCARDED_STAGED)? {
            Some(json) => Ok(serde_json::from_str(&json).ok()),
            None => Ok(None),
        }
    }

    /// Bring back the proposal that was last discarded — only while no proposal is open
    /// ([`Error::StagedEditPending`] otherwise), and once: the copy is spent. It comes back as it
    /// was (its steps, its held-back asset changes) but with no undo history, and it may now be
    /// `stale`. The user's restoring it leaves it owned by no session; an agent's keeps the
    /// ownership rule — and an agent may only bring back what an agent discarded: a proposal the
    /// user threw away stays thrown away unless the user restores it.
    pub fn restore_discarded(&self) -> Result<StagedEdit> {
        if self.staged_exists()? {
            return Err(Error::StagedEditPending);
        }
        let mut row = self.discarded_row()?.ok_or(Error::NoDiscardedEdit)?;
        if self.actor == EditSource::Agent && row.discarded_by_user {
            return Err(Error::InvalidArgument(
                "the user discarded that proposal; only they can bring it back in Kerf".to_string(),
            ));
        }
        self.check_owner(row.owner.as_deref())?;
        row.owner = match self.actor {
            EditSource::Agent => self.agent_owner.borrow().clone(),
            _ => None,
        };
        // The task it belonged to may have been dismissed since.
        if let Some(task) = row.task_id {
            if self.get_task(task)?.is_none() {
                row.task_id = None;
            }
        }
        let tx = self.tx()?;
        self.conn.execute(
            "INSERT INTO staged (id, base_seq, base, timeline, edits, task_id, note, created_at, updated_at, owner, assets)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                row.base_seq,
                row.base,
                row.timeline,
                serde_json::to_string(&row.edits)?,
                row.task_id.map(|id| id.to_string()),
                row.note,
                row.created_at.to_rfc3339(),
                Utc::now().to_rfc3339(),
                row.owner,
                serde_json::to_string(&row.assets)?,
            ],
        )?;
        self.conn.execute("DELETE FROM staged_undo WHERE 1", [])?;
        self.forget_discarded()?;
        tx.commit()?;
        self.staged()?.ok_or(Error::NoStagedEdit)
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

    /// Append a revision after the current head, dropping any redo branch, then
    /// [prune](Self::prune_history) the oldest beyond the limit.
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
        self.prune_history(seq)?;
        Ok(seq)
    }

    /// Keep the newest `history_limit` revisions, up to `newest` (the head, just recorded — so the
    /// redo branch is already gone and the head is always kept). A staged proposal's `base_seq`
    /// and everything after it are kept whatever the limit. The oldest revision left is the new
    /// baseline: undo stops there and [`Self::revision_diff`] reads it as changing nothing.
    fn prune_history(&self, newest: i64) -> Result<()> {
        let mut floor = newest - self.history_limit as i64 + 1;
        let base: Option<i64> = self
            .conn
            .query_row("SELECT base_seq FROM staged WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        if let Some(base) = base {
            floor = floor.min(base);
        }
        self.conn.execute("DELETE FROM history WHERE seq < ?1", params![floor])?;
        Ok(())
    }

    /// How many revisions the history keeps from now on (at least 1, the current one); older
    /// ones are dropped as new edits land. Defaults to [`DEFAULT_HISTORY_LIMIT`].
    pub fn set_history_limit(&mut self, limit: usize) {
        self.history_limit = limit.max(1);
    }

    /// Restore the stored snapshot at `seq` as the live timeline and move the
    /// head there. Does not itself record a new revision.
    fn restore(&self, seq: i64) -> Result<Timeline> {
        // Undo/redo/revert walk the *live* history. An agent holding staged
        // edits would be moving the ground under its own proposal, so make it
        // say which it means rather than guessing.
        if self.actor == EditSource::Agent {
            if self.staged_exists()? {
                return Err(Error::InvalidArgument(
                    "the timeline history is not available while edits are staged — apply or discard them first".to_string(),
                ));
            }
            // With review on every agent edit is a proposal, so what undo would rewind is the
            // user's own work.
            if self.agent_review.get() {
                return Err(Error::InvalidArgument(
                    "the agent cannot rewind the user's timeline history while its edits are held for review — undo_staged_edit takes back its own proposed steps"
                        .to_string(),
                ));
            }
        }
        let snapshot: Option<String> = self
            .conn
            .query_row("SELECT snapshot FROM history WHERE seq = ?1", params![seq], |r| r.get(0))
            .optional()?;
        let snapshot = snapshot.ok_or(Error::RevisionNotFound(seq))?;
        let timeline: Timeline = serde_json::from_str(&snapshot)?;
        self.refuse_removed_media(&timeline)?;
        let tx = self.tx()?;
        self.save_timeline_str(&snapshot)?;
        self.set_head(seq)?;
        tx.commit()?;
        Ok(timeline)
    }

    /// Refuse a snapshot that has a clip of an asset that is no longer in the library. Restoring it
    /// would put clips on the timeline that point at nothing — they could not be previewed,
    /// exported or even removed from a library listing. The refusal names the media that was
    /// removed (its name was kept when it went) and says what still works.
    fn refuse_removed_media(&self, timeline: &Timeline) -> Result<()> {
        let mut wanted: Vec<Uuid> = Vec::new();
        for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
            if !clip.is_generated() && !wanted.contains(&clip.asset_id) {
                wanted.push(clip.asset_id);
            }
        }
        if wanted.is_empty() {
            return Ok(());
        }
        let mut stmt = self.conn.prepare("SELECT id FROM assets")?;
        let present: HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        let gone: Vec<Uuid> = wanted.into_iter().filter(|id| !present.contains(&id.to_string())).collect();
        if gone.is_empty() {
            return Ok(());
        }
        let removed = self.removed_asset_names()?;
        let names: Vec<String> = gone
            .iter()
            .map(|id| {
                removed
                    .iter()
                    .find(|(k, _)| *k == id.to_string())
                    .map_or_else(|| id.to_string(), |(_, name)| name.clone())
            })
            .collect();
        Err(Error::InvalidArgument(format!(
            "that step uses media that was removed from the library ({}) — undo and redo cannot bring it back; import the file again to cut from it, or go back to a step from before it was used",
            names.join(", ")
        )))
    }

    /// Remember that `asset` was taken out of the library, by name, so a refused undo can say
    /// which media it misses. Bounded: the oldest are forgotten first (their ids then show).
    fn note_removed_asset(&self, asset: &Asset) -> Result<()> {
        let mut removed = self.removed_asset_names()?;
        let id = asset.id.to_string();
        removed.retain(|(k, _)| *k != id);
        removed.push((id, asset.name.clone()));
        if removed.len() > REMOVED_ASSETS_KEPT {
            removed.drain(..removed.len() - REMOVED_ASSETS_KEPT);
        }
        self.set_meta(REMOVED_ASSETS, &serde_json::to_string(&removed)?)
    }

    fn removed_asset_names(&self) -> Result<Vec<(String, String)>> {
        Ok(self
            .meta(REMOVED_ASSETS)?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default())
    }

    /// The full edit history, oldest first; the entry matching the head is `current`.
    pub fn history(&self) -> Result<Vec<Revision>> {
        Ok(self.history_page(Page::default())?.0)
    }

    /// A window onto the history, with how many revisions there are in all: `page.offset` of the
    /// newest are skipped and up to `page.limit` of the ones before them returned, oldest first
    /// like [`Project::history`] — so the default page is everything, and `limit: 20` is the
    /// last twenty edits.
    pub fn history_page(&self, page: Page) -> Result<(Vec<Revision>, usize)> {
        let head = self.head()?;
        let total: i64 = self.conn.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT seq, label, source, created_at FROM
                 (SELECT seq, label, source, created_at FROM history ORDER BY seq DESC LIMIT ?1 OFFSET ?2)
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![page.sql_limit(), page.offset as i64], |row| {
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
        Ok((revisions, total as usize))
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
        // The oldest revision a pruned history kept is its baseline too.
        let previous = match self.revision_timeline(seq - 1) {
            Ok(previous) if seq > 0 => previous,
            Err(Error::RevisionNotFound(_)) | Ok(_) => return Ok(timeline.diff(&timeline)),
            Err(e) => return Err(e),
        };
        Ok(previous.diff(&timeline))
    }

    /// Refuse `output` as a place to write a render, a still or a saved project: the source file
    /// of an asset (or a capture file one was stitched from), this project's own database and its
    /// journal, and anything inside Kerf's cache — a proxy, waveform or generated medium. Writing
    /// onto one would overwrite what the project still needs. Paths are canonicalised, so a
    /// symlink, a hard link or a `..` detour to the file is the file.
    ///
    /// Canonicalising and comparing are filesystem calls, so an adapter that renders with the
    /// lock released does not do them under it: it takes [`Project::output_path_inputs`] under the
    /// lock and runs [`Project::check_output_path_with`] after releasing it. This is the two in a
    /// row, for a caller with no lock to worry about (`export`, `export_with` and `save_as` are
    /// that caller).
    pub fn check_output_path(&self, output: &Path) -> Result<()> {
        Self::check_output_path_with(output, &self.output_path_inputs()?)
    }

    /// Every file this project names on disk: each asset's file and capture files, and each LUT a
    /// clip's grade reads in the cut, its history and an open proposal (and its undo steps). What a
    /// host that confines files to one directory (`kerf-app --headless --workspace`) checks before it
    /// takes a project in. Sorted, without repeats.
    pub fn referenced_files(&self) -> Result<ReferencedFiles> {
        self.flush_held_timeline()?;
        let mut media = Vec::new();
        for asset in self.list_assets()? {
            media.push(asset.path);
            media.extend(asset.source_paths);
        }
        let mut files = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT data FROM timeline UNION ALL SELECT snapshot FROM history UNION ALL SELECT base FROM staged
             UNION ALL SELECT timeline FROM staged UNION ALL SELECT timeline FROM staged_undo",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let timeline: Timeline = serde_json::from_str(&row?)?;
            let luts = timeline
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .filter_map(|c| c.grade.lut.as_ref());
            files.extend(luts.map(|lut| lut.path.clone()));
        }
        for list in [&mut media, &mut files] {
            list.sort();
            list.dedup();
        }
        Ok(ReferencedFiles { media, luts: files })
    }

    /// What an output path must not be, read from the database (and the environment, for the
    /// cache's place) — no file is touched. The locked half of [`Project::check_output_path`].
    pub fn output_path_inputs(&self) -> Result<crate::paths::Protected> {
        let mut protected = crate::paths::Protected::default();
        for asset in self.list_assets()? {
            protected
                .files
                .push((PathBuf::from(&asset.path), format!("the source file of \"{}\"", asset.name)));
            for capture in &asset.source_paths {
                protected
                    .files
                    .push((PathBuf::from(capture), format!("a capture file of \"{}\"", asset.name)));
            }
        }
        if let Some(db) = &self.path {
            for suffix in ["", "-journal", "-wal", "-shm"] {
                let mut name = db.as_os_str().to_os_string();
                name.push(suffix);
                protected
                    .files
                    .push((PathBuf::from(name), "this project's own file".to_string()));
            }
        }
        if let Some(cache) = crate::paths::cache_dir() {
            protected.dirs.push((
                cache.join("kerf"),
                "Kerf's cache of proxies, waveforms and generated media".to_string(),
            ));
        }
        Ok(protected)
    }

    /// The lock-free half of [`Project::check_output_path`]: `output` against what
    /// [`Project::output_path_inputs`] listed. Canonicalises and stats, so call it with the project
    /// lock released.
    pub fn check_output_path_with(output: &Path, protected: &crate::paths::Protected) -> Result<()> {
        crate::paths::check_output(output, protected)
    }

    /// Render the timeline to `output_path`. Requires the `ffmpeg` feature.
    pub fn export(&self, output_path: impl AsRef<Path>, format: &str) -> Result<PathBuf> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        let output = output_path.as_ref();
        self.check_output_path(output)?;
        engine::render(&timeline, &assets, output, format)?;
        Ok(output.to_path_buf())
    }

    /// Like [`export`] but with explicit [`engine::ExportOptions`].
    pub fn export_with(&self, output_path: impl AsRef<Path>, opts: &engine::ExportOptions) -> Result<PathBuf> {
        let timeline = self.working_timeline()?;
        let assets = self.list_assets()?;
        let output = output_path.as_ref();
        self.check_output_path(output)?;
        engine::render_with(&timeline, &assets, output, opts)?;
        Ok(output.to_path_buf())
    }
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
const ASSET_COLUMNS: &str =
    "id, path, name, duration, streams, imported_at, source_paths, voiceover, music_score, tags, sound_effect";

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
    music_score: Option<String>,
    tags: Option<String>,
    sound_effect: Option<String>,
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
        music_score: row.get(8)?,
        tags: row.get(9)?,
        sound_effect: row.get(10)?,
    })
}

/// The `tags` column for `tags`: JSON, or NULL for none.
fn tags_column(tags: &[Tag]) -> Result<Option<String>> {
    Ok(if tags.is_empty() {
        None
    } else {
        Some(serde_json::to_string(tags)?)
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
        music_score: row.music_score.map(|json| serde_json::from_str(&json)).transpose()?,
        sound_effect: row.sound_effect.map(|json| serde_json::from_str(&json)).transpose()?,
        tags: match row.tags {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        },
    })
}

/// The `staged` row as stored: timelines still serialized, so an edit that only
/// touches the proposal never pays to deserialize the base. Serializable, because a discarded
/// proposal is kept whole (`DISCARDED_STAGED`).
#[derive(serde::Serialize, serde::Deserialize)]
struct StagedRow {
    base_seq: i64,
    base: String,
    timeline: String,
    edits: Vec<String>,
    task_id: Option<Uuid>,
    note: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    assets: Vec<StagedAssetChange>,
    /// Set on the copy kept by a discard: the user threw it away, so an agent may not bring it back.
    #[serde(default)]
    discarded_by_user: bool,
}

/// What [`Project::apply_staged_subset`] did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubsetApplied {
    /// Changes applied, the ones that came along included.
    pub applied: usize,
    /// Changes applied although they were not picked, each with why it had to be.
    pub auto_included: Vec<AutoIncluded>,
    /// Picked changes that no longer apply (the user changed that part of the cut since).
    pub skipped: Vec<SkippedChange>,
    /// Changes still waiting for review in the proposal.
    pub remaining: usize,
    /// Nothing is left, so the proposal closed.
    pub closed: bool,
    /// One sentence for the toast or the agent.
    pub message: String,
    /// The live timeline afterwards.
    pub timeline: Timeline,
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

mod music;
#[cfg(test)]
mod music_tests;
pub use music::{
    AlignTo, BandBalance, BandBalanceRequest, BandPlan, BandSource, ComposePlan, ComposeRequest, Composed, ScoreAlign,
};
mod sfx;
#[cfg(test)]
mod sfx_tests;
pub use sfx::{PlannedSfx, SfxPlaced, SfxPlacement, SfxPlan, SfxUpdate, MAX_SFX_GAIN_DB, MAX_SFX_PER_CALL, MIN_SFX_GAIN_DB};
mod audio_sync;
pub use audio_sync::{AudioSync, AudioSyncMeasure, AudioSyncPlan, SyncSide, MAX_SYNC_SECONDS, SYNC_SAMPLE_RATE};
mod store;
pub use store::{backup_path as schema_backup_path, SCHEMA_VERSION};
mod exports;
pub use exports::{DeliveryInputs, ExportSizeInputs, ExportedFile};
mod batch;
pub use batch::Batch;
mod behavior;
pub use behavior::BakedClip;
mod motion;
pub use motion::MotionApplied;
mod templates;
mod title_clips;
pub use title_clips::{TitleClipPatch, TitleClipPlacement, TitleClipResult, TitleEdited};
mod tags;
pub use tags::{
    AssembleByTag, AssemblePlan, AssembleSummary, AssembledGroup, GroupTitle, PreparedAssemble, SortWithin, MAX_TAG_TITLE_SECONDS,
};
mod title_text;
pub use title_text::{PreparedText, TitleTextForm, TitleTextInputs};
mod stabilize;
pub use stabilize::{StabilizeJob, StabilizeStatusInput};
mod thumbnails;
pub use thumbnails::{prepare_thumbnail, PreparedThumbnail, ThumbnailBase, ThumbnailLayerSpec, ThumbnailResult, ThumbnailSpec};

/// A window onto a long, ordered list (the edit history, the task queue): `offset` of the
/// **newest** entries are skipped and up to `limit` of the ones before them returned, kept in
/// the list's own order. The default is everything.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Page {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
}

impl Page {
    /// The newest `limit` entries.
    pub fn last(limit: usize) -> Self {
        Self {
            limit: Some(limit),
            offset: 0,
        }
    }

    /// SQLite's `LIMIT`: negative means no limit.
    fn sql_limit(self) -> i64 {
        self.limit.map_or(-1, |l| l.min(i64::MAX as usize) as i64)
    }
}

#[cfg(test)]
mod linked_tests;

/// Cut every track, lift, extract and delete a gap: one revision each, staged, linked sound carried.
#[cfg(test)]
mod range_ops_tests;

/// The review round's project-level fixes (numbers, ranges, footage, staging, library, paths).
#[cfg(test)]
mod review_tests;

/// Task work with review off (it lands live, no proposal) and on, and the switch flipping with a
/// proposal open.
#[cfg(test)]
mod task_tests;

/// Speed ramps at the project level: one revision, the length change rippling, a linked sound, a head trim.
#[cfg(test)]
mod ramp_tests;

/// Blend modes at the project level: one revision, carried by a split / duplicate / paste, stored, diffed.
#[cfg(test)]
mod blend_tests;

/// Motion presets at the project level: one revision for several clips, staged, refused, undone.
#[cfg(test)]
mod behavior_tests;
#[cfg(test)]
mod motion_tests;

/// The Rust half of the differential corpus the browser harness is replayed against.
#[cfg(test)]
mod linked_corpus;

#[cfg(test)]
mod persist_tests;
#[cfg(test)]
mod tags_tests;

#[cfg(test)]
mod tests;
