//! A counter that moves whenever something a preview plan is made from may have changed, so a
//! caller that keeps the inputs of a frame (the timeline, the assets, which file each decodes)
//! can tell without taking the project lock that they are still the ones in force.
//!
//! It moves for every row a project's connection inserts, updates or deletes one by one outside the
//! tables that never reach a plan (SQLite's update hook, installed when a [`crate::Project`] is
//! opened), when a project is opened or created or replaced by another, when a preview proxy lands or
//! is deleted, and when the proxy size or the preview source changes. The hook does **not** see a
//! `DELETE` without a `WHERE`, which SQLite does by emptying the table (so such a statement says
//! `WHERE 1`), nor the row an `INSERT OR REPLACE` replaces (the insert after it is reported). A bump
//! that did not change the plan's inputs costs a caller one rebuild; a change that does not bump is
//! the one thing this must not allow, which is why the rule is "everything but what is known to be
//! irrelevant".
//!
//! A reader takes the epoch **before** it reads what it keeps, and a writer bumps **after** its
//! change is visible (a proxy after its rename), so an inputs snapshot is never newer than its tag.

use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
thread_local! {
    static BUMPS_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

static EPOCH: AtomicU64 = AtomicU64::new(1);

/// The current epoch. Compare it with the one a snapshot was tagged with.
pub fn plan_epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

/// Something a plan is made from may have changed.
pub fn bump_plan_epoch() {
    EPOCH.fetch_add(1, Ordering::AcqRel);
    #[cfg(test)]
    BUMPS_HERE.with(|n| n.set(n.get() + 1));
}

/// How many times this thread has bumped the epoch: a test's own count, which the other tests of the
/// process (all bumping the one epoch) cannot move.
#[cfg(test)]
pub(crate) fn bumps_here() -> u64 {
    BUMPS_HERE.with(std::cell::Cell::get)
}

/// Whether a write to `table` can change a plan. The analysis cache, the edit history, the task
/// list and a proposal's undo steps are written constantly and read by no plan.
pub(crate) fn table_moves_plan(table: &str) -> bool {
    !matches!(table, "analysis" | "history" | "tasks" | "staged_undo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_background_tables_do_not_move_the_epoch_and_everything_else_does() {
        for quiet in ["analysis", "history", "tasks", "staged_undo"] {
            assert!(!table_moves_plan(quiet), "{quiet}");
        }
        for loud in ["timeline", "assets", "meta", "staged", "a_table_added_later"] {
            assert!(table_moves_plan(loud), "{loud}");
        }
    }

    #[test]
    fn a_row_written_through_a_project_moves_the_epoch_and_so_does_opening_one() {
        let before = bumps_here();
        let project = crate::Project::open_in_memory().unwrap();
        assert!(bumps_here() > before, "opening a project");
        let before = bumps_here();
        project.set_meta("a-key", "a value").unwrap();
        assert!(bumps_here() > before, "a row written");
        let before = bumps_here();
        project.save_timeline(&crate::Timeline::new()).unwrap();
        assert!(bumps_here() > before, "the timeline written");
    }

    #[test]
    #[allow(clippy::print_stderr)]
    fn the_proxy_settings_and_the_deleting_of_a_proxy_move_the_epoch() {
        use crate::proxy::{preview_source_setting, proxy_size, set_preview_source_setting, set_proxy_size};
        let before = bumps_here();
        set_proxy_size(proxy_size());
        assert!(bumps_here() > before, "the proxy size");
        let before = bumps_here();
        set_preview_source_setting(preview_source_setting());
        assert!(bumps_here() > before, "the preview source");
        let source = std::path::Path::new("/nonexistent/kerf-plan-epoch-test.mp4");
        if crate::engine::proxy_path(source, 640).is_none() {
            eprintln!("skipped: this platform has no cache directory, so there is no proxy to delete");
            return;
        }
        let before = bumps_here();
        assert_eq!(crate::engine::remove_proxy_files(source, 640), 0);
        assert!(bumps_here() > before, "a proxy deleted (or looked for)");
    }

    #[test]
    fn a_bump_is_seen_by_the_next_read() {
        let before = plan_epoch();
        let here = bumps_here();
        bump_plan_epoch();
        assert!(plan_epoch() > before);
        assert_eq!(bumps_here(), here + 1);
    }
}
