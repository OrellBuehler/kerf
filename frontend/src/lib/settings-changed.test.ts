import { afterAll, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { SettingsView } from './types';

// `settings-changed` is the event an agent's `set_settings` raises. The backend sends the view in force; the
// listener must still survive an event with nothing in it (an older build, a `()` payload), which reads the
// settings again instead of absorbing `null`.

const g = globalThis as unknown as { document?: unknown };
const originalDocument = g.document;
// `applyTheme` writes the tokens onto the page root.
g.document = { documentElement: { style: { setProperty() {} }, classList: { toggle() {} } } };

const real = { ...(await import('./api')) };

let views = 0;
let view: SettingsView | null = null;
mock.module('./api', () => ({
	...real,
	getSettings: async () => {
		views++;
		return view ?? (await real.getSettings());
	}
}));
// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(() => {}, { success() {}, info() {}, error() {}, warning() {}, dismiss() {} })
}));
afterAll(() => {
	mock.module('./api', () => real);
	if (originalDocument === undefined) delete g.document;
	else g.document = originalDocument;
});

const { settings } = await import('./settings.svelte');

const settle = () => new Promise((r) => setTimeout(r, 0));

describe('the settings-changed event', () => {
	test('a view in the payload is taken as it is, without asking again', async () => {
		const before = views;
		const next = { ...(await real.getSettings()), agent_review: false, time_display: 'seconds' as const };
		views = before;
		settings.noteChanged(next);
		expect(settings.agentReview).toBe(false);
		expect(settings.timeMode).toBe('seconds');
		expect(views).toBe(before);
	});

	test('a payload without one reads the settings again instead of throwing', async () => {
		view = { ...(await real.getSettings()), agent_review: true, time_display: 'timecode' };
		const changes = settings.externalChanges;
		const before = views;
		expect(() => settings.noteChanged(null)).not.toThrow();
		expect(() => settings.noteChanged(undefined)).not.toThrow();
		await settle();
		await settle();
		expect(views).toBeGreaterThan(before);
		expect(settings.agentReview).toBe(true);
		expect(settings.timeMode).toBe('timecode');
		expect(settings.externalChanges).toBe(changes + 2);
	});
});
