import { beforeEach, describe, expect, test } from 'bun:test';
import { analyzeClipStabilization, clipStabilizationStatus, getHistory, getTimeline, revertTo, setClipStabilize } from './api';
import type { Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's stabilization, which keeps the backend's
// contract: one revision per change, the same refusals, a setting absent from the clip until it is set, and a
// status that is `missing` until an analysis lands (the harness has no decoder: its analysis just lands).

beforeEach(async () => {
	await revertTo(0);
});

const firstVideoClip = (t: Timeline) => t.tracks.find((tr) => tr.kind === 'video' && tr.clips.length)!.clips[0];
const firstAudioClip = (t: Timeline) => t.tracks.find((tr) => tr.kind === 'audio' && tr.clips.length)?.clips[0];

describe('stabilization (browser harness)', () => {
	test('switching it on is one revision, its status waits for an analysis, and off removes it', async () => {
		const clip = firstVideoClip(await getTimeline());
		expect(clip.stabilize).toBeUndefined();
		const t = await setClipStabilize(clip.id, { strength: 0.5 });
		expect(firstVideoClip(t).stabilize).toEqual({ strength: 0.5 });
		expect((await getHistory()).find((h) => h.current)?.label).toBe('Stabilize clip');
		expect(await clipStabilizationStatus([clip.id])).toEqual([{ clip_id: clip.id, state: 'missing' }]);
		expect(await analyzeClipStabilization(clip.id)).toEqual({ clip_id: clip.id, state: 'ready' });
		expect(await clipStabilizationStatus()).toEqual([{ clip_id: clip.id, state: 'ready' }]);
		const off = await setClipStabilize(clip.id, null);
		expect(firstVideoClip(off).stabilize).toBeUndefined();
		expect((await getHistory()).find((h) => h.current)?.label).toBe('Stop stabilizing clip');
		expect(await clipStabilizationStatus([clip.id])).toEqual([{ clip_id: clip.id, state: 'off' }]);
	});

	test('a fixed zoom is kept, and numbers out of range or a clip of sound are refused', async () => {
		const t = await getTimeline();
		const clip = firstVideoClip(t);
		expect(firstVideoClip(await setClipStabilize(clip.id, { strength: 1, zoom: 8 })).stabilize).toEqual({ strength: 1, zoom: 8 });
		await expect(setClipStabilize(clip.id, { strength: 2 })).rejects.toThrow('between 0 and 1');
		await expect(setClipStabilize(clip.id, { strength: 0.5, zoom: 31 })).rejects.toThrow('percent');
		const sound = firstAudioClip(t);
		if (sound) await expect(setClipStabilize(sound.id, { strength: 0.5 })).rejects.toThrow('audio track');
		await expect(analyzeClipStabilization('nope')).rejects.toThrow('not found');
	});
});
