import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// The few properties of the theme editor that live in how its components are wired rather than in a function a test
// can call: it replaces the settings dialog without closing it, it is drawn in the theme the sitting began with, it
// is docked with nothing dimmed, a sitting cannot outlive the dialog, and the toaster keeps clear of it.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

describe('the theme editor', () => {
	test('takes the settings dialog\'s place while it is open, so the page stays inert and the section is kept', () => {
		const dialog = read(`${EDITOR}SettingsDialog.svelte`);
		expect(dialog).toMatch(/\{#if settings\.themes\.session\}\s*<ThemeEditor \/>\s*\{:else\}\s*<Modal title="Settings"/);
		// `settings.open` is not touched by opening or closing the editor.
		expect(read(`${EDITOR}ThemeEditor.svelte`)).not.toMatch(/settings\.(open|close|toggle)\b/);
	});

	test('a sitting ends with the dialog, whatever closed it', () => {
		expect(read(`${EDITOR}SettingsDialog.svelte`)).toMatch(/onDestroy\(\(\) => settings\.themes\.cancel\(\)\)/);
	});

	test('is docked, undimmed, and drawn in the theme the sitting began with, not the draft', () => {
		const editor = read(`${EDITOR}ThemeEditor.svelte`);
		const tag = /<Modal\b[^>]*>/.exec(editor)![0];
		expect(tag).toContain(' side ');
		expect(tag).toContain('{tokens}');
		expect(tag).toContain('dismissOnScrim={false}');
		expect(editor).toMatch(/const tokens = \$derived\(session \? themeStyle\(session\.saved\) : undefined\)/);
		const modal = read(`${EDITOR}Modal.svelte`);
		expect(modal).toMatch(/side\s*\?\s*'background:transparent;/);
		expect(modal).toContain("{tokens ?? ''}");
	});

	test('Escape and the close button go through the question, not straight to Cancel', () => {
		expect(read(`${EDITOR}ThemeEditor.svelte`)).toContain('onClose={() => void themes.close()}');
		expect(read('lib/theme-book.svelte.ts')).toContain('CONFIRM.discardThemeEdits');
	});

	test('keeps the page\'s toasts clear of it', () => {
		expect(read('lib/components/ui/sonner/sonner.svelte')).toContain('right: settings.themes.session ? 504 : 12');
	});

	test('undo goes through the keymap, not a key written here', () => {
		const editor = read(`${EDITOR}ThemeEditor.svelte`);
		expect(editor).toContain("settings.actionFor(e)");
		expect(editor).toContain("'edit.undo'");
		expect(editor).not.toMatch(/e\.key\s*===\s*['"]z['"]/i);
	});
});

describe('the sections that list themes', () => {
	test('every row a user can act on is a named button, and a theme is picked as one of a radio group', () => {
		const appearance = read(`${EDITOR}SettingsAppearance.svelte`);
		expect(appearance).toMatch(/role="radiogroup" aria-label="Themes" use:roving/);
		expect(appearance).toMatch(/role="radio"\s*aria-checked=\{on\}/);
		for (const tipText of ['Make a copy', 'Export as a file…', 'Rename', 'Delete…']) expect(appearance).toContain(`title="${tipText}"`);
		// A text field is never inside the radio button (a button may not hold one).
		const radio = /<button\s+type="button"\s+role="radio"[\s\S]*?<\/button>/.exec(appearance)![0];
		expect(radio).not.toContain('<input');
	});
});
