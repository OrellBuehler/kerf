import { afterAll, beforeAll, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// `editor` is a runes class (`state.svelte.ts`); `test-runes` stands the runes in so
// it can be driven. What is checked: the toolbar's ripple flag follows the
// *project* (a file opened, a new project), not just what the last click left.
const real = { ...(await import('./api')) };
/** What the "backend" says the open project's flag is. */
let stored = false;
let tauri = false;

const backend = (over: Record<string, unknown> = {}) => ({
	...real,
	// The browser harness has no project files; in the desktop app these two
	// swap the project under the editor, which is the situation under test.
	newProject: async () => {
		stored = false;
		return tauri;
	},
	openProject: async () => {
		stored = true; // the file being opened was saved with ripple on
		return tauri ? '/work/cut.kerf' : null;
	},
	getRippleMode: async () => stored,
	setRippleMode: async (on: boolean) => (stored = on),
	...over
});

beforeAll(() => {
	mock.module('./api', () => backend());
});

// The module is mocked for this file only: put the real one back for the others.
afterAll(() => {
	mock.module('./api', () => real);
});

describe('editor.rippleMode follows the project', () => {
	test('load() reads the flag the project was saved with', async () => {
		const { editor } = await import('./state.svelte');
		stored = true;
		editor.rippleMode = false;
		await editor.load();
		expect(editor.rippleMode).toBe(true);
		stored = false;
		await editor.load();
		expect(editor.rippleMode).toBe(false);
	});

	test('opening a project saved with ripple on shows it on', async () => {
		const { editor } = await import('./state.svelte');
		tauri = true;
		stored = false;
		editor.rippleMode = false;
		expect(await editor.openProject('/work/cut.kerf')).toBe(true);
		expect(editor.rippleMode).toBe(true);
	});

	test('a new project resets a lit toggle', async () => {
		const { editor } = await import('./state.svelte');
		tauri = true;
		stored = true;
		editor.rippleMode = true;
		expect(await editor.newProject()).toBe(true);
		expect(editor.rippleMode).toBe(false);
	});

	test('a failed read keeps what was showing, and load() still completes', async () => {
		const { editor } = await import('./state.svelte');
		mock.module('./api', () =>
			backend({
				getRippleMode: async () => {
					throw new Error('backend busy');
				}
			})
		);
		editor.rippleMode = true;
		await editor.load();
		expect(editor.rippleMode).toBe(true);
		expect(editor.error).toBeNull();
		mock.module('./api', () => backend());
	});

	test('setRippleMode flips at once and reports what the backend stored', async () => {
		const { editor } = await import('./state.svelte');
		stored = false;
		editor.rippleMode = false;
		await editor.setRippleMode(true);
		expect(editor.rippleMode).toBe(true);
		expect(stored).toBe(true);
	});

	test('loadRippleMode says whether the flag changed (what the agent toast keys on)', async () => {
		const { editor } = await import('./state.svelte');
		stored = true;
		editor.rippleMode = true;
		expect(await editor.loadRippleMode()).toBe(false);
		stored = false;
		expect(await editor.loadRippleMode()).toBe(true);
		expect(editor.rippleMode).toBe(false);
	});
});
