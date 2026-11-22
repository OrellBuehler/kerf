// Running an edit from a click: the backend can refuse (a stale id, a value out
// of range, no clip under the playhead), and a button has nowhere to put that
// but a notice.

import { downloadTextFile, getAssetMetadata, inTauri, pickExportPath } from './api';
import { folderOption } from './folder-option';
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
import { settings } from './settings.svelte';
import { timeDisplay } from './time-display.svelte';
import { displayTracks } from './track-order';
import { quantizeTime } from './frames';
import { renderedFps } from './timecode';
import { cutNotice, lockedNotice, removalNotice } from './removal';
import { planPlayheadTrim, trimNotice } from './trim-tools';
import { keyEditDone, planKeyEdit, planPlayheadSplit, splitDone, splitNotice, type KeyTool } from './key-edits';
import { clampedNotice, motionOptions, motionReason, motionTargets } from './motion-ui';
import { additions, limitNotice } from './behavior-ui';
import { randomSeed } from './behaviors';
import {
	NOTHING_AT_PLAYHEAD,
	noEditPoint,
	pickAtPlayhead,
	stepEdit,
	stepSelection,
	targetedTracks,
	type Step
} from './edit-points';
import { gapPoint, planGapClose, type Gap } from './gaps';
import { cutAllLanes, cutAllSaid, cutAllWhy, gapSaid, planMarkedRange, rangeSaid, type Said } from './range-edit-ui';
import { NOTHING_TO_REDO, NOTHING_TO_UNDO, burstLine, headSeq, isEndOfHistory, nextBurst, redoLabel, undoLabel, type Burst } from './undo-feedback';
import type { BehaviorKind, MotionPreset, SplitSide } from './types';

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

/** Write a motion preset into the selected picture clips (the Motion tab) as one edit, in the mode, length and
 *  strength the tab holds (`ui.motion*`). A sound among the selected is left alone (a linked picture's partner is
 *  selected with it); the one thing said afterwards is a window the clip cut short. */
export async function applyMotion(preset: MotionPreset): Promise<void> {
	if (holdForPreview()) return;
	const selected = editor.selectedClips;
	const reason = motionReason(editor.timeline, selected);
	if (reason) {
		toast.info(reason);
		return;
	}
	const { motionMode: mode, motionLength: length, motionIntensity: intensity } = ui;
	try {
		const ids = motionTargets(editor.timeline, selected).map((c) => c.id);
		const applied = await editor.applyMotionPreset(ids, preset, motionOptions(mode, length, intensity));
		const note = clampedNotice(preset, applied, length);
		if (note) toast.info(note);
	} catch (e) {
		report(e);
	}
}

/** Add a behaviour of `kind`, with its starting numbers, to the selected picture clips (the Motion tab): each keeps
 *  the behaviours it has and gets the new one last. Clips that carry the same list are one edit (the usual case:
 *  none have any); a clip already at the limit refuses the lot, as the backend would, and says so. */
export async function addBehavior(kind: BehaviorKind): Promise<void> {
	if (holdForPreview()) return;
	const selected = editor.selectedClips;
	const reason = motionReason(editor.timeline, selected);
	if (reason) {
		toast.info(reason);
		return;
	}
	const targets = motionTargets(editor.timeline, selected);
	const { full, groups } = additions(targets, kind, randomSeed());
	if (full.length > 0) {
		toast.info(limitNotice(full, targets.length));
		return;
	}
	try {
		for (const g of groups) await editor.setBehaviors(g.ids, g.list);
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

/** Trim start / end to the playhead (Q / W, and the clip menu), and with `ripple` Premiere's *Ripple trim to the
 *  playhead* (the project's mode ignored: the later clips always close the gap): cut every selected
 *  clip the playhead is inside at the playhead and throw away the `left` or `right`
 *  half. The backend does the cut — ONE revision for the whole selection (so a V1 clip
 *  and its A1 partner undo together), each track rippling on its own under ripple mode,
 *  the surviving halves keeping their clips' ids so the selection holds — and this
 *  decides *what* to cut (`planPlayheadTrim`) and says so when there is nothing: a
 *  keypress that does nothing silently is a key that seems broken. */
export async function trimSelection(side: SplitSide, ripple?: true): Promise<void> {
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
			side,
			undefined,
			ripple
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
		toast.success(`Saved the transcript of ${asset.name}`, folderOption(out));
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


// ---- the range edits: cut every track, lift, extract, close a gap -----------------------------------------
// One revision each (`range-edit.ts`); each says what it did, and what it left alone — a locked track, a clip left
// whole, the sound the sync lock trimmed — so that nothing about it happens silently. A refusal is the engine's own
// sentence (`report`); "nothing to do" is said first, here, where the plan is made.

/** The toast for an edit that says `said`, with Undo; a warning when it left something undone. */
function sayEdit(said: Said): void {
	const opts = { description: said.detail, action: { label: 'Undo', onClick: () => void undoEdit() } };
	if (said.warn) toast.warning(said.text, opts);
	else toast(said.text, opts);
}

const fmtTime = (s: number): string => timeDisplay.fmt(s);

/** What a clip is called in a sentence: its asset's name (a title's own). */
function clipName(id: string): string {
	const clip = editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id);
	return clip ? editor.assetName(clip.asset_id) : id;
}

/** **Add edit to all tracks** at `at` (the key, Edit, the razor with Shift): every clip with `at` inside it, on
 *  every unlocked track, cut in one revision. Linked partners are cut with their clips unless `link` is `false`
 *  (Alt). Says so when there is nothing there to cut instead of asking the engine. */
export async function cutAllTracks(at: number, link?: boolean, source: 'playhead' | 'pointer' = 'playhead'): Promise<void> {
	if (holdForPreview()) return;
	const lanes = cutAllLanes(editor.timeline, at, rangeFps(), link !== false);
	const why = cutAllWhy(lanes, source);
	if (why) {
		toast.info(why);
		return;
	}
	try {
		const done = await editor.splitAllTracks(at, undefined, link);
		sayEdit(cutAllSaid(done, fmtTime, clipName));
	} catch (e) {
		report(e);
	}
}

/** The rate the range edits count a frame at: the cut's **rendered** rate, as the engine reads it (`oneFrame` in
 *  `link-ops.ts`, `Project`'s `frame`), not the editing grid's `editor.fps` — they differ when the first clip is muted or
 *  disabled — so the marks the GUI hands over are on the engine's own grid. */
export const rangeFps = (): number => renderedFps(editor.timeline, editor.assets);

/** **Add edit to all tracks** at the playhead (⌘⇧K): on the frame it shows. */
export function cutAllAtPlayhead(): Promise<void> {
	return cutAllTracks(quantizeTime(ui.time, { fps: rangeFps() }));
}

/** **Lift** (`;`) and **Extract** (`'`) the range between the in and out marks, on every unlocked track: a lift
 *  leaves the gap, an extract closes it. The marks are quantized to frames here, so the engine's "less than a frame
 *  would be left" refusal comes only from a clip whose own edge is off the frame grid, and then it names the range
 *  that works. Without both marks it says to set them. */
async function markedRange(kind: 'lift' | 'extract'): Promise<void> {
	if (holdForPreview()) return;
	const plan = planMarkedRange(ui.markIn, ui.markOut, rangeFps(), kind, {
		in: settings.shortcut('range.markIn'),
		out: settings.shortcut('range.markOut')
	});
	if (!plan.ok) {
		toast.info(plan.reason);
		return;
	}
	try {
		const done = kind === 'lift' ? await editor.liftRange(plan.from, plan.to) : await editor.extractRange(plan.from, plan.to);
		sayEdit(rangeSaid(kind, done, fmtTime));
	} catch (e) {
		report(e);
	}
}
export const liftMarked = (): Promise<void> => markedRange('lift');
export const extractMarked = (): Promise<void> => markedRange('extract');

/** **Ripple delete gap** for a gap that is known (a clicked one): the clips after it move up, their linked
 *  partners with them. The selection of the gap is over. */
export async function closeGapAt(gap: Gap): Promise<void> {
	if (holdForPreview()) return;
	const track = editor.timeline.tracks.find((t) => t.id === gap.trackId);
	if (track?.locked) {
		toast.info(`Track ${track.name} is locked`);
		return;
	}
	try {
		const done = await editor.rippleDeleteGap(gap.trackId, gapPoint(gap));
		ui.gap = null;
		sayEdit(gapSaid(done, track?.name ?? gap.trackId));
	} catch (e) {
		report(e);
	}
}

/** **Ripple delete gap** (the key, Edit, the menus): the gap under the playhead on the lane last clicked or focused
 *  (`ui.activeLane`) — never another lane's. Says what to do, or that there is no gap there. */
export async function closeGap(): Promise<void> {
	if (holdForPreview()) return;
	const plan = planGapClose(editor.timeline, { lane: ui.activeLane, time: ui.time });
	if (!plan.ok) {
		toast.info(plan.reason);
		return;
	}
	await closeGapAt(plan.gap);
}

// ---- keyboard-only editing: edit points and the selection ----------------------------------------------

/** One toast per kind of "there is nothing that way", rewritten on every press: a held key does not stack them. */
const quietly = (text: string, id: string): void => void toast.info(text, { id, duration: 1800 });

/** **Previous / next edit point** (↑ ↓): the playhead goes to the cut before / after it — of the selected clips'
 *  tracks, or of every track when nothing is selected. */
export function goToEdit(dir: 1 | -1): void {
	const tracks = targetedTracks(editor.timeline, editor.selectedClipIds);
	const at = stepEdit(editor.timeline, ui.time, dir, editor.fps, editor.duration, tracks);
	if (at === null) {
		quietly(noEditPoint(dir), 'edit-point');
		return;
	}
	ui.seek(at, { snap: 'up' });
}

/** Make `id` the selection (with its linked partners, as a click does), or with `extend` add it to it, and bring
 *  it into view. Along a track an extension is a range, like Shift-click; across tracks the clip is added. */
function selectTo(id: string, step: Step | 'at', extend: boolean): void {
	const clip = editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id);
	if (!clip) return;
	if (extend && editor.selectedClipIds.length > 0) {
		if (step === 'prev' || step === 'next') editor.selectClip(id, 'range');
		else editor.selectClips(withLinkPartners(editor.timeline, [...editor.selectedClipIds, id]), id);
	} else {
		editor.selectClip(id);
	}
	void editor.select(clip.asset_id);
	ui.revealClip(id);
}

/** **Select the clip at the playhead** (D): the topmost, and pressed again the next one down the stack. */
export function selectAtPlayhead(): void {
	const id = pickAtPlayhead(displayTracks(editor.timeline.tracks), ui.time, editor.selectedClipIds);
	if (!id) {
		quietly(NOTHING_AT_PLAYHEAD, 'select-step');
		return;
	}
	selectTo(id, 'at', false);
}

/** **Select the previous / next clip** on the selected clip's track, or the clip on the **track above / below**; with
 *  `extend`, added to the selection. With nothing selected, the clip at the playhead. `from` is the clip the move starts from:
 *  the primary selection, or — for a key pressed on a focused clip — that clip, which is what the keyboard is on. */
export function selectStep(step: Step, extend: boolean, from: string | null = editor.selectedClipId): void {
	const result = stepSelection(displayTracks(editor.timeline.tracks), from, step, extend, ui.time);
	if (!result.ok) {
		quietly(result.reason, 'select-step');
		return;
	}
	selectTo(result.to.id, result.to.extend ? step : 'at', result.to.extend);
}

/** The actions that move about the cut. A clip that has the keyboard focus keeps the arrows the page would have taken
 *  (a button's arrows are its own), so the timeline hands these to `runNav` for it. */
const NAV: Readonly<Record<string, (from: string | null) => void>> = {
	'playback.prevEdit': () => goToEdit(-1),
	'playback.nextEdit': () => goToEdit(1),
	'select.atPlayhead': () => selectAtPlayhead(),
	'select.prevClip': (from) => selectStep('prev', false, from),
	'select.nextClip': (from) => selectStep('next', false, from),
	'select.laneAbove': (from) => selectStep('above', false, from),
	'select.laneBelow': (from) => selectStep('below', false, from),
	'select.extendPrev': (from) => selectStep('prev', true, from),
	'select.extendNext': (from) => selectStep('next', true, from),
	'select.extendAbove': (from) => selectStep('above', true, from),
	'select.extendBelow': (from) => selectStep('below', true, from)
};

export const isNavAction = (id: string | null): id is string => id !== null && Object.hasOwn(NAV, id);

/** Run one of the navigation actions; false for any other. `from` is the clip the keyboard is on (a focused clip): the
 *  selection moves step from it, not from wherever the selection's primary happens to be. */
export function runNav(id: string, from: string | null = editor.selectedClipId): boolean {
	if (!Object.hasOwn(NAV, id)) return false;
	NAV[id](from);
	return true;
}

// ---- undo and redo say what they did -----------------------------------------------------------------

const UNDO_TOAST = 'undo-redo';
/** "Nothing to undo" is its own toast, not the burst's rewritten: a toast updated under its id keeps the action it had
 *  (the Redo of the line before), and at the end of the history there is nothing to redo. */
const UNDO_ROOT_TOAST = 'undo-end';
let burst: Burst | null = null;
let inFlight: Promise<void> | null = null;
let waiting: 'undo' | 'redo' | null = null;

/** The end of the history: said quietly, once, in place of whatever the burst's toast was saying. */
function saidEnd(kind: 'undo' | 'redo'): void {
	toast.dismiss(UNDO_TOAST);
	toast.info(kind === 'undo' ? NOTHING_TO_UNDO : NOTHING_TO_REDO, { id: UNDO_ROOT_TOAST, duration: 1500 });
}

/** One step back or forward, and the line for it. */
async function oneStep(kind: 'undo' | 'redo'): Promise<void> {
	if (!(kind === 'undo' ? editor.canUndo : editor.canRedo)) {
		saidEnd(kind);
		return;
	}
	const label = kind === 'undo' ? undoLabel(editor.history, editor.historyMore) : redoLabel(editor.history);
	const before = headSeq(editor.history);
	try {
		await (kind === 'undo' ? editor.undo() : editor.redo());
	} catch (e) {
		// A press that outran the history (the head not yet refreshed) meets the backend's own "nothing to undo": the end of
		// the line, not a failure.
		if (isEndOfHistory(e)) saidEnd(kind);
		else report(e);
		return;
	}
	toast.dismiss(UNDO_ROOT_TOAST);
	burst = nextBurst(burst, kind, label, Date.now(), before, headSeq(editor.history));
	toast(burstLine(burst), {
		id: UNDO_TOAST,
		duration: 2200,
		action: { label: kind === 'undo' ? 'Redo' : 'Undo', onClick: () => void stepHistory(kind === 'undo' ? 'redo' : 'undo') }
	});
}

/** **Undo** / **Redo** (the keys, Edit, the timeline's buttons): one step back or forward, and a short line saying
 *  which edit it was — "Undid: Split clip", with Redo beside it. A run of presses with nothing else edited between is one
 *  line that counts up (`undo-feedback.ts`), rewritten in place rather than stacked. The steps are **one at a time**: a
 *  held key over IPC must not start the next before the history has been read back (the labels, the head and whether
 *  there is anything left all come from it), so a press that arrives meanwhile waits — the latest one, not a queue of
 *  them — and runs when the step in flight is done. */
function stepHistory(kind: 'undo' | 'redo'): Promise<void> {
	if (holdForPreview()) return Promise.resolve();
	if (inFlight) {
		waiting = kind;
		return inFlight;
	}
	inFlight = (async () => {
		try {
			let next: 'undo' | 'redo' | null = kind;
			while (next) {
				waiting = null;
				await oneStep(next);
				next = waiting;
			}
		} finally {
			inFlight = null;
			waiting = null;
		}
	})();
	return inFlight;
}
export const undoEdit = (): Promise<void> => stepHistory('undo');
export const redoEdit = (): Promise<void> => stepHistory('redo');
