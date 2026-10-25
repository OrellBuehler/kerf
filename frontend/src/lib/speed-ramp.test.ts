import { describe, expect, test } from 'bun:test';
import {
	along,
	clipDuration,
	clipRamp,
	headSource,
	headTime,
	isRamped,
	localAtAlong,
	MAX_SPEED,
	MAX_SPEED_KEYS,
	meanSpeed,
	rampAlong,
	rampIsValid,
	rampLocalAt,
	rateAutomation,
	rampSpeedAt,
	rebaseRamp,
	sourceToTimeline,
	speedAt,
	tailSource,
	tailTime,
	timelineToSource
} from './speed-ramp';
import type { Clip, SpeedKey } from './types';

const NIL = '00000000-0000-0000-0000-000000000000';
const key = (time: number, speed: number, easing?: 'linear' | 'hold'): SpeedKey => (easing ? { time, speed, easing } : { time, speed });
const clip = (sourceIn: number, sourceOut: number, start: number, extra: Partial<Clip> = {}): Clip => ({
	id: 'c',
	asset_id: NIL,
	source_in: sourceIn,
	source_out: sourceOut,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
/** Rust's `ramped` helper: a clip of `sourceLen` source seconds from 10 at 5 s, holding `keys`. */
const ramped = (keys: SpeedKey[], sourceLen: number): Clip => clip(10, 10 + sourceLen, 5, { speed_ramp: keys });
const timelineEnd = (c: Clip) => c.timeline_start + clipDuration(c);
const close = (a: number, b: number, eps = 1e-9) => expect(Math.abs(a - b)).toBeLessThan(eps);

describe('the map (speed_ramp.rs tests)', () => {
	test('no ramp keeps the constant arithmetic bit for bit', () => {
		const c = clip(1.3, 7.1, 2.0, { speed: 1.7 });
		expect(clipDuration(c)).toBe((7.1 - 1.3) / 1.7);
		expect(along(c, 0.9)).toBe(0.9 * 1.7);
		expect(localAtAlong(c, 0.9)).toBe(0.9 / 1.7);
		expect(timelineToSource(c, 3.0)).toBe(1.3 + (3.0 - 2.0) * 1.7);
		expect(sourceToTimeline(c, 4.0)).toBe(2.0 + (4.0 - 1.3) / 1.7);
	});

	test('a single key is a constant speed', () => {
		const c = ramped([key(3.0, 2.0)], 8.0);
		close(clipDuration(c), 4.0);
		close(along(c, 1.0), 2.0);
		close(speedAt(c, -1.0), 2.0);
	});

	test('a linear segment covers a trapezoid', () => {
		// 1x to 3x over 2 s covers 4 source seconds, then 3x after.
		const c = ramped([key(0, 1), key(2, 3)], 4 + 6);
		close(along(c, 2.0), 4.0);
		close(along(c, 1.0), 1.0 + 0.5);
		close(speedAt(c, 1.0), 2.0);
		close(clipDuration(c), 2.0 + 2.0);
		close(timelineEnd(c), 5.0 + 4.0);
	});

	test('a hold jumps at the next key', () => {
		const c = ramped([key(0, 0.5, 'hold'), key(2, 2)], 1 + 4);
		close(speedAt(c, 1.999), 0.5);
		close(along(c, 2.0), 1.0);
		close(clipDuration(c), 4.0);
	});

	test('the map inverts everywhere including outside the keys', () => {
		const c = ramped([key(0.5, 1), key(1, 0.25), key(2, 0.25, 'hold'), key(2.5, 4), key(3, 1)], 6);
		for (let i = -20; i <= 80; i++) {
			const t = i * 0.05;
			close(localAtAlong(c, along(c, t)), t);
		}
		for (let i = 0; i <= 60; i++) {
			const s = 10 + i * 0.1;
			close(timelineToSource(c, sourceToTimeline(c, s)), s);
		}
	});

	test('a reversed ramp plays the window backwards on the same curve', () => {
		const c = ramped([key(0, 1), key(2, 3)], 10);
		const forward = clipDuration(c);
		c.speed = -1;
		close(clipDuration(c), forward);
		close(timelineToSource(c, 5.0), 20.0);
		close(timelineToSource(c, 5.0 + 2.0), 20.0 - 4.0);
	});

	test('a malformed ramp is not read', () => {
		const base = clip(0, 4, 0);
		const bad: SpeedKey[][] = [
			[key(1, 2), key(1, 3)],
			[key(2, 2), key(1, 3)],
			[key(NaN, 2)],
			[key(0, 0)],
			[key(0, MAX_SPEED * 2)],
			Array.from({ length: MAX_SPEED_KEYS + 1 }, (_, i) => key(i, 1))
		];
		for (const keys of bad) {
			const c = { ...base, speed_ramp: keys };
			expect(isRamped(c)).toBe(false);
			expect(clipRamp(c)).toBeNull();
			expect(clipDuration(c)).toBe(4);
		}
	});
});

describe('the ramp functions', () => {
	test('validity: empty, absent, infinite, in-range and boundary speeds', () => {
		expect(rampIsValid(undefined)).toBe(false);
		expect(rampIsValid([])).toBe(false);
		expect(rampIsValid([key(0, Infinity)])).toBe(false);
		expect(rampIsValid([key(Infinity, 1)])).toBe(false);
		expect(rampIsValid([key(0, 0.01), key(1, 100)])).toBe(true);
		expect(rampIsValid([key(0, 0.0099)])).toBe(false);
		expect(rampIsValid(Array.from({ length: MAX_SPEED_KEYS }, (_, i) => key(i, 1)))).toBe(true);
	});

	test('speed before the first key and after the last is theirs', () => {
		const keys = [key(1, 2), key(3, 0.5)];
		expect(rampSpeedAt(keys, 0)).toBe(2);
		expect(rampSpeedAt(keys, 1)).toBe(2);
		expect(rampSpeedAt(keys, 2)).toBeCloseTo(1.25, 12);
		expect(rampSpeedAt(keys, 3)).toBe(0.5);
		expect(rampSpeedAt(keys, 99)).toBe(0.5);
	});

	test('along is zero at the start and negative before it, at the first key speed', () => {
		const keys = [key(1, 2), key(3, 0.5)];
		expect(rampAlong(keys, 0)).toBeCloseTo(0, 12);
		expect(rampAlong(keys, -1)).toBeCloseTo(-2, 12);
		expect(rampLocalAt(keys, -2)).toBeCloseTo(-1, 12);
	});

	test('a flat linear segment inverts through the near-zero slope branch', () => {
		const keys = [key(0, 2), key(1, 2 + 1e-14)];
		close(rampLocalAt(keys, rampAlong(keys, 0.7)), 0.7);
	});

	test('a hold followed by a ramp inverts inside the hold', () => {
		const keys = [key(0, 0.5, 'hold'), key(2, 4), key(3, 1)];
		for (const t of [0.1, 1.9, 2.0, 2.4, 2.99, 3.5]) close(rampLocalAt(keys, rampAlong(keys, t)), t);
	});

	test('mean speed is the source span over the length', () => {
		const c = ramped([key(0, 1), key(2, 3)], 10);
		close(meanSpeed(c), 10 / 4);
		const plain = clip(0, 10, 0, { speed: -2 });
		expect(meanSpeed(plain)).toBe(2);
		const empty = ramped([key(0, 2)], 0);
		expect(meanSpeed(empty)).toBe(2);
	});
});

describe('head and tail', () => {
	test('headTime and tailTime are the plain / speed unramped', () => {
		const c = clip(1, 5, 0, { speed: 2 });
		expect(headTime(c, 3)).toBe(3 / 2);
		expect(tailTime(c, 3)).toBe(3 / 2);
		expect(headSource(c, 1.5)).toBe(1.5 * 2);
		expect(tailSource(c, 1.5)).toBe(1.5 * 2);
	});

	test('a ramped head extends at the first key speed, a tail at the last', () => {
		const c = ramped([key(0, 2), key(1, 4)], 10);
		close(headTime(c, 4), 2);
		close(tailTime(c, 8), 2);
		close(headSource(c, -2), -4);
	});

	test('a tail shift is the area under the curve across the move', () => {
		const c = ramped([key(0, 1), key(2, 3)], 4 + 6);
		const d = clipDuration(c);
		close(tailSource(c, 1), 3);
		close(tailSource(c, -1), -3);
		close(tailSource(c, -d), -10);
		expect(tailSource(c, 0)).toBe(0);
	});

	test('head then tail trims keep the content on the same curve', () => {
		const c = ramped([key(0, 1), key(2, 3)], 10);
		const by = 1;
		const shift = headSource(c, by);
		close(shift, 1.5);
		const sp = speedAt(c, by);
		rebaseRamp(c, by);
		c.source_in += shift;
		close(speedAt(c, 0), sp);
		close(clipDuration(c), 4 - by);
	});
});

describe('rebaseRamp', () => {
	test('a head trim inside a linear segment pins the speed there and keeps the rest', () => {
		const c = ramped([key(0, 1), key(2, 3), key(4, 1)], 10);
		rebaseRamp(c, 1);
		expect(c.speed_ramp).toEqual([key(0, 2), key(1, 3), key(3, 1)]);
	});

	test('a head trim inside a hold keeps holding', () => {
		const c = ramped([key(0, 0.5, 'hold'), key(2, 2)], 5);
		rebaseRamp(c, 1);
		expect(c.speed_ramp).toEqual([key(0, 0.5, 'hold'), key(1, 2)]);
	});

	test('a head trim exactly on a key drops it and pins its speed and easing', () => {
		const c = ramped([key(0, 1), key(1, 2, 'hold'), key(3, 4)], 5);
		rebaseRamp(c, 1);
		expect(c.speed_ramp).toEqual([key(0, 2, 'hold'), key(2, 4)]);
	});

	test('a head trim past the last key leaves a constant speed', () => {
		const c = ramped([key(0, 1), key(1, 3)], 5);
		rebaseRamp(c, 4);
		expect(c.speed_ramp).toEqual([key(0, 3)]);
	});

	test('a head trim before the first key only shifts', () => {
		const c = ramped([key(2, 1), key(3, 3)], 5);
		rebaseRamp(c, 1);
		expect(c.speed_ramp).toEqual([key(1, 1), key(2, 3)]);
	});

	test('a head extension shifts every key later', () => {
		const c = ramped([key(0, 1), key(2, 3)], 5);
		rebaseRamp(c, -1.5);
		expect(c.speed_ramp).toEqual([key(1.5, 1), key(3.5, 3)]);
	});

	test('the rebased clip plays the remaining footage at the same speeds', () => {
		const c = ramped([key(0.5, 1), key(1, 0.25), key(2, 0.25, 'hold'), key(2.5, 4), key(3, 1)], 6);
		const was = structuredClone(c);
		const by = 1.2;
		const shift = headSource(c, by);
		rebaseRamp(c, by);
		c.source_in += shift;
		c.timeline_start += by;
		for (let t = 0; t < clipDuration(c); t += 0.17) close(speedAt(c, t), speedAt(was, t + by));
		close(timelineEnd(c), timelineEnd(was));
		close(timelineToSource(c, c.timeline_start + 0.4), timelineToSource(was, was.timeline_start + 0.4 + by));
	});

	test('a malformed ramp is not touched', () => {
		const c = ramped([key(1, 2), key(1, 3)], 5);
		rebaseRamp(c, 2);
		expect(c.speed_ramp).toEqual([key(1, 2), key(1, 3)]);
	});
});

describe('rateAutomation', () => {
	/** The distance a player covers following `points` (events in the order it meets them) — what an `AudioParam`
	 *  makes of `setValueAtTime` and `linearRampToValueAtTime` — up to `end`, the rate holding after the last event. */
	function distance(points: ReturnType<typeof rateAutomation>, end: number): number {
		const last = points[points.length - 1];
		let total = last.speed * Math.abs(end - last.time);
		for (let i = 1; i < points.length; i++) {
			const [p, q] = [points[i - 1], points[i]];
			const span = Math.abs(q.time - p.time);
			total += q.step ? p.speed * span : ((p.speed + q.speed) / 2) * span;
		}
		return total;
	}

	const keys = [key(0.5, 1), key(1, 0.25), key(2, 0.25, 'hold'), key(2.5, 4), key(3, 1, 'hold'), key(3.5, 2)];

	test('a player at the automated rate covers what the map says, whichever way it runs', () => {
		for (const [a, b] of [
			[0, 4],
			[0.2, 3.7],
			[1.4, 2.2],
			[2.6, 3.4],
			[0.7, 0.9],
			[3.2, 5]
		]) {
			const want = rampAlong(keys, b) - rampAlong(keys, a);
			expect(distance(rateAutomation(keys, a, b), b) - want).toBeCloseTo(0, 9);
			expect(distance(rateAutomation(keys, a, b, true), a) - want).toBeCloseTo(0, 9);
		}
	});

	test('it starts where the player does, steps at a hold and ramps along a line', () => {
		const up = rateAutomation([key(0, 1, 'hold'), key(2, 3), key(4, 1)], 0.5, 3);
		expect(up).toEqual([
			{ time: 0.5, speed: 1, step: true },
			{ time: 2, speed: 3, step: true },
			{ time: 3, speed: 2, step: false }
		]);
		const down = rateAutomation([key(0, 1, 'hold'), key(2, 3), key(4, 1)], 0.5, 3, true);
		expect(down[0]).toEqual({ time: 3, speed: 2, step: true });
		expect(down.slice(1)).toEqual([
			{ time: 2, speed: 3, step: false },
			{ time: 2, speed: 1, step: true },
			{ time: 0.5, speed: 1, step: false }
		]);
	});

	test('a single key is one constant rate', () => {
		expect(rateAutomation([key(1, 2)], 0, 5)).toEqual([
			{ time: 0, speed: 2, step: true },
			{ time: 1, speed: 2, step: false },
			{ time: 5, speed: 2, step: false }
		]);
	});
});
