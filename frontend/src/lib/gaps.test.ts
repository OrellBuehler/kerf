import { describe, expect, test } from 'bun:test';
import { gapAt, gapPoint, gapStillThere, gapsOf, planGapClose, sameGap } from './gaps';
import type { Clip, Timeline, Track } from './types';

const clip = (id: string, start: number, dur: number): Clip => ({
	id,
	asset_id: 'a',
	source_in: 0,
	source_out: dur,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0
});
const track = (id: string, clips: Clip[], extra: Partial<Track> = {}): Track => ({ id, kind: 'video', name: id.toUpperCase(), clips, ...extra });

// V1: gap 0-2, c1 2-4, c2 4-6 (touching), gap 6-9, c3 9-10, nothing after.
const v1 = track('v1', [clip('c1', 2, 2), clip('c2', 4, 2), clip('c3', 9, 1)]);
const a1 = track('a1', [clip('a1c', 0, 10)], { kind: 'audio' });
const tl: Timeline = { tracks: [v1, a1] };

describe('gaps of a lane', () => {
	test('the stretch before the first clip and each one between clips; touching clips have none', () => {
		expect(gapsOf(v1)).toEqual([
			{ trackId: 'v1', from: 0, to: 2 },
			{ trackId: 'v1', from: 6, to: 9 }
		]);
	});

	test('nothing after the last clip is a gap, and a lane with no clips has none', () => {
		expect(gapsOf(track('x', []))).toEqual([]);
		expect(gapsOf(a1)).toEqual([]);
	});

	test('the model’s clip order is not trusted, and an overlapping clip does not open a gap behind it', () => {
		const odd = track('o', [clip('b', 5, 1), clip('a', 0, 4), clip('c', 3, 5)]);
		expect(gapsOf(odd)).toEqual([]);
	});

	test('a gap too small to see is no gap', () => {
		expect(gapsOf(track('t', [clip('a', 0, 1), clip('b', 1 + 1e-8, 1)]))).toEqual([]);
	});

	test('the gap a time is in, edges included; none inside a clip', () => {
		expect(gapAt(v1, 7)).toEqual({ trackId: 'v1', from: 6, to: 9 });
		expect(gapAt(v1, 6)).toEqual({ trackId: 'v1', from: 6, to: 9 });
		expect(gapAt(v1, 9)).toEqual({ trackId: 'v1', from: 6, to: 9 });
		expect(gapAt(v1, 1)).toEqual({ trackId: 'v1', from: 0, to: 2 });
		expect(gapAt(v1, 5)).toBeNull();
		expect(gapAt(v1, 12)).toBeNull();
	});

	test('the point it is closed by is its middle', () => {
		expect(gapPoint({ trackId: 'v1', from: 6, to: 9 })).toBe(7.5);
	});

	test('two gaps are the same when track and ends agree', () => {
		const g = { trackId: 'v1', from: 6, to: 9 };
		expect(sameGap(g, { ...g })).toBe(true);
		expect(sameGap(g, { ...g, to: 9.5 })).toBe(false);
		expect(sameGap(g, { ...g, trackId: 'v2' })).toBe(false);
		expect(sameGap(g, null)).toBe(false);
		expect(sameGap(null, null)).toBe(true);
	});

	test('a selected gap survives edits elsewhere and is gone once a neighbour moves or the track goes', () => {
		const g = gapAt(v1, 7)!;
		expect(gapStillThere(tl, g)).toEqual(g);
		const moved: Timeline = { tracks: [track('v1', [clip('c1', 2, 2), clip('c2', 4, 2), clip('c3', 8, 1)]), a1] };
		expect(gapStillThere(moved, g)).toBeNull();
		expect(gapStillThere({ tracks: [a1] }, g)).toBeNull();
		const filled: Timeline = { tracks: [track('v1', [...v1.clips, clip('c4', 6, 3)]), a1] };
		expect(gapStillThere(filled, g)).toBeNull();
	});
});

describe('which gap Ripple delete gap closes', () => {
	const two: Timeline = { tracks: [track('v1', [clip('c1', 0, 2), clip('c2', 5, 2)]), track('v2', [clip('d1', 0, 1), clip('d2', 6, 1)])] };

	test('the gap under the playhead on the lane last clicked', () => {
		expect(planGapClose(two, { lane: 'v2', time: 3 })).toEqual({ ok: true, gap: { trackId: 'v2', from: 1, to: 6 }, track: 'V2' });
		expect(planGapClose(two, { lane: 'v1', time: 3 })).toEqual({ ok: true, gap: { trackId: 'v1', from: 2, to: 5 }, track: 'V1' });
	});

	test('only that lane: a gap under the playhead on another lane is not closed', () => {
		// V1 has a gap at 3 and so has V2, but with the playhead at 1.5 only V1 does — and the lane is V2.
		const plan = planGapClose(two, { lane: 'v2', time: 1.5 });
		expect(plan).toEqual({ ok: true, gap: { trackId: 'v2', from: 1, to: 6 }, track: 'V2' });
		const odd: Timeline = { tracks: [track('v1', [clip('c1', 0, 2), clip('c2', 5, 2)]), track('v2', [clip('d1', 0, 9)])] };
		const away = planGapClose(odd, { lane: 'v2', time: 3 });
		expect(away).toEqual({ ok: false, reason: 'There is no gap under the playhead on V2' });
	});

	test('no lane clicked yet says to click one, whatever gaps there are', () => {
		const plan = planGapClose(two, { lane: null, time: 3 });
		expect(plan.ok).toBe(false);
		expect(plan.ok ? '' : plan.reason).toContain('Click a track first');
		expect(planGapClose(two, { lane: 'gone', time: 3 }).ok).toBe(false);
	});

	test('a locked lane is named, not skipped past', () => {
		const lockedTl: Timeline = { tracks: [track('v1', v1.clips, { locked: true }), a1] };
		expect(planGapClose(lockedTl, { lane: 'v1', time: 7 })).toEqual({ ok: false, reason: 'Track V1 is locked' });
	});

	test('the edges of a gap count: the playhead on the cut that begins it', () => {
		expect(planGapClose(tl, { lane: 'v1', time: 6 }).ok).toBe(true);
		expect(planGapClose(tl, { lane: 'v1', time: 9 }).ok).toBe(true);
		expect(planGapClose(tl, { lane: 'v1', time: 5 }).ok).toBe(false);
	});

	test('after the last clip there is no gap to close', () => {
		expect(planGapClose(tl, { lane: 'v1', time: 12 })).toEqual({ ok: false, reason: 'There is no gap under the playhead on V1' });
	});
});
