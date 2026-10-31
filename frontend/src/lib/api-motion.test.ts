import { beforeEach, describe, expect, test } from 'bun:test';
import { applyMotionPreset, getHistory, getTimeline, listMotionPresets, revertTo, undo } from './api';
import { propertyKeys } from './channels';
import type { Clip, Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's answer to `apply_motion_preset` and
// `list_motion_presets`: the contract of kerf-app's commands (one revision named after the preset and the count,
// all or nothing, a window per clip that a long `duration` clamps), the numbers being `motion-presets.ts`'s,
// which `motion-presets.test.ts` replays against kerf-core's corpus. The harness cut is V1 `c1 [0, 12.5)` and
// `c2 [12.5, 20.5)` over A1's `c3`, the interview's sound.

const clip = (t: Timeline, id: string): Clip => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === id)!;
const opacity = (t: Timeline, id: string) => propertyKeys(clip(t, id), 'opacity').map((k) => [k.time, k.value]);
/** The revision the cut stands at (a redo branch past it is not counted). */
const revisions = async () => (await getHistory()).find((r) => r.current)!.seq;
const lastLabel = async () => (await getHistory()).at(-1)!.label;

beforeEach(async () => {
	await revertTo(0);
});

describe('the catalogue (browser harness)', () => {
	test('lists the 23 presets, entrances first, then exits, then emphases', async () => {
		const list = await listMotionPresets();
		expect(list.length).toBe(23);
		expect(list.map((p) => p.kind).filter((k, i, all) => all.indexOf(k) === i)).toEqual(['entrance', 'exit', 'emphasis']);
		expect(list.find((p) => p.id === 'push_in')?.default_duration).toBeUndefined();
		expect(list.find((p) => p.id === 'pop_in')?.default_duration).toBe(0.5);
	});
});

describe('applying a preset (browser harness)', () => {
	test('is one revision for every clip, named after the preset and the count', async () => {
		const before = await revisions();
		const done = await applyMotionPreset(['c1', 'c2'], 'fade_in');
		expect(await revisions()).toBe(before + 1);
		expect(await lastLabel()).toBe('Apply motion preset Fade in (2 clips)');
		expect(done.applied).toEqual([
			{ clip_id: 'c1', start: 0, end: 0.5 },
			{ clip_id: 'c2', start: 0, end: 0.5 }
		]);
		expect(opacity(done.timeline, 'c1')).toEqual([
			[0, 0],
			[0.5, 1]
		]);
		expect(opacity(await getTimeline(), 'c2')).toEqual([
			[0, 0],
			[0.5, 1]
		]);
		await applyMotionPreset(['c2'], 'pulse');
		expect(await lastLabel()).toBe('Apply motion preset Pulse');
	});

	test('an exit ends on the clip, and a duration longer than the clip is clamped to it', async () => {
		const exit = await applyMotionPreset(['c2'], 'slide_out_to_left', { duration: 1 });
		expect(exit.applied).toEqual([{ clip_id: 'c2', start: 7, end: 8 }]);
		const long = await applyMotionPreset(['c2'], 'fade_in', { duration: 60 });
		expect(long.applied[0]).toEqual({ clip_id: 'c2', start: 0, end: 8 });
	});

	test('add keeps what is outside the window, replace starts the number afresh, undo takes it back', async () => {
		await applyMotionPreset(['c1'], 'fade_in');
		const both = await applyMotionPreset(['c1'], 'fade_out', { mode: 'add', duration: 1 });
		expect(opacity(both.timeline, 'c1')).toEqual([
			[0, 0],
			[0.5, 1],
			[11.5, 1],
			[12.5, 0]
		]);
		const fresh = await applyMotionPreset(['c1'], 'fade_in', { mode: 'replace' });
		expect(opacity(fresh.timeline, 'c1')).toEqual([
			[0, 0],
			[0.5, 1]
		]);
		await undo();
		expect(opacity(await getTimeline(), 'c1').length).toBe(4);
	});

	test('intensity scales how far it departs, within 0.25 to 2', async () => {
		const half = await applyMotionPreset(['c1'], 'zoom_in', { intensity: 0.5 });
		const scale = propertyKeys(clip(half.timeline, 'c1'), 'scale').map((k) => k.value);
		expect(scale[0]).toBeCloseTo(0.85, 12);
		expect(scale[1]).toBe(1);
		await expect(applyMotionPreset(['c1'], 'zoom_in', { intensity: 3 })).rejects.toThrow('intensity must be within 0.25..=2');
	});

	test('a refusal writes nothing at all: a sound, a clip that is not there, nothing named', async () => {
		const before = await revisions();
		await expect(applyMotionPreset(['c1', 'c3'], 'fade_in')).rejects.toThrow('a sound has no picture to move');
		await expect(applyMotionPreset(['c1', 'ghost'], 'fade_in')).rejects.toThrow('clip not found: ghost');
		await expect(applyMotionPreset([], 'fade_in')).rejects.toThrow('name at least one clip');
		await expect(applyMotionPreset(['c1'], 'pulse', { duration: 0 })).rejects.toThrow('duration must be');
		expect(await revisions()).toBe(before);
		expect(opacity(await getTimeline(), 'c1')).toEqual([]);
	});
});
