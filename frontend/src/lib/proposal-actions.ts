// What the buttons of the agent's proposal do — the Agent panel's card, the preview banner, the strip
// over the timeline. One place, so every one of them asks the same questions (a stale cut, a proposal
// worth a second thought) and says the same thing afterwards.

import { confirmAction } from './api';
import { CONFIRM, discardProposalCopy } from './confirm-copy';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { errorMessage } from './ops';
import { discardNeedsConfirm, reviewTarget } from './proposal-ui';
import { subsetTone } from './review-picks';
import { editor } from './state.svelte';
import type { DiffEntry } from './types';

/** Accept the proposal. A cut edited since it was staged asks first (applying replaces it). Resolves to
 *  whether it went in; a refusal is reported and leaves the preview as it was. */
export async function applyProposal(): Promise<boolean> {
	const staged = editor.staged;
	if (!staged) return false;
	if (staged.stale && !(await confirmAction(CONFIRM.staleApply.message, CONFIRM.staleApply.title, CONFIRM.staleApply))) return false;
	try {
		await editor.applyStaged(staged.stale);
		toast.success('Applied the proposed changes');
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
		toast.info('Discarded the proposed changes', { action: { label: 'Undo', onClick: () => void restoreProposal() } });
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
	if (staged.stale && !(await confirmAction(CONFIRM.staleApply.message, CONFIRM.staleApply.title, CONFIRM.staleApply))) return false;
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
