import { describe, expect, test } from 'bun:test';
import { MAX_SIDE, MIN_SIDE, editSide } from './export-size';

describe('editSide', () => {
	test('an even whole size inside the range stands as typed', () => {
		expect(editSide('1920', 1080)).toEqual({ value: 1920, note: null });
		expect(editSide(' 1080 ', 1920)).toEqual({ value: 1080, note: null });
		expect(editSide(String(MIN_SIDE), 10)).toEqual({ value: MIN_SIDE, note: null });
		expect(editSide(String(MAX_SIDE), 10)).toEqual({ value: MAX_SIDE, note: null });
	});

	test('a negative or zero size is the smallest, not an error from the backend', () => {
		for (const raw of ['-5', '0', '1']) {
			const out = editSide(raw, 1920);
			expect(out.value).toBe(MIN_SIDE);
			expect(out.note).toContain(`${MIN_SIDE} px`);
		}
	});

	test('a 12K / 360 delivery side is not clamped: the cap is the dialog’s, not the engine’s', () => {
		expect(editSide('11520', 1080)).toEqual({ value: 11520, note: null });
		expect(MAX_SIDE).toBeGreaterThanOrEqual(11520);
	});

	test('a size past the range is the largest', () => {
		const out = editSide('99999', 1080);
		expect(out.value).toBe(MAX_SIDE);
		expect(out.note).toContain(`${MAX_SIDE} px`);
	});

	test('an odd size goes down to the even number below it, as Delivery::new does, and says so', () => {
		expect(editSide('1081', 1080)).toEqual({ value: 1080, note: 'Sides are even: 1081 became 1080.' });
		expect(editSide('3', 1080).value).toBe(2);
	});

	test('a fraction is cut to whole pixels and an odd one then goes even', () => {
		expect(editSide('1280.5', 1080)).toEqual({ value: 1280, note: 'Sides are whole pixels: 1280.5 became 1280.' });
		expect(editSide('1281.9', 1080).value).toBe(1280);
	});

	test('no number keeps what was there, rather than the 1920 a parseInt fallback put back', () => {
		for (const raw of ['', '   ', 'abc', 'NaN', 'Infinity']) {
			expect(editSide(raw, 1280)).toEqual({ value: 1280, note: 'Enter a size in pixels.' });
		}
	});

	test('every result is a whole, even number in range', () => {
		for (const raw of ['-1e9', '1e9', '7', '8191', '8193', '2.9', '0.4', '4096']) {
			const { value } = editSide(raw, 1920);
			expect(Number.isInteger(value)).toBe(true);
			expect(value % 2).toBe(0);
			expect(value).toBeGreaterThanOrEqual(MIN_SIDE);
			expect(value).toBeLessThanOrEqual(MAX_SIDE);
		}
	});
});
