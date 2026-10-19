//! The `.kerf` file itself: which schema version it is at, the migrations that bring an
//! older one forward, and copies that a crash or a full disk cannot leave half-written.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{params, Connection};

use crate::error::{Error, Result};

/// One schema step: upgrades a file from the version at its index to the next.
struct Migration {
    run: fn(&Connection) -> rusqlite::Result<()>,
    /// Whether `run` would change this file; a file that already has what the step adds gets
    /// no backup for it.
    needed: fn(&Connection) -> bool,
    /// Whether the step drops or rewrites data. Only such a step refuses to run without a backup.
    destructive: bool,
    /// Whether a build that cannot write must still refuse a file that lacks what the step adds.
    /// False when every read tolerates the absence (an extra table), so a write-protected file that
    /// predates the step opens as it is rather than being turned away.
    needed_to_read: bool,
}

/// `MIGRATIONS[i]` upgrades schema `i` to `i + 1`. Version 0 is every file written before
/// `PRAGMA user_version` was used — the legacy baseline. Append one step per change; never edit
/// a shipped one. Steps run after `SCHEMA` has created any table the file lacked, so each must
/// tolerate that (`IF NOT EXISTS`, probe before `ALTER`).
///
/// The version covers **the JSON the tables hold** too (timeline, history, proposal, analysis, asset
/// streams): every shipped build since 0.26 refuses a newer version, while one that is let in drops a
/// field it does not know at its next write and fails on a variant it does not know. So a change an
/// older build would drop or misread (a new field, a new variant, a new meaning) is a step of its own:
/// `run` does nothing (or rewrites the JSON), `needed` is always true (the file is copied aside before
/// the older build is locked out of it) and `needed_to_read` false. `persist_tests` keeps a timeline
/// per version and fails when the stored shape moves without one.
const MIGRATIONS: &[Migration] = &[
    Migration {
        run: v0_to_v1,
        needed: v0_to_v1_needed,
        destructive: false,
        needed_to_read: true,
    },
    Migration {
        run: v1_to_v2,
        needed: v1_to_v2_needed,
        destructive: false,
        needed_to_read: true,
    },
    Migration {
        run: v2_to_v3,
        needed: v2_to_v3_needed,
        destructive: false,
        needed_to_read: false,
    },
    Migration {
        run: v3_to_v4,
        needed: v3_to_v4_needed,
        destructive: false,
        needed_to_read: true,
    },
    Migration {
        run: v4_to_v5,
        needed: v4_to_v5_needed,
        destructive: false,
        needed_to_read: false,
    },
];

/// The schema this build writes and the newest it opens.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

const V1_COLUMNS: [&str; 2] = ["source_paths", "voiceover"];

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("SELECT {column} FROM {table} LIMIT 1")).is_ok()
}

/// v1: the columns older builds added by probing, now a numbered step.
fn v0_to_v1(conn: &Connection) -> rusqlite::Result<()> {
    for column in V1_COLUMNS {
        if !has_column(conn, "assets", column) {
            conn.execute(&format!("ALTER TABLE assets ADD COLUMN {column} TEXT"), [])?;
        }
    }
    Ok(())
}

fn v0_to_v1_needed(conn: &Connection) -> bool {
    V1_COLUMNS.iter().any(|c| !has_column(conn, "assets", c))
}

const V2_COLUMNS: [&str; 2] = ["owner", "assets"];

/// v2: a proposal remembers which agent session owns it (`owner`) and the asset changes held
/// back with it (`assets`, JSON). A proposal staged by an older build has neither.
fn v1_to_v2(conn: &Connection) -> rusqlite::Result<()> {
    for column in V2_COLUMNS {
        if !has_column(conn, "staged", column) {
            conn.execute(&format!("ALTER TABLE staged ADD COLUMN {column} TEXT"), [])?;
        }
    }
    Ok(())
}

fn v1_to_v2_needed(conn: &Connection) -> bool {
    V2_COLUMNS.iter().any(|c| !has_column(conn, "staged", c))
}

/// v3: the table of fonts a project embeds (a template's typeface travels with it). Also in `SCHEMA`
/// for a new file; `CREATE TABLE IF NOT EXISTS`, so it does not matter which ran first.
fn v2_to_v3(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS fonts (
            sha256 TEXT PRIMARY KEY,
            family TEXT NOT NULL,
            data   BLOB NOT NULL
        )",
    )
}

fn v2_to_v3_needed(conn: &Connection) -> bool {
    !has_table(conn, "fonts")
}

pub(crate) fn has_table(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

/// v4: an asset remembers the score it was composed from (`music_score`, JSON). Audio a build
/// without composing wrote has none.
fn v3_to_v4(conn: &Connection) -> rusqlite::Result<()> {
    if !has_column(conn, "assets", "music_score") {
        conn.execute("ALTER TABLE assets ADD COLUMN music_score TEXT", [])?;
    }
    Ok(())
}

fn v3_to_v4_needed(conn: &Connection) -> bool {
    !has_column(conn, "assets", "music_score")
}

/// v5: a delivery frame may fit footage over a blurred copy of itself (`Fit::Blur`, `"fit":"blur"`
/// in the timeline JSON), a variant an older build fails to read. Nothing in the file changes; the
/// step is the version that keeps an older build out, after the file is copied aside.
fn v4_to_v5(_: &Connection) -> rusqlite::Result<()> {
    Ok(())
}

fn v4_to_v5_needed(_: &Connection) -> bool {
    true
}

/// The steps from `found` on that would change this file.
fn pending(conn: &Connection, found: u32) -> impl Iterator<Item = &'static Migration> + '_ {
    MIGRATIONS.iter().skip(found as usize).filter(move |m| (m.needed)(conn))
}

/// Whether a file at `found` lacks something this build cannot read it without: a step that would
/// change the file *and* that reads depend on. (An added table that reads tolerate missing does not
/// count: such a file opens read-only as it is.)
pub(crate) fn needs_upgrade(conn: &Connection, found: u32) -> bool {
    pending(conn, found).any(|m| m.needed_to_read)
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
        (step.run)(conn)?;
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

/// Keep a copy of the file `conn` has open before it is upgraded from `version`, when an upgrade
/// would change it. An existing backup is the older one and is left alone. A backup that cannot
/// be written (a full disk, a folder the user may not write to) stops only a destructive upgrade;
/// the others only add, so the file still opens.
pub(crate) fn backup_before_upgrade(conn: &Connection, path: &Path, version: u32) -> Result<Option<PathBuf>> {
    let mut steps = pending(conn, version).peekable();
    if steps.peek().is_none() {
        return Ok(None);
    }
    let destructive = steps.any(|m| m.destructive);
    let backup = backup_path(path, version);
    if backup.is_file() {
        return Ok(Some(backup));
    }
    match vacuum_into_atomic(conn, &backup) {
        Ok(()) => Ok(Some(backup)),
        Err(e) if !destructive => {
            tracing::warn!(backup = %backup.display(), error = %e, "could not back up a project before upgrading it; upgrading anyway");
            Ok(None)
        }
        Err(e) => Err(e),
    }
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
    sweep_stale_temps(&dir, &name);
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

/// How old a temp file of ours must be before a save takes it for one a crash left: well past
/// any snapshot still being written by another instance.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);

/// Whether `file` is a temp file [`vacuum_into_atomic`] makes for `name`: `.<name>.<uuid>.tmp`.
fn is_temp_for(file: &str, name: &str) -> bool {
    file.strip_prefix('.')
        .and_then(|f| f.strip_prefix(name))
        .and_then(|f| f.strip_prefix('.'))
        .and_then(|f| f.strip_suffix(".tmp"))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Delete the temp files a crashed save of `name` left in `dir`. Only this target's own pattern
/// is looked at, and only once it is [`STALE_TEMP`] old.
fn sweep_stale_temps(dir: &Path, name: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if !is_temp_for(&entry.file_name().to_string_lossy(), name) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age >= STALE_TEMP));
        if stale {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!(file = %entry.path().display(), error = %e, "could not delete a stale save temp file");
            }
        }
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
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
    fn a_save_sweeps_its_own_stale_temp_files_and_nothing_else() {
        let dir = scratch("sweep");
        let target = dir.join("cut.kerf");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let old = SystemTime::now() - STALE_TEMP - Duration::from_secs(60);
        let make = |name: &str, aged: bool| {
            let file = std::fs::File::create(dir.join(name)).unwrap();
            if aged {
                file.set_modified(old).unwrap();
            }
        };
        make(&format!(".cut.kerf.{id}.tmp"), true);
        make(&format!(".other.kerf.{id}.tmp"), true);
        make(&format!(".cut.kerf.{}.tmp", uuid::Uuid::new_v4().simple()), false);
        make(".cut.kerf.notauuid.tmp", true);
        vacuum_into_atomic(&db_with("x"), &target).unwrap();
        let left = names(&dir);
        assert!(!left.contains(&format!(".cut.kerf.{id}.tmp")), "{left:?}");
        assert_eq!(
            left.len(),
            4,
            "the other target's, the fresh one, the odd name and the file: {left:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_temp_pattern_is_exact() {
        let id = "0123456789abcdef0123456789abcdef";
        assert!(is_temp_for(&format!(".cut.kerf.{id}.tmp"), "cut.kerf"));
        assert!(!is_temp_for(&format!(".cut.kerf.{id}.tmp"), "cut"));
        assert!(!is_temp_for(&format!("cut.kerf.{id}.tmp"), "cut.kerf"));
        assert!(!is_temp_for(".cut.kerf.0123.tmp", "cut.kerf"));
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
