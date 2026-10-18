import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';

// `templates-changed` in the desktop app. The backend emits it for an agent's change (the MCP tools) and not for the
// Tauri commands the GUI calls itself, so `api.ts` announces the page's own save, import and delete — after the call
// has resolved — and every mounted library grid hears both.

const calls: string[] = [];
let outcome: 'added' | 'already_present' = 'added';
let failing = false;
let handlers: (() => void)[] = [];
let unlistened = 0;

const info = { id: 'x', name: 'Card', description: '', duration: 4, width: 1920, height: 1080, controls: [], fonts: [], has_poster: false, size_bytes: 1 };

const realCore = { ...(await import('@tauri-apps/api/core')) };
const realEvent = { ...(await import('@tauri-apps/api/event')) };

mock.module('@tauri-apps/api/core', () => ({
	invoke: async (cmd: string) => {
		calls.push(cmd);
		if (failing) throw new Error('refused');
		if (cmd === 'save_template') return { info, warnings: [] };
		if (cmd === 'import_template') return { status: outcome, info, warnings: [] };
		return undefined;
	}
}));
mock.module('@tauri-apps/api/event', () => ({
	listen: async (name: string, cb: () => void) => {
		calls.push(`listen ${name}`);
		handlers.push(cb);
		return () => {
			unlistened++;
			handlers = handlers.filter((h) => h !== cb);
		};
	}
}));

const { deleteTemplate, importTemplate, onTemplatesChanged, saveTemplate } = await import('./api');
const g = globalThis as unknown as { window?: unknown };

beforeEach(() => {
	calls.length = 0;
	outcome = 'added';
	failing = false;
	handlers = [];
	unlistened = 0;
	g.window = { __TAURI_INTERNALS__: {} };
});
afterEach(() => {
	delete g.window;
});
afterAll(() => {
	delete g.window;
	mock.module('@tauri-apps/api/core', () => realCore);
	mock.module('@tauri-apps/api/event', () => realEvent);
});

describe('templates-changed (desktop)', () => {
	test('listens for the backend’s event and for the page’s own save, import and delete', async () => {
		let heard = 0;
		const off = await onTemplatesChanged(() => void heard++);
		expect(calls).toContain('listen templates-changed');

		await saveTemplate('overlay', 'Card', []);
		expect(heard).toBe(1);

		outcome = 'already_present';
		await importTemplate('/tmp/card.kerftemplate');
		expect(heard).toBe(1);
		outcome = 'added';
		await importTemplate('/tmp/card.kerftemplate');
		expect(heard).toBe(2);

		await deleteTemplate('x');
		expect(heard).toBe(3);

		handlers.forEach((h) => h());
		expect(heard).toBe(4);

		off();
		expect(unlistened).toBe(1);
		await saveTemplate('overlay', 'Card', []);
		await deleteTemplate('x');
		expect(heard).toBe(4);
	});

	test('every mounted listener hears it', async () => {
		const heard = [0, 0];
		const offs = [await onTemplatesChanged(() => void heard[0]++), await onTemplatesChanged(() => void heard[1]++)];
		await saveTemplate('overlay', 'Card', []);
		expect(heard).toEqual([1, 1]);
		offs[0]();
		await deleteTemplate('x');
		expect(heard).toEqual([1, 2]);
		offs[1]();
	});

	test('a call that failed announces nothing', async () => {
		let heard = 0;
		const off = await onTemplatesChanged(() => void heard++);
		failing = true;
		await expect(saveTemplate('overlay', 'Card', [])).rejects.toThrow('refused');
		await expect(importTemplate('/tmp/card.kerftemplate')).rejects.toThrow('refused');
		await expect(deleteTemplate('x')).rejects.toThrow('refused');
		expect(heard).toBe(0);
		off();
	});
});
