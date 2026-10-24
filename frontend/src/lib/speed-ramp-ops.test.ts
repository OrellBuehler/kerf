import { describe, expect, test } from 'bun:test';
import * as ops from './link-ops';
import { handles, rollEdit, slideClip, slipClip, splitRemove } from './edit-modes';
import { splitClip, cutClipRange, detachAudio } from './links';
import { along, clipDuration, isRamped, localAtAlong, speedAt, timelineToSource } from './speed-ramp';
import { trimBounds } from './ripple-trim';
import { trimEdit } from './frames';
import type { Clip, SpeedKey, Timeline } from './types';

// What the harness does with a speed ramp: the keys it keeps, the lengths it changes, the partners it
// carries them to, and how every edit that moves a clip's edge rebases them (`speed_ramp.rs`'s rules).

const key = (time: number, speed: number, easing?: 'linear' | 'hold'): SpeedKey => (easing ? { time, speed, easing } : { time, speed });
const clip = (id: string, start: number, src: number, len = 8, extra: Partial<Clip> = {}): Clip =>
	({ id, asset_id: 'a', source_in: src, source_out: src + len, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0, ...extra }) as Clip;
const close = (a: number, b: number, eps = 1e-9) => expect(Math.abs(a - b)).toBeLessThan(eps);

/** V1 holding `a [0,..)` and `b` right after it (a 60 s asset); A1 a linked sound for `a`. */
function cut(ramp?: SpeedKey[]): Timeline {
	const a = clip('a', 0, 10, 8, { link_id: 'g', ...(ramp ? { speed_ramp: ramp } : {}) });
	const sound = clip('s', 0, 10, 8, { link_id: 'g', ...(ramp ? { speed_ramp: ramp } : {}) });
	const b = clip('b', clipDuration(a), 30, 4);
	return {
		tracks: [
			{ id: 'v1', kind: 'video', name: 'V1', clips: [a, b] },
			{ id: 'a1', kind: 'audio', name: 'A1', clips: [sound] }
		]
	} as unknown as Timeline;
}

const env = (over: Partial<ops.EditEnv> = {}): ops.EditEnv => ({
	ripple: false,
	links: true,
	footage: new Map([['a', 60]]),
	hasAudio: () => true,
	...over
});
const clipOf = (t: Timeline, id: string) => t.tracks.flatMap((x) => x.clips).find((c) => c.id === id)!;

const RAMP = [key(0, 1), key(4, 3)];

describe('set_speed_ramp', () => {
	test('writes the keys sorted, keeps the window and changes the length', () => {
		const done = ops.setSpeedRamp(cut(), env(), 'a', [key(4, 3), key(0, 1)]);
		const a = clipOf(done.timeline, 'a');
		expect(a.speed_ramp).toEqual([key(0, 1), key(4, 3)]);
		expect([a.source_in, a.source_out]).toEqual([10, 18]);
		expect(isRamped(a)).toBe(true);
		close(clipDuration(a), 4); // 1x to 3x over 4 s covers the 8 source seconds exactly
		expect(done.label).toBe('Set speed ramp');
	});

	test('a ramp that covers more footage than the clip has is shorter than the clip', () => {
		const a = clipOf(ops.setSpeedRamp(cut(), env(), 'a', [key(0, 2)]).timeline, 'a');
		close(clipDuration(a), 4);
	});

	test('empty keys clear it, on the partner too', () => {
		const ramped = ops.setSpeedRamp(cut(), env(), 'a', RAMP).timeline;
		const cleared = ops.setSpeedRamp(ramped, env(), 'a', []).timeline;
		expect(clipOf(cleared, 'a').speed_ramp).toBeUndefined();
		expect(clipOf(cleared, 's').speed_ramp).toBeUndefined();
		expect(clipDuration(clipOf(cleared, 'a'))).toBe(8);
	});

	test('linked partners get the same ramp; link false leaves them', () => {
		const linked = ops.setSpeedRamp(cut(), env(), 'a', RAMP).timeline;
		expect(clipOf(linked, 's').speed_ramp).toEqual(RAMP);
		const alone = ops.setSpeedRamp(cut(), env({ links: false }), 'a', RAMP).timeline;
		expect(clipOf(alone, 's').speed_ramp).toBeUndefined();
		expect(clipOf(alone, 'a').speed_ramp).toEqual(RAMP);
	});

	test('with ripple on, what follows moves by the change in length', () => {
		const t = ops.setSpeedRamp(cut(), env({ ripple: true }), 'a', RAMP).timeline;
		close(clipOf(t, 'b').timeline_start, clipDuration(clipOf(t, 'a')));
		close(clipOf(t, 'b').timeline_start, 4);
		const off = ops.setSpeedRamp(cut(), env({ ripple: false }), 'a', RAMP).timeline;
		expect(clipOf(off, 'b').timeline_start).toBe(8);
	});

	test('a malformed list is refused with nothing changed', () => {
		const t = cut();
		const before = structuredClone(t);
		const bad: [string, SpeedKey[]][] = [
			['finite', [key(NaN, 1)]],
			['finite', [key(0, Infinity)]],
			['share', [key(1, 1), key(1, 2)]],
			['between', [key(0, 0.001)]],
			['between', [key(0, 101)]],
			['at most', Array.from({ length: 17 }, (_, i) => key(i, 1))]
		];
		for (const [why, keys] of bad) expect(() => ops.setSpeedRamp(t, env(), 'a', keys)).toThrow(why);
		expect(t).toEqual(before);
		expect(() => ops.setSpeedRamp(t, env(), 'a', Array.from({ length: 16 }, (_, i) => key(i, 1)))).not.toThrow();
		expect(() => ops.setSpeedRamp(t, env(), 'nope', RAMP)).toThrow('clip not found');
	});

	test('a locked track refuses it, and a locked partner refuses the lot', () => {
		const locked = cut();
		locked.tracks[0].locked = true;
		expect(() => ops.setSpeedRamp(locked, env(), 'a', RAMP)).toThrow('track V1 is locked');
		const partner = cut();
		partner.tracks[1].locked = true;
		expect(() => ops.setSpeedRamp(partner, env(), 'a', RAMP)).toThrow('A1');
	});

	test('a constant speed clears the ramp, the partner’s too', () => {
		const ramped = ops.setSpeedRamp(cut(), env(), 'a', RAMP).timeline;
		const t = ops.setSpeed(ramped, env(), 'a', 2).timeline;
		for (const id of ['a', 's']) {
			expect(clipOf(t, id).speed_ramp).toBeUndefined();
			expect(isRamped(clipOf(t, id))).toBe(false);
		}
		expect(clipDuration(clipOf(t, 'a'))).toBe(4);
		const single = ops.setSpeed(ramped, env({ links: false }), 'a', 2).timeline;
		expect(clipOf(single, 'a').speed_ramp).toBeUndefined();
		expect(clipOf(single, 's').speed_ramp).toEqual(RAMP);
	});

	test('the direction is the speed’s sign and a ramp keeps it', () => {
		const t = cut();
		clipOf(t, 'a').speed = -1;
		const done = ops.setSpeedRamp(t, env({ links: false }), 'a', RAMP).timeline;
		expect(clipOf(done, 'a').speed).toBe(-1);
		close(timelineToSource(clipOf(done, 'a'), 0), 18);
	});
});

describe('a split', () => {
	test('cuts the footage where the ramp has reached it and rebases the right half', () => {
		const t = cut(RAMP);
		const [left, right] = splitClip(t, 'a', 1);
		close(left.source_out, 10 + along(left, 1));
		expect(right.source_in).toBe(left.source_out);
		expect(left.speed_ramp).toEqual(RAMP);
		expect(right.speed_ramp).toEqual([key(0, 1.5), key(3, 3)]);
		const whole = cut(RAMP).tracks[0].clips[0];
		close(clipDuration(left) + clipDuration(right), clipDuration(whole));
		close(right.timeline_start, 1);
		for (let u = 0; u < clipDuration(right); u += 0.3) close(speedAt(right, u), speedAt(whole, u + 1), 1e-9);
	});

	test('the halves of a reversed ramp play the footage backwards in order', () => {
		const t = cut(RAMP);
		clipOf(t, 'a').speed = -1;
		const whole = structuredClone(clipOf(t, 'a'));
		const [left, right] = splitClip(t, 'a', 1);
		close(right.source_out, left.source_in);
		close(timelineToSource(right, 1), timelineToSource(whole, 1));
		close(timelineToSource(left, 0.5), timelineToSource(whole, 0.5));
		close(timelineToSource(right, 3), timelineToSource(whole, 3));
	});

	test('the partner is split the same way', () => {
		const done = ops.splitAt(cut(RAMP), env(), 'a', 1).timeline;
		const rights = done.tracks.flatMap((x) => x.clips).filter((c) => c.timeline_start === 1);
		expect(rights).toHaveLength(2);
		for (const c of rights) expect(c.speed_ramp).toEqual([key(0, 1.5), key(3, 3)]);
	});
});

describe('trims', () => {
	test('a head trim rebases the ramp, so the tail stays put', () => {
		const t = cut(RAMP);
		const was = clipDuration(clipOf(t, 'a'));
		const e = trimEdit(clipOf(t, 'a'), 'l', 1);
		const done = ops.trim(t, env({ links: false }), 'a', e.source_in, undefined, e.timeline_start).timeline;
		const a = clipOf(done, 'a');
		expect(a.speed_ramp).toEqual([key(0, 1.5), key(3, 3)]);
		close(a.timeline_start + clipDuration(a), was);
		close(a.source_in, 10 + along(clipOf(t, 'a'), 1));
	});

	test('a tail trim drags the end where the pointer is, leaving the ramp', () => {
		const t = cut(RAMP);
		const e = trimEdit(clipOf(t, 'a'), 'r', 3);
		const a = clipOf(ops.trim(t, env({ links: false }), 'a', undefined, e.source_out).timeline, 'a');
		expect(a.speed_ramp).toEqual(RAMP);
		close(clipDuration(a), 3);
	});

	test('a linked trim carries the head to the partner and rebases its ramp too', () => {
		const t = cut(RAMP);
		const e = trimEdit(clipOf(t, 'a'), 'l', 1);
		const done = ops.trim(t, env(), 'a', e.source_in, undefined, e.timeline_start).timeline;
		const [a, s] = [clipOf(done, 'a'), clipOf(done, 's')];
		expect(s.speed_ramp).toEqual(a.speed_ramp!);
		close(s.timeline_start, a.timeline_start);
		close(s.source_in, a.source_in);
	});

	test('the edge bounds are the time the ramp takes to play the handles', () => {
		const z = clip('z', 30, 10, 8, { speed_ramp: RAMP });
		const lane = [z];
		// 10 s of footage before the in point at the first key's 1x; 42 s after the out point at the last key's 3x.
		expect(trimBounds(z, 'l', lane, 60, false, true).min).toBeCloseTo(30 - 10, 9);
		expect(trimBounds(z, 'r', lane, 60, false, true).max).toBeCloseTo(30 + 4 + 14, 9);
	});
});

describe('slip, slide, roll', () => {
	test('a slip keeps the keys and the length', () => {
		const t = cut(RAMP);
		const a0 = structuredClone(clipOf(t, 'a'));
		slipClip(t, 'a', 3, new Map([['a', 60]]));
		const a = clipOf(t, 'a');
		expect(a.speed_ramp).toEqual(RAMP);
		close(clipDuration(a), clipDuration(a0));
		expect(a.source_in).toBe(13);
	});

	test('a roll keeps the boundary: one clip gains what the other loses, on its own curve', () => {
		const t: Timeline = {
			tracks: [
				{
					id: 'v1',
					kind: 'video',
					name: 'V1',
					clips: [clip('x', 0, 10, 8, { speed_ramp: [key(0, 1), key(4, 3)] }), clip('y', 4, 30, 8, { speed_ramp: [key(0, 2), key(3, 0.5)] })]
				}
			]
		} as unknown as Timeline;
		const [x0, y0] = [structuredClone(clipOf(t, 'x')), structuredClone(clipOf(t, 'y'))];
		const endY = y0.timeline_start + clipDuration(y0);
		rollEdit(t, 'x', 'y', 0.5, new Map([['a', 60]]));
		const [x, y] = [clipOf(t, 'x'), clipOf(t, 'y')];
		close(x.timeline_start + clipDuration(x), x0.timeline_start + clipDuration(x0) + 0.5, 1e-9);
		close(y.timeline_start, x.timeline_start + clipDuration(x), 1e-9);
		close(y.timeline_start + clipDuration(y), endY, 1e-6);
		expect(x.speed_ramp).toEqual(x0.speed_ramp!);
		expect(y.speed_ramp![0].time).toBe(0);
		close(speedAt(y, 0), speedAt(y0, 0.5));
	});

	test('a slide keeps the moved clip as it is', () => {
		const t = cut(RAMP);
		const was = structuredClone(clipOf(t, 'a'));
		slideClip(t, 'a', 1.5, new Map([['a', 60]]));
		const a = clipOf(t, 'a');
		expect(a.speed_ramp).toEqual(was.speed_ramp!);
		close(clipDuration(a), clipDuration(was));
	});

	test('split and remove: the left half given up rebases, the right half given up keeps the ramp', () => {
		const t = cut(RAMP);
		const was = structuredClone(clipOf(t, 'a'));
		const kept = splitRemove(t, 'a', 1, 'left');
		expect(kept.speed_ramp).toEqual([key(0, 1.5), key(3, 3)]);
		close(kept.timeline_start + clipDuration(kept), was.timeline_start + clipDuration(was));
		const right = splitRemove(cut(RAMP), 'a', 1, 'right');
		expect(right.speed_ramp).toEqual(RAMP);
		close(clipDuration(right), 1);
	});

	test('handles read the curve at the clip’s edges', () => {
		const a = clipOf(cut(RAMP), 'a');
		const [head, tail] = handles(a, 60);
		close(head, 10 / 1);
		close(tail, (60 - 18) / 3);
	});
});

describe('cut range and detach', () => {
	test('a cut source range closes up by the time it took on the curve', () => {
		const t = cut(RAMP);
		const was = structuredClone(clipOf(t, 'a'));
		const pieces = cutClipRange(t, 'a', 12, 14);
		expect(pieces).toHaveLength(2);
		const [head, tail] = pieces;
		const removed = localAtAlong(was, 4) - localAtAlong(was, 2);
		close(clipDuration(head) + clipDuration(tail), clipDuration(was) - removed);
		close(head.timeline_start + clipDuration(head), localAtAlong(was, 2));
		close(speedAt(tail, 0), speedAt(was, localAtAlong(was, 4)));
		close(clipOf(t, 'b').timeline_start, clipDuration(was) - removed);
	});

	test('a linked cut takes the same stretch out of the partner', () => {
		const done = ops.cutRange(cut(RAMP), env(), 'a', 12, 14).timeline;
		const [v, a] = [done.tracks[0].clips.filter((c) => c.id !== 'b'), done.tracks[1].clips];
		expect(v).toHaveLength(2);
		expect(a).toHaveLength(2);
		for (let i = 0; i < 2; i++) {
			close(v[i].timeline_start, a[i].timeline_start, 1e-6);
			close(clipDuration(v[i]), clipDuration(a[i]), 1e-6);
		}
	});

	test('detached sound runs the picture’s ramp', () => {
		const t: Timeline = {
			tracks: [
				{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('p', 0, 0, 8, { speed_ramp: RAMP })] },
				{ id: 'a1', kind: 'audio', name: 'A1', clips: [] }
			]
		} as unknown as Timeline;
		const detached = detachAudio(t, 'p', true);
		const sound = t.tracks.flatMap((x) => x.clips).find((c) => c.id === detached.clip.id)!;
		expect(sound.speed_ramp).toEqual(RAMP);
		close(clipDuration(sound), clipDuration(clipOf(t, 'p')));
	});
});
