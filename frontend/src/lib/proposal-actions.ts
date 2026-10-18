// What the buttons of the agent's proposal do — the Agent panel's card, the preview banner, the strip
// over the timeline. One place, so every one of them asks the same questions (a stale cut, a proposal
// worth a second thought) and says the same thing afterwards.

import { confirmAction } from './api';
import { CONFIRM, discardProposalCopy } from './confirm-copy';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { errorMessage } from './ops';
import { discardNeedsConfirm, reviewTarget } from './proposal-ui';
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
		toast.info('Discarded the proposed changes');
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
