import { describe, expect, test } from 'bun:test';
import { energyPath, musicStructure, repeatLine, repeatSpans, repeatsOf, repeatWhere } from './music-structure';
import type { MusicAnalysis } from './types';

// 120 BPM in 4/4: a beat is 0.5 s, a bar 2 s, the first bar starts at 1 s.
const music = (phrases: MusicAnalysis['phrases'], bars = 32): MusicAnalysis => ({
	grid: { period_s: 0.5, phase_s: 1, downbeat_offset: 0, beats_per_bar: 4 },
	duration: 1 + bars * 2,
	bar_chroma: Array.from({ length: bars }, () => new Array(12).fill(0)),
	phrases
});

describe('the structure of a piece', () => {
	test('reads the tempo, the bars and where the intro ends', () => {
		const s = musicStructure(music([]));
		expect(s).toMatchObject({ bpm: 120, beatsPerBar: 4, barSeconds: 2, bars: 32, introEnd: 1, duration: 65, repeats: [] });
	});

	test('a grid whose first downbeat is a hair before 0 has no intro', () => {
		const m = music([]);
		m.grid.phase_s = -0.004;
		expect(musicStructure(m).introEnd).toBe(0);
	});
});

describe('repeats', () => {
	test('an 8-bar phrase and where it comes back is one section', () => {
		const r = repeatsOf(music([{ a: 8, b: 24, bars: 8 }]));
		expect(r).toEqual([{ label: 'A', bars: 8, at: [{ from: 8, to: 16 }, { from: 24, to: 32 }] }]);
	});

	test('the longer phrase hides the 4-bar halves inside it, and the rest stay', () => {
		const r = repeatsOf(
			music([
				{ a: 8, b: 24, bars: 8 },
				{ a: 8, b: 24, bars: 4 },
				{ a: 12, b: 28, bars: 4 },
				{ a: 0, b: 4, bars: 4 }
			])
		);
		expect(r.map((x) => [x.label, x.bars, x.at.map((b) => b.from)])).toEqual([
			['A', 4, [0, 4]],
			['B', 8, [8, 24]]
		]);
	});

	test('one start that comes back more than once gathers every place', () => {
		const r = repeatsOf(music([{ a: 0, b: 8, bars: 8 }, { a: 0, b: 16, bars: 8 }]));
		expect(r).toHaveLength(1);
		expect(r[0].at.map((b) => b.from)).toEqual([0, 8, 16]);
	});

	test('labels follow the order they first appear in the music', () => {
		const r = repeatsOf(music([{ a: 16, b: 24, bars: 4 }, { a: 2, b: 10, bars: 4 }]));
		expect(r.map((x) => [x.label, x.at[0].from])).toEqual([
			['A', 2],
			['B', 16]
		]);
	});

	test('no phrases, no repeats', () => {
		expect(repeatsOf(music([]))).toEqual([]);
	});
});

describe('words and places', () => {
	const m = music([{ a: 8, b: 24, bars: 8 }]);
	const [r] = repeatsOf(m);

	test('a repeat is drawn where the bars are in the file', () => {
		// bar 8 starts 1 + 8×2 s in
		expect(repeatSpans(m, r)).toEqual([
			{ start: 17, end: 33 },
			{ start: 49, end: 65 }
		]);
	});

	test('a musician counts bars from one', () => {
		expect(repeatWhere(r)).toBe('bars 9–16 and 25–32');
		expect(repeatLine(r)).toBe('A · 8 bars · bars 9–16 and 25–32');
		expect(repeatWhere({ label: 'C', bars: 4, at: [{ from: 0, to: 4 }, { from: 8, to: 12 }, { from: 16, to: 20 }] })).toBe('bars 1–4, 9–12 and 17–20');
	});
});

describe('the energy curve', () => {
	test('is a closed area scaled to the loudest bucket', () => {
		const path = energyPath([0, 1, 0.5], 100, 40);
		expect(path.startsWith('M0.0 40.0')).toBe(true);
		expect(path).toContain('L50.0 2.0');
		expect(path.endsWith('Z')).toBe(true);
	});

	test('nothing to draw is nothing', () => {
		expect(energyPath([], 100, 40)).toBe('');
		expect(energyPath([1], 100, 40)).toBe('');
		expect(energyPath([0, 0, 0], 100, 40)).toBe('');
		expect(energyPath([Number.NaN, -1], 100, 40)).toBe('');
	});
});
