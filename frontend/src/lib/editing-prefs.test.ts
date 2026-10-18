import { describe, expect, test } from 'bun:test';
import {
	clampSeconds,
	DEFAULT_EDITING,
	LENGTH_RANGE,
	parseEditing,
	placementOut,
	secondsLabel
} from './editing-prefs';

describe('parseEditing', () => {
	test('nothing stored is the defaults', () => {
		expect(parseEditing(null)).toEqual(DEFAULT_EDITING);
		expect(parseEditing(undefined)).toEqual(DEFAULT_EDITING);
		expect(parseEditing([])).toEqual(DEFAULT_EDITING);
		expect(parseEditing('snap')).toEqual(DEFAULT_EDITING);
	});

	test('a good value is kept and a bad one is that field alone', () => {
		expect(parseEditing({ snap: false, still_seconds: 8, title_seconds: 4.5 })).toEqual({ snap: false, still_seconds: 8, title_seconds: 4.5 });
		expect(parseEditing({ snap: 'no', still_seconds: 'long', title_seconds: 4 })).toEqual({ snap: true, still_seconds: 5, title_seconds: 4 });
		expect(parseEditing({ snap: false })).toEqual({ ...DEFAULT_EDITING, snap: false });
	});

	test('lengths are held to the range and to half seconds', () => {
		expect(parseEditing({ still_seconds: 0 }).still_seconds).toBe(LENGTH_RANGE.min);
		expect(parseEditing({ still_seconds: 1e9 }).still_seconds).toBe(LENGTH_RANGE.max);
		expect(parseEditing({ title_seconds: 2.74 }).title_seconds).toBe(2.5);
		expect(parseEditing({ title_seconds: Number.NaN }).title_seconds).toBe(3);
		expect(clampSeconds('7', 5)).toBe(7);
		expect(clampSeconds('', 5)).toBe(5);
		expect(clampSeconds(null, 5)).toBe(5);
	});
});

describe('the defaults', () => {
	test('read as seconds', () => {
		expect(DEFAULT_EDITING.still_seconds).toBe(5);
		expect(secondsLabel(5)).toBe('5 s');
		expect(secondsLabel(2.5)).toBe('2.5 s');
	});
});

describe('placementOut', () => {
	test('a still takes the preference, footage takes its own length', () => {
		expect(placementOut({ duration: 5, streams: [{ image: true }] }, 8)).toBe(8);
		expect(placementOut({ duration: 42, streams: [{ image: false }, {}] }, 8)).toBe(42);
	});
});
