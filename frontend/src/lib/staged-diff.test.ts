import { describe, expect, test } from 'bun:test';
import { changeIndex, changePoints, diffView, isEmptyView, stepChange } from './staged-diff';
import { frameTime } from './frames';
import type { Clip, DiffEntry, TextOverlay, Timeline, Track } from './types';

const clip = (id: string, start: number, len = 4): Clip => ({
	id,
	asset_id: 'a',
	source_in: 0,
	source_out: len,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0
});
const track = (id: string, kind: 'video' | 'audio', clips: Clip[]): Track => ({ id, kind, name: id.toUpperCase(), clips });
const overlay = (id: string, start: number, end: number, text = id): TextOverlay =>
	({ id, text, start, end, pos_x: 0.5, pos_y: 0.5, size: 0.08, color: 'white' }) as TextOverlay;

// live: V1 [x 0..4) [y 4..8) [z 8..12)     A1 [m 0..12)     title t1 [1, 3), t2 [5, 6)
const live: Timeline = {
	tracks: [track('v1', 'video', [clip('x', 0), clip('y', 4), clip('z', 8)]), track('a1', 'audio', [clip('m', 0, 12)])],
	overlays: [overlay('t1', 1, 3), overlay('t2', 5, 6)]
};
// proposed: x trimmed, y gone, z moved up, a new clip n on a new track v2, m gone; t1 retimed, t2 gone, t3 new
const proposed: Timeline = {
	tracks: [
		track('v1', 'video', [clip('x', 0, 3), clip('z', 3)]),
		track('v2', 'video', [clip('n', 1, 2)]),
		track('a1', 'audio', [])
	],
	overlays: [overlay('t1', 2, 3), overlay('t3', 0, 1)]
};
const entries: DiffEntry[] = [
	{ kind: 'track_added', summary: 'Added video track V2', track_id: 'v2' },
	{ kind: 'clip_removed', summary: 'Removed clip from V1', track_id: 'v1', clip_id: 'y', at: 4 },
	{ kind: 'clip_removed', summary: 'Removed clip from A1', track_id: 'a1', clip_id: 'm', at: 0 },
	{ kind: 'clip_retrimmed', summary: 'Trimmed clip', track_id: 'v1', clip_id: 'x', at: 0 },
	{ kind: 'clip_moved', summary: 'Moved clip', track_id: 'v1', clip_id: 'z', at: 3 },
	{ kind: 'clip_added', summary: 'Added clip', track_id: 'v2', clip_id: 'n', at: 1 },
	{ kind: 'overlay_changed', summary: 'Changed text', overlay_id: 't1', at: 2 },
	{ kind: 'overlay_added', summary: 'Added text', overlay_id: 't3', at: 0 },
	{ kind: 'overlay_removed', summary: 'Removed text', overlay_id: 't2', at: 5 },
	{ kind: 'marker_added', summary: 'Added marker', at: 7 }
];

describe('diffView', () => {
	const v = diffView(entries, proposed, live);

	test('what the proposal adds is added, what it moves or trims is shifted', () => {
		expect(v.clips.get('n')).toBe('added');
		expect(v.clips.get('x')).toBe('shifted');
		expect(v.clips.get('z')).toBe('shifted');
		expect(v.clips.has('y')).toBe(false);
	});

	test('titles and tracks are marked by id', () => {
		expect(v.titles.get('t3')).toBe('added');
		expect(v.titles.get('t1')).toBe('shifted');
		expect(v.tracks.get('v2')).toBe('added');
	});

	test('a removed clip is a ghost where it stood on the live cut, on a track that survives', () => {
		expect(v.removed).toEqual([
			{ id: 'y', trackId: 'v1', start: 4, dur: 4 },
			{ id: 'm', trackId: 'a1', start: 0, dur: 12 }
		]);
	});

	test('a clip on a track the proposal removes is the track entry’s business, not a ghost', () => {
		const gone = diffView(
			[{ kind: 'clip_removed', summary: '', track_id: 'v1', clip_id: 'y', at: 4 }],
			{ tracks: [track('a1', 'audio', [])] },
			live
		);
		expect(gone.removed).toEqual([]);
	});

	test('a removed title is a ghost with its old span', () => {
		expect(v.removedTitles).toEqual([{ id: 't2', start: 5, end: 6, text: 't2' }]);
	});

	test('added stays added when the same clip is also adjusted', () => {
		const both = diffView(
			[
				{ kind: 'clip_added', summary: '', clip_id: 'n', track_id: 'v2', at: 1 },
				{ kind: 'clip_changed', summary: '', clip_id: 'n', track_id: 'v2', at: 1 },
				{ kind: 'clip_retrimmed', summary: '', clip_id: 'q', track_id: 'v2', at: 1 },
				{ kind: 'clip_added', summary: '', clip_id: 'q', track_id: 'v2', at: 1 }
			],
			proposed,
			live
		);
		expect(both.clips.get('n')).toBe('added');
		expect(both.clips.get('q')).toBe('added');
	});

	test('an entry that names nothing it can draw (markers, format, master) marks nothing', () => {
		const quiet = diffView(
			[
				{ kind: 'marker_added', summary: '', at: 1 },
				{ kind: 'format_changed', summary: '' },
				{ kind: 'master_changed', summary: '' },
				{ kind: 'overlay_added', summary: '' }
			],
			proposed,
			live
		);
		expect(isEmptyView(quiet)).toBe(true);
		expect(isEmptyView(v)).toBe(false);
	});

	test('no entries is an empty view', () => {
		expect(isEmptyView(diffView([], proposed, live))).toBe(true);
	});
});

describe('walking the changes', () => {
	const pts = changePoints(entries);
	const [fps, dur] = [30, 600];

	test('the times of the changes, ascending and without repeats', () => {
		expect(pts).toEqual([0, 1, 2, 3, 4, 5, 7]);
		expect(changePoints([{ kind: 'clip_added', summary: '', at: 2 }, { kind: 'clip_moved', summary: '', at: 2 }, { kind: 'format_changed', summary: '' }])).toEqual([2]);
	});

	test('next and previous move on from the playhead, never to where it is', () => {
		expect(stepChange(pts, 0, 1, fps, dur)).toBe(1);
		expect(stepChange(pts, 1, 1, fps, dur)).toBe(2);
		expect(stepChange(pts, 2.5, 1, fps, dur)).toBe(3);
		expect(stepChange(pts, 7, 1, fps, dur)).toBeNull();
		expect(stepChange(pts, 7, -1, fps, dur)).toBe(5);
		expect(stepChange(pts, 4.2, -1, fps, dur)).toBe(4);
		expect(stepChange(pts, 0, -1, fps, dur)).toBeNull();
	});

	test('a playhead a hair off a change is on it', () => {
		expect(stepChange(pts, 2 + 1e-6, 1, fps, dur)).toBe(3);
		expect(stepChange(pts, 2 - 1e-6, -1, fps, dur)).toBe(1);
	});

	test('a change between frames is stepped to once: the playhead lands on its frame and goes on from there', () => {
		// 5.1234 s is not on a frame of 30 fps; the playhead goes to frame 154 and the next press must not pick it again
		const odd = [2, 5.1234, 9];
		let at = 2;
		const seen: number[] = [];
		for (let i = 0; i < 4; i++) {
			const next = stepChange(odd, at, 1, fps, dur);
			if (next === null) break;
			seen.push(next);
			at = next;
		}
		// the start of a change is its first frame: 5.1234 s is inside frame 154 (5.1333 s), not before it
		expect(seen).toEqual([frameTime(154, 30), 9]);
		// and back again
		expect(stepChange(odd, 9, -1, fps, dur)).toBe(frameTime(154, 30));
		expect(stepChange(odd, frameTime(154, 30), -1, fps, dur)).toBe(2);
	});

	test('a change begins on its first frame: the playhead is never put before it', () => {
		// 5.1067 s is frame 153.2: rounding would stand on 153 (5.1 s), where the new clip is not yet there
		expect(stepChange([5.1067], 0, 1, fps, dur)).toBe(frameTime(154, 30));
		expect(changeIndex([5.1067], frameTime(153, 30), fps, dur)).toBe(0);
		expect(changeIndex([5.1067], frameTime(154, 30), fps, dur)).toBe(1);
	});

	test('which change the playhead is at, for "3 of 7"', () => {
		expect(changeIndex(pts, -1, fps, dur)).toBe(0);
		expect(changeIndex(pts, 0, fps, dur)).toBe(1);
		expect(changeIndex(pts, 2.5, fps, dur)).toBe(3);
		expect(changeIndex(pts, 100, fps, dur)).toBe(7);
		// on the frame the playhead went to for a change between frames, it is at that change
		expect(changeIndex([5.1234], frameTime(154, 30), fps, dur)).toBe(1);
	});

	test('nothing to walk', () => {
		expect(stepChange([], 3, 1, fps, dur)).toBeNull();
		expect(stepChange([], 3, -1, fps, dur)).toBeNull();
	});
});
