//! The `.kerf` file itself: which schema version it is at, the migrations that bring an
//! older one forward, and copies that a crash or a full disk cannot leave half-written.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::error::{Error, Result};

/// One schema step: upgrades a file from the version at its index to the next.
type Migration = fn(&Connection) -> rusqlite::Result<()>;

/// `MIGRATIONS[i]` upgrades schema `i` to `i + 1`. Version 0 is every file written before
/// `PRAGMA user_version` was used — the legacy baseline. Append one step per change; never edit
/// a shipped one. Steps run after `SCHEMA` has created any table the file lacked, so each must
/// tolerate that (`IF NOT EXISTS`, probe before `ALTER`).
const MIGRATIONS: &[Migration] = &[v0_to_v1];

/// The schema this build writes and the newest it opens.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// v1: the columns older builds added by probing, now a numbered step.
fn v0_to_v1(conn: &Connection) -> rusqlite::Result<()> {
    for column in ["source_paths", "voiceover"] {
        if conn.prepare(&format!("SELECT {column} FROM assets LIMIT 1")).is_err() {
            conn.execute(&format!("ALTER TABLE assets ADD COLUMN {column} TEXT"), [])?;
        }
    }
    Ok(())
}

pub(crate) fn user_version(conn: &Connection) -> Result<u32> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(u32::try_from(v).unwrap_or(u32::MAX))
}

/// Whether the database has no tables yet: one this build is creating, not one to migrate.
pub(crate) fn is_blank(conn: &Connection) -> Result<bool> {
    let tables: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))?;
    Ok(tables == 0)
}

/// Refuse a file a newer Kerf wrote: its schema may hold what this build would drop.
pub(crate) fn check_supported(found: u32) -> Result<()> {
    if found > SCHEMA_VERSION {
        return Err(Error::NewerProject {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Run the steps from `found` up to [`SCHEMA_VERSION`], each in its own transaction that also
/// stamps the version it reached, so a failure leaves the file at the last step that finished.
pub(crate) fn migrate(conn: &Connection, found: u32) -> Result<()> {
    for (v, step) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let tx = conn.unchecked_transaction()?;
        step(conn)?;
        tx.pragma_update(None, "user_version", v as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

pub(crate) fn stamp_current(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "user_version", SCHEMA_VERSION as i64)?;
    Ok(())
}

/// Where the copy of `path` kept before its first upgrade from `version` goes:
/// `<stem> (schema v0 backup).kerf`, beside it.
pub fn backup_path(path: &Path, version: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kerf".into());
    path.with_file_name(format!("{stem} (schema v{version} backup).{ext}"))
}

/// Keep a copy of the file `conn` has open before it is upgraded from `version`. An existing
/// backup is the older one and is left alone.
pub(crate) fn backup_before_upgrade(conn: &Connection, path: &Path, version: u32) -> Result<PathBuf> {
    let backup = backup_path(path, version);
    if !backup.exists() {
        vacuum_into_atomic(conn, &backup)?;
    }
    Ok(backup)
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Write a consistent snapshot of `conn`'s database to `target`, replacing it only once the
/// snapshot is whole: `VACUUM INTO` a temp file in the same directory (so the rename never
/// crosses file systems), fsync it, rename it over `target`, then fsync the directory on unix so
/// the rename survives a power cut. On any failure the temp file is removed and `target` is as it
/// was.
pub(crate) fn vacuum_into_atomic(conn: &Connection, target: &Path) -> Result<()> {
    let dir = parent_dir(target);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let dst = tmp
            .to_str()
            .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 project path: {}", target.display())))?;
        conn.execute("VACUUM INTO ?1", params![dst])?;
        // Write access: Windows will not flush a handle opened read-only.
        OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    sync_dir(&dir);
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Err(e) = File::open(dir).and_then(|d| d.sync_all()) {
        tracing::warn!(dir = %dir.display(), error = %e, "could not fsync the directory after a save");
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-store-{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn db_with(value: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (v TEXT)").unwrap();
        conn.execute("INSERT INTO t VALUES (?1)", params![value]).unwrap();
        conn
    }

    fn read(path: &Path) -> String {
        Connection::open(path)
            .unwrap()
            .query_row("SELECT v FROM t", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_snapshot_replaces_the_target_and_leaves_no_temp_file() {
        let dir = scratch("replace");
        let target = dir.join("cut.kerf");
        vacuum_into_atomic(&db_with("one"), &target).unwrap();
        vacuum_into_atomic(&db_with("two"), &target).unwrap();
        assert_eq!(read(&target), "two");
        assert_eq!(names(&dir), vec!["cut.kerf".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_snapshot_keeps_the_old_file_and_cleans_up() {
        let dir = scratch("fail");
        let target = dir.join("cut.kerf");
        vacuum_into_atomic(&db_with("good"), &target).unwrap();
        // Renaming a file over a non-empty directory fails after the temp file is written.
        let blocked = dir.join("blocked.kerf");
        std::fs::create_dir_all(blocked.join("x")).unwrap();
        assert!(vacuum_into_atomic(&db_with("bad"), &blocked).is_err());
        assert_eq!(read(&target), "good");
        assert_eq!(names(&dir), vec!["blocked.kerf".to_string(), "cut.kerf".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_snapshot_into_a_missing_directory_is_an_error_and_writes_nothing() {
        let dir = scratch("missing");
        assert!(vacuum_into_atomic(&db_with("x"), &dir.join("nope/cut.kerf")).is_err());
        assert!(names(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_backup_sits_beside_the_file_and_names_the_version() {
        assert_eq!(
            backup_path(Path::new("/films/My cut.kerf"), 0),
            PathBuf::from("/films/My cut (schema v0 backup).kerf")
        );
        assert_eq!(
            backup_path(Path::new("/films/cut"), 3),
            PathBuf::from("/films/cut (schema v3 backup).kerf")
        );
    }

    #[test]
    fn a_newer_schema_is_refused_and_the_current_one_is_not() {
        assert!(check_supported(SCHEMA_VERSION).is_ok());
        let err = check_supported(SCHEMA_VERSION + 1).unwrap_err();
        assert!(err.to_string().contains("newer Kerf"), "{err}");
    }
}
