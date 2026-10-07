import { describe, expect, test } from 'bun:test';
import { moveClips, removeClips } from './multi-edit';
import type { Clip, ClipMove, StreamKind, Timeline, Track } from './types';

// `moveClips` / `removeClips` are the port of `Timeline::move_clips` /
// `remove_clips`; these replay the Rust tests (`// ---- multi-clip moves and
// removals` in model.rs) case for case.

let seq = 0;
const rclip = (start: number, dur: number): Clip => ({
	id: `clip-${++seq}`,
	asset_id: 'asset',
	source_in: 0,
	source_out: dur,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0
});
const track = (kind: StreamKind, name: string, clips: Clip[], locked = false): Track => ({
	id: `track-${name}`,
	kind,
	name,
	clips,
	...(locked ? { locked } : {})
});
const oneLane = (clips: Clip[]): Timeline => ({ tracks: [track('video', 'V1', clips)] });
const spansOf = (t: Timeline, ti: number) => t.tracks[ti].clips.map((c) => [c.timeline_start, c.timeline_start + (c.source_out - c.source_in)]);
const mv = (clip: Clip, start: number): ClipMove => ({ clip_id: clip.id, timeline_start: start });

/** The message a refused group carries. */
function refusal(run: () => unknown): string {
	try {
		run();
	} catch (e) {
		return (e as Error).message;
	}
	throw new Error('expected a refusal');
}

describe('moveClips', () => {
	test('a group moves together and may pass through the places it is leaving', () => {
		// Three abutting clips nudged by a second: each lands on its neighbour's old
		// spot, which moving them one at a time would reject.
		const clips = [rclip(0, 2), rclip(2, 2), rclip(4, 2)];
		const t = oneLane(clips);
		const moved = moveClips(t, [mv(clips[2], 5), mv(clips[0], 1), mv(clips[1], 3)]);
		expect(moved.map((c) => c.timeline_start)).toEqual([5, 1, 3]); // reported in request order
		expect(spansOf(t, 0)).toEqual([
			[1, 3],
			[3, 5],
			[5, 7]
		]); // lane re-sorted
	});

	test('clips can swap places', () => {
		const clips = [rclip(0, 2), rclip(2, 2)];
		const t = oneLane(clips);
		moveClips(t, [mv(clips[0], 2), mv(clips[1], 0)]);
		expect(t.tracks[0].clips[0].id).toBe(clips[1].id);
		expect(spansOf(t, 0)).toEqual([
			[0, 2],
			[2, 4]
		]);
	});

	test('a group can change tracks of the same kind', () => {
		const a = rclip(0, 2);
		const t: Timeline = {
			tracks: [track('video', 'V1', [a]), track('video', 'V2', [rclip(0, 1)]), track('audio', 'A1', [])]
		};
		moveClips(t, [{ ...mv(a, 1), track_id: 'track-V2' }]);
		expect(t.tracks[0].clips).toHaveLength(0);
		expect(spansOf(t, 1)).toEqual([
			[0, 1],
			[1, 3]
		]);

		expect(refusal(() => moveClips(t, [{ ...mv(a, 1), track_id: 'track-A1' }]))).toContain('different kind');
	});

	test('a move that cannot land refuses the whole group and changes nothing', () => {
		const clips = [rclip(0, 2), rclip(2, 2), rclip(10, 2)];
		const t = oneLane(clips);
		const untouched = structuredClone(t);

		// One good move and one that lands on the clip that is staying.
		const why = refusal(() => moveClips(t, [mv(clips[0], 5), mv(clips[1], 9.5)]));
		expect(why).toContain('overlap');
		expect(why).toContain('V1');
		// Two moved clips on top of each other.
		expect(refusal(() => moveClips(t, [mv(clips[0], 5), mv(clips[1], 6)]))).toContain('overlap');
		// The same clip twice, a start before 0, a start that is not a number.
		expect(refusal(() => moveClips(t, [mv(clips[0], 5), mv(clips[0], 6)]))).toContain('more than once');
		expect(refusal(() => moveClips(t, [mv(clips[0], -1)]))).toContain('before the beginning');
		expect(refusal(() => moveClips(t, [mv(clips[0], NaN)]))).toContain('finite');
		expect(refusal(() => moveClips(t, []))).toContain('no clips');
		// An unknown clip and an unknown track.
		expect(refusal(() => moveClips(t, [mv(clips[0], 5), mv(rclip(0, 1), 6)]))).toContain('clip not found');
		expect(refusal(() => moveClips(t, [{ ...mv(clips[0], 5), track_id: 'nowhere' }]))).toBe('track not found: nowhere');

		// A locked track, as the source…
		const lockedLane: Timeline = { tracks: [track('video', 'V1', [clips[0]], true)] };
		expect(refusal(() => moveClips(lockedLane, [mv(clips[0], 5)]))).toContain('locked');
		// …and as the destination.
		const two: Timeline = { tracks: [track('video', 'V1', [clips[0]]), track('video', 'V2', [], true)] };
		expect(refusal(() => moveClips(two, [{ ...mv(clips[0], 0), track_id: 'track-V2' }]))).toContain('V2 is locked');

		expect(t).toEqual(untouched); // every refusal left the lane exactly as it was
	});

	test('the refusals read as the backend words them', () => {
		const clips = [rclip(0, 2)];
		expect(refusal(() => moveClips(oneLane(clips), [mv(clips[0], -1)]))).toMatch(/^invalid argument: /);
		expect(refusal(() => moveClips(oneLane(clips), [mv(rclip(0, 1), 1)]))).toMatch(/^clip not found: /);
	});

	test('a start a hair below zero is the float noise it is, and lands at 0', () => {
		const clips = [rclip(3, 2)];
		const t = oneLane(clips);
		moveClips(t, [mv(clips[0], -1e-9)]);
		expect(t.tracks[0].clips[0].timeline_start).toBe(0);
	});

	test('a moved clip is handed back as a copy of what landed', () => {
		const clips = [rclip(0, 2)];
		const t = oneLane(clips);
		const [landed] = moveClips(t, [mv(clips[0], 4)]);
		landed.timeline_start = 99;
		expect(t.tracks[0].clips[0].timeline_start).toBe(4);
	});
});

describe('removeClips', () => {
	test('removing several clips is all or nothing', () => {
		const clips = [rclip(0, 2), rclip(2, 2), rclip(4, 2)];
		const t = oneLane(clips);

		// A clip named twice goes once.
		expect(removeClips(t, [clips[0].id, clips[2].id, clips[0].id])).toBe(2);
		expect(spansOf(t, 0)).toEqual([[2, 4]]); // gaps are left

		// One unknown id refuses the lot.
		const before = structuredClone(t);
		expect(refusal(() => removeClips(t, [clips[1].id, 'nope']))).toBe('clip not found: nope');
		expect(t).toEqual(before);
		expect(refusal(() => removeClips(t, []))).toBe('invalid argument: no clips to remove');

		// So does a locked track.
		t.tracks[0].locked = true;
		expect(refusal(() => removeClips(t, [clips[1].id]))).toContain('locked');
		expect(t.tracks[0].locked).toBe(true);
		expect(t.tracks[0].clips).toHaveLength(1);
	});
});
