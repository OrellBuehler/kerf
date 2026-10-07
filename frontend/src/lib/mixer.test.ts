import { describe, expect, test } from 'bun:test';
import { dbToGain, effectiveGain, gainLabel, gainToDb, isUnityMix, panGains, panLabel } from './mixer';

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

describe('dB and linear gain', () => {
	test('round-trip, with silence at -Infinity', () => {
		expect(gainToDb(1)).toBe(0);
		expect(gainToDb(0.5)).toBeCloseTo(-6.0206, 4);
		expect(gainToDb(2)).toBeCloseTo(6.0206, 4);
		expect(gainToDb(0)).toBe(-Infinity);
		expect(dbToGain(-Infinity)).toBe(0);
		expect(dbToGain(0)).toBe(1);
		for (const v of [0.01, 0.25, 0.7, 1, 1.9, 3.9]) expect(dbToGain(gainToDb(v))).toBeCloseTo(v, 12);
	});

	test('agrees with the label a fader reads', () => {
		expect(gainLabel(dbToGain(-12))).toBe('-12.0 dB');
		expect(gainLabel(dbToGain(3))).toBe('+3.0 dB');
	});
});

describe('effectiveGain', () => {
	test('is the clip gain through the track fader, both unity by default', () => {
		expect(effectiveGain(undefined, undefined)).toBe(1);
		expect(effectiveGain(0.5, 0.5)).toBe(0.25);
		expect(effectiveGain(2, 0.5)).toBe(1);
		expect(effectiveGain(1, undefined)).toBe(1);
		expect(effectiveGain(undefined, 0.8)).toBe(0.8);
	});

	test('never negative', () => {
		expect(effectiveGain(-1, 1)).toBe(0);
		expect(effectiveGain(1, -2)).toBe(0);
	});
});
