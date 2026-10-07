// Running an edit from a click: the backend can refuse (a stale id, a value out
// of range, no clip under the playhead), and a button has nowhere to put that
// but a notice.

import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { withLinkPartners } from './link-groups';
import {
	detachedNotice,
	linkPlans,
	linkedNotice,
	reattachedNotice,
	unlinkedNotice,
	type LinkPlans
} from './link-ui';
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

// ---- linked A/V: detach, reattach, link, unlink --------------------------------------
// The clip menu and the keymap both run these, over `linkPlans` — one place that knows what
// is possible for the selection and why not, so a key and a menu line never disagree.

/** What the selection can be Detached / Reattached / Linked / Unlinked, and why not. */
export function selectionLinkPlans(): LinkPlans {
	const audible = new Set(editor.assets.filter((a) => a.streams.some((s) => s.kind === 'audio')).map((a) => a.id));
	return linkPlans(editor.timeline, (id) => audible.has(id), editor.selectedClipIds);
}

/** Undo `n` revisions — a detach or reattach across several clips is one revision each. */
async function undoTimes(n: number): Promise<void> {
	for (let i = 0; i < n; i++) await editor.undo().catch(() => {});
}

/** Run `one` on each of `ids` in turn, stopping at the first refusal. Resolves to how many
 *  went through; the refusal, when there was one, is toasted here (what was done stays done). */
async function eachClip(ids: readonly string[], one: (id: string) => Promise<unknown>): Promise<number> {
	let done = 0;
	try {
		for (const id of ids) {
			await one(id);
			done++;
		}
	} catch (e) {
		toast.error(errorMessage(e));
	}
	return done;
}

/** **Detach audio** (⇧D, the clip menu): each selected picture clip still playing its own
 *  sound hands it to a linked clip on an audio track and goes quiet, so it is heard once.
 *  One revision per clip; the toast's Undo takes them all back. */
export async function detachSelection(): Promise<void> {
	const plan = selectionLinkPlans().detach;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	const done = await eachClip(plan.ids, (id) => editor.detachAudio(id));
	if (done === 0) return;
	// The sound that was just made is part of the pictures' link group: select it with them.
	editor.selectClips(withLinkPartners(editor.timeline, [...plan.ids.slice(0, done)]), editor.selectedClipId);
	toast(detachedNotice(done), { action: { label: 'Undo', onClick: () => void undoTimes(done) } });
}

/** **Reattach audio** (⇧⌘D, the clip menu): the linked audio clip goes and the picture
 *  plays its own sound again. */
export async function reattachSelection(): Promise<void> {
	const plan = selectionLinkPlans().reattach;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	const done = await eachClip(plan.ids, (id) => editor.reattachAudio(id));
	if (done === 0) return;
	toast(reattachedNotice(done), { action: { label: 'Undo', onClick: () => void undoTimes(done) } });
}

/** **Link** the selected clips (⌘L, the clip menu): one clip per track, a move / trim / split
 *  / delete of one carried to the others. */
export async function linkSelection(): Promise<void> {
	const plan = selectionLinkPlans().link;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	try {
		await editor.linkClips(plan.ids);
	} catch (e) {
		toast.error(errorMessage(e));
		return;
	}
	toast(linkedNotice(plan.ids.length), { action: { label: 'Undo', onClick: () => void editor.undo() } });
}

/** **Unlink** the selected clips (⇧⌘L, the clip menu): each leaves its group. */
export async function unlinkSelection(): Promise<void> {
	const plan = selectionLinkPlans().unlink;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	try {
		await editor.unlinkClips(plan.ids);
	} catch (e) {
		toast.error(errorMessage(e));
		return;
	}
	toast(unlinkedNotice(plan.ids.length), { action: { label: 'Undo', onClick: () => void editor.undo() } });
}
