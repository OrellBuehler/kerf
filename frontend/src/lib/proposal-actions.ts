// What the buttons of the agent's proposal do — the Agent panel's card, the preview banner, the strip
// over the timeline. One place, so every one of them asks the same questions (a stale cut, a proposal
// worth a second thought) and says the same thing afterwards.

import { agent } from './agent.svelte';
import { confirmAction } from './api';
import { CONFIRM, applyConfirmCopy, discardProposalCopy } from './confirm-copy';
import { needsOlderForHead, undoneBranch } from './history-view';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { errorMessage, holdForPreview } from './ops';
import { discardNeedsConfirm, reviewTarget } from './proposal-ui';
import { subsetTone } from './review-picks';
import { changePoints, stepChange } from './staged-diff';
import { editor } from './state.svelte';
import { backToAction, undoAction } from './undo-guard';
import { headRef, holdsRevision } from './revision-ref';
import type { DiffEntry } from './types';

/** How long the toast that offers the way back (Undo an apply, Restore a discard) stays: longer than a plain notice, because
 *  what it undoes is minutes of an agent's work and the press that did it was one click. */
const WAY_BACK_MS = 9000;

/** How many edits the user has made since the agent staged the open proposal, when that can be stood behind (`agent.editsSince`:
 *  revision numbers are recycled after an undo and a new edit, so it is only given for a revision seen fitting and still in the
 *  history); otherwise null, and nothing says a number. */
export function editsSinceStaged(): number | null {
	return editor.staged ? agent.editsSince : null;
}

/** The most pages of older edits read to find the revision the cut is at: 20 of them is two thousand edits. */
const MAX_HEAD_PAGES = 20;

/** Read older edits until the history held contains the revision the cut is at, or there are no more to read. A cut stepped back
 *  further than the newest page reaches has no current row in it, and what is "after" the head cannot be told from that. */
export async function loadHeadOfHistory(): Promise<void> {
	for (let i = 0; i < MAX_HEAD_PAGES && needsOlderForHead(editor.history, editor.historyMore); i++) await editor.showOlderHistory();
}

/** What is asked before an apply lands, whole or partial, from the card or from the bar: the cut changed since the proposal was
 *  staged, and the edits the user stepped back from that the apply would delete (an apply is an edit; an edit drops everything
 *  after the head). Resolves to whether to go on. */
async function confirmApply(partial: boolean): Promise<boolean> {
	await loadHeadOfHistory();
	const copy = applyConfirmCopy({ stale: !!editor.staged?.stale, edits: editsSinceStaged(), partial, undone: undoneBranch(editor.history) });
	return copy ? confirmAction(copy.message, copy.title, copy) : true;
}

/** Accept the proposal. A cut edited since it was staged asks first (applying replaces it), and so does one stepped back in
 *  History (applying drops the edits after it). Resolves to whether it went in; a refusal is reported and leaves the preview as it was. */
export async function applyProposal(): Promise<boolean> {
	const staged = editor.staged;
	if (!staged) return false;
	if (!(await confirmApply(false))) return false;
	try {
		await editor.applyStaged(editor.staged?.stale ?? staged.stale);
		// One revision, so one Undo takes all of it back — while it is still the newest: the button names this revision and
		// does nothing once the cut has been edited since, instead of undoing that edit.
		toast.success('Applied the proposed changes', { action: undoAction(), duration: WAY_BACK_MS });
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}

/** Throw the proposal away; one of more than a few changes asks first. Resolves to whether it went. */
export async function discardProposal(): Promise<boolean> {
	const staged = editor.staged;
	if (!staged) return false;
	const n = staged.diff.entries.length;
	if (discardNeedsConfirm(n)) {
		const copy = discardProposalCopy(n);
		if (!(await confirmAction(copy.message, copy.title, copy))) return false;
	}
	try {
		await editor.discardStaged();
		// A discard made by mistake is one press from undone: the backend keeps the last proposal thrown away.
		toast.info('Discarded the proposed changes', { action: { label: 'Undo', onClick: () => void restoreProposal() }, duration: WAY_BACK_MS });
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}

/** Bring the proposal that was thrown away back (the toast's Undo, the Agent panel's Restore). One from before the
 *  cut was edited asks first: it comes back as a stale proposal, which only a forced apply can land. */
export async function restoreProposal(): Promise<boolean> {
	const c = CONFIRM.restoreStale;
	if (editor.discarded?.stale && !(await confirmAction(c.message, c.title, c))) return false;
	// The user bringing a proposal back is not an arrival: it is the same proposal, and they know.
	if (editor.discarded) agent.markAnnounced(editor.discarded.created_at);
	try {
		await editor.restoreDiscarded();
		toast.success('Brought the proposed changes back');
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}

/** Accept only the picked changes (`entryIds`, from `review-picks.ts`). A stale cut asks first, as for the whole
 *  proposal. Says what went in — core's own sentence, with what came along and what no longer applied — and leaves
 *  the rest of the proposal open for review. Resolves to whether anything was applied. */
export async function applyPicked(entryIds: string[]): Promise<boolean> {
	const staged = editor.staged;
	if (!staged || entryIds.length === 0) return false;
	if (!(await confirmApply(true))) return false;
	try {
		const done = await editor.applyStagedSubset(entryIds, editor.staged?.stale ?? staged.stale);
		(subsetTone(done) === 'warning' ? toast.warning : toast.success)(done.message);
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}

/** Take back the agent's latest step inside the proposal; the live cut is untouched. */
export async function undoProposalStep(): Promise<boolean> {
	try {
		const label = await editor.undoStagedStep();
		toast.info(`Took “${label}” back out of the proposal`);
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}

/** Look at one change of the proposal: put the playhead where it happens and select what it is about —
 *  opening the preview first when that only exists in the proposal. */
export async function inspectChange(entry: DiffEntry): Promise<void> {
	const step = reviewTarget(entry, { previewing: editor.previewingStaged, live: editor.liveTimeline, shown: editor.timeline });
	try {
		if (step.preview) await editor.previewStaged();
	} catch (e) {
		toast.error(errorMessage(e));
		return;
	}
	if (step.seek !== null) ui.seek(step.seek, { snap: 'up' });
	if (!step.target) return;
	const t = step.target;
	if (t.kind === 'clip' && editor.timeline.tracks.some((tr) => tr.clips.some((c) => c.id === t.id))) editor.selectClip(t.id);
	else if (t.kind === 'title' && (editor.timeline.overlays ?? []).some((o) => o.id === t.id)) editor.selectOverlay(t.id);
}

/** What Apply does wherever it is pressed: the changes ticked in the review card and what comes along with them, or the whole
 *  proposal when none is ticked — or when every change is ticked, which is the whole proposal and goes the whole way (the
 *  same apply, the same Undo), not a partial apply that merges onto the cut. The card and the bar under the timeline are one
 *  answer, so a pick made in one is applied from the other. */
export async function applyReview(): Promise<boolean> {
	const picked = [...agent.picks];
	if (picked.length === 0) return applyProposal();
	if (agent.coversAll) {
		const done = await applyProposal();
		if (done) agent.clearPicks();
		return done;
	}
	const done = await applyPicked(picked);
	if (done) agent.clearPicks();
	return done;
}

/** What a refused revert says: a revision that is no longer in the history (older than the project keeps, or its number reused)
 *  is a sentence about the cut, not the backend's `revision not found: 7`. */
export function revertRefusal(e: unknown): string {
	const m = errorMessage(e);
	return /revision not found/i.test(m)
		? 'That edit is no longer in History, so the cut cannot be taken back to it. The proposal can still be applied over your cut, or discarded.'
		: m;
}

/** The way out of a stale proposal: go back to the revision the proposal started from, so it fits again. Only for a revision that
 *  was seen fitting and is still in the history (`agent.baseRef`) — a number alone can name another revision by now. Going back
 *  deletes nothing, but what is after it stays only until the next edit, and applying the proposal is one (`applyReview` asks).
 *  The backend has the last word on whether the proposal fits (`stale`, by content): if it still does not, that is said, with
 *  the apply that goes over the cut, and no success is claimed. Held while the proposal is previewed. */
export async function returnToProposalBase(): Promise<boolean> {
	const staged = editor.staged;
	if (!staged || holdForPreview()) return false;
	const target = agent.baseRef;
	if (!target || !holdsRevision(editor.history, target)) {
		toast.info('That edit is no longer in History, so the cut cannot be taken back to it. The proposal can still be applied over your cut, or discarded.');
		return false;
	}
	const from = headRef(editor.history);
	try {
		await editor.revertTo(target.seq);
	} catch (e) {
		toast.error(revertRefusal(e));
		return false;
	}
	// `revertTo` re-reads the proposal, so `stale` is the backend's word for the cut as it is now.
	if (editor.staged?.stale) {
		toast.warning('Went back, but the cut still is not the one the proposal started from, so it still does not fit.', {
			description: 'You can apply it over your cut, or discard it.',
			action: { label: 'Apply…', onClick: () => void applyReview() },
			duration: WAY_BACK_MS
		});
		return true;
	}
	toast.info('Went back to the cut the proposal started from', {
		description: 'The edits after it are kept only until you make a new edit.',
		action: backToAction(from),
		duration: WAY_BACK_MS
	});
	return true;
}

/** The other way out: redo to the newest edit, so an apply goes over the latest cut (which Undo can take back) instead of
 *  dropping the edits the cut was stepped back from (which nothing can). */
export async function goForwardToLatest(): Promise<boolean> {
	if (holdForPreview()) return false;
	await loadHeadOfHistory();
	const newest = [...editor.history].sort((a, b) => b.seq - a.seq)[0];
	const head = headRef(editor.history);
	if (!newest || !head || newest.seq === head.seq) return false;
	try {
		await editor.revertTo(newest.seq);
	} catch (e) {
		toast.error(revertRefusal(e));
		return false;
	}
	toast.info('Went forward to your latest edit', { action: backToAction(head), duration: WAY_BACK_MS });
	return true;
}

/** `[` / `]` and the preview bar's arrows: the playhead to the change before / after it, while the proposal is on the timeline.
 *  Returns false when there is nothing to step through (the caller leaves the key unhandled). */
export function stepProposal(dir: 1 | -1): void | false {
	if (!editor.previewingStaged || !editor.staged) return false;
	const at = stepChange(changePoints(editor.staged.diff.entries), ui.time, dir, editor.fps, editor.duration);
	if (at === null) toast.info(dir > 0 ? 'That is the last change in the proposal' : 'That is the first change in the proposal');
	else ui.seek(at);
}
