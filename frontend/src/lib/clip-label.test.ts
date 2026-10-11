import { describe, expect, test } from 'bun:test';
import { MIN_LABEL_CLIP_PX, NAME_CHIP_PCT, showsLabel } from './clip-label';

describe('showsLabel', () => {
	test('a clip with room shows its name, a sliver does not', () => {
		expect(showsLabel(MIN_LABEL_CLIP_PX)).toBe(true);
		expect(showsLabel(400)).toBe(true);
		expect(showsLabel(MIN_LABEL_CLIP_PX - 1)).toBe(false);
		expect(showsLabel(6)).toBe(false);
	});

	test('a width that is not a number has no name to show', () => {
		expect(showsLabel(NaN)).toBe(false);
		expect(showsLabel(-5)).toBe(false);
	});

	test('the chip is a share of the scrim, not all of it', () => {
		expect(NAME_CHIP_PCT).toBeGreaterThan(0);
		expect(NAME_CHIP_PCT).toBeLessThan(100);
	});
});
