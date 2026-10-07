import { describe, expect, test } from 'bun:test';
import { dbToGain, gainLabel, gainToDb } from './mixer';
import {
	DB_STEP,
	dragGain,
	fractionToGain,
	gainToFraction,
	LINE_DB_MAX,
	LINE_DB_MIN,
	lineTravel,
	lineY,
	SILENT_FRACTION,
	UNITY_DETENT_DB
} from './volume-line';

describe('the line scale', () => {
	test('is dB from the bottom to the top, with silence at the very bottom', () => {
		expect(gainToFraction(0)).toBe(0);
		expect(gainToFraction(dbToGain(LINE_DB_MIN))).toBeCloseTo(0, 12);
		expect(gainToFraction(dbToGain(LINE_DB_MAX))).toBeCloseTo(1, 12);
		expect(gainToFraction(dbToGain(-12))).toBeCloseTo(0.5, 12); // halfway up the travel
	});

	test('unity sits a quarter of the way down: room above to boost, most below to duck', () => {
		expect(gainToFraction(1)).toBeCloseTo(36 / 48, 12);
	});

	test('gain and fraction invert each other inside the travel', () => {
		for (const f of [0.05, 0.2, 0.5, 0.75, 0.9, 1]) {
			expect(gainToFraction(fractionToGain(f))).toBeCloseTo(f, 12);
		}
		for (const g of [0.03, 0.1, 0.5, 1, 2, 3.9]) {
			expect(fractionToGain(gainToFraction(g))).toBeCloseTo(g, 12);
		}
	});

	test('anything past either end clamps; the bottom is silence', () => {
		expect(fractionToGain(-1)).toBe(0);
		expect(fractionToGain(SILENT_FRACTION)).toBe(0);
		expect(fractionToGain(SILENT_FRACTION + 0.01)).toBeGreaterThan(0);
		expect(fractionToGain(5)).toBeCloseTo(dbToGain(LINE_DB_MAX), 12);
		expect(gainToFraction(1000)).toBe(1);
		expect(gainToFraction(-1)).toBe(0);
		expect(gainToFraction(NaN)).toBe(0);
	});
});

describe('dragGain', () => {
	const range = 36;

	test('no movement keeps the gain the line was grabbed at, near unity or not', () => {
		expect(dragGain(1, 0, range)).toBe(1);
		// not "close to": the rounding step is 0.1 dB, so an untouched value moves to it
		expect(gainToDb(dragGain(0.5, 0, range))).toBeCloseTo(-6, 5);
	});

	test('dragging down lowers it, up raises it', () => {
		expect(dragGain(1, 10, range)).toBeLessThan(1);
		expect(dragGain(1, -4, range)).toBeGreaterThan(1);
	});

	test('a drag is relative: the same pixels move the same dB from any start', () => {
		const dbPerPx = (LINE_DB_MAX - LINE_DB_MIN) / range;
		for (const start of [dbToGain(-20), dbToGain(-6), dbToGain(3)]) {
			const moved = gainToDb(dragGain(start, 6, range)) - gainToDb(start);
			expect(Math.abs(moved + 6 * dbPerPx)).toBeLessThan(DB_STEP);
		}
	});

	test('lands on exactly 1.0 near 0 dB, so a clip can be left alone', () => {
		const dbPerPx = (LINE_DB_MAX - LINE_DB_MIN) / range;
		// 0.2 dB under unity, whichever way it was reached
		expect(dragGain(dbToGain(-3), -(2.8 / dbPerPx), range)).toBe(1);
		expect(dragGain(dbToGain(3), 2.8 / dbPerPx, range)).toBe(1);
		expect(UNITY_DETENT_DB).toBeGreaterThan(DB_STEP);
	});

	test('rounds to a tenth of a dB', () => {
		const g = dragGain(1, 7.3, range);
		const db = gainToDb(g);
		expect(Math.abs(db * 10 - Math.round(db * 10))).toBeLessThan(1e-9);
		expect(gainLabel(g)).toMatch(/^-\d+\.\d dB$/);
	});

	test('dragging to the bottom mutes, and the top stops at the maximum', () => {
		expect(dragGain(1, 500, range)).toBe(0);
		expect(gainToDb(dragGain(1, -500, range))).toBeCloseTo(LINE_DB_MAX, 5);
	});

	test('from silence the line comes back up', () => {
		expect(dragGain(0, -20, range)).toBeGreaterThan(0);
		expect(dragGain(0, 5, range)).toBe(0);
	});

	test('a degenerate travel changes nothing', () => {
		expect(dragGain(0.7, 30, 0)).toBe(0.7);
		expect(dragGain(0.7, 30, NaN)).toBe(0.7);
	});
});

describe('line geometry', () => {
	test('the travel keeps clear of the fade handles and the bottom edge', () => {
		const { top, range } = lineTravel(54);
		expect(top).toBe(14);
		expect(top + range).toBe(54 - 4);
	});

	test('unity is a quarter of the travel from the top, silence is the bottom', () => {
		const { top, range } = lineTravel(54);
		expect(lineY(1, 54)).toBeCloseTo(top + range * 0.25, 9);
		expect(lineY(0, 54)).toBeCloseTo(top + range, 9);
		expect(lineY(dbToGain(LINE_DB_MAX), 54)).toBeCloseTo(top, 9);
	});

	test('a tiny clip still has a travel to compute against', () => {
		expect(lineTravel(10).range).toBe(1);
		expect(Number.isFinite(lineY(1, 10))).toBe(true);
	});
});
