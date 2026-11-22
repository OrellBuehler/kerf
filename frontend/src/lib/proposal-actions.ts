// What the buttons of the agent's proposal do — the Agent panel's card, the preview banner, the strip
// over the timeline. One place, so every one of them asks the same questions (a stale cut, a proposal
// worth a second thought) and says the same thing afterwards.

import { agent } from './agent.svelte';
import { confirmAction } from './api';
import { CONFIRM, discardProposalCopy, staleApplyCopy } from './confirm-copy';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { errorMessage, holdForPreview } from './ops';
import { discardNeedsConfirm, reviewTarget } from './proposal-ui';
import { subsetTone } from './review-picks';
import { changePoints, stepChange } from './staged-diff';
import { editor } from './state.svelte';
import type { DiffEntry } from './types';

/** How long the toast that offers the way back (Undo an apply, Restore a discard) stays: longer than a plain notice, because
 *  what it undoes is minutes of an agent's work and the press that did it was one click. */
const WAY_BACK_MS = 9000;

/** How many edits the user has made since the agent staged the open proposal: the revision the cut is at less the one the
 *  proposal started from (the revisions are numbered one after another). Null when the history does not say which is current. */
export function editsSinceStaged(): number | null {
	const staged = editor.staged;
	const head = editor.history.find((r) => r.current)?.seq;
	return staged && head !== undefined ? head - staged.base_seq : null;
}

/** The staleness prompt, asked before any apply of a cut the user has edited since. */
async function confirmStale(): Promise<boolean> {
	const copy = staleApplyCopy(editsSinceStaged());
	return confirmAction(copy.message, copy.title, copy);
}

/** Accept the proposal. A cut edited since it was staged asks first (applying replaces it). Resolves to
 *  whether it went in; a refusal is reported and leaves the preview as it was. */
export async function applyProposal(): Promise<boolean> {
	const staged = editor.staged;
	if (!staged) return false;
	if (staged.stale && !(await confirmStale())) return false;
	try {
		await editor.applyStaged(staged.stale);
		// One revision, so one Undo takes all of it back — the way back from a press that was a click too soon.
		toast.success('Applied the proposed changes', { action: { label: 'Undo', onClick: () => void editor.undo() }, duration: WAY_BACK_MS });
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
	if (staged.stale && !(await confirmStale())) return false;
	try {
		const done = await editor.applyStagedSubset(entryIds, staged.stale);
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
 *  proposal when none is ticked. The card and the bar under the timeline are one answer, so a pick made in one is applied from the other. */
export async function applyReview(): Promise<boolean> {
	const picked = [...agent.picks];
	if (picked.length === 0) return applyProposal();
	const done = await applyPicked(picked);
	if (done) agent.clearPicks();
	return done;
}

/** The way out of a stale proposal that keeps the user's work: go back to the revision the agent started from, so the
 *  proposal fits again. Nothing is deleted — the edits made since stay in History as the redo branch until a new edit is made.
 *  Held while the proposal is previewed (the cut on screen is not the user's). */
export async function returnToProposalBase(): Promise<boolean> {
	const staged = editor.staged;
	if (!staged || holdForPreview()) return false;
	const from = editor.history.find((r) => r.current)?.seq;
	try {
		await editor.revertTo(staged.base_seq);
		toast.info('Went back to the cut the proposal started from', {
			action: from === undefined ? undefined : { label: 'Undo', onClick: () => void editor.revertTo(from).catch((e) => toast.error(errorMessage(e))) },
			duration: WAY_BACK_MS
		});
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}

/** `[` / `]` and the preview bar's arrows: the playhead to the change before / after it, while the proposal is on the timeline.
 *  Returns false when there is nothing to step through (the caller leaves the key unhandled). */
export function stepProposal(dir: 1 | -1): void | false {
	if (!editor.previewingStaged || !editor.staged) return false;
	const at = stepChange(changePoints(editor.staged.diff.entries), ui.time, dir, editor.fps, editor.duration);
	if (at === null) toast.info(dir > 0 ? 'That is the last change in the proposal' : 'That is the first change in the proposal');
	else ui.seek(at);
}
