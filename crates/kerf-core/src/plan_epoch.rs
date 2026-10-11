//! A counter that moves whenever something a preview plan is made from may have changed, so a
//! caller that keeps the inputs of a frame (the timeline, the assets, which file each decodes)
//! can tell without taking the project lock that they are still the ones in force.
//!
//! It moves for every row written to a project database outside the tables that never reach a
//! plan (a connection's update hook, installed when a [`crate::Project`] is opened), when a
//! project is opened or created, when a preview proxy lands or is deleted, and when the proxy
//! size or the preview source changes. A bump that did not change the plan's inputs costs a caller
//! one rebuild; a change that does not bump is the one thing this must not allow, which is why
//! the rule is "everything but what is known to be irrelevant".
//!
//! A reader takes the epoch **before** it reads what it keeps, and a writer bumps **after** its
//! change is visible (a proxy after its rename), so an inputs snapshot is never newer than its tag.

use std::sync::atomic::{AtomicU64, Ordering};

static EPOCH: AtomicU64 = AtomicU64::new(1);

/// The current epoch. Compare it with the one a snapshot was tagged with.
pub fn plan_epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

/// Something a plan is made from may have changed.
pub fn bump_plan_epoch() {
    EPOCH.fetch_add(1, Ordering::AcqRel);
}

/// Whether a write to `table` can change a plan. The analysis cache, the edit history and the task
/// list are written constantly by background work and read by no plan.
pub(crate) fn table_moves_plan(table: &str) -> bool {
    !matches!(table, "analysis" | "history" | "tasks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_background_tables_do_not_move_the_epoch_and_everything_else_does() {
        for quiet in ["analysis", "history", "tasks"] {
            assert!(!table_moves_plan(quiet), "{quiet}");
        }
        for loud in ["timeline", "assets", "meta", "staged", "a_table_added_later"] {
            assert!(table_moves_plan(loud), "{loud}");
        }
    }

    #[test]
    fn a_row_written_through_a_project_moves_the_epoch_and_so_does_opening_one() {
        let before = plan_epoch();
        let project = crate::Project::open_in_memory().unwrap();
        assert!(plan_epoch() > before, "opening a project");
        let before = plan_epoch();
        project.set_meta("a-key", "a value").unwrap();
        assert!(plan_epoch() > before, "a row written");
        let before = plan_epoch();
        project.save_timeline(&crate::Timeline::new()).unwrap();
        assert!(plan_epoch() > before, "the timeline written");
    }

    #[test]
    fn a_bump_is_seen_by_the_next_read() {
        let before = plan_epoch();
        bump_plan_epoch();
        assert!(plan_epoch() > before);
    }
}
