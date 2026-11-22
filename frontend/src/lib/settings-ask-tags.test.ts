import { afterAll, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { SettingsPatch, SettingsView } from './types';

// "Ask for tags after importing" (Settings › Editing): off until it is switched on (a first import lands on the clip,
// not on a question about a feature the person has not met), and a stored choice wins either way: a settings file from
// before the setting existed means off, one that stored `true` keeps asking until its row or the prompt's own
// "Don’t ask again" says otherwise.

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
	test('is off by default: a store nothing has touched says off, whatever another test file left on the shared one', () => {
		const fresh = new (settings.constructor as new () => typeof settings)();
		expect(fresh.askTagsOnImport).toBe(false);
	});

	test('a view from before the setting means off', async () => {
		settings.noteChanged({ ...(await real.getSettings()), ask_tags_on_import: true });
		const old = { ...(await real.getSettings()) } as Partial<SettingsView>;
		delete old.ask_tags_on_import;
		settings.noteChanged(old as SettingsView);
		expect(settings.askTagsOnImport).toBe(false);
	});

	test('a stored choice wins over the default: a view that says on turns it on, one that says off turns it off', async () => {
		settings.noteChanged({ ...(await real.getSettings()), ask_tags_on_import: true });
		expect(settings.askTagsOnImport).toBe(true);
		settings.noteChanged({ ...(await real.getSettings()), ask_tags_on_import: false });
		expect(settings.askTagsOnImport).toBe(false);
	});

	test('switching it on writes the one field, and writing what is already in force writes nothing', async () => {
		patches.length = 0;
		await settings.setAskTagsOnImport(true);
		expect(patches).toEqual([{ ask_tags_on_import: true }]);
		expect(settings.askTagsOnImport).toBe(true);
		await settings.setAskTagsOnImport(true);
		expect(patches).toHaveLength(1);
		await settings.setAskTagsOnImport(false);
		expect(patches.at(-1)).toEqual({ ask_tags_on_import: false });
	});
});
