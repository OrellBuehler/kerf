import { describe, expect, test } from 'bun:test';
import { curve, EASE_STEPS, easedPoints, easingId, easingProblem, insertKeyframe, keyframeChannel, splitEasing } from './easing';
import type { Easing, Keyframe } from './types';

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

describe('splitting a segment when a key lands inside it', () => {
	const bez = (x1: number, y1: number, x2: number, y2: number): Easing => ({ bezier: { x1, y1, x2, y2 } });

	// The numbers model.rs's `easing_splits_match_the_frontend_mirror_bit_for_bit` pins too.
	test('matches kerf-core bit for bit', () => {
		expect(splitEasing('ease_in_out', 0.3)).toEqual([
			bez(0.38746992860370355, 0, 0.7085545411381486, 0.408751946428076),
			bez(0.32640021418612175, 0.35630403969052316, 0.5660585408301777, 1)
		]);
		expect(splitEasing('ease_in', 0.25)).toEqual([
			bez(0.31716215013249954, 0, 0.6571342389217906, 0.38132629691752473),
			bez(0.4910952083718046, 0.27408617720142425, 1, 1)
		]);
	});

	test('two pieces of a preset are the whole curve', () => {
		for (const e of ['ease_in', 'ease_out', 'ease_in_out', bez(0.2, 0.9, 0.3, 1), bez(0.1, 0.6, 0.4, 1)] as Easing[]) {
			for (const u of [0.05, 0.3, 0.5, 0.7, 0.95]) {
				const [before, after] = splitEasing(e, u);
				const v = curve(e, u);
				for (let i = 0; i <= 20; i++) {
					const w = i / 20;
					expect(Math.abs(v * curve(before, w) - curve(e, u * w))).toBeLessThan(1e-9);
					expect(Math.abs(v + (1 - v) * curve(after, w) - curve(e, u + (1 - u) * w))).toBeLessThan(1e-9);
				}
			}
		}
	});

	test('a hold stays held and a line a line', () => {
		expect(splitEasing('hold', 0.4)).toEqual(['hold', 'hold']);
		expect(splitEasing('linear', 0.4)).toEqual(['linear', 'linear']);
	});

	test('a key in a hold keeps it held through the key; one on a key keeps how it leaves', () => {
		const held = insertKeyframe([key(0, 1, 'hold'), key(4, 3)], key(2, 1));
		expect(held.map((k) => [k.time, k.easing ?? 'linear'])).toEqual([[0, 'hold'], [2, 'hold'], [4, 'linear']]);
		const rekeyed = insertKeyframe(held, key(2.0000004, 9));
		expect(rekeyed.length).toBe(3);
		expect([rekeyed[1].scale, rekeyed[1].easing]).toEqual([9, 'hold']);
	});

	test('a key in a curve splits it and the motion stays where it was', () => {
		const before = [key(1, 1, 'ease_in_out'), key(5, 3)];
		const after = insertKeyframe(before, key(2.3, 1 + 2 * curve('ease_in_out', 1.3 / 4)));
		expect(after.map((k) => easingId(k.easing))).toEqual(['custom', 'custom', 'linear']);
		const sample = (keys: Keyframe[], at: number) => {
			const pts = keyframeChannel(keys, (k) => k.scale);
			for (let i = 0; i + 1 < pts.length; i++) {
				const [[t0, v0], [t1, v1]] = [pts[i], pts[i + 1]];
				if (at < t1) return v0 + ((v1 - v0) * (at - t0)) / (t1 - t0);
			}
			return pts[pts.length - 1][1];
		};
		for (let i = 0; i < 40; i++) {
			const t = 1 + (i + 0.5) * 0.1;
			expect(Math.abs(sample(after, t) - sample(before, t))).toBeLessThan(0.01);
		}
	});

	test('before the first key and after the last, the key comes as it is', () => {
		const out = insertKeyframe([key(1, 1, 'ease_in'), key(5, 3, 'hold')], key(0, 2, 'hold'));
		expect(out.map((k) => k.easing ?? 'linear')).toEqual(['hold', 'ease_in', 'hold']);
	});
});

test('a bezier with a control point outside the unit square is refused', () => {
	expect(easingProblem(undefined)).toBeUndefined();
	expect(easingProblem('hold')).toBeUndefined();
	expect(easingProblem({ bezier: { x1: 0.2, y1: 1, x2: 0.3, y2: 0 } })).toBeUndefined();
	expect(easingProblem({ bezier: { x1: 0.2, y1: 1.4, x2: 0.3, y2: 0 } })).toContain('no overshoot');
	expect(easingProblem({ bezier: { x1: NaN, y1: 0, x2: 0.3, y2: 0 } })).toContain('no overshoot');
});
