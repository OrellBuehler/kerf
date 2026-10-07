import { describe, expect, test } from 'bun:test';
import { contrastRatio, mixSrgb, relativeLuminance } from './contrast';

describe('relativeLuminance', () => {
	test('black is 0 and white is 1', () => {
		expect(relativeLuminance('#000000')).toBe(0);
		expect(relativeLuminance('#ffffff')).toBeCloseTo(1, 10);
	});

	test('green carries most of the luminance, blue least', () => {
		expect(relativeLuminance('#00ff00')).toBeCloseTo(0.7152, 4);
		expect(relativeLuminance('#ff0000')).toBeCloseTo(0.2126, 4);
		expect(relativeLuminance('#0000ff')).toBeCloseTo(0.0722, 4);
	});

	test('is case-insensitive and refuses what is not a six-digit hex', () => {
		expect(relativeLuminance('#EEF1F5')).toBe(relativeLuminance('#eef1f5'));
		expect(() => relativeLuminance('#fff')).toThrow();
		expect(() => relativeLuminance('white')).toThrow();
		expect(() => relativeLuminance('#12345g')).toThrow();
	});
});

describe('contrastRatio', () => {
	test('is 21:1 for black on white and 1:1 for a color on itself', () => {
		expect(contrastRatio('#000000', '#ffffff')).toBeCloseTo(21, 10);
		expect(contrastRatio('#336699', '#336699')).toBe(1);
	});

	test('does not care which color is the text', () => {
		expect(contrastRatio('#14181e', '#f2f4f6')).toBe(contrastRatio('#f2f4f6', '#14181e'));
	});

	test('matches the published reference values', () => {
		// #767676 on white is the lightest gray that still passes AA (4.54:1), and
		// #777777 — one step lighter — is the famous 4.48:1 that does not.
		expect(contrastRatio('#767676', '#ffffff')).toBeCloseTo(4.54, 2);
		expect(contrastRatio('#777777', '#ffffff')).toBeCloseTo(4.48, 2);
		expect(contrastRatio('#767676', '#ffffff')).toBeGreaterThanOrEqual(4.5);
		expect(contrastRatio('#777777', '#ffffff')).toBeLessThan(4.5);
		// A mid-gray against black: (0.2159 + .05) / .05.
		expect(contrastRatio('#808080', '#000000')).toBeCloseTo(5.32, 2);
	});
});

describe('mixSrgb', () => {
	test('is the colors themselves at the ends and halfway between in the middle', () => {
		expect(mixSrgb('#336699', '#ffffff', 1)).toBe('#336699');
		expect(mixSrgb('#336699', '#ffffff', 0)).toBe('#ffffff');
		expect(mixSrgb('#000000', '#ffffff', 0.5)).toBe('#808080');
		expect(mixSrgb('#000000', '#ffffff', 0.25)).toBe('#bfbfbf');
	});

	test('weights the first color by the fraction given', () => {
		// 70% of #5d4180 over black: each channel scaled by .7.
		expect(mixSrgb('#5d4180', '#000000', 0.7)).toBe('#412e5a');
		expect(() => mixSrgb('red', '#000000', 0.5)).toThrow();
	});
});
