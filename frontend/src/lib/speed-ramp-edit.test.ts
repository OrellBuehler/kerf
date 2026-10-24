import { describe, expect, test } from 'bun:test';
import {
	addKeyAt,
	AXIS_HI,
	AXIS_LO,
	canAddKey,
	keysOf,
	MIN_KEY_GAP,
	moveKey,
	presetKeys,
	RAMP_PRESETS,
	rampSummary,
	removeKey,
	roundSpeed,
	sameKeys,
	setKeyEasing,
	speedAtY,
	speedPath,
	speedRange,
	speedTicks,
	timeAtX,
	timeSpan,
	xOfTime,
	yOfSpeed,
	type SpeedAxis,
	type TimeAxis
} from './speed-ramp-edit';
import { clipDuration, MAX_SPEED_KEYS, rampIsValid, speedAt } from './speed-ramp';
import type { Clip, SpeedKey } from './types';

const key = (time: number, speed: number, easing?: 'linear' | 'hold'): SpeedKey => (easing ? { time, speed, easing } : { time, speed });
const clip = (len: number, extra: Partial<Clip> = {}): Clip => ({
	id: 'c',
	asset_id: 'a',
	source_in: 0,
	source_out: len,
	timeline_start: 5,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});

describe('axes', () => {
	const axis: SpeedAxis = { lo: 0.1, hi: 10, height: 120, pad: 10 };

	test('speed is a log scale: 1x is the middle, the decades the edges', () => {
		expect(yOfSpeed(axis, 1)).toBeCloseTo(60, 9);
		expect(yOfSpeed(axis, 10)).toBeCloseTo(10, 9);
		expect(yOfSpeed(axis, 0.1)).toBeCloseTo(110, 9);
		expect(yOfSpeed(axis, 1000)).toBeCloseTo(10, 9);
	});

	test('a row reads back as the speed that draws on it', () => {
		for (const v of [0.1, 0.3, 1, 2.5, 10]) expect(speedAtY(axis, yOfSpeed(axis, v))).toBeCloseTo(v, 9);
		expect(speedAtY(axis, -50)).toBeCloseTo(10, 9);
		expect(speedAtY(axis, 500)).toBeCloseTo(0.1, 9);
	});

	test('the axis spans a decade each way and widens for a key outside it', () => {
		expect(speedRange([key(0, 1)])).toEqual([AXIS_LO, AXIS_HI]);
		expect(speedRange([key(0, 0.02), key(1, 30)])).toEqual([0.01, 100]);
		expect(speedTicks({ lo: 0.1, hi: 10 })).toEqual([0.1, 1, 10]);
		expect(speedTicks({ lo: 0.01, hi: 100 })).toEqual([0.01, 0.1, 1, 10, 100]);
	});

	test('time maps across the clip and back, held inside it', () => {
		const t: TimeAxis = { span: 4, width: 220, pad: 10 };
		expect(xOfTime(t, 0)).toBe(10);
		expect(xOfTime(t, 4)).toBe(210);
		expect(xOfTime(t, 99)).toBe(210);
		for (const s of [0, 1, 2.5, 4]) expect(timeAtX(t, xOfTime(t, s))).toBeCloseTo(s, 9);
		expect(timeAtX(t, -5)).toBe(0);
	});

	test('the span is the clip, or the last key when that lies past it', () => {
		const c = clip(4, { speed_ramp: [key(0, 1), key(2, 1)] });
		expect(timeSpan(c, keysOf(c))).toBe(4);
		expect(timeSpan(c, [key(0, 1), key(7, 2)])).toBe(7);
	});
});

describe('the curve', () => {
	const x = (t: number) => t * 10;
	const y = (v: number) => 100 - v * 10;

	test('flat before the first key and after the last, a line between', () => {
		expect(speedPath([key(1, 2), key(3, 4)], 5, x, y)).toBe('M0.00 80.00L10.00 80.00L30.00 60.00L50.00 60.00');
	});

	test('a hold runs level to the next key and steps there', () => {
		expect(speedPath([key(0, 1, 'hold'), key(2, 3)], 2, x, y)).toBe('M0.00 90.00L0.00 90.00L20.00 90.00L20.00 70.00L20.00 70.00');
	});

	test('no keys draw nothing', () => {
		expect(speedPath([], 5, x, y)).toBe('');
	});
});

describe('adding a key', () => {
	test('on a clip with no ramp it keeps the constant speed and adds a key at the start too', () => {
		const c = clip(10, { speed: 2 });
		const keys = addKeyAt(c, 3)!;
		expect(keys).toEqual([key(0, 2), key(3, 2)]);
		expect(rampIsValid(keys)).toBe(true);
		const ramped = { ...c, speed_ramp: keys };
		expect(clipDuration(ramped)).toBe(clipDuration(c));
	});

	test('at the very start it is a single key', () => {
		expect(addKeyAt(clip(10), 0)).toEqual([key(0, 1)]);
	});

	test('on a ramp it takes the speed the clip has there, so the curve does not move', () => {
		const c = clip(10, { speed_ramp: [key(0, 1), key(4, 3)] });
		const keys = addKeyAt(c, 1)!;
		expect(keys).toEqual([key(0, 1), key(1, 1.5), key(4, 3)]);
		const after = { ...c, speed_ramp: keys };
		for (let t = 0; t < 5; t += 0.25) expect(speedAt(after, t)).toBeCloseTo(speedAt(c, t), 12);
		expect(clipDuration(after)).toBeCloseTo(clipDuration(c), 9);
	});

	test('inside a hold the new key holds too', () => {
		const c = clip(10, { speed_ramp: [key(0, 0.5, 'hold'), key(4, 3)] });
		const keys = addKeyAt(c, 2)!;
		expect(keys).toEqual([key(0, 0.5, 'hold'), key(2, 0.5, 'hold'), key(4, 3)]);
		const after = { ...c, speed_ramp: keys };
		for (let t = 0; t < 5; t += 0.25) expect(speedAt(after, t)).toBeCloseTo(speedAt(c, t), 12);
	});

	test('not on top of a key, and not past the cap', () => {
		const c = clip(10, { speed_ramp: [key(0, 1), key(4, 3)] });
		expect(addKeyAt(c, 4 + MIN_KEY_GAP / 2)).toBeNull();
		const full = Array.from({ length: MAX_SPEED_KEYS }, (_, i) => key(i, 1));
		expect(canAddKey(full, 100)).toBe(false);
		expect(canAddKey(full.slice(1), 100)).toBe(true);
	});
});

describe('moving and removing keys', () => {
	const keys = [key(0, 1), key(2, 3), key(4, 1)];

	test('a drag keeps a key between its neighbours', () => {
		expect(moveKey(keys, 1, 9, 3, 6)[1].time).toBeCloseTo(4 - MIN_KEY_GAP, 12);
		expect(moveKey(keys, 1, -9, 3, 6)[1].time).toBeCloseTo(MIN_KEY_GAP, 12);
		expect(moveKey(keys, 2, 99, 1, 6)[2].time).toBe(6);
		expect(moveKey(keys, 0, -3, 1, 6)[0].time).toBe(0);
		expect(rampIsValid(moveKey(keys, 1, 9, 3, 6))).toBe(true);
	});

	test('a speed is rounded, held to the engine range and snapped to 1x', () => {
		expect(roundSpeed(0.314)).toBe(0.31);
		expect(roundSpeed(1.02)).toBe(1);
		expect(roundSpeed(0.001)).toBe(0.01);
		expect(roundSpeed(5000)).toBe(100);
		expect(roundSpeed(NaN)).toBe(1);
		expect(moveKey(keys, 1, 2, 0.314, 6)[1].speed).toBe(0.31);
	});

	test('a move leaves the others and the easing alone', () => {
		const held = [key(0, 1, 'hold'), key(2, 3)];
		const moved = moveKey(held, 0, 0.5, 2, 4);
		expect(moved[0]).toEqual(key(0.5, 2, 'hold'));
		expect(moved[1]).toEqual(held[1]);
		expect(held[0]).toEqual(key(0, 1, 'hold'));
	});

	test('removing a key keeps the rest; the last one going clears the ramp', () => {
		expect(removeKey(keys, 1)).toEqual([key(0, 1), key(4, 1)]);
		expect(removeKey([key(0, 1)], 0)).toEqual([]);
	});

	test('easing is set on one key and a repeat is the same list', () => {
		const held = setKeyEasing(keys, 1, 'hold');
		expect(held[1]).toEqual(key(2, 3, 'hold'));
		expect(sameKeys(setKeyEasing(held, 1, 'hold'), held)).toBe(true);
		expect(setKeyEasing(held, 1, 'linear')[1]).toEqual(key(2, 3));
		expect(sameKeys(keys, [key(0, 1), key(2, 3, 'linear'), key(4, 1)])).toBe(true);
		expect(sameKeys(keys, held)).toBe(false);
	});
});

describe('presets', () => {
	const c = clip(10, { speed: 2 });

	test('every preset makes a ramp the engine accepts', () => {
		for (const p of RAMP_PRESETS) {
			for (const at of [0, 0.01, 3, 9.99, 10]) {
				const keys = presetKeys(p, c, at);
				expect(rampIsValid(keys), `${p.id} at ${at}`).toBe(true);
			}
		}
	});

	test('ramp up and down run from the clip speed across its length', () => {
		const [up, down] = [RAMP_PRESETS[0], RAMP_PRESETS[1]];
		expect(presetKeys(up, c, 0)).toEqual([key(0, 2), key(5, 8)]);
		expect(presetKeys(down, c, 0)).toEqual([key(0, 2), key(5, 0.5)]);
	});

	test('slow-mo dips to 0.3x at the playhead and is back to speed half a second either side', () => {
		const slow = RAMP_PRESETS.find((p) => p.id === 'slow-mo')!;
		const k = presetKeys(slow, clip(10), 2.5);
		expect(k).toEqual([key(2, 1), key(2.5, 0.3), key(3, 1)]);
		expect(presetKeys(slow, clip(10), 0)).toEqual([key(0, 0.3), key(0.5, 1)]);
		expect(presetKeys(slow, clip(10), 10)).toEqual([key(9.5, 1), key(10, 0.3)]);
	});

	test('speed up middle is normal at the ends and fast through the centre', () => {
		const mid = RAMP_PRESETS.find((p) => p.id === 'speed-up-middle')!;
		const k = presetKeys(mid, clip(10), 5);
		expect(k.map((x) => x.speed)).toEqual([1, 1, 4, 4, 1]);
		expect(k[0].time).toBe(0);
	});

	test('a preset on a very short clip still yields a valid list', () => {
		const tiny = clip(0.01);
		for (const p of RAMP_PRESETS) expect(rampIsValid(presetKeys(p, tiny, 0)), p.id).toBe(true);
	});
});

describe('words', () => {
	test('a ramp is summed up by its slowest and fastest key', () => {
		expect(rampSummary(clip(4, { speed_ramp: [key(0, 1), key(2, 0.3), key(4, 4)] }))).toBe('Ramp · 0.3×–4×');
		expect(rampSummary(clip(4, { speed_ramp: [key(0, 2)] }))).toBe('Ramp · 2×');
		expect(rampSummary(clip(4))).toBe('');
	});
});
