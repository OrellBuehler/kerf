import { describe, expect, test } from 'bun:test';
import { centeredCrop, needsCrop } from './smart-crop';

describe('needsCrop', () => {
	test('same shape at a different size needs nothing', () => {
		expect(needsCrop(1920, 1080, 1280 / 720)).toBe(false);
		expect(needsCrop(1080, 1920, 1080 / 1920)).toBe(false);
	});

	test('a different shape does', () => {
		expect(needsCrop(1920, 1080, 1080 / 1920)).toBe(true);
		expect(needsCrop(1080, 1920, 16 / 9)).toBe(true);
		expect(needsCrop(1920, 1080, 4 / 5)).toBe(true);
	});

	test('nonsense is refused rather than guessed', () => {
		expect(needsCrop(1920, 1080, 0)).toBe(false);
		expect(needsCrop(1920, 1080, NaN)).toBe(false);
		expect(needsCrop(0, 1080, 1)).toBe(false);
	});
});

describe('centeredCrop', () => {
	test('16:9 into 9:16 crops the width symmetrically', () => {
		const crop = centeredCrop(1920, 1080, 1080 / 1920)!;
		expect(crop.top).toBe(0);
		expect(crop.bottom).toBe(0);
		expect(crop.left).toBeCloseTo(crop.right, 12);
		// 9:16 of 16:9 keeps 0.3164 of the width.
		expect(1 - crop.left - crop.right).toBeCloseTo((1080 * 1080) / (1920 * 1920), 12);
	});

	test('9:16 into 16:9 crops the height instead', () => {
		const crop = centeredCrop(1080, 1920, 16 / 9)!;
		expect(crop.left).toBe(0);
		expect(crop.right).toBe(0);
		expect(crop.top).toBeCloseTo(crop.bottom, 12);
	});

	test('matching footage is left alone', () => {
		expect(centeredCrop(1920, 1080, 16 / 9)).toBeNull();
	});
});
