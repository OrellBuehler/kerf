import { describe, expect, test } from 'bun:test';
import { alignCutsToBeats } from './beats';
import { handles, MIN_EDIT_CLIP, moveHead, moveTail, rollEdit, rollRange, slideClip, slipClip, splitRemove } from './edit-modes';
import { checkedRamp, setSpeedRampAlone, splitClip } from './links';
import {
	along,
	clipDuration,
	isRamped,
	localAtAlong,
	MAX_RAMP_TIME,
	MAX_SPEED,
	MAX_SPEED_KEYS,
	rampIsValid,
	rebaseRamp,
	speedAt,
	timelineToSource,
	trimRamp
} from './speed-ramp';
import type { Clip, SpeedKey, Timeline } from './types';

// `model/ramp_tests.rs`: every op that moves an edge, splits, slips or slides a clip with a ramp keeps each frame on
// the footage it played before (`assertSameFootage`), which is the whole contract — a ramp is a map, and an edit
// that changes the clip's extent must not change the map where the clip still exists.

const ASSET = 'a';
const key = (time: number, speed: number, easing?: 'linear' | 'hold'): SpeedKey => (easing ? { time, speed, easing } : { time, speed });
/** Fast, then a hold at a crawl, then a push up to double and back: every kind of segment. */
const keys = (): SpeedKey[] => [key(0.5, 1), key(1.5, 0.25, 'hold'), key(2.5, 2), key(3.5, 1)];
let serial = 0;
const ramped = (sourceIn: number, sourceOut: number, start: number, extra: Partial<Clip> = {}): Clip =>
	({ id: `r${++serial}`, asset_id: ASSET, source_in: sourceIn, source_out: sourceOut, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0, speed_ramp: keys(), ...extra }) as Clip;
const lane = (clips: Clip[]) => ({ tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips }] }) as unknown as Timeline;
const footage = (secs: number) => new Map([[ASSET, secs]]);
const near = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-9);
const end = (c: Clip) => c.timeline_start + clipDuration(c);
const pairs = (c: Clip) => (c.speed_ramp ?? []).map((k) => [k.time, k.speed]);

/** Every frame `now` shows (sampled across its length) plays the footage it played in `old`. */
function assertSameFootage(old: Clip, now: Clip) {
	for (let i = 0; i <= 48; i++) {
		const t = now.timeline_start + (clipDuration(now) * i) / 48;
		near(timelineToSource(now, t), timelineToSource(old, t));
	}
}

describe('keys handed in', () => {
	test('are checked and put in order', () => {
		expect(checkedRamp([])).toEqual([]);
		expect(checkedRamp([key(2, 1), key(0, 3)]).map((k) => k.time)).toEqual([0, 2]);
		const refused = (ks: SpeedKey[]) => {
			try {
				checkedRamp(ks);
			} catch (e) {
				return (e as Error).message;
			}
			throw new Error('not refused');
		};
		expect(refused([key(NaN, 1)])).toContain('key 1');
		expect(refused([key(0, 1), key(-1, 1)])).toContain('key 2');
		expect(refused([key(MAX_RAMP_TIME * 2, 1)])).toContain('time');
		expect(refused([key(0, 0)])).toContain('speed');
		expect(refused([key(0, -1)])).toContain('magnitude');
		expect(refused([key(0, MAX_SPEED * 2)])).toContain('speed');
		expect(refused([key(1, 1), key(1, 2)])).toContain('two speed ramp keys');
		expect(refused(Array.from({ length: MAX_SPEED_KEYS + 1 }, (_, i) => key(i, 1)))).toContain('at most');
		// A negative zero is a zero: two keys at "0" are a duplicate whichever sign they wear.
		expect(refused([key(-0, 1), key(0, 2)])).toContain('two speed ramp keys');
	});

	test('a key beyond MAX_RAMP_TIME is not a ramp the map reads', () => {
		expect(rampIsValid([key(0, 1), key(MAX_RAMP_TIME * 10, 2)])).toBe(false);
		expect(rampIsValid([key(-MAX_RAMP_TIME * 10, 1)])).toBe(false);
		expect(rampIsValid([key(-3, 1), key(MAX_RAMP_TIME, 2)])).toBe(true);
	});

	test('setting a ramp keeps the window, changes the length, and a clear needs a ramp', () => {
		const c = { ...ramped(10, 18, 2), speed_ramp: undefined } as Clip;
		const t = lane([c]);
		const set = setSpeedRampAlone(t, c.id, [key(2, 1), key(0, 4)]);
		expect([set.source_in, set.source_out, set.speed ?? 1]).toEqual([10, 18, 1]);
		expect(pairs(set)).toEqual([
			[0, 4],
			[2, 1]
		]);
		// 4x down to 1x over 2 s covers 5 source seconds, the last 3 at 1x: 2 + 3 s.
		near(clipDuration(set), 5);
		const cleared = setSpeedRampAlone(t, c.id, []);
		expect(cleared.speed_ramp).toBeUndefined();
		near(clipDuration(cleared), 8);
		expect(() => setSpeedRampAlone(t, c.id, [])).toThrow('no speed ramp to clear');
		expect(() => setSpeedRampAlone(t, c.id, [key(0, 0)])).toThrow();
		expect(() => setSpeedRampAlone(t, 'nope', [key(0, 1)])).toThrow();
		t.tracks[0].locked = true;
		expect(() => setSpeedRampAlone(t, c.id, [key(0, 1)])).toThrow('locked');
	});
});

describe('the head', () => {
	test('a trim pins the speed the clip opens at and shifts the rest back', () => {
		const c = ramped(10, 22, 5);
		const old = structuredClone(c);
		moveHead(c, 1, false);
		// 1.0 s is halfway down the first run, 1x to 0.25x: 0.625x. The hold's key keeps its easing.
		expect(pairs(c)).toEqual([
			[0, 0.625],
			[0.5, 0.25],
			[1.5, 2],
			[2.5, 1]
		]);
		expect(c.speed_ramp![1].easing).toBe('hold');
		expect(c.timeline_start).toBe(6);
		near(end(c), end(old));
		assertSameFootage(old, c);
	});

	test('a cut inside a hold keeps holding, and one on a key keeps that key’s segment', () => {
		const old = ramped(10, 22, 5);
		const inside = structuredClone(old);
		moveHead(inside, 2, false);
		expect(pairs(inside)).toEqual([
			[0, 0.25],
			[0.5, 2],
			[1.5, 1]
		]);
		expect(inside.speed_ramp![0].easing).toBe('hold');
		assertSameFootage(old, inside);
		const onKey = structuredClone(old);
		moveHead(onKey, 1.5, false);
		expect(pairs(onKey)).toEqual([
			[0, 0.25],
			[1, 2],
			[2, 1]
		]);
		expect(onKey.speed_ramp![0].easing).toBe('hold');
		assertSameFootage(old, onKey);
	});

	test('a cut before the first key pins nothing, and one past the last leaves its speed', () => {
		const old = ramped(10, 22, 5);
		const early = structuredClone(old);
		moveHead(early, 0.25, false);
		expect(pairs(early)).toEqual([
			[0.25, 1],
			[1.25, 0.25],
			[2.25, 2],
			[3.25, 1]
		]);
		assertSameFootage(old, early);
		const late = structuredClone(old);
		moveHead(late, 6, false);
		expect(pairs(late)).toEqual([[0, 1]]);
		expect(late.speed_ramp![0].easing).toBeUndefined();
		assertSameFootage(old, late);
	});

	test('pulling the head out shifts every key and the new head runs at the first speed', () => {
		const old = ramped(10, 22, 5);
		const c = structuredClone(old);
		moveHead(c, -1, false);
		expect(pairs(c)).toEqual([
			[1.5, 1],
			[2.5, 0.25],
			[3.5, 2],
			[4.5, 1]
		]);
		expect(c.timeline_start).toBe(4);
		near(c.source_in, 9);
		near(end(c), end(old));
		assertSameFootage(old, c);
	});

	test('the key cap holds through any head trim, and colliding keys are dropped, not the ramp', () => {
		const full = ramped(0, 100, 0, { speed_ramp: Array.from({ length: MAX_SPEED_KEYS }, (_, i) => key(i * 0.5, 1 + i * 0.1)) });
		for (const by of [0, 0.1, 0.5, 0.75, 3, 7.5, 9, -2]) {
			const c = structuredClone(full);
			rebaseRamp(c, by);
			expect(isRamped(c) && c.speed_ramp!.length <= MAX_SPEED_KEYS, `by ${by}`).toBe(true);
		}
		const close = ramped(0, 100, 0, { speed_ramp: [key(1, 1), key(1 + Number.EPSILON, 2)] });
		expect(isRamped(close)).toBe(true);
		rebaseRamp(close, -1000);
		expect(isRamped(close)).toBe(true);
		expect(close.speed_ramp).toHaveLength(1);
	});

	test('a trim that wrote the window directly takes the ramp along', () => {
		const old = ramped(10, 22, 5);
		const c = structuredClone(old);
		c.source_in = 11.2;
		trimRamp(old, c);
		const head = localAtAlong(old, 1.2);
		near(speedAt(c, 0), speedAt(old, head));
		near(clipDuration(c) + head, clipDuration(old));
		const reversed = { ...structuredClone(old), speed: -1 };
		const r = structuredClone(reversed);
		r.source_out = 20.8;
		trimRamp(reversed, r);
		near(speedAt(r, 0), speedAt(reversed, localAtAlong(reversed, 1.2)));
		// A tail trim, or a clip without a ramp, is none of its business.
		const tail = structuredClone(old);
		tail.source_out = 18;
		trimRamp(old, tail);
		expect(tail.speed_ramp).toEqual(old.speed_ramp);
		const plain = { ...ramped(0, 4, 0), speed_ramp: undefined, speed: 2 } as Clip;
		const was = structuredClone(plain);
		plain.source_in = 1;
		trimRamp(was, plain);
		expect(plain.speed_ramp).toBeUndefined();
	});
});

describe('the tail and the handles', () => {
	test('a tail move leaves the keys and runs on at the last speed', () => {
		const old = ramped(10, 20, 5);
		const shorter = structuredClone(old);
		moveTail(shorter, -1, false);
		expect(shorter.speed_ramp).toEqual(old.speed_ramp);
		near(clipDuration(shorter), clipDuration(old) - 1);
		assertSameFootage(old, shorter);
		const longer = structuredClone(old);
		moveTail(longer, 2, false);
		near(clipDuration(longer), clipDuration(old) + 2);
		// Past the last key (3.5 s in) the ramp holds 1x, and the extra seconds are source seconds.
		expect(clipDuration(old)).toBeGreaterThan(3.5);
		near(longer.source_out - old.source_out, 2);
		assertSameFootage(longer, old);
	});

	test('a tail move inside the ramp takes the footage the ramp plays there', () => {
		const c = ramped(10, 11, 0);
		const before = clipDuration(c);
		moveTail(c, 1, false);
		near(clipDuration(c), before + 1);
		near(along(c, clipDuration(c)), c.source_out - c.source_in);
		const back = structuredClone(c);
		moveTail(back, -1, false);
		near(back.source_out, 11);
	});

	test('the handles are the timeline the footage either side plays for', () => {
		for (const reversed of [false, true]) {
			const old = ramped(4, 9, 3, reversed ? { speed: -1 } : {});
			const [head, tail] = handles(old, 30);
			const pulled = structuredClone(old);
			moveHead(pulled, -head, false);
			const extended = structuredClone(old);
			moveTail(extended, tail, false);
			if (reversed) {
				near(pulled.source_out, 30);
				near(extended.source_in, 0);
			} else {
				near(pulled.source_in, 0);
				near(extended.source_out, 30);
			}
			assertSameFootage(old, pulled);
			assertSameFootage(extended, old);
		}
		expect(handles(ramped(4, 9, 3), Infinity)).toEqual([Infinity, Infinity]);
	});
});

describe('splits and the edit modes', () => {
	test('a split keeps every frame’s source time on both halves', () => {
		for (const reversed of [false, true]) {
			for (const at of [5.2, 6, 6.7, 7, 7.9, 9.5, 12]) {
				const c = ramped(10, 22, 5, reversed ? { speed: -2 } : {});
				const total = end(c);
				const t = lane([structuredClone(c)]);
				const [left, right] = splitClip(t, c.id, at);
				near(end(left), at);
				near(end(right), total);
				near(right.timeline_start, end(left));
				assertSameFootage(c, left);
				assertSameFootage(c, right);
				near(speedAt(right, 0), speedAt(c, at - c.timeline_start));
				// The halves tile the window: nothing played twice, nothing dropped.
				near(Math.min(left.source_in, right.source_in), c.source_in);
				near(Math.max(left.source_out, right.source_out), c.source_out);
				near(left.source_out - left.source_in + (right.source_out - right.source_in), c.source_out - c.source_in);
			}
		}
	});

	test('split and remove keeps the half that stays exactly', () => {
		const old = ramped(10, 22, 5);
		for (const at of [5.6, 6.5, 7.2, 9]) {
			for (const side of ['left', 'right'] as const) {
				const t = lane([structuredClone(old)]);
				const kept = splitRemove(t, old.id, at, side);
				assertSameFootage(old, kept);
				if (side === 'left') {
					near(kept.timeline_start, at);
					near(end(kept), end(old));
				} else {
					near(kept.timeline_start, old.timeline_start);
					near(end(kept), at);
				}
			}
		}
	});

	/** `a` (src 10..16) and `b` (src 40..46), both on the ramp, abutting at `a`'s end. */
	function abutting(): [Clip, Clip] {
		const a = ramped(10, 16, 0);
		return [a, ramped(40, 46, end(a))];
	}

	test('a roll trades footage at the cut and both clips keep what they show', () => {
		const [a, b] = abutting();
		for (const delta of [0.3, -0.4, 1.2, -1]) {
			const t = lane([structuredClone(a), structuredClone(b)]);
			const out = rollEdit(t, a.id, b.id, delta, footage(100));
			expect(out.clamped).toBe(false);
			const [na, nb] = t.tracks[0].clips;
			near(end(na), end(a) + delta);
			near(nb.timeline_start, end(na));
			near(end(nb), end(b));
			assertSameFootage(a, na);
			assertSameFootage(b, nb);
		}
	});

	test('a roll stops where a ramped clip runs out of footage', () => {
		// `a` has 1 source second after its out-point, played at the last key's 1x: 1 timeline second.
		const [a, b] = abutting();
		const t = lane([structuredClone(a), structuredClone(b)]);
		const range = rollRange(t, a.id, b.id, footage(17));
		near(range.max, 1);
		const out = rollEdit(t, a.id, b.id, 5, footage(17));
		expect(out.clamped).toBe(true);
		near(out.applied, 1);
		near(t.tracks[0].clips[0].source_out, 17);
		// Earlier, `a` may shrink to the floor.
		near(range.min, -(clipDuration(a) - MIN_EDIT_CLIP));
	});

	test('a slip keeps the length and moves every frame by the same footage', () => {
		for (const reversed of [false, true]) {
			const c = ramped(10, 22, 5, reversed ? { speed: -1 } : {});
			const [start, len] = [c.timeline_start, clipDuration(c)];
			const t = lane([structuredClone(c)]);
			const out = slipClip(t, c.id, 3, footage(100));
			expect(out.clamped).toBe(false);
			const slipped = t.tracks[0].clips[0];
			near(clipDuration(slipped), len);
			expect(slipped.timeline_start).toBe(start);
			expect(slipped.speed_ramp).toEqual(c.speed_ramp);
			const shift = reversed ? -3 : 3;
			for (let i = 0; i <= 24; i++) {
				const x = start + (len * i) / 24;
				near(timelineToSource(slipped, x), timelineToSource(c, x) + shift);
			}
		}
	});

	test('a slide trades footage with ramped neighbours and keeps the middle clip', () => {
		const prev = ramped(10, 16, 0);
		const mid = { ...ramped(50, 53, end(prev)), speed_ramp: undefined } as Clip;
		const next = ramped(40, 46, end(mid));
		for (const delta of [0.5, -0.5, 1.5]) {
			const t = lane([structuredClone(prev), structuredClone(mid), structuredClone(next)]);
			const out = slideClip(t, mid.id, delta, footage(100));
			expect(out.clamped, `${delta}`).toBe(false);
			const [p, m, n] = t.tracks[0].clips;
			near(m.timeline_start, mid.timeline_start + delta);
			near(end(p), m.timeline_start);
			near(n.timeline_start, end(m));
			near(end(n), end(next));
			assertSameFootage(prev, p);
			assertSameFootage(next, n);
		}
	});

	test('a beat snap ends a ramped clip on the beat with the footage the ramp plays there', () => {
		const old = ramped(10, 16, 0);
		const clips = [structuredClone(old)];
		const target = end(old) + 0.7;
		const moved = alignCutsToBeats(clips, [0, target], 1, () => 100);
		expect(moved).toBe(1);
		near(end(clips[0]), target);
		assertSameFootage(clips[0], old);
		expect(clips[0].speed_ramp).toEqual(old.speed_ramp);
	});
});
