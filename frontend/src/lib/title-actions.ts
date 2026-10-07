// What the Titles controls do, shared by the Inspector's "Titles lane" section
// and the library's Titles tab so the two cannot drift: add a title at the
// playhead, add one in a preset style, caption the cut, caption it from a
// subtitle file, pick a title to edit.

import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { confirmAction, pickCaptionFile, type CaptionFilePick } from './api';
import { describeImport } from './caption-import';
import {
	CAPTION_CONFIRM_TITLE,
	IMPORT_CONFIRM_TITLE,
	TIMELINE_CHOICE,
	generatedCount,
	importRequest,
	importTone,
	keepLinesOn,
	replaceConfirm,
	type ImportChoice
} from './caption-import-ui';
import { attempt, errorMessage } from './ops';
import type { TextStyle } from './style-presets';
import type { TextOverlay } from './types';

/** A plain title at the playhead. */
export function addTextHere(): Promise<void> {
	const at = Math.max(0, ui.time);
	return attempt(() => editor.addTitle('Text', at, at + 3));
}

/** Add a preset-styled overlay at the playhead: create, style, then (for faded
 *  styles) keyframe the opacity in and out, and select it. */
export function addStyledTitle(s: TextStyle): Promise<void> {
	const at = Math.max(0, ui.time);
	return attempt(async () => {
		await editor.addOverlay(s.text, at, at + s.duration);
		const created = editor.overlays[editor.overlays.length - 1];
		if (!created) return;
		const { bg, ...rest } = s.style;
		await editor.updateOverlay(created.id, bg == null ? rest : { ...rest, bg });
		if (s.fade > 0) {
			const kf = (time: number, opacity: number) => ({ time, pos_x: s.style.pos_x, pos_y: s.style.pos_y, opacity });
			await editor.setOverlayKeyframes(created.id, [
				kf(0, 0),
				kf(s.fade, 1),
				kf(s.duration - s.fade, 1),
				kf(s.duration, 0)
			]);
		}
		editor.selectOverlay(created.id);
	});
}

/** Pick a title from the lane's list, and bring the playhead into its span so
 *  the preview has it on screen to move and resize. Picking the selected one
 *  again lets go of it. */
export function pickTitle(o: TextOverlay) {
	if (editor.selectedOverlayId === o.id) {
		editor.selectOverlay(null);
		return;
	}
	editor.selectOverlay(o.id);
	if (ui.time < o.start || ui.time > o.end) ui.seek(o.start);
}

/** Captions are placed in timeline time, so they follow the cut — which also
 *  means a later trim moves the words out from under them. Re-running replaces
 *  the generated set, so the button stays the same after the first press and
 *  only its label admits what it is doing. */
export async function makeCaptions(): Promise<void> {
	if (!editor.timeline.tracks.some((t) => t.clips.length > 0)) {
		toast.error('Put a clip on the timeline first');
		return;
	}
	return attempt(async () => {
		if (!(await confirmReplaceCaptions(CAPTION_CONFIRM_TITLE))) return;
		await editor.generateCaptions({ style: ui.captionStyle });
	});
}

/** Ask before captions are written over ones already on the cut — a set that was
 *  generated from transcripts or imported from a subtitle file, which the engine
 *  treats as one (captions are one lane of text) and replaces whole. Counted on
 *  the live cut, the one the edit lands on, even while a proposal is on screen.
 *  `true` when there is nothing to replace or the user agreed; `fileName` names
 *  what is coming in, for an import. */
export async function confirmReplaceCaptions(title: string, fileName?: string): Promise<boolean> {
	const existing = generatedCount(editor.liveTimeline.overlays);
	return existing === 0 || (await confirmAction(replaceConfirm(existing, fileName), title));
}

export function dropCaptions(): Promise<void> {
	return attempt(() => editor.clearCaptions());
}

/** Whether an import is past its picker — between the file being chosen and the
 *  edit landing — so a second request in that window is dropped rather than
 *  stacked behind the confirmation. Not held across the picker itself: a picker
 *  that never answers must not leave the controls dead. */
let importing = false;

/** Caption the cut from a subtitle file the user picks (`.srt` / `.ass` / `.ssa`),
 *  timed as `choice` says and in the look the caption chips are set to. Picks the
 *  file, asks before replacing captions that are already on the cut — generated
 *  and imported ones are one set, so the import replaces either — then imports as
 *  one revision and says what it did: a success when every cue landed, a warning
 *  when any fell outside the cut, overlapped, or could not be read. The selection
 *  is left alone. Never throws; resolves `true` only when captions were imported
 *  (a cancelled picker or a declined confirmation is `false`, and silent). */
export async function importCaptionFile(choice: ImportChoice = TIMELINE_CHOICE): Promise<boolean> {
	if (importing) return false;
	let picked: CaptionFilePick | null;
	try {
		// Straight from the click: the browser harness opens its file input here.
		picked = await pickCaptionFile();
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	}
	if (!picked || importing) return false;
	importing = true;
	try {
		if (!(await confirmReplaceCaptions(IMPORT_CONFIRM_TITLE, picked.name))) return false;
		const req = importRequest(choice, ui.captionStyle, {
			keepLines: keepLinesOn(ui.captionImportKeepLines, ui.captionStyle, editor.liveTimeline.format),
			offset: ui.captionImportOffset
		});
		const summary =
			picked.kind === 'path'
				? await editor.importCaptions(picked.path, req)
				: await editor.importCaptionsText(picked.text, { ...req, format: picked.format ?? undefined });
		toast[importTone(summary)](describeImport(summary));
		return true;
	} catch (e) {
		toast.error(errorMessage(e));
		return false;
	} finally {
		importing = false;
	}
}
