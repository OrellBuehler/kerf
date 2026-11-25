//! A ceiling on the disk the regenerable caches may take, for a server that must not fill its disk.
//!
//! Nothing here runs unless an adapter asks (`kerf-app --headless --cache-max-gb`): the desktop app
//! never prunes these caches, and no setting or stored shape is involved.
//!
//! **What is pruned** is what Kerf can make again from the originals: `proxies/` (a proxy and its
//! sidecar are one unit), `waveforms/`, `filmstrips/`, `stems/` and `music/` under `<cache>/kerf`. Left
//! alone: the downloads (`models/`, `soundfonts/`, `runtime/`), `stitched/` (a stitched 360 file *is* an
//! asset's source, not a cache of one), and the caches that bound themselves (`luts/`, `sfx-preview/`,
//! `stabilize/`, `fonts/`, `svg/`, `segments/`).
//!
//! **What a run keeps**, however far over the cap that leaves the total: whatever the open project's
//! assets use ([`InUse`]), a file being written (`.part`), and anything touched within [`RECENT`] (a
//! write the name does not give away, a stems folder between two of its files). Past that, the least
//! recently used unit goes first until the total fits ([`prune_plan`], pure). Last use is the later of
//! the file's modified and accessed times: on a volume mounted `noatime` it is the write time.
//!
//! A removed cache is made again when something next asks for it. A removed proxy reads as `missing`
//! (`proxy::status`, `ready_proxy` is an existence check) and previews fall back to the original; the
//! plan epoch moves so a plan made around it is rebuilt. The open project's proxies are never removed.
//!
//! **Stems are the project's media**, not a cache of it: once placed, `stems/<hash>/<name>.flac` is an
//! asset's own file. [`InUse::of`] therefore keeps every asset's own file, and the unit that holds it
//! when it lies inside a managed directory, besides the caches the asset's source made.
//!
//! **Never blind.** A pass that cannot tell what the project uses is skipped, not guessed at: the
//! assets could not be read, or an original cannot be found (the caches are keyed on it, so what it
//! uses cannot be named). Nothing is removed until the next pass can tell.
//!
//! **Locks.** The pruner never holds the project lock across disk work: the adapter's closure hands over
//! the assets under it and returns, and the stats and deletes happen after.
//!
//! **One process.** What is kept is what *this* process's open project uses. Another Kerf (the desktop
//! app, a second server) sharing the cache directory has projects this one cannot see.

use std::collections::{BTreeMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use crate::model::Asset;

/// One gibibyte, the unit of `--cache-max-gb` (what `du -h` calls `1G`).
pub const GIB: u64 = 1 << 30;
/// How often the pruner looks even when nothing asked it to.
pub const INTERVAL: Duration = Duration::from_secs(10 * 60);
/// After a [`nudge`] the pruner waits this long for a burst of work to settle before it looks.
const SETTLE: Duration = Duration::from_secs(20);
/// A unit touched this recently is left alone: it may be in the middle of being written.
pub const RECENT: Duration = Duration::from_secs(10 * 60);
/// A `.part` this old is a crashed write's, not a running one's (a write keeps touching it).
pub const STALE_PARTIAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How a cache directory is laid out, which decides what one removable unit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Every file is a unit.
    Files,
    /// Files that differ only in their extension (`<hash>.mp4`, `<hash>.json`) are one unit.
    Pairs,
    /// Every directory is a unit.
    Dirs,
}

/// The directories under `<cache>/kerf` the cap manages.
const GROUPS: [(&str, Shape); 5] = [
    ("proxies", Shape::Pairs),
    ("waveforms", Shape::Files),
    ("music", Shape::Files),
    ("filmstrips", Shape::Dirs),
    ("stems", Shape::Dirs),
];

/// The group whose removal moves the plan epoch (a plan is made around which proxies are there).
const PROXIES: &str = "proxies";

/// What a run may do with a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Regenerable and idle: removed, least recently used first, while the total is over the cap.
    Evictable,
    /// A crashed write's `.part`: removed before anything evictable.
    Stale,
    /// Used by an asset of the open project: kept.
    InUse,
    /// Being written, or touched too recently to tell: kept.
    InFlight,
}

/// One removable unit of a cache: a file, a proxy with its sidecar, or a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// The cache directory it is in (`proxies`, `stems`, …).
    pub group: &'static str,
    /// What goes with it, with each one's size, in the order they are removed.
    pub paths: Vec<(PathBuf, u64)>,
    /// All of it, in bytes.
    pub bytes: u64,
    /// When it was last used: the latest modified or accessed time of its files.
    pub used: SystemTime,
    pub class: Class,
}

fn first_path(entry: &CacheEntry) -> Option<&Path> {
    entry.paths.first().map(|(p, _)| p.as_path())
}

/// What [`prune_plan`] decided.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Indices into the entries, in the order they are removed.
    pub remove: Vec<usize>,
    /// The entries' bytes before.
    pub total: u64,
    /// ... and once `remove` is gone.
    pub after: u64,
    /// The bytes that cannot go: in use or in flight.
    pub pinned: u64,
}

/// Which units to remove so `entries` fit in `cap` bytes (pure). Only [`Class::Stale`] and
/// [`Class::Evictable`] units go, stale ones first and then the least recently used (ties by path, so
/// the answer is the same every time), and only until the total fits; a unit of no bytes is never
/// worth removing. When what is kept alone is over the cap, `after` says so.
pub fn prune_plan(entries: &[CacheEntry], cap: u64) -> Plan {
    let total = entries.iter().fold(0u64, |sum, e| sum.saturating_add(e.bytes));
    let pinned = entries
        .iter()
        .filter(|e| matches!(e.class, Class::InUse | Class::InFlight))
        .fold(0u64, |sum, e| sum.saturating_add(e.bytes));
    let rank = |e: &CacheEntry| u8::from(e.class != Class::Stale);
    let mut order: Vec<usize> = (0..entries.len())
        .filter(|&i| matches!(entries[i].class, Class::Stale | Class::Evictable) && entries[i].bytes > 0)
        .collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&entries[a], &entries[b]);
        rank(a)
            .cmp(&rank(b))
            .then(a.used.cmp(&b.used))
            .then_with(|| first_path(a).cmp(&first_path(b)))
    });
    let mut after = total;
    let mut remove = Vec::new();
    for i in order {
        if after <= cap {
            break;
        }
        after -= entries[i].bytes;
        remove.push(i);
    }
    Plan {
        remove,
        total,
        after,
        pinned,
    }
}

/// What a unit is, from what is known of it (pure): `partial` for a name that says it is being
/// written, `in_use` for one the open project uses, `used` for when it was last touched. A time in the
/// future (a clock that moved) reads as just now.
pub fn classify(partial: bool, in_use: bool, used: SystemTime, now: SystemTime) -> Class {
    let age = now.duration_since(used).unwrap_or_default();
    if in_use {
        Class::InUse
    } else if partial {
        if age > STALE_PARTIAL {
            Class::Stale
        } else {
            Class::InFlight
        }
    } else if age < RECENT {
        Class::InFlight
    } else {
        Class::Evictable
    }
}

/// Whether a name is a write in progress (or a crashed one): `<hash>.<pid>.part`,
/// `<hash>.json.<pid>.<n>.part`, a stem's `<name>.<pid>.part.flac`.
fn is_partial(name: &str) -> bool {
    name.ends_with(".part") || name.contains(".part.")
}

// ---- what the open project uses ----------------------------------------------

/// The cache files the open project's assets use, which a run never removes.
#[derive(Debug, Clone, Default)]
pub struct InUse {
    keep: HashSet<PathBuf>,
    /// Originals that could not be found: what their caches are called cannot be worked out.
    unreadable: Vec<PathBuf>,
}

impl InUse {
    /// What `assets` use among the managed caches under `root` (`<cache>/kerf`): the files their
    /// sources made, and the assets' own files that lie inside it (separated stems are imported from
    /// there). Stats each original (the caches are keyed on it) but spawns nothing, so it runs with the
    /// project lock released, and a share that went away can make it slow.
    pub fn of(assets: &[Asset], root: &Path) -> Self {
        let canonical_root = std::fs::canonicalize(root).ok();
        let mut out = Self::default();
        for asset in assets {
            let path = Path::new(&asset.path);
            if std::fs::metadata(path).is_err() {
                out.unreadable.push(path.to_path_buf());
            }
            out.keep.extend(crate::engine::asset_cache_files(asset));
            out.keep_own_file(path, root, canonical_root.as_deref());
        }
        out
    }

    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            keep: paths.into_iter().collect(),
            unreadable: Vec::new(),
        }
    }

    /// The originals that could not be found; while there are any, [`InUse`] is not the whole answer.
    pub fn unreadable(&self) -> &[PathBuf] {
        &self.unreadable
    }

    /// An asset's own file, and every directory above it down to `root`, so the unit it is in (a
    /// stems folder with its other three) stays whole. Spelled as `path` has it and, when a link makes
    /// it a different one, as the pruner will see it.
    fn keep_own_file(&mut self, path: &Path, root: &Path, canonical_root: Option<&Path>) {
        let linked = canonical_root
            .zip(std::fs::canonicalize(path).ok())
            .and_then(|(canonical_root, real)| real.strip_prefix(canonical_root).ok().map(|rel| root.join(rel)));
        for own in std::iter::once(path.to_path_buf()).chain(linked) {
            if own.starts_with(root) {
                self.keep
                    .extend(own.ancestors().take_while(|a| *a != root).map(Path::to_path_buf));
            }
        }
    }

    fn holds(&self, path: &Path) -> bool {
        self.keep.contains(path)
    }
}

// ---- reading the disk --------------------------------------------------------

/// What one file or folder holds: bytes, the latest time any file was used, whether any is partial.
#[derive(Default)]
struct Stats {
    bytes: u64,
    used: Option<SystemTime>,
    partial: bool,
}

impl Stats {
    fn add(&mut self, bytes: u64, used: SystemTime, partial: bool) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.used = Some(self.used.map_or(used, |u| u.max(used)));
        self.partial |= partial;
    }
}

fn used_of(meta: &std::fs::Metadata) -> SystemTime {
    let modified = meta.modified().ok();
    let accessed = meta.accessed().ok();
    modified.into_iter().chain(accessed).max().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The files under `dir`, a few levels down (a cache folder is flat), links not followed.
fn walk(dir: &Path, depth: u8, stats: &mut Stats) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_file() {
            if let Ok(meta) = entry.metadata() {
                let partial = is_partial(&entry.file_name().to_string_lossy());
                stats.add(meta.len(), used_of(&meta), partial);
            }
        } else if kind.is_dir() && depth > 0 {
            walk(&entry.path(), depth - 1, stats);
        }
    }
}

/// The name a proxy's files share: `<hash>.mp4` and `<hash>.json` are `<hash>`, and the head-padded
/// `<hash>.lead.mp4` / `<hash>.lead.json` are `<hash>.lead`.
fn pair_key(name: &str) -> &str {
    name.strip_suffix(".mp4")
        .or_else(|| name.strip_suffix(".json"))
        .unwrap_or(name)
}

struct Unit {
    paths: Vec<(PathBuf, u64)>,
    stats: Stats,
}

/// Every unit of the managed caches under `root` (`<cache>/kerf`), as it is on disk at `now`.
pub fn scan(root: &Path, in_use: &InUse, now: SystemTime) -> Vec<CacheEntry> {
    let mut out = Vec::new();
    for (group, shape) in GROUPS {
        let Ok(entries) = std::fs::read_dir(root.join(group)) else {
            continue;
        };
        let mut units: BTreeMap<String, Unit> = BTreeMap::new();
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let mut stats = Stats::default();
            let key = match shape {
                Shape::Dirs if kind.is_dir() => {
                    walk(&path, 2, &mut stats);
                    stats.partial |= is_partial(&name);
                    name
                }
                Shape::Files | Shape::Pairs if kind.is_file() => {
                    let Ok(meta) = entry.metadata() else { continue };
                    stats.add(meta.len(), used_of(&meta), is_partial(&name));
                    if shape == Shape::Pairs && !stats.partial {
                        pair_key(&name).to_string()
                    } else {
                        name
                    }
                }
                _ => continue,
            };
            let bytes = stats.bytes;
            let unit = units.entry(key).or_insert_with(|| Unit {
                paths: Vec::new(),
                stats: Stats::default(),
            });
            unit.paths.push((path, bytes));
            if let Some(used) = stats.used {
                unit.stats.add(stats.bytes, used, stats.partial);
            } else {
                unit.stats.partial |= stats.partial;
            }
        }
        for mut unit in units.into_values() {
            // A proxy goes before its sidecar: with the sidecar alone left, nothing reads it as a proxy.
            unit.paths.sort_by_key(|(p, _)| p.extension().is_some_and(|e| e == "json"));
            let used = unit.stats.used.unwrap_or(SystemTime::UNIX_EPOCH);
            let held = unit.paths.iter().any(|(p, _)| in_use.holds(p));
            out.push(CacheEntry {
                group,
                bytes: unit.stats.bytes,
                class: classify(unit.stats.partial, held, used, now),
                used,
                paths: unit.paths,
            });
        }
    }
    out
}

// ---- running it --------------------------------------------------------------

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The managed caches' bytes before and after.
    pub before: u64,
    pub after: u64,
    pub freed: u64,
    /// Units removed.
    pub removed: usize,
    /// Bytes a run cannot remove: in use or in flight.
    pub pinned: u64,
    /// What was freed in each cache directory.
    pub by_group: BTreeMap<&'static str, u64>,
}

/// Remove one unit; the bytes actually freed. A path that is already gone, or cannot be removed (a
/// file a Windows reader holds open), frees nothing and does not stop the others.
fn remove(entry: &CacheEntry) -> u64 {
    let mut freed = 0;
    for (path, bytes) in &entry.paths {
        let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir());
        let gone = if is_dir {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match gone {
            Ok(()) => freed += bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::debug!(path = %path.display(), error = %e, "cache pruning could not remove a file"),
        }
    }
    freed
}

/// One pruning run over the caches under `root` (`<cache>/kerf`) for a ceiling of `cap` bytes,
/// keeping what `in_use` names. Touches the disk only; call it with no lock held.
pub fn prune(root: &Path, cap: u64, in_use: &InUse, now: SystemTime) -> Report {
    let entries = scan(root, in_use, now);
    let plan = prune_plan(&entries, cap);
    let mut report = Report {
        before: plan.total,
        pinned: plan.pinned,
        ..Report::default()
    };
    for &i in &plan.remove {
        let entry = &entries[i];
        let freed = remove(entry);
        if freed > 0 {
            report.removed += 1;
            report.freed += freed;
            *report.by_group.entry(entry.group).or_default() += freed;
            if entry.group == PROXIES {
                crate::plan_epoch::bump_plan_epoch();
            }
        }
    }
    report.after = plan.total.saturating_sub(report.freed);
    report
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIB as f64)
}

/// One pass: read the project's assets through `assets`, work out what they use and prune the caches
/// under `root` (`<cache>/kerf`) to `cap`. `Err` with the reason when the pass cannot tell what is in
/// use, and then nothing at all is removed: pruning on a guess would take what the project plays from.
pub fn run_once(
    root: &Path,
    cap: u64,
    assets: &dyn Fn() -> crate::Result<Vec<Asset>>,
    now: SystemTime,
) -> Result<Report, String> {
    let assets = assets().map_err(|e| format!("the project's assets could not be read ({e})"))?;
    let in_use = InUse::of(&assets, root);
    if let Some(missing) = in_use.unreadable().first() {
        return Err(format!(
            "{} of the project's media cannot be found, such as {}, so what the project uses cannot be told",
            in_use.unreadable().len(),
            missing.display()
        ));
    }
    Ok(prune(root, cap, &in_use, now))
}

/// How a condition changed between two passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Began,
    Ended,
    Same,
}

fn edge(was: bool, is: bool) -> Edge {
    match (was, is) {
        (false, true) => Edge::Began,
        (true, false) => Edge::Ended,
        _ => Edge::Same,
    }
}

/// Something worth saying about the state of the cache, which is said when it changes and not on every
/// pass while it lasts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Notice {
    /// The pass could not tell what the project uses (and why).
    Skipped(String),
    /// ... and now it can.
    Resumed,
    /// What is kept alone is over the cap.
    Over,
    /// ... and it is not any more.
    Under,
}

/// What the pruner has already said.
#[derive(Default)]
struct Told {
    over: bool,
    skipped: Option<String>,
}

impl Told {
    /// The notices a pass's outcome calls for, given what was said before (pure).
    fn note(&mut self, outcome: &Result<Report, String>, cap: u64) -> Vec<Notice> {
        let mut notices = Vec::new();
        let report = match outcome {
            Ok(report) => report,
            Err(reason) => {
                if self.skipped.as_deref() != Some(reason) {
                    notices.push(Notice::Skipped(reason.clone()));
                }
                self.skipped = Some(reason.clone());
                return notices;
            }
        };
        if self.skipped.take().is_some() {
            notices.push(Notice::Resumed);
        }
        let over = report.after > cap;
        match edge(self.over, over) {
            Edge::Began => notices.push(Notice::Over),
            Edge::Ended => notices.push(Notice::Under),
            Edge::Same => {}
        }
        self.over = over;
        notices
    }
}

/// Log a pass: what it freed (or that the cap is in force, on the first), then `notices`.
fn log(outcome: &Result<Report, String>, notices: &[Notice], cap: u64, first: bool) {
    if let Ok(report) = outcome {
        let freed = report
            .by_group
            .iter()
            .map(|(group, bytes)| format!("{group} {}", gib(*bytes)))
            .collect::<Vec<_>>()
            .join(", ");
        if report.removed > 0 {
            tracing::info!(
                freed = %gib(report.freed),
                removed = report.removed,
                now = %gib(report.after),
                cap = %gib(cap),
                "pruned the cache ({freed})"
            );
        } else if first {
            tracing::info!(now = %gib(report.after), cap = %gib(cap), "cache cap in force");
        }
    }
    for notice in notices {
        match (notice, outcome) {
            (Notice::Skipped(reason), _) => {
                tracing::warn!("the cache pruner skips a pass until it can tell what the project uses: {reason}");
            }
            (Notice::Resumed, _) => tracing::info!("the cache pruner can tell what the project uses again"),
            (Notice::Over, Ok(report)) => tracing::warn!(
                now = %gib(report.after),
                cap = %gib(cap),
                kept = %gib(report.pinned),
                "the cache is over its cap: what the open project uses and what is being written are kept"
            ),
            (Notice::Under, Ok(report)) => {
                tracing::info!(now = %gib(report.after), cap = %gib(cap), "the cache is back under its cap");
            }
            (Notice::Over | Notice::Under, Err(_)) => {}
        }
    }
}

// ---- the pruner ---------------------------------------------------------------

static RUNNING: AtomicBool = AtomicBool::new(false);

struct Signal {
    pending: Mutex<bool>,
    wake: Condvar,
}

fn signal() -> &'static Signal {
    static SIGNAL: OnceLock<Signal> = OnceLock::new();
    SIGNAL.get_or_init(|| Signal {
        pending: Mutex::new(false),
        wake: Condvar::new(),
    })
}

/// Ask the pruner to look soon: a proxy or an analysis has just finished and may have grown the
/// caches. A no-op when no pruner runs, which is every run but a capped headless one.
pub fn nudge() {
    if RUNNING.load(Ordering::Relaxed) {
        let signal = signal();
        *signal.pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        signal.wake.notify_one();
    }
}

/// Sleep until the next run is due: [`INTERVAL`] from now, or [`SETTLE`] after a [`nudge`].
fn wait() {
    let signal = signal();
    let mut pending = signal.pending.lock().unwrap_or_else(|e| e.into_inner());
    while !*pending {
        let (guard, timeout) = signal.wake.wait_timeout(pending, INTERVAL).unwrap_or_else(|e| e.into_inner());
        pending = guard;
        if timeout.timed_out() {
            break;
        }
    }
    let nudged = std::mem::take(&mut *pending);
    drop(pending);
    if nudged {
        std::thread::sleep(SETTLE);
        *signal.pending.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }
}

/// Keep the caches under `cap` bytes from now on: a pass at once, then one every [`INTERVAL`] and one
/// soon after each [`nudge`], on a thread of its own at low priority. `assets` hands over the open
/// project's assets (the adapter takes the project lock for it and releases it on return, and says
/// `Err` when it cannot read them: that pass is skipped); what they use is kept. `Err` with the reason
/// when the cap cannot be kept: a pruner already runs, the platform has no cache directory, or the
/// thread would not start.
pub fn start(cap: u64, assets: impl Fn() -> crate::Result<Vec<Asset>> + Send + 'static) -> Result<(), String> {
    let root = crate::paths::cache_dir()
        .map(|d| d.join("kerf"))
        .ok_or("there is no cache directory to cap (--cache-dir names one)")?;
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("a cache pruner already runs".into());
    }
    let spawned = std::thread::Builder::new().name("kerf-cache-cap".into()).spawn(move || {
        crate::engine::cpu::lower_current_thread();
        let (mut told, mut first) = (Told::default(), true);
        loop {
            let run = std::panic::catch_unwind(AssertUnwindSafe(|| run_once(&root, cap, &assets, SystemTime::now())));
            match run {
                Ok(outcome) => {
                    let notices = told.note(&outcome, cap);
                    log(&outcome, &notices, cap, first);
                    first = false;
                }
                Err(_) => tracing::error!("pruning the cache panicked; trying again later"),
            }
            wait();
        }
    });
    if let Err(e) = spawned {
        RUNNING.store(false, Ordering::SeqCst);
        return Err(format!("could not start the cache pruner: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn at(secs_ago: u64, now: SystemTime) -> SystemTime {
        now - Duration::from_secs(secs_ago)
    }

    fn entry(name: &str, bytes: u64, used: SystemTime, class: Class) -> CacheEntry {
        CacheEntry {
            group: "waveforms",
            paths: vec![(PathBuf::from(name), bytes)],
            bytes,
            used,
            class,
        }
    }

    fn names(entries: &[CacheEntry], plan: &Plan) -> Vec<String> {
        plan.remove
            .iter()
            .map(|&i| entries[i].paths[0].0.display().to_string())
            .collect()
    }

    #[test]
    fn nothing_goes_while_the_total_fits() {
        let now = SystemTime::now();
        let entries = [
            entry("a", 40, at(3 * 86400, now), Class::Evictable),
            entry("b", 60, at(2 * 86400, now), Class::Evictable),
        ];
        let plan = prune_plan(&entries, 100);
        assert_eq!(plan.remove, Vec::<usize>::new());
        assert_eq!((plan.total, plan.after), (100, 100));
        assert_eq!(prune_plan(&[], 0), Plan::default());
    }

    #[test]
    fn the_least_recently_used_go_first_and_only_until_it_fits() {
        let now = SystemTime::now();
        let entries = [
            entry("new", 50, at(86400, now), Class::Evictable),
            entry("oldest", 30, at(9 * 86400, now), Class::Evictable),
            entry("old", 30, at(5 * 86400, now), Class::Evictable),
            entry("middle", 30, at(3 * 86400, now), Class::Evictable),
        ];
        let plan = prune_plan(&entries, 100);
        assert_eq!(names(&entries, &plan), ["oldest", "old"]);
        assert_eq!((plan.total, plan.after), (140, 80));
        let exact = prune_plan(&entries, 110);
        assert_eq!(names(&entries, &exact), ["oldest"], "stops the moment it fits");
        assert_eq!(exact.after, 110, "at the cap is within it");
        let all = prune_plan(&entries, 0);
        assert_eq!(names(&entries, &all), ["oldest", "old", "middle", "new"]);
        assert_eq!(all.after, 0);
    }

    #[test]
    fn what_is_in_use_or_in_flight_is_never_planned_however_far_over_the_cap() {
        let now = SystemTime::now();
        let long_ago = at(400 * 86400, now);
        let entries = [
            entry("proxy of the open project", 500, long_ago, Class::InUse),
            entry("being written", 300, long_ago, Class::InFlight),
            entry("idle", 100, at(86400, now), Class::Evictable),
        ];
        let plan = prune_plan(&entries, 10);
        assert_eq!(names(&entries, &plan), ["idle"]);
        assert_eq!(plan.pinned, 800);
        assert_eq!(plan.after, 800, "what is kept alone is over the cap, and the plan says so");
        let nothing_idle = prune_plan(&entries[..2], 10);
        assert!(nothing_idle.remove.is_empty());
        assert_eq!(nothing_idle.after, 800);
    }

    #[test]
    fn a_crashed_write_goes_before_anything_idle_however_old() {
        let now = SystemTime::now();
        let entries = [
            entry("ancient but good", 50, at(900 * 86400, now), Class::Evictable),
            entry("crashed", 50, at(2 * 86400, now), Class::Stale),
        ];
        let plan = prune_plan(&entries, 60);
        assert_eq!(names(&entries, &plan), ["crashed"]);
    }

    #[test]
    fn ties_are_broken_by_name_and_empty_units_are_not_worth_removing() {
        let now = SystemTime::now();
        let same = at(86400, now);
        let entries = [
            entry("b", 10, same, Class::Evictable),
            entry("a", 10, same, Class::Evictable),
            entry("empty", 0, at(500 * 86400, now), Class::Evictable),
        ];
        let plan = prune_plan(&entries, 10);
        assert_eq!(names(&entries, &plan), ["a"]);
        let again = prune_plan(&entries, 10);
        assert_eq!(plan, again, "the same answer every time");
        assert!(!prune_plan(&entries, 0).remove.contains(&2));
    }

    #[test]
    fn a_unit_is_classified_by_its_name_use_and_age() {
        let now = SystemTime::now();
        let old = at(30 * 86400, now);
        assert_eq!(classify(false, false, old, now), Class::Evictable);
        assert_eq!(classify(false, true, old, now), Class::InUse);
        assert_eq!(classify(true, true, old, now), Class::InUse);
        assert_eq!(
            classify(false, false, at(60, now), now),
            Class::InFlight,
            "touched a minute ago"
        );
        assert_eq!(
            classify(false, false, now - RECENT - Duration::from_secs(1), now),
            Class::Evictable
        );
        assert_eq!(classify(true, false, at(60, now), now), Class::InFlight, "a running write");
        assert_eq!(classify(true, false, at(3600, now), now), Class::InFlight, "a long encode");
        assert_eq!(
            classify(true, false, now - STALE_PARTIAL - Duration::from_secs(1), now),
            Class::Stale
        );
        assert_eq!(classify(false, false, now + DAY, now), Class::InFlight, "a clock that moved");
    }

    #[test]
    fn partial_names_are_the_ones_a_write_leaves() {
        for partial in [
            "0123456789abcdef.4242.0.part",
            "0123456789abcdef.json.4242.0.part",
            "vocals.4242.part.flac",
            "abc.1.0.part",
        ] {
            assert!(is_partial(partial), "{partial}");
        }
        for done in [
            "0123456789abcdef.mp4",
            "0123456789abcdef.lead.json",
            "vocals.flac",
            "manifest.json",
        ] {
            assert!(!is_partial(done), "{done}");
        }
        assert_eq!(pair_key("0123.mp4"), "0123");
        assert_eq!(pair_key("0123.json"), "0123");
        assert_eq!(pair_key("0123.lead.mp4"), "0123.lead");
        assert_eq!(pair_key("0123.lead.json"), "0123.lead");
        assert_eq!(pair_key("notes.txt"), "notes.txt");
    }

    // ---- on a disk ----

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kerf-cache-cap-{tag}-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// A file of `bytes` bytes last used `age` ago.
        fn file(&self, rel: &str, bytes: usize, age: Duration) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, vec![7u8; bytes]).unwrap();
            let when = SystemTime::now() - age;
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(FileTimes::new().set_accessed(when).set_modified(when))
                .unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn weeks(n: u64) -> Duration {
        Duration::from_secs(n * 7 * 86400)
    }

    /// A cache folder as a headless run leaves it, with `age` for everything in it.
    fn populated(dir: &Dir, age: Duration) {
        dir.file("proxies/aaaa.mp4", 400, age);
        dir.file("proxies/aaaa.json", 4, age);
        dir.file("proxies/bbbb.lead.mp4", 300, age);
        dir.file("proxies/bbbb.lead.json", 3, age);
        dir.file("waveforms/w1.bin", 100, age);
        dir.file("music/m1.json", 10, age);
        dir.file("filmstrips/f1/sheet-000.jpg", 60, age);
        dir.file("filmstrips/f1/manifest.json", 5, age);
        dir.file("stems/s1/vocals.flac", 200, age);
        dir.file("stems/s1/drums.flac", 200, age);
    }

    fn by_group(entries: &[CacheEntry]) -> BTreeMap<&'static str, (usize, u64)> {
        let mut out: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
        for e in entries {
            let slot = out.entry(e.group).or_default();
            slot.0 += 1;
            slot.1 += e.bytes;
        }
        out
    }

    #[test]
    fn a_scan_finds_a_proxy_and_its_sidecar_as_one_unit_and_leaves_the_downloads_out() {
        let dir = Dir::new("scan");
        populated(&dir, weeks(2));
        dir.file("models/ggml-base.bin", 9000, weeks(9));
        dir.file("soundfonts/font.sf2", 9000, weeks(9));
        dir.file("runtime/onnxruntime-1/lib.so", 9000, weeks(9));
        dir.file("stitched/lens.mp4", 9000, weeks(9));
        dir.file("luts/a.cube", 9000, weeks(9));
        dir.file("sfx-preview/p.wav", 9000, weeks(9));
        dir.file("stabilize/slices/x.trf", 9000, weeks(9));
        dir.file("segments/1-0/part0.mp4", 9000, weeks(9));
        let entries = scan(&dir.0, &InUse::default(), SystemTime::now());
        assert_eq!(
            by_group(&entries),
            BTreeMap::from([
                ("filmstrips", (1, 65)),
                ("music", (1, 10)),
                ("proxies", (2, 707)),
                ("stems", (1, 400)),
                ("waveforms", (1, 100)),
            ])
        );
        assert!(entries.iter().all(|e| e.class == Class::Evictable));
        let proxy = entries
            .iter()
            .find(|e| e.paths[0].0.ends_with("aaaa.mp4"))
            .expect("the proxy is the first of its unit");
        assert!(proxy.paths[1].0.ends_with("aaaa.json"), "the sidecar goes after it");
        assert_eq!(proxy.bytes, 404);
    }

    #[test]
    fn a_scan_marks_writes_in_progress_and_what_the_open_project_uses() {
        let dir = Dir::new("classes");
        populated(&dir, weeks(2));
        dir.file("proxies/cccc.4242.0.part", 50, Duration::from_secs(30));
        dir.file("proxies/dddd.4242.1.part", 50, 3 * DAY);
        dir.file("filmstrips/g1.7.0.part/sheet-000.jpg", 20, Duration::from_secs(5));
        dir.file("stems/s2/vocals.4242.part.flac", 70, Duration::from_secs(5));
        dir.file("stems/s2/drums.flac", 70, weeks(2));
        dir.file("waveforms/fresh.bin", 10, Duration::from_secs(60));
        let used = InUse::from_paths([
            dir.0.join("proxies/aaaa.json"),
            dir.0.join("proxies/aaaa.mp4"),
            dir.0.join("stems/s1"),
            dir.0.join("waveforms/not-there.bin"),
        ]);
        let entries = scan(&dir.0, &used, SystemTime::now());
        let class = |tail: &str| {
            entries
                .iter()
                .find(|e| e.paths[0].0.ends_with(tail))
                .unwrap_or_else(|| panic!("{tail}"))
                .class
        };
        assert_eq!(class("aaaa.mp4"), Class::InUse, "a proxy is kept with its sidecar");
        assert_eq!(class("stems/s1"), Class::InUse, "a directory is kept whole");
        assert_eq!(class("bbbb.lead.mp4"), Class::Evictable);
        assert_eq!(class("cccc.4242.0.part"), Class::InFlight);
        assert_eq!(class("dddd.4242.1.part"), Class::Stale);
        assert_eq!(class("g1.7.0.part"), Class::InFlight);
        assert_eq!(
            class("stems/s2"),
            Class::InFlight,
            "one partial file holds back the whole folder"
        );
        assert_eq!(class("fresh.bin"), Class::InFlight, "touched a minute ago");
        assert_eq!(class("w1.bin"), Class::Evictable);
    }

    #[test]
    fn a_run_frees_the_oldest_until_it_fits_and_keeps_everything_it_must() {
        let dir = Dir::new("prune");
        let (old, mid, new) = (weeks(8), weeks(4), weeks(1));
        dir.file("proxies/p-old.mp4", 1000, old);
        dir.file("proxies/p-old.json", 10, old);
        dir.file("proxies/p-open.mp4", 1000, old);
        dir.file("proxies/p-open.json", 10, old);
        dir.file("proxies/p-new.mp4", 1000, new);
        dir.file("waveforms/w-mid.bin", 500, mid);
        dir.file("waveforms/w-new.bin", 500, new);
        dir.file("filmstrips/f-mid/sheet-000.jpg", 250, mid);
        dir.file("filmstrips/f-mid/manifest.json", 50, mid);
        dir.file("stems/s-mid/vocals.flac", 800, mid);
        dir.file("proxies/p-building.4242.0.part", 700, Duration::from_secs(20));
        dir.file("models/ggml-base.bin", 100_000, weeks(40));
        dir.file("stitched/lens.mp4", 100_000, weeks(40));
        let used = InUse::from_paths([dir.0.join("proxies/p-open.mp4"), dir.0.join("proxies/p-open.json")]);

        let epoch = crate::plan_epoch::bumps_here();
        let report = prune(&dir.0, 3500, &used, SystemTime::now());

        // 5820 bytes are managed, 1710 of them kept (the open project's proxy, the running encode): the
        // oldest unit goes first (p-old), then the mid-age ones until it fits, and the newer two stay.
        assert_eq!(report.before, 5820);
        assert_eq!(report.pinned, 1710);
        assert!(report.after <= 3500, "{report:?}");
        assert!(!dir.0.join("proxies/p-old.mp4").exists());
        assert!(!dir.0.join("proxies/p-old.json").exists(), "the sidecar goes with its proxy");
        assert!(dir.0.join("proxies/p-open.mp4").exists(), "the open project's proxy stays");
        assert!(dir.0.join("proxies/p-open.json").exists());
        assert!(
            dir.0.join("proxies/p-new.mp4").exists(),
            "recently used stays while older goes"
        );
        assert!(
            dir.0.join("proxies/p-building.4242.0.part").exists(),
            "a running encode is not touched"
        );
        assert!(dir.0.join("models/ggml-base.bin").exists(), "a model is not a cache");
        assert!(
            dir.0.join("stitched/lens.mp4").exists(),
            "a stitched file is an asset's source"
        );
        assert_eq!(report.freed, report.before - report.after);
        assert_eq!(report.removed, 4);
        assert_eq!(report.by_group["proxies"], 1010);
        assert!(
            crate::plan_epoch::bumps_here() > epoch,
            "a plan made around a proxy is remade"
        );

        let again = prune(&dir.0, 3500, &used, SystemTime::now());
        assert_eq!((again.removed, again.freed), (0, 0), "a second run has nothing to do");
        assert_eq!(again.after, report.after);
    }

    #[test]
    fn a_run_over_a_cache_that_cannot_fit_keeps_the_open_project_and_says_so() {
        let dir = Dir::new("over");
        dir.file("proxies/open.mp4", 1000, weeks(8));
        dir.file("waveforms/idle.bin", 100, weeks(8));
        let used = InUse::from_paths([dir.0.join("proxies/open.mp4")]);
        let report = prune(&dir.0, 10, &used, SystemTime::now());
        assert!(dir.0.join("proxies/open.mp4").exists());
        assert!(!dir.0.join("waveforms/idle.bin").exists());
        assert_eq!((report.after, report.pinned), (1000, 1000));
        assert!(report.after > 10);
        assert!(!report.by_group.contains_key("proxies"));
    }

    #[test]
    fn a_missing_cache_is_an_empty_one() {
        let dir = Dir::new("empty");
        let report = prune(&dir.0.join("never-made"), 0, &InUse::default(), SystemTime::now());
        assert_eq!(report, Report::default());
        assert!(scan(&dir.0, &InUse::default(), SystemTime::now()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_a_cache_is_not_followed() {
        let dir = Dir::new("link");
        let outside = Dir::new("outside");
        let keep = outside.file("keep/data.bin", 100, weeks(9));
        std::fs::create_dir_all(dir.0.join("stems")).unwrap();
        std::fs::create_dir_all(dir.0.join("waveforms")).unwrap();
        std::os::unix::fs::symlink(outside.0.join("keep"), dir.0.join("stems/linked")).unwrap();
        std::os::unix::fs::symlink(&keep, dir.0.join("waveforms/linked.bin")).unwrap();
        assert!(scan(&dir.0, &InUse::default(), SystemTime::now()).is_empty());
        let report = prune(&dir.0, 0, &InUse::default(), SystemTime::now());
        assert_eq!(report.removed, 0);
        assert!(keep.exists());
    }

    #[test]
    fn the_open_project_s_assets_name_their_cache_files() {
        let mut asset = crate::engine::test_support::av_asset(uuid::Uuid::new_v4(), 12.0);
        asset.path = "/media/cache-cap-never-imported.mov".into();
        let files = crate::engine::asset_cache_files(&asset);
        let Some(cache) = crate::paths::cache_dir() else {
            assert!(files.is_empty());
            return;
        };
        let under = |group: &str| files.iter().filter(|f| f.starts_with(cache.join("kerf").join(group))).count();
        assert_eq!(under("proxies"), 8, "four spellings, each with a sidecar");
        for group in ["waveforms", "music", "stems", "filmstrips"] {
            assert_eq!(under(group), 1, "{group}");
        }
        let used = InUse::of(&[asset], Path::new("/nowhere/kerf"));
        let Some(first) = files.first() else { return };
        assert!(used.holds(first));
        assert!(!used.holds(Path::new("/elsewhere/entirely.mp4")));
        assert_eq!(
            used.unreadable(),
            [PathBuf::from("/media/cache-cap-never-imported.mov")],
            "an original that is not there is reported"
        );
    }

    /// An asset whose file is `path`, which exists.
    fn asset_at(path: &Path) -> Asset {
        let mut asset = crate::engine::test_support::av_asset(uuid::Uuid::new_v4(), 12.0);
        asset.path = path.display().to_string();
        asset
    }

    #[test]
    fn a_stem_the_project_imported_keeps_its_folder_whole() {
        let dir = Dir::new("stems");
        let old = weeks(9);
        let vocals = dir.file("stems/s1/vocals.flac", 200, old);
        dir.file("stems/s1/drums.flac", 200, old);
        dir.file("stems/s1/bass.flac", 200, old);
        dir.file("stems/s1/other.flac", 200, old);
        dir.file("stems/s2/vocals.flac", 200, old);
        dir.file("waveforms/idle.bin", 100, old);
        let assets = [asset_at(&vocals)];

        let used = InUse::of(&assets, &dir.0);
        assert!(used.holds(&dir.0.join("stems/s1")), "the unit that holds it");
        assert!(used.holds(&vocals));
        assert!(!used.holds(&dir.0.join("stems/s2")));
        assert!(!used.holds(&dir.0), "not the cache itself");

        let report = run_once(&dir.0, 0, &|| Ok(assets.to_vec()), SystemTime::now()).expect("it can tell");
        for stem in ["vocals", "drums", "bass", "other"] {
            assert!(
                dir.0.join(format!("stems/s1/{stem}.flac")).exists(),
                "{stem} is the project's media"
            );
        }
        assert!(!dir.0.join("stems/s2").exists(), "a stems folder nothing uses goes");
        assert!(!dir.0.join("waveforms/idle.bin").exists());
        assert_eq!((report.pinned, report.after), (800, 800));
    }

    #[cfg(unix)]
    #[test]
    fn an_asset_in_the_cache_is_kept_however_a_link_spells_its_path() {
        let dir = Dir::new("spelled");
        let linked = Dir::new("spelled-link");
        let real = dir.file("stems/s1/vocals.flac", 10, weeks(9));
        dir.file("stems/s2/vocals.flac", 10, weeks(9));
        let link = linked.0.join("cache");
        std::os::unix::fs::symlink(&dir.0, &link).unwrap();

        // The project has the file by its real name, the pruner reaches the cache through the link.
        let used = InUse::of(&[asset_at(&real)], &link);
        assert!(used.holds(&link.join("stems/s1")));
        // ... and the other way round.
        let used = InUse::of(&[asset_at(&link.join("stems/s1/vocals.flac"))], &dir.0);
        assert!(used.holds(&dir.0.join("stems/s1")));
        assert!(!used.holds(&dir.0.join("stems/s2")));
    }

    #[test]
    fn a_pass_that_cannot_tell_what_the_project_uses_removes_nothing() {
        let dir = Dir::new("blind");
        populated(&dir, weeks(9));
        let before = |dir: &Dir| scan(&dir.0, &InUse::default(), SystemTime::now()).len();
        let units = before(&dir);
        assert!(units > 0);

        let failed = run_once(
            &dir.0,
            0,
            &|| Err(crate::Error::Engine("database is locked".into())),
            SystemTime::now(),
        )
        .unwrap_err();
        assert!(
            failed.contains("could not be read") && failed.contains("database is locked"),
            "{failed}"
        );
        assert_eq!(before(&dir), units, "an unreadable project is not an empty one");

        let gone = asset_at(&dir.0.join("an-original-on-a-share-that-went-away.mov"));
        let skipped = run_once(&dir.0, 0, &|| Ok(vec![gone.clone()]), SystemTime::now()).unwrap_err();
        assert!(skipped.contains("1 of the project's media cannot be found"), "{skipped}");
        assert!(skipped.contains("went-away.mov"), "{skipped}");
        assert_eq!(before(&dir), units, "an original that cannot be statted hides its caches");

        let report = run_once(&dir.0, 0, &|| Ok(Vec::new()), SystemTime::now()).expect("an empty project is known");
        assert!(report.removed > 0, "once it can tell, the pass runs");
        assert_eq!(before(&dir), 0);
    }

    #[test]
    fn a_condition_that_lasts_is_told_once() {
        assert_eq!(edge(false, true), Edge::Began);
        assert_eq!(edge(true, true), Edge::Same, "still over the cap: said already");
        assert_eq!(edge(false, false), Edge::Same);
        assert_eq!(edge(true, false), Edge::Ended);

        let report = |after| {
            Ok(Report {
                after,
                pinned: after,
                ..Report::default()
            })
        };
        let mut told = Told::default();
        assert_eq!(told.note(&report(50), 100), []);
        assert_eq!(told.note(&report(300), 100), [Notice::Over]);
        assert_eq!(told.note(&report(300), 100), [], "the next pass over the cap is not news");
        assert_eq!(told.note(&report(310), 100), []);
        assert_eq!(told.note(&report(80), 100), [Notice::Under]);

        let blind: Result<Report, String> = Err("the database is locked".into());
        assert_eq!(told.note(&blind, 100), [Notice::Skipped("the database is locked".into())]);
        assert_eq!(told.note(&blind, 100), [], "a pass skipped for the same reason is not news");
        let other: Result<Report, String> = Err("a share went away".into());
        assert_eq!(told.note(&other, 100), [Notice::Skipped("a share went away".into())]);
        assert_eq!(told.note(&report(300), 100), [Notice::Resumed, Notice::Over]);
    }

    #[test]
    fn a_nudge_with_no_pruner_running_does_nothing() {
        if !RUNNING.load(Ordering::Relaxed) {
            nudge();
            assert!(!*signal().pending.lock().unwrap());
        }
    }
}
