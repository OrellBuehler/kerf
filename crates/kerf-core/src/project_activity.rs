//! What changed in an open project, for whoever has to tell someone outside the engine (the MCP
//! server's live activity stream) without asking under the project lock.
//!
//! Every row a project's connection writes marks the area its table belongs to (SQLite's update
//! hook, the one [`crate::plan_epoch`] rides, with the same blind spots: a `DELETE` without a
//! `WHERE`, the row an `INSERT OR REPLACE` replaces), and a project replaced by another marks every
//! area. The marks gather in one process-wide set until [`take`] empties it; the [`set_listener`]
//! callback runs when an area goes from clean to marked, at most once per area between two takes,
//! so a write that touches thousands of rows costs one call. It runs on the writing thread, under
//! the project lock: it must only wake something up, never do the work. A write that rolled back
//! still marks: the answer errs towards "something changed".

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

/// The cut: the timeline and its history.
pub const TIMELINE: u32 = 1;
/// An agent's staged proposal, and its undo steps.
pub const PROPOSAL: u32 = 1 << 1;
/// The task queue.
pub const TASKS: u32 = 1 << 2;
/// The media and what analysis found in it.
pub const ASSETS: u32 = 1 << 3;
/// The project itself: its settings, its fonts, or another project opened in its place.
pub const PROJECT: u32 = 1 << 4;
/// Every area.
pub const ALL: u32 = TIMELINE | PROPOSAL | TASKS | ASSETS | PROJECT;

static PENDING: AtomicU32 = AtomicU32::new(0);
static LISTENER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Who to wake when an area is newly marked. One per process; a second is refused (`false`).
///
/// A project is opened before anyone listens, so areas are marked by then and would never be newly
/// marked again until someone took them: a listener set while marks are pending is woken at once.
pub fn set_listener(listener: impl Fn() + Send + Sync + 'static) -> bool {
    if LISTENER.set(Box::new(listener)).is_err() {
        return false;
    }
    if PENDING.load(Ordering::Acquire) != 0 {
        if let Some(listener) = LISTENER.get() {
            listener();
        }
    }
    true
}

/// The areas marked since the last take, and the set emptied.
pub fn take() -> u32 {
    PENDING.swap(0, Ordering::AcqRel)
}

/// The names of the areas in `areas`, in a fixed order (what a client is told).
pub fn names(areas: u32) -> Vec<&'static str> {
    [
        (TIMELINE, "timeline"),
        (PROPOSAL, "proposal"),
        (TASKS, "tasks"),
        (ASSETS, "assets"),
        (PROJECT, "project"),
    ]
    .into_iter()
    .filter(|(bit, _)| areas & bit != 0)
    .map(|(_, name)| name)
    .collect()
}

pub(crate) fn mark(areas: u32) {
    let before = PENDING.fetch_or(areas, Ordering::AcqRel);
    if before & areas != areas {
        if let Some(listener) = LISTENER.get() {
            listener();
        }
    }
}

/// The area a write to `table` changes.
pub(crate) fn table_area(table: &str) -> u32 {
    match table {
        "timeline" | "history" => TIMELINE,
        "staged" | "staged_undo" => PROPOSAL,
        "tasks" => TASKS,
        "assets" | "analysis" => ASSETS,
        _ => PROJECT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held by the tests that take the marks: the other tests of the process only ever add some.
    static TAKING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn taking() -> std::sync::MutexGuard<'static, ()> {
        TAKING.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn each_table_belongs_to_an_area_and_the_rest_are_the_projects() {
        assert_eq!(table_area("timeline"), TIMELINE);
        assert_eq!(table_area("history"), TIMELINE);
        assert_eq!(table_area("staged_undo"), PROPOSAL);
        assert_eq!(table_area("tasks"), TASKS);
        assert_eq!(table_area("analysis"), ASSETS);
        assert_eq!(table_area("meta"), PROJECT);
        assert_eq!(names(TASKS | TIMELINE), ["timeline", "tasks"]);
        assert_eq!(names(ALL).len(), 5);
    }

    /// Marks made before anyone listened (the project opened at launch) wake the listener as it is
    /// set, so a write to an area already marked is not lost: the listener takes the stale marks and
    /// the next write marks again. The listener is the process's one, so only this test sets it.
    #[test]
    fn a_listener_set_late_is_woken_for_the_marks_already_made() {
        static WOKEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let _taking = taking();
        mark(TIMELINE | PROJECT);
        assert!(set_listener(|| {
            WOKEN.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(WOKEN.load(Ordering::SeqCst) >= 1, "woken for what was pending");
        assert!(!set_listener(|| {}), "one listener per process");
        let before = WOKEN.load(Ordering::SeqCst);
        take();
        mark(TIMELINE);
        assert!(WOKEN.load(Ordering::SeqCst) > before, "a mark after the take wakes it again");
    }

    /// An edit, a task and a proposal each mark their area. The marks are the process's, so other
    /// tests may add theirs and take them: only what this one did is asserted, read after each write.
    #[test]
    fn writes_mark_the_areas_they_change() {
        let _taking = taking();
        let project = crate::Project::open_in_memory().unwrap();
        let areas = std::cell::Cell::new(0u32);
        let wrote = |f: &dyn Fn()| {
            take();
            f();
            areas.set(areas.get() | take());
        };
        wrote(&|| {
            project.add_task("trim the intro").unwrap();
        });
        assert_ne!(areas.get() & TASKS, 0, "{:?}", names(areas.get()));
        wrote(&|| {
            project.save_timeline(&crate::model::Timeline::default()).unwrap();
        });
        assert_ne!(areas.get() & TIMELINE, 0, "{:?}", names(areas.get()));
    }
}
