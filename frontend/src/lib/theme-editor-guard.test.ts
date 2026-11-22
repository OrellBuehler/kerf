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
		expect(modal).toMatch(/side\s*\?\s*`background:transparent;align-items:stretch;justify-content:flex-end;/);
		expect(modal).toContain("{tokens ?? ''}");
	});

	test('is drawn in that theme all the way down: derived tokens and the type styles are declared on it too, not inherited from the page', () => {
		// A custom property made with `var()` is substituted where it is declared and inherited as the result, so setting the
		// opaque tokens inline on the editor is not enough: its borders, radii, shadows, washes and `--type-*` would resolve
		// from the page's draft (a red border after `border-base` is edited, Inter after the typeface is).
		const modal = read(`${EDITOR}Modal.svelte`);
		expect(modal).toContain("data-theme-scope={tokens ? '' : undefined}");
		const tokens = read('lib/styles/kerf-tokens.css');
		const root = /(^|\n):root,\s*\[data-theme-scope\]\s*\{([\s\S]*?)\n\}\n/.exec(tokens);
		expect(root, 'the token block is declared on :root and on [data-theme-scope]').not.toBeNull();
		for (const derived of ['--border-default', '--radius-sm', '--shadow-lg', '--success-surface', '--type-ui', '--type-body', '--focus-outline', '--hover-fill'])
			expect(root![2], derived).toMatch(new RegExp(`${derived}:[^;]*var\\(`));
		// Nothing with a `var()` in it is declared on :root alone, outside that block.
		const rest = tokens.replace(root![0], '');
		expect(/:root\s*\{/.test(rest)).toBe(false);
		const layout = read('routes/layout.css');
		expect(layout).toMatch(/:root,\s*\[data-theme-scope\]\s*\{\s*color-scheme: dark;[\s\S]*?--background: var\(--surface-app\)/);
		expect(layout).toMatch(/\[data-theme-scope\]\s*\{\s*font: var\(--type-body\);\s*color: var\(--text-primary\);/);
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
	test('the arrows only move between themes: choosing one writes it, so Space or Enter chooses (and following the system, an arrow would reassign a slot)', () => {
		expect(read(`${EDITOR}SettingsAppearance.svelte`)).toContain('use:roving={{ follow: false }}');
		expect(read('lib/roving.ts')).toContain('if (follow) radios[to].click();');
	});

	test('the list is bounded and scrolls, so the rows under it stay in reach', () => {
		const appearance = read(`${EDITOR}SettingsAppearance.svelte`);
		expect(appearance).toMatch(/data-testid="theme-list"\s+style="[^"]*max-height:\d+px;overflow-y:auto/);
	});

	test('the editor has one way in, the row of the theme: a text button, not the palette glyph the section already wears', () => {
		const appearance = read(`${EDITOR}SettingsAppearance.svelte`);
		expect(appearance).toMatch(/\{e\.builtIn \? 'Customize…' : 'Edit…'\}/);
		expect(appearance).not.toContain('n="palette"');
		expect(appearance.match(/themes\.edit\(/g)?.length).toBe(1);
	});

	test('a copy and an import are never inert: at the limit they answer in words, like Customize', () => {
		const appearance = read(`${EDITOR}SettingsAppearance.svelte`);
		expect(appearance).not.toMatch(/pending=\{!themes\.canAdd\}/);
		expect(appearance).toContain('up to {MAX_THEMES} of your own');
	});

	test('every row a user can act on is a named button, and a theme is picked as one of a radio group', () => {
		const appearance = read(`${EDITOR}SettingsAppearance.svelte`);
		expect(appearance).toMatch(/role="radiogroup"\s+aria-label="Themes"\s+use:roving=\{\{ follow: false \}\}/);
		expect(appearance).toMatch(/role="radio"\s*aria-checked=\{on\}/);
		for (const tipText of ['Make a copy', 'Export as a file…', 'Rename', 'Delete…']) expect(appearance).toContain(`title="${tipText}"`);
		// A text field is never inside the radio button (a button may not hold one).
		const radio = /<button\s+type="button"\s+role="radio"[\s\S]*?<\/button>/.exec(appearance)![0];
		expect(radio).not.toContain('<input');
	});
});
