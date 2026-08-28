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
 *  speech backend reports word timings. */
export function timeChunks(
	chunks: string[],
	start: number,
	end: number,
	min = MIN_CAPTION
): { start: number; end: number; text: string }[] {
	let lines = [...chunks];
	const duration = Math.max(end - start, 0);
	for (;;) {
		const weights = lines.map((c) => Math.max(c.length, 1));
		const total = weights.reduce((a, b) => a + b, 0);
		const timed: { start: number; end: number; text: string }[] = [];
		let at = start;
		lines.forEach((text, i) => {
			const share = total > 0 ? weights[i] / total : 1;
			const to = i + 1 === lines.length ? end : at + duration * share;
			timed.push({ start: at, end: to, text });
			at = to;
		});
		if (lines.length < 2) return timed;
		const short = timed.findIndex((t) => t.end - t.start < min);
		if (short < 0) return timed;
		const mergeBack =
			short > 0 && (short + 1 === lines.length || lines[short - 1].length <= lines[short + 1].length);
		const into = mergeBack ? short - 1 : short;
		lines = [
			...lines.slice(0, into),
			`${lines[into]} ${lines[into + 1]}`,
			...lines.slice(into + 2)
		];
	}
}

/** Caption the cut: project each transcript segment through the clips that
 *  actually show its footage. Mirrors `Timeline::captions`. */
export function captionsForTimeline(
	timeline: Timeline,
	transcripts: Record<string, TranscriptSegment[]>,
	opts: CaptionOpts = CAPTION_DEFAULTS
): Omit<TextOverlay, 'id'>[] {
	const fmt = timeline.format;
	const aspect = fmt && fmt.height ? fmt.width / fmt.height : DEFAULT_CAPTION_ASPECT;
	const soloed = new Set(timeline.tracks.filter((t) => t.solo).map((t) => t.kind));
	const lines: { start: number; end: number; text: string }[] = [];
	for (const track of timeline.tracks) {
		if (track.muted || (soloed.has(track.kind) && !track.solo)) continue;
		for (const clip of track.clips) {
			if (clip.enabled === false) continue;
			const segments = transcripts[clip.asset_id];
			if (!segments) continue;
			const visibleStart = clip.timeline_start;
			const visibleEnd = clip.timeline_start + clipDuration(clip);
			for (const seg of segments) {
				const text = seg.text.trim();
				if (!text || seg.end <= seg.start || !coversSource(clip, seg.start, seg.end)) continue;
				// Chunk over the segment's whole projected span, then clip each
				// line — so a sentence cut in half captions only the surviving half.
				const a = sourceToTimeline(clip, seg.start);
				const b = sourceToTimeline(clip, seg.end);
				for (const line of timeChunks(chunkWords(text, opts), Math.min(a, b), Math.max(a, b), opts.min_line)) {
					const start = Math.max(line.start, visibleStart);
					const end = Math.min(line.end, visibleEnd);
					if (end - start < opts.min_visible) continue;
					lines.push({ start, end, text: line.text });
				}
			}
		}
	}
	lines.sort((x, y) => x.start - y.start || x.text.localeCompare(y.text));
	const deduped = lines.filter(
		(l, i) => i === 0 || l.text !== lines[i - 1].text || Math.abs(l.start - lines[i - 1].start) >= 1e-3
	);
	// Captions are one lane of text at one screen position, so two at once is two
	// unreadable ones. First line in wins the slot; the next starts where it ends,
	// or is dropped if nothing readable is left of it.
	const placed: typeof deduped = [];
	for (const l of deduped) {
		const start = placed.length ? Math.max(l.start, placed[placed.length - 1].end) : l.start;
		if (l.end - start < opts.min_visible) continue;
		placed.push({ ...l, start });
	}
	return placed.map((l) => ({
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
	}));
}
