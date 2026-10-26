//! Crash recovery for a project nobody has saved yet.
//!
//! An untitled project is a real `.kerf` file in `<app data>/recovery/<session>/`, so its edits
//! write through like a saved one's and a crash loses nothing. The session holds an OS lock on
//! `<session>/lock` for as long as it is open; a later launch offers the directories whose lock it
//! *can* take (no running Kerf owns them) and that hold real work. A directory is only ever
//! deleted by the process holding its lock — never when locking is unsupported or fails for any
//! reason but "already held", which would otherwise throw away a running instance's work.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;

use crate::error::{Error, Result};

const LOCK_FILE: &str = "lock";
const PROJECT_FILE: &str = "untitled.kerf";

/// A recovery directory this process owns. Its lock is held for as long as this value lives.
#[derive(Debug)]
pub struct RecoveryDir {
    id: String,
    dir: PathBuf,
    lock: SessionLock,
}

/// The held lock file, unlocked explicitly when dropped. Closing it is not enough: the lock belongs
/// to the open file, which a child forked by another thread shares until it execs (`pre_exec`
/// spawns fork), so a session let go at that moment would stay locked and be skipped as in use.
#[derive(Debug)]
struct SessionLock(File);

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl RecoveryDir {
    /// A new, locked session directory under `root`.
    pub fn create(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = root.join(&id);
        std::fs::create_dir(&dir)?;
        match lock(&dir) {
            Ok(lock) => Ok(Self { id, dir, lock }),
            Err(e) => {
                // Only this call has seen the directory, and it holds nothing.
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The project file inside.
    pub fn project_path(&self) -> PathBuf {
        self.dir.join(PROJECT_FILE)
    }

    /// Delete the directory: its work was saved elsewhere, or let go. The project file must be
    /// closed first (Windows refuses to delete an open file).
    pub fn remove(self) -> Result<()> {
        let Self { dir, lock, .. } = self;
        drop(lock);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}

fn lock(dir: &Path) -> Result<SessionLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(SessionLock(file)),
        Err(TryLockError::WouldBlock) => Err(Error::Other(format!("{} is in use", dir.display()))),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// What a recoverable session holds, for the prompt that offers it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RecoveryInfo {
    pub id: String,
    /// When its project file was last written.
    pub modified_at: Option<DateTime<Utc>>,
    pub assets: usize,
    pub clips: usize,
    /// Edits in its history (the initial state not counted).
    pub edits: usize,
    /// The label of the newest edit.
    pub last_edit: Option<String>,
}

impl RecoveryInfo {
    /// Whether there is anything to recover: media, an edit, a task or a proposal. A session that
    /// was opened and never touched has none.
    fn has_work(&self, tasks: usize, staged: bool) -> bool {
        self.assets > 0 || self.clips > 0 || self.edits > 0 || tasks > 0 || staged
    }
}

/// A session left behind by a Kerf that is no longer running, locked by this process until it is
/// recovered, discarded or dropped.
#[derive(Debug)]
pub struct Recoverable {
    pub info: RecoveryInfo,
    pub(crate) dir: RecoveryDir,
}

impl Recoverable {
    /// Throw it away.
    pub fn discard(self) -> Result<()> {
        self.dir.remove()
    }
}

/// What [`scan`] made of one directory.
enum Seen {
    Recoverable(Recoverable),
    /// Locked by us and holding no work: deleted.
    Empty,
    /// Not ours to judge: owned by a running instance, unlockable, or unreadable.
    Skipped,
}

/// The sessions under `root` that can be recovered, newest first. Directories a running Kerf
/// holds, or whose lock cannot be taken for any other reason, are left alone; one locked here that
/// holds no work is deleted. `skip` is the session this process is using.
pub fn scan(root: &Path, skip: Option<&str>) -> Vec<Recoverable> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let id = entry.file_name().to_string_lossy().into_owned();
        if !dir.is_dir() || skip == Some(id.as_str()) {
            continue;
        }
        match inspect(id, dir) {
            Seen::Recoverable(r) => found.push(r),
            Seen::Empty | Seen::Skipped => {}
        }
    }
    found.sort_by_key(|r| std::cmp::Reverse(r.info.modified_at));
    found
}

fn inspect(id: String, dir: PathBuf) -> Seen {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))
    {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot open a recovery lock; leaving it alone");
            return Seen::Skipped;
        }
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Seen::Skipped,
        Err(TryLockError::Error(e)) => {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot lock a recovery directory; leaving it alone");
            return Seen::Skipped;
        }
    }
    let owned = RecoveryDir {
        id,
        dir,
        lock: SessionLock(file),
    };
    let project = owned.project_path();
    let summary = if project.exists() {
        summarize(&owned.id, &project)
    } else {
        Ok(None)
    };
    match summary {
        Ok(Some(info)) => Seen::Recoverable(Recoverable { info, dir: owned }),
        Ok(None) => {
            let dir = owned.dir.clone();
            if let Err(e) = owned.remove() {
                tracing::warn!(dir = %dir.display(), error = %e, "could not delete an empty recovery directory");
            }
            Seen::Empty
        }
        Err(e) => {
            tracing::warn!(dir = %owned.dir.display(), error = %e, "cannot read a recovery project; leaving it alone");
            Seen::Skipped
        }
    }
}

/// What the project at `path` holds, or `None` when it holds no work. Opened read-write (but never
/// created) so SQLite can roll back the journal a crash left behind before it is read.
fn summarize(id: &str, path: &Path) -> Result<Option<RecoveryInfo>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    summarize_conn(&conn, id, path)
}

/// [`summarize`] over a connection already open on `path`. A table the file lacks holds nothing:
/// a session that crashed while its file was being created (a 0-byte file, a schema half
/// written) is empty, not unreadable, so it is cleaned up rather than skipped forever.
pub(crate) fn summarize_conn(conn: &Connection, id: &str, path: &Path) -> Result<Option<RecoveryInfo>> {
    let has = |table: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )?)
    };
    let count = |table: &str, filter: &str| -> Result<usize> {
        if !has(table)? {
            return Ok(0);
        }
        let n: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table} {filter}"), [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    };
    let assets = count("assets", "")?;
    let edits = count("history", "WHERE seq > 0")?;
    let tasks = count("tasks", "")?;
    let staged = count("staged", "")? > 0;
    let clips = if has("timeline")? {
        conn.query_row("SELECT data FROM timeline WHERE id = 1", [], |r| r.get::<_, String>(0))
            .optional()?
            .and_then(|json| serde_json::from_str::<crate::model::Timeline>(&json).ok())
            .map(|t| t.tracks.iter().map(|tr| tr.clips.len()).sum())
            .unwrap_or(0)
    } else {
        0
    };
    let last_edit = if edits > 0 {
        conn.query_row("SELECT label FROM history WHERE seq > 0 ORDER BY seq DESC LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    } else {
        None
    };
    let modified_at = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(DateTime::<Utc>::from);
    let info = RecoveryInfo {
        id: id.to_string(),
        modified_at,
        assets,
        clips,
        edits,
        last_edit,
    };
    Ok(info.has_work(tasks, staged).then_some(info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Asset;
    use crate::project::Project;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-recovery-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dirs(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn some_asset() -> Asset {
        Asset {
            id: uuid::Uuid::new_v4(),
            path: "/media/a.wav".into(),
            name: "a.wav".into(),
            duration: 10.0,
            streams: Vec::new(),
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
            music_score: None,
            tags: Vec::new(),
        }
    }

    /// A session that crashed: its directory and file stay, its lock is gone with the process.
    fn crashed_session(root: &Path, work: bool) -> String {
        let project = Project::create_recovery(root).unwrap();
        if work {
            project.insert_asset(&some_asset()).unwrap();
        }
        let id = project.recovery_id().unwrap().to_string();
        drop(project);
        id
    }

    #[test]
    fn a_session_with_work_is_offered_and_an_untouched_one_is_cleaned_up() {
        let root = scratch();
        let worked = crashed_session(&root, true);
        let untouched = crashed_session(&root, false);
        let found = scan(&root, None);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].info.id, worked);
        assert_eq!(found[0].info.assets, 1);
        assert!(!root.join(&untouched).exists(), "an empty session is deleted");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_session_still_open_is_neither_offered_nor_touched() {
        let root = scratch();
        let live = Project::create_recovery(&root).unwrap();
        live.insert_asset(&some_asset()).unwrap();
        assert!(scan(&root, None).is_empty());
        assert!(live.recovery_dir().unwrap().exists());
        // Nor is an empty one that a running instance holds.
        let idle = Project::create_recovery(&root).unwrap();
        assert!(scan(&root, None).is_empty());
        assert!(idle.recovery_dir().unwrap().exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_session_in_use_is_skipped_by_name() {
        let root = scratch();
        let id = crashed_session(&root, true);
        assert!(scan(&root, Some(&id)).is_empty());
        assert_eq!(scan(&root, None).len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_found_session_stays_locked_until_it_is_let_go() {
        let root = scratch();
        crashed_session(&root, true);
        let found = scan(&root, None);
        assert_eq!(found.len(), 1);
        assert!(scan(&root, None).is_empty(), "held by the first scan");
        drop(found);
        assert_eq!(scan(&root, None).len(), 1, "offered again once released");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_session_let_go_is_unlocked_while_a_forked_child_shares_its_lock() {
        let root = scratch();
        let dir = RecoveryDir::create(&root).unwrap();
        let path = dir.dir().to_path_buf();
        // What a child forked by another thread holds until it execs: the same open file.
        let inherited = dir.lock.0.try_clone().unwrap();
        drop(dir);
        assert!(lock(&path).is_ok(), "released though a copy of the lock file is still open");
        drop(inherited);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discarding_deletes_the_directory() {
        let root = scratch();
        let id = crashed_session(&root, true);
        let found = scan(&root, None).pop().unwrap();
        found.discard().unwrap();
        assert!(!root.join(id).exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_directory_without_a_project_is_empty_and_an_unreadable_one_is_kept() {
        let root = scratch();
        std::fs::create_dir_all(root.join("bare")).unwrap();
        let garbled = root.join("garbled");
        std::fs::create_dir_all(&garbled).unwrap();
        std::fs::write(
            garbled.join(PROJECT_FILE),
            b"not a database, and long enough to be read as one",
        )
        .unwrap();
        assert!(scan(&root, None).is_empty());
        assert_eq!(dirs(&root), vec!["garbled".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_zero_byte_or_half_made_project_is_empty() {
        let root = scratch();
        let zero = root.join("zero");
        std::fs::create_dir_all(&zero).unwrap();
        std::fs::write(zero.join(PROJECT_FILE), b"").unwrap();
        let half = root.join("half");
        std::fs::create_dir_all(&half).unwrap();
        Connection::open(half.join(PROJECT_FILE))
            .unwrap()
            .execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)")
            .unwrap();
        assert!(scan(&root, None).is_empty());
        assert!(dirs(&root).is_empty(), "both are cleaned up: {:?}", dirs(&root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn exiting_keeps_a_session_with_work_and_deletes_an_untouched_one() {
        let root = scratch();
        let worked = Project::create_recovery(&root).unwrap();
        worked.insert_asset(&some_asset()).unwrap();
        let worked_dir = worked.recovery_dir().unwrap().to_path_buf();
        worked.close_at_exit();
        assert!(worked_dir.exists(), "offered at the next launch");
        assert_eq!(scan(&root, None).len(), 1);

        let idle = Project::create_recovery(&root).unwrap();
        let idle_dir = idle.recovery_dir().unwrap().to_path_buf();
        idle.close_at_exit();
        assert!(!idle_dir.exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn recovering_carries_the_work_and_saving_it_removes_the_session() {
        let root = scratch();
        crashed_session(&root, true);
        let found = scan(&root, None).pop().unwrap();
        let dir = found.dir.dir().to_path_buf();
        let recovered = Project::open_recovered(found).unwrap();
        assert!(recovered.path().is_none(), "still untitled");
        assert_eq!(recovered.list_assets().unwrap().len(), 1);

        let target = root.join("saved.kerf");
        recovered.save_as(&target).unwrap();
        let mut current = recovered;
        current.replace_with(Project::open(&target).unwrap()).close();
        assert!(!dir.exists(), "the session goes once its work is in a file");
        assert_eq!(current.list_assets().unwrap().len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }
}
