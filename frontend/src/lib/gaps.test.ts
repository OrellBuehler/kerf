import { describe, expect, test } from 'bun:test';
import { gapAt, gapPoint, gapStillThere, gapsOf, planGapClose, sameGap } from './gaps';
import { displayTracks } from './track-order';
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
	const drawn = displayTracks(tl.tracks);
	const base = { selectedGap: null, selected: [] as string[], primary: null, time: 0, drawn };

	test('the selected gap, while it is there', () => {
		const g = gapAt(v1, 7)!;
		const plan = planGapClose(tl, { ...base, selectedGap: g, time: 0 });
		expect(plan).toEqual({ ok: true, gap: g, track: 'V1' });
	});

	test('else the gap the playhead is in, on a track in drawn order', () => {
		const plan = planGapClose(tl, { ...base, time: 7 });
		expect(plan).toEqual({ ok: true, gap: { trackId: 'v1', from: 6, to: 9 }, track: 'V1' });
	});

	test('with clips selected, only their tracks are looked at', () => {
		const two: Timeline = { tracks: [track('v1', [clip('c1', 0, 2), clip('c2', 5, 2)]), track('v2', [clip('d1', 0, 1), clip('d2', 6, 1)])] };
		const d = displayTracks(two.tracks);
		const plan = planGapClose(two, { ...base, drawn: d, selected: ['d1'], primary: 'd1', time: 3 });
		expect(plan).toEqual({ ok: true, gap: { trackId: 'v2', from: 1, to: 6 }, track: 'V2' });
		expect(planGapClose(two, { ...base, drawn: d, time: 3 }).ok).toBe(true);
	});

	test('a stale selection falls back to the playhead', () => {
		const stale = { trackId: 'v1', from: 6, to: 9.5 };
		const plan = planGapClose(tl, { ...base, selectedGap: stale, time: 1 });
		expect(plan).toEqual({ ok: true, gap: { trackId: 'v1', from: 0, to: 2 }, track: 'V1' });
	});

	test('a locked track is named, not skipped past', () => {
		const lockedTl: Timeline = { tracks: [track('v1', v1.clips, { locked: true }), a1] };
		const plan = planGapClose(lockedTl, { ...base, drawn: displayTracks(lockedTl.tracks), time: 7 });
		expect(plan).toEqual({ ok: false, reason: 'Track V1 is locked' });
		const g = gapAt(lockedTl.tracks[0], 7)!;
		expect(planGapClose(lockedTl, { ...base, drawn: displayTracks(lockedTl.tracks), selectedGap: g })).toEqual({ ok: false, reason: 'Track V1 is locked' });
	});

	test('no gap at the playhead says where to find one', () => {
		const plan = planGapClose(tl, { ...base, time: 5 });
		expect(plan.ok).toBe(false);
		if (!plan.ok) expect(plan.reason).toContain('no gap');
	});
});
