// The Undo button of a toast — which acts on whatever is newest when it is pressed, an hour or a second after the notice was
// made. A toast about an apply says "Undo" and `editor.undo()` undoes the head: if the user deleted a clip in the nine seconds
// it stayed on screen, that is what it undid. These make the button name the revision the action produced, and act only
// while that revision is still the head (`revision-ref.ts`: a number alone is recycled after an undo and a new edit).

import { editor } from './state.svelte';
import { toast } from './notifications.svelte';
import { errorMessage } from './ops';
import { EDITED_SINCE, headRef, holdsRevision, undoVerdict, type RevisionRef } from './revision-ref';

export interface NoticeAction {
	label: string;
	onClick: () => void;
}

/** The history as it is now, read afresh: the head of a stale list is the one thing this must not trust. */
async function freshHistory() {
	await editor.refreshHistory();
	return editor.history;
}

/** An Undo for the edit that has just landed: it undoes that edit while it is still the newest, and otherwise says so and does
 *  nothing. Call it once the edit has completed (`editor.history` is the one after it). `undefined` when the history does not
 *  say which revision the cut is at, since then there is nothing to name. */
export function undoAction(label = 'Undo'): NoticeAction | undefined {
	const produced = headRef(editor.history);
	if (!produced) return undefined;
	return { label, onClick: () => void undoIfHead(produced) };
}

/** Undo `produced` if it is still the head. Resolves to whether anything was undone. */
export async function undoIfHead(produced: RevisionRef): Promise<boolean> {
	const verdict = undoVerdict(produced, await freshHistory());
	if (verdict !== 'undo') {
		toast.info(EDITED_SINCE);
		return false;
	}
	try {
		await editor.undo();
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}

/** An Undo for a move of the head (a revert): it puts the head back where it was (`from`) while the cut is still at the
 *  revision the move produced, and otherwise says so and does nothing. Call it after the move. */
export function backToAction(from: RevisionRef | null, label = 'Undo'): NoticeAction | undefined {
	const produced = headRef(editor.history);
	if (!from || !produced) return undefined;
	return { label, onClick: () => void revertIfHead(produced, from) };
}

export async function revertIfHead(produced: RevisionRef, from: RevisionRef): Promise<boolean> {
	const history = await freshHistory();
	if (undoVerdict(produced, history) !== 'undo' || !holdsRevision(history, from)) {
		toast.info(EDITED_SINCE);
		return false;
	}
	try {
		await editor.revertTo(from.seq);
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
}
