import { afterAll, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { SettingsPatch, SettingsView } from './types';

// "Ask for tags after importing" (Settings › Editing): on until it is switched off, by its row or by the prompt's own
// "Don’t ask again"; a settings file from before the setting existed means on.

const g = globalThis as unknown as { document?: unknown };
const originalDocument = g.document;
g.document = { documentElement: { style: { setProperty() {} }, classList: { toggle() {} } } };

const real = { ...(await import('./api')) };
const patches: SettingsPatch[] = [];
mock.module('./api', () => ({
	...real,
	setSettings: async (patch: SettingsPatch) => {
		patches.push(patch);
		return { ...(await real.getSettings()), ...patch } as SettingsView;
	}
}));
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(() => {}, { success() {}, info() {}, error() {}, warning() {}, dismiss() {} })
}));
afterAll(() => {
	mock.module('./api', () => real);
	if (originalDocument === undefined) delete g.document;
	else g.document = originalDocument;
});

const { settings } = await import('./settings.svelte');

describe('ask_tags_on_import', () => {
	test('is on by default, and a view from before the setting means on', async () => {
		expect(settings.askTagsOnImport).toBe(true);
		const old = { ...(await real.getSettings()) } as Partial<SettingsView>;
		delete old.ask_tags_on_import;
		settings.noteChanged(old as SettingsView);
		expect(settings.askTagsOnImport).toBe(true);
	});

	test('a view that says off turns it off', async () => {
		settings.noteChanged({ ...(await real.getSettings()), ask_tags_on_import: false });
		expect(settings.askTagsOnImport).toBe(false);
		settings.noteChanged({ ...(await real.getSettings()), ask_tags_on_import: true });
		expect(settings.askTagsOnImport).toBe(true);
	});

	test('switching it off writes the one field, and writing what is already in force writes nothing', async () => {
		patches.length = 0;
		await settings.setAskTagsOnImport(false);
		expect(patches).toEqual([{ ask_tags_on_import: false }]);
		expect(settings.askTagsOnImport).toBe(false);
		await settings.setAskTagsOnImport(false);
		expect(patches).toHaveLength(1);
		await settings.setAskTagsOnImport(true);
		expect(patches.at(-1)).toEqual({ ask_tags_on_import: true });
	});
});
