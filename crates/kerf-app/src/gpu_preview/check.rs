//! The GPU path draws nothing the user looks at until it has been checked against this machine's
//! FFmpeg ([`kerf_gpu::parity_self_test`]): the first frame that wants the GPU starts the check on a
//! thread of its own and goes through FFmpeg, as does every frame until the verdict.
//!
//! * **Passed**: the GPU path is open, and stays so for the session.
//! * **Mismatch** (the GPU draws something else than this FFmpeg does) and **could not check** (an
//!   FFmpeg that would not make the fixtures, a colour policy that could not be measured, a deadline,
//!   the GPU failing on the check frame): the GPU path is closed, and the reason is what Settings shows.
//!   Turning the setting off and on again runs the check again, for either: what failed may not be
//!   there next time (a driver, an FFmpeg, a machine under load), and a mismatch that is real is found
//!   again at the price of a couple of seconds of work. Every verdict is logged.
//!
//! The verdict belongs to an adapter and an FFmpeg ([`AdapterKey`]): a backend rebuilt on the same
//! device after a loss keeps it, one rebuilt on another adapter is checked again.
//! `KERF_GPU_SELFTEST=off` skips the check (a developer's escape hatch; it is logged).
//!
//! **A mismatch outlives the process** ([`Check::refuse`], [`super::block`]): the adapter and FFmpeg it
//! was found on are kept in the settings, and the next launch starts with that key closed instead of
//! drawing on it again for a second to find out. A driver update, another FFmpeg or a new Kerf is another
//! key (or another version) and is checked again; switching the setting off and on forgets it, like any
//! failed check. A check that *could not be made* is not kept: what stopped it may not be there next time.

use std::sync::{Arc, Mutex, PoisonError};

use kerf_gpu::{Gpu, SelfTestError, SelfTestReport};

/// The adapter and the FFmpeg a verdict is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterKey(Arc<str>);

impl AdapterKey {
    /// Built once per backend and carried with it: a frame only clones the handle.
    pub fn of(gpu: &Gpu) -> Self {
        let info = gpu.adapter_info();
        Self(
            format!(
                "{} | {:?} | {} | {}",
                info.name,
                info.backend,
                info.driver_info,
                ffmpeg_identity()
            )
            .into(),
        )
    }

    /// What a block in the settings names ([`Check::refuse`]).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub fn named(name: &str) -> Self {
        Self(name.into())
    }
}

/// Which FFmpeg a key is about: the binary the engine runs, by where it is and by **size and modification time**,
/// not by version: a version would be a process spawned (with a timeout) for every backend that is built, and a
/// binary replaced in place (a distro's package update, a bundle's new FFmpeg) changes its size or its time
/// all the same. An FFmpeg that cannot be found or read is named by its path alone.
pub fn ffmpeg_identity() -> String {
    identity_of(&kerf_core::ffmpeg_path(), std::env::var_os("PATH").as_deref())
}

/// [`ffmpeg_identity`] for `program` (a path, or a name looked for in `search_path`).
fn identity_of(program: &str, search_path: Option<&std::ffi::OsStr>) -> String {
    let Some(file) = resolve(program, search_path) else {
        return program.to_string();
    };
    match std::fs::metadata(&file) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            format!("{} ({} bytes, {})", file.display(), meta.len(), modified)
        }
        Err(_) => program.to_string(),
    }
}

/// The file `program` runs: itself when it names a path, else the first directory of `search_path` that holds it
/// (with `.exe` too, where programs have one).
fn resolve(program: &str, search_path: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    std::env::split_paths(search_path?).find_map(|dir| {
        [program.to_string(), format!("{program}.exe")]
            .into_iter()
            .map(|name| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// What the gate says about a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// The GPU path is open.
    Open,
    /// The check is under way (or is about to be started by the caller: [`Gate::Begin`]).
    Wait(String),
    /// The caller starts the check now and finishes it with [`Check::finish`]; the frame goes through
    /// FFmpeg meanwhile.
    Begin(String),
    /// Closed, and why.
    Closed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Idle,
    Running,
    Passed(String),
    Failed(String),
}

#[derive(Debug)]
struct Slot {
    key: Option<AdapterKey>,
    phase: Phase,
    /// A mismatch an earlier run found, kept in the settings: the key it was found on and why. The key's
    /// first gate says closed with this, without a check.
    known_bad: Option<(String, String)>,
}

const CHECKING: &str = "checking the GPU against this machine's FFmpeg (the first frame the GPU would draw)";

/// What happened to a check, for the caller to log and show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Passed(String),
    Failed(String),
    /// The result belongs to an adapter that is no longer the one in use.
    Stale,
}

pub struct Check {
    slot: Mutex<Slot>,
    skipped: bool,
}

impl Default for Check {
    fn default() -> Self {
        Self::new(std::env::var("KERF_GPU_SELFTEST").is_ok_and(|v| v.eq_ignore_ascii_case("off") || v == "0"))
    }
}

impl Check {
    pub fn new(skipped: bool) -> Self {
        Self {
            slot: Mutex::new(Slot {
                key: None,
                phase: Phase::Idle,
                known_bad: None,
            }),
            skipped,
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// May a frame be drawn on `key`'s adapter? The first call for an adapter that has no verdict
    /// says [`Gate::Begin`], once.
    pub fn gate(&self, key: &AdapterKey) -> Gate {
        if self.skipped {
            return Gate::Open;
        }
        let mut slot = self.slot();
        if slot.key.as_ref() != Some(key) {
            let known_bad = slot.known_bad.take();
            let phase = match &known_bad {
                Some((bad, reason)) if bad == key.as_str() => Phase::Failed(reason.clone()),
                _ => Phase::Idle,
            };
            *slot = Slot {
                key: Some(key.clone()),
                phase,
                known_bad,
            };
        }
        match &slot.phase {
            Phase::Passed(_) => Gate::Open,
            Phase::Running => Gate::Wait(CHECKING.into()),
            Phase::Failed(reason) => Gate::Closed(reason.clone()),
            Phase::Idle => {
                slot.phase = Phase::Running;
                Gate::Begin(CHECKING.into())
            }
        }
    }

    /// Whether the check has passed on `key`'s adapter (or is off), **without** starting one: a
    /// caller that only wants to know whether the GPU would be allowed to draw. [`Check::gate`] is
    /// the one that begins a check, once.
    pub fn is_open(&self, key: &AdapterKey) -> bool {
        if self.skipped {
            return true;
        }
        let slot = self.slot();
        slot.key.as_ref() == Some(key) && matches!(slot.phase, Phase::Passed(_))
    }

    /// The check on `key` ended with `result`.
    pub fn finish(&self, key: &AdapterKey, result: Result<SelfTestReport, SelfTestError>) -> Verdict {
        let mut slot = self.slot();
        if slot.key.as_ref() != Some(key) {
            return Verdict::Stale;
        }
        match result {
            Ok(report) => {
                let summary = report.summary();
                slot.phase = Phase::Passed(summary.clone());
                slot.known_bad = None;
                Verdict::Passed(summary)
            }
            Err(error) => {
                let reason = format!("{error} — the GPU preview stays off until it is turned off and on again");
                slot.phase = Phase::Failed(reason.clone());
                Verdict::Failed(reason)
            }
        }
    }

    /// The check could not even be started (a thread that would not run): the same as a check that
    /// could not be made.
    pub fn failed_to_start(&self, key: &AdapterKey, why: &str) -> Verdict {
        self.finish(key, Err(SelfTestError::Inconclusive(why.to_string())))
    }

    /// The setting was turned on: a check that failed, whatever way (and a mismatch an earlier run
    /// kept), is run again; a pass stands.
    pub fn retry(&self) {
        let mut slot = self.slot();
        slot.known_bad = None;
        if matches!(slot.phase, Phase::Failed(_)) {
            slot.phase = Phase::Idle;
        }
    }

    /// A mismatch an earlier run found on `adapter` (an [`AdapterKey`]'s text) for `reason`: that key is
    /// closed from its first frame on. Call it after the setting was put in force, not before: turning
    /// the setting on is a [`Check::retry`].
    pub fn refuse(&self, adapter: &str, reason: &str) {
        let mut slot = self.slot();
        slot.known_bad = Some((adapter.to_string(), reason.to_string()));
        if slot.key.as_ref().is_some_and(|held| held.as_str() == adapter) {
            slot.phase = Phase::Failed(reason.to_string());
        }
    }

    /// Why the gate is closed on `key`'s adapter, if it is: what Settings keeps showing when a
    /// backend is rebuilt on the same adapter after the verdict, or has been built on the adapter an
    /// earlier run found wanting.
    pub fn closed(&self, key: &AdapterKey) -> Option<String> {
        let slot = self.slot();
        match (&slot.phase, slot.key.as_ref()) {
            (Phase::Failed(reason), Some(held)) if held == key => Some(reason.clone()),
            (_, held) if held != Some(key) => match &slot.known_bad {
                Some((bad, reason)) if bad == key.as_str() => Some(reason.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The summary of the passed check, if there is one, or the reason it is closed.
    #[cfg(test)]
    pub fn standing(&self) -> Option<String> {
        match &self.slot().phase {
            Phase::Passed(summary) => Some(format!("passed: {summary}")),
            Phase::Failed(reason) => Some(format!("closed: {reason}")),
            Phase::Running => Some("running".into()),
            Phase::Idle => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::CompositeColorPolicy;
    use std::time::Duration;

    fn report() -> SelfTestReport {
        SelfTestReport {
            cases: Vec::new(),
            policy: CompositeColorPolicy::FixedBt601,
            elapsed: Duration::from_millis(900),
        }
    }

    fn mismatch() -> SelfTestError {
        SelfTestError::Mismatch(report())
    }

    #[test]
    fn the_first_frame_begins_the_check_and_the_ones_after_it_wait_for_it() {
        let check = Check::new(false);
        let key = AdapterKey::named("lavapipe");
        assert!(matches!(check.gate(&key), Gate::Begin(_)));
        assert!(matches!(check.gate(&key), Gate::Wait(_)), "once, not per frame");
        assert!(matches!(check.gate(&key), Gate::Wait(_)));
        assert!(matches!(check.finish(&key, Ok(report())), Verdict::Passed(_)));
        assert_eq!(check.gate(&key), Gate::Open);
        assert_eq!(check.gate(&key), Gate::Open);
        assert!(check.standing().unwrap().starts_with("passed"));
    }

    #[test]
    fn asking_whether_the_gate_is_open_never_begins_a_check() {
        let check = Check::new(false);
        let key = AdapterKey::named("lavapipe");
        assert!(!check.is_open(&key));
        assert!(!check.is_open(&key));
        // Nothing was begun by asking: the first frame that wants the GPU still begins it, once.
        assert!(matches!(check.gate(&key), Gate::Begin(_)));
        assert!(!check.is_open(&key), "running is not open");
        check.finish(&key, Ok(report()));
        assert!(check.is_open(&key));
        assert!(
            !check.is_open(&AdapterKey::named("another")),
            "a verdict belongs to its adapter"
        );
        assert!(Check::new(true).is_open(&key), "a check that is off is open");
    }

    #[test]
    fn a_mismatch_closes_the_gate_until_the_setting_is_turned_on_again_and_the_check_runs_again() {
        let check = Check::new(false);
        let key = AdapterKey::named("a gpu");
        check.gate(&key);
        let Verdict::Failed(reason) = check.finish(&key, Err(mismatch())) else {
            panic!("failed")
        };
        assert!(reason.contains("turned off and on again"), "{reason}");
        assert_eq!(check.gate(&key), Gate::Closed(reason.clone()));
        assert_eq!(check.closed(&key), Some(reason), "what Settings keeps showing");
        assert_eq!(check.closed(&AdapterKey::named("another")), None);
        check.retry();
        assert_eq!(check.closed(&key), None);
        assert!(matches!(check.gate(&key), Gate::Begin(_)), "the check runs again");
        // And a mismatch that is still there is found again.
        assert!(matches!(check.finish(&key, Err(mismatch())), Verdict::Failed(_)));
        assert!(matches!(check.gate(&key), Gate::Closed(_)));
    }

    #[test]
    fn a_check_that_could_not_be_made_closes_the_gate_until_the_setting_is_turned_on_again() {
        let check = Check::new(false);
        let key = AdapterKey::named("a gpu");
        check.gate(&key);
        let Verdict::Failed(reason) = check.finish(&key, Err(SelfTestError::Inconclusive("no ffmpeg".into()))) else {
            panic!("failed")
        };
        assert!(
            reason.contains("no ffmpeg") && reason.contains("turned off and on again"),
            "{reason}"
        );
        assert!(matches!(check.gate(&key), Gate::Closed(_)));
        check.retry();
        assert!(matches!(check.gate(&key), Gate::Begin(_)), "tried again");
        // A pass stands through a retry.
        check.finish(&key, Ok(report()));
        check.retry();
        assert_eq!(check.gate(&key), Gate::Open);
    }

    #[test]
    fn another_adapter_is_checked_again_and_a_late_result_for_the_old_one_is_dropped() {
        let check = Check::new(false);
        let (old, new) = (AdapterKey::named("integrated"), AdapterKey::named("discrete"));
        check.gate(&old);
        check.finish(&old, Ok(report()));
        assert_eq!(check.gate(&old), Gate::Open);
        assert!(matches!(check.gate(&new), Gate::Begin(_)), "a rebuild on another adapter");
        assert_eq!(check.finish(&old, Err(mismatch())), Verdict::Stale);
        assert!(matches!(check.gate(&new), Gate::Wait(_)));
        assert!(matches!(check.failed_to_start(&new, "no thread"), Verdict::Failed(_)));
    }

    #[test]
    fn a_mismatch_an_earlier_run_kept_closes_that_key_from_its_first_frame_and_no_other() {
        let check = Check::new(false);
        let (bad, other) = (
            AdapterKey::named("gpu | Vulkan | driver 1 | ffmpeg"),
            AdapterKey::named("gpu | Vulkan | driver 2 | ffmpeg"),
        );
        check.refuse(bad.as_str(), "draws differently");
        assert_eq!(
            check.closed(&bad).as_deref(),
            Some("draws differently"),
            "what Settings shows once a backend is up, before any frame"
        );
        assert_eq!(check.closed(&other), None);
        assert_eq!(
            check.gate(&bad),
            Gate::Closed("draws differently".into()),
            "no check on a key known to fail"
        );
        assert_eq!(check.gate(&bad), Gate::Closed("draws differently".into()));
        // A driver update is another key: it is checked, and a pass forgets what was kept.
        assert!(matches!(check.gate(&other), Gate::Begin(_)));
        assert!(matches!(check.finish(&other, Ok(report())), Verdict::Passed(_)));
        assert_eq!(check.gate(&other), Gate::Open);
        assert_eq!(check.closed(&bad), None, "a pass forgets what an earlier run kept");
        assert!(
            matches!(check.gate(&bad), Gate::Begin(_)),
            "the old key is checked again, not trusted"
        );
    }

    #[test]
    fn switching_the_setting_on_forgets_a_mismatch_an_earlier_run_kept() {
        let check = Check::new(false);
        let key = AdapterKey::named("gpu | Vulkan | driver 1 | ffmpeg");
        check.refuse(key.as_str(), "draws differently");
        assert!(matches!(check.gate(&key), Gate::Closed(_)));
        check.retry();
        assert_eq!(check.closed(&key), None);
        assert!(matches!(check.gate(&key), Gate::Begin(_)), "the user's word is a retry");
        // Also before any frame has been asked about.
        let fresh = Check::new(false);
        fresh.refuse(key.as_str(), "draws differently");
        fresh.retry();
        assert!(matches!(fresh.gate(&key), Gate::Begin(_)));
    }

    #[test]
    fn a_mismatch_found_on_the_key_in_use_closes_it_at_once() {
        let check = Check::new(false);
        let key = AdapterKey::named("k");
        check.gate(&key);
        check.finish(&key, Ok(report()));
        assert_eq!(check.gate(&key), Gate::Open);
        check.refuse("k", "found again");
        assert_eq!(check.gate(&key), Gate::Closed("found again".into()));
    }

    #[test]
    fn an_ffmpeg_is_known_by_where_it_is_and_by_its_size_and_time_so_a_replaced_one_is_another() {
        let dir = std::env::temp_dir().join(format!("kerf-gpu-identity-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ffmpeg");
        std::fs::write(&file, b"one").unwrap();
        let by_path = identity_of(file.to_str().unwrap(), None);
        assert!(by_path.contains("ffmpeg") && by_path.contains("(3 bytes,"), "{by_path}");
        // The same binary is the same identity, whether it is named by its path or looked for.
        let looked_for = identity_of("ffmpeg", Some(dir.as_os_str()));
        assert_eq!(looked_for, by_path);
        // Replaced in place (a package update): another size, so another key, and a verdict is not carried over to it.
        std::fs::write(&file, b"another build").unwrap();
        let replaced = identity_of("ffmpeg", Some(dir.as_os_str()));
        assert_ne!(replaced, by_path);
        assert!(replaced.contains("(13 bytes,"), "{replaced}");
        // One that is not there, or not found, is its path alone.
        assert_eq!(identity_of("no-such-ffmpeg", Some(dir.as_os_str())), "no-such-ffmpeg");
        assert_eq!(identity_of("ffmpeg", None), "ffmpeg");
        assert_eq!(
            identity_of(dir.join("gone").to_str().unwrap(), None),
            dir.join("gone").to_str().unwrap()
        );
        // The first directory that holds it wins; `.exe` is tried where programs have one.
        let other = dir.join("later");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("tool.exe"), b"x").unwrap();
        let path = std::env::join_paths([dir.as_path(), other.as_path()]).unwrap();
        assert!(identity_of("tool", Some(&path)).contains("tool.exe"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_escape_hatch_opens_the_gate_without_a_check() {
        let check = Check::new(true);
        assert_eq!(check.gate(&AdapterKey::named("anything")), Gate::Open);
        assert_eq!(check.standing(), None);
    }
}
