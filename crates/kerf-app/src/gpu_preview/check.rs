//! The GPU path draws nothing the user looks at until it has been checked against this machine's
//! FFmpeg ([`kerf_gpu::parity_self_test`]): the first frame that wants the GPU starts the check on a
//! thread of its own and goes through FFmpeg, as does every frame until the verdict, which then
//! stands for the session.
//!
//! * **Passed**: the GPU path is open.
//! * **Mismatch**: the GPU draws something else than this FFmpeg does. Closed for the session — turning
//!   the setting off and on does not try again — and the reason is what Settings shows.
//! * **Could not check** (an FFmpeg that would not make the fixtures, a colour policy that could not be
//!   measured, a deadline, the GPU failing on the check frame): closed too, but turning the setting off
//!   and on again is a retry, since what failed may not be there next time.
//!
//! The verdict belongs to an adapter and an FFmpeg ([`AdapterKey`]): a backend rebuilt on the same
//! device after a loss keeps it, one rebuilt on another adapter is checked again.
//! `KERF_GPU_SELFTEST=off` skips the check (a developer's escape hatch; it is logged).

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
                kerf_core::ffmpeg_path()
            )
            .into(),
        )
    }

    #[cfg(test)]
    pub fn named(name: &str) -> Self {
        Self(name.into())
    }
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
    Failed { reason: String, session: bool },
}

#[derive(Debug)]
struct Slot {
    key: Option<AdapterKey>,
    phase: Phase,
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
            *slot = Slot {
                key: Some(key.clone()),
                phase: Phase::Idle,
            };
        }
        match &slot.phase {
            Phase::Passed(_) => Gate::Open,
            Phase::Running => Gate::Wait(CHECKING.into()),
            Phase::Failed { reason, .. } => Gate::Closed(reason.clone()),
            Phase::Idle => {
                slot.phase = Phase::Running;
                Gate::Begin(CHECKING.into())
            }
        }
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
                Verdict::Passed(summary)
            }
            Err(error) => {
                let session = matches!(error, SelfTestError::Mismatch(_));
                let reason = if session {
                    format!("{error} — the GPU preview is off for this session")
                } else {
                    format!("{error} — the GPU preview stays off until it is turned off and on again")
                };
                slot.phase = Phase::Failed {
                    reason: reason.clone(),
                    session,
                };
                Verdict::Failed(reason)
            }
        }
    }

    /// The check could not even be started (a thread that would not run): the same as a check that
    /// could not be made.
    pub fn failed_to_start(&self, key: &AdapterKey, why: &str) -> Verdict {
        self.finish(key, Err(SelfTestError::Inconclusive(why.to_string())))
    }

    /// The setting was turned on: a check that could not be made is tried again; a mismatch and a
    /// pass stand.
    pub fn retry(&self) {
        let mut slot = self.slot();
        if matches!(slot.phase, Phase::Failed { session: false, .. }) {
            slot.phase = Phase::Idle;
        }
    }

    /// The summary of the passed check, if there is one, or the reason it is closed.
    pub fn standing(&self) -> Option<String> {
        match &self.slot().phase {
            Phase::Passed(summary) => Some(format!("passed: {summary}")),
            Phase::Failed { reason, .. } => Some(format!("closed: {reason}")),
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
    fn a_mismatch_closes_the_gate_for_the_session_even_through_a_retry() {
        let check = Check::new(false);
        let key = AdapterKey::named("a gpu");
        check.gate(&key);
        let Verdict::Failed(reason) = check.finish(&key, Err(mismatch())) else {
            panic!("failed")
        };
        assert!(reason.contains("off for this session"), "{reason}");
        assert_eq!(check.gate(&key), Gate::Closed(reason.clone()));
        check.retry();
        assert_eq!(
            check.gate(&key),
            Gate::Closed(reason),
            "turning it off and on does not argue with a mismatch"
        );
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
    fn the_escape_hatch_opens_the_gate_without_a_check() {
        let check = Check::new(true);
        assert_eq!(check.gate(&AdapterKey::named("anything")), Gate::Open);
        assert_eq!(check.standing(), None);
    }
}
