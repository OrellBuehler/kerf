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
    /// The live timeline as the batch's edits left it, parsed, when one has made it.
    held: Option<Held>,
}

/// The live timeline a batch's edits have made, and whether the stored blob still holds the one before them.
#[derive(Debug)]
pub(super) struct Held {
    timeline: Timeline,
    unwritten: bool,
}

impl Project {
    /// Run several edits as **one**: `f` makes them through the ordinary edit methods — each with its
    /// own ripple, linked-clip sync lock, locks and checks, each seeing what the ones before it did —
    /// and they land as a single revision (an agent's with review on: a single staged step, which
    /// [`Project::undo_staged`] takes back whole), labelled `label` or after the edits. All or
    /// nothing: when `f` fails, nothing it did is kept, a proposal it opened included. `dry_run`
    /// runs the edits the same way and then keeps nothing, so `before` / `after` say what they would
    /// do. A batch does not nest.
    ///
    /// The live edits of a batch each start from the timeline the one before it left, **held parsed**
    /// ([`Project::hold_timeline`]), and the stored blob is written once, when the batch ends (or the
    /// moment anything reads it): a batch of many edits costs what they do, not a read, parse, check,
    /// serialization and rewrite of the whole cut for each. Every read of the live timeline inside a
    /// batch ([`Project::timeline`], [`Project::working_timeline`], the stored blob) sees the edits so far.
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
        if !dry_run {
            self.flush_held_timeline()?;
        }
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

    /// Whether a batch is open.
    pub(super) fn in_batch(&self) -> bool {
        self.batch.borrow().is_some()
    }

    /// A copy of the live timeline the batch's edits have left, `None` before the first one.
    pub(super) fn held_timeline(&self) -> Option<Timeline> {
        self.batch.borrow().as_ref()?.held.as_ref().map(|h| h.timeline.clone())
    }

    /// Whether that timeline has a linked clip, without copying it.
    pub(super) fn held_has_links(&self) -> Option<bool> {
        self.batch.borrow().as_ref()?.held.as_ref().map(|h| h.timeline.has_links())
    }

    /// Keep `timeline`, an edit's result, as the live one, to be written when the batch ends (or when anything reads
    /// the stored blob).
    pub(super) fn hold_timeline(&self, timeline: Timeline) {
        if let Some(steps) = self.batch.borrow_mut().as_mut() {
            steps.held = Some(Held {
                timeline,
                unwritten: true,
            });
        }
    }

    /// Forget the held timeline: something wrote the stored one, which is now the live one.
    pub(super) fn drop_held_timeline(&self) {
        if let Some(steps) = self.batch.borrow_mut().as_mut() {
            steps.held = None;
        }
    }

    /// Write the held timeline to the stored blob if the batch's edits have changed it since.
    pub(super) fn flush_held_timeline(&self) -> Result<()> {
        let json = match self.batch.borrow().as_ref().and_then(|s| s.held.as_ref()) {
            Some(held) if held.unwritten => timeline_to_json(&held.timeline)?,
            _ => return Ok(()),
        };
        self.write_timeline_str(&json)?;
        if let Some(held) = self.batch.borrow_mut().as_mut().and_then(|s| s.held.as_mut()) {
            held.unwritten = false;
        }
        Ok(())
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

    fn clip_mut(t: &mut Timeline, id: Uuid) -> &mut Clip {
        t.tracks.iter_mut().flat_map(|t| &mut t.clips).find(|c| c.id == id).unwrap()
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

    #[test]
    fn a_failing_batch_keeps_the_redo_branch_that_was_there() {
        let (p, _, c1, c2) = cut();
        p.set_volume(c1, 0.5).unwrap();
        p.undo().unwrap();
        let (live, revs) = (snap(&p), revisions(&p));
        p.batch(None, false, |p| -> Result<()> {
            p.set_volume(c2, 0.25)?;
            p.move_clip(Uuid::new_v4(), 1.0, None).map(|_| ())
        })
        .unwrap_err();
        assert_eq!((snap(&p), revisions(&p)), (live, revs), "nothing written, the branch not cut");
        assert_eq!(
            p.redo().unwrap().clip(c1).unwrap().volume,
            0.5,
            "and the redo still goes forward"
        );
    }

    #[test]
    fn a_dry_run_keeps_the_redo_branch_too() {
        let (p, _, c1, c2) = cut();
        p.set_volume(c1, 0.5).unwrap();
        p.undo().unwrap();
        let done = p.batch(None, true, |p| p.set_volume(c2, 0.25)).unwrap();
        assert!(!done.diff().is_empty());
        assert_eq!(p.redo().unwrap().clip(c1).unwrap().volume, 0.5);
        assert_eq!(p.working_timeline().unwrap().clip(c2).unwrap().volume, 1.0);
    }

    // The batch holds the timeline its edits made, parsed, and writes it once at the end (`Project::hold_timeline`).

    #[test]
    fn what_the_edits_so_far_made_is_read_inside_the_batch_whatever_the_reader() {
        let (p, _, c1, c2) = cut();
        let a1 = p.detach_audio(c1).unwrap().clip.id;
        p.batch(None, false, |p| -> Result<()> {
            p.set_volume(c1, 0.5)?;
            p.move_clip(c2, 15.0, None)?;
            for read in [p.timeline()?, p.working_timeline()?] {
                assert_eq!(
                    (read.clip(c1).unwrap().volume, read.clip(c2).unwrap().timeline_start),
                    (0.5, 15.0)
                );
            }
            let stored: Timeline = serde_json::from_str(&p.timeline_json()?)?;
            assert_eq!(
                stored.clip(c2).unwrap().timeline_start,
                15.0,
                "a read of the stored blob writes it first"
            );
            assert!(
                p.working_has_links()?,
                "c1 and its sound are linked: read from the held timeline"
            );
            p.unlink_clips(&[c1, a1])?;
            assert!(!p.working_has_links()?, "and the answer follows the edits");
            // A write that is not an edit of the batch replaces what is held.
            let mut other = p.timeline()?;
            clip_mut(&mut other, c2).volume = 0.125;
            p.save_timeline(&other)?;
            assert_eq!(p.timeline()?.clip(c2).unwrap().volume, 0.125);
            p.set_volume(c1, 0.75)?;
            assert_eq!(p.timeline()?.clip(c2).unwrap().volume, 0.125, "later edits start from it");
            Ok(())
        })
        .unwrap();
        let t = p.timeline().unwrap();
        assert_eq!((t.clip(c1).unwrap().volume, t.clip(c2).unwrap().volume), (0.75, 0.125));
    }

    #[test]
    fn a_batch_stores_what_the_same_edits_one_by_one_store_and_writes_it_once() {
        let (p, _, c1, c2) = cut();
        let a1 = p.detach_audio(c1).unwrap().clip.id;
        let start = p.timeline().unwrap();
        let edits = |p: &Project| -> Result<()> {
            p.with_ripple(Some(true), |p| p.trim(c1, Some(4.0), None, None))?;
            p.move_clip(c1, 30.0, None)?;
            p.set_volume(c2, 0.25)?;
            p.set_fade(a1, Some(0.5), None)?;
            p.with_links(Some(false), |p| p.set_volume(c1, 0.75))?;
            Ok(())
        };
        let changes = |p: &Project| p.conn.total_changes();

        let before = changes(&p);
        p.batch(Some("five edits"), false, edits).unwrap();
        let batched = (snap(&p), changes(&p) - before);
        assert_eq!(p.history().unwrap().last().unwrap().label, "five edits");

        p.save_timeline(&start).unwrap();
        let before = changes(&p);
        edits(&p).unwrap();
        let single = (snap(&p), changes(&p) - before);
        assert_eq!(batched.0, single.0, "the same stored timeline");

        p.save_timeline(&start).unwrap();
        let before = changes(&p);
        p.batch(None, false, |p| p.set_volume(c2, 0.5)).unwrap();
        let one_edit = changes(&p) - before;
        assert_eq!(
            batched.1, one_edit,
            "five edits write what one does: the timeline once, one revision"
        );
        assert!(single.1 > batched.1);
    }

    #[test]
    fn an_edit_that_makes_a_number_json_cannot_say_is_the_one_refused() {
        let (p, _, c1, c2) = cut();
        let (live, revs) = (snap(&p), revisions(&p));
        let mut reached = Vec::new();
        let err = p
            .batch(None, false, |p| -> Result<()> {
                p.set_volume(c1, 0.5)?;
                reached.push("volume");
                p.edit_timeline("break", |t| {
                    clip_mut(t, c2).color.brightness = f64::NAN;
                    Ok(())
                })?;
                reached.push("never");
                Ok(())
            })
            .unwrap_err();
        assert!(err.to_string().contains("is not a finite number"), "{err}");
        assert_eq!(reached, ["volume"], "refused by the edit, not at the end of the batch");
        assert_eq!((snap(&p), revisions(&p)), (live, revs));
    }
}
