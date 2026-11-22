import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { DOCK_MIN_VIEWPORT, EDITOR_WIDTH, dockWidth } from './theme-dock';
import { SRC } from './test-sources';

describe('where the theme editor sits', () => {
	test('a window wide enough gives it its strip, a narrower one makes it an overlay', () => {
		expect(dockWidth(1920)).toBe(EDITOR_WIDTH);
		expect(dockWidth(1366)).toBe(EDITOR_WIDTH);
		expect(dockWidth(DOCK_MIN_VIEWPORT)).toBe(EDITOR_WIDTH);
		expect(dockWidth(DOCK_MIN_VIEWPORT - 1)).toBe(0);
		expect(dockWidth(1100)).toBe(0);
	});

	test('a width that is not one docks nothing', () => {
		for (const v of [0, -1, NaN, Infinity - Infinity, undefined as unknown as number]) expect(dockWidth(v), String(v)).toBe(0);
	});

	test('what is left of the window is still a workable app', () => {
		expect(DOCK_MIN_VIEWPORT - EDITOR_WIDTH).toBeGreaterThanOrEqual(780);
	});

	test('the app shell gives up exactly that strip: the page\'s root keeps a right offset the editor sets', () => {
		const page = readFileSync(join(SRC, 'routes/+page.svelte'), 'utf8');
		expect(page).toContain('inset:0 var(--shell-right,0px) 0 0');
		const editor = readFileSync(join(SRC, 'lib/components/editor/ThemeEditor.svelte'), 'utf8');
		expect(editor).toContain("setProperty('--shell-right', `${dock}px`)");
		expect(editor).toContain("removeProperty('--shell-right')");
		expect(editor).toContain('dockWidth(');
	});
});
