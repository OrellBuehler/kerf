//! Task work with the user's review off and on: with it off, claiming a task opens no proposal
//! and the agent's edits land on the live cut one by one (ordinary undoable revisions attributed
//! to it), and `complete_task` / `fail_task` only settle the task; with it on, a claimed task
//! stages as it always did. And what happens to a proposal that is open when the switch flips.

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{EditSource, StreamKind};

/// Two shots on V1 made by the user; `Agent` is not the actor yet.
fn cut() -> (Project, Clip, Clip) {
    let p = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    p.insert_asset(&a).unwrap();
    let c1 = p.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = p.add_clip_to_timeline(a.id, None, 20.0, 26.0, Some(10.0)).unwrap();
    (p, c1, c2)
}

fn live_volume(p: &Project, id: Uuid) -> f32 {
    p.timeline().unwrap().clip(id).unwrap().volume
}

fn agent(p: &mut Project, session: &str) {
    p.set_actor(EditSource::Agent);
    p.declare_agent(Some(session));
}

fn user(p: &mut Project) {
    p.set_actor(EditSource::User);
}

fn revisions(p: &Project) -> Vec<Revision> {
    p.history().unwrap()
}

#[test]
fn with_review_off_claiming_opens_no_proposal_and_each_edit_lands_live_as_the_agents() {
    let (mut p, c1, c2) = cut();
    let task = p.add_task("tighten the intro").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "alpha");
    let before = revisions(&p).len();

    let claimed = p.claim_next_task(Some("alpha")).unwrap().unwrap();
    assert_eq!(claimed.id, task.id);
    assert_eq!(claimed.status, TaskStatus::Working);
    assert!(p.staged().unwrap().is_none(), "no proposal for the task");

    p.set_volume(c1.id, 0.5).unwrap();
    assert_eq!(live_volume(&p, c1.id), 0.5, "the edit is on the cut at once");
    p.set_volume(c2.id, 0.25).unwrap();
    assert_eq!(live_volume(&p, c2.id), 0.25);
    assert!(p.staged().unwrap().is_none(), "still nothing staged");
    assert_eq!(
        p.working_timeline().unwrap().clip(c1.id).unwrap().volume,
        0.5,
        "the agent's reads are the live cut"
    );

    // Each edit is its own revision, attributed to the agent.
    let history = revisions(&p);
    assert_eq!(history.len(), before + 2);
    for revision in &history[before..] {
        assert_eq!(revision.source, EditSource::Agent, "{}", revision.label);
    }

    // Completing only settles the task: nothing is waiting for the user, so it is done.
    let done = p.complete_task(task.id, Some("turned two clips down".to_string())).unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.result.as_deref(), Some("turned two clips down"));
    assert_eq!(p.require_task(task.id).unwrap().status, TaskStatus::Done);
    assert!(p.staged().unwrap().is_none());
    assert_eq!(revisions(&p).len(), before + 2, "settling a task is not an edit");
    assert_eq!(live_volume(&p, c1.id), 0.5);

    // Every step is an ordinary revision the user can step back, one at a time.
    user(&mut p);
    p.undo().unwrap();
    assert_eq!(live_volume(&p, c2.id), 1.0);
    assert_eq!(live_volume(&p, c1.id), 0.5);
    p.undo().unwrap();
    assert_eq!(live_volume(&p, c1.id), 1.0);
}

#[test]
fn with_review_off_an_agent_can_step_its_own_task_work_back() {
    let (mut p, c1, _) = cut();
    p.add_task("tighten").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.undo().unwrap();
    assert_eq!(live_volume(&p, c1.id), 1.0);
}

#[test]
fn with_review_on_a_claimed_task_still_opens_a_proposal_the_user_accepts() {
    let (mut p, c1, _) = cut();
    let task = p.add_task("tighten the intro").unwrap();
    assert!(p.agent_review(), "on by default");
    agent(&mut p, "alpha");
    let before = revisions(&p).len();

    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    let staged = p.staged().unwrap().expect("claiming opens a proposal");
    assert_eq!(staged.task_id, Some(task.id));
    assert_eq!(staged.owner.as_deref(), Some("alpha"));

    p.set_volume(c1.id, 0.5).unwrap();
    assert_eq!(live_volume(&p, c1.id), 1.0, "the live cut is untouched");
    assert_eq!(p.working_timeline().unwrap().clip(c1.id).unwrap().volume, 0.5);
    assert_eq!(revisions(&p).len(), before, "nothing in the history yet");

    // Handing it over leaves it for the user; the agent cannot accept it for them.
    let ready = p.complete_task(task.id, Some("done".to_string())).unwrap();
    assert_eq!(ready.status, TaskStatus::Ready);
    assert!(matches!(p.resolve_task(task.id), Err(Error::AgentReviewRequired)));
    assert!(p.staged().unwrap().is_some());

    user(&mut p);
    let done = p.resolve_task(task.id).unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(live_volume(&p, c1.id), 0.5);
    assert!(p.staged().unwrap().is_none());
    let history = revisions(&p);
    assert_eq!(history.len(), before + 1, "one revision for the whole task");
    assert_eq!(history.last().unwrap().source, EditSource::Agent);
}

#[test]
fn with_review_on_completing_a_task_always_leaves_it_ready() {
    // Even with nothing staged under it (the proposal was discarded, or never needed): accepting
    // is the user's switch, so the status is not the agent's to jump to done.
    let (mut p, ..) = cut();
    let task = p.add_task("look around").unwrap();
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    p.discard_staged().unwrap();
    assert!(p.staged().unwrap().is_none());
    assert_eq!(p.complete_task(task.id, None).unwrap().status, TaskStatus::Ready);
}

#[test]
fn with_review_off_failing_a_task_only_settles_it_and_its_edits_stay_undoable() {
    let (mut p, c1, _) = cut();
    let first = p.add_task("tighten the intro").unwrap();
    let second = p.add_task("caption the cut").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "alpha");

    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    let failed = p.fail_task(first.id, "could not find a good cut").unwrap();
    assert_eq!(failed.status, TaskStatus::Failed);
    assert_eq!(failed.result.as_deref(), Some("could not find a good cut"));
    assert_eq!(live_volume(&p, c1.id), 0.5, "failing a task does not roll its edits back");
    p.undo().unwrap();
    assert_eq!(
        live_volume(&p, c1.id),
        1.0,
        "…they are in the history, for whoever wants them gone"
    );

    let next = p.claim_next_task(Some("alpha")).unwrap().unwrap();
    assert_eq!(next.id, second.id, "the queue moves on");
    assert!(p.staged().unwrap().is_none());
}

#[test]
fn with_review_off_two_sessions_work_two_tasks_on_the_live_cut_without_a_proposal_between_them() {
    let (mut p, c1, c2) = cut();
    let first = p.add_task("tighten the intro").unwrap();
    let second = p.add_task("balance the levels").unwrap();
    p.set_agent_review(false);

    agent(&mut p, "alpha");
    assert_eq!(p.claim_next_task(Some("alpha")).unwrap().unwrap().id, first.id);
    agent(&mut p, "beta");
    // A proposal used to make this wait (`StagedEditPending`); the claim is atomic, so each session
    // gets a task of its own.
    assert_eq!(p.claim_next_task(Some("beta")).unwrap().unwrap().id, second.id);
    assert!(p.claim_next_task(Some("beta")).unwrap().is_none());

    agent(&mut p, "alpha");
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    // The user cuts in between: nothing goes stale, the next edit simply meets the cut as it is.
    user(&mut p);
    p.set_fade(c1.id, Some(0.5), None).unwrap();
    agent(&mut p, "beta");
    p.ensure_agent_staging(Some("beta")).unwrap();
    p.set_volume(c2.id, 0.25).unwrap();
    agent(&mut p, "alpha");
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c2.id, 0.3).unwrap();

    let live = p.timeline().unwrap();
    assert_eq!(live.clip(c1.id).unwrap().volume, 0.5);
    assert_eq!(live.clip(c1.id).unwrap().fade_in, 0.5, "the user's edit is in the cut");
    assert_eq!(
        live.clip(c2.id).unwrap().volume,
        0.3,
        "the last edit wins, as for any two editors"
    );
    assert!(p.staged().unwrap().is_none());

    for (session, task) in [("alpha", first.id), ("beta", second.id)] {
        agent(&mut p, session);
        assert_eq!(p.complete_task(task, None).unwrap().status, TaskStatus::Done);
    }
    let sources: Vec<EditSource> = revisions(&p).iter().rev().take(4).map(|r| r.source).collect();
    assert_eq!(
        sources,
        [EditSource::Agent, EditSource::Agent, EditSource::User, EditSource::Agent]
    );
}

#[test]
fn with_review_off_the_users_locks_hold_for_task_work_directly() {
    let (mut p, c1, _) = cut();
    p.add_task("tighten").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    let v1 = p.timeline().unwrap().tracks[0].id;
    user(&mut p);
    p.set_track_locked(v1, true).unwrap();

    agent(&mut p, "alpha");
    let e = p.split_at(c1.id, 5.0).unwrap_err();
    assert!(e.to_string().contains("locked"), "{e}");
    assert!(p.set_track_locked(v1, false).is_err(), "an agent never unlocks");
    assert!(p.timeline().unwrap().track(v1).unwrap().locked);
}

#[test]
fn stage_edits_still_opens_a_proposal_with_review_off_and_a_task_waits_on_it() {
    let (mut p, c1, _) = cut();
    let task = p.add_task("try a risky restructure").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();

    // The explicit opt-in: this one the user looks at first.
    p.begin_staging(None, Some("a risky restructure"), Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    assert_eq!(live_volume(&p, c1.id), 1.0, "held back");
    assert_eq!(p.working_timeline().unwrap().clip(c1.id).unwrap().volume, 0.5);

    // The task has work waiting, so it is ready, not done.
    assert_eq!(p.complete_task(task.id, None).unwrap().status, TaskStatus::Ready);
    // The agent may land its own proposal with review off, or the user may; either is explicit.
    p.apply_staged(false).unwrap();
    assert_eq!(live_volume(&p, c1.id), 0.5);
}

#[test]
fn a_task_does_not_wait_on_another_sessions_proposal() {
    let (mut p, c1, _) = cut();
    let task = p.add_task("balance the levels").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "beta");
    p.claim_next_task(Some("beta")).unwrap().unwrap();
    p.set_volume(c1.id, 0.5).unwrap();

    agent(&mut p, "alpha");
    p.begin_staging(None, None, Some("alpha")).unwrap();

    // Beta's work was live from the start; alpha's proposal is not its task's.
    agent(&mut p, "beta");
    assert_eq!(p.complete_task(task.id, None).unwrap().status, TaskStatus::Done);
    // …and while alpha's proposal is open, beta's edits are refused rather than folded into it.
    assert!(matches!(
        p.ensure_agent_staging(Some("beta")),
        Err(Error::StagedEditOwnedByOther)
    ));
}

// ---- the switch flips with a proposal open ---------------------------------------------------

#[test]
fn a_proposal_open_when_review_goes_off_is_neither_lost_nor_applied() {
    let (mut p, c1, c2) = cut();
    let first = p.add_task("tighten the intro").unwrap();
    let second = p.add_task("balance the levels").unwrap();
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    let before = revisions(&p).len();

    // The user turns review off while it waits.
    p.set_agent_review(false);
    let staged = p.staged().unwrap().expect("still open");
    assert_eq!(staged.task_id, Some(first.id));
    assert_eq!(staged.edits.len(), 1, "with what it held");
    assert_eq!(live_volume(&p, c1.id), 1.0, "and nothing was applied");
    assert_eq!(revisions(&p).len(), before);

    // It stays the agent's working copy: further edits (and its reads) go on in it, so they do
    // not split between the proposal and the live cut.
    p.set_volume(c2.id, 0.25).unwrap();
    assert_eq!(live_volume(&p, c2.id), 1.0);
    assert_eq!(p.working_timeline().unwrap().clip(c2.id).unwrap().volume, 0.25);
    assert_eq!(p.staged().unwrap().unwrap().edits.len(), 2);

    // The next task does not slip in beside it.
    assert!(matches!(p.claim_next_task(Some("alpha")), Err(Error::StagedEditPending)));
    assert_eq!(p.require_task(second.id).unwrap().status, TaskStatus::Queued);

    // Completing leaves the task for the user: its work is still in the proposal.
    let ready = p.complete_task(first.id, Some("two clips".to_string())).unwrap();
    assert_eq!(ready.status, TaskStatus::Ready);
    assert!(p.staged().unwrap().is_some());
    assert_eq!(live_volume(&p, c1.id), 1.0);

    // The user lands it as they would have with review on: one revision, the agent's.
    user(&mut p);
    assert_eq!(p.resolve_task(first.id).unwrap().status, TaskStatus::Done);
    assert_eq!(live_volume(&p, c1.id), 0.5);
    assert_eq!(live_volume(&p, c2.id), 0.25);
    assert_eq!(revisions(&p).len(), before + 1);
    assert_eq!(revisions(&p).last().unwrap().source, EditSource::Agent);

    // With it gone, the next task is worked live.
    agent(&mut p, "alpha");
    assert_eq!(p.claim_next_task(Some("alpha")).unwrap().unwrap().id, second.id);
    assert!(p.staged().unwrap().is_none());
}

#[test]
fn a_proposal_open_when_review_goes_off_can_be_discarded_by_the_user_and_nothing_lands() {
    let (mut p, c1, _) = cut();
    let task = p.add_task("tighten the intro").unwrap();
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_agent_review(false);

    user(&mut p);
    p.remove_task(task.id).unwrap();
    assert!(p.staged().unwrap().is_none());
    assert_eq!(live_volume(&p, c1.id), 1.0);
    assert!(p.discarded_staged().unwrap().is_some(), "and it can still be brought back");
}

#[test]
fn the_agent_lands_its_own_open_proposal_only_by_asking_and_a_stranger_cannot() {
    let (mut p, c1, _) = cut();
    p.add_task("tighten the intro").unwrap();
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_agent_review(false);

    agent(&mut p, "beta");
    assert!(matches!(p.apply_staged(false), Err(Error::StagedEditOwnedByOther)));
    assert_eq!(live_volume(&p, c1.id), 1.0);
    agent(&mut p, "alpha");
    p.apply_staged(false).unwrap();
    assert_eq!(live_volume(&p, c1.id), 0.5);
}

#[test]
fn turning_review_on_mid_task_holds_the_next_edit_and_leaves_the_earlier_ones_landed() {
    let (mut p, c1, c2) = cut();
    let task = p.add_task("tighten the intro").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "alpha");
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    assert!(p.staged().unwrap().is_none());

    // The user switches review on while the agent is still working.
    p.set_agent_review(true);
    p.set_volume(c2.id, 0.25).unwrap();
    assert_eq!(live_volume(&p, c1.id), 0.5, "what already landed stays");
    assert_eq!(live_volume(&p, c2.id), 1.0, "the next edit is held for review");
    let staged = p.staged().unwrap().expect("the edit opened a proposal");
    assert_eq!(staged.edits.len(), 1);
    assert!(p.undo().is_err(), "and the agent no longer steps the user's history back");

    // Handing the task over is the same as with review on from the start.
    assert_eq!(p.complete_task(task.id, None).unwrap().status, TaskStatus::Ready);
    assert!(p.staged().unwrap().is_some());
}

// ---- a session left behind ----------------------------------------------------------------

#[test]
fn with_review_off_a_claim_clears_a_session_orphaned_by_a_finished_task_and_opens_none() {
    let (mut p, c1, _) = cut();
    let first = p.add_task("tighten the intro").unwrap();
    let second = p.add_task("caption the cut").unwrap();
    p.set_agent_review(false);
    agent(&mut p, "alpha");
    // Left behind by a task that is over (a proposal opened for it, then it went done).
    p.begin_staging(Some(first.id), None, Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_task_state(first.id, TaskStatus::Done, None).unwrap();

    let claimed = p.claim_next_task(Some("alpha")).unwrap().unwrap();
    assert_eq!(claimed.id, second.id);
    assert!(
        p.staged().unwrap().is_none(),
        "the orphan is cleared and the new task opens none"
    );
    assert!(
        p.discarded_staged().unwrap().is_some(),
        "what it held is kept aside, not lost"
    );
    assert_eq!(live_volume(&p, c1.id), 1.0);
}
