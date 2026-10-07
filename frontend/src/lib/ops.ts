// Running an edit from a click: the backend can refuse (a stale id, a value out
// of range, no clip under the playhead), and a button has nowhere to put that
// but a notice.

import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { cutNotice, lockedNotice, removalNotice } from './removal';
import { planPlayheadTrim, trimNotice } from './trim-tools';
import type { SplitSide } from './types';

export function errorMessage(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

/** Await `op`, turning a rejection into an error toast. Never throws. */
export async function attempt(op: () => Promise<unknown>): Promise<void> {
	try {
		await op();
	} catch (e) {
		toast.error(errorMessage(e));
	}
}

/** Delete the selected clips as one edit (one undo), with `ripple` closing the gaps
 *  behind them — the Delete / Shift+Delete keys and the clip menu. Says what it
 *  did, offers Undo, and reports a refusal instead of throwing. */
export async function deleteSelection(ripple: boolean): Promise<void> {
	try {
		const { removed, skipped } = await editor.removeSelected(ripple);
		if (removed === 0) {
			if (skipped > 0) toast.error(lockedNotice(skipped));
			return;
		}
		toast(removalNotice(removed, skipped, ripple), { action: { label: 'Undo', onClick: () => void editor.undo() } });
	} catch (e) {
		toast.error(errorMessage(e));
	}
}

/** Cut (⌘/Ctrl+X): the selected clips go to the clipboard and off the timeline as
 *  one edit. Only what the cut actually removes is copied — clips on a locked
 *  track stay (and stay selected), and if that is everything, nothing happens to
 *  the clipboard and the notice says why instead of claiming a cut. */
export async function cutSelection(): Promise<void> {
	try {
		editor.copySelection(true);
		const { removed, skipped } = await editor.removeSelected(false);
		if (removed === 0) {
			if (skipped > 0) toast.error(lockedNotice(skipped));
			return;
		}
		toast(cutNotice(removed, skipped), { action: { label: 'Undo', onClick: () => void editor.undo() } });
	} catch (e) {
		toast.error(errorMessage(e));
	}
}

/** Trim start / end to the playhead (Q / W, and the clip menu): cut every selected
 *  clip the playhead is inside at the playhead and throw away the `left` or `right`
 *  half. The backend does the cut — ONE revision for the whole selection (so a V1 clip
 *  and its A1 partner undo together), each track rippling on its own under ripple mode,
 *  the surviving halves keeping their clips' ids so the selection holds — and this
 *  decides *what* to cut (`planPlayheadTrim`) and says so when there is nothing: a
 *  keypress that does nothing silently is a key that seems broken. */
export async function trimSelection(side: SplitSide): Promise<void> {
	const selected = editor.selectedClipIds;
	const plan = planPlayheadTrim(editor.timeline, selected, ui.time, editor.fps, side);
	if (plan.trims.length === 0) {
		toast.info(trimNotice(plan, selected.length, side));
		return;
	}
	try {
		await editor.splitRemoveClips(
			plan.trims.map((t) => ({ clip_id: t.clipId, at: t.at })),
			side
		);
	} catch (e) {
		toast.error(errorMessage(e));
		return;
	}
	// Some were cut and some could not be (a locked track, a clip too short): the
	// edit that happened is visible, the one that did not needs a sentence.
	if (plan.problems.length > 0) toast.warning(trimNotice({ ...plan, trims: [] }, selected.length, side));
}
