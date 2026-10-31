import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';

// In the desktop app `setBehaviors` and `bakeBehaviors` are the Tauri commands `set_behaviors` and `bake_behaviors`:
// what is sent is camelCase arguments with the behaviours as given (tagged by `kind`) and a frame rate only when one
// was asked for, and what comes back is the timeline and, for a bake, what each clip got.

const calls: [string, Record<string, unknown> | undefined][] = [];
const timeline = { tracks: [] };

const realCore = { ...(await import('@tauri-apps/api/core')) };

mock.module('@tauri-apps/api/core', () => ({
	invoke: async (cmd: string, args?: Record<string, unknown>) => {
		calls.push([cmd, args]);
		if (cmd === 'set_behaviors') return timeline;
		if (cmd === 'bake_behaviors') return { timeline, baked: [{ clip_id: 'c1', properties: ['rotation'], keys: 31, fps: 30 }] };
		return undefined;
	}
}));

const { bakeBehaviors, setBehaviors } = await import('./api');
const g = globalThis as unknown as { window?: unknown };

beforeEach(() => {
	calls.length = 0;
	g.window = { __TAURI_INTERNALS__: {} };
});
afterEach(() => {
	delete g.window;
});
afterAll(() => {
	delete g.window;
	mock.module('@tauri-apps/api/core', () => realCore);
});

describe('live behaviours (desktop)', () => {
	test('setting sends the clips and the whole list, tagged by kind', async () => {
		const list = [
			{ kind: 'sway', amount: 5, speed: 0.5, phase: 0 },
			{ kind: 'shake', amount: 0.01, speed: 8, seed: 7, phase: 0 }
		] as const;
		expect(await setBehaviors(['c1', 'c2'], [...list])).toBe(timeline);
		expect(calls).toEqual([['set_behaviors', { clipIds: ['c1', 'c2'], behaviors: list }]]);
		await setBehaviors(['c1'], []);
		expect(calls[1]).toEqual(['set_behaviors', { clipIds: ['c1'], behaviors: [] }]);
	});

	test('baking sends the clips, and a rate only when given, and answers what each clip got', async () => {
		const done = await bakeBehaviors(['c1']);
		expect(calls).toEqual([['bake_behaviors', { clipIds: ['c1'], fps: undefined }]]);
		expect(done.baked).toEqual([{ clip_id: 'c1', properties: ['rotation'], keys: 31, fps: 30 }]);
		await bakeBehaviors(['c1', 'c2'], 24);
		expect(calls[1]).toEqual(['bake_behaviors', { clipIds: ['c1', 'c2'], fps: 24 }]);
	});
});
