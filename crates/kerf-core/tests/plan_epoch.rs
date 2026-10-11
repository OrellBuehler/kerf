//! The plan epoch against a frame request that interleaves with the replacing of the open project.
//! Its own test binary with one test, so no other test moves the one process-wide epoch meanwhile.
//!
//! This holds that replacing a project moves the epoch past what a frame tagged its old inputs with.
//! It cannot tell a bump made before the swap inside `replace_with` from one made after it: that the
//! bump comes **after** (so a reader that locks later sees the new project under the new epoch) is
//! the ordering `Project::replace_with`'s documentation states, and the test does not enforce it.

use kerf_core::{plan_epoch, Project};

#[test]
fn a_frame_that_took_its_inputs_between_opening_a_project_and_swapping_it_in_is_stale() {
    let mut shared = Project::open_in_memory().expect("the project in use");
    // Opening the new project moves the epoch (before the swap).
    let opened = Project::open_in_memory().expect("the project being opened");
    // A frame request takes the lock now and tags the OLD project's inputs with the epoch it reads.
    let tag = plan_epoch();
    // The swap, under the same lock.
    shared.replace_with(opened);
    assert!(
        plan_epoch() > tag,
        "the snapshot of the old project is still tagged fresh after the new one is in"
    );
}
