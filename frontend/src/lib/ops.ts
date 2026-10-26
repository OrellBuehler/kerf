// Running an edit from a click: the backend can refuse (a stale id, a value out
// of range, no clip under the playhead), and a button has nowhere to put that
// but a notice.

import { downloadTextFile, getAssetMetadata, inTauri, pickExportPath, revealPath } from './api';
import { animatedProps, keyframeOffer, propsPhrase } from './keyframe-copy';
import { toSrt } from './transcript';
import { editor, PREVIEW_READ_ONLY, PreviewReadOnly } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { withLinkPartners } from './link-groups';
import { appendedLine } from './tag-ui';
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

/** Say why an edit did not happen: a refusal because the proposal is being previewed is a state, not a failure,
 *  so it is a notice; anything else is an error. The one place a rejected edit becomes a toast. */
export function report(e: unknown): void {
	if (e instanceof PreviewReadOnly) toast.info(e.message);
	// A stop the user asked for (smart crop, an import, a cover frame) is not a failure.
	else if (isCancelled(e)) toast.info(capitalize(errorMessage(e)));
	else toast.error(errorMessage(e));
}

/** A rejection that says the user stopped the run: `smart crop cancelled`, `export cancelled`. */
export function isCancelled(e: unknown): boolean {
	return /\bcancel+ed$/i.test(errorMessage(e).trim());
}

const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** The first line of every edit that reads the cut on screen before it asks the backend: while the proposal is
 *  previewed that cut is not the project's, so there is nothing to plan, copy or run — say so and stop.
 *  Resolves to whether the edit is held (`if (holdForPreview()) return;`). */
export function holdForPreview(): boolean {
	if (!editor.previewingStaged) return false;
	toast.info(PREVIEW_READ_ONLY);
	return true;
}

/** Await `op`, turning a rejection into a toast (`report`). Never throws. */
export async function attempt(op: () => Promise<unknown>): Promise<void> {
	try {
		await op();
	} catch (e) {
		report(e);
	}
}

/** Delete the selected clips as one edit (one undo), with `ripple` closing the gaps
 *  behind them — the Delete / Shift+Delete keys and the clip menu. Says what it
 *  did, offers Undo, and reports a refusal instead of throwing. */
export async function deleteSelection(ripple: boolean): Promise<void> {
	if (holdForPreview()) return;
	try {
		const { removed, skipped } = await editor.removeSelected(ripple);
		if (removed === 0) {
			if (skipped > 0) toast.error(lockedNotice(skipped));
			return;
		}
		toast(removalNotice(removed, skipped, ripple), { action: { label: 'Undo', onClick: () => void editor.undo() } });
	} catch (e) {
		report(e);
	}
}

/** Cut (⌘/Ctrl+X): the selected clips go to the clipboard and off the timeline as
 *  one edit. Only what the cut actually removes is copied — clips on a locked
 *  track stay (and stay selected), and if that is everything, nothing happens to
 *  the clipboard and the notice says why instead of claiming a cut. */
export async function cutSelection(): Promise<void> {
	if (holdForPreview()) return;
	try {
		editor.copySelection(true);
		const { removed, skipped } = await editor.removeSelected(false);
		if (removed === 0) {
			if (skipped > 0) toast.error(lockedNotice(skipped));
			return;
		}
		toast(cutNotice(removed, skipped), { action: { label: 'Undo', onClick: () => void editor.undo() } });
	} catch (e) {
		report(e);
	}
}

/** Paste the clipboard at the playhead (⌘V, Edit › Paste, the timeline's menu) and say how many. */
export async function pasteClips(): Promise<void> {
	if (holdForPreview()) return;
	try {
		const n = await editor.paste(ui.time);
		if (n) toast(n === 1 ? 'Clip pasted' : `${n} clips pasted`);
	} catch (e) {
		report(e);
	}
}

/** Duplicate the selection right after itself (⌘D, Edit › Duplicate, the clip menu). */
export async function duplicateClips(): Promise<void> {
	if (holdForPreview()) return;
	try {
		const n = await editor.duplicateSelection();
		if (n) toast(n === 1 ? 'Clip duplicated' : `${n} clips duplicated`);
	} catch (e) {
		report(e);
	}
}

/** Drop a marker at the playhead (M, the timeline's menu), with an Undo. */
export async function addMarker(): Promise<void> {
	if (holdForPreview()) return;
	try {
		await editor.addMarkerAtPlayhead(ui.time);
		toast('Marker added', { action: { label: 'Undo', onClick: () => void editor.undo() } });
	} catch (e) {
		report(e);
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
	if (holdForPreview()) return;
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
		report(e);
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
	if (holdForPreview()) return;
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
		report(e);
		if (done === 0) return;
	}
	const n = done;
	toast(splitDone(n), {
		action: {
			label: 'Undo',
			onClick: () => {
				void (async () => {
					for (let i = 0; i < n; i++) await editor.undo();
				})().catch(report);
			}
		}
	});
	if (plan.problems.length > 0) toast.warning(splitNotice({ ...plan, splits: [] }, selected.length));
}

/** One press of a roll / slip / slide key (`edit.rollBack` …): a frame of the cut, planned and checked
 *  on a scratch copy first (`key-edits.ts`). A refusal is said, not swallowed; a roll takes the playhead
 *  with the cut so the next press acts on the same one. */
export async function keyEdit(tool: KeyTool, dir: 1 | -1): Promise<void> {
	if (holdForPreview()) return;
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
		report(e);
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
	if (holdForPreview()) return;
	const plan = selectionLinkPlans().detach;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	let result;
	try {
		result = await editor.detachAudioClips(plan.ids);
	} catch (e) {
		report(e);
		return;
	}
	// The sound that was just made is part of the pictures' link group: select it with them.
	editor.selectClips(withLinkPartners(editor.timeline, [...plan.ids]), editor.selectedClipId);
	const note = result.skipped.length > 0 ? `${detachedNotice(result.detached)} — ${result.skipped.length} skipped (${result.skipped[0].reason})` : detachedNotice(result.detached);
	toast(note, { action: { label: 'Undo', onClick: () => void editor.undo() } });
}

/** **Reattach audio** (⇧⌘D, the clip menu): the linked audio clip goes and the picture
 *  plays its own sound again. One revision for the whole selection (`reattach_audio_clips`),
 *  all or nothing — a refusal changes nothing — and the toast's Undo takes it back. */
export async function reattachSelection(): Promise<void> {
	if (holdForPreview()) return;
	const plan = selectionLinkPlans().reattach;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	try {
		await editor.reattachAudioClips(plan.ids);
	} catch (e) {
		report(e);
		return;
	}
	toast(reattachedNotice(plan.ids.length), { action: { label: 'Undo', onClick: () => void editor.undo() } });
}

/** **Link** the selected clips (⌘L, the clip menu): one clip per track, a move / trim / split
 *  / delete of one carried to the others. */
export async function linkSelection(): Promise<void> {
	if (holdForPreview()) return;
	const plan = selectionLinkPlans().link;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	try {
		await editor.linkClips(plan.ids);
	} catch (e) {
		report(e);
		return;
	}
	toast(linkedNotice(plan.ids.length), { action: { label: 'Undo', onClick: () => void editor.undo() } });
}

/** **Unlink** the selected clips (⇧⌘L, the clip menu): each leaves its group. */
export async function unlinkSelection(): Promise<void> {
	if (holdForPreview()) return;
	const plan = selectionLinkPlans().unlink;
	if (plan.reason !== null) {
		toast.info(plan.reason);
		return;
	}
	try {
		await editor.unlinkClips(plan.ids);
	} catch (e) {
		report(e);
		return;
	}
	toast(unlinkedNotice(plan.ids.length), { action: { label: 'Undo', onClick: () => void editor.undo() } });
}

// ---- things the engine could always do and the interface did not reach ------------------------------

/** **Export transcript as .srt** (the library's menu, the asset's panel, the transcript's header): the cached
 *  transcript of an asset as a SubRip file, in the asset's own time. The desktop app asks where; the browser harness
 *  hands the same text to the download manager. Says why when there is no transcript to write. */
export async function exportTranscriptSrt(assetId: string): Promise<void> {
	const asset = editor.assets.find((a) => a.id === assetId);
	if (!asset) return;
	let transcript;
	try {
		transcript = (await getAssetMetadata(assetId)).analysis?.transcript ?? [];
	} catch (e) {
		report(e);
		return;
	}
	if (transcript.length === 0) {
		toast.info(`${asset.name} has no transcript yet — transcribe it first (Analyze › Transcribe speech)`);
		return;
	}
	const name = `${asset.name.replace(/\.[^./\\]+$/, '')}.srt`;
	if (!inTauri()) {
		downloadTextFile(toSrt(transcript), name, 'application/x-subrip');
		toast.success(`Saved ${name} — in the desktop app you choose where it goes`);
		return;
	}
	try {
		const path = await pickExportPath('srt', name);
		if (!path) return;
		const out = await editor.exportSrt(assetId, path);
		toast.success(`Saved the transcript of ${asset.name}`, { action: { label: 'Show in folder', onClick: () => void revealPath(out).catch(() => {}) } });
	} catch (e) {
		report(e);
	}
}

/** **Append all media to the timeline** (the library's menu): every asset, in library order, end to end — a picture
 *  on the first picture track and a sound on the first sound track, each after what is already there. One revision
 *  (`concatenate`), so one Undo. With `ids` (the assets a tag filter shows) only those, in the order given. */
export async function appendAllMedia(ids: readonly string[] = editor.assets.map((a) => a.id)): Promise<void> {
	if (holdForPreview()) return;
	if (ids.length === 0) {
		toast.info('Import media first');
		return;
	}
	try {
		await editor.concatenate([...ids]);
	} catch (e) {
		report(e);
		return;
	}
	toast(appendedLine(ids.length, editor.assets.length), {
		action: { label: 'Undo', onClick: () => void editor.undo() }
	});
}

/** **Copy keyframes** (the clip menu, the Inspector's Animation section): remember the clip whose animation is to be
 *  pasted. The animation is read at the paste, so what is pasted is what the clip does then. */
export function copyKeyframesOf(clipId: string): void {
	if (holdForPreview()) return;
	const offer = keyframeOffer(editor.timeline, editor.keyframeSource, clipId).copy;
	if (offer.disabled) {
		toast.info(offer.reason ?? 'Nothing to copy');
		return;
	}
	editor.keyframeSource = clipId;
	const clip = editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === clipId);
	toast(`Copied the animation of ${clip ? editor.assetName(clip.asset_id) : 'the clip'} — select another clip and paste it`);
}

/** **Paste keyframes**: the copied clip's animation onto `clipId`, one revision, keeping the clip's own timing (the
 *  keys stay at the same distance from the clip's start). */
export async function pasteKeyframesOnto(clipId: string): Promise<void> {
	if (holdForPreview()) return;
	const offer = keyframeOffer(editor.timeline, editor.keyframeSource, clipId).paste;
	const from = editor.keyframeSource;
	if (offer.disabled || !from) {
		toast.info(offer.reason ?? 'Nothing to paste');
		return;
	}
	const source = editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === from);
	const props = source ? animatedProps(source) : [];
	try {
		await editor.copyKeyframes(from, clipId, [], 0);
	} catch (e) {
		report(e);
		return;
	}
	toast(`Pasted the ${propsPhrase(props)} animation`, { action: { label: 'Undo', onClick: () => void editor.undo() } });
}
