import { describe, expect, test } from 'bun:test';
import { TRANSPORT_FULL_PX, measuredWidth, transportParts } from './transport-bar';

const FULL = { duration: true, rate: true, tight: false };
const NARROW = { duration: false, rate: false, tight: true };

describe('transportParts', () => {
	test('a width that is not known shows the full bar', () => {
		expect(transportParts(undefined)).toEqual(FULL);
		expect(transportParts(null)).toEqual(FULL);
		expect(transportParts(0)).toEqual(FULL);
	});

	test('a nonsense measurement is not a narrow bar', () => {
		expect(transportParts(NaN)).toEqual(FULL);
		expect(transportParts(-1)).toEqual(FULL);
		expect(transportParts(-Infinity)).toEqual(FULL);
		expect(transportParts(Infinity)).toEqual(FULL);
	});

	test('a measured narrow bar drops the duration and the rate and tightens', () => {
		expect(transportParts(1)).toEqual(NARROW);
		expect(transportParts(240)).toEqual(NARROW);
		expect(transportParts(TRANSPORT_FULL_PX - 1)).toEqual(NARROW);
	});

	test('the threshold itself and anything wider is the full bar', () => {
		expect(transportParts(TRANSPORT_FULL_PX)).toEqual(FULL);
		expect(transportParts(720)).toEqual(FULL);
		expect(transportParts(1600)).toEqual(FULL);
	});

	test('only the readouts compact: nothing else is described by the rule', () => {
		for (const w of [undefined, 0, 1, 100, 379, 380, 1600]) {
			expect(Object.keys(transportParts(w)).sort()).toEqual(['duration', 'rate', 'tight']);
		}
	});
});

describe('measuredWidth', () => {
	test('keeps a positive width and forgets the rest', () => {
		expect(measuredWidth(1067)).toBe(1067);
		expect(measuredWidth(0.5)).toBe(0.5);
		expect(measuredWidth(0)).toBeUndefined();
		expect(measuredWidth(undefined)).toBeUndefined();
		expect(measuredWidth(null)).toBeUndefined();
		expect(measuredWidth(NaN)).toBeUndefined();
		expect(measuredWidth(-5)).toBeUndefined();
	});
});
