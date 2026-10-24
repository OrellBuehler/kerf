//! Several edits as one: [`Project::batch`] runs them through the ordinary edit methods inside one
//! transaction and records them as a single revision, or a single staged step.

use super::*;
use crate::model::TimelineDiff;

/// What [`Project::batch`] did: the closure's answer and the working timeline before and after it.
#[derive(Debug, Clone)]
pub struct Batch<R> {
    pub result: R,
    pub before: Timeline,
    pub after: Timeline,
    /// The label of the one step the edits were recorded as; `None` for a dry run or a batch that
    /// made no edit.
    pub label: Option<String>,
}

impl<R> Batch<R> {
    /// What the batch changed (or, for a dry run, would change) about the working timeline.
    pub fn diff(&self) -> TimelineDiff {
        self.before.diff(&self.after)
    }
}

/// The edits made so far in a batch: their labels and trimmed sounds, and where they landed.
#[derive(Debug, Default)]
pub(super) struct BatchSteps {
    labels: Vec<String>,
    notes: Vec<String>,
    live: bool,
    staged: bool,
}

impl Project {
    /// Run several edits as **one**: `f` makes them through the ordinary edit methods — each with its
    /// own ripple, linked-clip sync lock, locks and checks, each seeing what the ones before it did —
    /// and they land as a single revision (an agent's with review on: a single staged step, which
    /// [`Project::undo_staged`] takes back whole), labelled `label` or after the edits. All or
    /// nothing: when `f` fails, nothing it did is kept, a proposal it opened included. `dry_run`
    /// runs the edits the same way and then keeps nothing, so `before` / `after` say what they would
    /// do. A batch does not nest.
    pub fn batch<R, E: From<Error>>(
        &self,
        label: Option<&str>,
        dry_run: bool,
        f: impl FnOnce(&Project) -> std::result::Result<R, E>,
    ) -> std::result::Result<Batch<R>, E> {
        if self.batch.borrow().is_some() {
            return Err(Error::InvalidArgument("a batch of edits cannot run inside another".to_string()).into());
        }
        let tx = self.tx()?;
        let before = self.working_timeline()?;
        struct End<'a>(&'a RefCell<Option<BatchSteps>>);
        impl Drop for End<'_> {
            fn drop(&mut self) {
                self.0.take();
            }
        }
        *self.batch.borrow_mut() = Some(BatchSteps::default());
        let end = End(&self.batch);
        let result = f(self)?;
        let after = self.working_timeline()?;
        let steps = self.batch.take().unwrap_or_default();
        drop(end);
        if dry_run {
            return Ok(Batch {
                result,
                before,
                after,
                label: None,
            });
        }
        let label = self.record_batch(steps, label)?;
        tx.commit()?;
        Ok(Batch {
            result,
            before,
            after,
            label,
        })
    }

    /// Record a finished batch as one step where its edits landed.
    fn record_batch(&self, steps: BatchSteps, label: Option<&str>) -> Result<Option<String>> {
        if steps.labels.is_empty() {
            return Ok(None);
        }
        let named = label.map(str::trim).filter(|l| !l.is_empty());
        let label = format!(
            "{}{}",
            named.map_or_else(|| batch_label(&steps.labels), str::to_string),
            trimmed_suffix(&steps.notes)
        );
        if steps.live {
            self.record_revision(&label, self.actor, &self.timeline_json()?)?;
        }
        if steps.staged {
            let mut edits = self.staged_row()?.ok_or(Error::NoStagedEdit)?.edits;
            edits.pop();
            edits.push(label.clone());
            self.conn.execute(
                "UPDATE staged SET edits = ?1 WHERE id = 1",
                params![serde_json::to_string(&edits)?],
            )?;
        }
        Ok(Some(label))
    }

    /// Note an edit made inside a batch: `None` outside one (the edit records its own step), else
    /// whether it is the batch's first edit to land there (staged or live) — the one that takes the
    /// step the rest join.
    pub(super) fn note_batch_step(&self, label: &str, notes: &[String], staged: bool) -> Option<bool> {
        let mut batch = self.batch.borrow_mut();
        let steps = batch.as_mut()?;
        steps.labels.push(label.to_string());
        steps.notes.extend_from_slice(notes);
        let landed = if staged { &mut steps.staged } else { &mut steps.live };
        Some(!std::mem::replace(landed, true))
    }

    /// A later edit of a batch joins the staged step its first one took: the proposal moves on,
    /// with no undo entry and no label of its own.
    pub(super) fn continue_staged_step(&self, timeline: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE staged SET timeline = ?1, updated_at = ?2 WHERE id = 1",
            params![timeline, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}

/// A batch's label when it was given none: its one edit's, else the count and the kinds of edit.
fn batch_label(labels: &[String]) -> String {
    if let [one] = labels {
        return one.clone();
    }
    let mut kinds: Vec<&str> = Vec::new();
    for label in labels {
        if !kinds.contains(&label.as_str()) {
            kinds.push(label);
        }
    }
    let more = if kinds.len() > 3 { ", …" } else { "" };
    format!("{} edits: {}{more}", labels.len(), kinds[..kinds.len().min(3)].join(", "))
}

#[cfg(test)]
mod tests {
    use super::super::linked_tests::{asset, stream};
    use super::*;
    use crate::model::EditSource;

    /// Two shots on V1 — `c1` 0..10 at 0 and `c2` 20..26 at 10 — made by the user.
    fn cut() -> (Project, Asset, Uuid, Uuid) {
        let p = Project::open_in_memory().unwrap();
        let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
        p.insert_asset(&a).unwrap();
        let c1 = p.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap().id;
        let c2 = p.add_clip_to_timeline(a.id, None, 20.0, 26.0, Some(10.0)).unwrap().id;
        (p, a, c1, c2)
    }

    fn snap(p: &Project) -> String {
        p.timeline_json().unwrap()
    }

    fn revisions(p: &Project) -> usize {
        p.history().unwrap().len()
    }

    fn start(p: &Project, id: Uuid) -> f64 {
        p.working_timeline().unwrap().clip(id).unwrap().timeline_start
    }

    #[test]
    fn several_edits_land_as_one_revision_that_undoes_as_one() {
        let (p, _, c1, c2) = cut();
        let (before, revs) = (snap(&p), revisions(&p));
        let done = p
            .batch(None, false, |p| -> Result<()> {
                p.set_volume(c1, 0.5)?;
                p.move_clip(c2, 15.0, None)?;
                p.set_volume(c2, 0.25)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(revisions(&p), revs + 1);
        let label = p.history().unwrap().last().unwrap().label.clone();
        assert_eq!(done.label.as_deref(), Some(label.as_str()));
        assert!(label.starts_with("3 edits: "), "{label}");
        assert_eq!(start(&p, c2), 15.0);
        assert_eq!(done.diff().entries.len(), 3);
        p.undo().unwrap();
        assert_eq!(snap(&p), before);
    }

    #[test]
    fn a_failing_edit_keeps_nothing_the_batch_did() {
        let (p, _, c1, c2) = cut();
        let (before, revs) = (snap(&p), revisions(&p));
        let err = p
            .batch(Some("plan"), false, |p| -> Result<()> {
                p.set_volume(c1, 0.5)?;
                p.split_at(c2, 12.0)?;
                p.move_clip(Uuid::new_v4(), 1.0, None)?;
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(err, Error::ClipNotFound(_)), "{err}");
        assert_eq!(snap(&p), before);
        assert_eq!(revisions(&p), revs);
        // The project is usable afterwards, and the next edit is an ordinary revision.
        p.set_volume(c1, 0.7).unwrap();
        assert_eq!(revisions(&p), revs + 1);
    }

    #[test]
    fn an_agent_batch_is_one_staged_step_and_a_failed_one_opens_no_proposal() {
        let (mut p, _, c1, c2) = cut();
        p.set_actor(EditSource::Agent);
        let live = snap(&p);
        assert!(p
            .batch(None, false, |p| -> Result<()> {
                p.set_volume(c1, 0.5)?;
                p.remove(Uuid::new_v4())
            })
            .is_err());
        assert!(p.staged().unwrap().is_none());

        p.set_fade(c1, Some(1.0), None).unwrap();
        p.batch(Some("Tighten the open"), false, |p| -> Result<()> {
            p.trim(c1, Some(2.0), None, None)?;
            p.move_clip(c2, 14.0, None)?;
            Ok(())
        })
        .unwrap();
        let staged = p.staged().unwrap().unwrap();
        assert_eq!(staged.edits.len(), 2);
        assert_eq!(staged.edits[1], "Tighten the open");
        assert_eq!(staged.undo_depth, 2);
        assert_eq!(snap(&p), live, "the user's cut does not move");
        assert_eq!(start(&p, c2), 14.0);

        let (undone, staged) = p.undo_staged().unwrap();
        assert_eq!(undone, "Tighten the open");
        assert_eq!(staged.edits.len(), 1);
        assert_eq!(start(&p, c2), 10.0);
        assert_eq!(p.working_timeline().unwrap().clip(c1).unwrap().source_in, 0.0);
    }

    #[test]
    fn a_dry_run_says_what_would_change_and_leaves_no_trace() {
        let (mut p, _, c1, c2) = cut();
        p.set_actor(EditSource::Agent);
        let (live, revs) = (snap(&p), revisions(&p));
        let done = p
            .batch(None, true, |p| -> Result<usize> {
                p.remove(c1)?;
                p.move_clip(c2, 0.0, None)?;
                Ok(2)
            })
            .unwrap();
        assert_eq!(done.result, 2);
        assert_eq!(done.label, None);
        assert_eq!(start_in(&done.after, c2), 0.0);
        assert!(!done.diff().is_empty());
        assert!(
            p.staged().unwrap().is_none(),
            "review on: the proposal the edits opened is gone too"
        );
        assert_eq!(snap(&p), live);
        assert_eq!(revisions(&p), revs);
    }

    fn start_in(t: &Timeline, id: Uuid) -> f64 {
        t.clip(id).unwrap().timeline_start
    }

    #[test]
    fn each_edit_still_ripples_and_carries_its_linked_partner() {
        let (p, _, c1, c2) = cut();
        let a1 = p.detach_audio(c1).unwrap().clip.id;
        p.batch(None, false, |p| -> Result<()> {
            // Rippled: c1 loses 4 s, so c2 moves up behind it.
            p.with_ripple(Some(true), |p| p.trim(c1, Some(4.0), None, None))?;
            assert_eq!(start(p, c2), 6.0);
            // Linked: moving the picture takes its detached sound along.
            p.move_clip(c1, 30.0, None)?;
            Ok(())
        })
        .unwrap();
        let t = p.timeline().unwrap();
        assert_eq!(start_in(&t, c2), 6.0);
        assert_eq!(start_in(&t, c1), 30.0);
        assert_eq!(start_in(&t, a1), 30.0);
    }

    #[test]
    fn a_batch_does_not_nest() {
        let (p, _, c1, _) = cut();
        let err = p
            .batch(None, false, |p| p.batch(None, false, |p| p.set_volume(c1, 0.5)))
            .unwrap_err();
        assert!(err.to_string().contains("inside another"), "{err}");
    }
}
