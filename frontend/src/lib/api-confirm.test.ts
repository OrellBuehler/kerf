import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { UNSAVED } from './confirm-copy';

// The native prompts: what the dialog plugin is asked to show (named buttons, not Yes / No) and how
// each answer is read. A dismissed or unrecognised answer must never be taken for "go ahead".

const shown: { text: string; opts: Record<string, unknown> }[] = [];
const asked: { text: string; opts: Record<string, unknown> }[] = [];
let answer: string | Error = 'Cancel';
let askAnswer = true;

// Put back for the other files once this one is done.
const realCore = { ...(await import('@tauri-apps/api/core')) };
const realDialog = { ...(await import('@tauri-apps/plugin-dialog')) };

mock.module('@tauri-apps/api/core', () => ({ invoke: async () => null }));
mock.module('@tauri-apps/plugin-dialog', () => ({
	message: async (text: string, opts: Record<string, unknown>) => {
		shown.push({ text, opts });
		if (answer instanceof Error) throw answer;
		return answer;
	},
	ask: async (text: string, opts: Record<string, unknown>) => {
		asked.push({ text, opts });
		return askAnswer;
	},
	open: async () => null,
	save: async () => null
}));

const { askUnsaved, confirmAction } = await import('./api');
const g = globalThis as unknown as { window?: unknown };

beforeEach(() => {
	shown.length = 0;
	asked.length = 0;
	answer = 'Cancel';
	askAnswer = true;
	g.window = { __TAURI_INTERNALS__: {} };
});
afterEach(() => {
	delete g.window;
});
afterAll(() => {
	delete g.window;
	mock.module('@tauri-apps/api/core', () => realCore);
	mock.module('@tauri-apps/plugin-dialog', () => realDialog);
});

describe('askUnsaved', () => {
	test('asks with Save…, Don’t save and Cancel, under a title that names the act', async () => {
		answer = "Don't save";
		await askUnsaved(UNSAVED.close);
		expect(shown).toHaveLength(1);
		expect(shown[0].text).toBe("This project hasn't been saved. If you close now it will be lost.");
		expect(shown[0].opts).toMatchObject({
			title: 'Close Kerf?',
			kind: 'warning',
			buttons: { yes: 'Save…', no: "Don't save", cancel: 'Cancel' }
		});
	});

	test('reads each answer as the choice it names', async () => {
		for (const [pressed, choice] of [['Save…', 'save'], ["Don't save", 'discard'], ['Cancel', 'cancel']] as const) {
			answer = pressed;
			expect(await askUnsaved(UNSAVED.close)).toBe(choice);
		}
	});

	test('a dialog that was closed, or answered with something else, is Cancel', async () => {
		for (const closed of ['', 'No', 'Yes']) {
			answer = closed;
			expect(await askUnsaved(UNSAVED.quit)).toBe('cancel');
		}
	});

	test('a platform that cannot show named buttons falls back to a two-button ask, without Save', async () => {
		answer = new Error('buttons are not supported here');
		askAnswer = true;
		expect(await askUnsaved(UNSAVED.replace)).toBe('discard');
		expect(asked[0].opts).toMatchObject({ okLabel: 'Discard', cancelLabel: 'Cancel', title: 'Replace this project?' });
		askAnswer = false;
		expect(await askUnsaved(UNSAVED.replace)).toBe('cancel');
	});

	test('in the browser it is window.confirm: discard or cancel, never save', async () => {
		delete g.window;
		g.window = { confirm: () => true };
		expect(await askUnsaved(UNSAVED.close)).toBe('discard');
		g.window = { confirm: () => false };
		expect(await askUnsaved(UNSAVED.close)).toBe('cancel');
		expect(shown).toHaveLength(0);
	});
});

describe('confirmAction with named buttons', () => {
	test('shows Ok / Cancel under their own names and is true only for the first', async () => {
		answer = 'Reset all';
		expect(await confirmAction('Every workspace goes back.', 'Reset all workspaces?', { ok: 'Reset all', cancel: 'Cancel' })).toBe(true);
		expect(shown[0].opts).toMatchObject({ title: 'Reset all workspaces?', buttons: { ok: 'Reset all', cancel: 'Cancel' } });
		answer = 'Cancel';
		expect(await confirmAction('x', 't', { ok: 'Reset all', cancel: 'Cancel' })).toBe(false);
		answer = '';
		expect(await confirmAction('x', 't', { ok: 'Reset all', cancel: 'Cancel' })).toBe(false);
	});
});
