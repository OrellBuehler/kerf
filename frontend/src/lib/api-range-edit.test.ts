import { beforeEach, describe, expect, test } from 'bun:test';
import {
	extractRange,
	getHistory,
	getTimeline,
	liftRange,
	revertTo,
	rippleDeleteGap,
	setRippleMode,
	setTrackLocked,
	splitAllTracks,
	splitRemoveClips,
	undo
} from './api';
import type { Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's answer to the range edits — the same contract as
// the backend's (the pure maths is `range-edit.ts`, replayed against the Rust tests and kerf-core's corpus). The
// harness cut is V1 `c1 [0, 12.5)` of the 120 s interview abutting `c2 [12.5, 20.5)` of the b-roll, over A1 `c3`, the
// interview's sound for the first span, linked to c1 (its picture is muted: sound detached).

const lane = (t: Timeline, track: number) => t.tracks[track].clips.map((c) => [c.timeline_start, c.timeline_start + (c.source_out - c.source_in)]);
const headSeq = async () => (await getHistory()).find((r) => r.current)!.seq;
const lastLabel = async () => (await getHistory()).at(-1)!.label;

beforeEach(async () => {
	await setRippleMode(false);
	await revertTo(0);
});

describe('range edits (browser harness)', () => {
	test('cutting every track is one revision that undo takes back, and says what it cut', async () => {
		const before = await headSeq();
		const done = await splitAllTracks(3);
		expect(done.result.cuts.map((c) => c.track_id)).toEqual([done.timeline.tracks[0].id, done.timeline.tracks[1].id]);
		expect(done.result.skipped).toEqual([]);
		expect(lane(done.timeline, 0)).toEqual([[0, 3], [3, 12.5], [12.5, 20.5]]);
		expect(lane(done.timeline, 1)).toEqual([[0, 3], [3, 12.5]]);
		expect(await headSeq()).toBe(before + 1);
		expect(await lastLabel()).toBe('Split all tracks (2 clips)');
		expect(lane(await undo(), 0)).toEqual([[0, 12.5], [12.5, 20.5]]);
	});

	test('a lift leaves the gap and an extract closes it, whatever the project’s ripple mode', async () => {
		for (const ripple of [false, true]) {
			await revertTo(0);
			await setRippleMode(ripple);
			const lifted = await liftRange(3, 5);
			expect(lane(lifted.timeline, 0), `lift, ripple ${ripple}`).toEqual([[0, 3], [5, 12.5], [12.5, 20.5]]);
			expect(lane(lifted.timeline, 1)).toEqual([[0, 3], [5, 12.5]]);
			expect(lifted.result.split).toBe(2);
			expect(await lastLabel()).toBe('Lift range (2 clips)');
			await revertTo(0);
			const extracted = await extractRange(3, 5);
			expect(lane(extracted.timeline, 0), `extract, ripple ${ripple}`).toEqual([[0, 3], [3, 10.5], [10.5, 18.5]]);
			expect(lane(extracted.timeline, 1)).toEqual([[0, 3], [3, 10.5]]);
			expect(extracted.result.closed).toBe(2);
			expect(await lastLabel()).toBe('Extract range (2 clips)');
		}
	});

	test('naming a track takes the linked partner along; the links off leave it', async () => {
		const v1 = (await getTimeline()).tracks[0].id;
		const linked = await extractRange(3, 5, [v1]);
		expect(lane(linked.timeline, 1)).toEqual([[0, 3], [3, 10.5]]);
		await revertTo(0);
		const alone = await extractRange(3, 5, [v1], false);
		expect(lane(alone.timeline, 0)).toEqual([[0, 3], [3, 10.5], [10.5, 18.5]]);
		expect(lane(alone.timeline, 1)).toEqual([[0, 12.5]]);
	});

	test('a gap on a track is closed and the sound follows', async () => {
		const v1 = (await getTimeline()).tracks[0].id;
		await liftRange(3, 5);
		const done = await rippleDeleteGap(v1, 4);
		expect([done.result.from, done.result.to, done.result.moved]).toEqual([3, 5, 2]);
		expect(lane(done.timeline, 0)).toEqual([[0, 3], [3, 10.5], [10.5, 18.5]]);
		expect(lane(done.timeline, 1)).toEqual([[0, 3], [3, 10.5]]);
		expect(await lastLabel()).toBe('Ripple delete gap');
	});

	test('a locked track is left alone and named, a locked partner refuses, and a refusal records nothing', async () => {
		const before = await headSeq();
		const tl = await getTimeline();
		const [v1, a1] = [tl.tracks[0].id, tl.tracks[1].id];
		await setTrackLocked(a1, true);
		const locked = await headSeq();
		await expect(splitAllTracks(3)).rejects.toThrow('a linked clip is on locked track A1');
		await expect(liftRange(3, 5)).rejects.toThrow('locked track A1');
		const picture = await liftRange(3, 5, [v1], false);
		expect(picture.result.skipped).toEqual([]);
		expect(lane(picture.timeline, 1)).toEqual([[0, 12.5]]);
		await revertTo(locked);
		await expect(extractRange(3, 3)).rejects.toThrow('the range to extract is empty');
		await expect(extractRange(NaN, 3)).rejects.toThrow('the start of the range must be a finite number');
		await expect(splitAllTracks(100)).rejects.toThrow('nothing to cut at');
		await expect(rippleDeleteGap(v1, 5)).rejects.toThrow('not a gap');
		expect(await headSeq()).toBe(locked);
		expect(locked).toBeGreaterThan(before);
	});

	test('a remainder shorter than a frame of the cut refuses the range, naming the clip and the range that works', async () => {
		// The harness cut is the 30 fps interview: 20 ms of head is less than a frame (33.3 ms).
		const head = await headSeq();
		await expect(liftRange(0.02, 5)).rejects.toThrow(
			'lifting 0.02–5 s would leave 20 ms of interview.mp4 on V1 (clip c1) (and 1 more remainder like it), less than a frame (33 ms): start at 0 instead'
		);
		expect(await headSeq()).toBe(head);
		const lifted = await liftRange(0, 5);
		expect([lifted.result.from, lifted.result.to]).toEqual([0, 5]);
		expect(lane(lifted.timeline, 0)).toEqual([[5, 12.5], [12.5, 20.5]]);
		expect(lane(lifted.timeline, 1)).toEqual([[5, 12.5]]);
		await revertTo(0);
		// The b-roll starts at 12.5: a range that starts 10 ms into it would leave 10 ms of it.
		await expect(extractRange(12.51, 14)).rejects.toThrow('extracting 12.51–14 s would leave 10 ms of broll.mp4 on V1');
		await expect(extractRange(12.51, 14)).rejects.toThrow('start at 12.5 instead');
		const extracted = await extractRange(12.5, 14);
		expect(extracted.result.closed).toBeCloseTo(1.5, 9);
		expect(lane(extracted.timeline, 0)).toEqual([[0, 12.5], [12.5, 19]]);
		await revertTo(0);
		// A cut 10 ms from an edge is no cut for that clip; the refusal records nothing.
		await expect(splitAllTracks(0.01)).rejects.toThrow('already a cut to within a frame');
		await expect(splitAllTracks(12.49)).rejects.toThrow('already a cut to within a frame');
		expect(await headSeq()).toBe(head);
	});

	test('a ripple trim to the playhead is one call with ripple on, whatever the mode says', async () => {
		const starts = async () => lane(await getTimeline(), 0).map((s) => s[0]);
		await splitRemoveClips([{ clip_id: 'c1', at: 4 }], 'left', false, true);
		expect(await starts()).toEqual([0, 8.5]);
		await revertTo(0);
		await splitRemoveClips([{ clip_id: 'c1', at: 4 }], 'left', false);
		expect(await starts()).toEqual([4, 12.5]);
		await revertTo(0);
		await setRippleMode(true);
		await splitRemoveClips([{ clip_id: 'c1', at: 4 }], 'left', false, false);
		expect(await starts()).toEqual([4, 12.5]);
		await splitRemoveClips([{ clip_id: 'c1', at: 6 }], 'left', false);
		expect((await starts())[1]).toBeCloseTo(10.5, 9);
	});
});
