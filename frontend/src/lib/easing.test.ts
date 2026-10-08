import { describe, expect, test } from 'bun:test';
import { curve, EASE_STEPS, easedPoints, easingId, keyframeChannel } from './easing';
import type { Keyframe } from './types';

const key = (time: number, scale: number, easing?: Keyframe['easing']): Keyframe => ({
	time,
	scale,
	pos_x: 0,
	pos_y: 0,
	rotation: 0,
	opacity: 1,
	...(easing ? { easing } : {})
});

describe('easing curves', () => {
	// The numbers model.rs's `easing_curves_match_the_frontend_mirror_bit_for_bit` pins too.
	test('match kerf-core bit for bit', () => {
		expect(curve('ease_in', 0.25)).toBe(0.09346465071845697);
		expect(curve('ease_out', 0.25)).toBe(0.37813813082472236);
		expect(curve('ease_in_out', 0.3)).toBe(0.18739590670494793);
		expect(curve({ bezier: { x1: 0.2, y1: 0.9, x2: 0.3, y2: 0.1 } }, 0.5)).toBe(0.5513296820201573);
		expect(curve('ease_in_out', 7 / 12)).toBe(0.6411736034061419);
	});

	test('start at 0, end at 1, hold until the end, clamp wild control points', () => {
		for (const e of ['linear', 'ease_in', 'ease_out', 'ease_in_out'] as const) {
			expect(curve(e, 0)).toBeCloseTo(0, 12);
			expect(curve(e, 1)).toBeCloseTo(1, 12);
		}
		expect(curve('hold', 0.99)).toBe(0);
		expect(curve('hold', 1)).toBe(1);
		const wild = { bezier: { x1: 0.5, y1: 3, x2: 0.5, y2: -2 } };
		for (let i = 0; i <= 20; i++) {
			const v = curve(wild, i / 20);
			expect(v >= 0 && v <= 1).toBe(true);
		}
	});
});

describe('the polyline', () => {
	test('linear keys are themselves; a curve is twelve pieces; a hold is a step', () => {
		expect(easedPoints([[0, 1, 'linear'], [2, 3, 'linear']])).toEqual([[0, 1], [2, 3]]);
		const eased = easedPoints([[0, 1, 'ease_in_out'], [2, 3, 'linear']]);
		expect(eased.length).toBe(EASE_STEPS + 1);
		expect(eased[0]).toEqual([0, 1]);
		expect(eased[eased.length - 1]).toEqual([2, 3]);
		expect(easedPoints([[0, 1, 'hold'], [2, 3, 'linear']])).toEqual([[0, 1], [2, 1], [2, 3]]);
	});

	test('a channel reads each key’s easing, a key with none is linear, order is by time', () => {
		const pts = keyframeChannel([key(2, 3), key(0, 1, 'hold')], (k) => k.scale);
		expect(pts).toEqual([[0, 1], [2, 1], [2, 3]]);
	});
});

test('a bezier matching a preset reads as it, any other as custom', () => {
	expect(easingId(undefined)).toBe('linear');
	expect(easingId('hold')).toBe('hold');
	expect(easingId({ bezier: { x1: 0.2, y1: 0.9, x2: 0.3, y2: 1 } })).toBe('snappy');
	expect(easingId({ bezier: { x1: 0.21, y1: 0.9, x2: 0.3, y2: 1 } })).toBe('custom');
});
