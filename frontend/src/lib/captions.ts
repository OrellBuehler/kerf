/** The caption arithmetic, mirrored from `kerf_core::model` so the browser dev
 *  harness produces the same captions the backend would.
 *
 *  Only the *timing* math lives here — projecting a transcript's source time
 *  onto timeline time through each clip's trim / speed / reverse, and splitting
 *  a sentence into readable lines. That is the whole feature: which words a
 *  caption carries and when it appears. Kerf-core stays the authority; this is
 *  the same arrangement as `platforms.ts` and `smart-crop.ts`.
 */

import type { CaptionOptions, CaptionStyle, Clip, TextOverlay, Timeline, TranscriptSegment } from './types';

/** Shortest a generated line stays on screen; below this it reads as a flicker. */
export const MIN_CAPTION = 0.45;
/** How much of a line has to survive a cut for it to be kept. */
export const MIN_CAPTION_VISIBLE = 0.15;
/** The same two floors where a line *is* one word: held to `MIN_CAPTION` every
 *  short word would merge into a neighbour and word punch would collapse back
 *  into lines. */
export const MIN_WORD_CAPTION = 0.12;
export const MIN_WORD_VISIBLE = 0.06;

/** A style's numbers with any override applied — what captioning works from. */
export interface CaptionOpts {
	max_words: number;
	max_chars: number;
	pos_y: number;
	size: number;
	bold: boolean;
	min_line: number;
	min_visible: number;
}

/** The two looks. A subtitle line is read; a punched word is watched — so the
 *  word count, the size, the position and the flicker floors move together. */
export const CAPTION_STYLES: Record<CaptionStyle, CaptionOpts> = {
	lines: {
		max_words: 4,
		max_chars: 28,
		pos_y: 0.88,
		size: 0.05,
		bold: false,
		min_line: MIN_CAPTION,
		min_visible: MIN_CAPTION_VISIBLE
	},
	word_punch: {
		max_words: 1,
		max_chars: 28,
		pos_y: 0.72,
		size: 0.11,
		bold: true,
		min_line: MIN_WORD_CAPTION,
		min_visible: MIN_WORD_VISIBLE
	}
};

export const CAPTION_DEFAULTS: CaptionOpts = CAPTION_STYLES.lines;

/** The style's numbers with any usable override applied over them. Mirrors
 *  `CaptionOptions::resolve`: omitted fields follow the style, so asking for
 *  `word_punch` alone gets the whole look rather than one word left at subtitle
 *  size in the subtitle position. */
export function resolveCaptions(opts?: CaptionOptions): CaptionOpts {
	const base = CAPTION_STYLES[opts?.style ?? 'lines'] ?? CAPTION_DEFAULTS;
	const num = (v: number | undefined, fallback: number, lo: number, hi: number) =>
		typeof v === 'number' && Number.isFinite(v) ? Math.min(Math.max(v, lo), hi) : fallback;
	return {
		...base,
		max_words: typeof opts?.max_words === 'number' ? Math.max(opts.max_words, 1) : base.max_words,
		max_chars: typeof opts?.max_chars === 'number' ? Math.max(opts.max_chars, 1) : base.max_chars,
		pos_y: num(opts?.pos_y, base.pos_y, 0, 1),
		size: num(opts?.size, base.size, 0.005, 0.5)
	};
}

/** Roughly how wide one character is as a fraction of the font size, measured
 *  off `drawtext`'s default face; 0.6 sits above the ~0.53 that long text — the
 *  only kind that reaches the cap — averages. */
const CHAR_ADVANCE = 0.6;
/** How much of the frame width a caption may take. */
const CAPTION_WIDTH = 0.9;
/** The frame captions assume when the project has not picked one: wide enough
 *  that the fit never binds, so an unframed project captions as it always did. */
const DEFAULT_CAPTION_ASPECT = 16 / 9;

/** Shrink a caption's size until its text fits across a frame of `aspect`.
 *  `drawtext` neither wraps nor scales, and a 9:16 frame is barely half as wide
 *  as it is tall — so the social shape is exactly where a long word runs off
 *  both edges. Mirrors `fit_size`. */
export function fitSize(text: string, size: number, aspect: number): number {
	return Math.min(size, (CAPTION_WIDTH * aspect) / (Math.max([...text].length, 1) * CHAR_ADVANCE));
}

const MIN_SPEED = 0.01;
const speedMag = (c: Clip) => Math.max(Math.abs(c.speed ?? 1), MIN_SPEED);
const reversed = (c: Clip) => (c.speed ?? 1) < 0;

export function clipDuration(c: Clip): number {
	return Math.max(c.source_out - c.source_in, 0) / speedMag(c);
}

/** Where a source timestamp of this clip lands on the timeline. */
export function sourceToTimeline(c: Clip, source: number): number {
	const offset = reversed(c) ? c.source_out - source : source - c.source_in;
	return c.timeline_start + offset / speedMag(c);
}

/** Whether any of the source span `[from, to)` is inside the clip's window. */
export function coversSource(c: Clip, from: number, to: number): boolean {
	const lo = Math.min(from, to);
	const hi = Math.max(from, to);
	return Math.min(hi, c.source_out) > Math.max(lo, c.source_in);
}

/** Break a line into caption-sized groups of words; always at least one word,
 *  so a single word longer than `max_chars` is its own line rather than cut. */
export function chunkWords(text: string, opts: CaptionOpts): string[] {
	const out: string[] = [];
	let current = '';
	let words = 0;
	for (const word of text.split(/\s+/).filter(Boolean)) {
		const extra = current ? word.length + 1 : word.length;
		const fits = words < opts.max_words && current.length + extra <= opts.max_chars;
		if (current && !fits) {
			out.push(current);
			current = '';
			words = 0;
		}
		current = current ? `${current} ${word}` : word;
		words += 1;
	}
	if (current) out.push(current);
	return out;
}

/** Spread a span across lines by character share, merging away any line too
 *  short to read. Character share is the approximation available: neither
 *  speech backend reports word timings.
 *
 *  The merge is repeated — join the first too-short line to its shorter
 *  neighbour, re-time, look again — and what that costs is the *scan*: weights
 *  and their total are kept as they change (a merge adds exactly the joining
 *  space) and a scan stops at the first short line, so a pass allocates nothing.
 *  Mirrors `time_chunks`, which the same sweep pins bit-for-bit. */
export function timeChunks(
	chunks: string[],
	start: number,
	end: number,
	min = MIN_CAPTION
): { start: number; end: number; text: string }[] {
	const lines = [...chunks];
	const duration = Math.max(end - start, 0);
	const weights = lines.map((c) => Math.max(c.length, 1));
	let total = weights.reduce((a, b) => a + b, 0);
	const endOf = (at: number, i: number) =>
		i + 1 === lines.length ? end : at + duration * (total > 0 ? weights[i] / total : 1);
	// A whole segment shorter than `min` is one line, not a merge loop.
	while (lines.length >= 2) {
		let at = start;
		let short = -1;
		for (let i = 0; i < lines.length; i++) {
			const to = endOf(at, i);
			if (to - at < min) {
				short = i;
				break;
			}
			at = to;
		}
		if (short < 0) break;
		const mergeBack =
			short > 0 && (short + 1 === lines.length || lines[short - 1].length <= lines[short + 1].length);
		const into = mergeBack ? short - 1 : short;
		const moved = weights[into + 1];
		lines.splice(into, 2, `${lines[into]} ${lines[into + 1]}`);
		weights.splice(into, 2, weights[into] + moved + 1);
		total += 1;
	}
	const timed: { start: number; end: number; text: string }[] = [];
	let at = start;
	lines.forEach((text, i) => {
		const to = endOf(at, i);
		timed.push({ start: at, end: to, text });
		at = to;
	});
	return timed;
}

/** One caption line on its way to the screen: when, what, and which input it was
 *  cut from. `origin` lets an import say *which cue* fell outside the cut or lost
 *  its slot to another; transcript captioning never reads it. */
export interface CaptionLine {
	start: number;
	end: number;
	text: string;
	origin: number;
}

/** Chunk `text` over a span into caption lines and keep what lies inside the
 *  window. The span is chunked *whole* and each line is clipped afterwards, so a
 *  sentence cut in half captions only the half still in the cut. Mirrors
 *  `push_caption_lines`. */
export function pushCaptionLines(
	lines: CaptionLine[],
	text: string,
	span: { start: number; end: number },
	window: { start: number; end: number },
	origin: number,
	opts: CaptionOpts
) {
	for (const line of timeChunks(chunkWords(text, opts), span.start, span.end, opts.min_line)) {
		const start = Math.max(line.start, window.start);
		const end = Math.min(line.end, window.end);
		if (end - start < opts.min_visible) continue;
		lines.push({ start, end, text: line.text, origin });
	}
}

/** The clips that actually reach the render: muted and solo-shadowed tracks and
 *  disabled clips removed. Mirrors `Timeline::for_render`. */
export function renderedClips(timeline: Timeline): Clip[] {
	const soloed = new Set(timeline.tracks.filter((t) => t.solo).map((t) => t.kind));
	const out: Clip[] = [];
	for (const track of timeline.tracks) {
		if (track.muted || (soloed.has(track.kind) && !track.solo)) continue;
		for (const clip of track.clips) if (clip.enabled !== false) out.push(clip);
	}
	return out;
}

/** Project timed text in a clip's **source** time through every clip that shows
 *  its footage. `segmentsFor` says which text belongs to an asset. Mirrors
 *  `project_through_clips`. */
export function projectThroughClips(
	timeline: Timeline,
	segmentsFor: (assetId: string) => TranscriptSegment[] | undefined,
	opts: CaptionOpts
): CaptionLine[] {
	const lines: CaptionLine[] = [];
	for (const clip of renderedClips(timeline)) {
		const segments = segmentsFor(clip.asset_id);
		if (!segments) continue;
		const window = { start: clip.timeline_start, end: clip.timeline_start + clipDuration(clip) };
		segments.forEach((seg, origin) => {
			const text = seg.text.trim();
			const timed = Number.isFinite(seg.start) && Number.isFinite(seg.end) && seg.end > seg.start;
			if (!text || !timed || !coversSource(clip, seg.start, seg.end)) return;
			const a = sourceToTimeline(clip, seg.start);
			const b = sourceToTimeline(clip, seg.end);
			pushCaptionLines(lines, text, { start: Math.min(a, b), end: Math.max(a, b) }, window, origin, opts);
		});
	}
	return lines;
}

/** How lines that start at the same moment are ordered: alphabetically (all a
 *  transcript has, and what it has always done) or in the order the input gave
 *  them (an imported file's cues, in the order its author wrote them). */
export type SimultaneousLines = 'text' | 'origin';

/** Settle chunked lines into the caption lane: ordered, de-duplicated, never two
 *  on screen at once, each sized to fit the frame. Each overlay comes back with
 *  the `origin` of the line it was made from. Mirrors `settle_caption_lines`. */
export function settleCaptionLines(
	lines: CaptionLine[],
	opts: CaptionOpts,
	aspect: number,
	simultaneous: SimultaneousLines = 'text'
): { overlay: Omit<TextOverlay, 'id'>; origin: number }[] {
	const sorted = [...lines].sort((x, y) => x.start - y.start || compareText(x.text, y.text));
	// The same words can reach two clips (`extract_audio` leaves picture and
	// detached audio both on the asset); drawing one twice is drawing it bolder.
	let deduped = sorted.filter(
		(l, i) => i === 0 || l.text !== sorted[i - 1].text || Math.abs(l.start - sorted[i - 1].start) >= 1e-3
	);
	// Stable (`Array.prototype.sort` is), so everything else keeps its order.
	if (simultaneous === 'origin') deduped = [...deduped].sort((x, y) => x.start - y.start || x.origin - y.origin);
	// Captions are one lane of text at one screen position, so two at once is two
	// unreadable ones. First line in wins the slot; the next starts where it ends,
	// or is dropped if nothing readable is left of it.
	const placed: CaptionLine[] = [];
	for (const l of deduped) {
		const start = placed.length ? Math.max(l.start, placed[placed.length - 1].end) : l.start;
		if (l.end - start < opts.min_visible) continue;
		placed.push({ ...l, start });
	}
	return placed.map((l) => ({
		origin: l.origin,
		overlay: {
			text: l.text,
			start: Math.max(l.start, 0),
			end: l.end,
			pos_x: 0.5,
			pos_y: opts.pos_y,
			size: fitSize(l.text, opts.size, aspect),
			color: 'white',
			bg: 'black@0.5',
			bold: opts.bold,
			generated: true
		}
	}));
}

/** Code-point order, like Rust's `str::cmp` on UTF-8 — `localeCompare` would
 *  order "b" and "B" differently from the backend and so reorder equal-start
 *  lines. */
function compareText(a: string, b: string): number {
	return a < b ? -1 : a > b ? 1 : 0;
}

/** The frame captions are fitted to: the project's own, else 16:9. */
function captionAspect(timeline: Timeline): number {
	const fmt = timeline.format;
	return fmt && fmt.height ? fmt.width / fmt.height : DEFAULT_CAPTION_ASPECT;
}

/** Caption the cut: project each transcript segment through the clips that
 *  actually show its footage. Mirrors `Timeline::captions`. */
export function captionsForTimeline(
	timeline: Timeline,
	transcripts: Record<string, TranscriptSegment[]>,
	opts: CaptionOpts = CAPTION_DEFAULTS
): Omit<TextOverlay, 'id'>[] {
	const lines = projectThroughClips(timeline, (asset) => transcripts[asset], opts);
	return settleCaptionLines(lines, opts, captionAspect(timeline)).map((l) => l.overlay);
}

/** Which clock an imported caption file's times are on: `timeline` (a subtitle
 *  track for the finished cut) or one `source` asset's own footage. */
export type CaptionBase = { kind: 'timeline' } | { kind: 'source'; assetId: string };

/** Imported cues laid onto the cut: the overlays and an account of every cue.
 *  `placed + droppedOutside + droppedShort + droppedOverlap` is the number of
 *  cues offered. */
export interface CaptionPlacement {
	overlays: Omit<TextOverlay, 'id'>[];
	placed: number;
	/** Never met the cut: past its end or before its start, footage no clip
	 *  shows, or not a usable cue. */
	droppedOutside: number;
	/** Met the cut, but for a moment too short to read. */
	droppedShort: number;
	/** Lost their slot to another cue: captions are one lane. */
	droppedOverlap: number;
}

/** How far into an *empty* timeline imported cues may reach (seconds): a cut has
 *  an end that cues are clipped to; with nothing on the timeline yet the day is
 *  the bound. Mirrors `EMPTY_CUT_WINDOW`. */
export const EMPTY_CUT_WINDOW = 24 * 3600;

/** Lay the cues of an imported subtitle file onto the cut — `captionsForTimeline`
 *  for text that did not come from a transcript, sharing everything after the
 *  time mapping. Mirrors `Timeline::place_cues`: a `source` base *is* transcript
 *  captioning, a `timeline` base takes the times as they stand, clipped to the
 *  length the cut renders at (`EMPTY_CUT_WINDOW` when there is nothing on the
 *  timeline), and a muted track does not silence them. Simultaneous cues keep the
 *  file's order. */
export function placeCues(
	timeline: Timeline,
	cues: TranscriptSegment[],
	base: CaptionBase,
	opts: CaptionOpts = CAPTION_DEFAULTS
): CaptionPlacement {
	let cutEnd = 0;
	for (const clip of renderedClips(timeline)) cutEnd = Math.max(cutEnd, clip.timeline_start + clipDuration(clip));
	const windowEnd = cutEnd > 0 ? cutEnd : EMPTY_CUT_WINDOW;
	const usable = (cue: TranscriptSegment) =>
		cue.text.trim() !== '' && Number.isFinite(cue.start) && Number.isFinite(cue.end) && cue.end > cue.start;
	let lines: CaptionLine[];
	if (base.kind === 'source') {
		lines = projectThroughClips(timeline, (id) => (id === base.assetId ? cues : undefined), opts);
	} else {
		lines = [];
		cues.forEach((cue, origin) => {
			if (!usable(cue)) return;
			pushCaptionLines(lines, cue.text.trim(), cue, { start: 0, end: windowEnd }, origin, opts);
		});
	}
	const reached = new Set(lines.map((l) => l.origin));
	// A cue that produced nothing either never met the cut, or met it for too
	// short a moment to read: different complaints, different counts.
	const sourceClips =
		base.kind === 'source' ? renderedClips(timeline).filter((c) => c.asset_id === base.assetId) : [];
	const meetsTheCut = (cue: TranscriptSegment) =>
		usable(cue) &&
		(base.kind === 'timeline'
			? cue.end > 0 && cue.start < windowEnd
			: sourceClips.some((c) => coversSource(c, cue.start, cue.end)));
	const droppedShort = cues.filter((cue, origin) => !reached.has(origin) && meetsTheCut(cue)).length;
	const settled = settleCaptionLines(lines, opts, captionAspect(timeline), 'origin');
	const kept = new Set(settled.map((l) => l.origin));
	return {
		overlays: settled.map((l) => l.overlay),
		placed: kept.size,
		droppedOutside: cues.length - reached.size - droppedShort,
		droppedShort,
		droppedOverlap: reached.size - kept.size
	};
}
