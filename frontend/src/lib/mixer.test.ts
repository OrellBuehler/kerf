import { describe, expect, test } from 'bun:test';
import { gainLabel, isUnityMix, panGains, panLabel } from './mixer';

describe('panGains', () => {
	test('centre is exactly unity on both sides', () => {
		// The same assertion as the Rust test: an untouched track must not be
		// touched, or every existing mix comes back changed.
		expect(panGains(0)).toEqual([1, 1]);
		expect(panGains(undefined as never)).toEqual([1, 1]);
	});

	test('is a balance, never a boost', () => {
		expect(panGains(-1)).toEqual([1, 0]);
		expect(panGains(1)).toEqual([0, 1]);
		expect(panGains(-0.5)).toEqual([1, 0.5]);
		for (const p of [-1, -0.5, 0, 0.25, 1]) {
			const [l, r] = panGains(p);
			expect(l).toBeLessThanOrEqual(1);
			expect(r).toBeLessThanOrEqual(1);
		}
	});

	test('clamps rather than inverting out of range', () => {
		expect(panGains(9)).toEqual([0, 1]);
		expect(panGains(-9)).toEqual([1, 0]);
	});
});

describe('labels', () => {
	test('a fader reads in dB, silence included', () => {
		expect(gainLabel(1)).toBe('0.0 dB'); // unity reads as 0, the way a mixer shows it
		expect(gainLabel(0.5)).toBe('-6.0 dB');
		expect(gainLabel(0)).toBe('−∞ dB');
		expect(gainLabel(2)).toBe('+6.0 dB');
	});

	test('a pan reads as a mixer shows it', () => {
		expect(panLabel(0)).toBe('centre');
		expect(panLabel(-1)).toBe('L100');
		expect(panLabel(0.3)).toBe('R30');
	});
});

describe('isUnityMix', () => {
	test('an unset mix is unity', () => {
		expect(isUnityMix(undefined, undefined)).toBe(true);
		expect(isUnityMix(1, 0)).toBe(true);
		expect(isUnityMix(0.5, 0)).toBe(false);
		expect(isUnityMix(1, -0.2)).toBe(false);
	});
});
