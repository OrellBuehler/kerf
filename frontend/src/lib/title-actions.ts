// What the Titles controls do, shared by the Inspector's "Titles lane" section
// and the library's Titles tab so the two cannot drift: add a title at the
// playhead, add one in a preset style, add an animated SVG title, caption the cut, caption it from a
// subtitle file, pick a title to edit.

import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { confirmAction, listSvgPresets, pickCaptionFile, svgPresetPreview, validateSvg, type CaptionFilePick } from './api';
import { CONFIRM } from './confirm-copy';
import { describeImport } from './caption-import';
import {
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
import { sampleSvg, svgDataUrl, svgFrame, svgPresetChoice } from './svg-titles';
import type { SvgCheck, SvgOverlay, SvgPresetInfo, SvgPresetKind, TextOverlay } from './types';

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
	if (ui.time < o.start || ui.time > o.end) ui.seek(o.start, { snap: 'up', end: o.end });
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
		if (!(await confirmReplaceCaptions())) return;
		await editor.generateCaptions({ style: ui.captionStyle });
	});
}

/** Ask before captions are written over ones already on the cut — a set that was
 *  generated from transcripts or imported from a subtitle file, which the engine
 *  treats as one (captions are one lane of text) and replaces whole. Counted on
 *  the live cut, the one the edit lands on, even while a proposal is on screen.
 *  `true` when there is nothing to replace or the user agreed; `fileName` names
 *  what is coming in, for an import. */
export async function confirmReplaceCaptions(fileName?: string): Promise<boolean> {
	const existing = generatedCount(editor.liveTimeline.overlays);
	const copy = CONFIRM.replaceCaptions;
	return existing === 0 || (await confirmAction(replaceConfirm(existing, fileName), copy.title, copy));
}

export function dropCaptions(): Promise<void> {
	return attempt(() => editor.clearCaptions());
}

/** Whether an import is past its picker — between the file being chosen and the
 *  edit landing — so a second request in that window is dropped rather than
 *  stacked behind the confirmation. Not held across the picker itself: a picker
 *  that never answers must not leave the controls dead. */
let importing = false;

/** Caption the cut from a subtitle file the user picks (`.srt` / `.vtt` / `.ass` / `.ssa`),
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
		if (!(await confirmReplaceCaptions(picked.name))) return false;
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

// ---- animated SVG titles ---------------------------------------------------------------------
//
// The artwork is the engine's (rendered into every preview and export frame); the page asks for
// SVG text only to show a preset as a thumbnail, through an `<img>` data URL, never inline.

let presetListing: Promise<SvgPresetInfo[]> | null = null;

/** What each preset takes (`list_svg_presets`), asked for once: it is the same for the life of the app. A failed
 *  read is asked again next time. */
export function svgPresetInfos(): Promise<SvgPresetInfo[]> {
	presetListing ??= listSvgPresets().catch((e) => {
		presetListing = null;
		throw e;
	});
	return presetListing;
}

/** A thumbnail is a preset drawn long enough that the browser's own SMIL player plays the entrance once and holds
 *  it, rather than going on to the exit. */
export const THUMBNAIL_SECONDS = 600;

const thumbnails = new Map<string, Promise<string>>();

/** A preset's thumbnail as an `<img>` data URL, drawn for the project's delivery frame with its sample text and kept
 *  per frame shape. */
export function presetThumbnail(kind: SvgPresetKind): Promise<string> {
	const { width, height } = svgFrame(editor.timeline.format);
	const key = `${kind}|${width}x${height}`;
	let art = thumbnails.get(key);
	if (!art) {
		art = svgPresetPreview(kind, svgPresetChoice(kind)?.sample ?? { text: 'Your title' }, THUMBNAIL_SECONDS).then(svgDataUrl);
		art.catch(() => thumbnails.delete(key));
		thumbnails.set(key, art);
	}
	return art;
}

/** A preset animated title at the playhead, filled with its sample text, selected. */
export function addAnimatedTitle(kind: SvgPresetKind): Promise<void> {
	const at = Math.max(0, ui.time);
	const params = svgPresetChoice(kind)?.sample ?? { text: 'Your title' };
	return attempt(async () => void (await editor.addSvgTitle({ preset: { kind, params } }, at)));
}

/** A custom animated title at the playhead, seeded with a small animated SVG sized to the frame, selected, with the
 *  Inspector's source box open on it. */
export function addCustomSvgTitle(): Promise<void> {
	const at = Math.max(0, ui.time);
	const { width, height } = svgFrame(editor.timeline.format);
	return attempt(async () => {
		const created = await editor.addSvgTitle({ custom: sampleSvg(width, height) }, at);
		if (created) ui.svgSourceFocus = created.id;
	});
}

/** Pick an animated title from a list, and bring the playhead into its span so the preview has its box on screen.
 *  Picking the selected one again lets go of it. */
export function pickSvgTitle(o: SvgOverlay) {
	if (editor.selectedSvgOverlayId === o.id) {
		editor.selectSvgOverlay(null);
		return;
	}
	editor.selectSvgOverlay(o.id);
	if (ui.time < o.start || ui.time > o.end) ui.seek(o.start, { snap: 'up', end: o.end });
}

/** Put custom source on an animated title: the backend's reading of it first (`validate_svg` throws on a document it
 *  would refuse, and says what it could not honour — an edit does not), then the edit. The warnings are the caller's to show. */
export async function applySvgSource(overlayId: string, svg: string): Promise<SvgCheck> {
	const check = await validateSvg(svg);
	await editor.updateSvgOverlay(overlayId, { svg });
	return check;
}

/** Make a preset title custom: its own artwork becomes the source, which drops the preset, and the source box opens. */
export function convertToCustom(o: SvgOverlay): Promise<void> {
	return attempt(async () => {
		await editor.updateSvgOverlay(o.id, { svg: o.svg });
		ui.svgSourceFocus = o.id;
	});
}

/** Change which preset a title is: one edit that keeps the title's id, name, text, colours, timing, placement and place
 *  in the draw order, and leaves it selected if it was. */
export function changePreset(o: SvgOverlay, kind: SvgPresetKind): Promise<void> {
	if (!o.preset || o.preset.kind === kind) return Promise.resolve();
	return attempt(() => editor.updateSvgOverlay(o.id, { preset_kind: kind }));
}
