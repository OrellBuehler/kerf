/** The interface side of importing a subtitle file: which clips a file can be
 *  timed to, what a request looks like, and the words around it (the replace
 *  confirmation, the button tooltips, the toast's tone). Pure, so the controls
 *  and the titles-lane menu cannot say different things about the same import.
 *  Parsing and placing the cues is `caption-import.ts`; running the import is
 *  `importCaptionFile` in `title-actions.ts`.
 */

import { MAX_CAPTION_OFFSET, MAX_CUE_CHARS } from './caption-import';
import { renderedClips } from './captions';
import type {
	Asset,
	CaptionImportRequest,
	CaptionImportSummary,
	CaptionOptions,
	CaptionStyle,
	CaptionTimeBase,
	Delivery,
	TextOverlay,
	Timeline
} from './types';

/** The extensions the picker offers, and the ones `read_caption_file` reads. */
export const CAPTION_EXTENSIONS = ['srt', 'ass', 'ssa'] as const;

/** The last segment of a path, whichever separator it uses (a Windows path
 *  reaches the webview with backslashes). Trailing separators are ignored. */
export function baseName(path: string): string {
	const parts = path.split(/[\\/]+/).filter(Boolean);
	return parts[parts.length - 1] ?? path;
}

const parentName = (path: string): string => {
	const parts = path.split(/[\\/]+/).filter(Boolean);
	return parts.length >= 2 ? parts[parts.length - 2] : '';
};

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? '' : 's'}`;

/** Shorten `text` to at most `max` characters with an ellipsis. */
export function shorten(text: string, max: number): string {
	const chars = [...text];
	return chars.length <= max ? text : `${chars.slice(0, Math.max(max - 1, 1)).join('')}…`;
}

// ---- which clips a file can be timed to -------------------------------------

/** An asset a subtitle file can be timed to. */
export interface ImportAsset {
	id: string;
	name: string;
	/** `name`, told apart from another asset of the same name by its folder. */
	label: string;
	/** How many clips of the cut show it. */
	clips: number;
}

/** The assets a `source`-timed file can land on: those with a clip that actually
 *  renders (a muted track or a disabled clip shows nothing, so a caption timed
 *  to it would have nowhere to go), in the order the cut first shows them. An id
 *  the project no longer lists is left out. */
export function importableAssets(timeline: Timeline, assets: readonly Asset[]): ImportAsset[] {
	const known = new Map(assets.map((a) => [a.id, a]));
	const seen = new Map<string, { first: number; clips: number; order: number }>();
	for (const clip of renderedClips(timeline)) {
		if (!known.has(clip.asset_id)) continue;
		const s = seen.get(clip.asset_id);
		if (s) {
			s.clips += 1;
			s.first = Math.min(s.first, clip.timeline_start);
		} else {
			seen.set(clip.asset_id, { first: clip.timeline_start, clips: 1, order: seen.size });
		}
	}
	const ordered = [...seen].sort(([, a], [, b]) => a.first - b.first || a.order - b.order);
	const nameCount = new Map<string, number>();
	for (const [id] of ordered) {
		const name = known.get(id)!.name;
		nameCount.set(name, (nameCount.get(name) ?? 0) + 1);
	}
	return ordered.map(([id, s]) => {
		const a = known.get(id)!;
		const folder = (nameCount.get(a.name) ?? 0) > 1 ? parentName(a.path) : '';
		return { id, name: a.name, label: folder ? `${a.name} · ${folder}` : a.name, clips: s.clips };
	});
}

/** Which offered asset a source-timed import is about: the one already chosen if
 *  the cut still shows it, else the first of `hints` (the selected clip's, say)
 *  that is offered, else the first offered. `null` only when nothing is offered. */
export function pickImportAsset(
	offered: readonly ImportAsset[],
	chosen: string | null,
	hints: readonly (string | null | undefined)[] = []
): string | null {
	if (chosen && offered.some((a) => a.id === chosen)) return chosen;
	for (const h of hints) if (h && offered.some((a) => a.id === h)) return h;
	return offered[0]?.id ?? null;
}

// ---- the choice and the request ----------------------------------------------

/** What an import is timed to, once the options are settled. */
export interface ImportChoice {
	base: CaptionTimeBase;
	/** The asset a `source` base is about; `null` for `timeline`. */
	assetId: string | null;
}

/** Imports timed to the finished cut, the default. */
export const TIMELINE_CHOICE: ImportChoice = { base: 'timeline', assetId: null };

/** The choice the options amount to. A `source` base with nothing to time to
 *  falls back to `timeline` rather than asking for a request that cannot work. */
export function resolveChoice(
	base: CaptionTimeBase,
	chosen: string | null,
	offered: readonly ImportAsset[],
	hints: readonly (string | null | undefined)[] = []
): ImportChoice {
	if (base !== 'source') return TIMELINE_CHOICE;
	const assetId = pickImportAsset(offered, chosen, hints);
	return assetId ? { base: 'source', assetId } : TIMELINE_CHOICE;
}

/** `max_words` / `max_chars` that keep a whole cue as one caption. A cue is capped
 *  at `MAX_CUE_CHARS` characters and so at fewer words than this, which is why
 *  these can never split one. */
export const KEEP_LINES: Required<Pick<CaptionOptions, 'max_words' | 'max_chars'>> = {
	max_words: MAX_CUE_CHARS,
	max_chars: MAX_CUE_CHARS
};

/** Whether "Keep the file's lines" is on when the user has not touched it.
 *
 *  A subtitle file is already timed and broken into lines by whoever made it, so
 *  re-chunking it into the generator's short lines mostly throws that away — on
 *  by default. But a kept cue is one caption, and the engine shrinks a caption to
 *  fit the frame's *width*: a two-line cue is ~80 characters on one line, which is
 *  a legible 60% size at 16:9 and a 19-pixel smear at 9:16 (or 1:1). So it is on
 *  for landscape and unframed projects and off for square and tall ones, where the
 *  generator's short lines are the readable choice. Touching the checkbox settles
 *  it either way. */
export function keepLinesDefault(format: Pick<Delivery, 'width' | 'height'> | null | undefined): boolean {
	return !format || !format.height || format.width / format.height > 1;
}

/** Whether the import sends `KEEP_LINES`: `choice` (`null` = untouched, follow
 *  the frame) — but never in Word punch, which shows one word at a time and has
 *  no lines to keep (sending the override would defeat the look). */
export function keepLinesOn(
	choice: boolean | null,
	style: CaptionStyle,
	format: Pick<Delivery, 'width' | 'height'> | null | undefined
): boolean {
	return style === 'lines' && (choice ?? keepLinesDefault(format));
}

/** A typed time offset as the number of seconds to send: a number field is `null`
 *  when emptied and may hold anything, so a non-number is no shift and the rest is
 *  held within what the engine accepts. */
export function normalizeOffset(value: number | null | undefined): number {
	if (typeof value !== 'number' || !Number.isFinite(value)) return 0;
	return Math.min(Math.max(value, -MAX_CAPTION_OFFSET), MAX_CAPTION_OFFSET);
}

/** What the options row adds to the timing and the look. */
export interface ImportExtras {
	/** Keep each cue as one caption (ignored in Word punch). */
	keepLines?: boolean;
	/** Seconds added to every cue. */
	offset?: number | null;
}

/** The request `import_captions` takes: the timing, in the caption look the
 *  controls are set to, shifted by the offset when there is one. */
export function importRequest(choice: ImportChoice, style: CaptionStyle, extras: ImportExtras = {}): CaptionImportRequest {
	const options: CaptionOptions = { style, ...(extras.keepLines && style === 'lines' ? KEEP_LINES : {}) };
	const offset = normalizeOffset(extras.offset);
	const timing: CaptionImportRequest =
		choice.base === 'source' && choice.assetId
			? { base: 'source', assetId: choice.assetId, options }
			: { base: 'timeline', options };
	return offset === 0 ? timing : { ...timing, offset };
}

/** The two timings, as the options row names them. */
export const IMPORT_BASES: { id: CaptionTimeBase; label: string }[] = [
	{ id: 'timeline', label: 'Timed to the cut' },
	{ id: 'source', label: 'Timed to a source clip' }
];

/** One line saying what the chosen timing means for the file's times. */
export function baseHint(choice: ImportChoice, assetLabel?: string): string {
	if (choice.base === 'source') {
		return (
			`The file times ${assetLabel ?? 'that clip'}'s own footage, like a transcript. ` +
			'Each line follows the footage through your cuts; lines on footage you cut out drop away.'
		);
	}
	return "The file's times are the finished cut's, like a subtitle track made for this edit.";
}

/** Said in place of `baseHint` when the cut has nothing to time a file to. */
export const NO_SOURCE_HINT = 'Timing a file to a clip needs a clip on the timeline that shows.';

// ---- the titles-lane menu ----------------------------------------------------

/** How many source clips the lane menu lists before pointing at the controls. */
export const MENU_ASSET_LIMIT = 3;
/** The longest a clip name gets in a menu label. */
const MENU_NAME = 26;

export interface ImportMenuEntry {
	label: string;
	choice: ImportChoice;
}

/** The lane menu's import entries: the cut-timed import first, then one per clip
 *  the cut shows (up to `MENU_ASSET_LIMIT`; `more` counts the rest, which the
 *  Titles controls' picker still reaches). */
export function importMenuEntries(offered: readonly ImportAsset[]): { entries: ImportMenuEntry[]; more: number } {
	const entries: ImportMenuEntry[] = [{ label: 'Import captions…', choice: TIMELINE_CHOICE }];
	for (const a of offered.slice(0, MENU_ASSET_LIMIT)) {
		entries.push({
			label: `Import captions timed to ${shorten(a.label, MENU_NAME)}…`,
			choice: { base: 'source', assetId: a.id }
		});
	}
	return { entries, more: Math.max(offered.length - MENU_ASSET_LIMIT, 0) };
}

// ---- words around the import -------------------------------------------------

/** Dialog title of the replace confirmation. */
export const IMPORT_CONFIRM_TITLE = 'Import captions';

/** How many captions on the cut an import (or a recaption) would replace: the
 *  generated ones, which includes an earlier import — captions are one lane, and
 *  an imported set is the caption set. */
export function generatedCount(overlays: readonly TextOverlay[] | undefined): number {
	return (overlays ?? []).filter((o) => o.generated).length;
}

/** Dialog title of the confirmation before captioning the cut from transcripts
 *  replaces an existing set. */
export const CAPTION_CONFIRM_TITLE = 'Caption the cut';

/** The question asked before captions are written over ones already there — an
 *  import (`fileName` names what is coming in) or a recaption from transcripts. */
export function replaceConfirm(existing: number, fileName?: string): string {
	const what = existing === 1 ? 'the caption' : `the ${existing} captions`;
	const from = fileName ? ` with those in ${fileName}` : '';
	return `Replace ${what} already on the cut${from}? Titles you added by hand are kept.`;
}

/** Whether the toast after an import is good news or has something to read: any
 *  cue that did not make it onto the cut (outside it, too short, overlapping,
 *  unreadable) makes it a warning. */
export function importTone(s: CaptionImportSummary): 'success' | 'warning' {
	return s.dropped_outside + s.dropped_short + s.dropped_overlap + s.skipped_lines > 0 ? 'warning' : 'success';
}

/** Tooltip of the first-time caption button. */
export const CAPTION_HINT = 'Caption the cut from the transcripts of the clips on it';

/** Recaption writes the set again from transcripts, and a set that came from a
 *  subtitle file is indistinguishable from one that was generated — so it says
 *  that it replaces either. */
export function recaptionHint(count: number): string {
	return (
		`Caption the cut again from the transcripts of its clips. Replaces the ${plural(count, 'caption')} on the cut, ` +
		'including any imported from a subtitle file.'
	);
}

/** Clear removes the generated set, an imported one included. */
export function clearHint(count: number): string {
	const what = count === 1 ? 'the generated or imported caption' : `the ${count} generated or imported captions`;
	return `Remove ${what}. Titles you added by hand stay.`;
}

// ---- the options row's two extra fields ---------------------------------------

/** What "Keep the file's lines" does, said under the checkbox. */
export function keepLinesHint(style: CaptionStyle, on: boolean, format: Pick<Delivery, 'width' | 'height'> | null | undefined): string {
	if (style !== 'lines') return 'Word punch shows one word at a time, so there are no lines to keep.';
	if (on) return 'Each cue stays one caption, as it is timed in the file. A long one is drawn smaller to fit the frame.';
	const tall = !keepLinesDefault(format);
	return tall
		? 'Cues are split into short lines for this tall frame; a long line kept whole would be drawn too small to read.'
		: 'Cues are split into short lines in the caption look above, retimed by their length.';
}

/** What the offset field means, said under it. */
export const OFFSET_HINT =
	'Seconds added to every time in the file; negative moves it earlier. A broadcast file that starts at 01:00:00 wants -3600.';
