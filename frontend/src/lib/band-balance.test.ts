import { describe, expect, test } from 'bun:test';
import { BANDS, balanceNotes, checkSections, equalWindows, phraseWindows, synthBalance } from './band-balance';
import type { BandName, WindowBalance } from './types';

// The window cases and the notes are the Rust tests of `project/music.rs`; the made-up measurement
// is checked for the properties a reader of the balance relies on.

describe('windows', () => {
	test('phrases come four bars at a time and grow for a long piece', () => {
		const w = phraseWindows(0, 2, 0, 40);
		expect(w.length).toBe(5);
		expect(w[0].label).toBe('bars 1–4');
		expect([w[1].start, w[1].end]).toEqual([8, 16]);
		const long = phraseWindows(0, 2, 0, 400);
		expect(long.length).toBeLessThanOrEqual(16);
		expect(long[0].end - long[0].start).toBe(32);
		const odd = phraseWindows(0, 2, 0, 18);
		expect(odd.length).toBe(3);
		expect(odd[2].label).toBe('bar 9');
	});

	test('equal windows split the stretch in eight, or stay one when too short', () => {
		const w = equalWindows(10, 18);
		expect(w.length).toBe(8);
		expect([w[0].start, w[7].end]).toEqual([10, 18]);
		expect(Math.abs(w[3].end - w[4].start)).toBeLessThan(1e-12);
		expect(equalWindows(0, 1).length).toBe(1);
	});

	test('explicit sections are cut to the sound and bad ones named', () => {
		const w = checkSections(
			[
				{ start: -5, end: 4, label: 'a' },
				{ start: 8, end: 99 }
			],
			0,
			10
		) as { start: number; end: number }[];
		expect([w[0].start, w[0].end]).toEqual([0, 4]);
		expect([w[1].start, w[1].end]).toEqual([8, 10]);
		expect(checkSections([{ start: 20, end: 30 }], 0, 10)).toContain('sections[0]');
		expect(typeof checkSections([{ start: 3, end: 3 }], 0, 10)).toBe('string');
		expect(typeof checkSections([], 0, 10)).toBe('string');
	});
});

describe('the made-up measurement', () => {
	const windows = [
		{ start: 0, end: 8, label: 'intro' },
		{ start: 8, end: 24, label: 'drop' }
	];

	test('has six bands per window that add to a hundred percent, low to high', () => {
		const { windows: measured, overall } = synthBalance(windows, ['intro', 'drop']);
		const names: BandName[] = BANDS.map((b) => b.name);
		for (const w of [...measured, overall]) {
			expect(w.bands.map((b) => b.name)).toEqual(names);
			expect(w.bands.reduce((a, b) => a + b.share_pct, 0)).toBeCloseTo(100, 0);
			expect(w.bands.find((b) => b.name === w.dominant)!.share_pct).toBe(Math.max(...w.bands.map((b) => b.share_pct)));
		}
		expect([overall.start, overall.end]).toEqual([0, 24]);
	});

	test('is steady from run to run, and a drop is louder and bassier than an intro', () => {
		const a = synthBalance(windows, ['intro', 'drop']);
		expect(synthBalance(windows, ['intro', 'drop'])).toEqual(a);
		const [intro, drop] = a.windows;
		expect(drop.loudness_lufs!).toBeGreaterThan(intro.loudness_lufs! + 4);
		const low = (w: WindowBalance) => w.bands[0].share_pct + w.bands[1].share_pct;
		expect(low(drop)).toBeGreaterThan(low(intro));
	});
});

describe('the notes', () => {
	const band = (name: BandName, share: number) => {
		const edges = BANDS.find((b) => b.name === name)!;
		return { name, low_hz: edges.low_hz, high_hz: edges.high_hz, energy_dbfs: -20, share_pct: share };
	};
	const window = (label: string, shares: number[], lufs: number): WindowBalance => ({
		start: 0,
		end: 8,
		label,
		rms_dbfs: -20,
		loudness_lufs: lufs,
		bands: BANDS.map((b, i) => band(b.name, shares[i])),
		dominant: 'bass'
	});

	test('call out a heavy low end, a thin mid range and a dull top, and the section that departs', () => {
		const overall = window('all', [25, 50, 15.5, 5, 4, 0.5], -18);
		const text = balanceNotes(overall, [
			window('verse', [10, 40, 20, 15, 10, 5], -24),
			window('drop', [40, 52, 3, 2, 2, 1], -12)
		]).join('\n');
		for (const needle of ['low end', 'sub band', 'mid range', 'dull', 'Loudness swings 12 LU', '"drop"', '"verse"', '"drop" is']) {
			expect(text).toContain(needle);
		}
		const fine = window('all', [3, 22, 20, 30, 18, 7], -14);
		expect(balanceNotes(fine, [fine])).toEqual([]);
	});
});
