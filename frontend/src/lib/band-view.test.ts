import { describe, expect, test } from 'bun:test';
import { BANDS, synthBalance } from './band-balance';
import { BAND_LABEL, BAND_ORDER, BAND_RANGE, BAND_SHORT, balanceView, fmtDb, scaleFor } from './band-view';
import type { BandBalance, BandLevel, BandName, WindowBalance } from './types';

// The readout of a balance: a row per window with a bar per band, the dominant one marked, the
// loudness beside it. The numbers come from the backend (here, from the harness's made-up
// measurement); what is checked is that they are laid out as measured and compare across rows.

const clock = (s: number) => `${Math.floor(s / 60)}:${String(Math.round(s % 60)).padStart(2, '0')}`;

function windowOf(shares: number[], over: Partial<WindowBalance> = {}): WindowBalance {
	const bands: BandLevel[] = BANDS.map((b, i) => ({
		name: b.name,
		low_hz: b.low_hz,
		high_hz: b.high_hz,
		energy_dbfs: shares[i] > 0 ? -20 + 10 * Math.log10(shares[i] / 100) : null,
		share_pct: shares[i]
	}));
	const top = bands.reduce((m, b) => (b.share_pct > m.share_pct ? b : m), bands[0]);
	return { start: 0, end: 8, label: 'intro', rms_dbfs: -20, loudness_lufs: -18.2, bands, dominant: top.name, ...over };
}

function balance(windows: WindowBalance[], over: Partial<BandBalance> = {}): BandBalance {
	return {
		target: 'clip',
		name: 'Music',
		start: 0,
		end: 24,
		windows,
		overall: windowOf([2, 20, 20, 36, 16, 6], { label: null, start: 0, end: 24, loudness_lufs: -17.4 }),
		windows_from: 'score',
		notes: [],
		...over
	};
}

describe('the bands', () => {
	test('are the six, low to high, each with a name, a range and a short label', () => {
		expect(BAND_ORDER).toEqual(BANDS.map((b) => b.name));
		const names: BandName[] = ['sub', 'bass', 'low_mid', 'mid', 'high_mid', 'air'];
		expect([...BAND_ORDER]).toEqual(names);
		for (const n of names) {
			expect(BAND_LABEL[n]).toBeTruthy();
			expect(BAND_RANGE[n]).toBeTruthy();
			// A bar is a couple of dozen pixels wide in the Inspector: four letters at most.
			expect(BAND_SHORT[n].length).toBeLessThanOrEqual(4);
		}
	});

	test('the ranges are the engine’s edges', () => {
		expect(BAND_RANGE.sub).toBe('20–60 Hz');
		expect(BAND_RANGE.air).toBe('6–20 kHz');
		expect(BANDS.map((b) => [b.low_hz, b.high_hz])).toEqual([
			[20, 60],
			[60, 250],
			[250, 500],
			[500, 2000],
			[2000, 6000],
			[6000, 20000]
		]);
	});
});

describe('the scale', () => {
	test('is forty percent for a balanced sound and grows in tens when a band holds more', () => {
		expect(scaleFor([windowOf([2, 20, 20, 36, 16, 6])])).toBe(40);
		expect(scaleFor([windowOf([2, 41, 20, 20, 11, 6])])).toBe(50);
		expect(scaleFor([windowOf([0, 80, 5, 5, 5, 5])])).toBe(80);
		expect(scaleFor([])).toBe(40);
	});

	test('is the same for every row, so a bar is comparable across windows', () => {
		const view = balanceView(balance([windowOf([2, 20, 20, 36, 16, 6]), windowOf([1, 55, 20, 14, 6, 4], { label: 'drop' })]), clock);
		expect(view.scale).toBe(60);
		const mid = (i: number) => view.rows[i].bars.find((b) => b.name === 'mid')!;
		expect(mid(0).height).toBeCloseTo(36 / 60, 9);
		expect(mid(1).height).toBeCloseTo(14 / 60, 9);
		expect(view.overall.bars.find((b) => b.name === 'mid')!.height).toBeCloseTo(36 / 60, 9);
	});
});

describe('a window', () => {
	const view = balanceView(
		balance([windowOf([2, 20, 20, 36, 16, 6]), windowOf([5, 40, 20, 20, 10, 5], { label: null, start: 8, end: 24, loudness_lufs: -12.04 })]),
		clock
	);

	test('has a bar for each of the six bands, low to high, with the share as measured', () => {
		expect(view.rows[0].bars.map((b) => b.name)).toEqual([...BAND_ORDER]);
		expect(view.rows[0].bars.map((b) => b.share)).toEqual([2, 20, 20, 36, 16, 6]);
	});

	test('marks the dominant band, and only that one', () => {
		expect(view.rows[0].dominant).toBe('mid');
		expect(view.rows[0].bars.filter((b) => b.dominant).map((b) => b.name)).toEqual(['mid']);
		expect(view.rows[1].bars.filter((b) => b.dominant).map((b) => b.name)).toEqual(['bass']);
	});

	test('is named by its label, else by the times it spans', () => {
		expect(view.rows[0].label).toBe('intro');
		expect(view.rows[1].label).toBe('0:08–0:24');
		expect(view.rows[1].range).toBe('0:08–0:24');
	});

	test('reads its loudness to a tenth, with a true minus', () => {
		expect(view.rows[0].loudness).toBe('−18.2 LUFS');
		expect(view.rows[1].loudness).toBe('−12.0 LUFS');
		expect(fmtDb(-0.04)).toBe('−0.0');
		expect(fmtDb(3.14)).toBe('3.1');
	});

	test('says what it is in the tooltip: the window, the band that leads, the loudness and the level', () => {
		expect(view.rows[0].tip).toContain('intro (0:00–0:08)');
		expect(view.rows[0].tip).toContain('Mostly mid · −18.2 LUFS · RMS −20.0 dBFS');
		expect(view.rows[0].bars[3].tip).toBe('Mid 500 Hz–2 kHz: 36 % · −24.4 dBFS');
	});
});

describe('the whole stretch', () => {
	test('is the first row, called Overall, with a headline of the band that leads and the loudness', () => {
		const view = balanceView(balance([windowOf([2, 20, 20, 36, 16, 6])]), clock);
		expect(view.overall.label).toBe('Overall');
		expect(view.headline).toBe('Mostly mid · −17.4 LUFS');
	});

	test('says where the windows came from', () => {
		const from = (windows_from: BandBalance['windows_from']) => balanceView(balance([windowOf([2, 20, 20, 36, 16, 6])], { windows_from }), clock).basis;
		expect(from('score')).toBe('the score’s sections');
		expect(from('phrases')).toBe('the music’s phrases');
		expect(from('equal')).toBe('equal parts');
		expect(from('sections')).toBe('the sections given');
	});
});

describe('silence', () => {
	const silent = windowOf([0, 0, 0, 0, 0, 0], { rms_dbfs: null, loudness_lufs: null, dominant: null, bands: [] });

	test('has no bars to speak of, no dominant band and says it is silent', () => {
		const view = balanceView(balance([silent], { overall: { ...silent, label: null } }), clock);
		expect(view.rows[0].dominant).toBeNull();
		expect(view.rows[0].loudness).toBe('silent');
		expect(view.rows[0].bars.every((b) => b.share === 0 && b.height === 0 && !b.dominant)).toBe(true);
		expect(view.rows[0].tip).toContain('Nothing to measure');
		expect(view.headline).toBe('Silent');
	});

	test('a band with no energy has no level in its tooltip', () => {
		const w = windowOf([0, 50, 25, 15, 10, 0]);
		const view = balanceView(balance([w]), clock);
		expect(view.rows[0].bars[0].tip).toBe('Sub 20–60 Hz: 0 %');
		expect(view.rows[0].bars[5].tip).toBe('Air 6–20 kHz: 0 %');
	});
});

describe('the harness’s measurement, laid out', () => {
	test('every window has six bars that stay inside their cell', () => {
		const m = synthBalance(
			[
				{ start: 0, end: 8, label: 'intro' },
				{ start: 8, end: 24, label: 'drop' }
			],
			['intro', 'drop']
		);
		const view = balanceView({ ...balance([]), ...m }, clock);
		expect(view.rows.length).toBe(2);
		for (const row of [view.overall, ...view.rows]) {
			expect(row.bars.length).toBe(6);
			for (const b of row.bars) expect(b.height >= 0 && b.height <= 1).toBe(true);
			expect(Math.max(...row.bars.map((b) => b.height))).toBeGreaterThan(0.3);
		}
	});
});
