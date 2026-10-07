import { describe, expect, test } from 'bun:test';
import { dbToGain, gainLabel, gainToDb, MAX_GAIN } from './mixer';
import {
	DB_STEP,
	dragGain,
	fractionToGain,
	gainToFraction,
	LINE_DB_MAX,
	LINE_DB_MIN,
	lineTravel,
	lineY,
	SILENT_DB,
	SILENT_FRACTION,
	UNITY_DETENT_DB
} from './volume-line';

describe('the line scale', () => {
	test('is dB from the bottom to the top, with silence at the very bottom', () => {
		expect(gainToFraction(0)).toBe(0);
		expect(gainToFraction(dbToGain(LINE_DB_MIN))).toBeCloseTo(0, 12);
		expect(gainToFraction(dbToGain(LINE_DB_MAX))).toBeCloseTo(1, 12);
		expect(gainToFraction(dbToGain(-12))).toBeCloseTo((-12 - LINE_DB_MIN) / (LINE_DB_MAX - LINE_DB_MIN), 12);
	});

	test('the top is the same ceiling the Inspector slider and the fader stop at', () => {
		expect(MAX_GAIN).toBe(2);
		expect(dbToGain(LINE_DB_MAX)).toBeCloseTo(MAX_GAIN, 12);
		expect(gainToFraction(MAX_GAIN)).toBeCloseTo(1, 12);
	});

	test('unity sits near the top: a little room to boost, most of the travel to duck', () => {
		expect(gainToFraction(1)).toBeCloseTo(-LINE_DB_MIN / (LINE_DB_MAX - LINE_DB_MIN), 12);
		expect(gainToFraction(1)).toBeGreaterThan(0.8);
	});

	test('gain and fraction invert each other inside the travel', () => {
		for (const f of [0.05, 0.2, 0.5, 0.75, 0.9, 1]) {
			expect(gainToFraction(fractionToGain(f))).toBeCloseTo(f, 12);
		}
		for (const g of [0.03, 0.1, 0.5, 1, 1.9, 2]) {
			expect(fractionToGain(gainToFraction(g))).toBeCloseTo(g, 12);
		}
	});

	test('anything past either end clamps; the bottom is silence', () => {
		expect(fractionToGain(-1)).toBe(0);
		expect(fractionToGain(SILENT_FRACTION)).toBe(0);
		expect(fractionToGain(SILENT_FRACTION + 0.01)).toBeGreaterThan(0);
		expect(fractionToGain(5)).toBeCloseTo(MAX_GAIN, 12);
		expect(gainToFraction(1000)).toBe(1);
		expect(gainToFraction(-1)).toBe(0);
		expect(gainToFraction(NaN)).toBe(0);
	});
});

describe('dragGain', () => {
	const range = 36;
	const dbPerPx = (LINE_DB_MAX - LINE_DB_MIN) / range;

	test('no movement keeps the gain the line was grabbed at, near unity or not', () => {
		expect(dragGain(1, 0, range)).toBe(1);
		// not "close to": the rounding step is 0.1 dB, so an untouched value moves to it
		expect(gainToDb(dragGain(0.5, 0, range))).toBeCloseTo(-6, 5);
	});

	test('dragging down lowers it, up raises it', () => {
		expect(dragGain(1, 10, range)).toBeLessThan(1);
		expect(dragGain(0.5, -4, range)).toBeGreaterThan(0.5);
	});

	test('a drag is relative: the same pixels move the same dB from any start', () => {
		for (const start of [dbToGain(-20), dbToGain(-12), dbToGain(-3)]) {
			const moved = gainToDb(dragGain(start, 6, range)) - gainToDb(start);
			expect(Math.abs(moved + 6 * dbPerPx)).toBeLessThan(DB_STEP);
		}
	});

	test('lands on exactly 1.0 near 0 dB, so a clip can be left alone', () => {
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

	test('dragging to the bottom mutes, and the top stops at exactly the shared maximum', () => {
		expect(dragGain(1, 500, range)).toBe(0);
		expect(dragGain(1, -500, range)).toBe(MAX_GAIN);
		expect(dragGain(1.5, -500, range)).toBe(MAX_GAIN);
	});

	test('from silence the line comes back up', () => {
		expect(dragGain(0, -20, range)).toBeGreaterThan(0);
		expect(dragGain(0, 5, range)).toBe(0);
		expect(dragGain(0, -0.3, range)).toBe(0); // not out of the silent zone yet
		expect(gainToDb(dragGain(0, -4, range))).toBeGreaterThan(SILENT_DB);
	});

	test('a clip set above the scale keeps its level: a small drag moves it from there', () => {
		// 6x is +15.6 dB, past the top of the line
		const loud = 6;
		const down = dragGain(loud, 2, range);
		expect(gainToDb(down)).toBeCloseTo(gainToDb(loud) - 2 * dbPerPx, 0);
		expect(down).toBeGreaterThan(MAX_GAIN); // not collapsed to the top of the scale
		// up from past the top changes nothing; the stored value stays exactly as it was
		expect(dragGain(loud, -3, range)).toBe(loud);
		// and it can still be dragged all the way down
		expect(dragGain(loud, 500, range)).toBe(0);
	});

	test('a tiny gain keeps its level when dragged up, and mutes when dragged down', () => {
		const faint = dbToGain(-50);
		expect(dragGain(faint, -2, range)).toBeGreaterThan(0);
		expect(dragGain(faint, 2, range)).toBe(0);
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

	test('the maximum is the top of the travel, unity just below it, silence the bottom', () => {
		const { top, range } = lineTravel(54);
		expect(lineY(1, 54)).toBeCloseTo(top + range * (1 - gainToFraction(1)), 9);
		expect(lineY(1, 54)).toBeGreaterThan(top);
		expect(lineY(0, 54)).toBeCloseTo(top + range, 9);
		expect(lineY(MAX_GAIN, 54)).toBeCloseTo(top, 9);
		expect(lineY(50, 54)).toBeCloseTo(top, 9); // a clip past the top is drawn at it
	});

	test('a tiny clip still has a travel to compute against', () => {
		expect(lineTravel(10).range).toBe(1);
		expect(Number.isFinite(lineY(1, 10))).toBe(true);
	});
});
