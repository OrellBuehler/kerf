/* The bar structure of an analysed piece of music, for the Inspector — pure. `get_music_structure` (the MCP tool) gives an
 * agent the grid, every bar start and the repeating phrases; this reads the same off `MusicAnalysis` and adds what a
 * person wants to see: where the intro ends, which stretches repeat (a verse that comes back is the same *section*), and
 * a strip to draw them on beside the energy curve. The grid arithmetic is `music-fit.ts`'s (`barStart`, a mirror of
 * `BeatGrid::bar_start`). Bars are counted from the first downbeat, as the backend's phrases are. */

import { barStart, firstDownbeat, gridBarSeconds, gridBpm } from './music-fit';
import type { MusicAnalysis } from './types';

/** Bars `[from, to)`, counted from the first downbeat. */
export interface BarRange {
	from: number;
	to: number;
}

/** A stretch of music that comes back: the same `bars` bars at each of `at`. */
export interface Repeat {
	/** `A`, `B`, … in the order the first occurrence appears. */
	label: string;
	bars: number;
	at: BarRange[];
}

export interface Structure {
	bpm: number;
	beatsPerBar: number;
	barSeconds: number;
	/** Whole bars from the first downbeat. */
	bars: number;
	/** Where the first bar starts, seconds into the file (the intro before the beat grid starts). */
	introEnd: number;
	/** Length of the analysed audio. */
	duration: number;
	repeats: Repeat[];
}

const overlaps = (a: BarRange, b: BarRange) => a.from < b.to && b.from < a.to;

/** The repeating stretches, longest first and without overlap: of two phrases that cover the same bars the longer
 *  one is the section (an 8-bar phrase hides the two 4-bar halves inside it). */
export function repeatsOf(music: Pick<MusicAnalysis, 'phrases'>): Repeat[] {
	// Every `(a, bars)` start and the places it comes back to.
	const byStart = new Map<string, { bars: number; from: Set<number> }>();
	for (const p of music.phrases) {
		const key = `${p.a}:${p.bars}`;
		const entry = byStart.get(key) ?? { bars: p.bars, from: new Set<number>([p.a]) };
		entry.from.add(p.b);
		byStart.set(key, entry);
	}
	const candidates = [...byStart.values()]
		.map((e) => ({ bars: e.bars, at: [...e.from].sort((x, y) => x - y).map((from): BarRange => ({ from, to: from + e.bars })) }))
		.sort((x, y) => y.bars - x.bars || y.at.length - x.at.length || x.at[0].from - y.at[0].from);
	const taken: BarRange[] = [];
	const chosen: Omit<Repeat, 'label'>[] = [];
	for (const c of candidates) {
		if (c.at.some((r) => taken.some((t) => overlaps(r, t)))) continue;
		taken.push(...c.at);
		chosen.push(c);
	}
	return chosen
		.sort((x, y) => x.at[0].from - y.at[0].from)
		.map((c, i) => ({ ...c, label: String.fromCharCode(65 + (i % 26)) }));
}

export function musicStructure(m: MusicAnalysis): Structure {
	return {
		bpm: Math.round(gridBpm(m.grid) * 10) / 10,
		beatsPerBar: m.grid.beats_per_bar,
		barSeconds: gridBarSeconds(m.grid),
		bars: m.bar_chroma.length,
		introEnd: Math.max(0, firstDownbeat(m.grid)),
		duration: m.duration,
		repeats: repeatsOf(m)
	};
}

/** Where a repeat's occurrences sit in the file, in seconds — for the bands under the energy curve. */
export function repeatSpans(m: Pick<MusicAnalysis, 'grid'>, r: Repeat): { start: number; end: number }[] {
	return r.at.map((b) => ({ start: barStart(m.grid, b.from), end: barStart(m.grid, b.to) }));
}

/** `bars 9–16 and 25–32` (1-based, as a musician counts them). */
export function repeatWhere(r: Repeat): string {
	const spans = r.at.map((b) => (b.to - b.from === 1 ? `${b.from + 1}` : `${b.from + 1}–${b.to}`));
	return `bar${r.bars === 1 && r.at.length === 1 ? '' : 's'} ${spans.length > 2 ? `${spans.slice(0, -1).join(', ')} and ${spans[spans.length - 1]}` : spans.join(' and ')}`;
}

/** One line for a repeat: `A · 8 bars · bars 9–16 and 25–32`. */
export function repeatLine(r: Repeat): string {
	return `${r.label} · ${r.bars} bars · ${repeatWhere(r)}`;
}

/** The energy buckets as an SVG area path in a `w × h` box, scaled to the loudest bucket. Empty for no data. */
export function energyPath(values: readonly number[], w: number, h: number): string {
	const finite = values.map((v) => (Number.isFinite(v) && v > 0 ? v : 0));
	const peak = Math.max(...finite, 1e-9);
	if (finite.length < 2 || peak <= 1e-9) return '';
	const x = (i: number) => (i / (finite.length - 1)) * w;
	const y = (v: number) => h - (v / peak) * (h - 2);
	const top = finite.map((v, i) => `${i === 0 ? 'M' : 'L'}${x(i).toFixed(1)} ${y(v).toFixed(1)}`).join(' ');
	return `${top} L${w} ${h} L0 ${h} Z`;
}
