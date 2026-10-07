import { describe, expect, test } from 'bun:test';
import {
	ADJACENT_EPS,
	MIN_EDIT_CLIP,
	rollEdit,
	rollRange,
	slideClip,
	slideRange,
	slipClip,
	slipRange,
	splitRemove,
	splitRemoveClips,
	type SourceLimits
} from './edit-modes';
import { rippleFrom } from './ripple';
import type { Clip, Keyframe, StreamKind, Timeline, Track } from './types';
import { clipDuration } from './types';

// `rollEdit` / `slipClip` / `slideClip` / `splitRemove` are the port of kerf-core's
// `Timeline::roll_edit` / `slip_clip` / `slide_clip` / `split_remove`; these replay
// the Rust tests (`// ---- edit modes` in model.rs) case for case, numbers and
// messages included.

let seq = 0;
const sclip = (si: number, so: number, start: number, extra: Partial<Clip> = {}): Clip => ({
	id: `clip-${++seq}`,
	asset_id: 'asset',
	source_in: si,
	source_out: so,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const track = (name: string, clips: Clip[], locked = false): Track => ({
	id: `track-${name}`,
	kind: 'video' as StreamKind,
	name,
	clips,
	...(locked ? { locked } : {})
});
const oneLane = (clips: Clip[]): Timeline => ({ tracks: [track('V1', clips)] });
const footage = (secs: number): SourceLimits => new Map([['asset', secs]]);
const spans = (t: Timeline, ti = 0) =>
	t.tracks[ti].clips.map((c) => [c.timeline_start, c.timeline_start + clipDuration(c)]);
const win = (c: Clip) => [c.source_in, c.source_out];
const kf = (time: number, scale: number): Keyframe => ({ time, scale, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 });
const scales = (c: Clip) => (c.keyframes ?? []).map((k) => [k.time, k.scale]);
const near = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-9);
const clone = <T>(t: T): T => structuredClone(t);

/** The message a refusal carries; the timeline it ran on is checked unchanged by the caller. */
function refusal(run: () => unknown): string {
	try {
		run();
	} catch (e) {
		return (e as Error).message;
	}
	throw new Error('expected a refusal');
}

/** `a [0,4)` src 10..14 abutting `b [4,8)` src 20..24. */
function cutPair(): [Timeline, string, string] {
	const [a, b] = [sclip(10, 14, 0), sclip(20, 24, 4)];
	return [oneLane([a, b]), a.id, b.id];
}

describe('rollEdit', () => {
	test('moves the cut and leaves the stretch it spans alone', () => {
		const [t, a, b] = cutPair();
		const out = rollEdit(t, a, b, 1, footage(60));
		expect([out.requested, out.applied, out.clamped]).toEqual([1, 1, false]);
		expect(out.clips).toHaveLength(2);
		expect(win(t.tracks[0].clips[0])).toEqual([10, 15]);
		expect(win(t.tracks[0].clips[1])).toEqual([21, 24]);
		expect(spans(t)).toEqual([
			[0, 5],
			[5, 8]
		]);

		rollEdit(t, a, b, -2.5, footage(60));
		expect(win(t.tracks[0].clips[0])).toEqual([10, 12.5]);
		expect(win(t.tracks[0].clips[1])).toEqual([18.5, 24]);
		expect(spans(t)).toEqual([
			[0, 2.5],
			[2.5, 8]
		]);
	});

	test('clamps to the footage each clip has left and says so', () => {
		const [ca, cb] = [sclip(0, 4, 0), sclip(1, 5, 4)];
		const t = oneLane([ca, cb]);

		const later = clone(t);
		const out = rollEdit(later, ca.id, cb.id, 5, footage(6));
		expect([out.requested, out.applied, out.clamped]).toEqual([5, 2, true]);
		expect(win(later.tracks[0].clips[0])).toEqual([0, 6]);
		expect(later.tracks[0].clips[1].timeline_start).toBe(6);

		const earlier = clone(t);
		const back = rollEdit(earlier, ca.id, cb.id, -5, footage(6));
		expect([back.applied, back.clamped]).toEqual([-1, true]);
		expect(win(earlier.tracks[0].clips[1])).toEqual([0, 5]);
		expect(spans(earlier)).toEqual([
			[0, 3],
			[3, 8]
		]);

		const range = rollRange(t, ca.id, cb.id, footage(6));
		expect([range.min, range.max]).toEqual([-1, 2]);
	});

	test('never leaves a clip shorter than the floor', () => {
		const [ca, cb] = [sclip(10, 11, 0), sclip(20, 21, 1)];
		const t = oneLane([ca, cb]);
		const later = clone(t);
		near(rollEdit(later, ca.id, cb.id, 5, footage(60)).applied, 0.95);
		near(clipDuration(later.tracks[0].clips[1]), MIN_EDIT_CLIP);
		const earlier = clone(t);
		near(rollEdit(earlier, ca.id, cb.id, -5, footage(60)).applied, -0.95);
		near(clipDuration(earlier.tracks[0].clips[0]), MIN_EDIT_CLIP);
	});

	test('honors speed', () => {
		const ca = sclip(0, 8, 0, { speed: 2 });
		const cb = sclip(2, 3, 4, { speed: 0.5 });
		const t = oneLane([ca, cb]);

		const later = clone(t);
		const out = rollEdit(later, ca.id, cb.id, 3, footage(10));
		expect([out.applied, out.clamped]).toEqual([1, true]);
		expect(win(later.tracks[0].clips[0])).toEqual([0, 10]);
		near(later.tracks[0].clips[1].source_in, 2.5);
		expect([later.tracks[0].clips[1].timeline_start, clipDuration(later.tracks[0].clips[1])]).toEqual([5, 1]);

		const earlier = clone(t);
		rollEdit(earlier, ca.id, cb.id, -1, footage(10));
		expect(win(earlier.tracks[0].clips[0])).toEqual([0, 6]);
		near(earlier.tracks[0].clips[1].source_in, 1.5);
		expect([earlier.tracks[0].clips[1].timeline_start, clipDuration(earlier.tracks[0].clips[1])]).toEqual([3, 3]);
	});

	test('honors reverse: the outgoing edge is the in-point', () => {
		const [ca, cb] = [sclip(5, 9, 0, { speed: -1 }), sclip(5, 9, 4, { speed: -1 })];
		const t = oneLane([ca, cb]);

		const later = clone(t);
		expect(rollEdit(later, ca.id, cb.id, 2, footage(60)).applied).toBe(2);
		expect(win(later.tracks[0].clips[0])).toEqual([3, 9]); // a: in-point down by 2
		expect(win(later.tracks[0].clips[1])).toEqual([5, 7]); // b: out-point down by 2
		expect(spans(later)).toEqual([
			[0, 6],
			[6, 8]
		]);

		const earlier = clone(t);
		rollEdit(earlier, ca.id, cb.id, -2, footage(60));
		expect(win(earlier.tracks[0].clips[0])).toEqual([7, 9]);
		expect(win(earlier.tracks[0].clips[1])).toEqual([5, 11]);

		// The handle is the one on the playing side: `a` has 1s below its in-point, `b` half a second above its out-point.
		const [ra, rb] = [sclip(1, 5, 0, { speed: -1 }), sclip(5, 59.5, 4, { speed: -1 })];
		const range = rollRange(oneLane([ra, rb]), ra.id, rb.id, footage(60));
		expect([range.min, range.max]).toEqual([-0.5, 1]);
	});

	test('through a still: footage without limit and never a negative window', () => {
		const limits: SourceLimits = new Map([
			['asset', 60],
			['still', Infinity]
		]);
		const [ca, cb] = [sclip(10, 15, 0), sclip(0, 5, 5, { asset_id: 'still' })];
		const t = oneLane([ca, cb]);
		rollEdit(t, ca.id, cb.id, -2, limits);
		const grown = t.tracks[0].clips[1];
		expect([grown.timeline_start, clipDuration(grown)]).toEqual([3, 7]);
		expect(win(grown)).toEqual([0, 7]);

		const [sa, sb] = [sclip(0, 5, 0, { asset_id: 'still' }), sclip(10, 15, 5)];
		const u = oneLane([sa, sb]);
		near(rollEdit(u, sa.id, sb.id, 20, limits).applied, 4.95);
		expect(u.tracks[0].clips[0].source_in).toBe(0);
	});

	test('needs two adjacent clips in order on one unlocked track', () => {
		const [t, a, b] = cutPair();
		const f = footage(60);
		const refused = (name: string, tl: Timeline, ia: string, ib: string, delta: number) => {
			const copy = clone(tl);
			const why = refusal(() => rollEdit(copy, ia, ib, delta, f));
			expect(copy).toEqual(tl); // a refused roll changes nothing
			return why;
		};

		expect(refused('swapped', t, b, a, 1)).toContain('clip_a must be the earlier clip');
		expect(refused('itself', t, a, a, 1)).toContain('two different clips');
		expect(refused('zero', t, a, b, 0)).toContain('zero');
		expect(refused('nan', t, a, b, NaN)).toContain('finite');

		const gapped = clone(t);
		gapped.tracks[0].clips[1].timeline_start = 5;
		expect(refused('gap', gapped, a, b, 1)).toContain('1.00s gap');
		const overlapped = clone(t);
		overlapped.tracks[0].clips[1].timeline_start = 3.5;
		expect(refused('overlap', overlapped, a, b, 1)).toContain('overlap by 0.50s');

		const two: Timeline = {
			tracks: [track('V1', [clone(t.tracks[0].clips[0])]), track('V2', [clone(t.tracks[0].clips[1])])]
		};
		expect(refused('tracks', two, a, b, 1)).toContain('different tracks (V1 and V2)');

		const locked = clone(t);
		locked.tracks[0].locked = true;
		expect(refused('locked', locked, a, b, 1)).toContain('V1 is locked');
		expect(() => rollEdit(clone(t), a, 'nope', 1, f)).toThrow('clip not found: nope');
		expect(() => rollEdit(clone(t), a, b, 1, new Map())).toThrow('asset not found: asset');
	});

	test('a clamp to nothing is an error that says why', () => {
		const [ca, cb] = [sclip(0, 6, 0), sclip(0, 3, 6)];
		const t = oneLane([ca, cb]);
		const before = clone(t);
		const later = refusal(() => rollEdit(t, ca.id, cb.id, 1, footage(6)));
		expect(later).toContain('cannot roll the cut later');
		expect(later).toContain('outgoing clip has no footage left');
		const earlier = refusal(() => rollEdit(t, ca.id, cb.id, -1, footage(6)));
		expect(earlier).toContain('cannot roll the cut earlier');
		expect(earlier).toContain('incoming clip has no footage left');
		expect(t).toEqual(before);
	});

	test('two clips a hair apart still share a cut', () => {
		const [t, a, b] = cutPair();
		t.tracks[0].clips[1].timeline_start = 4.0005;
		expect(rollEdit(t, a, b, 1, footage(60)).applied).toBe(1);
		near(t.tracks[0].clips[1].timeline_start, 5.0005);

		const [u, ua, ub] = cutPair();
		u.tracks[0].clips[1].timeline_start = 4 + 2 * ADJACENT_EPS;
		expect(refusal(() => rollEdit(u, ua, ub, 1, footage(60)))).toContain('not adjacent');
	});

	test('carries the incoming clips animation with its footage', () => {
		const [t, a, b] = cutPair();
		t.tracks[0].clips[0].keyframes = [kf(0, 1), kf(2, 3)];
		t.tracks[0].clips[1].keyframes = [kf(0, 1), kf(2, 2)];
		t.tracks[0].clips[1].reframe = {
			input: 'equirect',
			output: 'flat',
			lens_fov: 190,
			yaw: 0,
			pitch: 0,
			roll: 0,
			fov: 100,
			keyframes: [
				{ time: 0, yaw: 0, pitch: 0, roll: 0, fov: 100 },
				{ time: 2, yaw: 90, pitch: 0, roll: 0, fov: 100 }
			]
		};

		const later = clone(t);
		rollEdit(later, a, b, 1, footage(60));
		expect(scales(later.tracks[0].clips[1])).toEqual([
			[0, 1.5],
			[1, 2]
		]);
		expect(later.tracks[0].clips[1].reframe!.keyframes!.map((k) => [k.time, k.yaw])).toEqual([
			[0, 45],
			[1, 90]
		]);
		expect(scales(later.tracks[0].clips[0])).toEqual([
			[0, 1],
			[2, 3]
		]); // the outgoing clip's head did not move

		const earlier = clone(t);
		rollEdit(earlier, a, b, -1, footage(60));
		expect(scales(earlier.tracks[0].clips[1])).toEqual([
			[1, 1],
			[3, 2]
		]);
	});

	test('keeps fades inside a clip that shrank and the transition on the cut', () => {
		const [t, a, b] = cutPair();
		t.tracks[0].clips[0].fade_out = 2;
		t.tracks[0].clips[0].fade_in = 0.5;
		t.tracks[0].clips[1].fade_in = 3;
		const transition = { kind: 'crossfade' as const, duration: 1 };
		t.tracks[0].clips[1].transition_in = transition;

		rollEdit(t, a, b, -3.9, footage(60)); // a is 0.1s now
		near(t.tracks[0].clips[0].fade_out, 0.1);
		near(t.tracks[0].clips[0].fade_in, 0.1);
		expect(t.tracks[0].clips[1].fade_in).toBe(3);
		expect(t.tracks[0].clips[1].transition_in).toEqual(transition);

		rollEdit(t, a, b, 10, footage(60)); // b shrinks to the floor
		near(t.tracks[0].clips[1].fade_in, MIN_EDIT_CLIP);
	});
});

describe('slipClip', () => {
	test('moves the window and nothing on the timeline', () => {
		const c = sclip(10, 14, 3, { keyframes: [kf(0, 1), kf(2, 2)], fade_in: 0.5 });
		const t = oneLane([c]);
		const keys = clone(c.keyframes);

		const out = slipClip(t, c.id, 2, footage(60));
		expect([out.applied, out.clamped]).toEqual([2, false]);
		const slipped = t.tracks[0].clips[0];
		expect(win(slipped)).toEqual([12, 16]);
		expect([slipped.timeline_start, clipDuration(slipped)]).toEqual([3, 4]);
		expect(slipped.keyframes).toEqual(keys);
		expect(slipped.fade_in).toBe(0.5);

		slipClip(t, c.id, -5, footage(60));
		expect(win(t.tracks[0].clips[0])).toEqual([7, 11]);
	});

	test('clamps to the asset and errors at the edge', () => {
		const c = sclip(10, 14, 0);
		const t = oneLane([c]);

		const up = clone(t);
		const out = slipClip(up, c.id, 10, footage(20));
		expect([out.applied, out.clamped]).toEqual([6, true]);
		expect(win(up.tracks[0].clips[0])).toEqual([16, 20]);
		const later = refusal(() => slipClip(up, c.id, 1, footage(20)));
		expect(later).toContain('cannot slip the footage later');
		expect(later).toContain("after the clip's out-point");

		const down = clone(t);
		near(slipClip(down, c.id, -50, footage(20)).applied, -10);
		expect(win(down.tracks[0].clips[0])).toEqual([0, 4]);
		const earlier = refusal(() => slipClip(down, c.id, -1, footage(20)));
		expect(earlier).toContain('cannot slip the footage earlier');
		expect(earlier).toContain("before the clip's in-point");

		const range = slipRange(t, c.id, footage(20));
		expect([range.min, range.max]).toEqual([-10, 6]);
	});

	test('is in source seconds whatever the speed', () => {
		const c = sclip(10, 14, 0, { speed: 2 });
		const t = oneLane([c]);
		slipClip(t, c.id, 1, footage(60));
		const slipped = t.tracks[0].clips[0];
		expect(win(slipped)).toEqual([11, 15]);
		expect([slipped.timeline_start, slipped.timeline_start + clipDuration(slipped)]).toEqual([0, 2]);
	});

	test('a reversed clip slips the mirrored way, so the sign means the same on screen', () => {
		const c = sclip(10, 14, 0, { speed: -1 });
		const t = oneLane([c]);

		const later = clone(t);
		slipClip(later, c.id, 3, footage(20));
		expect(win(later.tracks[0].clips[0])).toEqual([7, 11]); // positive moves the window down
		const earlier = clone(t);
		slipClip(earlier, c.id, -3, footage(20));
		expect(win(earlier.tracks[0].clips[0])).toEqual([13, 17]);

		const range = slipRange(t, c.id, footage(20));
		expect([range.min, range.max]).toEqual([-6, 10]);
		const hi = clone(t);
		near(slipClip(hi, c.id, 100, footage(20)).applied, 10);
		expect(win(hi.tracks[0].clips[0])).toEqual([0, 4]);
		const lo = clone(t);
		near(slipClip(lo, c.id, -100, footage(20)).applied, -6);
		expect(win(lo.tracks[0].clips[0])).toEqual([16, 20]);
	});

	test('a still has nothing to slip, and the other refusals', () => {
		const limits: SourceLimits = new Map([
			['asset', 60],
			['still', Infinity]
		]);
		const s = sclip(0, 5, 0, { asset_id: 'still' });
		const t = oneLane([s]);
		expect(refusal(() => slipClip(t, s.id, 1, limits))).toContain('still image');
		expect(refusal(() => slipRange(t, s.id, limits))).toContain('still image');

		const [u, a] = cutPair();
		expect(refusal(() => slipClip(u, a, 0, footage(60)))).toContain('zero');
		expect(refusal(() => slipClip(u, a, Infinity, footage(60)))).toContain('finite');
		expect(() => slipClip(u, 'nope', 1, footage(60))).toThrow('clip not found: nope');
		u.tracks[0].locked = true;
		expect(refusal(() => slipClip(u, a, 1, footage(60)))).toContain('V1 is locked');
	});
});

/** `p [0,4)` src 10..14, `c [4,7)` src 30..33, `n [7,12)` src 20..25, abutting. */
function three(): [Timeline, [string, string, string]] {
	const [p, c, n] = [sclip(10, 14, 0), sclip(30, 33, 4), sclip(20, 25, 7)];
	return [oneLane([p, c, n]), [p.id, c.id, n.id]];
}

describe('slideClip', () => {
	test('moves the clip and the neighbours give way', () => {
		const [t, [p, c, n]] = three();

		const later = clone(t);
		const out = slideClip(later, c, 1, footage(60));
		expect([out.requested, out.applied, out.clamped]).toEqual([1, 1, false]);
		expect(out.clips.map((x) => x.id)).toEqual([p, c, n]);
		expect(spans(later)).toEqual([
			[0, 5],
			[5, 8],
			[8, 12]
		]);
		const cl = later.tracks[0].clips;
		expect(win(cl[0])).toEqual([10, 15]);
		expect(win(cl[1])).toEqual([30, 33]);
		expect(win(cl[2])).toEqual([21, 25]);

		const earlier = clone(t);
		slideClip(earlier, c, -1.5, footage(60));
		expect(spans(earlier)).toEqual([
			[0, 2.5],
			[2.5, 5.5],
			[5.5, 12]
		]);
		const el = earlier.tracks[0].clips;
		expect(win(el[0])).toEqual([10, 12.5]);
		expect(win(el[1])).toEqual([30, 33]);
		expect(win(el[2])).toEqual([18.5, 25]);
	});

	test('clamps to the neighbours footage and floor', () => {
		const [t, [, c]] = three();

		const later = clone(t);
		const out = slideClip(later, c, 10, footage(60));
		expect([out.applied, out.clamped]).toEqual([4.95, true]);
		near(clipDuration(later.tracks[0].clips[2]), MIN_EDIT_CLIP);
		const earlier = clone(t);
		near(slideClip(earlier, c, -10, footage(60)).applied, -3.95);
		near(clipDuration(earlier.tracks[0].clips[0]), MIN_EDIT_CLIP);

		// A 14s asset: the previous clip (10..14) has no footage after it to grow into.
		const dry = clone(t);
		const why = refusal(() => slideClip(dry, c, 1, footage(14)));
		expect(why).toContain('cannot slide the clip later');
		expect(why).toContain('previous clip has no footage left');
		near(slideClip(dry, c, -1, footage(14)).applied, -1);

		// The next clip's head handle bounds the earlier direction too.
		const [tight, ids] = three();
		tight.tracks[0].clips[2].source_in = 0.5;
		const bound = slideClip(tight, ids[1], -2, footage(60));
		expect([bound.applied, bound.clamped]).toEqual([-0.5, true]);

		const range = slideRange(t, c, footage(60));
		expect([range.min, range.max]).toEqual([-3.95, 4.95]);
	});

	test('beside a gap trims only the neighbour that touches', () => {
		const [p, c, n] = [sclip(10, 14, 0), sclip(30, 33, 4), sclip(20, 25, 9)];
		const t = oneLane([p, c, n]);

		const later = clone(t);
		const out = slideClip(later, c.id, 1, footage(60));
		expect(out.clips.map((x) => x.id)).toEqual([p.id, c.id]); // n is not touched
		const cl = later.tracks[0].clips;
		expect(win(cl[0])).toEqual([10, 15]);
		expect(cl[1].timeline_start).toBe(5);
		expect([cl[2].timeline_start, win(cl[2])]).toEqual([9, [20, 25]]);

		const far = clone(t);
		const stop = slideClip(far, c.id, 5, footage(60));
		expect([stop.applied, stop.clamped]).toEqual([2, true]);
		expect(far.tracks[0].clips[2].timeline_start).toBe(9);
		expect(far.tracks[0].clips[1].timeline_start + clipDuration(far.tracks[0].clips[1])).toBe(9);

		const earlier = clone(t);
		slideClip(earlier, c.id, -1, footage(60));
		expect(spans(earlier)).toEqual([
			[0, 3],
			[3, 6],
			[9, 14]
		]);
	});

	test('after a gap leaves the previous clip alone', () => {
		const [p, c, n] = [sclip(10, 13, 0), sclip(30, 33, 4), sclip(20, 25, 7)];
		const t = oneLane([p, c, n]);

		const earlier = clone(t);
		const out = slideClip(earlier, c.id, -0.5, footage(60));
		expect(out.clips.map((x) => x.id)).toEqual([c.id, n.id]);
		const cl = earlier.tracks[0].clips;
		expect([cl[0].timeline_start + clipDuration(cl[0]), win(cl[0])]).toEqual([3, [10, 13]]);
		expect(cl[1].timeline_start).toBe(3.5);
		expect([cl[2].timeline_start, win(cl[2])]).toEqual([6.5, [19.5, 25]]);

		const far = clone(t);
		const stop = slideClip(far, c.id, -3, footage(60));
		expect([stop.applied, stop.clamped]).toEqual([-1, true]);
		expect(far.tracks[0].clips[1].timeline_start).toBe(3);

		const later = clone(t);
		slideClip(later, c.id, 1, footage(60));
		expect(spans(later)).toEqual([
			[0, 3],
			[5, 8],
			[8, 12]
		]);
	});

	test('at the ends of a track', () => {
		// First clip: nothing before it, so it cannot go below 0.
		const [c0, n0] = [sclip(30, 33, 2), sclip(20, 25, 5)];
		const t = oneLane([c0, n0]);
		const out = slideClip(t, c0.id, -5, footage(60));
		expect([out.applied, out.clamped]).toEqual([-2, true]);
		expect(spans(t)).toEqual([
			[0, 3],
			[3, 10]
		]);
		expect(refusal(() => slideClip(t, c0.id, -1, footage(60)))).toContain('already at the start of the timeline');
		slideClip(t, c0.id, 1, footage(60));
		expect(spans(t)).toEqual([
			[1, 4],
			[4, 10]
		]);

		// Last clip: nothing after it to give way, so the track's end moves with it.
		const [p1, c1] = [sclip(10, 14, 0), sclip(30, 33, 4)];
		const u = oneLane([p1, c1]);
		expect(slideClip(u, c1.id, 3, footage(60)).applied).toBe(3);
		expect(spans(u)).toEqual([
			[0, 7],
			[7, 10]
		]);

		// Free space on both sides: a plain move, bounded by what it would run into.
		const [p2, c2, n2] = [sclip(10, 11, 0), sclip(30, 33, 3), sclip(20, 21, 9)];
		const w = oneLane([p2, c2, n2]);
		const move = slideClip(w, c2.id, 5, footage(60));
		expect([move.applied, move.clips.length]).toEqual([3, 1]);
		expect(spans(w)).toEqual([
			[0, 1],
			[6, 9],
			[9, 10]
		]);
	});

	test('honors the neighbours speed and direction', () => {
		const p = sclip(10, 14, 0, { speed: -1 });
		const c = sclip(30, 33, 4);
		const n = sclip(20, 30, 7, { speed: 2 });
		const t = oneLane([p, c, n]);
		slideClip(t, c.id, 2, footage(60));
		const cl = t.tracks[0].clips;
		expect(win(cl[0])).toEqual([8, 14]); // the reversed clip's end is its in-point
		expect(cl[1].timeline_start).toBe(6);
		expect([cl[2].timeline_start, win(cl[2])]).toEqual([9, [24, 30]]);
		expect(spans(t)).toEqual([
			[0, 6],
			[6, 9],
			[9, 12]
		]);
	});

	test('moves the next clips animation with its footage and not the slid clips', () => {
		const [t, [p, c, n]] = three();
		const keys: Record<string, Keyframe[]> = {
			[p]: [kf(0, 1), kf(2, 3)],
			[c]: [kf(0, 1), kf(1, 3)],
			[n]: [kf(0, 1), kf(4, 4)]
		};
		for (const clip of t.tracks[0].clips) clip.keyframes = keys[clip.id];
		slideClip(t, c, 1, footage(60));
		const cl = t.tracks[0].clips;
		expect(scales(cl[0])).toEqual([
			[0, 1],
			[2, 3]
		]);
		expect(scales(cl[1])).toEqual([
			[0, 1],
			[1, 3]
		]);
		expect(scales(cl[2])).toEqual([
			[0, 1.75],
			[3, 4]
		]);
	});

	test('through a still neighbour extends it without limit', () => {
		const limits: SourceLimits = new Map([
			['asset', 60],
			['still', Infinity]
		]);
		const [p, c, n] = [sclip(0, 4, 0, { asset_id: 'still' }), sclip(30, 33, 4), sclip(20, 25, 7)];
		const t = oneLane([p, c, n]);
		near(slideClip(t, c.id, 10, limits).applied, 4.95);
		expect(win(t.tracks[0].clips[0])).toEqual([0, 8.95]);

		const [p2, c2, n2] = [sclip(10, 14, 0), sclip(30, 33, 4), sclip(0, 5, 7, { asset_id: 'still' })];
		const u = oneLane([p2, c2, n2]);
		near(slideClip(u, c2.id, -2, limits).applied, -2);
		expect(win(u.tracks[0].clips[2])).toEqual([0, 7]);
		expect(u.tracks[0].clips[2].timeline_start).toBe(5);
	});

	test('errors', () => {
		const [t, [, c]] = three();
		const copy = clone(t);
		expect(refusal(() => slideClip(copy, c, 0, footage(60)))).toContain('zero');
		expect(refusal(() => slideClip(copy, c, NaN, footage(60)))).toContain('finite');
		expect(() => slideClip(copy, 'nope', 1, footage(60))).toThrow('clip not found: nope');
		expect(() => slideClip(copy, c, 1, new Map())).toThrow('asset not found: asset');
		expect(copy).toEqual(t); // none of them changed anything
		copy.tracks[0].locked = true;
		expect(refusal(() => slideClip(copy, c, 1, footage(60)))).toContain('V1 is locked');
	});
});

describe('splitRemove', () => {
	test('right shortens the clip at its end', () => {
		const keys = [kf(0, 1), kf(8, 3)];
		const c = sclip(10, 20, 5, { fade_in: 1, fade_out: 2, keyframes: clone(keys) });
		const t = oneLane([c]);
		const kept = splitRemove(t, c.id, 9, 'right');
		expect(kept.id).toBe(c.id);
		expect(win(kept)).toEqual([10, 14]);
		expect([kept.timeline_start, kept.timeline_start + clipDuration(kept)]).toEqual([5, 9]);
		expect(kept.fade_in).toBe(1);
		expect(kept.fade_out).toBe(0);
		expect(kept.keyframes).toEqual(keys);
		expect(t.tracks[0].clips).toHaveLength(1);
	});

	test('left moves the start up and drops what belonged to the old start', () => {
		const c = sclip(10, 20, 5, { fade_in: 1, fade_out: 2, transition_in: { kind: 'crossfade', duration: 1 } });
		const t = oneLane([c]);
		const kept = splitRemove(t, c.id, 9, 'left');
		expect(kept.id).toBe(c.id);
		expect(win(kept)).toEqual([14, 20]);
		expect([kept.timeline_start, kept.timeline_start + clipDuration(kept)]).toEqual([9, 15]);
		expect(kept.fade_in).toBe(0);
		expect(kept.transition_in).toBeNull();
		expect(kept.fade_out).toBe(2);
	});

	test('honors speed and reverse', () => {
		const c = sclip(10, 20, 0, { speed: -2 });
		const t = oneLane([c]);

		const left = clone(t);
		const kept = splitRemove(left, c.id, 2, 'left');
		expect(win(kept)).toEqual([10, 16]);
		expect([kept.timeline_start, clipDuration(kept)]).toEqual([2, 3]);

		const right = clone(t);
		const tail = splitRemove(right, c.id, 2, 'right');
		expect(win(tail)).toEqual([16, 20]);
		expect([tail.timeline_start, clipDuration(tail)]).toEqual([0, 2]);
	});

	test('left carries the animation with the footage that stays', () => {
		const c = sclip(0, 8, 0, { keyframes: [kf(0, 1), kf(4, 5)] });
		const t = oneLane([c]);
		expect(scales(splitRemove(t, c.id, 1, 'left'))).toEqual([
			[0, 2],
			[3, 5]
		]);
	});

	test('holds the fade inside what is left', () => {
		const c = sclip(0, 4, 0, { fade_in: 3 });
		const t = oneLane([c]);
		expect(splitRemove(t, c.id, 1, 'right').fade_in).toBe(1);
	});

	test('only cuts inside the clip and leaves something', () => {
		const c = sclip(0, 4, 2); // [2, 6)
		const t = oneLane([c]);
		const run = (at: number, side: 'left' | 'right') => {
			const copy = clone(t);
			const why = refusal(() => splitRemove(copy, c.id, at, side));
			expect(copy).toEqual(t);
			return why;
		};
		expect(run(1, 'left')).toContain('not inside the clip (0:02.0–0:06.0)');
		expect(run(7, 'right')).toContain('not inside the clip');
		expect(run(2, 'left')).toContain('not inside the clip');
		expect(run(6, 'right')).toContain('not inside the clip');
		expect(run(NaN, 'left')).toContain('finite');
		expect(run(5.99, 'left')).toContain('only 0.01s of the clip');
		expect(run(2.02, 'right')).toContain('only 0.02s of the clip');

		const copy = clone(t);
		near(clipDuration(splitRemove(copy, c.id, 2.02, 'left')), 3.98);

		const locked = clone(t);
		locked.tracks[0].locked = true;
		expect(refusal(() => splitRemove(locked, c.id, 4, 'left'))).toContain('V1 is locked');
		expect(() => splitRemove(clone(t), 'nope', 4, 'left')).toThrow('clip not found: nope');
	});

	test('under ripple closes the gap through the standard path', () => {
		// `a [0,4)  b [5,8)  c [10,12)`, ripple applied the way `devEdit` does.
		const [a, b, c] = [sclip(0, 4, 0), sclip(0, 3, 5), sclip(0, 2, 10)];
		const before = oneLane([a, b, c]);
		const rippled = (side: 'left' | 'right') => {
			const after = clone(before);
			splitRemove(after, b.id, 6, side);
			return [after, rippleFrom(after, before)] as const;
		};
		expect(spans(rippled('right')[1])).toEqual([
			[0, 4],
			[5, 6],
			[8, 10]
		]);
		const [bare, left] = rippled('left');
		expect(spans(bare)).toEqual([
			[0, 4],
			[6, 8],
			[10, 12]
		]);
		expect(spans(left)).toEqual([
			[0, 4],
			[5, 7],
			[9, 11]
		]);
	});
});

// ---- no float residue between touching clips ------------------------------------------

/** mulberry32 — a small deterministic generator, so the fuzz is the same on every run. */
function rng(seed: number) {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = a;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

/** A lane of `n` clips of random length, speed (and direction) and window in a 600 s
 *  asset laid end to end — the float values a real cut is made of — with an occasional real gap. */
function randomLane(next: () => number, n: number): Timeline {
	const between = (lo: number, hi: number) => lo + (hi - lo) * next();
	const speeds = [1, 1, 2, 0.5, 1.5, -1, -2, 0.37];
	const clips: Clip[] = [];
	let cursor = 0;
	for (let i = 0; i < n; i++) {
		const speed = speeds[Math.floor(next() * speeds.length) % speeds.length];
		const si = between(30, 400);
		const so = si + between(0.4, 7) * Math.max(Math.abs(speed), 0.01);
		if (next() < 0.25) cursor += between(0.2, 2);
		const c = sclip(si, so, cursor, { speed });
		cursor = c.timeline_start + clipDuration(c);
		clips.push(c);
	}
	return oneLane(clips);
}

/** Junctions where a clip starts *before* the one ahead of it ends, by float noise —
 *  what a strict overlap test reads as one clip lying on the next. */
function noiseOverlaps(t: Timeline): [number, number][] {
	const clips = t.tracks[0].clips;
	const out: [number, number][] = [];
	for (let i = 1; i < clips.length; i++) {
		const over = clips[i - 1].timeline_start + clipDuration(clips[i - 1]) - clips[i].timeline_start;
		if (over > 0 && over < ADJACENT_EPS) out.push([i, over]);
	}
	return out;
}

describe('roll and slide leave no float residue between clips', () => {
	test('every junction of the lane comes out clean, for requests inside the range and past it', () => {
		const next = rng(7);
		const between = (lo: number, hi: number) => lo + (hi - lo) * next();
		let tried = 0;
		for (let k = 0; k < 3000; k++) {
			const t = randomLane(next, 5);
			expect(noiseOverlaps(t)).toEqual([]);
			const ids = t.tracks[0].clips.map((c) => c.id);
			const at = 1 + Math.floor(next() * 3);
			const delta = next() < 0.3 ? between(-9, 9) : between(-1.5, 1.5);
			for (const op of [0, 1]) {
				const u = clone(t);
				let out;
				try {
					out = op === 0 ? rollEdit(u, ids[at - 1], ids[at], delta, footage(600)) : slideClip(u, ids[at], delta, footage(600));
				} catch {
					continue;
				}
				tried++;
				expect(noiseOverlaps(u)).toEqual([]);
				// …and nothing but noise moved to get there.
				const [was, now] = [t.tracks[0].clips, u.tracks[0].clips];
				was.forEach((w, i) => {
					if (!out.clips.some((c) => c.id === w.id))
						expect([now[i].timeline_start, now[i].source_in, now[i].source_out]).toEqual([w.timeline_start, w.source_in, w.source_out]);
				});
				near(now[at].timeline_start, was[at].timeline_start + out.applied);
				if (op === 0) {
					near(now[at].timeline_start + clipDuration(now[at]), was[at].timeline_start + clipDuration(was[at]));
					near(now[at - 1].timeline_start + clipDuration(now[at - 1]), was[at - 1].timeline_start + clipDuration(was[at - 1]) + out.applied);
				} else {
					near(clipDuration(now[at]), clipDuration(was[at]));
				}
			}
		}
		expect(tried).toBeGreaterThan(4000);
	});

	test('a cut a hair apart keeps its real gap; only float noise is welded', () => {
		const [t, a, b] = cutPair();
		t.tracks[0].clips[1].timeline_start = 4.0005;
		rollEdit(t, a, b, 1, footage(60));
		expect(t.tracks[0].clips[1].timeline_start).toBe(5.0005);

		const [u, ua, ub] = cutPair();
		rollEdit(u, ua, ub, 0.1, footage(60));
		expect(u.tracks[0].clips[1].timeline_start).toBe(u.tracks[0].clips[0].timeline_start + clipDuration(u.tracks[0].clips[0]));
	});
});

// ---- split and remove on a group --------------------------------------------------------

/** V1 `v [0,6)` over A1 `a [1,7)` — a picture and its sound, not quite in step. */
function pictureAndSound(): [Timeline, string, string] {
	const [v, a] = [sclip(10, 16, 0), sclip(10, 16, 1)];
	return [{ tracks: [track('V1', [v]), { ...track('A1', [a]), kind: 'audio' as StreamKind }] }, v.id, a.id];
}

describe('splitRemoveClips', () => {
	test('cuts every track in one edit, the survivors in request order', () => {
		const [t, v, a] = pictureAndSound();
		const kept = splitRemoveClips(t, [{ clip_id: v, at: 3 }, { clip_id: a, at: 3 }], 'left');
		expect(kept.map((c) => c.id)).toEqual([v, a]);
		expect([kept[0].timeline_start, kept[0].source_in]).toEqual([3, 13]);
		expect([kept[1].timeline_start, kept[1].source_in]).toEqual([3, 12]);
		expect(spans(t, 0)).toEqual([[3, 6]]);
		expect(spans(t, 1)).toEqual([[3, 7]]);

		const [u, uv, ua] = pictureAndSound();
		splitRemoveClips(u, [{ clip_id: uv, at: 2 }, { clip_id: ua, at: 4.5 }], 'right');
		expect(spans(u, 0)).toEqual([[0, 2]]);
		expect(spans(u, 1)).toEqual([[1, 4.5]]);
	});

	test('is all or nothing, one clip per track', () => {
		const [t, v, a] = pictureAndSound();
		const cut = (clip_id: string, at: number) => ({ clip_id, at });
		const refused = (name: string, tl: Timeline, cuts: { clip_id: string; at: number }[]) => {
			const copy = clone(tl);
			const why = refusal(() => splitRemoveClips(copy, cuts, 'left'));
			expect(copy).toEqual(tl); // a refused group changes nothing
			return why;
		};
		expect(refused('outside', t, [cut(v, 3), cut(a, 0.5)])).toContain('not inside the clip');
		expect(refused('floor', t, [cut(v, 3), cut(a, 6.99)])).toContain('only 0.01s');
		expect(refused('empty', t, [])).toContain('no clips');
		expect(refused('twice', t, [cut(v, 3), cut(v, 4)])).toContain('more than once');

		const crowded = clone(t);
		const extra = sclip(0, 2, 7);
		crowded.tracks[0].clips.push(extra);
		const why = refused('crowded', crowded, [cut(v, 3), cut(extra.id, 7.5)]);
		expect(why).toContain('one clip per track');
		expect(why).toContain('V1');

		const locked = clone(t);
		locked.tracks[1].locked = true;
		expect(refused('locked', locked, [cut(v, 3), cut(a, 3)])).toContain('A1 is locked'); // V1 was not cut either
		expect(() => splitRemoveClips(clone(t), [cut(v, 3), cut('nope', 3)], 'left')).toThrow('clip not found: nope');
	});

	test('under ripple each track closes on its own', () => {
		const [before, v, a] = pictureAndSound();
		before.tracks[0].clips.push(sclip(0, 2, 8));
		before.tracks[1].clips.push(sclip(0, 2, 9));
		const after = clone(before);
		splitRemoveClips(after, [{ clip_id: v, at: 2 }, { clip_id: a, at: 5 }], 'left');
		const rippled = rippleFrom(after, before);
		expect(spans(after, 0)).toEqual([[2, 6], [8, 10]]);
		expect(spans(rippled, 0)).toEqual([[0, 4], [6, 8]]);
		expect(spans(after, 1)).toEqual([[5, 7], [9, 11]]);
		expect(spans(rippled, 1)).toEqual([[1, 3], [5, 7]]);
	});
});
