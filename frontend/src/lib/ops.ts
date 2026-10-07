// Running an edit from a click: the backend can refuse (a stale id, a value out
// of range, no clip under the playhead), and a button has nowhere to put that
// but a notice.

import { editor } from './state.svelte';
import { toast } from './notifications.svelte';
import { cutNotice, lockedNotice, removalNotice } from './removal';

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
