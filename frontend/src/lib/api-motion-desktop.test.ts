import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';

// In the desktop app `applyMotionPreset` and `listMotionPresets` are the Tauri commands `apply_motion_preset` and
// `list_motion_presets`: what is sent is camelCase arguments with the options a caller left out left out, and
// what comes back is the timeline and a window per clip.

const calls: [string, Record<string, unknown> | undefined][] = [];
const timeline = { tracks: [] };

const realCore = { ...(await import('@tauri-apps/api/core')) };

mock.module('@tauri-apps/api/core', () => ({
	invoke: async (cmd: string, args?: Record<string, unknown>) => {
		calls.push([cmd, args]);
		if (cmd === 'apply_motion_preset') return { timeline, applied: [{ clip_id: 'c1', start: 0, end: 0.5 }] };
		if (cmd === 'list_motion_presets') return [{ id: 'fade_in', label: 'Fade in', kind: 'entrance', hint: 'x', properties: ['opacity'] }];
		return undefined;
	}
}));

const { applyMotionPreset, listMotionPresets } = await import('./api');
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

describe('motion presets (desktop)', () => {
	test('applying sends the clips, the preset and only the options given', async () => {
		const done = await applyMotionPreset(['c1', 'c2'], 'fade_in');
		expect(calls).toEqual([['apply_motion_preset', { clipIds: ['c1', 'c2'], preset: 'fade_in' }]]);
		expect(done.applied).toEqual([{ clip_id: 'c1', start: 0, end: 0.5 }]);
		await applyMotionPreset(['c1'], 'slide_out_to_left', { mode: 'replace', duration: 0.8, intensity: 1.5 });
		expect(calls[1]).toEqual([
			'apply_motion_preset',
			{ clipIds: ['c1'], preset: 'slide_out_to_left', mode: 'replace', duration: 0.8, intensity: 1.5 }
		]);
	});

	test('the catalogue is the backend’s', async () => {
		expect((await listMotionPresets()).map((p) => p.id)).toEqual(['fade_in']);
		expect(calls).toEqual([['list_motion_presets', undefined]]);
	});
});
