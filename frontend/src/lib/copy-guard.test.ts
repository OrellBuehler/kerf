import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { buildMenus, entryLabel, flatten, type MenuState } from './menus';
import { plainLabel } from './keymap';
import { posix, SRC, uiSources } from './test-sources';

// One product, one way of saying each thing (the review's copy deck, `dialogs.md`): an ellipsis
// promises a dialog or a picker and nothing else; one action has one name wherever it is offered; a
// failure is "Couldn't …"; a confirm names its buttons. These hold the words that kept drifting.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

const state: MenuState = {
	saved: false,
	canUndo: true,
	canRedo: true,
	hasClips: true,
	exporting: false,
	hasSelection: true,
	rippleMode: false,
	snap: true,
	minimap: true,
	safeAreas: false,
	hasFrame: true,
	tool: 'pointer',
	delivery: 'source',
	allHeight: 'medium',
	workspace: 'edit',
	openPanels: ['library', 'preview', 'timeline', 'inspector', 'agent'],
	detachedPanels: [],
	desktop: true,
	previewing: false
};

describe('the ellipsis', () => {
	/** The menu entries that open a dialog, a picker or a question before anything happens. */
	const OPENS_SOMETHING = new Set([
		'file.open',
		'file.save',
		'file.import',
		'file.importCaptions',
		'file.export',
		'file.saveCover',
		'app.settings',
		'app.shortcuts',
		'app.keyboard',
		'app.checkUpdate',
		'window.resetAllWorkspaces'
	]);

	const entries = buildMenus(state)
		.flatMap((m) => flatten(m.items))
		.filter((e): e is Extract<typeof e, { kind: 'action' | 'command' | 'submenu' }> => e.kind !== 'separator');

	test('an entry that opens a dialog says so, and one that acts at once does not', () => {
		const offenders = entries.flatMap((e) => {
			const label = entryLabel(e);
			const opens = e.kind === 'action' && OPENS_SOMETHING.has(e.id);
			if (opens && !label.endsWith('…')) return [`${label} opens something and has no ellipsis`];
			if (!opens && label.endsWith('…')) return [`${label} acts at once and has an ellipsis`];
			return [];
		});
		expect(offenders).toEqual([]);
	});

	test('the menu says Settings…, Keyboard shortcuts…, Reset all workspaces…, Release notes, Show log folder', () => {
		const names = entries.map(entryLabel);
		for (const want of ['Settings…', 'Keyboard shortcuts…', 'Reset all workspaces…', 'Release notes', 'Show log folder'])
			expect(names, want).toContain(want);
		for (const gone of ['Settings', 'Keyboard shortcuts', 'Reset all workspaces', 'Release page', 'Open the log folder'])
			expect(names, gone).not.toContain(gone);
	});

	test('in a sentence an action’s name has no ellipsis', () => {
		expect(plainLabel('Open project…')).toBe('Open project');
		expect(plainLabel('Open project...')).toBe('Open project');
		expect(plainLabel('Undo')).toBe('Undo');
		const keyboard = read(`${EDITOR}KeyboardSettings.svelte`);
		expect(keyboard).toContain('plainLabel(');
		// every place a label runs into a sentence goes through it
		expect(keyboard).not.toMatch(/\$\{conflicts\.map\(\(c\) => c\.label\)/);
		expect(keyboard).not.toMatch(/\{a\.label\}'s default/);
	});
});

describe('one name for one action', () => {
	test('the voiceover is "Write a voiceover…" wherever it is offered, and the dialog is titled for it', () => {
		for (const file of ['TitlesControls', 'AudioTab', 'MediaBin']) expect(read(`${EDITOR}${file}.svelte`), file).toMatch(/[Ww]rite a voiceover…/);
		expect(read(`${EDITOR}TitlesControls.svelte`)).not.toMatch(/>Voiceover…</);
		expect(read(`${EDITOR}VoiceoverDialog.svelte`)).toContain('title="Write a voiceover"');
		expect(read(`${EDITOR}VoiceoverDialog.svelte`)).toContain('>Generate voiceover</Btn>');
	});

	test('fitting music is "Fit to video…", titled "Fit to video", with a "Fit" button', () => {
		const dialog = read(`${EDITOR}FitMusicDialog.svelte`);
		expect(dialog).toContain('<Modal title="Fit to video"');
		expect(dialog).toContain('onclick={apply}>Fit</Btn>');
		expect(read(`${EDITOR}Inspector.svelte`)).toContain('Fit to video…');
		expect(read(`${EDITOR}Timeline.svelte`)).toContain("label: 'Fit to video…'");
	});

	test('separating stems is "Separate stems…", titled "Separate stems", with a "Separate stems" button', () => {
		const dialog = read(`${EDITOR}StemsDialog.svelte`);
		expect(dialog).toContain('<Modal title="Separate stems"');
		expect(dialog).toContain('onclick={start}>Separate stems</Btn>');
		expect(read(`${EDITOR}MediaBin.svelte`)).toContain("label: 'Separate stems…'");
	});

	test('Browse opens a file picker, so it says so', () => {
		expect(read(`${EDITOR}ExportDialog.svelte`)).toContain('onclick={browse}>Browse…</Btn>');
	});

	test('the empty bin imports "media", as the menu does', () => {
		const bin = read(`${EDITOR}MediaBin.svelte`);
		expect(bin).toContain('Import media…</Btn>');
		expect(bin).not.toContain('>Import files<');
	});
});

describe('a failure is "Couldn\'t"', () => {
	test('no toast says "Could not"', () => {
		const offenders = uiSources(SRC)
			.map(posix)
			.filter((f) => /\.(ts|svelte)$/.test(f))
			.flatMap((f) =>
				read(f)
					.split('\n')
					.map((line, i) => ({ line, at: `${f}:${i + 1}` }))
					.filter(({ line }) => /toast[.(][^\n]*Could not/.test(line))
					.map(({ at }) => at)
			);
		expect(offenders).toEqual([]);
	});

	test('the long-running dialogs lead their failures with what they were doing', () => {
		expect(read(`${EDITOR}ExportDialog.svelte`)).toContain("failure('Export failed', e)");
		expect(read(`${EDITOR}VoiceoverDialog.svelte`)).toContain("failure('Voiceover failed', e)");
		expect(read(`${EDITOR}StemsDialog.svelte`)).toContain("failure('Stem separation failed', e)");
	});
});

describe('a confirm names its buttons', () => {
	test('a confirm cannot be asked without naming its buttons', () => {
		// `labels` is a required argument of `confirmAction`, so the type system holds every caller to it.
		expect(read('lib/api.ts')).toMatch(/confirmAction\(message: string, title: string, labels: \{ ok: string; cancel: string \}\)/);
	});

	test('the unsaved-project prompts are asked with Save…, Don’t save and Cancel', () => {
		const page = read('routes/+page.svelte');
		for (const which of ['close', 'quit', 'replace']) expect(page, which).toContain(`proceedUnsaved(UNSAVED.${which})`);
		expect(page).not.toContain('never been saved');
	});
});
