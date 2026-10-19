import { beforeEach, describe, expect, test } from 'bun:test';
import { addTrack, getTimeline, listAssets, moveClip, removeSilence, revertTo, setClipEnabled, setTrackLocked, slipClip } from './api';

// The browser harness refuses what the backend refuses on a locked track (`Timeline::ensure_editable`), with the
// same words, so the editor is not built against a harness that lets a locked clip move. The harness cut is V1
// `c1 [0, 12.5)  c2 [12.5, 20.5)` over A1 `c3` (c1's sound); `link: false` keeps the partners out of it.

beforeEach(async () => {
	await revertTo(0);
});

describe('a locked track in the browser harness', () => {
	test('a clip on it does not move, and nothing moves onto it', async () => {
		await setTrackLocked('v1', true);
		await expect(moveClip('c2', 30, undefined, false)).rejects.toThrow('track V1 is locked');
		await setTrackLocked('v1', false);
		const v2 = (await addTrack('video')).tracks.find((t) => t.kind === 'video' && t.id !== 'v1')!;
		await setTrackLocked(v2.id, true);
		await expect(moveClip('c2', 30, v2.id, false)).rejects.toThrow(`track ${v2.name} is locked`);
		expect((await getTimeline()).tracks[0].clips.map((c) => c.timeline_start)).toEqual([0, 12.5]);
	});

	test('a clip on it is not disabled', async () => {
		await setTrackLocked('v1', true);
		await expect(setClipEnabled('c2', false)).rejects.toThrow('track V1 is locked');
		expect((await getTimeline()).tracks[0].clips[1].enabled).not.toBe(false);
	});

	test('a clip on it is not slipped', async () => {
		await setTrackLocked('v1', true);
		const before = (await getTimeline()).tracks[0].clips[1].source_in;
		await expect(slipClip('c2', 0.5, false)).rejects.toThrow('locked');
		expect((await getTimeline()).tracks[0].clips[1].source_in).toBe(before);
	});

	test('remove silence goes to the first unlocked video track, and with every one locked refuses', async () => {
		const [interview] = await listAssets();
		await setTrackLocked('v1', true);
		await expect(removeSilence(interview.id)).rejects.toThrow('every video track is locked — unlock one, or add a track');
	});
});
