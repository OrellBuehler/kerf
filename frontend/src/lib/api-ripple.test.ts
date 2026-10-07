import { beforeEach, describe, expect, test } from 'bun:test';
import {
	addClip,
	addTrack,
	getHistory,
	getRippleMode,
	getTimeline,
	listAssets,
	moveClips,
	removeClip,
	removeClips,
	revertTo,
	setRippleMode,
	setSpeed,
	setTrackLocked,
	trimClip,
	undo
} from './api';
import type { Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's answer to the
// ripple surface — which has to honour the same contract as the backend's, or the
// editor would be built against behaviour the desktop app never shows. The
// harness cut is V1 `c1 [0, 12.5)  c2 [12.5, 20.5)` over A1 `c3 [0, 120)`; each
// test adds `c4` at 24 on V1, leaving a gap of 3.5s before it.

const starts = (t: Timeline, track: number) => t.tracks[track].clips.map((c) => c.timeline_start);

async function withFourthClip(): Promise<string> {
	const [interview] = await listAssets();
	const t = await addClip(interview.id, 0, 4, 'v1', 24);
	expect(starts(t, 0)).toEqual([0, 12.5, 24]);
	return t.tracks[0].clips[2].id;
}

beforeEach(async () => {
	await setRippleMode(false);
	await revertTo(0);
});

describe('ripple mode (browser harness)', () => {
	test('it is off until someone turns it on, and reads back what was written', async () => {
		expect(await getRippleMode()).toBe(false);
		expect(await setRippleMode(true)).toBe(true);
		expect(await getRippleMode()).toBe(true);
		expect(await setRippleMode(false)).toBe(false);
	});

	test('a setting is not an edit: no revision is recorded', async () => {
		const revisions = (await getHistory()).length;
		await setRippleMode(true);
		await setRippleMode(false);
		expect((await getHistory()).length).toBe(revisions);
	});

	test('off, a trim leaves the later clips where they were', async () => {
		await withFourthClip();
		const t = await trimClip('c1', 0, 10);
		expect(starts(t, 0)).toEqual([0, 12.5, 24]);
	});

	test('on, a trim carries the track along and leaves the other tracks alone', async () => {
		await withFourthClip();
		await setRippleMode(true);
		const t = await trimClip('c1', 0, 10); // 12.5s -> 10s
		expect(starts(t, 0)).toEqual([0, 10, 21.5]);
		expect(starts(t, 1)).toEqual([0]); // no sync lock
	});

	test('on, a left-edge trim as the GUI sends it keeps the clips start', async () => {
		await withFourthClip();
		await setRippleMode(true);
		// In-point a second later and a start a second later, to hold the right edge.
		const t = await trimClip('c2', 1, undefined, 13.5);
		const c2 = t.tracks[0].clips.find((c) => c.id === 'c2')!;
		expect(c2.timeline_start).toBe(12.5); // restored, not 13.5
		expect(c2.source_in).toBe(1);
		expect(starts(t, 0)).toEqual([0, 12.5, 23]);
	});

	test('on, a trim is one revision and undo puts the whole track back', async () => {
		await withFourthClip();
		await setRippleMode(true);
		const revisions = (await getHistory()).length;
		await trimClip('c1', 0, 10);
		expect((await getHistory()).length).toBe(revisions + 1);
		expect(starts(await undo(), 0)).toEqual([0, 12.5, 24]);
	});

	test('on, a speed change follows the change in duration', async () => {
		await withFourthClip();
		await setRippleMode(true);
		const t = await setSpeed('c1', 2); // 12.5s -> 6.25s
		expect(starts(t, 0)).toEqual([0, 6.25, 17.75]);
	});

	test('on, removing a clip closes its span and keeps the gap before the next', async () => {
		await withFourthClip();
		await setRippleMode(true);
		const t = await removeClip('c2'); // 8s
		expect(starts(t, 0)).toEqual([0, 16]);
	});

	test('on, a clip dropped onto footage pushes it; one that fits moves nothing', async () => {
		await withFourthClip();
		await setRippleMode(true);
		const [interview] = await listAssets();
		// 2s at 12.5 lands on the head of c2.
		let t = await addClip(interview.id, 0, 2, 'v1', 12.5);
		expect(starts(t, 0)).toEqual([0, 12.5, 14.5, 26]);
		// 1s in the free stretch after the last clip.
		t = await addClip(interview.id, 0, 1, 'v1', 40);
		expect(starts(t, 0)).toEqual([0, 12.5, 14.5, 26, 40]);
	});

	test('on, a lengthening trim pushes the track right, and trimming back restores it', async () => {
		await withFourthClip();
		await setRippleMode(true);
		let t = await trimClip('c1', 0, 12.5 + 12);
		expect(starts(t, 0)).toEqual([0, 24.5, 36]);
		t = await trimClip('c1', 0, 12.5);
		expect(starts(t, 0)).toEqual([0, 12.5, 24]);
	});

	test('on, a locked track never moves', async () => {
		await withFourthClip();
		await setRippleMode(true);
		await setTrackLocked('v1', true);
		const t = await trimClip('c1', 0, 10);
		expect(starts(t, 0)).toEqual([0, 12.5, 24]);
	});

	test('a trim that makes a clip empty is refused, as the backend refuses it', async () => {
		await expect(trimClip('c1', 5, 5)).rejects.toThrow('source_out must be greater than source_in');
	});
});

describe('remove_clips (browser harness)', () => {
	test('off, it leaves the gaps, as one revision', async () => {
		const c4 = await withFourthClip();
		const revisions = (await getHistory()).length;
		const t = await removeClips(['c1', c4]);
		expect(starts(t, 0)).toEqual([12.5]);
		const history = await getHistory();
		expect(history.length).toBe(revisions + 1);
		expect(history[history.length - 1].label).toBe('Remove 2 clips');
	});

	test('forced on, every track closes up behind what it lost', async () => {
		const c4 = await withFourthClip();
		const t = await removeClips(['c1', c4], true);
		expect(starts(t, 0)).toEqual([0]); // c2: 12.5 - 12.5
		const history = await getHistory();
		expect(history[history.length - 1].label).toBe('Ripple delete 2 clips');
	});

	test('forced off over a project that is on, it leaves the gaps', async () => {
		await withFourthClip();
		await setRippleMode(true);
		const t = await removeClips(['c1'], false);
		expect(starts(t, 0)).toEqual([12.5, 24]);
		expect(await getRippleMode()).toBe(true); // the override never touched the project's flag
	});

	test('omitted, it follows the project mode', async () => {
		await withFourthClip();
		await setRippleMode(true);
		const t = await removeClips(['c1']);
		expect(starts(t, 0)).toEqual([0, 11.5]);
		const history = await getHistory();
		expect(history[history.length - 1].label).toBe('Ripple delete');
	});

	test('an unknown id refuses the lot and records nothing', async () => {
		await withFourthClip();
		const revisions = (await getHistory()).length;
		await expect(removeClips(['c1', 'nope'], true)).rejects.toThrow('clip not found: nope');
		await expect(removeClips([])).rejects.toThrow('no clips to remove');
		expect((await getHistory()).length).toBe(revisions);
		expect(starts(await getTimeline(), 0)).toEqual([0, 12.5, 24]);
	});
});

describe('move_clips (browser harness)', () => {
	test('a group lands as one revision and never ripples', async () => {
		const c4 = await withFourthClip();
		await setRippleMode(true);
		const revisions = (await getHistory()).length;
		const t = await moveClips([
			{ clip_id: 'c2', timeline_start: 14 },
			{ clip_id: c4, timeline_start: 26 }
		]);
		expect(starts(t, 0)).toEqual([0, 14, 26]);
		const history = await getHistory();
		expect(history.length).toBe(revisions + 1);
		expect(history[history.length - 1].label).toBe('Move 2 clips');
	});

	test('a group that cannot land changes nothing', async () => {
		const c4 = await withFourthClip();
		const revisions = (await getHistory()).length;
		// c2 would be fine at 14.5; c4 lands on c1.
		await expect(
			moveClips([
				{ clip_id: 'c2', timeline_start: 14.5 },
				{ clip_id: c4, timeline_start: 5 }
			])
		).rejects.toThrow('overlap');
		await expect(moveClips([{ clip_id: 'c2', timeline_start: -1 }])).rejects.toThrow('before the beginning');
		await expect(moveClips([{ clip_id: 'c3', timeline_start: 1, track_id: 'v1' }])).rejects.toThrow('different kind');
		expect((await getHistory()).length).toBe(revisions);
		expect(starts(await getTimeline(), 0)).toEqual([0, 12.5, 24]);
	});

	test('a clip can change to another track of its kind', async () => {
		const c4 = await withFourthClip();
		const t0 = await addTrack('video', 'V2');
		const v2 = t0.tracks.find((t) => t.name === 'V2')!;
		const t = await moveClips([{ clip_id: c4, timeline_start: 2, track_id: v2.id }]);
		expect(t.tracks.find((tr) => tr.id === v2.id)!.clips.map((c) => c.id)).toEqual([c4]);
		expect(starts(t, 0)).toEqual([0, 12.5]);
	});
});
