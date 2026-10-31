import { beforeEach, describe, expect, test } from 'bun:test';
import { bakeBehaviors, getHistory, getTimeline, revertTo, setBehaviors, undo } from './api';
import { baseTransformAt, propertyKeys, transformAt } from './channels';
import type { Behavior, Clip, Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's answer to `set_behaviors` and `bake_behaviors`:
// the contract of kerf-app's commands (one revision named after the edit and the count, all or nothing, a list that
// replaces the clip's, the limits the core refuses), the arithmetic being `behaviors.ts`'s, which
// `behaviors.test.ts` replays against kerf-core's corpus. The harness cut is V1 `c1 [0, 12.5)` and `c2 [12.5, 20.5)`
// over A1's `c3`, the interview's sound.

const clip = (t: Timeline, id: string): Clip => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === id)!;
const revisions = async () => (await getHistory()).find((r) => r.current)!.seq;
const lastLabel = async () => (await getHistory()).at(-1)!.label;
const sway: Behavior = { kind: 'sway', amount: 5, speed: 0.5 };
const shake: Behavior = { kind: 'shake', amount: 0.01, speed: 8, seed: 7 };

beforeEach(async () => {
	await revertTo(0);
});

describe('setting behaviours (browser harness)', () => {
	test('is one revision for every clip and stores whole behaviours that replace what a clip had', async () => {
		const before = await revisions();
		const done = await setBehaviors(['c1', 'c2'], [sway, shake]);
		expect(await revisions()).toBe(before + 1);
		expect(await lastLabel()).toBe('Set behaviours (2 clips)');
		expect(clip(done, 'c1').behaviors).toEqual([
			{ kind: 'sway', amount: 5, speed: 0.5, phase: 0 },
			{ kind: 'shake', amount: 0.01, speed: 8, seed: 7, phase: 0 }
		]);
		expect(clip(await getTimeline(), 'c2').behaviors?.length).toBe(2);
		await setBehaviors(['c1'], [{ kind: 'spin', speed: 0.1 }]);
		expect(await lastLabel()).toBe('Set behaviours');
		expect(clip(await getTimeline(), 'c1').behaviors).toEqual([{ kind: 'spin', speed: 0.1, phase: 0 }]);
		const cleared = await setBehaviors(['c1', 'c2'], []);
		expect(await lastLabel()).toBe('Clear behaviours (2 clips)');
		expect(clip(cleared, 'c1')).not.toHaveProperty('behaviors');
		await undo();
		expect(clip(await getTimeline(), 'c1').behaviors?.length).toBe(1);
	});

	test('the picture moves with them and the pose does not', async () => {
		const done = await setBehaviors(['c1'], [{ kind: 'sway', amount: 10, speed: 1 }]);
		const c = clip(done, 'c1');
		expect(transformAt(c, 0.25).rotation).toBeCloseTo(10, 9);
		expect(baseTransformAt(c, 0.25).rotation).toBe(0);
	});

	test('a refusal writes nothing at all: a sound, a clip that is not there, nothing named, a number past its limit', async () => {
		const before = await revisions();
		await expect(setBehaviors(['c1', 'c3'], [sway])).rejects.toThrow('a sound has no picture to move');
		await expect(setBehaviors(['c1', 'ghost'], [sway])).rejects.toThrow('clip not found: ghost');
		await expect(setBehaviors([], [sway])).rejects.toThrow('name at least one clip');
		await expect(setBehaviors(['c1'], [{ kind: 'sway', amount: 181, speed: 1 }])).rejects.toThrow('sway amount must be a number within 0..=180');
		await expect(setBehaviors(['c1'], Array.from({ length: 9 }, () => sway))).rejects.toThrow('at most 8 behaviours per clip');
		expect(await revisions()).toBe(before);
		expect(clip(await getTimeline(), 'c1').behaviors).toBeUndefined();
	});
});

describe('baking behaviours (browser harness)', () => {
	test('is one revision that turns the motion into keys and clears the behaviours', async () => {
		await setBehaviors(['c1'], [{ kind: 'sway', amount: 10, speed: 1 }]);
		const before = await revisions();
		const done = await bakeBehaviors(['c1'], 10);
		expect(await revisions()).toBe(before + 1);
		expect(await lastLabel()).toBe('Bake behaviours');
		expect(done.baked).toEqual([{ clip_id: 'c1', properties: ['rotation'], keys: 126, fps: 10 }]);
		const c = clip(done.timeline, 'c1');
		expect(c.behaviors).toBeUndefined();
		const keys = propertyKeys(c, 'rotation');
		expect(keys.length).toBe(126);
		expect(keys[0]).toEqual({ time: 0, value: 0 });
		expect(keys[2].value).toBeCloseTo(10 * Math.sin(2 * Math.PI * 0.2), 9);
		await undo();
		expect(clip(await getTimeline(), 'c1').behaviors?.length).toBe(1);
	});

	test('a refusal bakes nothing: no behaviours, a clip too long for its keys', async () => {
		await expect(bakeBehaviors(['c1'], 10)).rejects.toThrow('the clip has no behaviours to bake');
		await setBehaviors(['c1', 'c2'], [sway]);
		const before = await revisions();
		await expect(bakeBehaviors(['c1', 'c2'], 120)).rejects.toThrow('split the clip into pieces under 8.3 s');
		expect(await revisions()).toBe(before);
		expect(clip(await getTimeline(), 'c1').behaviors?.length).toBe(1);
	});
});
