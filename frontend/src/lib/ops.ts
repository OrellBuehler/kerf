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
import { keyEditDone, planKeyEdit, planPlayheadSplit, splitDone, splitNotice, type KeyTool } from './key-edits';
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

/** **Split at playhead** (the key, Edit › Split at playhead, the clip menu): the selected clips under the
 *  playhead — or every clip under it when nothing is selected — are cut on the frame it is on. A link group
 *  is one cut; separate lanes are separate edits, so the toast's Undo takes back as many as were made. Says
 *  why when there is nothing to cut instead of doing nothing. */
export async function splitSelection(): Promise<void> {
	const selected = editor.selectedClipIds;
	const plan = planPlayheadSplit(editor.timeline, selected, ui.time, editor.fps);
	if (plan.splits.length === 0) {
		toast.info(splitNotice(plan, selected.length));
		return;
	}
	let done = 0;
	try {
		for (const s of plan.splits) {
			await editor.split(s.clipId, s.at);
			done++;
		}
	} catch (e) {
		toast.error(errorMessage(e));
		if (done === 0) return;
	}
	const n = done;
	toast(splitDone(n), {
		action: {
			label: 'Undo',
			onClick: () => {
				void (async () => {
					for (let i = 0; i < n; i++) await editor.undo();
				})().catch((e) => toast.error(errorMessage(e)));
			}
		}
	});
	if (plan.problems.length > 0) toast.warning(splitNotice({ ...plan, splits: [] }, selected.length));
}

/** One press of a roll / slip / slide key (`edit.rollBack` …): a frame of the cut, planned and checked
 *  on a scratch copy first (`key-edits.ts`). A refusal is said, not swallowed; a roll takes the playhead
 *  with the cut so the next press acts on the same one. */
export async function keyEdit(tool: KeyTool, dir: 1 | -1): Promise<void> {
	const result = planKeyEdit(
		tool,
		{
			timeline: editor.timeline,
			assets: editor.assets,
			selected: editor.selectedClipIds,
			primary: editor.selectedClipId,
			time: ui.time,
			fps: editor.fps
		},
		dir
	);
	if (!result.ok) {
		toast.info(result.reason);
		return;
	}
	const { edit, delta, cutAt } = result.plan;
	try {
		if (edit.tool === 'roll') await editor.roll(edit.a, edit.b, delta, undefined);
		else if (edit.tool === 'slip') await editor.slip(edit.clipId, delta, undefined);
		else await editor.slide(edit.clipId, delta, undefined);
	} catch (e) {
		toast.error(errorMessage(e));
		return;
	}
	if (cutAt !== null) ui.seek(cutAt);
	toast(result.note ?? keyEditDone(tool, dir), { action: { label: 'Undo', onClick: () => void editor.undo() } });
}

// ---- linked A/V: detach, reattach, link, unlink --------------------------------------
// The clip menu and the keymap both run these, over `linkPlans` — one place that knows what
// is possible for the selection and why not, so a key and a menu line never disagree.

/** What the selection can be Detached / Reattached / Linked / Unlinked, and why not. */
export function selectionLinkPlans(): LinkPlans {
	const audible = new Set(editor.assets.filter((a) => a.streams.some((s) => s.kind === 'audio')).map((a) => a.id));
	return linkPlans(editor.timeline, (id) => audible.has(id), editor.selectedClipIds);
}

/** **Detach audio** (⇧D, the clip menu): each selected picture clip still playing its own
 *  sound hands it to a linked clip on an audio track and goes quiet, so it is heard once.
 *  One revision for the lot (`detach_audio_clips`); the toast's Undo takes it back. A clip the
 *  backend skips (a locked track) is named in the toast rather than failing the rest. */
export async function detachSelection(): Promise<void> {
	const plan = selectionLinkPlans().detach;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	let report;
	try {
		report = await editor.detachAudioClips(plan.ids);
	} catch (e) {
		toast.error(errorMessage(e));
		return;
	}
	// The sound that was just made is part of the pictures' link group: select it with them.
	editor.selectClips(withLinkPartners(editor.timeline, [...plan.ids]), editor.selectedClipId);
	const note = report.skipped.length > 0 ? `${detachedNotice(report.detached)} — ${report.skipped.length} skipped (${report.skipped[0].reason})` : detachedNotice(report.detached);
	toast(note, { action: { label: 'Undo', onClick: () => void editor.undo() } });
}

/** **Reattach audio** (⇧⌘D, the clip menu): the linked audio clip goes and the picture
 *  plays its own sound again. One revision for the whole selection (`reattach_audio_clips`),
 *  all or nothing — a refusal changes nothing — and the toast's Undo takes it back. */
export async function reattachSelection(): Promise<void> {
	const plan = selectionLinkPlans().reattach;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	try {
		await editor.reattachAudioClips(plan.ids);
	} catch (e) {
		toast.error(errorMessage(e));
		return;
	}
	toast(reattachedNotice(plan.ids.length), { action: { label: 'Undo', onClick: () => void editor.undo() } });
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
