/** Reading subtitle files into captions, mirrored from `kerf_core::captions_import`
 *  and `Project::import_captions` so the browser dev harness imports exactly what
 *  the backend would.
 *
 *  Two formats, read tolerantly: SubRip (BOM, CRLF, odd indices, `,` or `.`
 *  milliseconds, markup and `{\an8}` overrides) and ASS / SSA (the `[Events]`
 *  `Format:` line decides the columns; override blocks, drawings and `\N` / `\h`
 *  are resolved). What cannot be read is skipped and counted, never fatal.
 *  Placing the cues on the cut is `placeCues` in `captions.ts`, which shares its
 *  chunking, timing and fitting with transcript captioning. Kerf-core stays the
 *  authority; this is the same arrangement as `captions.ts`.
 */

import { clipDuration, placeCues, renderedClips, resolveCaptions, type CaptionBase } from './captions';
import { formatTime } from './diff';
import type {
	CaptionFormat,
	CaptionImportSummary,
	CaptionOptions,
	CaptionTimeBase,
	TextOverlay,
	Timeline,
	TranscriptSegment
} from './types';

/** The largest subtitle file Kerf reads. */
export const MAX_CAPTION_FILE_BYTES = 5 * 1024 * 1024;
/** The most cues one import may carry. */
export const MAX_CAPTION_CUES = 10_000;
/** The longest one cue's text may be (characters). */
export const MAX_CUE_CHARS = 2_000;

/** One cue of a subtitle file, in the file's own time (seconds). */
export interface ImportedCue {
	start: number;
	end: number;
	text: string;
}

export interface ParsedCaptions {
	/** Usable cues, ordered by start time. */
	cues: ImportedCue[];
	/** Entries that could not be used (unreadable time, no text, no duration, a
	 *  stray line outside any cue, a malformed ASS `Dialogue:` line). */
	skipped: number;
}

const chars = (s: string) => [...s].length;

// ---- timestamps ------------------------------------------------------------

/** `[h:]m:s` with an optional `,` or `.` fraction, in seconds. The fraction is a
 *  decimal fraction whatever its width (`,5` and `,500` are both half a second).
 *  Negative times, non-numeric text and absurdly long fields are not times. */
export function parseTimestamp(raw: string): number | null {
	const token = raw.trim();
	if (!token || token.length > 40) return null;
	const sep = token.search(/[,.]/);
	const clock = sep < 0 ? token : token.slice(0, sep);
	const frac = sep < 0 ? '' : token.slice(sep + 1);
	if (!/^\d*$/.test(frac)) return null;
	const parts = clock.split(':');
	if (parts.length < 2 || parts.length > 3) return null;
	let seconds = 0;
	for (const part of parts) {
		if (part.length === 0 || part.length > 6 || !/^\d+$/.test(part)) return null;
		seconds = seconds * 60 + Number(part);
	}
	if (frac) seconds += Number(`0.${frac.slice(0, 9)}`);
	return Number.isFinite(seconds) ? seconds : null;
}

// ---- text cleanup ----------------------------------------------------------

const MARKUP_TAGS = new Set([
	'a', 'b', 'br', 'c', 'div', 'em', 'font', 'i', 'lang', 'p', 'rp', 'rt', 'ruby', 's', 'span', 'strike', 'strong', 'u', 'v'
]);

/** Remove `<i>` / `</font>`-style markup; `<br>` becomes a line break. Angle
 *  brackets that are not a known tag (`<laughs>`, `<3`, `a < b`) are kept. */
function stripMarkup(line: string): string {
	let out = '';
	let rest = line;
	for (;;) {
		const open = rest.indexOf('<');
		if (open < 0) break;
		out += rest.slice(0, open);
		const after = rest.slice(open + 1);
		const close = after.indexOf('>');
		const inner = close >= 0 && close <= 256 ? after.slice(0, close) : null;
		const name =
			inner === null ? '' : (inner.replace(/^\/+/, '').split(/[^A-Za-z0-9]/)[0] ?? '').toLowerCase();
		if (inner !== null && !inner.includes('<') && MARKUP_TAGS.has(name)) {
			if (name === 'br') out += '\n';
			rest = after.slice(close + 1);
		} else {
			out += '<';
			rest = after;
		}
	}
	return out + rest;
}

/** Remove `{…}` blocks — with `onlyOverrides`, only those that start `{\`. An
 *  unterminated `{` is literal text. */
function stripBraces(line: string, onlyOverrides: boolean): string {
	let out = '';
	let rest = line;
	for (;;) {
		const open = rest.indexOf('{');
		if (open < 0) break;
		out += rest.slice(0, open);
		const after = rest.slice(open + 1);
		const close = after.indexOf('}');
		if (close >= 0 && (!onlyOverrides || after.startsWith('\\'))) {
			rest = after.slice(close + 1);
		} else {
			out += '{';
			rest = after;
		}
	}
	return out + rest;
}

const tidy = (line: string) => line.split(/\s+/).filter(Boolean).join(' ');

/** A cue's lines cleaned and joined with `\n`; empty when nothing is left. */
function joinLines(lines: string[]): string {
	return lines
		.flatMap((l) => l.split('\n'))
		.map(tidy)
		.filter((l) => l.length > 0)
		.join('\n');
}

const normalizeNewlines = (text: string) =>
	text.replace(/^﻿+/, '').replace(/\r\n/g, '\n').replace(/\r/g, '\n');

// ---- SubRip ----------------------------------------------------------------

const isIndex = (line: string) => /^\d+$/.test(line);

function parseSrtTiming(line: string): [number, number] | null {
	const at = line.indexOf('-->');
	if (at < 0) return null;
	// `--->` is a typo that turns up; the extra dash lands on the left.
	const start = parseTimestamp(line.slice(0, at).trim().replace(/-+$/, ''));
	const end = parseTimestamp(line.slice(at + 3).trim().split(/\s+/)[0] ?? '');
	return start === null || end === null ? null : [start, end];
}

/** Read SubRip text. Never throws: what is unusable is counted in `skipped`.
 *  A cue runs from its `-->` line to the next blank line *or* the next timing
 *  line, handing back the index line that then ends the previous cue. */
export function parseSrt(input: string): ParsedCaptions {
	const out: ParsedCaptions = { cues: [], skipped: 0 };
	type Block = { time: [number, number] | null; body: string[] };
	const finish = (block: Block) => {
		if (!block.time) {
			out.skipped += 1;
			return;
		}
		const [start, end] = block.time;
		const text = joinLines(block.body.map((l) => stripBraces(stripMarkup(l), true)));
		if (!text || end <= start || chars(text) > MAX_CUE_CHARS) {
			out.skipped += 1;
			return;
		}
		out.cues.push({ start, end, text });
	};

	let current: Block | null = null;
	for (const raw of normalizeNewlines(input).split('\n')) {
		const line = raw.trim();
		if (line.includes('-->')) {
			const time = parseSrtTiming(line);
			// An unparsable `-->` line inside a good cue is that cue's text.
			const insideGoodCue = current !== null && current.time !== null;
			if (time || !insideGoodCue) {
				if (current) {
					if (current.body.length && isIndex(current.body[current.body.length - 1])) current.body.pop();
					finish(current);
				}
				current = { time, body: [] };
				continue;
			}
		}
		if (!line) {
			if (current) finish(current);
			current = null;
			continue;
		}
		if (current) current.body.push(line);
		else if (!isIndex(line)) out.skipped += 1;
	}
	if (current) finish(current);
	out.cues.sort((a, b) => a.start - b.start);
	return out;
}

// ---- ASS / SSA -------------------------------------------------------------

interface EventColumns {
	fields: number;
	start: number;
	end: number;
	text: number;
}

/** What a file with no `Format:` line is read as (ASS and SSA v4 agree on where
 *  the three columns we need are). */
const DEFAULT_COLUMNS: EventColumns = { fields: 10, start: 1, end: 2, text: 9 };

function columnsFromFormat(value: string): EventColumns {
	const names = value.split(',').map((n) => n.trim().toLowerCase());
	const start = names.indexOf('start');
	const end = names.indexOf('end');
	const text = names.indexOf('text');
	if (start < 0 || end < 0 || text < 0) {
		throw new Error('the [Events] Format line of this ASS file has no Start, End and Text column');
	}
	return { fields: names.length, start, end, text };
}

/** `splitn(n, ',')`: at most `n` pieces, the last keeping its commas. */
function splitN(value: string, n: number): string[] {
	const parts = value.split(',');
	return parts.length <= n ? parts : [...parts.slice(0, n - 1), parts.slice(n - 1).join(',')];
}

/** The visible text of an ASS `Text` field: override blocks and vector drawings
 *  removed, `\N` / `\n` line breaks, `\h` a space. */
function cleanAssText(raw: string): string {
	let text = '';
	let drawing = false;
	let rest = raw;
	for (;;) {
		const open = rest.indexOf('{');
		if (open < 0) break;
		if (!drawing) text += rest.slice(0, open);
		const after = rest.slice(open + 1);
		const close = after.indexOf('}');
		if (close < 0) {
			// Unterminated: libass shows it, so it is text.
			if (!drawing) text += rest.slice(open);
			rest = '';
			break;
		}
		// `\p1` starts a vector drawing and `\p0` ends it; `\pos` / `\pbo` are not.
		for (const tag of after.slice(0, close).split('\\').slice(1)) {
			const level = tag.startsWith('p') ? tag.slice(1) : '';
			if (level && /^\d+$/.test(level)) drawing = level.replace(/^0+/, '') !== '';
		}
		rest = after.slice(close + 1);
	}
	if (!drawing) text += rest;
	return joinLines([text.replace(/\\[Nn]/g, '\n').replace(/\\h/g, ' ')]);
}

/** Read ASS / SSA text. Only `Dialogue:` lines in `[Events]` (or a headerless run
 *  of them) are cues; `Comment:`, styles and other events are ignored without
 *  being counted. Throws only when the file's own `Format:` line leaves out
 *  Start, End or Text. */
export function parseAss(input: string): ParsedCaptions {
	const out: ParsedCaptions = { cues: [], skipped: 0 };
	let section: string | null = null;
	let columns = DEFAULT_COLUMNS;
	for (const raw of normalizeNewlines(input).split('\n')) {
		const line = raw.trim();
		if (!line || line.startsWith(';') || line.startsWith('!')) continue;
		if (line.startsWith('[') && line.endsWith(']')) {
			section = line.slice(1, -1).trim().toLowerCase();
			continue;
		}
		// `Format:` also heads the styles section; only the events one counts.
		if (section !== null && section !== 'events') continue;
		const colon = line.indexOf(':');
		if (colon < 0) continue;
		const key = line.slice(0, colon).trim().toLowerCase();
		const value = line.slice(colon + 1);
		if (key === 'format') {
			columns = columnsFromFormat(value);
		} else if (key === 'dialogue') {
			const fields = splitN(value.trimStart(), columns.fields);
			if (fields.length < columns.fields) {
				out.skipped += 1;
				continue;
			}
			const start = parseTimestamp(fields[columns.start]);
			const end = parseTimestamp(fields[columns.end]);
			const text = cleanAssText(fields[columns.text]);
			if (start !== null && end !== null && end > start && text && chars(text) <= MAX_CUE_CHARS) {
				out.cues.push({ start, end, text });
			} else {
				out.skipped += 1;
			}
		}
	}
	out.cues.sort((a, b) => a.start - b.start);
	return out;
}

// ---- entry points ----------------------------------------------------------

/** Guess a format from the text: an ASS file always has a `[Script Info]` or
 *  `[Events]` section; failing that, an SRT `-->` wins over a bare `Dialogue:`. */
export function detectFormat(text: string): CaptionFormat {
	let arrow = false;
	let dialogue = false;
	for (const raw of text.replace(/^﻿+/, '').split(/\r\n|\r|\n/)) {
		const line = raw.trim().toLowerCase();
		if (line === '[script info]' || line === '[events]') return 'ass';
		arrow ||= line.includes('-->');
		dialogue ||= line.startsWith('dialogue:');
	}
	return dialogue && !arrow ? 'ass' : 'srt';
}

/** A format by name or extension (`srt`, `ass`, `ssa`; any case, optional dot). */
export function parseFormat(name: string): CaptionFormat | null {
	switch (name.trim().replace(/^\./, '').toLowerCase()) {
		case 'srt':
		case 'subrip':
			return 'srt';
		case 'ass':
		case 'ssa':
			return 'ass';
		default:
			return null;
	}
}

/** The refusal for a file over `MAX_CAPTION_FILE_BYTES`, shared by the parser and
 *  the browser harness's file picker (which checks before reading it all). */
export const fileTooLarge = () =>
	new Error(`subtitle file is larger than ${MAX_CAPTION_FILE_BYTES >> 20} MiB — not a subtitle file`);

/** Read subtitle text in `format`, or in whichever format it looks like. Throws on
 *  text over the size cap, more than `MAX_CAPTION_CUES` cues, or an unusable ASS
 *  `Format:` line; an unreadable entry is never an error. */
export function parseCaptions(
	text: string,
	format?: CaptionFormat | null
): { format: CaptionFormat; parsed: ParsedCaptions } {
	if (new TextEncoder().encode(text).length > MAX_CAPTION_FILE_BYTES) throw fileTooLarge();
	const used = format ?? detectFormat(text);
	const parsed = used === 'srt' ? parseSrt(text) : parseAss(text);
	if (parsed.cues.length > MAX_CAPTION_CUES) {
		throw new Error(
			`this file has ${parsed.cues.length} cues; Kerf imports at most ${MAX_CAPTION_CUES} at a time`
		);
	}
	return { format: used, parsed };
}

/** The base a caller asked for by name plus the asset a `source` base is about.
 *  An asset alone implies `source`; neither implies `timeline`; a contradiction is
 *  an error. Mirrors `CaptionTimeBase::resolve`. */
export function resolveBase(base?: CaptionTimeBase | string | null, assetId?: string | null): CaptionBase {
	const named = (base ?? '').trim().toLowerCase();
	if (named === '' || named === 'timeline' || named === 'source') {
		if (assetId) {
			if (named === 'timeline') {
				throw new Error(
					'asset_id only applies to base "source"; cues in timeline time are not tied to an asset'
				);
			}
			return { kind: 'source', assetId };
		}
		if (named === 'source') {
			throw new Error('base "source" needs asset_id: the asset whose footage the cue times belong to');
		}
		return { kind: 'timeline' };
	}
	throw new Error(`unknown caption time base "${named}"; expected "timeline" or "source"`);
}

/** What the harness needs to know about the project to import. */
export interface ImportEnv {
	/** Whether an asset id exists (a `source` base must name a real one). */
	assetKnown: (assetId: string) => boolean;
}

/** `Project::import_captions` over a plain timeline: parse, place, and return the
 *  overlays the cut should hold afterwards (typed titles kept, the previous
 *  generated / imported set replaced) with the summary. Throws, changing
 *  nothing, when the text holds no cues or none of them reaches the cut. */
export function importCaptionsInto(
	timeline: Timeline,
	text: string,
	req: { format?: CaptionFormat | null; base?: CaptionBase; options?: CaptionOptions },
	env: ImportEnv
): { overlays: Omit<TextOverlay, 'id'>[]; kept: TextOverlay[]; summary: CaptionImportSummary } {
	const { format, parsed } = parseCaptions(text, req.format);
	if (parsed.cues.length === 0) {
		throw new Error(
			`no captions found in the ${format.toUpperCase()} text (${parsed.skipped} entries could not be read)`
		);
	}
	const base = req.base ?? { kind: 'timeline' };
	if (base.kind === 'source' && !env.assetKnown(base.assetId)) {
		throw new Error(`asset not found: ${base.assetId}`);
	}
	if (base.kind === 'source' && !timeline.tracks.some((t) => t.clips.some((c) => c.asset_id === base.assetId))) {
		throw new Error(
			`asset ${base.assetId} is not used by any clip in the cut; put it on the timeline first, ` +
				'or import with base "timeline" if the file already times the finished cut'
		);
	}
	const cues: TranscriptSegment[] = parsed.cues;
	const placement = placeCues(timeline, cues, base, resolveCaptions(req.options));
	if (placement.overlays.length === 0) {
		const first = parsed.cues[0].start;
		const last = parsed.cues.reduce((m, c) => Math.max(m, c.end), 0);
		if (base.kind === 'timeline') {
			let cut = 0;
			for (const c of renderedClips(timeline)) cut = Math.max(cut, c.timeline_start + clipDuration(c));
			throw new Error(
				`none of the ${cues.length} cues fall inside the cut: they run ${formatTime(first)} to ${formatTime(last)} and the cut is ${formatTime(cut)} long`
			);
		}
		throw new Error(
			`none of the ${cues.length} cues land on footage this asset shows in the cut: they run ${formatTime(first)} to ${formatTime(last)} of the source (a muted track or a disabled clip does not count)`
		);
	}
	const existing = timeline.overlays ?? [];
	const kept = existing.filter((o) => !o.generated);
	return {
		overlays: placement.overlays,
		kept,
		summary: {
			format,
			cues: cues.length,
			placed: placement.placed,
			captions: placement.overlays.length,
			skipped_lines: parsed.skipped,
			dropped_outside: placement.droppedOutside,
			dropped_overlap: placement.droppedOverlap,
			replaced: existing.length - kept.length
		}
	};
}

/** The one-line account of an import a toast can show: what landed, and — only
 *  when it happened — what did not. */
export function describeImport(s: CaptionImportSummary): string {
	const lines = s.captions === 1 ? '1 caption' : `${s.captions} captions`;
	const parts = [`Imported ${lines} from ${s.placed} of ${s.cues} cues`];
	const aside: string[] = [];
	if (s.dropped_outside) aside.push(`${s.dropped_outside} outside the cut`);
	if (s.dropped_overlap) aside.push(`${s.dropped_overlap} overlapping`);
	if (s.skipped_lines) aside.push(`${s.skipped_lines} unreadable`);
	if (aside.length) parts.push(`(${aside.join(', ')})`);
	if (s.replaced) parts.push(`· replaced ${s.replaced}`);
	return parts.join(' ');
}
