import { describe, expect, test } from 'bun:test';
import * as ops from './link-ops';
import type { Clip, Timeline } from './types';

// The lock rules `Project`'s single-clip ops follow (project.rs
// `single_clip_layout_edits_refuse_a_locked_track_as_the_group_edits_do` and
// `nothing_is_put_on_a_locked_track`), as the harness mirror makes them.

const clip = (id: string, start: number, src: number): Clip =>
	({ id, asset_id: 'a', source_in: src, source_out: src + 4, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0 }) as Clip;

/** V1 holding `a [0,4)`, `b [4,8)`, `c [8,12)`, abutting, of a 60 s asset; V2 empty. */
function cut(lockV1: boolean, lockV2 = false): Timeline {
	return {
		tracks: [
			{ id: 'v1', kind: 'video', name: 'V1', locked: lockV1, clips: [clip('a', 0, 10), clip('b', 4, 20), clip('c', 8, 30)] },
			{ id: 'v2', kind: 'video', name: 'V2', locked: lockV2, clips: [] }
		]
	} as unknown as Timeline;
}

const env: ops.EditEnv = { ripple: false, links: true, footage: new Map([['a', 60]]), hasAudio: () => true };
const asset = { id: 'a', name: 'a', duration: 60, streams: [{ kind: 'video' }] } as never;

const layout: [string, (t: Timeline) => unknown][] = [
	['split', (t) => ops.splitAt(t, env, 'b', 5)],
	['trim', (t) => ops.trim(t, env, 'b', null, 23.5)],
	['trim and move', (t) => ops.trim(t, env, 'c', null, null, 8.25)],
	['cut range', (t) => ops.cutRange(t, env, 'b', 21, 22)],
	['reorder', (t) => ops.reorder(t, env, 'v1', 'b', 2)],
	['ripple delete', (t) => ops.rippleDelete(t, env, 'b')],
	['remove', (t) => ops.remove(t, env, 'b')],
	['speed', (t) => ops.setSpeed(t, env, 'b', 2)],
	['duplicate', (t) => ops.duplicateClips(t, env, ['b'], 30)]
];

describe('a locked track', () => {
	for (const [name, op] of layout) {
		test(`refuses ${name}, and the same op goes through unlocked`, () => {
			const locked = cut(true);
			const before = structuredClone(locked);
			expect(() => op(locked)).toThrow('invalid argument: track V1 is locked');
			expect(locked).toEqual(before);
			expect(() => op(cut(false))).not.toThrow();
		});
	}

	test('takes nothing put on it: a paste, or a clip added to it by name', () => {
		const t = cut(false, true);
		expect(() => ops.insertClips(t, env, [{ track_id: 'v2', clip: clip('x', 0, 0) }], 0)).toThrow('track V2 is locked');
		expect(() => ops.addClip(t, env, asset, 'v2', 0, 1, 50)).toThrow('track V2 is locked');
	});

	test('named no track, a clip goes to the first unlocked one of its kind; with every one locked, nowhere', () => {
		const placed = ops.addClip(cut(true), env, asset, null, 0, 1, 50);
		expect(placed.timeline.tracks[1].clips.map((c) => c.id)).toEqual([placed.result.id]);
		expect(() => ops.addClip(cut(true, true), env, asset, null, 0, 1, 50)).toThrow('every video track is locked');
	});

	test('a paste puts nothing on a track it cannot play on (`Project::insert_clips`)', () => {
		const t = cut(false);
		t.tracks.push({ id: 'a1', kind: 'audio', name: 'A1', locked: false, clips: [] } as never);
		const known = { ...env, asset: (id: string) => (id === 'm' ? { name: 'm', duration: 9, streams: [{ kind: 'audio' }] } : { name: 'a', duration: 60, streams: [{ kind: 'video' }] }) };
		const sound = { ...clip('s', 0, 0), asset_id: 'm' } as Clip;
		expect(() => ops.insertClips(t, known, [{ track_id: 'v2', clip: sound }], 0)).toThrow('a clip with no picture cannot go on video track V2');
		expect(() => ops.insertClips(t, known, [{ track_id: 'a1', clip: clip('p', 0, 0) }], 0)).toThrow('a clip with no sound cannot go on audio track A1');
		expect(() => ops.insertClips(t, known, [{ track_id: 'a1', clip: sound }], 0)).not.toThrow();
	});

	test('the sound of an asset gets a track of its own when every audio track is locked', () => {
		const t = cut(false);
		t.tracks.push({ id: 'a1', kind: 'audio', name: 'A1', locked: true, clips: [] } as never);
		const added = ops.addAssetAudio(t, env, { id: 'a', duration: 60 });
		expect(added.timeline.tracks.map((x) => x.name)).toEqual(['V1', 'V2', 'A1', 'A2']);
		expect(added.timeline.tracks[3].clips).toHaveLength(1);
	});
});
