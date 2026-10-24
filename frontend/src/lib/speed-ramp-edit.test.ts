import { describe, expect, test } from 'bun:test';
import {
	addKeyAt,
	AXIS_HI,
	AXIS_LO,
	canAddKey,
	keysOf,
	MIN_KEY_GAP,
	moveKey,
	planRampWrite,
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
import { clipById, linkClips } from './link-groups';
import { setSpeedRampLinked } from './links';
import type { Clip, SpeedKey, Timeline } from './types';

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

describe('the keys of a clip', () => {
	test('keys a linked partner holds before its start are folded into a key at 0', () => {
		const c = clip(10, { speed_ramp: [key(-1.5, 1), key(-0.5, 0.4), key(0.5, 0.4, 'hold'), key(1.5, 1.5)] });
		expect(keysOf(c)).toEqual([key(0, 0.4), key(0.5, 0.4, 'hold'), key(1.5, 1.5)]);
		for (let t = 0; t < 3; t += 0.2) expect(speedAt({ ...c, speed_ramp: keysOf(c) }, t)).toBeCloseTo(speedAt(c, t), 12);
		const on = clip(10, { speed_ramp: [key(-1, 1), key(0, 0.5, 'hold'), key(2, 2)] });
		expect(keysOf(on)).toEqual([key(0, 0.5, 'hold'), key(2, 2)]);
		const inside = clip(10, { speed_ramp: [key(-2, 1), key(2, 3)] });
		expect(keysOf(inside)).toEqual([key(0, 2), key(2, 3)]);
		const plain = clip(10, { speed_ramp: [key(0, 1), key(2, 3)] });
		expect(keysOf(plain)).toEqual([key(0, 1), key(2, 3)]);
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

	test('between neighbours too close to leave room the key keeps its time, so the order holds', () => {
		const tight = [key(0, 1), key(0.01, 2), key(0.019, 3), key(4, 1)];
		for (const to of [-5, 0, 0.015, 0.019, 3, 99]) {
			const moved = moveKey(tight, 1, to, 5, 6);
			expect(moved.map((k) => k.time)).toEqual([0, 0.01, 0.019, 4]);
			expect(rampIsValid(moved)).toBe(true);
			expect(moved[1].speed).toBe(5);
		}
		const first = [key(0.005, 1), key(0.01, 2)];
		expect(moveKey(first, 0, 3, 1, 6)[0].time).toBe(0.005);
		expect(rampIsValid(moveKey(first, 0, 3, 1, 6))).toBe(true);
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

// An edit made on a clip that starts after a linked partner must not clobber the partner's own head: the
// write goes through the earliest partner, so `set_speed_ramp_linked`'s shift gives every member the curve.
describe('planRampWrite', () => {
	let serial = 0;
	const member = (name: string, start: number, extra: Partial<Clip> = {}): Clip =>
		clip(8, { id: name, timeline_start: start, ...extra });
	/** Clips `[name, start, extra]` on one lane each, linked. */
	function group(...members: [string, number, Partial<Clip>?][]): Timeline {
		const t = {
			tracks: members.map(([name, start, extra], i) => ({
				id: `t${i}-${++serial}`,
				kind: i === 0 ? 'video' : 'audio',
				name: `T${i}`,
				clips: [member(name, start, extra)]
			}))
		} as unknown as Timeline;
		linkClips(t, members.map(([n]) => n));
		return t;
	}
	const at = (t: Timeline, id: string) => clipById(t, id)!;
	/** Every member's speed at timeline time `T`, from `from` on. */
	const curve = (t: Timeline, id: string, T: number) => speedAt(at(t, id), T - at(t, id).timeline_start);
	const apply = (t: Timeline, id: string, next: SpeedKey[]) => {
		const w = planRampWrite(t, id, next)!;
		expect(w).not.toBeNull();
		setSpeedRampLinked(t, w.clipId, w.keys);
		return w;
	};
	const N = [key(0, 1), key(1, 0.2), key(3, 1)];

	function repro(): Timeline {
		const t = group(['n', 0], ['p', 1]);
		setSpeedRampLinked(t, 'n', N);
		return t;
	}

	test('the later partner holds the earlier clip’s keys before its start, folded into a key at 0', () => {
		const t = repro();
		expect(at(t, 'p').speed_ramp).toEqual([key(-1, 1), key(0, 0.2), key(2, 1)]);
		expect(keysOf(at(t, 'p'))).toEqual([key(0, 0.2), key(2, 1)]);
	});

	test('a write that changes nothing is not sent, whoever it is on', () => {
		const t = repro();
		const p = keysOf(at(t, 'p'));
		expect(planRampWrite(t, 'p', p)).toBeNull();
		expect(planRampWrite(t, 'p', setKeyEasing(p, 0, 'linear'))).toBeNull();
		expect(planRampWrite(t, 'n', N.map((k) => ({ ...k })))).toBeNull();
		expect(planRampWrite(t, 'n', setKeyEasing(N, 1, 'linear'))).toBeNull();
		expect(planRampWrite(t, 'p', setKeyEasing(p, 0, 'hold'))).not.toBeNull();
		const plain = group(['a', 0], ['b', 1]);
		expect(planRampWrite(plain, 'a', [])).toBeNull();
	});

	test('a later partner’s edit goes through the earliest one and leaves its earlier keys', () => {
		const t = repro();
		const before = structuredClone(t);
		const next = moveKey(keysOf(at(t, 'p')), 1, 2.5, 0.5, 4);
		const w = apply(t, 'p', next);
		expect(w.clipId).toBe('n');
		expect(at(t, 'n').speed_ramp![0]).toEqual(key(0, 1));
		// The partner has the keys it was given, to rounding…
		const got = keysOf(at(t, 'p'));
		expect(got.map((k) => k.time)).toEqual(next.map((k) => expect.closeTo(k.time, 9)));
		expect(got.map((k) => k.speed)).toEqual(next.map((k) => expect.closeTo(k.speed, 9)));
		// …the earlier clip still opens as it did, and both play one curve from the partner's start on.
		for (const T of [0, 0.25, 0.5, 0.99]) expect(curve(t, 'n', T)).toBeCloseTo(curve(before, 'n', T), 9);
		for (const T of [1, 1.4, 2, 3.5, 4.6, 6]) expect(curve(t, 'n', T)).toBeCloseTo(curve(t, 'p', T), 9);
		// Written from the partner as the authority, the same edit loses the earlier key (the bug this routes around).
		const naive = structuredClone(before);
		setSpeedRampLinked(naive, 'p', next);
		expect(at(naive, 'n').speed_ramp![0].time).not.toBe(0);
	});

	test('a key added from the later partner, and a preset, route the same way', () => {
		for (const make of [
			(t: Timeline) => addKeyAt(at(t, 'p'), 1.5)!,
			(t: Timeline) => presetKeys(RAMP_PRESETS[0], at(t, 'p'), 0)
		]) {
			const t = repro();
			const next = make(t);
			expect(apply(t, 'p', next).clipId).toBe('n');
			expect(at(t, 'n').speed_ramp![0]).toEqual(key(0, 1));
			for (const T of [1, 1.7, 2.6, 4, 7]) expect(curve(t, 'p', T)).toBeCloseTo(speedAt({ ...at(t, 'p'), speed_ramp: next }, T - 1), 9);
			for (const T of [1, 1.7, 2.6, 4, 7]) expect(curve(t, 'n', T)).toBeCloseTo(curve(t, 'p', T), 9);
		}
	});

	test('a first key after the start still leaves the head flat, and the earlier keys alone before it', () => {
		const t = repro();
		const next = moveKey(keysOf(at(t, 'p')), 0, 0.75, 0.4, 4);
		expect(next[0].time).toBe(0.75);
		apply(t, 'p', next);
		for (const T of [1, 1.3, 1.75, 2.4, 3.5, 5]) expect(curve(t, 'p', T)).toBeCloseTo(speedAt({ ...at(t, 'p'), speed_ramp: next }, T - 1), 9);
		expect(curve(t, 'p', 1)).toBeCloseTo(0.4, 9);
		expect(curve(t, 'n', 0)).toBe(1); // 1x opening kept; it eases to the 0.4x the partner now opens on
		expect(curve(t, 'n', 0.25)).toBeCloseTo(1 - 0.6 * 0.25, 9);
	});

	test('the ratio between members is divided out and put back: a partner at another pace gets its own speeds', () => {
		const t = group(['n', 0], ['p', 1, { speed: 2 }]);
		setSpeedRampLinked(t, 'n', N);
		expect(at(t, 'p').speed_ramp!.map((k) => k.speed)).toEqual([2, 0.4, 2]);
		const next = moveKey(keysOf(at(t, 'p')), 1, 1.5, 0.6, 4);
		const w = apply(t, 'p', next);
		expect(w.clipId).toBe('n');
		const got = keysOf(at(t, 'p'));
		got.forEach((k, i) => {
			expect(k.time).toBeCloseTo(next[i].time, 9);
			expect(k.speed).toBeCloseTo(next[i].speed, 9);
		});
		expect(at(t, 'n').speed_ramp![0]).toEqual(key(0, 1));
		for (const T of [1, 2, 2.5, 4]) expect(curve(t, 'p', T)).toBeCloseTo(2 * curve(t, 'n', T), 9);
	});

	test('with three members the earliest speaks for all of them', () => {
		const t = group(['a', 0], ['b', 0.5], ['c', 1.25]);
		setSpeedRampLinked(t, 'a', N);
		const next = addKeyAt(at(t, 'c'), 1)!;
		expect(apply(t, 'c', next).clipId).toBe('a');
		for (const T of [1.25, 1.9, 2.25, 3, 4.5, 7]) {
			const want = speedAt({ ...at(t, 'c'), speed_ramp: next }, T - 1.25);
			for (const id of ['a', 'b', 'c']) expect(curve(t, id, T)).toBeCloseTo(want, 9);
		}
		expect(at(t, 'a').speed_ramp![0]).toEqual(key(0, 1));
	});

	test('the earliest clip, a clip alone, and a clear write straight', () => {
		const t = repro();
		const next = moveKey(N, 1, 1.5, 0.3, 4);
		expect(planRampWrite(t, 'n', next)).toEqual({ clipId: 'n', keys: next });
		expect(planRampWrite(t, 'p', [])).toEqual({ clipId: 'p', keys: [] });
		const solo = clip(8, { id: 'solo', speed_ramp: [key(0, 1), key(2, 2)] });
		const lone = { tracks: [{ id: 't', kind: 'video', name: 'V1', clips: [solo] }] } as unknown as Timeline;
		expect(planRampWrite(lone, 'solo', [key(0, 1), key(3, 2)])).toEqual({ clipId: 'solo', keys: [key(0, 1), key(3, 2)] });
		const ties = group(['x', 2], ['y', 2]);
		expect(planRampWrite(ties, 'y', [key(0, 1), key(1, 2)])?.clipId).toBe('y');
	});
});
