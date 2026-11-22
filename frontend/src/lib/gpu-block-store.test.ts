import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { GpuBlock, SettingsPatch, SettingsView } from './types';

// The settings store says a GPU preview that turned itself off once, quietly, and writes that it said so (a restart
// finds the block told and keeps quiet). The block arrives in the view `get_settings` answers and in the
// `settings-changed` event the backend raises when its self-test finds a mismatch.

const g = globalThis as unknown as { document?: unknown };
const originalDocument = g.document;
g.document = { documentElement: { style: { setProperty() {} }, classList: { toggle() {} } } };

const real = { ...(await import('./api')) };

const written: SettingsPatch[] = [];
const toasts: Array<{ message: string; description?: string }> = [];
mock.module('./api', () => ({
	...real,
	// What the backend answers a write: the view with the block as written.
	setSettings: async (patch: SettingsPatch) => {
		written.push(patch);
		return { ...(await real.getSettings()), ...('gpu_preview_block' in patch ? { gpu_preview_block: patch.gpu_preview_block } : {}) };
	}
}));
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(() => {}, {
		success() {},
		info: (message: string, options?: { description?: string }) => toasts.push({ message, description: options?.description }),
		error() {},
		warning() {},
		dismiss() {}
	})
}));
afterAll(() => {
	mock.module('./api', () => real);
	if (originalDocument === undefined) delete g.document;
	else g.document = originalDocument;
});

const { settings } = await import('./settings.svelte');

const settle = async () => {
	for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
};

const crash: GpuBlock = {
	kind: 'crash',
	reason: 'turned itself off: the last run ended while the GPU preview was starting',
	app_version: '0.31.0',
	at: 1,
	told: false
};

const view = async (block: GpuBlock | null): Promise<SettingsView> => ({ ...(await real.getSettings()), gpu_preview_block: block });

beforeEach(async () => {
	written.length = 0;
	toasts.length = 0;
	// No block: the store forgets that it said one.
	settings.noteChanged(await view(null));
	await settle();
	written.length = 0;
	toasts.length = 0;
});

describe('the settings store says a GPU preview that turned itself off', () => {
	test('once, with words and not the technical reason, and writes the same block back told', async () => {
		settings.noteChanged(await view(crash));
		await settle();
		expect(toasts.length).toBe(1);
		expect(toasts[0].message).toContain('turned off');
		expect(toasts[0].description).toContain('Settings > Preview');
		expect(toasts[0].description).not.toContain(crash.reason);
		expect(written).toEqual([{ gpu_preview_block: { ...crash, told: true } }]);
	});

	test('not again for the same block, however often it is read before the write has come back', async () => {
		settings.noteChanged(await view(crash));
		settings.noteChanged(await view(crash));
		await settle();
		settings.noteChanged(await view(crash));
		await settle();
		expect(toasts.length).toBe(1);
		expect(written.length).toBe(1);
	});

	test('not at all for a block it already said (a restart finds it told)', async () => {
		settings.noteChanged(await view({ ...crash, told: true }));
		await settle();
		expect(toasts).toEqual([]);
		expect(written).toEqual([]);
	});

	test('and again for a block that comes after the first was lifted', async () => {
		settings.noteChanged(await view(crash));
		await settle();
		expect(toasts.length).toBe(1);
		// The user switched the preview on: the block is gone.
		settings.noteChanged(await view(null));
		await settle();
		expect(toasts.length).toBe(1);
		// It turns itself off again (a mismatch this time): that is news.
		settings.noteChanged(await view({ ...crash, kind: 'mismatch', adapter: 'a | b | c | d' }));
		await settle();
		expect(toasts.length).toBe(2);
		expect(toasts[1].description).toContain('driver');
		expect(written.length).toBe(2);
	});

	test('says nothing about a view with no block', async () => {
		settings.noteChanged(await view(null));
		await settle();
		expect(toasts).toEqual([]);
		expect(written).toEqual([]);
	});
});
